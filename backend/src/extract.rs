//! Turns the bytes of a downloaded file into searchable text: PDFs that carry a text layer, and
//! Word, PowerPoint and Excel files. Scanned pages and photos have no text to read, so they come
//! back empty and the file is still findable by name.
//!
//! Files from clients are untrusted input. Everything here is bounded (input size, archive size,
//! text size, spreadsheet cells) and never trusts a size a file claims for itself; sizes are
//! enforced by counting the bytes actually read. In production this runs in a separate
//! locked-down process (see `extract_process`), so a hostile file can crash only that process.

use std::io::{Cursor, Read, Seek};

use quick_xml::{Reader, events::Event};
use serde::{Deserialize, Serialize};

/// Bump this when extraction improves (or records more about each file). Files read by an older
/// version are read again. Version 2: each name-only file records why. Version 3: no size or
/// length limits, and zip bombs are refused up front. Version 4: scanned pages and pictures are
/// read with OCR.
pub const PARSER_VERSION: i32 = 4;

#[derive(Debug, Clone)]
pub struct Limits {
    pub max_input_bytes: usize,
    pub max_entries: usize,
    /// Most bytes read from any one part of a zip file.
    pub max_part_bytes: u64,
    /// Most bytes read from all parts of a zip file together.
    pub max_total_bytes: u64,
    pub max_text_chars: usize,
    pub max_cells: usize,
}

/// There is no limit on how much a firm may store. These limits protect the reader's own memory
/// and time from files built to overwhelm it, and sit far above any real document.
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_input_bytes: 512 * 1024 * 1024,
            max_entries: 20_000,
            max_part_bytes: 512 * 1024 * 1024,
            max_total_bytes: 512 * 1024 * 1024,
            max_text_chars: 400_000_000,
            max_cells: 50_000_000,
        }
    }
}

/// A part of a zip file may be this many times larger once unpacked than it is inside the file.
/// Real XML documents compress by about 5 to 100 times; zip bombs by thousands to millions.
const MAX_EXPANSION_RATIO: u64 = 250;
/// Small parts are allowed some slack, so a tiny file is not judged by its ratio alone.
const EXPANSION_SLACK: u64 = 1024 * 1024;

/// A run of text with where it sits in the file ("Page 3", "Slide 2", "Sheet: Fees").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Section {
    pub label: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Extracted {
    pub sections: Vec<Section>,
    /// The file held more text than the limit allows, and the rest was dropped.
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExtractError {
    /// A type this reader does not handle (for example old .doc files).
    Unsupported,
    /// The file could not be read: damaged, protected, or not what its name says.
    Unreadable(String),
    TooLarge,
    /// The reading process ran too long. May be worth trying again later.
    TimedOut,
    /// The reading process died. May be worth trying again later.
    Crashed(String),
}

impl ExtractError {
    /// Whether trying the same file again could succeed.
    pub fn is_transient(&self) -> bool {
        matches!(self, ExtractError::TimedOut | ExtractError::Crashed(_))
    }
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ExtractError::Unsupported => write!(f, "unsupported file type"),
            ExtractError::Unreadable(why) => write!(f, "unreadable file: {why}"),
            ExtractError::TooLarge => write!(f, "file too large"),
            ExtractError::TimedOut => write!(f, "reading took too long"),
            ExtractError::Crashed(why) => write!(f, "reader failed: {why}"),
        }
    }
}

impl std::error::Error for ExtractError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Pdf,
    Docx,
    Pptx,
    Spreadsheet,
    /// A picture. It has no text layer of its own; any text in it comes from OCR.
    Image,
}

/// Which kind of file this is, going by its name and the type the provider reports. Old binary
/// Office formats (.doc, .ppt) are not handled.
pub fn kind_for(name: &str, mime: &str) -> Option<Kind> {
    let lower = name.to_lowercase();
    match lower.rsplit_once('.').map(|(_, extension)| extension) {
        Some("pdf") => return Some(Kind::Pdf),
        Some("docx" | "docm") => return Some(Kind::Docx),
        Some("pptx" | "pptm") => return Some(Kind::Pptx),
        Some("xlsx" | "xlsm" | "xls" | "xlsb" | "ods") => return Some(Kind::Spreadsheet),
        Some("png" | "jpg" | "jpeg" | "tif" | "tiff" | "bmp" | "gif" | "webp") => {
            return Some(Kind::Image);
        }
        _ => {}
    }
    match mime {
        "application/pdf" => Some(Kind::Pdf),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            Some(Kind::Docx)
        }
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => {
            Some(Kind::Pptx)
        }
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
        | "application/vnd.ms-excel"
        | "application/vnd.oasis.opendocument.spreadsheet" => Some(Kind::Spreadsheet),
        "image/png" | "image/jpeg" | "image/tiff" | "image/bmp" | "image/gif" | "image/webp" => {
            Some(Kind::Image)
        }
        _ => None,
    }
}

