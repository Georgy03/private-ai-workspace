//! Turns a report written in Markdown into a Word (.docx) or Excel (.xlsx) file.
//!
//! The files are built here from plain XML inside a zip, so nothing is sent anywhere and nothing is
//! run. Text is escaped everywhere it is written, and spreadsheet cells are always plain values or
//! text, never formulas, so a report cannot plant a formula that runs when it is opened.

use std::io::{Cursor, Write};

use regex::Regex;
use zip::{write::SimpleFileOptions, CompressionMethod, ZipWriter};

#[derive(Debug, Clone, PartialEq)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
}

#[derive(Debug, Clone, PartialEq)]
enum Block {
    Heading(usize, Vec<Run>),
    Paragraph(Vec<Run>),
    Bullets(Vec<Vec<Run>>),
    Numbered(Vec<Vec<Run>>),
    Quote(Vec<Run>),
    Rule,
    Table { head: Vec<Vec<Run>>, rows: Vec<Vec<Vec<Run>>> },
}

fn runs(text: &str) -> Vec<Run> {
    let pattern = Regex::new(r"(\*\*[^*]+\*\*|`[^`]+`|\*[^*\s][^*]*\*)").expect("pattern");
    let mut out = Vec::new();
    let mut last = 0;
    let plain = |text: &str| Run { text: text.to_string(), bold: false, italic: false, code: false };
    for found in pattern.find_iter(text) {
        if found.start() > last {
            out.push(plain(&text[last..found.start()]));
        }
        let token = found.as_str();
        out.push(if let Some(inner) = token.strip_prefix("**") {
            Run { bold: true, ..plain(inner.trim_end_matches("**")) }
        } else if let Some(inner) = token.strip_prefix('`') {
            Run { code: true, ..plain(inner.trim_end_matches('`')) }
        } else {
            Run { italic: true, ..plain(token.trim_matches('*')) }
        });
        last = found.end();
    }
    if last < text.len() {
        out.push(plain(&text[last..]));
    }
    out
}

fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line.strip_suffix('|').unwrap_or(line);
    line.split('|').map(|cell| cell.trim().to_string()).collect()
}

fn is_table_rule(line: &str) -> bool {
    let cells = cells(line);
    !cells.is_empty()
        && cells.iter().all(|cell| {
            let cell = cell.trim_matches(':');
            cell.len() >= 2 && cell.chars().all(|c| c == '-')
        })
}

fn is_table_row(line: &str) -> bool {
    line.contains('|') && line.trim().len() > 1
}

fn bullet_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    for mark in ["- ", "* ", "• "] {
        if let Some(rest) = trimmed.strip_prefix(mark) {
            return Some(rest);
        }
    }
    None
}

fn number_text(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    let digits = trimmed.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return None;
    }
    let rest = &trimmed[digits..];
    let rest = rest.strip_prefix('.').or_else(|| rest.strip_prefix(')'))?;
    rest.strip_prefix(' ').or_else(|| rest.strip_prefix('\t'))
}

