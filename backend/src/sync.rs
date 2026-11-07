//! Brings one connection's documents up to date. It lists changes page by page, stores each file
//! in its own small transaction, and saves the provider's cursor together with the page's
//! deletions, so a crash anywhere just repeats a little work and never skips any.

use deadpool_postgres::Pool;
use uuid::Uuid;

use crate::{
    Error, Result,
    auth::TenantId,
    connectors::{Access, Change, Connector, ConnectorError, RemoteFile, chunk_text},
    db::{Passage, StoredDocument, TenantTx},
    extract::{Kind, PARSER_VERSION, kind_for},
    extract_process::Extractor,
};

/// How long permissions may go without being re-read. A full listing re-reads every file's
/// permissions, so this is also how long a revoked share can stay visible in search.
pub const FULL_LISTING_EVERY_SECS: f64 = 6.0 * 3600.0;

/// After the file reader improves, files read by the old version are read again. This keeps a file
/// that can never be read from forcing a full listing on every sync.
const REPARSE_MIN_GAP_SECS: f64 = 600.0;

const STATUS_INDEXED: &str = "indexed";
const STATUS_METADATA_ONLY: &str = "metadata_only";

/// Why a file is stored by name only. The sync dashboard turns these into plain words.
pub const NOTE_UNSUPPORTED: &str = "unsupported_type";
pub const NOTE_TOO_LARGE: &str = "too_large";
pub const NOTE_NO_TEXT: &str = "no_text";
pub const NOTE_UNREADABLE: &str = "unreadable";
pub const NOTE_RETRY: &str = "retry";

/// A running sync reports how far it has got this often, in files.
const PROGRESS_EVERY: u32 = 10;

/// Reading a file that times out or crashes the reader is tried this many times in a row, then
/// given up on until the file changes. Without this, a file built to crash the reader would crash
/// it again at every sync.
const MAX_READ_ATTEMPTS: i32 = 3;

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncStats {
    pub stored: u32,
    pub unchanged: u32,
    pub removed: u32,
    pub metadata_only: u32,
    /// Files whose text came from the file reader (PDF, Word, Excel, PowerPoint).
    pub read_by_reader: u32,
    pub failed_files: u32,
    pub full_listing: bool,
}

/// The text indexed for a file with no readable content: its name, with punctuation turned into
/// spaces so "Retainer_2024.pdf" is found by "retainer".
fn name_text(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_alphanumeric() { c } else { ' ' })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

async fn begin<'a>(
    conn: &'a mut deadpool_postgres::Object,
    tenant: TenantId,
) -> Result<TenantTx<'a>> {
    TenantTx::begin(conn, tenant).await
}

async fn checkout(pool: &Pool) -> Result<deadpool_postgres::Object> {
    pool.get()
        .await
        .map_err(|error| Error::Pool(error.to_string()))
}

/// Short messages for the connection's `last_error`. Provider text is never copied in.
fn public_message(error: &ConnectorError) -> &'static str {
    match error {
        ConnectorError::Unauthorized => {
            "Reconnect required: the provider no longer accepts our access."
        }
        ConnectorError::RateLimited(_) => "The provider asked us to slow down. Will retry.",
        _ => "Sync failed. Will retry.",
    }
}

async fn record_failure(
    pool: &Pool,
    tenant: TenantId,
    connection: Uuid,
    error: &ConnectorError,
) -> Result<()> {
    let mut conn = checkout(pool).await?;
    let tx = begin(&mut conn, tenant).await?;
    if matches!(error, ConnectorError::Unauthorized) {
        tx.set_connection_status(connection, "needs_reauth").await?;
    }
    tx.finish_sync(connection, Some(public_message(error)), false)
        .await?;
    tx.commit().await
}

/// Tells the dashboard how far a running sync has got.
async fn report_progress(
    pool: &Pool,
    tenant: TenantId,
    connection: Uuid,
    looked_at: u32,
    failed: u32,
) -> Result<()> {
    let mut conn = checkout(pool).await?;
    let tx = begin(&mut conn, tenant).await?;
    tx.sync_progress(connection, looked_at as i32, failed as i32)
        .await?;
    tx.commit().await
}