/// Counts text against the limit as it is collected.
struct Budget {
    remaining: usize,
    truncated: bool,
}

impl Budget {
    fn take(&mut self, text: &str) -> String {
        let count = text.chars().count();
        if count <= self.remaining {
            self.remaining -= count;
            return text.to_string();
        }
        self.truncated = true;
        let kept: String = text.chars().take(self.remaining).collect();
        self.remaining = 0;
        kept
    }

    fn section(&mut self, label: Option<String>, text: &str) -> Option<Section> {
        let text = text.trim();
        if text.is_empty() || self.remaining == 0 {
            if !text.is_empty() {
                self.truncated = true;
            }
            return None;
        }
        Some(Section {
            label,
            text: self.take(text),
        })
    }
}

pub fn extract(bytes: &[u8], name: &str, mime: &str) -> Result<Extracted, ExtractError> {
    extract_with(bytes, name, mime, &Limits::default())
}

pub fn extract_with(
    bytes: &[u8],
    name: &str,
    mime: &str,
    limits: &Limits,
) -> Result<Extracted, ExtractError> {
    if bytes.len() > limits.max_input_bytes {
        return Err(ExtractError::TooLarge);
    }
    let kind = kind_for(name, mime).ok_or(ExtractError::Unsupported)?;
    let mut budget = Budget {
        remaining: limits.max_text_chars,
        truncated: false,
    };
    let sections = match kind {
        Kind::Pdf => read_pdf(bytes, &mut budget)?,
        Kind::Docx => read_docx(bytes, limits, &mut budget)?,
        Kind::Pptx => read_pptx(bytes, limits, &mut budget)?,
        Kind::Spreadsheet => read_spreadsheet(bytes, limits, &mut budget)?,
        // Nothing to read without OCR; the OCR step handles pictures.
        Kind::Image => Vec::new(),
    };
    Ok(Extracted {
        sections,
        truncated: budget.truncated,
    })
}

fn unreadable(reason: &str) -> ExtractError {
    ExtractError::Unreadable(reason.into())
}

// ---- PDF ----

fn read_pdf(bytes: &[u8], budget: &mut Budget) -> Result<Vec<Section>, ExtractError> {
    if !bytes[..bytes.len().min(1024)]
        .windows(5)
        .any(|window| window == b"%PDF-")
    {
        return Err(unreadable("not a PDF"));
    }
    // The library can panic on damaged files; that must not take the process down.
    let pages = std::panic::catch_unwind(|| pdf_extract::extract_text_from_mem_by_pages(bytes))
        .map_err(|_| unreadable("could not read this PDF"))?
        .map_err(|_| unreadable("could not open this PDF"))?;
    let readable: usize = pages
        .iter()
        .map(|page| page.chars().filter(|c| !c.is_whitespace()).count())
        .sum();
    if readable == 0 {
        // Pages with no text layer are scans. Reading them needs OCR, which is a later step.
        return Ok(Vec::new());
    }
    Ok(pages
        .iter()
        .enumerate()
        .filter_map(|(index, page)| budget.section(Some(format!("Page {}", index + 1)), page))
        .collect())
}

// ---- Zip-based Office files ----

/// Looks over a zip file's table of contents before anything is unpacked, and turns away zip
/// bombs: too many entries, unsafe paths, any part that expands far more than real documents do,
/// or XML parts that together would unpack to more than `xml_cap` bytes. Pictures and other
/// media are never unpacked by this reader, so they do not count towards the cap.
fn inspect_zip<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    limits: &Limits,
    xml_cap: u64,
) -> Result<(), ExtractError> {
    if archive.len() > limits.max_entries {
        return Err(ExtractError::TooLarge);
    }
    let mut declared_xml = 0u64;
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|_| unreadable("damaged archive"))?;
        if entry.enclosed_name().is_none() {
            return Err(unreadable("archive contains an unsafe path"));
        }
        let (size, packed) = (entry.size(), entry.compressed_size());
        if size > packed.saturating_mul(MAX_EXPANSION_RATIO) + EXPANSION_SLACK {
            return Err(unreadable(
                "archive expands far more than a document should",
            ));
        }
        let name = entry.name().to_ascii_lowercase();
        if [".xml", ".rels", ".bin", ".csv", ".txt"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
        {
            declared_xml = declared_xml.saturating_add(size);
        }
    }
    // A file may lie about its sizes, so what is actually read is counted again below.
    if declared_xml > xml_cap {
        return Err(ExtractError::TooLarge);
    }
    Ok(())
}

