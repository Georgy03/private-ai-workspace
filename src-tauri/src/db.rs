use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use base64::{engine::general_purpose::STANDARD, Engine};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};

use crate::{
    error::{AppError, AppResult},
    models::{
        DocumentResource, Entity, ParseStatus, ParsedBlock, PrivacyReceipt, SearchHit,
        SourceLocation,
    },
};

pub(crate) const KEYRING_SERVICE: &str = "ai.private.connector.workspace";
const LEGACY_KEYRING_SERVICE: &str = "ai.enclave.workspace";
const KEYRING_ACCOUNT: &str = "local-database-key";
const DEV_KEY_FILE: &str = "database.key";

pub struct Database {
    pub(crate) connection: Mutex<Connection>,
}

impl Database {
    pub fn open_default() -> AppResult<Self> {
        let base = workspace_dir()?;
        migrate_legacy_workspace(&base)?;
        fs::create_dir_all(&base).map_err(|error| AppError::Filesystem(error.to_string()))?;
        let path = base.join("workspace.db");
        let key = load_or_create_key(&base)?;
        Self::open_encrypted(path, &key)
    }

    pub fn open_encrypted(path: PathBuf, key: &[u8]) -> AppResult<Self> {
        let connection = Connection::open(path).map_err(db_error)?;
        connection
            .pragma_update(None, "key", format!("x'{}'", hex::encode(key)))
            .map_err(db_error)?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys = ON;
                 PRAGMA journal_mode = WAL;
                 PRAGMA synchronous = NORMAL;
                 PRAGMA secure_delete = ON;",
            )
            .map_err(db_error)?;

        let cipher_version: Option<String> = connection
            .query_row("PRAGMA cipher_version", [], |row| row.get(0))
            .optional()
            .map_err(db_error)?;
        if cipher_version.as_deref().unwrap_or_default().is_empty() {
            return Err(AppError::Database(
                "SQLCipher is unavailable; refusing to create an unencrypted index".into(),
            ));
        }

