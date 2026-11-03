use std::{
    fs,
    io::{Cursor, Read},
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use calamine::{open_workbook_auto_from_rs, Data, Reader};
use quick_xml::{events::Event, Reader as XmlReader};
use sha2::{Digest, Sha256};

use crate::{
    error::{AppError, AppResult},
    models::{ParseRequest, ParseResult, ParsedBlock, SourceLocation},
};

const MAX_FILE_BYTES: usize = 50 * 1024 * 1024;
const MAX_EXPANDED_BYTES: u64 = 150 * 1024 * 1024;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const TARGET_CHUNK_CHARS: usize = 2_400;

pub fn parse_request(request: ParseRequest) -> AppResult<ParseResult> {
    if request.bytes.len() > MAX_FILE_BYTES {
        return Err(AppError::Parser(format!(
            "{} exceeds the 50 MB indexing limit",
            request.file_name
        )));
    }
    let extension = request
        .extension
        .trim_start_matches('.')
        .to_ascii_lowercase();
    match extension.as_str() {
        "txt" => parse_text(&request.bytes, false),
        "md" | "markdown" => parse_text(&request.bytes, true),
        "csv" => parse_csv(&request.bytes),
        "xlsx" | "xls" | "xlsb" | "ods" => parse_spreadsheet(&request.bytes),
        "docx" => parse_docx(&request.bytes),
        "pdf" => parse_pdf(&request.bytes),
        _ => Err(AppError::Parser(format!(
            "unsupported file extension: {extension}"
        ))),
    }
}

pub fn content_hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn parse_text(bytes: &[u8], markdown: bool) -> AppResult<ParseResult> {
    let text = String::from_utf8(bytes.to_vec())
        .map_err(|_| AppError::Parser("text file is not valid UTF-8".into()))?;
    let blocks = chunk_structured_text(&text, markdown);
    Ok(ParseResult {
        blocks,
        warnings: Vec::new(),
    })
}

fn parse_csv(bytes: &[u8]) -> AppResult<ParseResult> {
    let mut reader = csv::ReaderBuilder::new()
        .flexible(true)
        .from_reader(Cursor::new(bytes));
    let headers = reader
        .headers()
        .map_err(|error| AppError::Parser(error.to_string()))?
        .clone();
    let mut blocks = Vec::new();
    let mut group = Vec::new();
    let mut group_start = 2usize;
    for (index, record) in reader.records().enumerate() {
        let record = record.map_err(|error| AppError::Parser(error.to_string()))?;
        if group.is_empty() {
            group_start = index + 2;
        }
        group.push(
            headers
                .iter()
                .zip(record.iter())
                .map(|(header, value)| format!("{header}: {value}"))
                .collect::<Vec<_>>()
                .join(" · "),
        );
        if group.len() >= 25 {
            let end = group_start + group.len() - 1;
            blocks.push(ParsedBlock {
                text: group.join("\n"),
                location: SourceLocation {
                    sheet: Some("CSV".into()),
                    cell_range: Some(format!("rows {group_start}-{end}")),
                    ..Default::default()
                },
                ocr: false,
                ocr_confidence: None,
            });
            group.clear();
        }
    }
    if !group.is_empty() {
        let end = group_start + group.len() - 1;
        blocks.push(ParsedBlock {
            text: group.join("\n"),
            location: SourceLocation {
                sheet: Some("CSV".into()),
                cell_range: Some(format!("rows {group_start}-{end}")),
                ..Default::default()
            },
            ocr: false,
            ocr_confidence: None,
        });
    }
    Ok(ParseResult {
        blocks,
        warnings: Vec::new(),
    })
}

fn parse_spreadsheet(bytes: &[u8]) -> AppResult<ParseResult> {
    let cursor = Cursor::new(bytes.to_vec());
    let mut workbook =
        open_workbook_auto_from_rs(cursor).map_err(|error| AppError::Parser(error.to_string()))?;
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    for sheet_name in workbook.sheet_names().to_vec() {
        let range = match workbook.worksheet_range(&sheet_name) {
            Ok(range) => range,
            Err(error) => {
                warnings.push(format!("Could not read worksheet {sheet_name}: {error}"));
                continue;
            }
        };
        let rows = range.rows().collect::<Vec<_>>();
        if rows.is_empty() {
            continue;
        }
        let headers = rows[0]
            .iter()
            .enumerate()
            .map(|(index, value)| {
                let value = cell_text(value);
                if value.is_empty() {
                    format!("Column {}", column_label(index))
                } else {
                    value
                }
            })
            .collect::<Vec<_>>();
        for (group_index, group) in rows[1..].chunks(25).enumerate() {
            let first_row = group_index * 25 + 2;
            let last_row = first_row + group.len().saturating_sub(1);
            let text = group
                .iter()
                .map(|row| {
                    headers
                        .iter()
                        .zip(row.iter())
                        .filter_map(|(header, value)| {
                            let value = cell_text(value);
                            (!value.is_empty()).then(|| format!("{header}: {value}"))
                        })
                        .collect::<Vec<_>>()
                        .join(" · ")
                })
                .filter(|row| !row.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            if text.is_empty() {
                continue;
            }
            let last_col = column_label(headers.len().saturating_sub(1));
            blocks.push(ParsedBlock {
                text,
                location: SourceLocation {
                    sheet: Some(sheet_name.clone()),
                    cell_range: Some(format!("A{first_row}:{last_col}{last_row}")),
                    ..Default::default()
                },
                ocr: false,
                ocr_confidence: None,
            });
        }
    }
    Ok(ParseResult { blocks, warnings })
}

fn parse_docx(bytes: &[u8]) -> AppResult<ParseResult> {
    let cursor = Cursor::new(bytes);
    let mut archive = zip::ZipArchive::new(cursor)
        .map_err(|error| AppError::Parser(format!("invalid DOCX archive: {error}")))?;
    validate_archive(&mut archive)?;
    let mut document = archive
        .by_name("word/document.xml")
        .map_err(|_| AppError::Parser("DOCX has no word/document.xml".into()))?;
    let mut xml = String::new();
    document
        .read_to_string(&mut xml)
        .map_err(|error| AppError::Parser(error.to_string()))?;
    let paragraphs = docx_paragraphs(&xml)?;
    let text = paragraphs.join("\n\n");
    Ok(ParseResult {
        blocks: chunk_structured_text(&text, false),
        warnings: Vec::new(),
    })
}

fn parse_pdf(bytes: &[u8]) -> AppResult<ParseResult> {
    // pdf-extract handles the font encodings and empty-password encryption that real-world PDFs
    // (IRS notices, bank statements) use and lopdf's own extractor returns nothing for. A bad
    // file must not take the parser down, so a panic is reported as an unreadable PDF.
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
        .map_err(|_| AppError::Parser("could not read this PDF".into()))?
        .map_err(|error| AppError::Parser(format!("could not open PDF: {error}")))?;
    let mut blocks = Vec::new();
    let mut warnings = Vec::new();
    for (index, text) in pages.iter().enumerate() {
        let page_number = index as u32 + 1;
        let readable = text.chars().filter(|c| !c.is_whitespace()).count() >= 40;
        if !readable {
            if let Some(text) = ocr_pdf_page(bytes, page_number, pages.len() as u32, &mut warnings)
            {
                for chunk in chunk_plain(&text, TARGET_CHUNK_CHARS) {
                    blocks.push(ParsedBlock {
                        text: chunk,
                        location: SourceLocation {
                            page: Some(page_number),
                            ..Default::default()
                        },
                        ocr: true,
                        ocr_confidence: None,
                    });
                }
                continue;
            }
            warnings.push(format!(
                "Page {page_number} has no usable text layer and could not be read by local OCR"
            ));
            continue;
        }
        for chunk in chunk_plain(text, TARGET_CHUNK_CHARS) {
            blocks.push(ParsedBlock {
                text: chunk,
                location: SourceLocation {
                    page: Some(page_number),
                    ..Default::default()
                },
                ocr: false,
                ocr_confidence: None,
            });
        }
    }
    Ok(ParseResult { blocks, warnings })
}

/// The production Windows package will bundle the same recognition model. On macOS development
/// builds, Vision is already part of the operating system, keeps pages local, and gives us a real
/// OCR path without sending a scan to any service.
#[cfg(target_os = "macos")]
fn ocr_pdf_page(
    bytes: &[u8],
    page: u32,
    _page_count: u32,
    warnings: &mut Vec<String>,
) -> Option<String> {
    let temp = std::env::temp_dir().join(format!(
        "private-ai-ocr-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos()
    ));
    if fs::create_dir_all(&temp).is_err() {
        return None;
    }
    let pdf = temp.join("scan.pdf");
    let image = temp.join("page.png");
    let result = (|| -> Option<String> {
        fs::write(&pdf, bytes).ok()?;
        let rendered = Command::new("pdftoppm")
            .args([
                "-f",
                &page.to_string(),
                "-l",
                &page.to_string(),
                "-r",
                "200",
                "-png",
                "-singlefile",
            ])
            .arg(&pdf)
            .arg(temp.join("page"))
            .status()
            .ok()?;
        if !rendered.success() || !image.is_file() {
            return None;
        }
        let helper = vision_helper().ok()?;
        let output = Command::new(helper).arg(&image).output().ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8(output.stdout).ok()?;
        (text.chars().filter(|c| !c.is_whitespace()).count() >= 8).then_some(text)
    })();
    let _ = fs::remove_dir_all(&temp);
    if result.is_none() {
        warnings.push(format!(
            "Page {page} local OCR did not produce readable text"
        ));
    }
    result
}

#[cfg(target_os = "macos")]
fn vision_helper() -> AppResult<PathBuf> {
    const SOURCE: &str = include_str!("../ocr/vision_ocr.swift");
    let root = dirs::cache_dir()
        .ok_or_else(|| AppError::Parser("no local cache directory is available for OCR".into()))?
        .join("PrivateAIConnectorWorkspace/ocr");
    fs::create_dir_all(&root).map_err(|error| AppError::Parser(error.to_string()))?;
    let binary = root.join("vision-ocr");
    if binary.is_file() {
        return Ok(binary);
    }
    let source = root.join("vision_ocr.swift");
    fs::write(&source, SOURCE).map_err(|error| AppError::Parser(error.to_string()))?;
    let status = Command::new("swiftc")
        .args([
            source.to_string_lossy().as_ref(),
            "-o",
            binary.to_string_lossy().as_ref(),
            "-framework",
            "Vision",
            "-framework",
            "AppKit",
        ])
        .status()
        .map_err(|error| AppError::Parser(format!("could not start local macOS OCR: {error}")))?;
    if !status.success() {
        return Err(AppError::Parser(
            "could not build local macOS OCR helper".into(),
        ));
    }
    Ok(binary)
}

#[cfg(not(target_os = "macos"))]
fn ocr_pdf_page(
    _bytes: &[u8],
    _page: u32,
    _page_count: u32,
    _warnings: &mut Vec<String>,
) -> Option<String> {
    None
}

fn chunk_structured_text(text: &str, markdown: bool) -> Vec<ParsedBlock> {
    let mut blocks = Vec::new();
    let mut heading: Option<String> = None;
    let mut buffer = String::new();
    for paragraph in text.split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        let is_heading = if markdown {
            paragraph.starts_with('#') && !paragraph.contains('\n')
        } else {
            paragraph.len() < 100
                && !paragraph.contains('\n')
                && (paragraph.ends_with(':') || paragraph.chars().all(|c| !c.is_lowercase()))
        };
        if is_heading {
            if !buffer.trim().is_empty() {
                push_text_chunks(&mut blocks, &buffer, heading.clone());
                buffer.clear();
            }
            heading = Some(
                paragraph
                    .trim_start_matches('#')
                    .trim_end_matches(':')
                    .trim()
                    .into(),
            );
            continue;
        }
        if buffer.len() + paragraph.len() + 2 > TARGET_CHUNK_CHARS && !buffer.is_empty() {
            push_text_chunks(&mut blocks, &buffer, heading.clone());
            buffer.clear();
        }
        if !buffer.is_empty() {
            buffer.push_str("\n\n");
        }
        buffer.push_str(paragraph);
    }
    if !buffer.trim().is_empty() {
        push_text_chunks(&mut blocks, &buffer, heading);
    }
    blocks
}

fn push_text_chunks(blocks: &mut Vec<ParsedBlock>, text: &str, heading: Option<String>) {
    for chunk in chunk_plain(text, TARGET_CHUNK_CHARS) {
        blocks.push(ParsedBlock {
            text: chunk,
            location: SourceLocation {
                heading: heading.clone(),
                ..Default::default()
            },
            ocr: false,
            ocr_confidence: None,
        });
    }
}

fn chunk_plain(text: &str, max_chars: usize) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    for line in text.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if current.chars().count() + line.chars().count() + 1 > max_chars && !current.is_empty() {
            chunks.push(current.trim().to_string());
            current.clear();
        }
        if !current.is_empty() {
            current.push('\n');
        }
        current.push_str(line);
    }
    if !current.trim().is_empty() {
        chunks.push(current.trim().to_string());
    }
    chunks
}