pub async fn sync_connection(
    pool: &Pool,
    tenant: TenantId,
    connection: Uuid,
    connector: &dyn Connector,
    extractor: &dyn Extractor,
) -> Result<SyncStats> {
    let mut stats = SyncStats::default();

    // Where did we stop, and is it time for a full listing?
    let (mut cursor, started) = {
        let mut conn = checkout(pool).await?;
        let tx = begin(&mut conn, tenant).await?;
        let state = tx
            .connection(connection)
            .await?
            .ok_or(Error::TenantMismatch)?;
        if state.status != "active" {
            return Ok(stats);
        }
        let stale = state
            .full_listing_age_secs
            .is_some_and(|age| age >= FULL_LISTING_EVERY_SECS);
        // A reader that has improved since some files were read wants them read again, which a
        // full listing does. Not more often than every ten minutes.
        let reparse = state
            .full_listing_age_secs
            .is_none_or(|age| age >= REPARSE_MIN_GAP_SECS)
            && tx.needs_reparse(connection, PARSER_VERSION).await?;
        stats.full_listing = state.cursor.is_none() || stale || reparse;
        let cursor = if stats.full_listing {
            None
        } else {
            state.cursor
        };
        tx.begin_sync(connection).await?;
        let started = tx.clock().await?;
        tx.commit().await?;
        (cursor, started)
    };
    let mut restarts = 0;
    let mut looked_at = 0u32;

    loop {
        let page = match connector.changes(cursor.as_deref()).await {
            Ok(page) => page,
            // The provider forgot our place: list everything again, and afterwards remove
            // whatever the new listing did not mention.
            Err(ConnectorError::CursorExpired) if cursor.is_some() && restarts < 2 => {
                restarts += 1;
                cursor = None;
                stats.full_listing = true;
                continue;
            }
            Err(error) => {
                record_failure(pool, tenant, connection, &error).await?;
                return Err(error.into());
            }
        };

        let mut removed = Vec::new();
        let mut removed_trees = Vec::new();
        for change in &page.changes {
            match change {
                Change::Delete(id) => removed.push(id.clone()),
                Change::DeleteTree(prefix) => removed_trees.push(prefix.clone()),
                Change::Upsert(file) => {
                    match store_file(
                        pool, tenant, connection, connector, extractor, file, &mut stats,
                    )
                    .await
                    {
                        Ok(()) => {}
                        Err(Error::Connector(
                            error @ (ConnectorError::Unauthorized | ConnectorError::RateLimited(_)),
                        )) => {
                            record_failure(pool, tenant, connection, &error).await?;
                            return Err(error.into());
                        }
                        // One unreadable file must not stop the rest. It is retried next time because
                        // its stored version stays empty.
                        Err(Error::Connector(_)) => stats.failed_files += 1,
                        Err(other) => return Err(other),
                    }
                    looked_at += 1;
                    if looked_at.is_multiple_of(PROGRESS_EVERY) {
                        report_progress(pool, tenant, connection, looked_at, stats.failed_files)
                            .await?;
                    }
                }
            }
        }

        let mut conn = checkout(pool).await?;
        let tx = begin(&mut conn, tenant).await?;
        for id in &removed {
            stats.removed += tx.delete_document(connection, id).await? as u32;
        }
        for prefix in &removed_trees {
            stats.removed += tx.delete_tree(connection, prefix).await? as u32;
        }
        tx.set_cursor(connection, Some(&page.cursor)).await?;
        tx.commit().await?;

        if !page.has_more {
            break;
        }
        cursor = Some(page.cursor);
    }

    let mut conn = checkout(pool).await?;
    let tx = begin(&mut conn, tenant).await?;
    let first_ever = tx
        .connection(connection)
        .await?
        .is_some_and(|state| state.full_listing_age_secs.is_none());
    if stats.full_listing {
        stats.removed += tx.sweep(connection, started).await? as u32;
    }
    tx.sync_progress(connection, looked_at as i32, stats.failed_files as i32)
        .await?;
    tx.finish_sync(connection, None, stats.full_listing || first_ever)
        .await?;
    tx.commit().await?;
    Ok(stats)
}

/// What reading a file produced.
struct Read {
    version: String,
    status: &'static str,
    parser_version: i32,
    passages: Vec<Passage>,
    note: Option<&'static str>,
}

impl Read {
    fn indexed(version: &str, passages: Vec<Passage>) -> Self {
        Self {
            version: version.into(),
            status: STATUS_INDEXED,
            parser_version: PARSER_VERSION,
            passages,
            note: None,
        }
    }

    /// No readable text: the file can still be found by its name.
    fn name_only(file: &RemoteFile, note: &'static str) -> Self {
        Self {
            version: file.version.clone(),
            status: STATUS_METADATA_ONLY,
            parser_version: PARSER_VERSION,
            passages: vec![Passage {
                text: name_text(&file.name),
                label: None,
            }],
            note: Some(note),
        }
    }

    /// Reading failed in a way that might not repeat. The file is found by name for now, and its
    /// version is left empty so the next sync tries again.
    fn try_again_later(file: &RemoteFile) -> Self {
        Self {
            version: String::new(),
            parser_version: 0,
            ..Self::name_only(file, NOTE_RETRY)
        }
    }
}

fn is_fatal(error: &ConnectorError) -> bool {
    matches!(
        error,
        ConnectorError::Unauthorized | ConnectorError::RateLimited(_)
    )
}