fn parse(markdown: &str) -> Vec<Block> {
    let text = markdown.replace("\r\n", "\n");
    let lines: Vec<&str> = text.split('\n').collect();
    let mut blocks = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() {
            i += 1;
        } else if let Some(level) = heading_level(trimmed) {
            blocks.push(Block::Heading(level, runs(trimmed[level..].trim())));
            i += 1;
        } else if trimmed.len() >= 3 && (trimmed.chars().all(|c| c == '-') || trimmed.chars().all(|c| c == '*')) {
            blocks.push(Block::Rule);
            i += 1;
        } else if is_table_row(line) && i + 1 < lines.len() && is_table_rule(lines[i + 1]) {
            let head: Vec<Vec<Run>> = cells(line).iter().map(|c| runs(c)).collect();
            i += 2;
            let mut rows = Vec::new();
            while i < lines.len() && is_table_row(lines[i]) {
                let mut row: Vec<Vec<Run>> = cells(lines[i]).iter().map(|c| runs(c)).collect();
                row.resize(head.len(), Vec::new());
                row.truncate(head.len());
                rows.push(row);
                i += 1;
            }
            blocks.push(Block::Table { head, rows });
        } else if bullet_text(line).is_some() {
            let mut items = Vec::new();
            while i < lines.len() {
                match bullet_text(lines[i]) {
                    Some(item) => items.push(runs(item.trim())),
                    None => break,
                }
                i += 1;
            }
            blocks.push(Block::Bullets(items));
        } else if number_text(line).is_some() {
            let mut items = Vec::new();
            while i < lines.len() {
                match number_text(lines[i]) {
                    Some(item) => items.push(runs(item.trim())),
                    None => break,
                }
                i += 1;
            }
            blocks.push(Block::Numbered(items));
        } else if trimmed.starts_with('>') {
            let mut quoted = Vec::new();
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                quoted.push(lines[i].trim_start().trim_start_matches('>').trim().to_string());
                i += 1;
            }
            blocks.push(Block::Quote(runs(&quoted.join(" "))));
        } else {
            let mut paragraph = Vec::new();
            while i < lines.len() {
                let current = lines[i];
                let t = current.trim();
                let starts_table = is_table_row(current) && i + 1 < lines.len() && is_table_rule(lines[i + 1]);
                if t.is_empty()
                    || heading_level(t).is_some()
                    || t.starts_with('>')
                    || bullet_text(current).is_some()
                    || number_text(current).is_some()
                    || starts_table
                {
                    break;
                }
                paragraph.push(t.to_string());
                i += 1;
            }
            if paragraph.is_empty() {
                paragraph.push(lines[i].trim().to_string());
                i += 1;
            }
            blocks.push(Block::Paragraph(runs(&paragraph.join(" "))));
        }
    }
    blocks
}

fn heading_level(line: &str) -> Option<usize> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && line[hashes..].starts_with(' ') {
        Some(hashes)
    } else {
        None
    }
}

fn plain(runs: &[Run]) -> String {
    runs.iter().map(|run| run.text.as_str()).collect()
}

/// Escapes text for XML and drops characters XML cannot hold at all.
fn xml(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            '\t' | '\n' | '\r' => out.push(c),
            c if (c as u32) < 0x20 => {}
            '\u{FFFE}' | '\u{FFFF}' => {}
            c => out.push(c),
        }
    }
    out
}

fn archive<N: AsRef<str>>(files: Vec<(N, String)>) -> Result<Vec<u8>, String> {
    let mut writer = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(CompressionMethod::Deflated);
    for (name, content) in files {
        writer.start_file(name.as_ref(), options).map_err(|error| error.to_string())?;
        writer.write_all(content.as_bytes()).map_err(|error| error.to_string())?;
    }
    Ok(writer.finish().map_err(|error| error.to_string())?.into_inner())
}

// ---------------------------------------------------------------- Word

fn docx_runs(runs: &[Run], extra: &str) -> String {
    runs.iter()
        .filter(|run| !run.text.is_empty())
        .map(|run| {
            let mut props = String::from(extra);
            if run.bold {
                props.push_str("<w:b/>");
            }
            if run.italic {
                props.push_str("<w:i/>");
            }
            if run.code {
                props.push_str("<w:rFonts w:ascii=\"Menlo\" w:hAnsi=\"Menlo\" w:cs=\"Menlo\"/>");
            }
            let props = if props.is_empty() { String::new() } else { format!("<w:rPr>{props}</w:rPr>") };
            format!("<w:r>{props}<w:t xml:space=\"preserve\">{}</w:t></w:r>", xml(&run.text))
        })
        .collect()
}

fn docx_paragraph(properties: &str, content: &str) -> String {
    let properties = if properties.is_empty() { String::new() } else { format!("<w:pPr>{properties}</w:pPr>") };
    format!("<w:p>{properties}{content}</w:p>")
}

