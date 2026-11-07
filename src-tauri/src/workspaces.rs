//! Workspaces: one place for everything about a client or a matter. A workspace holds the files
//! chosen for it, the chats had in it, and nothing else. Files stay where they are on the computer:
//! the workspace only points at them, so it adds only the small search index the app already keeps.
//! Everything lives in the same encrypted database as the rest of the local index.

use rusqlite::{params, OptionalExtension};
use serde::Serialize;

use crate::{
    db::{covers_enough_terms, db_error, excerpt, make_fts_query, poison_error, query_terms, Database},
    error::{AppError, AppResult},
    models::{Origin, SearchHit, SourceLocation},
};

pub const MAX_NAME_CHARS: usize = 80;
pub const MAX_DESCRIPTION_CHARS: usize = 500;
/// A saved chat may not be larger than this, so history can never fill the disk.
pub const MAX_CHAT_BYTES: usize = 2_000_000;
pub const MAX_CHATS_PER_WORKSPACE: i64 = 200;
pub const MAX_FILES_PER_WORKSPACE: i64 = 500;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkspaceSummary {
    pub id: String,
    pub name: String,
    pub description: String,
    pub files: u32,
    pub cloud_files: u32,
    pub chats: u32,
    /// About how much room this workspace takes on this computer: the search index of its files and
    /// its saved chats. The files themselves are not copied.
    pub stored_bytes: u64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorkspaceFile {
    /// "local" (a file on this computer) or "cloud" (a document found through a connected source).
    pub kind: String,
    pub id: String,
    pub name: String,
    /// The size of the file itself.
    pub size: u64,
    /// "ready", "warning" (read with a problem), "failed", or "missing" (no longer indexed).
    pub status: String,
    pub warning: Option<String>,
    /// For a cloud document, which source it came from ("google_drive").
    pub source: Option<String>,
    pub added_at: i64,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SavedChat {
    pub id: String,
    pub title: String,
    pub updated_at: i64,
}

fn clean_name(name: &str) -> AppResult<String> {
    let name: String = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > MAX_NAME_CHARS {
        return Err(AppError::InvalidRequest(format!("Give the workspace a name of 1 to {MAX_NAME_CHARS} characters.")));
    }
    Ok(name)
}

fn clean_description(text: &str) -> AppResult<String> {
    let text = text.trim().to_string();
    if text.chars().count() > MAX_DESCRIPTION_CHARS {
        return Err(AppError::InvalidRequest(format!("The description can be up to {MAX_DESCRIPTION_CHARS} characters.")));
    }
    Ok(text)
}

fn new_id(seed: &str) -> String {
    crate::state::make_id(format!("{seed}:{}:{}", chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default(), rand::random::<u64>()).as_bytes())
}

/// A path is inside an approved folder: the folder name must be followed by a separator.
const UNDER_APPROVED_FOLDER: &str = "EXISTS (SELECT 1 FROM approved_roots r WHERE substr(d.path, 1, length(r.path)) = r.path AND substr(d.path, length(r.path) + 1, 1) IN ('/', '\\'))";

impl Database {
    pub(crate) fn migrate_workspaces(&self) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS workspaces (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    description TEXT NOT NULL DEFAULT '',
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS workspace_files (
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    kind TEXT NOT NULL CHECK (kind IN ('local', 'cloud')),
                    ref TEXT NOT NULL,
                    name TEXT NOT NULL,
                    source TEXT,
                    added_at INTEGER NOT NULL,
                    PRIMARY KEY (workspace_id, kind, ref)
                );
                CREATE TABLE IF NOT EXISTS workspace_chats (
                    id TEXT PRIMARY KEY,
                    workspace_id TEXT NOT NULL REFERENCES workspaces(id) ON DELETE CASCADE,
                    title TEXT NOT NULL,
                    body TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS workspace_chats_by_workspace ON workspace_chats (workspace_id, updated_at DESC);",
            )
            .map_err(db_error)
    }

    fn workspace_summary_sql() -> &'static str {
        "SELECT w.id, w.name, w.description, w.created_at,
                (SELECT COUNT(*) FROM workspace_files f WHERE f.workspace_id = w.id AND f.kind = 'local'),
                (SELECT COUNT(*) FROM workspace_files f WHERE f.workspace_id = w.id AND f.kind = 'cloud'),
                (SELECT COUNT(*) FROM workspace_chats c WHERE c.workspace_id = w.id),
                -- The indexed text is kept twice (the passages and the search index over them).
                (SELECT COALESCE(SUM(length(ch.text)), 0) * 2 FROM workspace_files f
                    JOIN chunks ch ON ch.document_id = f.ref WHERE f.workspace_id = w.id AND f.kind = 'local')
                + (SELECT COALESCE(SUM(length(c.body)), 0) FROM workspace_chats c WHERE c.workspace_id = w.id),
                MAX(w.updated_at, COALESCE((SELECT MAX(c.updated_at) FROM workspace_chats c WHERE c.workspace_id = w.id), 0))
         FROM workspaces w"
    }

    fn summary_from(row: &rusqlite::Row<'_>) -> rusqlite::Result<WorkspaceSummary> {
        Ok(WorkspaceSummary {
            id: row.get(0)?,
            name: row.get(1)?,
            description: row.get(2)?,
            created_at: row.get(3)?,
            files: row.get::<_, i64>(4)? as u32,
            cloud_files: row.get::<_, i64>(5)? as u32,
            chats: row.get::<_, i64>(6)? as u32,
            stored_bytes: row.get::<_, i64>(7)?.max(0) as u64,
            updated_at: row.get(8)?,
        })
    }

    pub fn workspaces(&self) -> AppResult<Vec<WorkspaceSummary>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection.prepare(&format!("{} ORDER BY 9 DESC, w.name", Self::workspace_summary_sql())).map_err(db_error)?;
        let rows = statement.query_map([], Self::summary_from).map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn workspace(&self, id: &str) -> AppResult<Option<WorkspaceSummary>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .query_row(&format!("{} WHERE w.id = ?1", Self::workspace_summary_sql()), [id], Self::summary_from)
            .optional()
            .map_err(db_error)
    }

    fn require_workspace(&self, id: &str) -> AppResult<WorkspaceSummary> {
        self.workspace(id)?.ok_or_else(|| AppError::InvalidRequest("That workspace no longer exists.".into()))
    }

    pub fn create_workspace(&self, name: &str, description: &str) -> AppResult<WorkspaceSummary> {
        let name = clean_name(name)?;
        let description = clean_description(description)?;
        let id = new_id(&format!("workspace:{name}"));
        let now = chrono::Utc::now().timestamp();
        {
            let connection = self.connection.lock().map_err(poison_error)?;
            let taken: bool = connection
                .query_row("SELECT EXISTS (SELECT 1 FROM workspaces WHERE name = ?1)", [&name], |row| row.get(0))
                .map_err(db_error)?;
            if taken {
                return Err(AppError::InvalidRequest("You already have a workspace with that name.".into()));
            }
            connection
                .execute("INSERT INTO workspaces (id, name, description, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?4)", params![id, name, description, now])
                .map_err(db_error)?;
        }
        self.require_workspace(&id)
    }

    pub fn update_workspace(&self, id: &str, name: &str, description: &str) -> AppResult<WorkspaceSummary> {
        let name = clean_name(name)?;
        let description = clean_description(description)?;
        {
            let connection = self.connection.lock().map_err(poison_error)?;
            let taken: bool = connection
                .query_row("SELECT EXISTS (SELECT 1 FROM workspaces WHERE name = ?1 AND id <> ?2)", params![name, id], |row| row.get(0))
                .map_err(db_error)?;
            if taken {
                return Err(AppError::InvalidRequest("You already have a workspace with that name.".into()));
            }
            let changed = connection
                .execute("UPDATE workspaces SET name = ?2, description = ?3, updated_at = ?4 WHERE id = ?1", params![id, name, description, chrono::Utc::now().timestamp()])
                .map_err(db_error)?;
            if changed == 0 {
                return Err(AppError::InvalidRequest("That workspace no longer exists.".into()));
            }
        }
        self.require_workspace(id)
    }

    /// Deletes the workspace and its chats, and forgets what was read from files that nothing else
    /// uses: files in no other workspace and in no approved folder. The files themselves are not
    /// touched. Returns how many files were forgotten.
    pub fn delete_workspace(&self, id: &str) -> AppResult<usize> {
        let mut connection = self.connection.lock().map_err(poison_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        let forgotten = forget_unused_files(&transaction, id, None)?;
        transaction.execute("DELETE FROM workspaces WHERE id = ?1", [id]).map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(forgotten)
    }

    pub fn workspace_files(&self, id: &str) -> AppResult<Vec<WorkspaceFile>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT f.kind, f.ref, f.name, COALESCE(d.size, 0), d.status, d.warning, f.source, f.added_at, d.id IS NULL
                 FROM workspace_files f LEFT JOIN documents d ON f.kind = 'local' AND d.id = f.ref
                 WHERE f.workspace_id = ?1 ORDER BY f.added_at DESC, f.name",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([id], |row| {
                let kind: String = row.get(0)?;
                let missing: bool = row.get(8)?;
                let status: Option<String> = row.get(4)?;
                Ok(WorkspaceFile {
                    status: if kind == "cloud" { "ready".into() } else if missing { "missing".into() } else { status.unwrap_or_else(|| "ready".into()).to_lowercase() },
                    kind,
                    id: row.get(1)?,
                    name: row.get(2)?,
                    size: row.get::<_, i64>(3)?.max(0) as u64,
                    warning: row.get(5)?,
                    source: row.get(6)?,
                    added_at: row.get(7)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    fn add_file(&self, workspace: &str, kind: &str, reference: &str, name: &str, source: Option<&str>) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let exists: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM workspaces WHERE id = ?1)", [workspace], |row| row.get(0)).map_err(db_error)?;
        if !exists {
            return Err(AppError::InvalidRequest("That workspace no longer exists.".into()));
        }
        let held: i64 = connection.query_row("SELECT COUNT(*) FROM workspace_files WHERE workspace_id = ?1", [workspace], |row| row.get(0)).map_err(db_error)?;
        let already: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM workspace_files WHERE workspace_id = ?1 AND kind = ?2 AND ref = ?3)", params![workspace, kind, reference], |row| row.get(0)).map_err(db_error)?;
        if !already && held >= MAX_FILES_PER_WORKSPACE {
            return Err(AppError::InvalidRequest(format!("A workspace can hold up to {MAX_FILES_PER_WORKSPACE} files.")));
        }
        let now = chrono::Utc::now().timestamp();
        connection
            .execute(
                "INSERT INTO workspace_files (workspace_id, kind, ref, name, source, added_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(workspace_id, kind, ref) DO UPDATE SET name = excluded.name",
                params![workspace, kind, reference, name.chars().take(300).collect::<String>(), source, now],
            )
            .map_err(db_error)?;
        connection.execute("UPDATE workspaces SET updated_at = ?2 WHERE id = ?1", params![workspace, now]).map_err(db_error)?;
        Ok(())
    }

    /// A file on this computer, already read into the local index.
    pub fn add_local_file(&self, workspace: &str, document: &str, name: &str) -> AppResult<()> {
        self.add_file(workspace, "local", document, name, None)
    }

    /// A document found through a connected source. Only its name and ID are kept here; the text
    /// stays in the firm's own index and is read only when a question needs it.
    pub fn add_cloud_file(&self, workspace: &str, document: &str, name: &str, source: &str) -> AppResult<()> {
        let valid = document.len() == 36 && document.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
        if !valid {
            return Err(AppError::InvalidRequest("That is not a document in the firm cloud index.".into()));
        }
        let source: String = source.chars().filter(|c| c.is_ascii_lowercase() || *c == '_').take(30).collect();
        self.add_file(workspace, "cloud", document, name, Some(&source).filter(|s| !s.is_empty()).map(String::as_str))
    }

    /// Takes a file out of the workspace. What was read from it is forgotten too, unless another
    /// workspace or an approved folder uses it. Returns whether that happened.
    pub fn remove_workspace_file(&self, workspace: &str, kind: &str, reference: &str) -> AppResult<bool> {
        let mut connection = self.connection.lock().map_err(poison_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        let forgotten = if kind == "local" { forget_unused_files(&transaction, workspace, Some(reference))? > 0 } else { false };
        transaction.execute("DELETE FROM workspace_files WHERE workspace_id = ?1 AND kind = ?2 AND ref = ?3", params![workspace, kind, reference]).map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(forgotten)
    }

    pub fn workspace_document_ids(&self, workspace: &str, kind: &str) -> AppResult<Vec<String>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection.prepare("SELECT ref FROM workspace_files WHERE workspace_id = ?1 AND kind = ?2").map_err(db_error)?;
        let rows = statement.query_map(params![workspace, kind], |row| row.get(0)).map_err(db_error)?;
        rows.collect::<Result<Vec<String>, _>>().map_err(db_error)
    }

    // ---- Chats ----

    pub fn workspace_chats(&self, workspace: &str) -> AppResult<Vec<SavedChat>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection.prepare("SELECT id, title, updated_at FROM workspace_chats WHERE workspace_id = ?1 ORDER BY updated_at DESC").map_err(db_error)?;
        let rows = statement.query_map([workspace], |row| Ok(SavedChat { id: row.get(0)?, title: row.get(1)?, updated_at: row.get(2)? })).map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    /// The saved chat, as the page wrote it, only if it belongs to this workspace.
    pub fn workspace_chat(&self, workspace: &str, chat: &str) -> AppResult<Option<String>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection.query_row("SELECT body FROM workspace_chats WHERE id = ?1 AND workspace_id = ?2", params![chat, workspace], |row| row.get(0)).optional().map_err(db_error)
    }

    pub fn save_workspace_chat(&self, workspace: &str, chat: &str, title: &str, body: &str) -> AppResult<()> {
        if body.len() > MAX_CHAT_BYTES {
            return Err(AppError::InvalidRequest("This chat is too long to keep. Start a new chat to continue.".into()));
        }
        if chat.is_empty() || chat.len() > 64 || !chat.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
            return Err(AppError::InvalidRequest("That chat is not valid.".into()));
        }
        serde_json::from_str::<serde_json::Value>(body).map_err(|_| AppError::InvalidRequest("That chat could not be saved.".into()))?;
        let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ").chars().take(120).collect();
        let connection = self.connection.lock().map_err(poison_error)?;
        let exists: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM workspaces WHERE id = ?1)", [workspace], |row| row.get(0)).map_err(db_error)?;
        if !exists {
            return Err(AppError::InvalidRequest("That workspace no longer exists.".into()));
        }
        // Another workspace's chat can never be overwritten by guessing its ID.
        let elsewhere: bool = connection.query_row("SELECT EXISTS (SELECT 1 FROM workspace_chats WHERE id = ?1 AND workspace_id <> ?2)", params![chat, workspace], |row| row.get(0)).map_err(db_error)?;
        if elsewhere {
            return Err(AppError::InvalidRequest("That chat is not valid.".into()));
        }
        let held: i64 = connection.query_row("SELECT COUNT(*) FROM workspace_chats WHERE workspace_id = ?1 AND id <> ?2", params![workspace, chat], |row| row.get(0)).map_err(db_error)?;
        if held >= MAX_CHATS_PER_WORKSPACE {
            return Err(AppError::InvalidRequest(format!("A workspace keeps up to {MAX_CHATS_PER_WORKSPACE} chats. Delete an old one first.")));
        }
        let now = chrono::Utc::now().timestamp();
        connection
            .execute(
                "INSERT INTO workspace_chats (id, workspace_id, title, body, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)
                 ON CONFLICT(id) DO UPDATE SET title = excluded.title, body = excluded.body, updated_at = excluded.updated_at",
                params![chat, workspace, title, body, now],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn delete_workspace_chat(&self, workspace: &str, chat: &str) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection.execute("DELETE FROM workspace_chats WHERE id = ?1 AND workspace_id = ?2", params![chat, workspace]).map_err(db_error)?;
        Ok(())
    }

    // ---- Searching only what the workspace holds ----

    /// Passages of the given files that match the question. Only these files are looked at, however
    /// large the rest of the index is.
    pub fn search_in_documents(&self, query: &str, limit: usize, documents: &[String]) -> AppResult<Vec<SearchHit>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let Ok(fts_query) = make_fts_query(query) else { return Ok(Vec::new()) };
        let wanted = query_terms(query);
        let ids = serde_json::to_string(documents).map_err(|error| AppError::Internal(error.to_string()))?;
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT f.chunk_id, f.document_id, d.name, d.path, c.text, c.location_json,
                        bm25(chunks_fts, 0.0, 0.0, 3.0, 2.0, 1.0), c.ocr
                 FROM chunks_fts f
                 JOIN chunks c ON c.id = f.chunk_id
                 JOIN documents d ON d.id = f.document_id
                 WHERE chunks_fts MATCH ?1 AND f.document_id IN (SELECT value FROM json_each(?3))
                 ORDER BY bm25(chunks_fts, 0.0, 0.0, 3.0, 2.0, 1.0)
                 LIMIT ?2",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![fts_query, (limit * 4) as i64, ids], |row| {
                let location: SourceLocation = serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default();
                let text: String = row.get(4)?;
                let name: String = row.get(2)?;
                Ok((
                    covers_enough_terms(&wanted, &name, &text),
                    SearchHit {
                        citation_id: row.get(0)?,
                        document_id: row.get(1)?,
                        document_name: name,
                        path: row.get(3)?,
                        excerpt: excerpt(&text, query, 520),
                        location_label: location.label(),
                        location,
                        score: -row.get::<_, f64>(6)?,
                        ocr: row.get::<_, i64>(7)? != 0,
                        stale: false,
                        origin: Origin::Local,
                        link: None,
                    },
                ))
            })
            .map_err(db_error)?;
        let mut hits = Vec::new();
        for row in rows {
            let (relevant, hit) = row.map_err(db_error)?;
            if relevant && hits.len() < limit {
                hits.push(hit);
            }
        }
        Ok(hits)
    }

    /// The opening passage of each of the given files, for a broad question ("where does the case
    /// stand?") whose words match nothing in particular.
    pub fn opening_passages_of(&self, documents: &[String], limit: usize) -> AppResult<Vec<SearchHit>> {
        if documents.is_empty() {
            return Ok(Vec::new());
        }
        let ids = serde_json::to_string(documents).map_err(|error| AppError::Internal(error.to_string()))?;
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT c.id, d.id, d.name, d.path, c.text, c.location_json, c.ocr
                 FROM documents d JOIN chunks c ON c.document_id = d.id AND c.ordinal = 0
                 WHERE d.id IN (SELECT value FROM json_each(?1)) ORDER BY d.modified_at DESC LIMIT ?2",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![ids, limit as i64], |row| {
                let location: SourceLocation = serde_json::from_str(&row.get::<_, String>(5)?).unwrap_or_default();
                let text: String = row.get(4)?;
                Ok(SearchHit {
                    citation_id: row.get(0)?,
                    document_id: row.get(1)?,
                    document_name: row.get(2)?,
                    path: row.get(3)?,
                    excerpt: text.chars().take(520).collect(),
                    location_label: location.label(),
                    location,
                    score: 0.0,
                    ocr: row.get::<_, i64>(6)? != 0,
                    stale: false,
                    origin: Origin::Local,
                    link: None,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }
}

/// Forgets what was read from this workspace's local files (or just one of them) that no other
/// workspace uses and no approved folder covers. The files on disk are never touched.
fn forget_unused_files(transaction: &rusqlite::Transaction<'_>, workspace: &str, only: Option<&str>) -> AppResult<usize> {
    let unused = format!(
        "d.id IN (SELECT f.ref FROM workspace_files f WHERE f.workspace_id = ?1 AND f.kind = 'local' AND (?2 IS NULL OR f.ref = ?2))
         AND NOT EXISTS (SELECT 1 FROM workspace_files o WHERE o.kind = 'local' AND o.ref = d.id AND o.workspace_id <> ?1)
         AND NOT {UNDER_APPROVED_FOLDER}"
    );
    transaction
        .execute(&format!("DELETE FROM chunks_fts WHERE document_id IN (SELECT d.id FROM documents d WHERE {unused})"), params![workspace, only])
        .map_err(db_error)?;
    transaction.execute(&format!("DELETE FROM documents WHERE id IN (SELECT d.id FROM documents d WHERE {unused})"), params![workspace, only]).map_err(db_error)
}

// ---- What the page asks for ----

use std::{path::PathBuf, sync::Arc};
use tauri::{AppHandle, State};

use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct FailedFile {
    pub name: String,
    pub reason: String,
}

/// What happened when files were added: how many are in, and which could not be read, and why.
#[derive(Debug, Serialize)]
pub struct AddResult {
    pub added: u32,
    pub failed: Vec<FailedFile>,
}

const MAX_FILES_PER_ADD: usize = 200;

#[tauri::command]
pub fn workspace_list(state: State<'_, Arc<AppState>>) -> AppResult<Vec<WorkspaceSummary>> {
    state.db.workspaces()
}

#[tauri::command]
pub fn workspace_create(state: State<'_, Arc<AppState>>, name: String, description: String) -> AppResult<WorkspaceSummary> {
    state.db.create_workspace(&name, &description)
}

#[tauri::command]
pub fn workspace_update(state: State<'_, Arc<AppState>>, id: String, name: String, description: String) -> AppResult<WorkspaceSummary> {
    state.db.update_workspace(&id, &name, &description)
}

/// Deletes the workspace and its chats. The files themselves are never touched.
#[tauri::command]
pub fn workspace_delete(state: State<'_, Arc<AppState>>, id: String) -> AppResult<usize> {
    state.db.delete_workspace(&id)
}

#[tauri::command]
pub fn workspace_files(state: State<'_, Arc<AppState>>, id: String) -> AppResult<Vec<WorkspaceFile>> {
    state.db.workspace_files(&id)
}

/// Reads the given files into the workspace. Each one is read in the same isolated parser as the
/// rest of the index, and stays where it is.
fn add_paths(state: &AppState, workspace: &str, paths: Vec<PathBuf>) -> AddResult {
    let mut result = AddResult { added: 0, failed: Vec::new() };
    for path in paths.into_iter().take(MAX_FILES_PER_ADD) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("File").to_string();
        let mut fail = |reason: &str| result.failed.push(FailedFile { name: name.clone(), reason: reason.into() });
        if !crate::parser::is_supported(&path) {
            fail("That kind of file cannot be read. Use PDF, Word, Excel, CSV, text or Markdown.");
            continue;
        }
        match state.index_one(&path) {
            Ok(document) if document.chunk_count == 0 => fail("No readable text was found. It may be a scan."),
            Ok(document) => match state.db.add_local_file(workspace, &document.id, &document.name) {
                Ok(()) => result.added += 1,
                Err(error) => fail(&error.to_string()),
            },
            Err(error) => fail(&error.to_string()),
        }
    }
    result
}