        let database = Self {
            connection: Mutex::new(connection),
        };
        database.migrate()?;
        Ok(database)
    }

    #[cfg(test)]
    pub fn open_memory() -> AppResult<Self> {
        let connection = Connection::open_in_memory().map_err(db_error)?;
        connection
            .execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(db_error)?;
        let database = Self {
            connection: Mutex::new(connection),
        };
        database.migrate()?;
        Ok(database)
    }

    fn migrate(&self) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .execute_batch(
                "CREATE TABLE IF NOT EXISTS schema_migrations (
                    version INTEGER PRIMARY KEY,
                    applied_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS approved_roots (
                    path TEXT PRIMARY KEY,
                    added_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS documents (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL,
                    path TEXT NOT NULL UNIQUE,
                    mime_type TEXT NOT NULL,
                    size INTEGER NOT NULL,
                    modified_at INTEGER NOT NULL,
                    content_hash TEXT NOT NULL,
                    status TEXT NOT NULL,
                    warning TEXT,
                    chunk_count INTEGER NOT NULL DEFAULT 0,
                    indexed_at INTEGER NOT NULL
                );
                CREATE TABLE IF NOT EXISTS chunks (
                    id TEXT PRIMARY KEY,
                    document_id TEXT NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
                    ordinal INTEGER NOT NULL,
                    text TEXT NOT NULL,
                    location_json TEXT NOT NULL,
                    ocr INTEGER NOT NULL DEFAULT 0,
                    ocr_confidence REAL,
                    content_hash TEXT NOT NULL,
                    UNIQUE(document_id, ordinal)
                );
                CREATE VIRTUAL TABLE IF NOT EXISTS chunks_fts USING fts5(
                    chunk_id UNINDEXED,
                    document_id UNINDEXED,
                    document_name,
                    heading,
                    body,
                    tokenize = 'unicode61 remove_diacritics 2'
                );
                CREATE TABLE IF NOT EXISTS privacy_receipts (
                    id TEXT PRIMARY KEY,
                    created_at INTEGER NOT NULL,
                    receipt_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS entities (
                    id TEXT PRIMARY KEY,
                    name TEXT NOT NULL UNIQUE COLLATE NOCASE,
                    aliases_json TEXT NOT NULL
                );
                CREATE TABLE IF NOT EXISTS dismissed_entities (
                    id TEXT PRIMARY KEY
                );
                CREATE TABLE IF NOT EXISTS settings (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );",
            )
            .map_err(db_error)?;
        drop(connection);
        self.migrate_workspaces()
    }

    pub fn setting(&self, key: &str) -> AppResult<Option<String>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .query_row("SELECT value FROM settings WHERE key = ?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(db_error)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection.execute("INSERT INTO settings(key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value", params![key, value]).map_err(db_error)?;
        Ok(())
    }

    pub fn add_root(&self, path: &str) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO approved_roots(path, added_at) VALUES (?1, ?2)",
                params![path, Utc::now().timestamp()],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Stops indexing a folder and forgets what was read from it. The files themselves are untouched.
    pub fn remove_root(&self, path: &str) -> AppResult<usize> {
        let mut connection = self.connection.lock().map_err(poison_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        // Only files inside this folder: the path must continue with a separator, so removing
        // "/Users/me/Docs" does not touch "/Users/me/Docs-old".
        let inside = "substr(path, 1, length(?1)) = ?1 AND substr(path, length(?1) + 1, 1) IN ('/', '\\')";
        transaction
            .execute(
                &format!("DELETE FROM chunks_fts WHERE document_id IN (SELECT id FROM documents WHERE {inside})"),
                [path],
            )
            .map_err(db_error)?;
        let removed = transaction
            .execute(&format!("DELETE FROM documents WHERE {inside}"), [path])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM approved_roots WHERE path = ?1", [path])
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)?;
        Ok(removed)
    }

    pub fn roots(&self) -> AppResult<Vec<String>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare("SELECT path FROM approved_roots ORDER BY added_at")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| row.get(0))
            .map_err(db_error)?;
        rows.collect::<Result<Vec<String>, _>>().map_err(db_error)
    }

    pub fn upsert_document(
        &self,
        document: &DocumentResource,
        blocks: &[ParsedBlock],
    ) -> AppResult<()> {
        let mut connection = self.connection.lock().map_err(poison_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        transaction
            .execute(
                "INSERT INTO documents(
                    id, name, path, mime_type, size, modified_at, content_hash,
                    status, warning, chunk_count, indexed_at
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT(path) DO UPDATE SET
                    id=excluded.id, name=excluded.name, mime_type=excluded.mime_type,
                    size=excluded.size, modified_at=excluded.modified_at,
                    content_hash=excluded.content_hash, status=excluded.status,
                    warning=excluded.warning, chunk_count=excluded.chunk_count,
                    indexed_at=excluded.indexed_at",
                params![
                    document.id,
                    document.name,
                    document.path,
                    document.mime_type,
                    document.size as i64,
                    document.modified_at,
                    document.content_hash,
                    status_to_str(&document.status),
                    document.warning,
                    blocks.len() as i64,
                    Utc::now().timestamp(),
                ],
            )
            .map_err(db_error)?;
        transaction
            .execute(
                "DELETE FROM chunks_fts WHERE document_id = ?1",
                [&document.id],
            )
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM chunks WHERE document_id = ?1", [&document.id])
            .map_err(db_error)?;

        for (ordinal, block) in blocks.iter().enumerate() {
            let chunk_id = format!("{}:{ordinal}", document.id);
            let content_hash = hex::encode(Sha256Digest::of(block.text.as_bytes()));
            let location_json = serde_json::to_string(&block.location)
                .map_err(|error| AppError::Internal(error.to_string()))?;
            transaction
                .execute(
                    "INSERT INTO chunks(
                        id, document_id, ordinal, text, location_json, ocr,
                        ocr_confidence, content_hash
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                    params![
                        chunk_id,
                        document.id,
                        ordinal as i64,
                        block.text,
                        location_json,
                        block.ocr as i64,
                        block.ocr_confidence,
                        content_hash,
                    ],
                )
                .map_err(db_error)?;
            transaction
                .execute(
                    "INSERT INTO chunks_fts(chunk_id, document_id, document_name, heading, body)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        format!("{}:{ordinal}", document.id),
                        document.id,
                        document.name,
                        block.location.heading.as_deref().unwrap_or_default(),
                        block.text,
                    ],
                )
                .map_err(db_error)?;
        }
        transaction.commit().map_err(db_error)
    }

    pub fn documents(&self) -> AppResult<Vec<DocumentResource>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT id, name, path, mime_type, size, modified_at, content_hash,
                        status, warning, chunk_count
                 FROM documents ORDER BY indexed_at DESC, name COLLATE NOCASE",
            )
            .map_err(db_error)?;
        let rows = statement.query_map([], map_document).map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn document(&self, id: &str) -> AppResult<Option<DocumentResource>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .query_row(
                "SELECT id, name, path, mime_type, size, modified_at, content_hash,
                        status, warning, chunk_count
                 FROM documents WHERE id = ?1",
                [id],
                map_document,
            )
            .optional()
            .map_err(db_error)
    }

    pub fn document_by_path(&self, path: &str) -> AppResult<Option<DocumentResource>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .query_row(
                "SELECT id, name, path, mime_type, size, modified_at, content_hash,
                        status, warning, chunk_count
                 FROM documents WHERE path = ?1",
                [path],
                map_document,
            )
            .optional()
            .map_err(db_error)
    }

    pub fn blocks_for_document(&self, id: &str) -> AppResult<Vec<ParsedBlock>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT text, location_json, ocr, ocr_confidence
                 FROM chunks WHERE document_id = ?1 ORDER BY ordinal",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map([id], |row| {
                let location_json: String = row.get(1)?;
                Ok(ParsedBlock {
                    text: row.get(0)?,
                    location: serde_json::from_str(&location_json).unwrap_or_default(),
                    ocr: row.get::<_, i64>(2)? != 0,
                    ocr_confidence: row.get(3)?,
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn clear_local_index(&self) -> AppResult<()> {
        let mut connection = self.connection.lock().map_err(poison_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        transaction
            .execute("DELETE FROM chunks_fts", [])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM documents", [])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM approved_roots", [])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM privacy_receipts", [])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM entities", [])
            .map_err(db_error)?;
        transaction
            .execute("DELETE FROM dismissed_entities", [])
            .map_err(db_error)?;
        transaction.commit().map_err(db_error)
    }

    pub fn search(&self, query: &str, limit: usize) -> AppResult<Vec<SearchHit>> {
        let fts_query = make_fts_query(query)?;
        let wanted = query_terms(query);
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT f.chunk_id, f.document_id, d.name, d.path, c.text,
                        c.location_json, bm25(chunks_fts, 0.0, 0.0, 3.0, 2.0, 1.0),
                        c.ocr
                 FROM chunks_fts f
                 JOIN chunks c ON c.id = f.chunk_id
                 JOIN documents d ON d.id = f.document_id
                 WHERE chunks_fts MATCH ?1
                 ORDER BY bm25(chunks_fts, 0.0, 0.0, 3.0, 2.0, 1.0)
                 LIMIT ?2",
            )
            .map_err(db_error)?;
        let rows = statement
            .query_map(params![fts_query, (limit * 4) as i64], |row| {
                let location_json: String = row.get(5)?;
                let location: SourceLocation =
                    serde_json::from_str(&location_json).unwrap_or_default();
                let text: String = row.get(4)?;
                let name: String = row.get(2)?;
                let relevant = covers_enough_terms(&wanted, &name, &text);
                Ok((
                    relevant,
                    SearchHit {
                        citation_id: row.get(0)?,
                        document_id: row.get(1)?,
                        document_name: row.get(2)?,
                        path: row.get(3)?,
                        excerpt: excerpt(&text, query, 520),
                        location_label: location.label(),
                        location,
                        score: -row.get::<_, f64>(6)?,
                        ocr: row.get::<_, i64>(7)? != 0,
                        stale: false,
                        origin: crate::models::Origin::Local,
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

    /// Filename matches are high-signal evidence: a question naming "McPherson" should reach
    /// `...mcpherson...pdf` before thousands of generic documents containing "United States".
    pub fn filename_hits(&self, query: &str, limit: usize) -> AppResult<Vec<SearchHit>> {
        let words = query_terms(query);
        // A name written "zero dock" in the question may be "ZeroDock" in the file name, so the
        // joined spelling counts more than either word alone.
        let terms = words
            .iter()
            .map(|word| (word.clone(), 1000.0))
            .chain(compounds(&words).into_iter().map(|word| (word, 1500.0)))
            .collect::<Vec<_>>();
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT c.id, d.id, d.name, d.path, c.text, c.location_json, c.ocr
             FROM documents d JOIN chunks c ON c.document_id = d.id
             WHERE lower(d.name) LIKE ?1 ESCAPE '\\' ORDER BY c.ordinal LIMIT ?2",
            )
            .map_err(db_error)?;
        let mut hits: Vec<SearchHit> = Vec::new();
        for (term, weight) in terms {
            if term.len() < 4 {
                continue;
            }
            let like = format!("%{}%", escape_like(&term));
            // A word found in many file names ("dock" in every "Docket") does not identify a
            // file, so it is left to the text search instead of outranking it.
            let named: i64 = connection
                .query_row(
                    "SELECT COUNT(*) FROM documents WHERE lower(name) LIKE ?1 ESCAPE '\\'",
                    [&like],
                    |row| row.get(0),
                )
                .map_err(db_error)?;
            if named as usize > MAX_DISTINCTIVE_NAME_MATCHES {
                continue;
            }
            let rows = statement
                .query_map(params![like, limit as i64], |row| {
                    let location_json: String = row.get(5)?;
                    let location: SourceLocation =
                        serde_json::from_str(&location_json).unwrap_or_default();
                    let text: String = row.get(4)?;
                    Ok(SearchHit {
                        citation_id: row.get(0)?,
                        document_id: row.get(1)?,
                        document_name: row.get(2)?,
                        path: row.get(3)?,
                        excerpt: excerpt(&text, query, 520),
                        location_label: location.label(),
                        location,
                        score: weight,
                        ocr: row.get::<_, i64>(6)? != 0,
                        stale: false,
                        origin: crate::models::Origin::Local,
                        link: None,
                    })
                })
                .map_err(db_error)?;
            for row in rows {
                let hit = row.map_err(db_error)?;
                // A file whose name matches several words is a better match than one that
                // matches a single common word.
                match hits
                    .iter_mut()
                    .find(|seen| seen.citation_id == hit.citation_id)
                {
                    Some(seen) => seen.score += weight,
                    None => hits.push(hit),
                }
            }
        }
        hits.sort_by(|a, b| b.score.total_cmp(&a.score));
        hits.truncate(limit);
        Ok(hits)
    }

    pub fn store_receipt(&self, receipt: &PrivacyReceipt) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let json = serde_json::to_string(receipt)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        connection
            .execute(
                "INSERT INTO privacy_receipts(id, created_at, receipt_json) VALUES (?1, ?2, ?3)",
                params![receipt.id, receipt.created_at, json],
            )
            .map_err(db_error)?;
        Ok(())
    }

    pub fn entities(&self) -> AppResult<Vec<Entity>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare("SELECT id, name, aliases_json FROM entities ORDER BY name")
            .map_err(db_error)?;
        let rows = statement
            .query_map([], |row| {
                let aliases: String = row.get(2)?;
                Ok(Entity {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    aliases: serde_json::from_str(&aliases).unwrap_or_default(),
                })
            })
            .map_err(db_error)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(db_error)
    }

    pub fn upsert_entity(&self, entity: &Entity) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .execute("DELETE FROM dismissed_entities WHERE id = ?1", [&entity.id])
            .map_err(db_error)?;
        let aliases = serde_json::to_string(&entity.aliases)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        connection
            .execute(
                "INSERT INTO entities(id, name, aliases_json) VALUES (?1, ?2, ?3)
                 ON CONFLICT(id) DO UPDATE SET name = excluded.name, aliases_json = excluded.aliases_json",
                params![entity.id, entity.name, aliases],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Removing an entity also stops folder discovery from adding it back.
    pub fn remove_entity(&self, id: &str) -> AppResult<()> {
        let connection = self.connection.lock().map_err(poison_error)?;
        connection
            .execute("DELETE FROM entities WHERE id = ?1", [id])
            .map_err(db_error)?;
        connection
            .execute(
                "INSERT OR IGNORE INTO dismissed_entities(id) VALUES (?1)",
                [id],
            )
            .map_err(db_error)?;
        Ok(())
    }

    /// Adds a discovered entity unless it exists or the user removed it before.
    pub fn add_entity_if_new(&self, entity: &Entity) -> AppResult<bool> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let dismissed: bool = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM dismissed_entities WHERE id = ?1)",
                [&entity.id],
                |row| row.get(0),
            )
            .map_err(db_error)?;
        if dismissed {
            return Ok(false);
        }
        let aliases = serde_json::to_string(&entity.aliases)
            .map_err(|error| AppError::Internal(error.to_string()))?;
        let added = connection
            .execute(
                "INSERT OR IGNORE INTO entities(id, name, aliases_json) VALUES (?1, ?2, ?3)",
                params![entity.id, entity.name, aliases],
            )
            .map_err(db_error)?;
        Ok(added > 0)
    }

    /// First chunk of every document whose path contains one of the names as a folder or file
    /// name part. Location evidence for entity search.
    pub fn first_chunks_under(&self, names: &[String], limit: usize) -> AppResult<Vec<SearchHit>> {
        let connection = self.connection.lock().map_err(poison_error)?;
        let mut statement = connection
            .prepare(
                "SELECT c.id, d.id, d.name, d.path, c.text, c.location_json, c.ocr
                 FROM documents d
                 JOIN chunks c ON c.document_id = d.id AND c.ordinal = 0
                 WHERE d.path LIKE ?1 ESCAPE '\\'
                 LIMIT ?2",
            )
            .map_err(db_error)?;
        let mut hits = Vec::new();
        for name in names.iter().filter(|name| name.chars().count() >= 3) {
            let like = format!("%{}%", escape_like(name));
            let rows = statement
                .query_map(params![like, limit as i64], |row| {
                    let location_json: String = row.get(5)?;
                    let location: SourceLocation =
                        serde_json::from_str(&location_json).unwrap_or_default();
                    let text: String = row.get(4)?;
                    Ok(SearchHit {
                        citation_id: row.get(0)?,
                        document_id: row.get(1)?,
                        document_name: row.get(2)?,
                        path: row.get(3)?,
                        excerpt: excerpt(&text, name, 520),
                        location_label: location.label(),
                        location,
                        score: 0.0,
                        ocr: row.get::<_, i64>(6)? != 0,
                        stale: false,
                        origin: crate::models::Origin::Local,
                        link: None,
                    })
                })
                .map_err(db_error)?;
            for row in rows {
                hits.push(row.map_err(db_error)?);
            }
        }
        Ok(hits)
    }

    pub fn purge_missing(&self, seen_paths: &HashSet<String>, root: &str) -> AppResult<()> {
        let mut connection = self.connection.lock().map_err(poison_error)?;
        let transaction = connection.transaction().map_err(db_error)?;
        let like = format!("{}%", escape_like(root));
        let mut statement = transaction
            .prepare("SELECT id, path FROM documents WHERE path LIKE ?1 ESCAPE '\\'")
            .map_err(db_error)?;
        let candidates = statement
            .query_map([like], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(db_error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(db_error)?;
        drop(statement);
        for (id, path) in candidates {
            if !seen_paths.contains(&path) {
                transaction
                    .execute("DELETE FROM chunks_fts WHERE document_id = ?1", [&id])
                    .map_err(db_error)?;
                transaction
                    .execute("DELETE FROM documents WHERE id = ?1", [&id])
                    .map_err(db_error)?;
            }
        }
        transaction.commit().map_err(db_error)
    }
}

pub(crate) fn workspace_dir() -> AppResult<PathBuf> {
    Ok(dirs::data_local_dir()
        .ok_or_else(|| AppError::Filesystem("no local data directory is available".into()))?
        .join("PrivateAIConnectorWorkspace"))
}

fn load_or_create_key(base: &Path) -> AppResult<Vec<u8>> {
    // An unsigned debug build looks like a new app to macOS after every rebuild, which makes it
    // ask for the Keychain password on each launch. Debug builds therefore keep the key in a
    // private file; release builds always use the Keychain.
    if cfg!(debug_assertions) {
        return dev_file_key(base);
    }
    Ok(keychain_key(true)?.expect("a key is created when requested"))
}

fn dev_file_key(base: &Path) -> AppResult<Vec<u8>> {
    let path = base.join(DEV_KEY_FILE);
    match fs::read_to_string(&path) {
        Ok(encoded) => {
            return STANDARD.decode(encoded.trim()).map_err(|error| {
                AppError::Database(format!("stored database key is invalid: {error}"))
            })
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(AppError::Filesystem(error.to_string())),
    }
    // First run: adopt the Keychain key so an index created earlier still opens.
    let key = match keychain_key(false)? {
        Some(key) => key,
        None => rand::random::<[u8; 32]>().to_vec(),
    };
    write_private_file(&path, STANDARD.encode(&key).as_bytes())?;
    Ok(key)
}

pub(crate) fn write_private_file(path: &Path, contents: &[u8]) -> AppResult<()> {
    use std::io::Write;
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents))
        .map_err(|error| {
            AppError::Filesystem(format!("could not save {}: {error}", path.display()))
        })
}

fn keychain_key(create: bool) -> AppResult<Option<Vec<u8>>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, KEYRING_ACCOUNT)
        .map_err(|error| AppError::Database(format!("credential store unavailable: {error}")))?;
    match entry.get_password() {
        Ok(encoded) => STANDARD.decode(encoded).map(Some).map_err(|error| {
            AppError::Database(format!("stored database key is invalid: {error}"))
        }),
        Err(keyring::Error::NoEntry) => {
            let legacy =
                keyring::Entry::new(LEGACY_KEYRING_SERVICE, KEYRING_ACCOUNT).map_err(|error| {
                    AppError::Database(format!("credential store unavailable: {error}"))
                })?;
            if let Ok(encoded) = legacy.get_password() {
                let key = STANDARD.decode(encoded).map_err(|error| {
                    AppError::Database(format!("stored database key is invalid: {error}"))
                })?;
                entry
                    .set_password(&STANDARD.encode(&key))
                    .map_err(|error| {
                        AppError::Database(format!("could not migrate database key: {error}"))
                    })?;
                return Ok(Some(key));
            }
            if !create {
                return Ok(None);
            }
            let key: [u8; 32] = rand::random();
            entry.set_password(&STANDARD.encode(key)).map_err(|error| {
                AppError::Database(format!("could not store database key: {error}"))
            })?;
            Ok(Some(key.to_vec()))
        }
        Err(error) => Err(AppError::Database(format!(
            "could not read database key: {error}"
        ))),
    }
}

