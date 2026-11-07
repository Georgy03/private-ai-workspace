//! Search that returns one row per file, with filters and the counts shown beside the results
//! (how many matches are in each source and each kind of file). Everything is limited to files the
//! person may open: the counts never include a file they cannot see.

use std::collections::{HashMap, HashSet};

use axum::{Json, Router, extract::State, routing::post};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    Result,
    api::{ApiError, AppState, Authed, open},
    auth::Principal,
    db::TenantTx,
};

pub fn routes() -> Router<AppState> {
    Router::new().route("/v1/search/browse", post(browse))
}

const MAX_QUERY_CHARS: usize = 2_000;
/// Passages looked at to find the matching files, and files shown.
const POOL: i64 = 300;
const SHOWN: usize = 50;
const EXCERPT_CHARS: usize = 320;
pub const FILE_TYPES: &[&str] = &["document", "spreadsheet", "presentation", "pdf", "image", "other"];

/// A plain kind of file, from what the source says it is and the file's name.
pub fn file_type(mime: &str, name: &str) -> &'static str {
    let mime = mime.to_lowercase();
    let name = name.to_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| name.ends_with(e));
    if mime.contains("pdf") || ends(&[".pdf"]) {
        "pdf"
    } else if mime.contains("spreadsheet") || mime.contains("excel") || mime.contains("csv") || ends(&[".xlsx", ".xls", ".csv"]) {
        "spreadsheet"
    } else if mime.contains("presentation") || mime.contains("powerpoint") || ends(&[".pptx", ".ppt"]) {
        "presentation"
    } else if mime.starts_with("image/") || ends(&[".png", ".jpg", ".jpeg", ".gif", ".tif", ".tiff", ".heic"]) {
        "image"
    } else if mime.contains("word") || mime.contains("document") || mime.starts_with("text/") || ends(&[".docx", ".doc", ".txt", ".md", ".rtf"]) {
        "document"
    } else {
        "other"
    }
}

/// The program a file belongs to, for its icon: Google's own formats are told apart from Office
/// files that merely sit in Google Drive.
pub fn file_app(mime: &str, name: &str) -> &'static str {
    let mime = mime.to_lowercase();
    let name = name.to_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| name.ends_with(e));
    if mime == "application/vnd.google-apps.document" {
        "google_docs"
    } else if mime == "application/vnd.google-apps.spreadsheet" {
        "google_sheets"
    } else if mime == "application/vnd.google-apps.presentation" {
        "google_slides"
    } else if mime.contains("wordprocessingml") || mime.contains("msword") || ends(&[".docx", ".doc", ".docm"]) {
        "word"
    } else if mime.contains("spreadsheetml") || mime.contains("ms-excel") || ends(&[".xlsx", ".xls", ".xlsm"]) {
        "excel"
    } else if mime.contains("presentationml") || mime.contains("ms-powerpoint") || ends(&[".pptx", ".ppt", ".pptm"]) {
        "powerpoint"
    } else if mime.contains("pdf") || ends(&[".pdf"]) {
        "pdf"
    } else if mime.starts_with("image/") || file_type(&mime, &name) == "image" {
        "image"
    } else if mime.contains("csv") || ends(&[".csv"]) {
        "csv"
    } else if mime.starts_with("text/") || ends(&[".txt", ".md", ".rtf"]) {
        "text"
    } else {
        "other"
    }
}

#[derive(Deserialize, Default)]
struct Filters {
    /// Only files changed at or after this time (seconds since 1970).
    since_epoch: Option<i64>,
    #[serde(default)]
    sources: Vec<String>,
    #[serde(default)]
    types: Vec<String>,
}

#[derive(Deserialize)]
struct BrowseRequest {
    query: String,
    #[serde(default)]
    filters: Filters,
}

#[derive(Serialize)]
struct Row {
    document_id: Uuid,
    name: String,
    source: String,
    file_type: &'static str,
    /// Which program's icon to show.
    app: &'static str,
    modified_epoch: i64,
    link: Option<String>,
    excerpt: String,
    location_label: Option<String>,
    score: f32,
}

#[derive(Serialize)]
struct Count {
    value: String,
    count: usize,
}

/// Counts, most common first.
fn counted<'a>(values: impl Iterator<Item = &'a str>) -> Vec<Count> {
    let mut tally: HashMap<&str, usize> = HashMap::new();
    for value in values {
        *tally.entry(value).or_default() += 1;
    }
    let mut counts: Vec<Count> = tally.into_iter().map(|(value, count)| Count { value: value.to_string(), count }).collect();
    counts.sort_by(|a, b| b.count.cmp(&a.count).then(a.value.cmp(&b.value)));
    counts
}

pub struct Matched {
    rows: Vec<Row>,
}

