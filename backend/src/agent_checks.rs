//! Checks on a finished report that need no AI: are the figures and quotations in it really in the
//! text the agent read, and does it name documents by their file name rather than by an ID?
//! A figure that is not found is not necessarily wrong (a total the agent worked out will not
//! appear in any document), so it is reported as "not found in the sources", never as false.

use std::collections::HashSet;

use serde::Serialize;
use serde_json::Value;

const MAX_LISTED: usize = 20;
const MIN_QUOTE_CHARS: usize = 12;
const MAX_QUOTE_CHARS: usize = 300;

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Checks {
    pub figures_total: usize,
    pub figures_found: usize,
    pub figures_missing: Vec<String>,
    pub quotes_total: usize,
    pub quotes_found: usize,
    pub quotes_missing: Vec<String>,
}

/// Every number in `text` as hundredths ("$1,880.77" and "1880.77" are both 188077), with the
/// text it was written as and whether it is a figure worth checking: money, a percentage, a
/// decimal or a number of four digits or more. Small whole numbers (list numbers, "3 speakers")
/// are left out of the report's side because they say nothing.
fn numbers(text: &str) -> Vec<(i64, String, bool)> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && (chars[i].is_ascii_digit() || ((chars[i] == ',' || chars[i] == '.') && chars.get(i + 1).is_some_and(|c| c.is_ascii_digit()))) {
            i += 1;
        }
        let raw: String = chars[start..i].iter().collect();
        let money = start > 0 && chars[start - 1] == '$';
        let percent = chars.get(i) == Some(&'%');
        let clean: String = raw.chars().filter(|c| *c != ',').collect();
        let Ok(value) = clean.parse::<f64>() else { continue };
        let digits = clean.chars().filter(char::is_ascii_digit).count();
        let worth = money || percent || clean.contains('.') || digits >= 4;
        let shown = format!("{}{raw}{}", if money { "$" } else { "" }, if percent { "%" } else { "" });
        found.push(((value * 100.0).round() as i64, shown, worth));
    }
    found
}

/// Lower case with single spaces and plain quotes, so line breaks and curly quotes in a document
/// do not hide a match.
fn squash(text: &str) -> String {
    text.chars()
        .map(|c| match c {
            '\u{2018}' | '\u{2019}' => '\'',
            '\u{201c}' | '\u{201d}' => '"',
            c => c,
        })
        .collect::<String>()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Passages in double quotes that are long enough to be a quotation and not a single word.
fn quotations(text: &str) -> Vec<String> {
    let plain: String = text.chars().map(|c| if c == '\u{201c}' || c == '\u{201d}' { '"' } else { c }).collect();
    plain
        .split('"')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, quote)| quote.trim().to_string())
        .filter(|quote| (MIN_QUOTE_CHARS..=MAX_QUOTE_CHARS).contains(&quote.chars().count()))
        .collect()
}

/// Looks for each figure and quotation of the report in `evidence`, the text the agent was shown.
pub fn check_report(report: &str, evidence: &str) -> Checks {
    let known: HashSet<i64> = numbers(evidence).into_iter().map(|(value, _, _)| value).collect();
    let mut seen = HashSet::new();
    let mut checks = Checks::default();
    for (value, shown, worth) in numbers(report) {
        if !worth || !seen.insert(value) {
            continue;
        }
        checks.figures_total += 1;
        if known.contains(&value) {
            checks.figures_found += 1;
        } else if checks.figures_missing.len() < MAX_LISTED {
            checks.figures_missing.push(shown);
        }
    }
    let haystack = squash(evidence);
    let mut quoted = HashSet::new();
    for quote in quotations(report) {
        if !quoted.insert(squash(&quote)) {
            continue;
        }
        checks.quotes_total += 1;
        if haystack.contains(&squash(&quote)) {
            checks.quotes_found += 1;
        } else if checks.quotes_missing.len() < MAX_LISTED {
            checks.quotes_missing.push(quote);
        }
    }
    checks
}