fn docx_table(head: &[Vec<Run>], rows: &[Vec<Vec<Run>>]) -> String {
    let columns = head.len().max(1);
    let width = 9360 / columns;
    let mut out = String::from(
        "<w:tbl><w:tblPr><w:tblW w:w=\"9360\" w:type=\"dxa\"/><w:tblBorders>\
         <w:top w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>\
         <w:left w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>\
         <w:bottom w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>\
         <w:right w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>\
         <w:insideH w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>\
         <w:insideV w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"999999\"/>\
         </w:tblBorders><w:tblLayout w:type=\"fixed\"/><w:tblCellMar>\
         <w:left w:w=\"80\" w:type=\"dxa\"/><w:right w:w=\"80\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr><w:tblGrid>",
    );
    for _ in 0..columns {
        out.push_str(&format!("<w:gridCol w:w=\"{width}\"/>"));
    }
    out.push_str("</w:tblGrid>");
    let cell = |content: &[Run], header: bool| {
        let shade = if header { "<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"EEEEEE\"/>" } else { "" };
        let extra = if header { "<w:b/>" } else { "" };
        format!(
            "<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/>{shade}</w:tcPr>{}</w:tc>",
            docx_paragraph("", &docx_runs(content, extra))
        )
    };
    out.push_str("<w:tr><w:trPr><w:tblHeader/></w:trPr>");
    for head_cell in head {
        out.push_str(&cell(head_cell, true));
    }
    out.push_str("</w:tr>");
    for row in rows {
        out.push_str("<w:tr>");
        for column in 0..columns {
            out.push_str(&cell(row.get(column).map(Vec::as_slice).unwrap_or(&[]), false));
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
    // Word needs a paragraph after a table, and it keeps the next block from touching the grid.
    out.push_str(&docx_paragraph("", ""));
    out
}

/// Builds a Word document from a Markdown report.
pub fn to_docx(title: &str, markdown: &str) -> Result<Vec<u8>, String> {
    let mut body = String::new();
    if !title.trim().is_empty() {
        body.push_str(&docx_paragraph("<w:pStyle w:val=\"Title\"/>", &docx_runs(&runs(title.trim()), "")));
    }
    for block in parse(markdown) {
        match block {
            Block::Heading(level, content) => {
                let style = format!("<w:pStyle w:val=\"Heading{}\"/>", level.min(3));
                body.push_str(&docx_paragraph(&style, &docx_runs(&content, "")));
            }
            Block::Paragraph(content) => body.push_str(&docx_paragraph("", &docx_runs(&content, ""))),
            Block::Bullets(items) => {
                for item in items {
                    let mut content = docx_runs(&[Run { text: "•\t".into(), bold: false, italic: false, code: false }], "");
                    content.push_str(&docx_runs(&item, ""));
                    body.push_str(&docx_paragraph("<w:ind w:left=\"720\" w:hanging=\"360\"/>", &content));
                }
            }
            Block::Numbered(items) => {
                for (n, item) in items.iter().enumerate() {
                    let mut content = docx_runs(&[Run { text: format!("{}.\t", n + 1), bold: false, italic: false, code: false }], "");
                    content.push_str(&docx_runs(item, ""));
                    body.push_str(&docx_paragraph("<w:ind w:left=\"720\" w:hanging=\"360\"/>", &content));
                }
            }
            Block::Quote(content) => body.push_str(&docx_paragraph(
                "<w:pBdr><w:left w:val=\"single\" w:sz=\"12\" w:space=\"8\" w:color=\"999999\"/></w:pBdr><w:ind w:left=\"360\"/>",
                &docx_runs(&content, "<w:i/><w:color w:val=\"555555\"/>"),
            )),
            Block::Rule => body.push_str(&docx_paragraph(
                "<w:pBdr><w:bottom w:val=\"single\" w:sz=\"6\" w:space=\"1\" w:color=\"999999\"/></w:pBdr>",
                "",
            )),
            Block::Table { head, rows } => body.push_str(&docx_table(&head, &rows)),
        }
    }
    let document = format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}\
         <w:sectPr><w:pgSz w:w=\"12240\" w:h=\"15840\"/><w:pgMar w:top=\"1440\" w:right=\"1440\" w:bottom=\"1440\" w:left=\"1440\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>\
         </w:body></w:document>"
    );
    let styles = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
        <w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
        <w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:cs=\"Calibri\"/><w:sz w:val=\"22\"/></w:rPr></w:rPrDefault>\
        <w:pPrDefault><w:pPr><w:spacing w:after=\"120\" w:line=\"264\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
        <w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/></w:style>\
        <w:style w:type=\"paragraph\" w:styleId=\"Title\"><w:name w:val=\"Title\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:spacing w:after=\"240\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"40\"/></w:rPr></w:style>\
        <w:style w:type=\"paragraph\" w:styleId=\"Heading1\"><w:name w:val=\"heading 1\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"320\" w:after=\"120\"/><w:outlineLvl w:val=\"0\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"32\"/></w:rPr></w:style>\
        <w:style w:type=\"paragraph\" w:styleId=\"Heading2\"><w:name w:val=\"heading 2\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"240\" w:after=\"100\"/><w:outlineLvl w:val=\"1\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"28\"/></w:rPr></w:style>\
        <w:style w:type=\"paragraph\" w:styleId=\"Heading3\"><w:name w:val=\"heading 3\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:pPr><w:keepNext/><w:spacing w:before=\"200\" w:after=\"80\"/><w:outlineLvl w:val=\"2\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"24\"/></w:rPr></w:style>\
        </w:styles>";
    archive(vec![
        (
            "[Content_Types].xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
             <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
             <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
             <Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
             <Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
             </Types>"
                .into(),
        ),
        (
            "_rels/.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
             </Relationships>"
                .into(),
        ),
        (
            "word/_rels/document.xml.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
             </Relationships>"
                .into(),
        ),
        ("word/styles.xml", styles.into()),
        ("word/document.xml", document),
    ])
}

