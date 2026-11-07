use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::UNIX_EPOCH,
};

use chrono::Utc;
use sha2::{Digest, Sha256};
use tauri::{AppHandle, Emitter};
use walkdir::WalkDir;

use crate::{
    cloud::Cloud,
    db::Database,
    entities::entity_from_folder,
    error::{AppError, AppResult},
    models::{DocumentResource, IndexJob, ParseRequest, ParseStatus},
    parser::{content_hash, is_supported},
    parser_ipc::parse_isolated,
};

const MAX_FILE_SIZE: u64 = 50 * 1024 * 1024;

/// A file the person attached to a chat. It is read on this device and kept only in memory, so it
/// is gone when the app closes, and it is sent to Claude only when they ask a question with it.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub name: String,
    pub blocks: Vec<crate::models::ParsedBlock>,
}

pub struct AppState {
    pub db: Arc<Database>,
    pub cloud: Arc<Cloud>,
    pub active_job: Mutex<Option<IndexJob>>,
    pub attachments: Mutex<std::collections::HashMap<String, Attachment>>,
    cancel_indexing: AtomicBool,
}

impl AppState {
    pub fn new(db: Database, cloud: Cloud) -> Self {
        Self {
            db: Arc::new(db),
            cloud: Arc::new(cloud),
            active_job: Mutex::new(None),
            attachments: Mutex::new(std::collections::HashMap::new()),
            cancel_indexing: AtomicBool::new(false),
        }
    }

    pub fn cancel(&self) {
        self.cancel_indexing.store(true, Ordering::SeqCst);
    }

    pub fn active_job(&self) -> Option<IndexJob> {
        self.active_job.lock().ok().and_then(|job| job.clone())
    }

    pub async fn index_root(self: Arc<Self>, app: AppHandle, root: String) -> AppResult<String> {
        let root_path = PathBuf::from(&root)
            .canonicalize()
            .map_err(|error| AppError::Filesystem(error.to_string()))?;
        if !root_path.is_dir() {
            return Err(AppError::InvalidRequest(
                "The selected location is not a folder.".into(),
            ));
        }
        if self
            .active_job
            .lock()
            .map_err(|error| AppError::Internal(error.to_string()))?
            .as_ref()
            .is_some_and(|job| job.state == "running")
        {
            return Err(AppError::InvalidRequest(
                "An indexing job is already running.".into(),
            ));
        }

        let root = root_path.to_string_lossy().to_string();
        self.db.add_root(&root)?;
        self.cancel_indexing.store(false, Ordering::SeqCst);
        let id = make_id(
            format!(
                "{root}:{}",
                Utc::now().timestamp_nanos_opt().unwrap_or_default()
            )
            .as_bytes(),
        );
        let job = IndexJob {
            id: id.clone(),
            root: root.clone(),
            state: "running".into(),
            discovered: 0,
            completed: 0,
            failed: 0,
            current_file: None,
            started_at: Utc::now().timestamp(),
            finished_at: None,
        };
        *self
            .active_job
            .lock()
            .map_err(|error| AppError::Internal(error.to_string()))? = Some(job);

        let state = self.clone();
        let job_id = id.clone();
        tauri::async_runtime::spawn_blocking(move || {
            if let Err(error) = state.run_index(&app, &root_path) {
                let _ = app.emit("index-error", error.to_string());
                state.finish_job("failed");
                let _ = app.emit("index-progress", state.active_job());
            }
        });
        Ok(job_id)
    }