/// Preserve existing encrypted indexes while the product name changes. The old directory is only
/// moved when the new one does not yet exist; no source document is touched.
fn migrate_legacy_workspace(new_base: &PathBuf) -> AppResult<()> {
    if new_base.exists() {
        return Ok(());
    }
    let Some(parent) = new_base.parent() else {
        return Ok(());
    };
    let old_base = parent.join("EnclaveWorkspace");
    if old_base.exists() {
        fs::rename(old_base, new_base).map_err(|error| {
            AppError::Filesystem(format!("could not migrate existing workspace: {error}"))
        })?;
    }
    Ok(())
}

fn map_document(row: &rusqlite::Row<'_>) -> rusqlite::Result<DocumentResource> {
    let status: String = row.get(7)?;
    Ok(DocumentResource {
        id: row.get(0)?,
        name: row.get(1)?,
        path: row.get(2)?,
        mime_type: row.get(3)?,
        size: row.get::<_, i64>(4)? as u64,
        modified_at: row.get(5)?,
        content_hash: row.get(6)?,
        status: str_to_status(&status),
        warning: row.get(8)?,
        chunk_count: row.get::<_, i64>(9)? as u32,
    })
}

fn status_to_str(status: &ParseStatus) -> &'static str {
    match status {
        ParseStatus::Pending => "pending",
        ParseStatus::Indexing => "indexing",
        ParseStatus::Ready => "ready",
        ParseStatus::Warning => "warning",
        ParseStatus::Failed => "failed",
    }
}