/// Gets a file's text: directly for plain text and Google's own formats, through the file reader
/// for PDF and Office files.
async fn read_file(
    connector: &dyn Connector,
    extractor: &dyn Extractor,
    file: &RemoteFile,
    stats: &mut SyncStats,
) -> Result<Read> {
    match connector.text(file).await {
        Ok(Some(text)) => {
            let passages: Vec<Passage> = chunk_text(&text)
                .into_iter()
                .map(|text| Passage { text, label: None })
                .collect();
            return Ok(if passages.is_empty() {
                Read::name_only(file, NOTE_NO_TEXT)
            } else {
                Read::indexed(&file.version, passages)
            });
        }
        Ok(None) => {}
        Err(error) if is_fatal(&error) => return Err(error.into()),
        Err(_) => return Ok(Read::try_again_later(file)),
    }

    let Some(kind) = kind_for(&file.name, &file.mime_type) else {
        return Ok(Read::name_only(file, NOTE_UNSUPPORTED));
    };
    if kind == Kind::Image && file.size < crate::connectors::MIN_IMAGE_BYTES {
        return Ok(Read::name_only(file, NOTE_NO_TEXT));
    }
    let bytes = match connector.bytes(file).await {
        Ok(Some(bytes)) => bytes,
        // Too large to download: found by name.
        Ok(None) => return Ok(Read::name_only(file, NOTE_TOO_LARGE)),
        Err(error) if is_fatal(&error) => return Err(error.into()),
        Err(_) => return Ok(Read::try_again_later(file)),
    };
    match extractor.extract(bytes, &file.name, &file.mime_type).await {
        Ok(extracted) => {
            let mut passages = Vec::new();
            for section in &extracted.sections {
                for text in chunk_text(&section.text) {
                    passages.push(Passage {
                        text,
                        label: section.label.clone(),
                    });
                }
            }
            if passages.is_empty() {
                // A scan or an empty file: nothing to read until OCR exists.
                return Ok(Read::name_only(file, NOTE_NO_TEXT));
            }
            stats.read_by_reader += 1;
            Ok(Read::indexed(&file.version, passages))
        }
        Err(error) if error.is_transient() => Ok(Read::try_again_later(file)),
        // Damaged, protected or unsupported: reading it again would give the same answer.
        Err(_) => Ok(Read::name_only(file, NOTE_UNREADABLE)),
    }
}

/// A private connection's files are for the person who connected it, whatever the provider says
/// about who else may open them. Every other connection keeps the provider's own list.
async fn only_the_owner(tx: &crate::db::TenantTx<'_>, connection: Uuid, provider_readers: Vec<String>) -> Result<Vec<String>> {
    Ok(match tx.private_owner(connection).await? {
        Some(owner) => vec![format!("user:{owner}")],
        None => provider_readers,
    })
}

async fn store_file(
    pool: &Pool,
    tenant: TenantId,
    connection: Uuid,
    connector: &dyn Connector,
    extractor: &dyn Extractor,
    file: &RemoteFile,
    stats: &mut SyncStats,
) -> Result<()> {
    // Permissions are read for every file the provider reports, so a change to who may see a
    // file is never missed just because its content stayed the same.
    let access = match connector.access(file).await {
        Ok(access) => access,
        Err(ConnectorError::Forbidden | ConnectorError::NotFound) => Access::Unknown,
        Err(error) => return Err(error.into()),
    };
    let readers = access.readers();

    let existing = {
        let mut conn = checkout(pool).await?;
        let tx = begin(&mut conn, tenant).await?;
        tx.existing_documents(connection, std::slice::from_ref(&file.external_id))
            .await?
            .remove(&file.external_id)
    };

    // Unchanged, and read by the current reader: only its permissions need refreshing.
    if existing.as_ref().is_some_and(|document| {
        document.version == file.version
            && !file.version.is_empty()
            && document.parser_version >= PARSER_VERSION
    }) {
        let mut conn = checkout(pool).await?;
        let tx = begin(&mut conn, tenant).await?;
        let readers = only_the_owner(&tx, connection, readers).await?;
        tx.refresh_document(
            connection,
            &file.external_id,
            &readers,
            file.web_url.as_deref(),
        )
        .await?;
        tx.commit().await?;
        stats.unchanged += 1;
        return Ok(());
    }

    let mut read = read_file(connector, extractor, file, stats).await?;
    let mut read_attempts = 0;
    if read.note == Some(NOTE_RETRY) {
        read_attempts = existing.as_ref().map_or(0, |found| found.read_attempts) + 1;
        if read_attempts >= MAX_READ_ATTEMPTS {
            read = Read::name_only(file, NOTE_UNREADABLE);
            read_attempts = 0;
        }
    }

    let mut conn = checkout(pool).await?;
    let tx = begin(&mut conn, tenant).await?;
    let readers = only_the_owner(&tx, connection, readers).await?;
    let id = tx
        .store_document(&StoredDocument {
            connection_id: connection,
            external_id: file.external_id.clone(),
            name: file.name.clone(),
            path: file.path.clone(),
            mime_type: file.mime_type.clone(),
            modified_at: file.modified_at,
            version: read.version,
            readable_by: readers,
            web_url: file.web_url.clone(),
            note: read.note.map(str::to_owned),
            status: read.status.to_string(),
            parser_version: read.parser_version,
            read_attempts,
        })
        .await?;
    tx.replace_chunks(id, &read.passages).await?;
    tx.commit().await?;
    if read.status == STATUS_METADATA_ONLY {
        stats.metadata_only += 1;
    }
    stats.stored += 1;
    Ok(())
}