    fn run_index(&self, app: &AppHandle, root: &Path) -> AppResult<()> {
        let mut files = Vec::new();
        // Skip dependency, build and hidden folders: they bury real client files in noise.
        let walker = WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                entry.depth() == 0
                    || !entry.file_type().is_dir()
                    || !is_noise_dir(&entry.file_name().to_string_lossy())
            });
        for entry in walker {
            if self.cancel_indexing.load(Ordering::SeqCst) {
                self.finish_job("cancelled");
                let _ = app.emit("index-progress", self.active_job());
                return Ok(());
            }
            let Ok(entry) = entry else { continue };
            if entry.file_type().is_file() && is_supported(entry.path()) && !entry.path_is_symlink()
            {
                files.push(entry.into_path());
            }
        }
        self.update_job(|job| job.discovered = files.len() as u32, app);
        let mut seen = HashSet::new();

        for path in files {
            if self.cancel_indexing.load(Ordering::SeqCst) {
                self.finish_job("cancelled");
                let _ = app.emit("index-progress", self.active_job());
                return Ok(());
            }
            let canonical = match path.canonicalize() {
                Ok(path) if path.starts_with(root) => path,
                _ => {
                    self.update_job(|job| job.failed += 1, app);
                    continue;
                }
            };
            let path_string = canonical.to_string_lossy().to_string();
            seen.insert(path_string.clone());
            self.update_job(
                |job| {
                    job.current_file = canonical
                        .file_name()
                        .map(|name| name.to_string_lossy().into())
                },
                app,
            );
            match self.index_file(&canonical) {
                Ok(()) => self.update_job(|job| job.completed += 1, app),
                Err(AppError::Cancelled) => {
                    self.finish_job("cancelled");
                    let _ = app.emit("index-progress", self.active_job());
                    return Ok(());
                }
                Err(error) => {
                    if self.cancel_indexing.load(Ordering::SeqCst) {
                        self.finish_job("cancelled");
                        let _ = app.emit("index-progress", self.active_job());
                        return Ok(());
                    }
                    self.store_failed_document(&canonical, &error.to_string());
                    self.update_job(|job| job.failed += 1, app);
                }
            }
        }
        if self.cancel_indexing.load(Ordering::SeqCst) {
            self.finish_job("cancelled");
            let _ = app.emit("index-progress", self.active_job());
            return Ok(());
        }
        self.db.purge_missing(&seen, &root.to_string_lossy())?;
        self.discover_entities(root);
        self.finish_job("complete");
        let _ = app.emit("index-progress", self.active_job());
        Ok(())
    }

    /// Each immediate subfolder of an approved root is treated as a client or matter. Removed
    /// entities are not re-added.
    fn discover_entities(&self, root: &Path) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if !kind.is_dir() || kind.is_symlink() {
                continue;
            }
            if let Some(entity) = entity_from_folder(&entry.file_name().to_string_lossy()) {
                let _ = self.db.add_entity_if_new(&entity);
            }
        }
    }

    fn index_file(&self, path: &Path) -> AppResult<()> {
        self.index_file_with(path, &self.cancel_indexing)
    }

    /// Reads one file the person chose (for a workspace) into the local index, whether or not it is
    /// inside an approved folder, and returns it. A running folder scan is not affected.
    pub fn index_one(&self, path: &Path) -> AppResult<DocumentResource> {
        self.index_file_with(path, &std::sync::atomic::AtomicBool::new(false))?;
        self.db
            .document_by_path(&path.to_string_lossy())?
            .ok_or_else(|| AppError::Parser("the file could not be read".into()))
    }

    fn index_file_with(&self, path: &Path, cancel: &std::sync::atomic::AtomicBool) -> AppResult<()> {
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled);
        }
        let metadata =
            fs::metadata(path).map_err(|error| AppError::Filesystem(error.to_string()))?;
        if metadata.len() > MAX_FILE_SIZE {
            return Err(AppError::Parser(
                "file exceeds the 50 MB indexing limit".into(),
            ));
        }
        let bytes = fs::read(path).map_err(|error| AppError::Filesystem(error.to_string()))?;
        let hash = content_hash(&bytes);
        let path_string = path.to_string_lossy().to_string();
        if self
            .db
            .document_by_path(&path_string)?
            .is_some_and(|document| {
                // A prior scan-only PDF has no chunks. Revisit it after OCR support is added even
                // though the source bytes are unchanged; already-indexed readable files remain fast.
                document.content_hash == hash
                    && document.status != ParseStatus::Failed
                    && document.chunk_count > 0
            })
        {
            return Ok(());
        }
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let result = parse_isolated(
            ParseRequest {
                file_name: path
                    .file_name()
                    .map(|value| value.to_string_lossy().into())
                    .unwrap_or_else(|| "Untitled".into()),
                extension: extension.clone(),
                bytes,
            },
            cancel,
        )?;
        if cancel.load(Ordering::SeqCst) {
            return Err(AppError::Cancelled);
        }
        let warning = (!result.warnings.is_empty()).then(|| result.warnings.join(" · "));
        let document = DocumentResource {
            id: make_id(path_string.as_bytes()),
            name: path
                .file_name()
                .map(|value| value.to_string_lossy().into())
                .unwrap_or_else(|| "Untitled".into()),
            path: path_string,
            mime_type: mime_for_extension(&extension).into(),
            size: metadata.len(),
            modified_at: modified_timestamp(&metadata),
            content_hash: hash,
            status: if warning.is_some() {
                ParseStatus::Warning
            } else {
                ParseStatus::Ready
            },
            warning,
            chunk_count: result.blocks.len() as u32,
        };
        self.db.upsert_document(&document, &result.blocks)
    }

    fn store_failed_document(&self, path: &Path, warning: &str) {
        let Ok(metadata) = fs::metadata(path) else {
            return;
        };
        let path_string = path.to_string_lossy().to_string();
        let extension = path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default();
        let document = DocumentResource {
            id: make_id(path_string.as_bytes()),
            name: path
                .file_name()
                .map(|value| value.to_string_lossy().into())
                .unwrap_or_default(),
            path: path_string,
            mime_type: mime_for_extension(extension).into(),
            size: metadata.len(),
            modified_at: modified_timestamp(&metadata),
            content_hash: String::new(),
            status: ParseStatus::Failed,
            warning: Some(warning.into()),
            chunk_count: 0,
        };
        let _ = self.db.upsert_document(&document, &[]);
    }

    fn update_job(&self, update: impl FnOnce(&mut IndexJob), app: &AppHandle) {
        if let Ok(mut active) = self.active_job.lock() {
            if let Some(job) = active.as_mut() {
                update(job);
                let _ = app.emit("index-progress", job.clone());
            }
        }
    }

    fn finish_job(&self, status: &str) {
        if let Ok(mut active) = self.active_job.lock() {
            if let Some(job) = active.as_mut() {
                job.state = status.into();
                job.current_file = None;
                job.finished_at = Some(Utc::now().timestamp());
            }
        }
    }
}

fn is_noise_dir(name: &str) -> bool {
    name.starts_with('.')
        || matches!(
            name,
            "node_modules" | "target" | "dist" | "build" | "__pycache__" | "venv" | "site-packages"
        )
        || name.ends_with(".app")
}

pub fn make_id(value: &[u8]) -> String {
    hex::encode(&Sha256::digest(value)[..12])
}

fn modified_timestamp(metadata: &fs::Metadata) -> i64 {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or_default()
}

fn mime_for_extension(extension: &str) -> &'static str {
    match extension.to_ascii_lowercase().as_str() {
        "pdf" => "application/pdf",
        "docx" => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
        "xlsx" => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
        "xls" => "application/vnd.ms-excel",
        "csv" => "text/csv",
        "md" | "markdown" => "text/markdown",
        _ => "text/plain",
    }
}