fn open_zip<'a>(
    bytes: &'a [u8],
    limits: &Limits,
) -> Result<zip::ZipArchive<Cursor<&'a [u8]>>, ExtractError> {
    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).map_err(|_| unreadable("not an Office file"))?;
    inspect_zip(&mut archive, limits, limits.max_total_bytes)?;
    Ok(archive)
}

fn read_part<R: Read + Seek>(
    archive: &mut zip::ZipArchive<R>,
    name: &str,
    limits: &Limits,
    total: &mut u64,
) -> Result<Option<String>, ExtractError> {
    let Ok(entry) = archive.by_name(name) else {
        return Ok(None);
    };
    let mut buffer = Vec::new();
    entry
        .take(limits.max_part_bytes + 1)
        .read_to_end(&mut buffer)
        .map_err(|_| unreadable("damaged archive"))?;
    if buffer.len() as u64 > limits.max_part_bytes {
        return Err(ExtractError::TooLarge);
    }
    *total += buffer.len() as u64;
    if *total > limits.max_total_bytes {
        return Err(ExtractError::TooLarge);
    }
    Ok(Some(String::from_utf8_lossy(&buffer).into_owned()))
}

/// Pulls the visible text out of a Word or PowerPoint XML part. Matching is by element name
/// without its prefix: `t` holds text, `p` ends a paragraph, `tab` and `br` are breaks, and table
/// cells and rows get separators so a table row stays on one line.
fn xml_text(xml: &str) -> Result<String, ExtractError> {
    let mut reader = Reader::from_str(xml);
    // Spaces inside a text run matter ("Hello" + " world"), so nothing is trimmed here.
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let (mut in_text, mut cell_depth) = (false, 0usize);
    loop {
        match reader.read_event() {
            Ok(Event::Start(tag)) => match tag.local_name().as_ref() {
                b"t" => in_text = true,
                b"tc" => cell_depth += 1,
                _ => {}
            },
            Ok(Event::End(tag)) => match tag.local_name().as_ref() {
                b"t" => in_text = false,
                b"p" => out.push_str(if cell_depth > 0 { " " } else { "\n\n" }),
                b"tc" => {
                    cell_depth = cell_depth.saturating_sub(1);
                    out.truncate(out.trim_end_matches(' ').len());
                    out.push_str(" | ");
                }
                b"tr" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Empty(tag)) => match tag.local_name().as_ref() {
                b"tab" => out.push('\t'),
                b"br" | b"cr" => out.push('\n'),
                _ => {}
            },
            Ok(Event::Text(text)) if in_text => {
                out.push_str(&text.decode().map_err(|_| unreadable("damaged text"))?);
            }
            Ok(Event::CData(text)) if in_text => out.push_str(&String::from_utf8_lossy(&text)),
            // &amp; and friends arrive as separate events. Only the five built-in names and
            // numeric references are resolved, so a file cannot define entities to expand.
            Ok(Event::GeneralRef(reference)) if in_text => {
                if let Ok(Some(c)) = reference.resolve_char_ref() {
                    out.push(c);
                } else if let Ok(name) = reference.decode() {
                    match name.as_ref() {
                        "amp" => out.push('&'),
                        "lt" => out.push('<'),
                        "gt" => out.push('>'),
                        "quot" => out.push('"'),
                        "apos" => out.push('\''),
                        _ => {}
                    }
                }
            }
            Ok(Event::Eof) => break,
            Err(_) => return Err(unreadable("damaged document")),
            _ => {}
        }
    }
    Ok(tidy(&out))
}

/// Removes the separators left dangling at line ends and collapses long runs of blank lines.
fn tidy(text: &str) -> String {
    let mut lines: Vec<String> = text
        .lines()
        .map(|line| line.trim_end().trim_end_matches('|').trim_end().to_string())
        .collect();
    lines.dedup_by(|a, b| a.is_empty() && b.is_empty());
    lines.join("\n")
}