// --------------------------------------------------------------- Excel

fn column_name(mut index: usize) -> String {
    let mut name = String::new();
    loop {
        name.insert(0, (b'A' + (index % 26) as u8) as char);
        if index < 26 {
            break;
        }
        index = index / 26 - 1;
    }
    name
}

/// A number only when the whole cell is one ("$1,200.50", "(300)"). Anything that could lose
/// something by becoming a number (leading zeros, very long ids, signs, text) stays text.
fn as_number(text: &str) -> Option<f64> {
    let mut t = text.trim().to_string();
    if t.is_empty() {
        return None;
    }
    let negative = t.starts_with('(') && t.ends_with(')');
    if negative {
        t = t[1..t.len() - 1].trim().to_string();
    }
    if let Some(rest) = t.strip_prefix('$') {
        t = rest.trim().to_string();
    }
    if t.is_empty() || !t.chars().all(|c| c.is_ascii_digit() || c == ',' || c == '.') {
        return None;
    }
    if t.starts_with(',') || t.ends_with(',') || t.starts_with('.') || t.ends_with('.') || t.matches('.').count() > 1 {
        return None;
    }
    let digits = t.replace(',', "");
    if digits.chars().filter(char::is_ascii_digit).count() > 15 {
        return None;
    }
    if digits.len() > 1 && digits.starts_with('0') && !digits.starts_with("0.") {
        return None;
    }
    let value: f64 = digits.parse().ok()?;
    Some(if negative { -value } else { value })
}

fn sheet_cell(row: usize, column: usize, text: &str, bold: bool) -> String {
    let reference = format!("{}{}", column_name(column), row + 1);
    let style = if bold { " s=\"1\"" } else { "" };
    if text.is_empty() {
        return String::new();
    }
    if !bold {
        if let Some(number) = as_number(text) {
            return format!("<c r=\"{reference}\"{style}><v>{number}</v></c>");
        }
    }
    format!("<c r=\"{reference}\"{style} t=\"inlineStr\"><is><t xml:space=\"preserve\">{}</t></is></c>", xml(text))
}

struct Sheet {
    name: String,
    rows: Vec<Vec<String>>,
    header: bool,
}