/// Lets the person pick files from this computer for the workspace. The window is opened here, so
/// the page never chooses a path. Returns nothing if they cancelled.
#[tauri::command]
pub async fn workspace_add_files(app: AppHandle, state: State<'_, Arc<AppState>>, id: String) -> AppResult<Option<AddResult>> {
    use tauri_plugin_dialog::DialogExt;
    state.db.workspace(&id)?.ok_or_else(|| AppError::InvalidRequest("That workspace no longer exists.".into()))?;
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        app.dialog().file().add_filter("Documents", &["pdf", "docx", "xlsx", "xls", "csv", "txt", "md"]).blocking_pick_files()
    })
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    let Some(chosen) = chosen else { return Ok(None) };
    let paths: Vec<PathBuf> = chosen.into_iter().filter_map(|file| file.into_path().ok()).collect();
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || add_paths(&state, &id, paths))
        .await
        .map(Some)
        .map_err(|error| AppError::Internal(error.to_string()))
}

/// Lets the person pick a folder (a client's folder) and adds the readable files in it.
#[tauri::command]
pub async fn workspace_add_folder(app: AppHandle, state: State<'_, Arc<AppState>>, id: String) -> AppResult<Option<AddResult>> {
    use tauri_plugin_dialog::DialogExt;
    state.db.workspace(&id)?.ok_or_else(|| AppError::InvalidRequest("That workspace no longer exists.".into()))?;
    let chosen = tauri::async_runtime::spawn_blocking(move || app.dialog().file().blocking_pick_folder())
        .await
        .map_err(|error| AppError::Internal(error.to_string()))?;
    let Some(chosen) = chosen else { return Ok(None) };
    let folder = chosen.into_path().map_err(|error| AppError::Filesystem(error.to_string()))?;
    let state = state.inner().clone();
    tauri::async_runtime::spawn_blocking(move || {
        // Hidden and build folders only bury real files in noise.
        let files: Vec<PathBuf> = walkdir::WalkDir::new(&folder)
            .max_depth(8)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| entry.depth() == 0 || !entry.file_name().to_string_lossy().starts_with('.') && entry.file_name() != "node_modules")
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file() && crate::parser::is_supported(entry.path()))
            .map(|entry| entry.into_path())
            .take(MAX_FILES_PER_ADD)
            .collect();
        add_paths(&state, &id, files)
    })
    .await
    .map(Some)
    .map_err(|error| AppError::Internal(error.to_string()))
}