fn read_docx(
    bytes: &[u8],
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Vec<Section>, ExtractError> {
    let mut archive = open_zip(bytes, limits)?;
    let mut total = 0;
    let body = read_part(&mut archive, "word/document.xml", limits, &mut total)?
        .ok_or_else(|| unreadable("not a Word document"))?;
    let mut text = xml_text(&body)?;
    for extra in ["word/footnotes.xml", "word/endnotes.xml"] {
        if let Some(xml) = read_part(&mut archive, extra, limits, &mut total)? {
            let notes = xml_text(&xml)?;
            if !notes.trim().is_empty() {
                text.push_str("\n\n");
                text.push_str(&notes);
            }
        }
    }
    Ok(budget.section(None, &text).into_iter().collect())
}

fn read_pptx(
    bytes: &[u8],
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Vec<Section>, ExtractError> {
    let mut archive = open_zip(bytes, limits)?;
    let mut slides: Vec<(u32, String)> = archive
        .file_names()
        .filter_map(|name| {
            let number = name
                .strip_prefix("ppt/slides/slide")?
                .strip_suffix(".xml")?;
            Some((number.parse().ok()?, name.to_string()))
        })
        .collect();
    if slides.is_empty() {
        return Err(unreadable("not a presentation"));
    }
    // Numeric order, so slide 10 follows slide 9 rather than slide 1.
    slides.sort();
    let mut total = 0;
    let mut sections = Vec::new();
    for (number, name) in slides {
        if let Some(xml) = read_part(&mut archive, &name, limits, &mut total)? {
            sections.extend(budget.section(Some(format!("Slide {number}")), &xml_text(&xml)?));
        }
    }
    Ok(sections)
}

// ---- Spreadsheets ----

fn read_spreadsheet(
    bytes: &[u8],
    limits: &Limits,
    budget: &mut Budget,
) -> Result<Vec<Section>, ExtractError> {
    use calamine::{Data, Reader as _, open_workbook_auto_from_rs};
    const ROWS_PER_PASSAGE: usize = 20;
    // The spreadsheet library unpacks sheets entirely into memory and takes no limits from us, so
    // newer (zip-based) workbooks are checked first. A workbook is held in memory several times
    // over once parsed, so the cap is lower than for documents.
    if bytes.starts_with(b"PK") {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes))
            .map_err(|_| unreadable("could not open this spreadsheet"))?;
        inspect_zip(&mut archive, limits, limits.max_total_bytes / 2)?;
    }
    let owned = bytes.to_vec();
    let sheets = std::panic::catch_unwind(move || {
        let mut workbook = open_workbook_auto_from_rs(Cursor::new(owned)).map_err(|_| ())?;
        let mut sheets = Vec::new();
        for name in workbook.sheet_names().to_vec() {
            let range = workbook.worksheet_range(&name).map_err(|_| ())?;
            let rows: Vec<Vec<String>> = range
                .rows()
                .map(|row| {
                    row.iter()
                        .filter(|cell| !matches!(cell, Data::Empty))
                        .map(|cell| cell.to_string())
                        .filter(|text| !text.trim().is_empty())
                        .collect()
                })
                .collect();
            sheets.push((name, rows));
        }
        Ok::<_, ()>(sheets)
    })
    .map_err(|_| unreadable("could not read this spreadsheet"))?
    .map_err(|_| unreadable("could not open this spreadsheet"))?;

    let mut cells = 0usize;
    let mut sections = Vec::new();
    for (name, rows) in sheets {
        let mut kept = Vec::new();
        for row in rows.into_iter().filter(|row| !row.is_empty()) {
            cells += row.len();
            if cells > limits.max_cells {
                budget.truncated = true;
                break;
            }
            kept.push(row.join(" | "));
        }
        // Blank lines between groups of rows let the sync split long sheets sensibly.
        let text = kept
            .chunks(ROWS_PER_PASSAGE)
            .map(|group| group.join("\n"))
            .collect::<Vec<_>>()
            .join("\n\n");
        sections.extend(budget.section(Some(format!("Sheet: {name}")), &text));
    }
    Ok(sections)
}

/// Small valid files built in code, for tests in this crate and the integration tests.
#[doc(hidden)]
pub mod testdata {
    use std::io::Write;

    use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