fn docx_paragraphs(xml: &str) -> AppResult<Vec<String>> {
    let mut reader = XmlReader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut paragraphs = Vec::new();
    let mut paragraph = String::new();
    loop {
        match reader.read_event() {
            Ok(Event::Text(text)) => {
                let value = text
                    .decode()
                    .map_err(|error| AppError::Parser(error.to_string()))?;
                paragraph.push_str(&value);
            }
            Ok(Event::End(tag)) if tag.name().as_ref() == b"w:p" => {
                if !paragraph.trim().is_empty() {
                    paragraphs.push(paragraph.trim().to_string());
                }
                paragraph.clear();
            }
            Ok(Event::Eof) => break,
            Err(error) => return Err(AppError::Parser(error.to_string())),
            _ => {}
        }
    }
    Ok(paragraphs)
}

fn validate_archive<R: Read + std::io::Seek>(archive: &mut zip::ZipArchive<R>) -> AppResult<()> {
    if archive.len() > MAX_ARCHIVE_ENTRIES {
        return Err(AppError::Parser("archive has too many entries".into()));
    }
    let mut expanded = 0u64;
    for index in 0..archive.len() {
        let file = archive
            .by_index(index)
            .map_err(|error| AppError::Parser(error.to_string()))?;
        if file.enclosed_name().is_none() {
            return Err(AppError::Parser("archive contains an unsafe path".into()));
        }
        expanded = expanded.saturating_add(file.size());
        if expanded > MAX_EXPANDED_BYTES {
            return Err(AppError::Parser(
                "expanded archive exceeds the 150 MB limit".into(),
            ));
        }
    }
    Ok(())
}