fn str_to_status(value: &str) -> ParseStatus {
    match value {
        "pending" => ParseStatus::Pending,
        "indexing" => ParseStatus::Indexing,
        "warning" => ParseStatus::Warning,
        "failed" => ParseStatus::Failed,
        _ => ParseStatus::Ready,
    }
}

/// A query word that appears in more file names than this is too common to count as evidence.
const MAX_DISTINCTIVE_NAME_MATCHES: usize = 20;

const STOP_WORDS: &[&str] = &[
    "a", "about", "all", "an", "and", "any", "are", "be", "can", "could", "did", "do", "does",
    "find", "for", "from", "give", "has", "have", "how", "in", "is", "it", "me", "my", "of", "on",
    "please", "show", "tell", "the", "there", "this", "to", "was", "were", "what", "when", "where",
    "which", "who", "with", "you",
];

/// Adjacent words joined into one ("zero", "dock" -> "zerodock"). Names are often written both
/// with and without the space, and the index treats the joined form as a single word.
fn compounds(words: &[String]) -> Vec<String> {
    words
        .windows(2)
        .map(|pair| format!("{}{}", pair[0], pair[1]))
        .collect()
}

pub(crate) fn make_fts_query(query: &str) -> AppResult<String> {
    let words = query
        .split(|character: char| {
            !character.is_alphanumeric() && character != '-' && character != '_'
        })
        .map(str::to_lowercase)
        .filter(|token| token.len() >= 2 && !STOP_WORDS.contains(&token.as_str()))
        .take(16)
        .collect::<Vec<_>>();
    let tokens = words
        .iter()
        .cloned()
        .chain(compounds(&words))
        .map(|token| format!("\"{}\"*", token.replace('"', "")))
        .collect::<Vec<_>>();
    if tokens.is_empty() {
        return Err(AppError::InvalidRequest(
            "enter at least one searchable word".into(),
        ));
    }
    Ok(tokens.join(" OR "))
}