    fn escape(text: &str) -> String {
        text.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn zip(parts: &[(&str, String)]) -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        for (name, body) in parts {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body.as_bytes()).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// A zip with one part of the given size, all zeros. Compresses to almost nothing.
    pub fn zip_bomb(part: &str, size: usize) -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
        writer.start_file(part, options).unwrap();
        let block = vec![b' '; 1024 * 1024];
        let mut left = size;
        while left > 0 {
            let n = left.min(block.len());
            writer.write_all(&block[..n]).unwrap();
            left -= n;
        }
        writer.finish().unwrap().into_inner()
    }

    /// A zip with one stored (not compressed) part of the given size, filled with bytes that do
    /// not compress. It looks like a normal file to a ratio check, but is large.
    pub fn zip_large_part(part: &str, size: usize) -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        writer.start_file(part, options).unwrap();
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut block = vec![0u8; 64 * 1024];
        let mut left = size;
        while left > 0 {
            for byte in block.iter_mut() {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                *byte = b'a' + (state % 26) as u8;
            }
            let n = left.min(block.len());
            writer.write_all(&block[..n]).unwrap();
            left -= n;
        }
        writer.finish().unwrap().into_inner()
    }

    /// A zip holding the given parts, each stored as is. Used to hide one zip inside another.
    pub fn zip_with_parts(parts: &[(&str, &[u8])]) -> Vec<u8> {
        let mut writer = ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = SimpleFileOptions::default().compression_method(CompressionMethod::Stored);
        for (name, body) in parts {
            writer.start_file(*name, options).unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap().into_inner()
    }

    /// Each paragraph is a list of runs, so tests can check that spaces between runs survive.
    pub fn docx(paragraphs: &[&[&str]]) -> Vec<u8> {
        let body: String = paragraphs
            .iter()
            .map(|runs| {
                let runs: String = runs
                    .iter()
                    .map(|run| {
                        format!(
                            "<w:r><w:t xml:space=\"preserve\">{}</w:t></w:r>",
                            escape(run)
                        )
                    })
                    .collect();
                format!("<w:p>{runs}</w:p>")
            })
            .collect();
        zip(&[(
            "word/document.xml",
            format!(
                "<?xml version=\"1.0\"?><w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}</w:body></w:document>"
            ),
        )])
    }

    /// A Word file whose only content is a table with the given rows.
    pub fn docx_table(rows: &[&[&str]]) -> Vec<u8> {
        let table: String = rows
            .iter()
            .map(|row| {
                let cells: String = row
                    .iter()
                    .map(|cell| {
                        format!(
                            "<w:tc><w:p><w:r><w:t>{}</w:t></w:r></w:p></w:tc>",
                            escape(cell)
                        )
                    })
                    .collect();
                format!("<w:tr>{cells}</w:tr>")
            })
            .collect();
        zip(&[(
            "word/document.xml",
            format!(
                "<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:tbl>{table}</w:tbl></w:body></w:document>"
            ),
        )])
    }

    pub fn pptx(slides: &[(u32, &str)]) -> Vec<u8> {
        let mut parts = vec![("ppt/presentation.xml", "<p:presentation/>".to_string())];
        let names: Vec<String> = slides
            .iter()
            .map(|(n, _)| format!("ppt/slides/slide{n}.xml"))
            .collect();
        let bodies: Vec<String> = slides
            .iter()
            .map(|(_, text)| {
                format!(
                    "<p:sld xmlns:a=\"http://schemas.openxmlformats.org/drawingml/2006/main\" xmlns:p=\"http://schemas.openxmlformats.org/presentationml/2006/main\"><p:cSld><p:spTree><p:sp><p:txBody><a:p><a:r><a:t>{}</a:t></a:r></a:p></p:txBody></p:sp></p:spTree></p:cSld></p:sld>",
                    escape(text)
                )
            })
            .collect();
        for (name, body) in names.iter().zip(bodies) {
            parts.push((name.as_str(), body));
        }
        zip(&parts)
    }

    /// One sheet; cells are written as inline strings.
    pub fn xlsx(sheet: &str, rows: &[&[&str]]) -> Vec<u8> {
        let sheet_data: String = rows
            .iter()
            .enumerate()
            .map(|(r, row)| {
                let cells: String = row
                    .iter()
                    .enumerate()
                    .map(|(c, value)| {
                        format!(
                            "<c r=\"{}{}\" t=\"inlineStr\"><is><t>{}</t></is></c>",
                            (b'A' + c as u8) as char,
                            r + 1,
                            escape(value)
                        )
                    })
                    .collect();
                format!("<row r=\"{}\">{cells}</row>", r + 1)
            })
            .collect();
        zip(&[
            (
                "[Content_Types].xml",
                "<?xml version=\"1.0\"?><Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\"><Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/><Default Extension=\"xml\" ContentType=\"application/xml\"/><Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/><Override PartName=\"/xl/worksheets/sheet1.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/></Types>".to_string(),
            ),
            (
                "_rels/.rels",
                "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/workbook.xml",
                format!("<?xml version=\"1.0\"?><workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets><sheet name=\"{}\" sheetId=\"1\" r:id=\"rId1\"/></sheets></workbook>", escape(sheet)),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                "<?xml version=\"1.0\"?><Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\"><Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet1.xml\"/></Relationships>".to_string(),
            ),
            (
                "xl/worksheets/sheet1.xml",
                format!("<?xml version=\"1.0\"?><worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\"><sheetData>{sheet_data}</sheetData></worksheet>"),
            ),
        ])
    }

    /// A PDF with one page per entry, using a standard font so it needs no embedded data. An
    /// empty string makes a page with no text, like a scan.
    pub fn pdf(pages: &[&str]) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let page_count = pages.len();
        // 1 catalog, 2 page tree, 3 font, then a page object and a content stream per page.
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".into());
        let kids: String = (0..page_count)
            .map(|i| format!("{} 0 R ", 4 + i * 2))
            .collect();
        objects.push(format!(
            "<< /Type /Pages /Kids [{kids}] /Count {page_count} >>"
        ));
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".into());
        for (i, text) in pages.iter().enumerate() {
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents {} 0 R /Resources << /Font << /F1 3 0 R >> >> >>",
                5 + i * 2
            ));
            let escaped = text
                .replace('\\', "\\\\")
                .replace('(', "\\(")
                .replace(')', "\\)");
            let stream = if text.is_empty() {
                String::new()
            } else {
                format!("BT /F1 12 Tf 72 720 Td ({escaped}) Tj ET")
            };
            objects.push(format!(
                "<< /Length {} >>\nstream\n{stream}\nendstream",
                stream.len()
            ));
        }
        let mut out = b"%PDF-1.4\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(
            format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
        );
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::testdata::*;
    use super::*;

    const DOCX: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

    fn text(extracted: &Extracted) -> String {
        extracted
            .sections
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn word_text_keeps_spaces_between_runs_symbols_and_accents() {
        let file = docx(&[
            &["The ", "retainer", " is due"],
            &["Fees & costs: café < 30 days"],
        ]);
        let result = extract(&file, "Letter.docx", DOCX).unwrap();
        let text = text(&result);
        assert!(text.contains("The retainer is due"), "{text}");
        assert!(text.contains("Fees & costs: café < 30 days"), "{text}");
        assert!(!result.truncated && result.sections[0].label.is_none());
    }

    #[test]
    fn word_tables_keep_each_row_on_one_line() {
        let file = docx_table(&[&["Item", "Fee"], &["Retainer", "$5,000"]]);
        let text = text(&extract(&file, "Fees.docx", DOCX).unwrap());
        assert!(text.contains("Item | Fee"), "{text}");
        assert!(text.contains("Retainer | $5,000"), "{text}");
    }

    #[test]
    fn slides_are_labelled_and_kept_in_numeric_order() {
        let file = pptx(&[(10, "Last slide"), (2, "Second slide"), (1, "First slide")]);
        let result = extract(&file, "Deck.pptx", "").unwrap();
        let labels: Vec<_> = result
            .sections
            .iter()
            .map(|s| s.label.clone().unwrap())
            .collect();
        assert_eq!(labels, ["Slide 1", "Slide 2", "Slide 10"]);
        assert_eq!(result.sections[0].text, "First slide");
    }

    #[test]
    fn spreadsheets_give_one_labelled_section_per_sheet_with_rows_on_lines() {
        let file = xlsx(
            "Fees",
            &[
                &["Client", "Amount"],
                &["Acme", "5000"],
                &["Globex", "7200"],
            ],
        );
        let result = extract(&file, "Fees.xlsx", "").unwrap();
        assert_eq!(result.sections.len(), 1);
        assert_eq!(result.sections[0].label.as_deref(), Some("Sheet: Fees"));
        assert!(
            result.sections[0].text.contains("Acme | 5000"),
            "{}",
            result.sections[0].text
        );
        assert!(result.sections[0].text.contains("Globex | 7200"));
    }

    #[test]
    fn pdfs_give_one_section_per_page_with_page_numbers() {
        let file = pdf(&[
            "The retainer is due within thirty days.",
            "",
            "Interest accrues monthly.",
        ]);
        let result = extract(&file, "Letter.pdf", "application/pdf").unwrap();
        let labels: Vec<_> = result
            .sections
            .iter()
            .map(|s| s.label.clone().unwrap())
            .collect();
        assert_eq!(
            labels,
            ["Page 1", "Page 3"],
            "the empty page has no section"
        );
        assert!(
            result.sections[0]
                .text
                .contains("retainer is due within thirty days"),
            "{:?}",
            result.sections
        );
    }

    #[test]
    fn a_pdf_with_no_text_layer_comes_back_empty_rather_than_failing() {
        let result = extract(&pdf(&["", ""]), "Scan.pdf", "application/pdf").unwrap();
        assert!(result.sections.is_empty() && !result.truncated);
    }

    #[test]
    fn unsupported_damaged_and_mislabelled_files_are_refused_not_crashed_on() {
        assert_eq!(
            extract(b"whatever", "Old.doc", "application/msword"),
            Err(ExtractError::Unsupported)
        );
        // A picture is a recognised kind with no text layer, not an unsupported one.
        assert_eq!(
            extract(b"whatever", "photo.jpg", "image/jpeg"),
            Ok(Extracted::default())
        );
        for (name, bytes) in [
            ("a.pdf", b"not a pdf at all".to_vec()),
            ("a.pdf", b"%PDF-1.7 garbage garbage garbage".to_vec()),
            ("a.docx", b"PK\x03\x04 truncated".to_vec()),
            ("a.docx", Vec::new()),
            ("a.xlsx", b"not a workbook".to_vec()),
            ("a.pptx", docx(&[&["a word file named as slides"]])),
            ("a.docx", pdf(&["a pdf named as word"])),
            ("a.pdf", docx(&[&["a word file named as a pdf"]])),
        ] {
            assert!(
                matches!(extract(&bytes, name, ""), Err(ExtractError::Unreadable(_))),
                "{name}: {:?}",
                extract(&bytes, name, "")
            );
        }
    }

    #[test]
    fn a_zip_bomb_is_turned_away_before_anything_is_unpacked() {
        // 200 MB of spaces in a few hundred KB: the classic shape. The reader must refuse it from the
        // table of contents alone, which is also why this test is fast.
        let bomb = zip_bomb("word/document.xml", 200 * 1024 * 1024);
        assert!(
            bomb.len() < 1024 * 1024,
            "the file is small: {}",
            bomb.len()
        );
        let started = std::time::Instant::now();
        assert!(matches!(
            extract(&bomb, "bomb.docx", ""),
            Err(ExtractError::Unreadable(_))
        ));
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(matches!(
            extract(&bomb, "bomb.pptx", ""),
            Err(ExtractError::Unreadable(_))
        ));
    }

    #[test]
    fn a_spreadsheet_bomb_is_refused_instead_of_being_handed_to_the_spreadsheet_library() {
        // The spreadsheet library would unpack this entirely into memory.
        let bomb = zip_bomb("xl/worksheets/sheet1.xml", 200 * 1024 * 1024);
        assert_eq!(
            extract(&bomb, "bomb.xlsx", ""),
            Err(ExtractError::Unreadable(
                "archive expands far more than a document should".into()
            ))
        );
        // A workbook that is large but ordinary is over the spreadsheet cap, not a bomb.
        let limits = Limits {
            max_total_bytes: 2 * 1024 * 1024,
            ..Limits::default()
        };
        let large = zip_large_part("xl/worksheets/sheet1.xml", 1536 * 1024);
        assert_eq!(
            extract_with(&large, "large.xlsx", "", &limits),
            Err(ExtractError::TooLarge)
        );
    }

    #[test]
    fn an_archive_hidden_inside_a_document_is_never_unpacked() {
        let inner = zip_bomb("payload.xml", 200 * 1024 * 1024);
        let document = zip_with_parts(&[
            ("[Content_Types].xml", b"<Types/>"),
            (
                "word/document.xml",
                b"<w:document xmlns:w=\"w\"><w:body><w:p><w:r><w:t>Real text</w:t></w:r></w:p></w:body></w:document>",
            ),
            ("word/embeddings/oleObject1.bin", &inner),
        ]);
        let read = extract(&document, "with-attachment.docx", "").unwrap();
        assert!(
            read.sections
                .iter()
                .any(|section| section.text.contains("Real text"))
        );
    }

    #[test]
    fn a_large_real_document_is_read_in_full() {
        // About 20 MB of ordinary text across paragraphs, far above the old 5 MB text limit.
        let paragraph = "The retainer is due within thirty days of the invoice date. ";
        let runs: Vec<&str> = vec![paragraph; 40];
        let rows: Vec<&[&str]> = vec![&runs[..]; 8_000];
        let bytes = docx(&rows);
        let read = extract(&bytes, "big.docx", "").unwrap();
        let total: usize = read.sections.iter().map(|section| section.text.len()).sum();
        assert!(total > 15_000_000, "{total}");
        assert!(!read.truncated);
    }

    #[test]
    fn size_limits_are_enforced_by_counting_what_is_actually_read() {
        let limits = Limits {
            max_part_bytes: 1024 * 1024,
            max_total_bytes: 2 * 1024 * 1024,
            ..Limits::default()
        };
        // Bytes that do not compress look ordinary to a ratio check, so only counting what is
        // actually unpacked stops a part that is simply too big.
        let big = zip_large_part("word/document.xml", 3 * 1024 * 1024);
        assert_eq!(
            extract_with(&big, "big.docx", "", &limits),
            Err(ExtractError::TooLarge)
        );

        let too_big = Limits {
            max_input_bytes: 10,
            ..Limits::default()
        };
        assert_eq!(
            extract_with(&pdf(&["x"]), "a.pdf", "", &too_big),
            Err(ExtractError::TooLarge)
        );
        let few = Limits {
            max_entries: 1,
            ..Limits::default()
        };
        assert_eq!(
            extract_with(&pptx(&[(1, "a"), (2, "b")]), "a.pptx", "", &few),
            Err(ExtractError::TooLarge)
        );
    }

    #[test]
    fn long_text_is_cut_at_the_limit_and_says_so() {
        let long = "word ".repeat(100);
        let file = docx(&[&[long.as_str()], &[long.as_str()]]);
        let limits = Limits {
            max_text_chars: 300,
            ..Limits::default()
        };
        let result = extract_with(&file, "a.docx", "", &limits).unwrap();
        assert!(result.truncated);
        assert!(
            result
                .sections
                .iter()
                .map(|s| s.text.chars().count())
                .sum::<usize>()
                <= 300
        );

        let sheet = xlsx("S", &[&["a", "b"], &["c", "d"], &["e", "f"]]);
        let limits = Limits {
            max_cells: 3,
            ..Limits::default()
        };
        let result = extract_with(&sheet, "a.xlsx", "", &limits).unwrap();
        assert!(result.truncated, "rows past the cell limit are dropped");
    }

    #[test]
    fn xml_that_tries_to_define_entities_gets_nothing_expanded() {
        let evil = "<?xml version=\"1.0\"?><!DOCTYPE d [<!ENTITY x \"EXPANDED\"><!ENTITY y SYSTEM \"file:///etc/passwd\">]>\
            <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body><w:p><w:r><w:t>a&x;b&y;c</w:t></w:r></w:p></w:body></w:document>";
        let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
        use std::io::Write;
        writer
            .start_file(
                "word/document.xml",
                zip::write::SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(evil.as_bytes()).unwrap();
        let file = writer.finish().unwrap().into_inner();
        let text = text(&extract(&file, "evil.docx", "").unwrap());
        assert!(
            !text.contains("EXPANDED") && !text.contains("root:"),
            "{text}"
        );
        assert!(text.contains('a') && text.contains('c'));
    }

    #[test]
    fn file_kinds_are_recognised_by_name_or_reported_type() {
        assert_eq!(kind_for("A.PDF", ""), Some(Kind::Pdf));
        assert_eq!(kind_for("a.xls", ""), Some(Kind::Spreadsheet));
        assert_eq!(kind_for("noextension", "application/pdf"), Some(Kind::Pdf));
        assert_eq!(kind_for("notes.txt", "text/plain"), None);
        assert_eq!(kind_for("old.doc", "application/msword"), None);
        assert_eq!(kind_for("Scan.JPG", ""), Some(Kind::Image));
        assert_eq!(kind_for("noextension", "image/png"), Some(Kind::Image));
        assert_eq!(kind_for("logo.svg", "image/svg+xml"), None);
        assert!(ExtractError::TimedOut.is_transient() && !ExtractError::Unsupported.is_transient());
    }
}