/// Everything the agent was shown by its tools: the text its report has to rest on. For the
/// calculator only the answer counts: the numbers the agent typed in are its own, and whether they
/// match a document is exactly what the check is for.
pub fn evidence_text(messages: &[Value]) -> String {
    let mut calculators = HashSet::new();
    for message in messages {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] == "tool_use"
                && block["name"] == "calculate"
                && let Some(id) = block["id"].as_str()
            {
                calculators.insert(id.to_string());
            }
        }
    }
    let mut text = String::new();
    for message in messages {
        for block in message["content"].as_array().into_iter().flatten() {
            if block["type"] == "tool_result"
                && let Some(content) = block["content"].as_str()
            {
                if block["tool_use_id"].as_str().is_some_and(|id| calculators.contains(id)) {
                    text.push_str(content.rsplit(" = ").next().unwrap_or(""));
                } else {
                    text.push_str(content);
                }
                text.push('\n');
            }
        }
    }
    text
}

fn is_hex(c: char) -> bool {
    c.is_ascii_hexdigit()
}

/// Positions where a full ID (8-4-4-4-12 hex digits) starts.
fn id_positions(chars: &[char]) -> Vec<usize> {
    let shape = [8, 4, 4, 4, 12];
    let mut found = Vec::new();
    let mut i = 0;
    'scan: while i + 36 <= chars.len() {
        let mut at = i;
        for (index, length) in shape.iter().enumerate() {
            if !chars[at..at + length].iter().all(|c| is_hex(*c)) {
                i += 1;
                continue 'scan;
            }
            at += length;
            if index < 4 {
                if chars[at] != '-' {
                    i += 1;
                    continue 'scan;
                }
                at += 1;
            }
        }
        found.push(i);
        i += 36;
    }
    found
}

/// Replaces document IDs (and the first eight characters of them, which agents like to use as a
/// shorthand) with the document's name, so a report reads in words. An ID that matches nothing is
/// removed rather than left as a meaningless string.
pub fn scrub_ids(text: &str, documents: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (id, name) in documents {
        let short: String = id.chars().take(8).collect();
        for prefix in ["Doc ", "doc ", "Document ", "document "] {
            out = out.replace(&format!("{prefix}{id}"), name).replace(&format!("{prefix}{short}"), name);
        }
        out = out.replace(id.as_str(), name);
        // The short form, only where it stands alone.
        let chars: Vec<char> = out.chars().collect();
        let needle: Vec<char> = short.chars().collect();
        let mut rebuilt = String::new();
        let mut i = 0;
        while i < chars.len() {
            let alone = i + needle.len() <= chars.len()
                && chars[i..i + needle.len()] == needle[..]
                && (i == 0 || !chars[i - 1].is_alphanumeric())
                && chars.get(i + needle.len()).is_none_or(|c| !c.is_alphanumeric());
            if alone {
                rebuilt.push_str(name);
                i += needle.len();
            } else {
                rebuilt.push(chars[i]);
                i += 1;
            }
        }
        out = rebuilt;
    }
    let chars: Vec<char> = out.chars().collect();
    let positions = id_positions(&chars);
    if positions.is_empty() {
        return out;
    }
    let mut rebuilt = String::new();
    let mut i = 0;
    while i < chars.len() {
        if positions.contains(&i) {
            rebuilt.push_str("(document)");
            i += 36;
        } else {
            rebuilt.push(chars[i]);
            i += 1;
        }
    }
    rebuilt
}