/// The files that match, best first, one row each.
async fn matching(
    tx: &TenantTx<'_>,
    principal: &Principal,
    query: &str,
    vector: Option<(&str, &[f32])>,
) -> Result<Matched> {
    let hits = tx.search_hybrid(principal, query, vector, POOL).await?;
    let mut seen = HashSet::new();
    let ids: Vec<Uuid> = hits.iter().map(|h| h.document_id).filter(|id| seen.insert(*id)).collect();
    // Only what the person may open comes back, so a hit never leaks through its metadata.
    let meta: HashMap<Uuid, (String, String, i64, String)> = tx
        .sql()
        .query(
            "SELECT d.id, cn.kind, d.mime_type, d.modified_at, d.name
             FROM documents d JOIN connections cn ON cn.id = d.connection_id
             WHERE d.id = ANY($1) AND d.readable_by && $2::text[]",
            &[&ids, &principal.readers()],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), (row.get(1), row.get(2), row.get(3), row.get(4))))
        .collect();
    let mut used = HashSet::new();
    let mut rows = Vec::new();
    for hit in hits {
        let Some((source, mime, modified, name)) = meta.get(&hit.document_id) else { continue };
        if !used.insert(hit.document_id) {
            continue;
        }
        rows.push(Row {
            document_id: hit.document_id,
            name: name.clone(),
            source: source.clone(),
            file_type: file_type(mime, name),
            app: file_app(mime, name),
            modified_epoch: *modified,
            link: hit.web_url,
            excerpt: hit.text.chars().take(EXCERPT_CHARS).collect(),
            location_label: hit.location_label,
            score: hit.score,
        });
    }
    Ok(Matched { rows })
}

fn passes(row: &Row, filters: &Filters) -> bool {
    filters.since_epoch.is_none_or(|since| row.modified_epoch >= since)
        && (filters.sources.is_empty() || filters.sources.contains(&row.source))
        && (filters.types.is_empty() || filters.types.iter().any(|t| t == row.file_type))
}

async fn browse(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<BrowseRequest>,
) -> std::result::Result<Json<Value>, ApiError> {
    let query = request.query.trim();
    let chars = query.chars().count();
    if chars == 0 || chars > MAX_QUERY_CHARS {
        return Err(ApiError::BadRequest("query must be 1 to 2000 characters"));
    }
    if request.filters.sources.len() > 20 || request.filters.types.len() > FILE_TYPES.len() {
        return Err(ApiError::BadRequest("too many filters"));
    }
    let vector = match state.embedder.as_ref() {
        Some(embedder) => embedder.embed_query(query).await.ok().map(|v| (embedder.model().to_string(), v)),
        None => None,
    };
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let matched = matching(&tx, &principal, query, vector.as_ref().map(|(m, v)| (m.as_str(), v.as_slice()))).await?;
    tx.audit(&principal.user, "search", &json!({ "query_chars": chars, "results": matched.rows.len(), "filtered": true })).await?;
    tx.commit().await?;

    // The side counts say what is available, so choosing one source does not hide the others.
    let sources = counted(matched.rows.iter().filter(|r| passes_except_source(r, &request.filters)).map(|r| r.source.as_str()));
    let types = counted(matched.rows.iter().filter(|r| passes_except_type(r, &request.filters)).map(|r| r.file_type));
    let total = matched.rows.iter().filter(|r| passes(r, &request.filters)).count();
    let results: Vec<&Row> = matched.rows.iter().filter(|r| passes(r, &request.filters)).take(SHOWN).collect();
    Ok(Json(json!({ "results": results, "total": total, "facets": { "sources": sources, "types": types } })))
}

fn passes_except_source(row: &Row, filters: &Filters) -> bool {
    passes(row, &Filters { since_epoch: filters.since_epoch, sources: Vec::new(), types: filters.types.clone() })
}

fn passes_except_type(row: &Row, filters: &Filters) -> bool {
    passes(row, &Filters { since_epoch: filters.since_epoch, sources: filters.sources.clone(), types: Vec::new() })
}

#[cfg(test)]
mod tests {
    use super::{file_app, file_type};

    #[test]
    fn files_are_sorted_into_plain_kinds() {
        assert_eq!(file_type("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "a.docx"), "document");
        assert_eq!(file_type("application/vnd.google-apps.spreadsheet", "Budget"), "spreadsheet");
        assert_eq!(file_type("", "deck.PPTX"), "presentation");
        assert_eq!(file_type("application/pdf", "x"), "pdf");
        assert_eq!(file_type("image/png", "scan"), "image");
        assert_eq!(file_type("application/zip", "a.zip"), "other");
    }

    #[test]
    fn icons_tell_google_formats_from_office_files_in_drive() {
        assert_eq!(file_app("application/vnd.google-apps.document", "Plan"), "google_docs");
        assert_eq!(file_app("application/vnd.google-apps.spreadsheet", "Budget"), "google_sheets");
        assert_eq!(file_app("application/vnd.google-apps.presentation", "Deck"), "google_slides");
        // A Word file stored in Google Drive is still a Word file.
        assert_eq!(file_app("application/vnd.openxmlformats-officedocument.wordprocessingml.document", "a.docx"), "word");
        assert_eq!(file_app("", "model.XLSX"), "excel");
        assert_eq!(file_app("", "deck.pptx"), "powerpoint");
        assert_eq!(file_app("application/pdf", "x"), "pdf");
    }

}