fn cell_text(value: &Data) -> String {
    match value {
        Data::Empty => String::new(),
        other => other.to_string(),
    }
}

fn column_label(mut index: usize) -> String {
    let mut label = String::new();
    loop {
        label.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    label
}

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "pdf"
                    | "docx"
                    | "xlsx"
                    | "xls"
                    | "xlsb"
                    | "ods"
                    | "csv"
                    | "txt"
                    | "md"
                    | "markdown"
            )
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_markdown_headings() {
        let result = parse_request(ParseRequest {
            file_name: "procedure.md".into(),
            extension: "md".into(),
            bytes: b"# Intake\n\nCollect the signed engagement letter.\n\n# Review\n\nConfirm the tax year."
                .to_vec(),
        })
        .unwrap();
        assert_eq!(result.blocks.len(), 2);
        assert_eq!(result.blocks[0].location.heading.as_deref(), Some("Intake"));
    }

    #[test]
    fn rejects_unsupported_files() {
        let error = parse_request(ParseRequest {
            file_name: "script.exe".into(),
            extension: "exe".into(),
            bytes: vec![0],
        })
        .unwrap_err();
        assert!(error.to_string().contains("unsupported"));
    }

    #[test]
    fn labels_spreadsheet_columns() {
        assert_eq!(column_label(0), "A");
        assert_eq!(column_label(25), "Z");
        assert_eq!(column_label(26), "AA");
    }
}