fn sheet_xml(sheet: &Sheet) -> String {
    let columns = sheet.rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <worksheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">",
    );
    if sheet.header {
        out.push_str("<sheetViews><sheetView workbookViewId=\"0\"><pane ySplit=\"1\" topLeftCell=\"A2\" activePane=\"bottomLeft\" state=\"frozen\"/></sheetView></sheetViews>");
    }
    out.push_str("<cols>");
    for column in 0..columns {
        let longest = sheet.rows.iter().filter_map(|row| row.get(column)).map(|cell| cell.chars().count()).max().unwrap_or(8);
        let width = (longest + 2).clamp(10, 60);
        out.push_str(&format!("<col min=\"{0}\" max=\"{0}\" width=\"{width}\" customWidth=\"1\"/>", column + 1));
    }
    out.push_str("</cols><sheetData>");
    for (r, row) in sheet.rows.iter().enumerate() {
        out.push_str(&format!("<row r=\"{}\">", r + 1));
        for (c, text) in row.iter().enumerate() {
            out.push_str(&sheet_cell(r, c, text, sheet.header && r == 0));
        }
        out.push_str("</row>");
    }
    out.push_str("</sheetData></worksheet>");
    out
}

fn sheet_name(wanted: &str, taken: &[String]) -> String {
    let cleaned: String = wanted
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | ':' | '*' | '?' | '/' | '\\'))
        .collect();
    let cleaned = cleaned.trim().trim_matches('\'').to_string();
    let base: String = if cleaned.is_empty() { "Table".into() } else { cleaned.chars().take(28).collect() };
    let mut name = base.clone();
    let mut n = 2;
    while taken.iter().any(|t| t.eq_ignore_ascii_case(&name)) {
        name = format!("{base} {n}");
        n += 1;
    }
    name
}