/// Pins a document found through a connected source. Only its name and ID are kept here.
#[tauri::command]
pub fn workspace_add_cloud(state: State<'_, Arc<AppState>>, id: String, document: String, name: String, source: String) -> AppResult<()> {
    let document = document.strip_prefix("cloud:").unwrap_or(&document).to_string();
    state.db.add_cloud_file(&id, &document, &name, &source)
}

/// Takes a file out of the workspace; what was read from it is forgotten unless something else uses it.
#[tauri::command]
pub fn workspace_remove_file(state: State<'_, Arc<AppState>>, id: String, kind: String, reference: String) -> AppResult<bool> {
    if kind != "local" && kind != "cloud" {
        return Err(AppError::InvalidRequest("That file is not valid.".into()));
    }
    state.db.remove_workspace_file(&id, &kind, &reference)
}

#[tauri::command]
pub fn workspace_chats(state: State<'_, Arc<AppState>>, id: String) -> AppResult<Vec<SavedChat>> {
    state.db.workspace_chats(&id)
}

#[tauri::command]
pub fn workspace_chat(state: State<'_, Arc<AppState>>, id: String, chat: String) -> AppResult<Option<String>> {
    state.db.workspace_chat(&id, &chat)
}

#[tauri::command]
pub fn workspace_save_chat(state: State<'_, Arc<AppState>>, id: String, chat: String, title: String, body: String) -> AppResult<()> {
    state.db.save_workspace_chat(&id, &chat, &title, &body)
}