/// Prefix matching lets "mens" find "mens" and "mensaje" alike, so a passage that matches only one
/// of several question words is usually a coincidence. It must contain at least half of them.
pub(crate) fn covers_enough_terms(terms: &[String], name: &str, text: &str) -> bool {
    if terms.len() < 2 {
        return true;
    }
    let haystack = format!("{name} {text}").to_lowercase();
    let words: Vec<&str> = haystack
        .split(|character: char| !character.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let covered = terms
        .iter()
        .filter(|term| words.iter().any(|word| word.starts_with(term.as_str())))
        .count();
    covered >= terms.len().div_ceil(2)
}

pub(crate) fn query_terms(query: &str) -> Vec<String> {
    query
        .split(|character: char| !character.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|term| term.len() >= 2 && !STOP_WORDS.contains(&term.as_str()))
        .collect()
}

pub(crate) fn excerpt(text: &str, query: &str, max_chars: usize) -> String {
    let lower = text.to_lowercase();
    let first_term = query
        .split_whitespace()
        .find(|term| term.len() > 2)
        .unwrap_or(query)
        .to_lowercase();
    let hit_byte = lower.find(&first_term).unwrap_or(0);
    let hit_char = lower[..hit_byte].chars().count();
    let start_char = hit_char.saturating_sub(max_chars / 3);
    let value = text
        .chars()
        .skip(start_char)
        .take(max_chars)
        .collect::<String>();
    if start_char > 0 || value.chars().count() < text.chars().count() {
        format!("{}…", value.trim())
    } else {
        value.trim().to_string()
    }
}

pub(crate) fn escape_like(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

pub(crate) fn db_error(error: rusqlite::Error) -> AppError {
    AppError::Database(error.to_string())
}

pub(crate) fn poison_error<T>(error: std::sync::PoisonError<T>) -> AppError {
    AppError::Internal(error.to_string())
}

struct Sha256Digest;

impl Sha256Digest {
    fn of(bytes: &[u8]) -> [u8; 32] {
        use sha2::{Digest, Sha256};
        Sha256::digest(bytes).into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{ParsedBlock, SourceLocation};

    #[test]
    fn indexes_and_finds_chunks() {
        let db = Database::open_memory().unwrap();
        let document = DocumentResource {
            id: "doc-1".into(),
            name: "Engagement letter.txt".into(),
            path: "/tmp/engagement.txt".into(),
            mime_type: "text/plain".into(),
            size: 10,
            modified_at: 1,
            content_hash: "abc".into(),
            status: ParseStatus::Ready,
            warning: None,
            chunk_count: 1,
        };
        db.upsert_document(
            &document,
            &[ParsedBlock {
                text: "Payment is due within thirty days of the invoice.".into(),
                location: SourceLocation {
                    heading: Some("Payment terms".into()),
                    ..Default::default()
                },
                ocr: false,
                ocr_confidence: None,
            }],
        )
        .unwrap();
        let hits = db.search("payment due", 8).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].document_id, "doc-1");
    }

    #[test]
    fn removing_a_folder_forgets_only_its_own_files() {
        let db = Database::open_memory().unwrap();
        let add = |id: &str, path: &str| {
            let document = DocumentResource {
                id: id.into(),
                name: format!("{id}.txt"),
                path: path.into(),
                mime_type: "text/plain".into(),
                size: 1,
                modified_at: 1,
                content_hash: id.into(),
                status: ParseStatus::Ready,
                warning: None,
                chunk_count: 1,
            };
            let block = ParsedBlock {
                text: "Payment is due within thirty days.".into(),
                location: SourceLocation::default(),
                ocr: false,
                ocr_confidence: None,
            };
            db.upsert_document(&document, &[block]).unwrap();
        };
        db.add_root("/work/Docs").unwrap();
        db.add_root("/work/Docs-old").unwrap();
        add("inside", "/work/Docs/a.txt");
        add("nested", "/work/Docs/sub/b.txt");
        add("sibling", "/work/Docs-old/c.txt");

        assert_eq!(db.remove_root("/work/Docs").unwrap(), 2);
        assert_eq!(db.roots().unwrap(), vec!["/work/Docs-old".to_string()]);
        let hits = db.search("payment due", 8).unwrap();
        assert_eq!(hits.iter().map(|h| h.document_id.as_str()).collect::<Vec<_>>(), ["sibling"]);
    }

    #[test]
    fn conversational_queries_keep_content_terms() {
        assert_eq!(
            make_fts_query("When is payment due?").unwrap(),
            "\"payment\"* OR \"due\"* OR \"paymentdue\"*"
        );
    }

    #[test]
    fn unicode_excerpt_does_not_slice_invalid_boundaries() {
        let text = "Résumé notes: élève payment details and café receipts.";
        assert!(excerpt(text, "payment", 24).contains("payment"));
    }
}