/// Builds an Excel workbook from a Markdown report: one sheet for each table, named after the
/// heading above it. A report with no tables becomes one sheet with a line of text in each row.
pub fn to_xlsx(markdown: &str) -> Result<Vec<u8>, String> {
    let blocks = parse(markdown);
    let mut sheets: Vec<Sheet> = Vec::new();
    let mut heading = String::new();
    for block in &blocks {
        match block {
            Block::Heading(_, content) => heading = plain(content),
            Block::Table { head, rows } => {
                let taken: Vec<String> = sheets.iter().map(|s| s.name.clone()).collect();
                let name = sheet_name(if heading.is_empty() { "Table" } else { &heading }, &taken);
                let mut grid = vec![head.iter().map(|c| plain(c)).collect::<Vec<_>>()];
                grid.extend(rows.iter().map(|row| row.iter().map(|c| plain(c)).collect()));
                sheets.push(Sheet { name, rows: grid, header: true });
            }
            _ => {}
        }
    }
    if sheets.is_empty() {
        let mut lines = Vec::new();
        for block in &blocks {
            match block {
                Block::Heading(_, c) | Block::Paragraph(c) | Block::Quote(c) => lines.push(vec![plain(c)]),
                Block::Bullets(items) | Block::Numbered(items) => {
                    lines.extend(items.iter().map(|item| vec![plain(item)]));
                }
                _ => {}
            }
        }
        if lines.is_empty() {
            lines.push(vec![String::new()]);
        }
        sheets.push(Sheet { name: "Report".into(), rows: lines, header: false });
    }

    let mut files: Vec<(String, String)> = Vec::new();
    let mut content_types = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
         <Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
         <Default Extension=\"xml\" ContentType=\"application/xml\"/>\
         <Override PartName=\"/xl/workbook.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml\"/>\
         <Override PartName=\"/xl/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml\"/>",
    );
    let mut workbook = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <workbook xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\" xmlns:r=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships\"><sheets>",
    );
    let mut relations = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">",
    );
    for (n, sheet) in sheets.iter().enumerate() {
        let id = n + 1;
        content_types.push_str(&format!(
            "<Override PartName=\"/xl/worksheets/sheet{id}.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml\"/>"
        ));
        workbook.push_str(&format!("<sheet name=\"{}\" sheetId=\"{id}\" r:id=\"rId{id}\"/>", xml(&sheet.name)));
        relations.push_str(&format!(
            "<Relationship Id=\"rId{id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet\" Target=\"worksheets/sheet{id}.xml\"/>"
        ));
        files.push((format!("xl/worksheets/sheet{id}.xml"), sheet_xml(sheet)));
    }
    let styles_id = sheets.len() + 1;
    relations.push_str(&format!(
        "<Relationship Id=\"rId{styles_id}\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/></Relationships>"
    ));
    content_types.push_str("</Types>");
    workbook.push_str("</sheets></workbook>");

    let fixed: Vec<(&str, String)> = vec![
        ("[Content_Types].xml", content_types),
        (
            "_rels/.rels",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
             <Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"xl/workbook.xml\"/>\
             </Relationships>"
                .into(),
        ),
        ("xl/workbook.xml", workbook),
        ("xl/_rels/workbook.xml.rels", relations),
        (
            "xl/styles.xml",
            "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
             <styleSheet xmlns=\"http://schemas.openxmlformats.org/spreadsheetml/2006/main\">\
             <fonts count=\"2\"><font><sz val=\"11\"/><name val=\"Calibri\"/></font><font><b/><sz val=\"11\"/><name val=\"Calibri\"/></font></fonts>\
             <fills count=\"2\"><fill><patternFill patternType=\"none\"/></fill><fill><patternFill patternType=\"gray125\"/></fill></fills>\
             <borders count=\"1\"><border><left/><right/><top/><bottom/><diagonal/></border></borders>\
             <cellStyleXfs count=\"1\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\"/></cellStyleXfs>\
             <cellXfs count=\"2\"><xf numFmtId=\"0\" fontId=\"0\" fillId=\"0\" borderId=\"0\" xfId=\"0\"/><xf numFmtId=\"0\" fontId=\"1\" fillId=\"0\" borderId=\"0\" xfId=\"0\" applyFont=\"1\"/></cellXfs>\
             <cellStyles count=\"1\"><cellStyle name=\"Normal\" xfId=\"0\" builtinId=\"0\"/></cellStyles>\
             </styleSheet>"
                .into(),
        ),
    ];
    let mut parts: Vec<(String, String)> = fixed.into_iter().map(|(name, content)| (name.to_string(), content)).collect();
    parts.extend(files);
    archive(parts)
}

/// Which file the person is asking the chat to make, if any: "xlsx" for a spreadsheet, "docx" for
/// a Word document. It needs both a word for making something and the kind of file, so a question
/// about an existing spreadsheet ("what does the budget sheet say") is answered as usual.
pub fn wanted_file(question: &str) -> Option<&'static str> {
    let q = question.to_lowercase();
    let makes = ["create", "make", "build", "generate", "put together", "prepare", "produce", "export", "draft", "write", "give me", "i need", "i want", "turn "]
        .iter()
        .any(|word| q.contains(word));
    if !makes {
        return None;
    }
    let has = |words: &[&str]| words.iter().any(|word| q.contains(word));
    if has(&["excel", "spreadsheet", "xlsx", "worksheet", " sheet"]) {
        Some("xlsx")
    } else if has(&["word document", "word doc", "word file", "docx", "in word"]) {
        Some("docx")
    } else {
        None
    }
}

/// The question without the words that ask for a file, so the search looks for the subject
/// ("mens retreat expenses") and not for the words "create", "excel" or "sheet".
pub fn without_file_request(question: &str) -> String {
    const ASKING: &[&str] = &[
        "create", "make", "build", "generate", "prepare", "produce", "export", "draft", "write", "give", "put", "together", "turn", "into",
        "me", "us", "i", "need", "want", "a", "an", "please", "can", "you", "could", "excel", "spreadsheet", "spreadsheets", "xlsx", "worksheet",
        "sheet", "word", "document", "doc", "docx", "file",
    ];
    if wanted_file(question).is_none() {
        return question.to_string();
    }
    let kept: Vec<&str> = question
        .split_whitespace()
        .filter(|word| {
            let bare: String = word.chars().filter(|c| c.is_alphanumeric()).collect::<String>().to_lowercase();
            !ASKING.contains(&bare.as_str())
        })
        .collect();
    if kept.is_empty() {
        question.to_string()
    } else {
        kept.join(" ")
    }
}