#[tauri::command]
pub fn workspace_delete_chat(state: State<'_, Arc<AppState>>, id: String, chat: String) -> AppResult<()> {
    state.db.delete_workspace_chat(&id, &chat)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{DocumentResource, ParseStatus, ParsedBlock};

    fn document(db: &Database, name: &str, path: &str, text: &str) -> String {
        let id = crate::state::make_id(path.as_bytes());
        let document = DocumentResource {
            id: id.clone(), name: name.into(), path: path.into(), mime_type: "text/plain".into(), size: text.len() as u64,
            modified_at: 1, content_hash: "h".into(), status: ParseStatus::Ready, warning: None, chunk_count: 1,
        };
        let block = ParsedBlock { text: text.into(), location: Default::default(), ocr: false, ocr_confidence: None };
        db.upsert_document(&document, &[block]).unwrap();
        id
    }

    #[test]
    fn a_workspace_needs_a_unique_name_and_can_be_renamed() {
        let db = Database::open_memory().unwrap();
        let steve = db.create_workspace("  Steve   Miller ", "Custody case").unwrap();
        assert_eq!((steve.name.as_str(), steve.description.as_str(), steve.files, steve.chats), ("Steve Miller", "Custody case", 0, 0));
        assert!(db.create_workspace("steve miller", "").is_err(), "names are compared without regard to capitals");
        assert!(db.create_workspace("   ", "").is_err());
        assert!(db.create_workspace(&"x".repeat(81), "").is_err());
        assert!(db.create_workspace("Other", &"x".repeat(501)).is_err());
        let other = db.create_workspace("Other", "").unwrap();
        assert!(db.update_workspace(&other.id, "STEVE MILLER", "").is_err());
        assert_eq!(db.update_workspace(&steve.id, "Steve M.", "Closed").unwrap().name, "Steve M.");
        assert!(db.update_workspace("nope", "X", "").is_err());
        assert_eq!(db.workspaces().unwrap().len(), 2);
    }

    #[test]
    fn a_workspace_searches_only_its_own_files_even_when_the_index_is_large() {
        let db = Database::open_memory().unwrap();
        let steve = db.create_workspace("Steve", "").unwrap();
        let mine = document(&db, "Steve lease.txt", "/cases/steve/lease.txt", "The tenant must pay rent on the first of the month.");
        let other = document(&db, "Other lease.txt", "/cases/other/lease.txt", "The tenant must pay rent on the fifth of the month.");
        for index in 0..60 {
            document(&db, &format!("noise{index}.txt"), &format!("/n/{index}.txt"), "The tenant must pay rent every month without exception.");
        }
        db.add_local_file(&steve.id, &mine, "Steve lease.txt").unwrap();
        let ids = db.workspace_document_ids(&steve.id, "local").unwrap();
        let hits = db.search_in_documents("when must the tenant pay rent", 5, &ids).unwrap();
        assert_eq!(hits.len(), 1, "{hits:?}");
        assert_eq!(hits[0].document_id, mine);
        assert!(hits.iter().all(|hit| hit.document_id != other));
        assert!(db.search_in_documents("rent", 5, &[]).unwrap().is_empty());
        assert_eq!(db.opening_passages_of(&ids, 5).unwrap().len(), 1);
    }

    #[test]
    fn removing_a_file_forgets_it_unless_another_workspace_or_an_approved_folder_uses_it() {
        let db = Database::open_memory().unwrap();
        let (a, b) = (db.create_workspace("A", "").unwrap(), db.create_workspace("B", "").unwrap());
        let alone = document(&db, "alone.txt", "/x/alone.txt", "alone");
        let shared = document(&db, "shared.txt", "/x/shared.txt", "shared");
        let covered = document(&db, "covered.txt", "/approved/covered.txt", "covered");
        db.add_root("/approved").unwrap();
        for (ws, id, name) in [(&a.id, &alone, "alone.txt"), (&a.id, &shared, "shared.txt"), (&b.id, &shared, "shared.txt"), (&a.id, &covered, "covered.txt")] {
            db.add_local_file(ws, id, name).unwrap();
        }
        assert!(db.remove_workspace_file(&a.id, "local", &alone).unwrap(), "used by nothing else: forgotten");
        assert!(db.document(&alone).unwrap().is_none());
        assert!(!db.remove_workspace_file(&a.id, "local", &shared).unwrap());
        assert!(db.document(&shared).unwrap().is_some(), "another workspace still uses it");
        assert!(!db.remove_workspace_file(&a.id, "local", &covered).unwrap());
        assert!(db.document(&covered).unwrap().is_some(), "an approved folder still covers it");
        // Deleting a workspace forgets what only it used, and its chats go with it.
        db.save_workspace_chat(&b.id, "chat-1", "Hello", "[]").unwrap();
        assert_eq!(db.delete_workspace(&b.id).unwrap(), 1);
        assert!(db.document(&shared).unwrap().is_none());
        assert!(db.workspace_chats(&b.id).unwrap().is_empty());
        assert_eq!(db.workspaces().unwrap().len(), 1);
    }

    #[test]
    fn cloud_documents_are_kept_by_name_and_id_only() {
        let db = Database::open_memory().unwrap();
        let ws = db.create_workspace("Steve", "").unwrap();
        let id = "123e4567-e89b-12d3-a456-426614174000";
        db.add_cloud_file(&ws.id, id, "Retainer.docx", "google_drive").unwrap();
        db.add_cloud_file(&ws.id, id, "Retainer v2.docx", "google_drive").unwrap();
        assert!(db.add_cloud_file(&ws.id, "../../etc", "x", "google_drive").is_err());
        let files = db.workspace_files(&ws.id).unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].kind.as_str(), files[0].name.as_str(), files[0].source.as_deref(), files[0].status.as_str()), ("cloud", "Retainer v2.docx", Some("google_drive"), "ready"));
        assert_eq!(db.workspace_document_ids(&ws.id, "cloud").unwrap(), [id]);
        assert_eq!(db.workspace(&ws.id).unwrap().unwrap().cloud_files, 1);
    }

    #[test]
    fn chats_are_saved_per_workspace_with_limits_and_cannot_cross_workspaces() {
        let db = Database::open_memory().unwrap();
        let (a, b) = (db.create_workspace("A", "").unwrap(), db.create_workspace("B", "").unwrap());
        db.save_workspace_chat(&a.id, "chat-a", "  First \n question ", "[{\"q\":1}]").unwrap();
        db.save_workspace_chat(&a.id, "chat-a", "First question", "[{\"q\":1},{\"q\":2}]").unwrap();
        assert_eq!(db.workspace_chats(&a.id).unwrap().len(), 1);
        assert_eq!(db.workspace_chat(&a.id, "chat-a").unwrap().as_deref(), Some("[{\"q\":1},{\"q\":2}]"));
        assert_eq!(db.workspace_chat(&b.id, "chat-a").unwrap(), None, "B cannot read A's chat");
        assert!(db.save_workspace_chat(&b.id, "chat-a", "Stolen", "[]").is_err(), "B cannot overwrite A's chat");
        assert!(db.save_workspace_chat(&a.id, "bad id!", "x", "[]").is_err());
        assert!(db.save_workspace_chat(&a.id, "c2", "x", "not json").is_err());
        assert!(db.save_workspace_chat(&a.id, "c3", "x", &format!("[\"{}\"]", "x".repeat(MAX_CHAT_BYTES))).is_err());
        assert!(db.save_workspace_chat("nope", "c4", "x", "[]").is_err());
        assert!(db.workspace(&a.id).unwrap().unwrap().stored_bytes > 0);
        db.delete_workspace_chat(&b.id, "chat-a").unwrap();
        assert_eq!(db.workspace_chats(&a.id).unwrap().len(), 1, "B cannot delete A's chat");
        db.delete_workspace_chat(&a.id, "chat-a").unwrap();
        assert!(db.workspace_chats(&a.id).unwrap().is_empty());
    }
}