/// The documents a report really used: those the agent read in full, and any other it names.
/// A document that only turned up in a search and was never opened or mentioned is left out.
pub fn used_sources(sources: &Value, report: &str) -> Value {
    let report = squash(report);
    Value::Array(
        sources
            .as_array()
            .into_iter()
            .flatten()
            .filter(|source| {
                source["read"].as_bool().unwrap_or(false)
                    || source["name"].as_str().is_some_and(|name| !name.is_empty() && report.contains(&squash(name)))
            })
            .cloned()
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn figures_are_found_however_they_are_written() {
        let evidence = "Food 2,325.48 and speakers $600 and 6.98% under; Total 5670.77";
        let report = "Food was **$2,325.48**, speakers $600.00, 6.98%, the total $5,670.77 and a made-up $9,999.99 over 3 speakers in 2025.";
        let checks = check_report(report, evidence);
        // 2025 counts as a figure and is missing; 3 is too small to matter.
        assert_eq!(checks.figures_total, 6, "{checks:?}");
        assert_eq!(checks.figures_found, 4);
        assert_eq!(checks.figures_missing, vec!["$9,999.99".to_string(), "2025".to_string()]);
    }

    #[test]
    fn quotations_must_appear_in_the_documents_ignoring_line_breaks_and_curly_quotes() {
        let evidence = "The note reads\n\"$17 each 75 registrants + 25 Volunteers\" and \u{201c}Angel Tree - Gifts for 13 people\u{201d}.";
        let report = "The note says \"$17 each 75 registrants + 25 volunteers\", \u{201c}Angel Tree - Gifts for 13 people\u{201d} and \"a sentence nobody wrote\"; \"short\" is ignored.";
        let checks = check_report(report, evidence);
        assert_eq!((checks.quotes_total, checks.quotes_found), (3, 2), "{checks:?}");
        assert_eq!(checks.quotes_missing, vec!["a sentence nobody wrote".to_string()]);
    }

    #[test]
    fn evidence_is_what_the_tools_returned() {
        let messages = vec![
            json!({ "role": "user", "content": "the question 12345" }),
            json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": "t", "name": "x", "input": {} }] }),
            json!({ "role": "user", "content": [{ "type": "tool_result", "tool_use_id": "t", "content": "Total $4,200.00" }] }),
        ];
        let evidence = evidence_text(&messages);
        assert!(evidence.contains("$4,200.00") && !evidence.contains("12345"));
    }

    #[test]
    fn a_calculation_counts_by_its_answer_not_by_the_numbers_typed_into_it() {
        let messages = vec![
            json!({ "role": "assistant", "content": [{ "type": "tool_use", "id": "c1", "name": "calculate", "input": {} }, { "type": "tool_use", "id": "s1", "name": "search_documents", "input": {} }] }),
            json!({ "role": "user", "content": [
                { "type": "tool_result", "tool_use_id": "c1", "content": "999 + 1 = 1000" },
                { "type": "tool_result", "tool_use_id": "s1", "content": "Rent is 450" },
            ] }),
        ];
        let evidence = evidence_text(&messages);
        let checks = check_report("Rent $450.00, total $1,000.00, invented input $999.00.", &evidence);
        assert_eq!(checks.figures_missing, vec!["$999.00".to_string()], "{checks:?}");
    }

    #[test]
    fn ids_become_names() {
        let id = "5bd75039-f815-449e-989b-bee97d478b2e";
        let docs = vec![(id.to_string(), "Men's Retreat Budget".to_string())];
        let text = format!("Source: Doc 5bd75039 and {id}, also 5bd75039. Another 11111111-2222-3333-4444-555555555555 here. Not 5bd75039abc.");
        let cleaned = scrub_ids(&text, &docs);
        assert_eq!(
            cleaned,
            "Source: Men's Retreat Budget and Men's Retreat Budget, also Men's Retreat Budget. Another (document) here. Not 5bd75039abc."
        );
    }

    #[test]
    fn only_documents_that_were_read_or_named_are_listed() {
        let sources = json!([
            { "document_id": "1", "name": "Budget.xlsx", "read": true },
            { "document_id": "2", "name": "Planning notes", "read": false },
            { "document_id": "3", "name": "Registration form", "read": false },
        ]);
        let used = used_sources(&sources, "Fees come from the registration form, which lists $40.");
        let names: Vec<&str> = used.as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["Budget.xlsx", "Registration form"]);
    }
}