/// A file name that is safe to suggest in a save dialog: no folders, no odd characters.
pub fn file_stem(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|c| if c.is_alphanumeric() || c == ' ' || c == '-' || c == '_' { c } else { ' ' })
        .collect();
    let joined = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let stem: String = joined.chars().take(60).collect();
    if stem.is_empty() {
        "Report".into()
    } else {
        stem
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    const REPORT: &str = "## Expense summary\n\nTotals are **checked** and `final`.\n\n| Item | Cost | Id |\n|---|---|---|\n| Hotel | $1,200.50 | 00123 |\n| Taxi & tip | (45) | A-1 |\n| =HYPERLINK(\"x\") | 2024 | |\n\n- first <point>\n- second\n\n1. one\n2. two\n\n> quoted\n\n---\n\n### Other\n\n| A | B |\n|---|---|\n| 1 | 2 |\n";

    fn read(bytes: &[u8], name: &str) -> String {
        let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).expect("a zip");
        let mut text = String::new();
        archive.by_name(name).unwrap_or_else(|_| panic!("{name} missing")).read_to_string(&mut text).unwrap();
        text
    }

    fn well_formed(text: &str) {
        let mut reader = quick_xml::Reader::from_str(text);
        loop {
            match reader.read_event() {
                Ok(quick_xml::events::Event::Eof) => break,
                Ok(_) => {}
                Err(error) => panic!("bad xml: {error}\n{text}"),
            }
        }
    }

    #[test]
    fn word_file_has_every_part_and_escapes_text() {
        let bytes = to_docx("Q3 <report> & more", REPORT).unwrap();
        for part in ["[Content_Types].xml", "_rels/.rels", "word/document.xml", "word/styles.xml", "word/_rels/document.xml.rels"] {
            well_formed(&read(&bytes, part));
        }
        let document = read(&bytes, "word/document.xml");
        assert!(document.contains("Q3 &lt;report&gt; &amp; more"));
        assert!(document.contains("first &lt;point&gt;"));
        assert!(document.contains("Taxi &amp; tip"));
        assert!(document.contains("<w:pStyle w:val=\"Heading2\"/>"));
        assert!(document.contains("<w:tbl>"));
        assert_eq!(document.matches("<w:tbl>").count(), 2);
        assert!(document.contains("<w:b/>"), "bold text and table headers are bold");
        assert!(!document.contains("<script"), "nothing raw is passed through");
    }

    #[test]
    fn excel_file_has_a_sheet_for_each_table_named_after_its_heading() {
        let bytes = to_xlsx(REPORT).unwrap();
        for part in ["[Content_Types].xml", "xl/workbook.xml", "xl/_rels/workbook.xml.rels", "xl/styles.xml", "xl/worksheets/sheet1.xml", "xl/worksheets/sheet2.xml"] {
            well_formed(&read(&bytes, part));
        }
        let workbook = read(&bytes, "xl/workbook.xml");
        assert!(workbook.contains("name=\"Expense summary\""));
        assert!(workbook.contains("name=\"Other\""));
        let sheet = read(&bytes, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains("<v>1200.5</v>"), "money becomes a number: {sheet}");
        assert!(sheet.contains("<v>-45</v>"), "brackets mean negative");
        assert!(sheet.contains("<v>2024</v>"));
        assert!(sheet.contains(">00123<"), "ids with leading zeros stay text");
        assert!(sheet.contains("Taxi &amp; tip"));
    }

    #[test]
    fn a_cell_that_looks_like_a_formula_is_only_ever_text() {
        let bytes = to_xlsx(REPORT).unwrap();
        let sheet = read(&bytes, "xl/worksheets/sheet1.xml");
        assert!(!sheet.contains("<f>"), "no formulas are written");
        assert!(sheet.contains("t=\"inlineStr\"><is><t xml:space=\"preserve\">=HYPERLINK(&quot;x&quot;)</t>"));
    }

    #[test]
    fn a_report_without_tables_becomes_one_sheet_of_lines() {
        let bytes = to_xlsx("# Notes\n\nOne line.\n\n- a\n- b\n").unwrap();
        let workbook = read(&bytes, "xl/workbook.xml");
        assert!(workbook.contains("name=\"Report\""));
        let sheet = read(&bytes, "xl/worksheets/sheet1.xml");
        assert!(sheet.contains(">Notes<") && sheet.contains(">One line.<") && sheet.contains(">b<"));
        well_formed(&sheet);
    }

    #[test]
    fn an_empty_report_still_makes_valid_files() {
        well_formed(&read(&to_xlsx("").unwrap(), "xl/worksheets/sheet1.xml"));
        well_formed(&read(&to_docx("", "").unwrap(), "word/document.xml"));
    }

    #[test]
    fn sheet_names_are_cleaned_unique_and_short() {
        let taken = vec!["Totals".to_string()];
        assert_eq!(sheet_name("Totals", &taken), "Totals 2");
        assert_eq!(sheet_name("a/b:c*d?[e]", &[]), "abcde");
        assert!(sheet_name(&"x".repeat(80), &[]).chars().count() <= 31);
        assert_eq!(sheet_name("///", &[]), "Table");
    }

    #[test]
    fn numbers_are_only_taken_when_nothing_is_lost() {
        assert_eq!(as_number("1,234.5"), Some(1234.5));
        assert_eq!(as_number("$ 20"), Some(20.0));
        assert_eq!(as_number("(7)"), Some(-7.0));
        assert_eq!(as_number("0.5"), Some(0.5));
        for text in ["007", "12%", "1.2.3", "1,", "abc", "", "+5", "-5", "1234567890123456789", "12 USD"] {
            assert_eq!(as_number(text), None, "{text}");
        }
    }

    #[test]
    fn it_tells_when_the_chat_is_asked_to_make_a_file() {
        for (question, file) in [
            ("create me a excel sheet with the mens retreat expenses", Some("xlsx")),
            ("Make a spreadsheet of last month's invoices", Some("xlsx")),
            ("can you put together a Word document summarising the lease", Some("docx")),
            ("Generate an xlsx of all clients", Some("xlsx")),
            ("what does the budget spreadsheet say about travel", None),
            ("who signed the lease", None),
            ("make a list of the clients", None),
            ("", None),
        ] {
            assert_eq!(wanted_file(question), file, "{question}");
        }
    }

    #[test]
    fn the_search_looks_for_the_subject_not_the_request() {
        assert_eq!(without_file_request("create me a excel sheet with the mens retreat expenses"), "with the mens retreat expenses");
        assert_eq!(without_file_request("who signed the lease"), "who signed the lease");
        assert_eq!(without_file_request("make a spreadsheet"), "make a spreadsheet");
    }

    #[test]
    fn column_letters_roll_over() {
        assert_eq!(column_name(0), "A");
        assert_eq!(column_name(25), "Z");
        assert_eq!(column_name(26), "AA");
        assert_eq!(column_name(27), "AB");
        assert_eq!(column_name(701), "ZZ");
        assert_eq!(column_name(702), "AAA");
    }

    #[test]
    fn file_names_cannot_leave_the_folder() {
        assert_eq!(file_stem("../../etc/passwd"), "etc passwd");
        assert_eq!(file_stem("Q3: results / draft"), "Q3 results draft");
        assert_eq!(file_stem("///"), "Report");
        assert!(file_stem(&"a".repeat(200)).len() <= 60);
    }

    #[test]
    fn control_characters_never_reach_the_xml() {
        let bytes = to_docx("t", "bad \u{0}\u{8} text").unwrap();
        well_formed(&read(&bytes, "word/document.xml"));
    }
}
