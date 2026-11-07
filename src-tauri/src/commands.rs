use std::{collections::HashSet, fs, path::Path, sync::Arc};

use chrono::{TimeZone, Utc};
use tauri::{AppHandle, State};

use crate::{
    cloud::CloudError,
    error::{AppError, AppResult},
    models::{
        AnswerResponse, Citation, CloudState, CloudStatus, DocumentPreview, Entity, ModelCatalog,
        ModelRuntimeStatus, Origin, ParsedBlock, PrivacyReceipt, ProviderState, ProviderStatus,
        SearchHit, SourceLocation, WorkspaceSnapshot,
    },
    parser::content_hash,
    provider::provider,
    state::{make_id, AppState},
};

#[tauri::command]
pub async fn get_workspace(state: State<'_, Arc<AppState>>) -> AppResult<WorkspaceSnapshot> {
    let provider = provider()
        .map(|provider| provider.status())
        .unwrap_or_else(|error| ProviderStatus {
            kind: Default::default(),
            state: ProviderState::Failed,
            model: None,
            message: error.to_string(),
            checked_at: None,
        });
    Ok(WorkspaceSnapshot {
        roots: state.db.roots()?,
        documents: state.db.documents()?,
        active_job: state.active_job(),
        provider,
        model_runtime: crate::model_catalog::runtime_status(&state.db)?,
        sandbox_enforced: false,
    })
}

#[tauri::command]
pub async fn import_model_catalog(
    state: State<'_, Arc<AppState>>,
    catalog_json: String,
) -> AppResult<ModelCatalog> {
    crate::model_catalog::store_catalog(&state.db, catalog_json.as_bytes())
}

#[tauri::command]
pub async fn local_model_runtime_status(
    state: State<'_, Arc<AppState>>,
) -> AppResult<ModelRuntimeStatus> {
    crate::model_catalog::runtime_status(&state.db)
}

#[tauri::command]
pub async fn install_verified_model(
    state: State<'_, Arc<AppState>>,
    source_path: String,
    model_id: String,
) -> AppResult<()> {
    let catalog = crate::model_catalog::catalog(&state.db)?
        .ok_or_else(|| AppError::Verification("Import a signed model catalog first.".into()))?;
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == model_id)
        .ok_or(AppError::NotFound)?;
    crate::model_catalog::install_model_file(source_path.into(), &model.sha256)
}

#[tauri::command]
pub async fn start_index(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    root: String,
) -> AppResult<String> {
    state.inner().clone().index_root(app, root).await
}

#[tauri::command]
pub async fn cancel_index(state: State<'_, Arc<AppState>>) -> AppResult<()> {
    state.cancel();
    Ok(())
}

/// What a search found, with where each passage came from and how the cloud search went.
struct Retrieved {
    hits: Vec<SearchHit>,
    local: u32,
    cloud: u32,
    cloud_status: CloudStatus,
    cloud_question_sent: bool,
}

/// Searches this device and the firm cloud index together and merges the results. The local
/// search never waits on the cloud: if the cloud is off, signed out, slow or down, the answer is
/// built from local passages and the status says why.
/// Where a question may look, when the person has limited it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    All,
    /// Only files on this device: nothing is asked of the firm's cloud index.
    Local,
    /// Only the firm's cloud index.
    Cloud,
}

impl Scope {
    fn from_text(text: Option<&str>) -> Scope {
        match text {
            Some("local") => Scope::Local,
            Some("cloud") => Scope::Cloud,
            _ => Scope::All,
        }
    }
}

async fn retrieve(state: &AppState, query: &str, limit: usize, scope: Scope) -> AppResult<Retrieved> {
    retrieve_filtered(state, query, limit, scope, &crate::cloud::SearchFilter::default()).await
}

/// `retrieve`, with the firm cloud index limited to some sources and kinds of file. Choosing a
/// limit means the firm cloud only: files on this device are not looked at.
async fn retrieve_filtered(state: &AppState, query: &str, limit: usize, scope: Scope, filter: &crate::cloud::SearchFilter) -> AppResult<Retrieved> {
    let scope = if filter.is_empty() { scope } else { Scope::Cloud };
    let ((cloud_result, cloud_question_sent), local_result) = tokio::join!(
        async {
            if scope == Scope::Local {
                (Err(CloudError::Off), false)
            } else {
                state.cloud.search_filtered_tracked(query, limit, filter).await
            }
        },
        async {
            if scope == Scope::Cloud {
                Ok(Vec::new())
            } else {
                crate::entities::search(&state.db, query, limit)
            }
        }
    );
    let local_hits = local_result?;
    let mut cloud_status = state.cloud.status();
    let cloud_hits = match cloud_result {
        Ok(hits) => hits,
        Err(error) => {
            // "Off" and "not set up" are choices, not problems, so they get no warning.
            if !matches!(error, CloudError::Off | CloudError::NotConfigured) {
                cloud_status.state = match error {
                    CloudError::SignedOut => CloudState::SignedOut,
                    _ => CloudState::Unavailable,
                };
                cloud_status.message = format!("{} Showing local results only.", error.message());
                cloud_status.signed_in = cloud_status.signed_in && error != CloudError::SignedOut;
            }
            Vec::new()
        }
    };
    let hits = crate::merge::merge(local_hits, cloud_hits, limit);
    let cloud = hits
        .iter()
        .filter(|hit| hit.origin == Origin::Cloud)
        .count() as u32;
    Ok(Retrieved {
        local: hits.len() as u32 - cloud,
        cloud,
        hits,
        cloud_status,
        cloud_question_sent,
    })
}

/// Keeps only the passages of documents pinned to the workspace.
fn only_pinned(hits: Vec<SearchHit>, pinned: &[String]) -> Vec<SearchHit> {
    hits.into_iter().filter(|hit| pinned.iter().any(|id| hit.document_id == format!("cloud:{id}"))).collect()
}

/// What a question inside a workspace is answered from: only the files the person put in it. The
/// firm's cloud index is asked only when documents from it are pinned, and then only about those.
async fn retrieve_workspace(state: &AppState, workspace: &str, query: &str, limit: usize) -> AppResult<(Retrieved, crate::workspaces::WorkspaceSummary)> {
    let summary = state.db.workspace(workspace)?.ok_or_else(|| AppError::InvalidRequest("That workspace no longer exists.".into()))?;
    let local_ids = state.db.workspace_document_ids(workspace, "local")?;
    let cloud_ids = state.db.workspace_document_ids(workspace, "cloud")?;
    let mut local_hits = state.db.search_in_documents(query, limit, &local_ids)?;
    if local_hits.len() < 3 {
        // A broad question ("where does the case stand?") matches no words in particular: the
        // start of each file is the best place to look.
        for hit in state.db.opening_passages_of(&local_ids, limit / 2 + 1)? {
            if local_hits.len() < limit && !local_hits.iter().any(|existing| existing.citation_id == hit.citation_id) {
                local_hits.push(hit);
            }
        }
    }
    let (cloud_result, cloud_question_sent) = if cloud_ids.is_empty() {
        (Err(CloudError::Off), false)
    } else {
        let filter = crate::cloud::SearchFilter { documents: cloud_ids.clone(), ..Default::default() };
        let (result, sent) = state.cloud.search_filtered_tracked(query, limit, &filter).await;
        // The server is asked for these documents only; an older server that ignores the request
        // must still never put anything else in a workspace's answer.
        (result.map(|hits| only_pinned(hits, &cloud_ids)), sent)
    };
    let mut cloud_status = state.cloud.status();
    let cloud_hits = match cloud_result {
        Ok(hits) => hits,
        Err(error) => {
            if !matches!(error, CloudError::Off | CloudError::NotConfigured) {
                cloud_status.state = match error {
                    CloudError::SignedOut => CloudState::SignedOut,
                    _ => CloudState::Unavailable,
                };
                cloud_status.message = format!("{} Showing the workspace's files on this device only.", error.message());
                cloud_status.signed_in = cloud_status.signed_in && error != CloudError::SignedOut;
            }
            Vec::new()
        }
    };
    let hits = crate::merge::merge(local_hits, cloud_hits, limit);
    let cloud = hits.iter().filter(|hit| hit.origin == Origin::Cloud).count() as u32;
    Ok((Retrieved { local: hits.len() as u32 - cloud, cloud, hits, cloud_status, cloud_question_sent }, summary))
}

/// What a question about one of the person's own reports is answered from: the report itself, first,
/// and the passages of the files it was drawn from that best match the question. Nothing local is
/// searched, and the firm's server only returns files this person can open today.
async fn retrieve_report(state: &AppState, task: &str, query: &str) -> AppResult<(Retrieved, String)> {
    let valid = task.len() == 36 && task.chars().all(|c| c.is_ascii_hexdigit() || c == '-');
    if !valid {
        return Err(AppError::InvalidRequest("that is not a report".into()));
    }
    let report = knowledge_call(state, reqwest::Method::GET, &format!("/v1/tasks/{task}"), &[], None).await?;
    let text = report["edited_result"].as_str().or_else(|| report["result"].as_str()).unwrap_or("").trim().to_string();
    if text.is_empty() {
        return Err(AppError::Cloud("That report is not ready, or is no longer available.".into()));
    }
    let title = report["title"].as_str().unwrap_or("Report").to_string();
    let author = report["agent_name"].as_str().unwrap_or("a helper");
    let (found, cloud_question_sent) = state.cloud.search_report(task, query, ANSWER_PASSAGES).await;
    let cloud_status = state.cloud.status();
    let mut hits = vec![SearchHit {
        citation_id: "report".into(),
        document_id: "report".into(),
        document_name: format!("The report “{title}” by {author}"),
        path: "Report".into(),
        excerpt: text.chars().take(24_000).collect(),
        location: Default::default(),
        location_label: "Report".into(),
        score: 1.0,
        ocr: false,
        stale: false,
        origin: Origin::Cloud,
        link: None,
    }];
    hits.extend(found.map_err(|error| AppError::Cloud(format!("The evidence could not be searched: {}", error.message())))?);
    let cloud = hits.len() as u32;
    Ok((Retrieved { hits, local: 0, cloud, cloud_status, cloud_question_sent }, title))
}

/// A file the person tagged in the message box.
#[derive(serde::Deserialize)]
pub struct TaggedFile {
    id: String,
    name: String,
    /// "local" or "cloud".
    origin: String,
}

/// A passage-shaped stand-in for a whole file, so it can sit among the sources of an answer.
fn file_hit(id: &str, name: &str, origin: Origin, excerpt: &str) -> SearchHit {
    SearchHit {
        citation_id: format!("file:{id}"),
        document_id: id.to_string(),
        document_name: name.to_string(),
        path: if origin == Origin::Cloud { "Firm cloud index".into() } else { "Chosen file".into() },
        excerpt: excerpt.chars().take(1_200).collect(),
        location: Default::default(),
        location_label: "Chosen file".into(),
        score: 1.0,
        ocr: false,
        stale: false,
        origin,
        link: None,
    }
}

/// An attached file as passages, one for each block of its text.
fn attachment_hits(id: &str, name: &str, blocks: &[ParsedBlock]) -> Vec<SearchHit> {
    blocks
        .iter()
        .enumerate()
        .map(|(index, block)| SearchHit {
            citation_id: format!("attachment:{id}:{index}"),
            document_id: format!("attachment:{id}"),
            document_name: name.to_string(),
            path: "Attached file".into(),
            excerpt: block.text.clone(),
            location: block.location.clone(),
            location_label: "Attached file".into(),
            score: 1.0,
            ocr: block.ocr,
            stale: false,
            origin: Origin::Local,
            link: None,
        })
        .collect()
}

const MAX_ATTACHMENTS: usize = 3;
const MAX_ATTACHMENT_BYTES: u64 = 20 * 1024 * 1024;

/// What the page learns about a file that was attached.
#[derive(serde::Serialize)]
pub struct AttachedFile {
    id: String,
    name: String,
    characters: usize,
    warnings: Vec<String>,
}

/// Lets the person pick a file from this computer to ask about. The window is opened here, so the
/// page never chooses a path. The file is read in the same isolated parser the indexer uses, kept
/// only in memory, and sent to Claude only with a question. Returns nothing if they cancelled.
#[tauri::command]
pub async fn attach_file(app: AppHandle, state: State<'_, Arc<AppState>>) -> AppResult<Option<AttachedFile>> {
    use tauri_plugin_dialog::DialogExt;
    if state.attachments.lock().map(|held| held.len()).unwrap_or(0) >= MAX_ATTACHMENTS {
        return Err(AppError::InvalidRequest(format!("You can attach up to {MAX_ATTACHMENTS} files. Remove one first.")));
    }
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        app.dialog()
            .file()
            .add_filter("Documents", &["pdf", "docx", "xlsx", "xls", "csv", "txt", "md"])
            .blocking_pick_file()
    })
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    let Some(chosen) = chosen else { return Ok(None) };
    let path = chosen.into_path().map_err(|error| AppError::Filesystem(error.to_string()))?;
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("File").to_string();
    if !crate::parser::is_supported(&path) {
        return Err(AppError::InvalidRequest("That kind of file cannot be read. Use PDF, Word, Excel, CSV, text or Markdown.".into()));
    }
    let size = fs::metadata(&path).map_err(|error| AppError::Filesystem(error.to_string()))?.len();
    if size > MAX_ATTACHMENT_BYTES {
        return Err(AppError::InvalidRequest("That file is larger than 20 MB.".into()));
    }
    let bytes = fs::read(&path).map_err(|error| AppError::Filesystem(error.to_string()))?;
    let extension = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_string();
    let request = crate::models::ParseRequest { file_name: name.clone(), extension, bytes };
    let parsed = tauri::async_runtime::spawn_blocking(move || crate::parser_ipc::parse_isolated(request, &std::sync::atomic::AtomicBool::new(false)))
        .await
        .map_err(|error| AppError::Internal(error.to_string()))??;
    let characters: usize = parsed.blocks.iter().map(|block| block.text.chars().count()).sum();
    if characters == 0 {
        return Err(AppError::InvalidRequest("No readable text was found in that file.".into()));
    }
    let id = make_id(format!("attach:{name}:{}", Utc::now().timestamp_nanos_opt().unwrap_or_default()).as_bytes());
    state
        .attachments
        .lock()
        .map_err(|_| AppError::Internal("attachments are unavailable".into()))?
        .insert(id.clone(), crate::state::Attachment { name: name.clone(), blocks: parsed.blocks });
    Ok(Some(AttachedFile { id, name, characters, warnings: parsed.warnings }))
}

#[tauri::command]
pub fn remove_attachment(state: State<'_, Arc<AppState>>, id: String) -> AppResult<()> {
    state
        .attachments
        .lock()
        .map_err(|_| AppError::Internal("attachments are unavailable".into()))?
        .remove(&id);
    Ok(())
}

#[tauri::command]
pub async fn search_documents(
    state: State<'_, Arc<AppState>>,
    query: String,
) -> AppResult<Vec<SearchHit>> {
    if query.trim().len() < 2 {
        return Err(AppError::InvalidRequest(
            "Enter at least two characters to search.".into(),
        ));
    }
    Ok(retrieve(&state, query.trim(), 24, Scope::All).await?.hits)
}

/// One earlier exchange in a chat, so a follow-up such as "and what about the deposit?" can be
/// understood. It only helps the model read the new question; the answer still has to come from
/// the passages found for it.
#[derive(serde::Deserialize)]
pub struct ChatTurn {
    question: String,
    answer: String,
    /// This exchange was in the web tab: research, not an answer from the person's documents.
    #[serde(default)]
    web: bool,
}

/// The question as the model should read it, with the last few exchanges ahead of it.
fn question_with_context(question: &str, earlier: &[ChatTurn]) -> String {
    if earlier.is_empty() {
        return question.to_string();
    }
    let research = earlier.iter().any(|turn| turn.web);
    let mut text = if research {
        format!("{}\nEarlier in this conversation, including research the person did on the web. Use the research to understand the new question and to relate it to the passages; facts about their own files come only from the passages.\n", crate::provider::RESEARCH_MARKER)
    } else {
        String::from("Earlier in this conversation (only for understanding what the new question refers to; answer the new question from the passages):\n")
    };
    for turn in earlier.iter().rev().take(if research { 4 } else { 3 }).rev() {
        // Research answers are kept longer: the list of verses is the point of them.
        let answer: String = turn.answer.chars().take(if turn.web { 2_500 } else { 600 }).collect();
        let label = if turn.web { "Web research" } else { "Earlier" };
        text.push_str(&format!("{label} Q: {}\n{label} A: {}\n", turn.question.chars().take(300).collect::<String>(), answer));
    }
    text.push_str(&format!("\nNew question: {question}"));
    text
}

/// What to search for. A short follow-up is searched together with the question before it.
fn search_text(question: &str, earlier: &[ChatTurn]) -> String {
    let question = crate::export::without_file_request(question);
    match earlier.last() {
        Some(last) if question.split_whitespace().count() < 8 => format!("{} {}", last.question, question),
        _ => question,
    }
}

#[tauri::command]
pub async fn answer_question(
    state: State<'_, Arc<AppState>>,
    question: String,
    advanced: Option<bool>,
    earlier: Option<Vec<ChatTurn>>,
    report: Option<String>,
    attachments: Option<Vec<String>>,
    tagged: Option<TaggedFile>,
    scope: Option<String>,
    sources: Option<Vec<String>>,
    types: Option<Vec<String>>,
    workspace: Option<String>,
) -> AppResult<AnswerResponse> {
    let filter = crate::cloud::SearchFilter::checked(sources, types).map_err(AppError::InvalidRequest)?;
    let earlier = earlier.unwrap_or_default();
    if question.trim().len() < 3 {
        return Err(AppError::InvalidRequest(
            "Enter a complete question.".into(),
        ));
    }
    // A question about a report is answered from that report and its evidence, not from everything.
    let searched = search_text(question.trim(), &earlier);
    let mut in_workspace: Option<crate::workspaces::WorkspaceSummary> = None;
    let (retrieved, report_title) = match (report.as_deref(), workspace.as_deref()) {
        (Some(task), _) => {
            let (retrieved, title) = retrieve_report(&state, task, &searched).await?;
            (retrieved, Some(title))
        }
        (None, Some(id)) => {
            let (retrieved, summary) = retrieve_workspace(&state, id, &searched, RERANK_CANDIDATES).await?;
            in_workspace = Some(summary);
            (retrieved, None)
        }
        (None, None) => (retrieve_filtered(&state, &searched, RERANK_CANDIDATES, Scope::from_text(scope.as_deref()), &filter).await?, None),
    };
    let Retrieved {
        hits,
        local,
        cloud,
        cloud_status,
        cloud_question_sent,
    } = retrieved;
    let provider = provider()?;
    let status = provider.status();
    #[cfg(debug_assertions)]
    for hit in &hits {
        debug_note(&format!(
            "  {:?} {} ({} characters)",
            hit.origin,
            hit.document_name,
            hit.excerpt.len()
        ));
    }
    #[cfg(debug_assertions)]
    debug_note(&format!(
        "passages: {local} local, {cloud} cloud; cloud: {:?}; provider: {:?}; {} characters of excerpts",
        cloud_status.state,
        status.state,
        hits.iter().map(|hit| hit.excerpt.len()).sum::<usize>()
    ));
    let mut work = search_steps(&searched, report_title.as_deref(), if filter.is_empty() { Scope::from_text(scope.as_deref()) } else { Scope::Cloud }, &filter, local, cloud, cloud_question_sent);
    if let Some(summary) = &in_workspace {
        work.insert(0, format!("Looked only in the workspace “{}”: {} file{} on this device and {} from the firm cloud index.", summary.name, summary.files, if summary.files == 1 { "" } else { "s" }, summary.cloud_files));
    }
    let withheld = |summary: &str| PrivacyReceipt {
        id: receipt_id(),
        created_at: Utc::now().timestamp(),
        provider: status.kind.clone(),
        model: None,
        document_ids: hits.iter().map(|hit| hit.document_id.clone()).collect(),
        passages_local: local,
        passages_cloud: cloud,
        cloud_question_sent,
        state: status.state.clone(),
        summary: summary.into(),
    };
    if hits.is_empty() || status.state != ProviderState::Ready {
        // Abstain without calling the model when retrieval found nothing or the model is off.
        let receipt = withheld(if hits.is_empty() {
            "No passages matched, so nothing was sent to the AI model."
        } else {
            "Search-only mode: the AI model is unavailable, so nothing was sent."
        });
        state.db.store_receipt(&receipt)?;
        work.push(if hits.is_empty() {
            "Nothing matched, so nothing was sent to the AI model.".into()
        } else {
            "The AI model is unavailable, so nothing was sent.".into()
        });
        return Ok(AnswerResponse {
            claims: Vec::new(),
            citations: Vec::new(),
            abstained: true,
            dropped_claims: 0,
            provider: status,
            privacy_receipt_id: receipt.id.clone(),
            receipt,
            cloud: cloud_status,
            notice: None,
            file: None,
            work,
        });
    }

    // The allowance is checked once, before anything is sent to Claude, including the ranking pass.
    ensure_allowance(&state.cloud).await?;
    let advanced = advanced.unwrap_or(false);

    // A question that names a document is answered from all of it, not from a few passages, so a
    // detailed question about one file is not missed because the answer sat in a part of it that
    // did not rank. This is decided over everything that was found, before any passages are cut,
    // and applies to local files and to files in the firm cloud index.
    let mut hits = hits;
    let mut chosen: Option<Whole> = None;
    let mut included: Vec<String> = Vec::new();
    if report_title.is_none() {
        let cap = if advanced { WHOLE_DOCUMENT_CHARS_ADVANCED } else { WHOLE_DOCUMENT_CHARS };
        if let Some(tag) = &tagged {
            // A file the person tagged is answered from all of it, wherever it lives.
            let origin = if tag.origin == "cloud" { Origin::Cloud } else { Origin::Local };
            let probe = file_hit(&tag.id, &tag.name, origin, "");
            match fetch_document(&state, &probe, cap).await? {
                Whole::No => return Err(AppError::InvalidRequest(format!("{} could not be read. It may have been moved, or you may no longer be able to open it.", tag.name))),
                whole => {
                    if let Whole::Read { blocks, .. } = &whole {
                        let first = blocks.first().map(|block| block.text.clone()).unwrap_or_default();
                        hits.insert(0, file_hit(&tag.id, &tag.name, origin, &first));
                    }
                    included.push(tag.name.clone());
                    chosen = Some(whole);
                }
            }
        }
        let attached: Vec<(String, String, Vec<ParsedBlock>)> = {
            let held = state.attachments.lock().map_err(|_| AppError::Internal("attachments are unavailable".into()))?;
            attachments
                .iter()
                .flatten()
                .filter_map(|id| held.get(id).map(|file| (id.clone(), file.name.clone(), file.blocks.clone())))
                .collect()
        };
        if !attached.is_empty() {
            for (_, name, _) in &attached {
                included.push(name.clone());
            }
            if chosen.is_none() && attached.len() == 1 {
                let (id, name, blocks) = attached[0].clone();
                let total: usize = blocks.iter().map(|block| block.text.chars().count()).sum();
                let first = blocks.first().map(|block| block.text.clone()).unwrap_or_default();
                hits.insert(0, file_hit(&format!("attachment:{id}"), &name, Origin::Local, &first));
                chosen = Some(if total > cap { Whole::TooLong { name } } else { Whole::Read { name, blocks } });
            }
            if !matches!(chosen, Some(Whole::Read { .. })) {
                // Several files, or one that is too long to send whole: their passages join the rest.
                let mut passages = Vec::new();
                for (id, name, blocks) in &attached {
                    passages.extend(attachment_hits(id, name, blocks));
                }
                passages.extend(hits);
                hits = passages;
            }
        }
    }
    let found = hits;
    let whole_document = if report_title.is_some() {
        Whole::No
    } else if let Some(whole) = chosen {
        whole
    } else {
        read_whole_document(&state, question.trim(), &found, advanced).await?
    };
    let named = match &whole_document {
        Whole::Read { name, .. } | Whole::TooLong { name } => Some(name.clone()),
        Whole::No => None,
    };
    let mut hits = found.clone();
    if matches!(whole_document, Whole::Read { .. }) {
        // The whole file is what Claude reads, so there is nothing to rank. The other passages
        // are only kept as the list of sources.
        hits.truncate(ANSWER_PASSAGES);
    } else if report_title.is_some() {
        // The report and its evidence are already the right set; nothing is ranked or cut further.
        hits.truncate(ANSWER_PASSAGES + 1);
    } else {
        hits = best_passages(provider.as_ref(), &state.cloud, question.trim(), hits).await;
        hits = expand_short_documents(&state, hits).await;
    }
    // A document the question names stays in, first, even if ranking left it out.
    if let Some(name) = &named {
        match hits.iter().position(|hit| &hit.document_name == name) {
            Some(position) => {
                let first = hits.remove(position);
                hits.insert(0, first);
            }
            None => {
                if let Some(first) = found.iter().find(|hit| &hit.document_name == name) {
                    hits.insert(0, first.clone());
                    hits.truncate(ANSWER_PASSAGES);
                }
            }
        }
    }
    if !included.is_empty() {
        work.push(format!("Included the file(s) you chose: {}.", included.join(", ")));
    }
    work.push(match &whole_document {
        Whole::Read { name, .. } => format!("Read all of {name}, because the question is about that file."),
        Whole::TooLong { name } => format!("{name} is too long to read in full, so its best passages were used."),
        Whole::No if report_title.is_some() => "Used the report and the passages it was drawn from.".into(),
        Whole::No => format!("Picked the {} most relevant passages out of {}.", hits.len(), found.len()),
    });
    #[cfg(debug_assertions)]
    debug_note(&format!(
        "sending: {}",
        hits.iter()
            .map(|hit| format!("{:?}:{}", hit.origin, hit.document_name))
            .collect::<Vec<_>>()
            .join(" | ")
    ));
    let long_document_note = match &whole_document {
        Whole::TooLong { name } if !advanced => Some(format!(
            " {name} is too long to read in full here, so this answer used its best passages. Turn on Advanced analysis to read all of it."
        )),
        Whole::TooLong { name } => Some(format!(
            " {name} is too long to read in full, so this answer used its best passages."
        )),
        Whole::Read { .. } | Whole::No => None,
    };
    let document_blocks = match &whole_document {
        Whole::Read { blocks, .. } => blocks.clone(),
        _ => Vec::new(),
    };
    let whole = (!document_blocks.is_empty()).then_some(document_blocks.as_slice());
    let asked = match &report_title {
        Some(title) => format!(
            "This is a question about the report called \"{title}\". The first source is the report itself and the others are passages from the files it was drawn from. Answer only from them, and say which file a figure or claim comes from. If they do not say, say so. Question: {}",
            question.trim()
        ),
        None => match &in_workspace {
            Some(summary) => {
                let about = if summary.description.is_empty() { String::new() } else { format!(" ({})", summary.description) };
                format!("This question is asked inside the workspace \"{}\"{about}, which holds the files that follow. Question: {}", summary.name, question.trim())
            }
            None => question.trim().to_string(),
        },
    };
    let model_question = question_with_context(&asked, &earlier);
    let mut result = provider
        .answer(&model_question, &hits, whole, advanced)
        .await?;
    count_usage(&state.cloud, &result).await;
    if result.claims.is_empty() && whole.is_some() {
        // The whole file did not answer it, so the question is probably about something else.
        // Try again with the best passages from every source instead.
        hits = best_passages(provider.as_ref(), &state.cloud, question.trim(), found).await;
        hits = expand_short_documents(&state, hits).await;
        result = provider
            .answer(&model_question, &hits, None, advanced)
            .await?;
        count_usage(&state.cloud, &result).await;
    }
    #[cfg(debug_assertions)]
    debug_note(&format!(
        "model gave {} claim(s), {} dropped",
        result.claims.len(),
        result.dropped_claims
    ));
    validate_claims(&result.claims, &hits)?;
    let mut cited_ids = result
        .claims
        .iter()
        .flat_map(|claim| claim.citation_ids.iter().cloned())
        .collect::<HashSet<_>>();
    if cited_ids.is_empty() {
        // An abstention still names the retrieved documents, but never displays a raw
        // source excerpt as though it were an AI-written answer.
        cited_ids.extend(hits.iter().map(|hit| hit.citation_id.clone()));
    }
    let mut cited_documents = HashSet::new();
    let citations = hits
        .iter()
        .filter(|hit| {
            cited_ids.contains(&hit.citation_id) && cited_documents.insert(hit.document_id.clone())
        })
        .map(|hit| Citation {
            id: hit.citation_id.clone(),
            document_id: hit.document_id.clone(),
            document_name: hit.document_name.clone(),
            excerpt: hit.excerpt.clone(),
            location: hit.location.clone(),
            location_label: hit.location_label.clone(),
            ocr: hit.ocr,
            origin: hit.origin,
            link: hit.link.clone(),
        })
        .collect::<Vec<_>>();
    let document_ids = hits
        .iter()
        .map(|hit| hit.document_id.clone())
        .collect::<HashSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    // The receipt counts what was sent, not everything that was found.
    let cloud = hits
        .iter()
        .filter(|hit| hit.origin == Origin::Cloud)
        .count() as u32;
    let local = hits.len() as u32 - cloud;
    work.push(format!(
        "Sent your question and {} passage(s) from {} document(s) to Claude {}.",
        hits.len(),
        document_ids.len(),
        receipt_model(&result.usage, &status)
    ));
    work.push(if result.dropped_claims > 0 {
        format!("Checked each statement against its source and left out {} that no source supported.", result.dropped_claims)
    } else {
        "Checked each statement against its source; every one is supported.".into()
    });
    work.push(format!("Cited {} source(s).", citations.len()));
    let mut summary = if cloud > 0 {
        format!(
            "{} {cloud} of the passages came from the firm cloud index.",
            result.summary
        )
    } else {
        result.summary
    };
    if !included.is_empty() {
        summary.push_str(&format!(" It included the file(s) you chose: {}.", included.join(", ")));
    }
    let notice = long_document_note
        .as_ref()
        .map(|note| note.trim().to_string());
    if let Some(note) = long_document_note {
        summary.push_str(&note);
    }
    let receipt = PrivacyReceipt {
        id: receipt_id(),
        created_at: Utc::now().timestamp(),
        provider: status.kind.clone(),
        // The model or models that actually answered, not just the provider's default.
        model: if result.usage.is_empty() {
            status.model.clone()
        } else {
            let mut used: Vec<String> = Vec::new();
            for entry in &result.usage {
                if !used.contains(&entry.model) {
                    used.push(entry.model.clone());
                }
            }
            Some(used.join(", "))
        },
        document_ids,
        passages_local: local,
        passages_cloud: cloud,
        cloud_question_sent,
        state: status.state.clone(),
        summary,
    };
    state.db.store_receipt(&receipt)?;
    Ok(AnswerResponse {
        abstained: result.claims.is_empty(),
        claims: result.claims,
        citations,
        dropped_claims: result.dropped_claims,
        provider: status,
        privacy_receipt_id: receipt.id.clone(),
        receipt,
        cloud: cloud_status,
        notice,
        file: crate::export::wanted_file(&question).map(String::from),
        work,
    })
}

/// The most of the attached files' text sent with a web question, in all.
const WEB_ATTACHMENT_CHARS: usize = 80_000;

/// The earlier web conversation, as the web tab's model should read it before the new question.
fn web_message(question: &str, earlier: &[ChatTurn], files: &[(String, String)]) -> String {
    let mut text = String::new();
    // Files the person attached to this message, and only those: they chose to send them.
    for (name, contents) in files {
        text.push_str(&format!("<attached_file name=\"{}\">\n{}\n</attached_file>\n\n", name.replace('"', "'"), contents));
    }
    let turns: Vec<&ChatTurn> = earlier.iter().filter(|turn| turn.web).collect();
    if !turns.is_empty() {
        text.push_str("Earlier in this conversation:\n");
        for turn in turns.iter().rev().take(6).rev() {
            text.push_str(&format!("Q: {}\nA: {}\n", turn.question.chars().take(600).collect::<String>(), turn.answer.chars().take(3_000).collect::<String>()));
        }
        text.push('\n');
    }
    text.push_str(&format!("Question: {question}"));
    text
}

/// The web tab: regular Claude. Any question, with the web searched when that helps. None of the
/// person's documents are used or sent, and only earlier answers from this same tab join the question.
#[tauri::command]
pub async fn ask_web(
    state: State<'_, Arc<AppState>>,
    question: String,
    advanced: Option<bool>,
    earlier: Option<Vec<ChatTurn>>,
    attachments: Option<Vec<String>>,
) -> AppResult<AnswerResponse> {
    let question = question.trim().to_string();
    if question.chars().count() < 2 || question.chars().count() > 4_000 {
        return Err(AppError::InvalidRequest("Ask a question of up to 4,000 characters.".into()));
    }
    let earlier = earlier.unwrap_or_default();
    let provider = provider()?;
    let status = provider.status();
    if status.state != ProviderState::Ready {
        return Err(AppError::InvalidRequest("The AI model is not available, so the web tab cannot answer right now.".into()));
    }
    ensure_allowance(&state.cloud).await?;
    let advanced = advanced.unwrap_or(false);
    // Files attached to this message, read on this device when they were attached. Nothing else of the
    // person's is sent: not their indexed documents, and not earlier answers drawn from them.
    let files: Vec<(String, String)> = {
        let held = state.attachments.lock().map_err(|_| AppError::Internal("attachments are unavailable".into()))?;
        let mut room = WEB_ATTACHMENT_CHARS;
        attachments
            .iter()
            .flatten()
            .filter_map(|id| held.get(id))
            .map(|file| {
                let text: String = file.blocks.iter().map(|block| block.text.trim()).collect::<Vec<_>>().join("\n\n").chars().take(room).collect();
                room = room.saturating_sub(text.chars().count());
                (file.name.clone(), text)
            })
            .collect()
    };
    let reply = provider.web_chat(&web_message(&question, &earlier, &files), advanced).await?;
    count_models(&state.cloud, &reply.usage).await;

    let model = reply.usage.first().map(|used| used.model.clone()).or(status.model.clone());
    let sources: Vec<Citation> = reply
        .sources
        .iter()
        .enumerate()
        .map(|(index, source)| Citation {
            id: format!("web:{index}"),
            document_id: format!("web:{index}"),
            document_name: if source.title.trim().is_empty() { source.url.clone() } else { source.title.clone() },
            excerpt: source.cited.clone(),
            location: Default::default(),
            location_label: "Web".into(),
            ocr: false,
            origin: Origin::Cloud,
            link: Some(source.url.clone()),
        })
        .collect();
    let summary = if reply.could_search {
        format!("Web tab: your question{}{} was sent to Claude, which searched the web {} time(s). None of your indexed documents were used or sent.", if earlier.iter().any(|turn| turn.web) { ", the earlier answers from this tab" } else { "" }, if files.is_empty() { String::new() } else { format!(" and the {} file(s) you attached", files.len()) }, reply.searches)
    } else {
        "Web tab: your question was sent to Claude, which answered from what it already knows because web search is not available with this AI provider. None of your indexed documents were used or sent.".to_string()
    };
    let receipt = PrivacyReceipt {
        id: receipt_id(),
        created_at: Utc::now().timestamp(),
        provider: status.kind.clone(),
        model,
        document_ids: Vec::new(),
        passages_local: 0,
        passages_cloud: 0,
        cloud_question_sent: false,
        state: status.state.clone(),
        summary,
    };
    state.db.store_receipt(&receipt)?;
    let mut work = vec!["Asked in the web tab, without your indexed documents.".to_string()];
    if !files.is_empty() {
        work.push(format!("Included the {} file(s) you attached.", files.len()));
    }
    work.push(if reply.could_search {
        match reply.searches {
            0 => "Answered without needing to search the web.".to_string(),
            n => format!("Searched the web {n} time{}.", if n == 1 { "" } else { "s" }),
        }
    } else {
        "Web search is not available with this AI provider, so this was answered from what it already knows.".to_string()
    });
    if !sources.is_empty() {
        work.push(format!("Cited {} web page(s).", sources.len()));
    }
    let abstained = reply.text.trim().is_empty();
    Ok(AnswerResponse {
        claims: vec![crate::models::AnswerClaim { text: reply.text.clone(), citation_ids: sources.iter().map(|source| source.id.clone()).collect() }],
        citations: sources,
        abstained,
        dropped_claims: 0,
        provider: status,
        privacy_receipt_id: receipt.id.clone(),
        receipt,
        cloud: state.cloud.status(),
        notice: if reply.could_search { None } else { Some("Web search is not available with this AI provider, so this answer comes from what Claude already knows.".into()) },
        file: None,
        work,
    })
}

/// Drafts an email from the person's request, for them to read, change and send. Only the request
/// and today's date go to the model: no documents.
#[tauri::command]
pub async fn draft_email(state: State<'_, Arc<AppState>>, request: String, today: String) -> AppResult<crate::email_draft::EmailDraft> {
    let request = request.trim();
    if request.chars().count() < 5 || request.chars().count() > crate::email_draft::MAX_REQUEST_CHARS {
        return Err(AppError::InvalidRequest(format!("Describe the email in 5 to {} characters.", crate::email_draft::MAX_REQUEST_CHARS)));
    }
    // Only the look of a date is passed on, so nothing else can be slipped into the instructions.
    let today: String = today.chars().filter(|c| c.is_ascii_alphanumeric() || " ,".contains(*c)).take(60).collect();
    let provider = provider()?;
    if provider.status().state != ProviderState::Ready {
        return Err(AppError::InvalidRequest("The AI model is not available, so an email cannot be drafted right now.".into()));
    }
    ensure_allowance(&state.cloud).await?;
    let (reply, usage) = provider
        .complete(&crate::email_draft::prompt(&today), &format!("Request: {request}"), crate::email_draft::DRAFT_MAX_TOKENS)
        .await?;
    count_models(&state.cloud, &usage).await;
    crate::email_draft::parse_draft(&reply)
}

/// Creates a Zoom meeting in the person's own Zoom account and returns the link to join it. The page
/// calls this only from the preview's "Add a Zoom link" button, which is the person's say-so.
#[tauri::command]
pub async fn cloud_zoom_meeting(
    state: State<'_, Arc<AppState>>,
    topic: String,
    start: String,
    duration_minutes: u32,
    timezone: String,
    agenda: String,
) -> AppResult<serde_json::Value> {
    knowledge_call(
        &state,
        reqwest::Method::POST,
        "/v1/zoom/meeting",
        &[],
        Some(serde_json::json!({ "topic": topic, "start": start, "duration_minutes": duration_minutes, "timezone": timezone, "agenda": agenda, "approved": true })),
    )
    .await
}

/// A meeting is named by the ID the cloud service gave it: 36 letters, digits and dashes, so it can
/// never change which address is called.
fn check_meeting_id(id: &str) -> AppResult<()> {
    if id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(())
    } else {
        Err(AppError::InvalidRequest("That meeting is not valid.".into()))
    }
}

/// The Zoom meetings the person made from the app that are still on: the only ones it can change.
#[tauri::command]
pub async fn cloud_zoom_meetings(state: State<'_, Arc<AppState>>) -> AppResult<serde_json::Value> {
    knowledge_call(&state, reqwest::Method::GET, "/v1/zoom/meetings", &[], None).await
}

/// Moves one of those meetings, after the person chose "Move meeting" on it.
#[tauri::command]
pub async fn cloud_zoom_reschedule(
    state: State<'_, Arc<AppState>>,
    id: String,
    start: String,
    duration_minutes: u32,
    timezone: String,
) -> AppResult<serde_json::Value> {
    check_meeting_id(&id)?;
    knowledge_call(
        &state,
        reqwest::Method::POST,
        &format!("/v1/zoom/meetings/{id}/reschedule"),
        &[],
        Some(serde_json::json!({ "start": start, "duration_minutes": duration_minutes, "timezone": timezone, "approved": true })),
    )
    .await
}

/// Cancels one of those meetings, after the person chose "Cancel meeting" and confirmed.
#[tauri::command]
pub async fn cloud_zoom_cancel(state: State<'_, Arc<AppState>>, id: String) -> AppResult<serde_json::Value> {
    check_meeting_id(&id)?;
    knowledge_call(&state, reqwest::Method::POST, &format!("/v1/zoom/meetings/{id}/cancel"), &[], Some(serde_json::json!({ "approved": true }))).await
}

/// Sends the email the person has just read, from their own Outlook, through the firm cloud service.
/// The page calls this only from the email preview's Send button.
#[tauri::command]
pub async fn cloud_send_email(
    state: State<'_, Arc<AppState>>,
    to: Vec<String>,
    cc: Vec<String>,
    subject: String,
    body: String,
    confirm_outside: bool,
) -> AppResult<serde_json::Value> {
    knowledge_call(
        &state,
        reqwest::Method::POST,
        "/v1/mail/send",
        &[],
        Some(serde_json::json!({ "to": to, "cc": cc, "subject": subject, "body": body, "approved": true, "confirm_outside": confirm_outside })),
    )
    .await
}

/// Drafts a helper from a sentence. Only that sentence is sent to the model, never any document.
#[tauri::command]
pub async fn draft_helper(state: State<'_, Arc<AppState>>, description: String) -> AppResult<crate::helper_draft::HelperDraft> {
    let description = description.trim();
    if description.chars().count() < 10 {
        return Err(AppError::InvalidRequest("Say a little more about what the helper should do.".into()));
    }
    if description.chars().count() > crate::helper_draft::MAX_DESCRIPTION_CHARS {
        return Err(AppError::InvalidRequest(format!("Keep the description under {} characters.", crate::helper_draft::MAX_DESCRIPTION_CHARS)));
    }
    let provider = provider()?;
    if provider.status().state != ProviderState::Ready {
        return Err(AppError::InvalidRequest("The AI model is unavailable, so a helper cannot be drafted. You can still start from a template.".into()));
    }
    ensure_allowance(&state.cloud).await?;
    let (reply, usage) = provider
        .complete(crate::helper_draft::DRAFT_PROMPT, &format!("The helper I want: {description}"), crate::helper_draft::DRAFT_MAX_TOKENS)
        .await?;
    count_models(&state.cloud, &usage).await;
    crate::helper_draft::parse_draft(&reply)
}

/// "Haiku 5.5", or the models that answered when more than one was used.
fn receipt_model(usage: &[crate::provider::ModelUsage], status: &ProviderStatus) -> String {
    let mut names: Vec<String> = Vec::new();
    let models = usage.iter().map(|entry| Some(entry.model.clone())).chain(usage.is_empty().then(|| status.model.clone()));
    for model in models.flatten() {
        let label = crate::provider::model_label(&model);
        if !names.contains(&label) {
            names.push(label);
        }
    }
    if names.is_empty() { "the AI model".into() } else { names.join(" and ") }
}

/// The first steps of the work, from what searching found.
fn search_steps(searched: &str, report: Option<&str>, scope: Scope, filter: &crate::cloud::SearchFilter, local: u32, cloud: u32, cloud_question_sent: bool) -> Vec<String> {
    let mut steps = Vec::new();
    match report {
        Some(title) => steps.push(format!("Opened the report “{title}” and the evidence it was drawn from.")),
        None => {
            let place: String = match scope {
                Scope::Local => "on this device only".into(),
                Scope::Cloud if !filter.is_empty() => format!("in the firm cloud only, limited to {},", filter.describe()),
                Scope::Cloud => "in the firm cloud only".into(),
                Scope::All => "on this device and in the firm cloud".into(),
            };
            steps.push(format!("Searched {place} for “{}”.", searched.trim()));
            steps.push(format!(
                "Found {local} passage(s) on this device and {cloud} in the firm cloud.{}",
                if cloud_question_sent { " The question text was sent to the firm cloud index to search it." } else { "" }
            ));
        }
    }
    steps
}

#[tauri::command]
pub async fn cloud_status(state: State<'_, Arc<AppState>>) -> AppResult<CloudStatus> {
    Ok(state.cloud.status())
}

#[tauri::command]
pub async fn cloud_sign_in(
    state: State<'_, Arc<AppState>>,
    choice: Option<crate::cloud::SignInChoice>,
) -> AppResult<CloudStatus> {
    state
        .cloud
        .sign_in(
            &choice.unwrap_or(crate::cloud::SignInChoice::Any),
            crate::cloud::open_browser,
        )
        .await
        .map_err(|error| AppError::Cloud(error.message()))?;
    Ok(state.cloud.status())
}

#[tauri::command]
pub async fn cloud_connectors(
    state: State<'_, Arc<AppState>>,
) -> AppResult<crate::cloud::ConnectorsView> {
    state
        .cloud
        .connectors_view()
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

/// Opens the provider's sign-in page in the browser. The connection appears in the list once the
/// administrator approves access there.
#[tauri::command]
pub async fn cloud_connect(
    state: State<'_, Arc<AppState>>,
    kind: String,
    domains: Vec<String>,
    reconnect: Option<String>,
    scope: Option<String>,
) -> AppResult<()> {
    let url = state
        .cloud
        .start_connection(&kind, &domains, reconnect.as_deref(), scope.as_deref())
        .await
        .map_err(|error| AppError::Cloud(error.message()))?;
    crate::cloud::open_browser(&url);
    Ok(())
}

/// Changes who a connection's files are for: only the person who connected it, or the workspace.
#[tauri::command]
pub async fn cloud_set_scope(state: State<'_, Arc<AppState>>, id: String, scope: String) -> AppResult<()> {
    state
        .cloud
        .set_scope(&id, &scope)
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

#[tauri::command]
pub async fn cloud_disconnect(state: State<'_, Arc<AppState>>, id: String) -> AppResult<()> {
    state
        .cloud
        .disconnect(&id)
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

#[tauri::command]
pub async fn cloud_sync_now(state: State<'_, Arc<AppState>>, id: String) -> AppResult<()> {
    state
        .cloud
        .sync_now(&id)
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

#[tauri::command]
pub async fn cloud_sign_out(state: State<'_, Arc<AppState>>) -> AppResult<CloudStatus> {
    state.cloud.sign_out();
    Ok(state.cloud.status())
}

#[tauri::command]
pub async fn set_cloud_enabled(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> AppResult<CloudStatus> {
    state
        .cloud
        .set_enabled(enabled)
        .map_err(|error| AppError::Cloud(error.message()))?;
    Ok(state.cloud.status())
}

fn receipt_id() -> String {
    make_id(
        format!(
            "receipt:{}",
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        )
        .as_bytes(),
    )
}

/// Cuts what was found down to what will be sent. When there is more than that, a cheap model puts
/// the passages that really answer the question first; if it cannot, the merged order stands.
async fn best_passages(
    provider: &dyn crate::provider::InferenceProvider,
    cloud: &crate::cloud::Cloud,
    question: &str,
    mut hits: Vec<SearchHit>,
) -> Vec<SearchHit> {
    if hits.len() > ANSWER_PASSAGES {
        match provider.rerank(question, &hits).await {
            Ok(ranking) => {
                #[cfg(debug_assertions)]
                debug_note(&format!("ranking put first: {:?}", ranking.order));
                count_models(cloud, &ranking.usage).await;
                hits = apply_ranking(hits, &ranking.order);
            }
            Err(error) => {
                #[cfg(debug_assertions)]
                debug_note(&format!("ranking failed: {error}"));
                #[cfg(not(debug_assertions))]
                let _ = error;
            }
        }
        hits.truncate(ANSWER_PASSAGES);
    }
    hits
}

/// The best documents this many are sent whole when they are short. A passage can hold a table's
/// line items while the total sits in the next one, which gives a confident, partial answer.
const EXPAND_DOCUMENTS: usize = 2;
/// A document longer than this, about 3,000 tokens, stays as passages.
const EXPAND_MAX_CHARS: usize = 12_000;

/// Replaces the passages of the top documents with their whole text, when they are short. A
/// document that is long or cannot be fetched keeps its passages.
async fn expand_short_documents(state: &AppState, hits: Vec<SearchHit>) -> Vec<SearchHit> {
    let mut result: Vec<SearchHit> = Vec::with_capacity(hits.len());
    let mut expanded: Vec<String> = Vec::new();
    let mut considered: Vec<String> = Vec::new();
    for mut hit in hits {
        if expanded.contains(&hit.document_id) {
            continue; // Its whole text is already in the list.
        }
        if considered.len() < EXPAND_DOCUMENTS && !considered.contains(&hit.document_id) {
            considered.push(hit.document_id.clone());
            if let Ok(Whole::Read { blocks, .. }) =
                fetch_document(state, &hit, EXPAND_MAX_CHARS).await
            {
                hit.excerpt = blocks
                    .iter()
                    .map(|block| block.text.trim())
                    .collect::<Vec<_>>()
                    .join("\n\n");
                hit.location_label = match hit.origin {
                    Origin::Cloud => "Cloud · Whole file".into(),
                    Origin::Local => "Whole file".into(),
                };
                // Passages of this document that came earlier are now part of this one.
                result.retain(|earlier| earlier.document_id != hit.document_id);
                expanded.push(hit.document_id.clone());
            }
        }
        result.push(hit);
    }
    result
}

/// Passages found before ranking, and passages sent to Claude after it.
const RERANK_CANDIDATES: usize = 24;
const ANSWER_PASSAGES: usize = 8;

/// The ranked passages first, in the order given, then the rest in their original order.
fn apply_ranking(hits: Vec<SearchHit>, order: &[usize]) -> Vec<SearchHit> {
    let mut slots: Vec<Option<SearchHit>> = hits.into_iter().map(Some).collect();
    let mut result: Vec<SearchHit> = order
        .iter()
        .filter_map(|index| slots.get_mut(*index).and_then(Option::take))
        .collect();
    result.extend(slots.into_iter().flatten());
    result
}

/// Longest document, in characters, sent whole to the everyday model. About 100,000 tokens: above
/// that Haiku costs five times as much per token, so a longer file needs Advanced analysis.
const WHOLE_DOCUMENT_CHARS: usize = 380_000;
/// Longest document sent whole to the advanced model, about 375,000 tokens.
const WHOLE_DOCUMENT_CHARS_ADVANCED: usize = 1_500_000;

/// What reading the document a question names came to.
enum Whole {
    /// The question does not name a document, or the document could not be fetched.
    No,
    Read {
        name: String,
        blocks: Vec<ParsedBlock>,
    },
    /// The document is named but longer than may be sent.
    TooLong { name: String },
}

/// Finds the document the question names among the results and fetches all of it: from this
/// device for a local file, from the firm's server for a cloud file (which checks the person may
/// read it). A cloud server that cannot be reached means the answer uses passages instead.
async fn read_whole_document(
    state: &AppState,
    question: &str,
    hits: &[SearchHit],
    advanced: bool,
) -> AppResult<Whole> {
    let Some(hit) = hits
        .iter()
        .find(|hit| question_names_document(question, &hit.document_name))
    else {
        return Ok(Whole::No);
    };
    let cap = if advanced {
        WHOLE_DOCUMENT_CHARS_ADVANCED
    } else {
        WHOLE_DOCUMENT_CHARS
    };
    fetch_document(state, hit, cap).await
}

/// Fetches all of a document, if it is no longer than `cap` characters: from this device for a
/// local file, from the firm's server for a cloud file (which checks the person may read it).
async fn fetch_document(state: &AppState, hit: &SearchHit, cap: usize) -> AppResult<Whole> {
    let name = hit.document_name.clone();
    let blocks = match hit.origin {
        Origin::Local => state.db.blocks_for_document(&hit.document_id)?,
        Origin::Cloud => {
            let id = hit
                .document_id
                .strip_prefix("cloud:")
                .unwrap_or(&hit.document_id);
            match state.cloud.document_text(id, cap).await {
                Ok(Some(document)) if document.too_long => return Ok(Whole::TooLong { name }),
                Ok(Some(document)) => document
                    .passages
                    .into_iter()
                    .map(|passage| ParsedBlock {
                        text: passage.text,
                        location: SourceLocation {
                            heading: passage.label,
                            ..Default::default()
                        },
                        ocr: false,
                        ocr_confidence: None,
                    })
                    .collect(),
                _ => return Ok(Whole::No),
            }
        }
    };
    let total: usize = blocks.iter().map(|block| block.text.chars().count()).sum();
    if blocks.is_empty() {
        Ok(Whole::No)
    } else if total > cap {
        Ok(Whole::TooLong { name })
    } else {
        Ok(Whole::Read { name, blocks })
    }
}

/// Whether the question names this document: every word of its name, or all but one when the name
/// is four words or longer. Sharing most of a short name is not naming it: "what did the men's
/// retreat cost us" is about the retreat, not about "Men's Retreat Planning", and sending that file
/// in full would leave out the budget that has the answer.
fn question_names_document(question: &str, document_name: &str) -> bool {
    let words = |text: &str| {
        text.to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .filter(|word| word.chars().count() >= 3)
            .map(str::to_owned)
            .collect::<HashSet<_>>()
    };
    let stem = document_name
        .rsplit_once('.')
        .map_or(document_name, |(stem, _)| stem);
    let asked = words(question);
    let name = words(stem);
    let matched = name.iter().filter(|word| asked.contains(*word)).count();
    let needed = if name.len() >= 4 {
        name.len() - 1
    } else {
        name.len()
    };
    needed > 0 && matched >= needed
}

/// Refuses a question once this person has used up their allowance for the month. If the
/// firm's server cannot be asked (signed out, switched off, or down), the question goes ahead:
/// a person using only local files is not held up by a service they are not using.
async fn ensure_allowance(cloud: &crate::cloud::Cloud) -> AppResult<()> {
    match cloud.usage().await {
        Ok(usage) if usage.remaining_micros <= 0 => Err(AppError::Cloud(allowance_message(&usage))),
        _ => Ok(()),
    }
}

/// Counts a request against the person's allowance. The request has been paid for whatever the
/// answer turns out to be, and a server that cannot be reached must not stop the answer.
async fn count_usage(cloud: &crate::cloud::Cloud, result: &crate::provider::ProviderAnswer) {
    count_models(cloud, &result.usage).await;
}

async fn count_models(cloud: &crate::cloud::Cloud, usage: &[crate::provider::ModelUsage]) {
    for used in usage {
        let _ = cloud
            .record_usage(&used.model, used.input_tokens, used.output_tokens)
            .await;
    }
}

fn dollars(micros: i64) -> String {
    format!("${:.2}", micros as f64 / 1_000_000.0)
}

fn allowance_message(usage: &crate::cloud::Usage) -> String {
    let resets = Utc
        .timestamp_opt(usage.period_end_epoch as i64, 0)
        .single()
        .map(|date| format!(" It starts again on {}.", date.format("%B %-d")))
        .unwrap_or_default();
    format!(
        "You have used your {} of AI answers for this month.{resets} Search still works. Ask your administrator to raise the limit if you need more.",
        dollars(usage.limit_micros)
    )
}

#[tauri::command]
pub async fn cloud_usage(
    state: State<'_, Arc<AppState>>,
) -> AppResult<Option<crate::cloud::Usage>> {
    Ok(state.cloud.usage().await.ok())
}

#[tauri::command]
pub async fn cloud_set_usage_limit(
    state: State<'_, Arc<AppState>>,
    cents: Option<i64>,
) -> AppResult<crate::cloud::Usage> {
    state
        .cloud
        .set_usage_limit(cents)
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

// ---- Company knowledge: the interface shows what the server found and sends reviews back ----

async fn knowledge_call(
    state: &AppState,
    method: reqwest::Method,
    path: &str,
    query: &[(&str, &str)],
    body: Option<serde_json::Value>,
) -> AppResult<serde_json::Value> {
    state
        .cloud
        .knowledge(method, path, query, body.as_ref())
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

#[tauri::command]
pub async fn cloud_knowledge(state: State<'_, Arc<AppState>>) -> AppResult<serde_json::Value> {
    knowledge_call(&state, reqwest::Method::GET, "/v1/knowledge", &[], None).await
}

#[tauri::command]
pub async fn cloud_knowledge_enable(
    state: State<'_, Arc<AppState>>,
    enabled: bool,
) -> AppResult<serde_json::Value> {
    let body = serde_json::json!({ "enabled": enabled });
    knowledge_call(
        &state,
        reqwest::Method::PUT,
        "/v1/knowledge/settings",
        &[],
        Some(body),
    )
    .await
}

#[tauri::command]
pub async fn cloud_knowledge_list(
    state: State<'_, Arc<AppState>>,
    what: String,
    status: Option<String>,
    kind: Option<String>,
    query: Option<String>,
    review_only: Option<bool>,
) -> AppResult<serde_json::Value> {
    let path = match what.as_str() {
        "facts" => "/v1/knowledge/facts",
        "entities" => "/v1/knowledge/entities",
        _ => return Err(AppError::InvalidRequest("unknown list".into())),
    };
    let review = if review_only == Some(true) {
        "true"
    } else {
        ""
    };
    knowledge_call(
        &state,
        reqwest::Method::GET,
        path,
        &[
            ("status", status.as_deref().unwrap_or("")),
            ("kind", kind.as_deref().unwrap_or("")),
            ("q", query.as_deref().unwrap_or("")),
            ("review_only", review),
        ],
        None,
    )
    .await
}

/// Confirms, rejects or corrects a fact or an entity.
#[tauri::command]
pub async fn cloud_knowledge_review(
    state: State<'_, Arc<AppState>>,
    what: String,
    id: String,
    action: String,
    text: Option<String>,
) -> AppResult<serde_json::Value> {
    let kind = match what.as_str() {
        "fact" => "facts",
        "entity" => "entities",
        _ => return Err(AppError::InvalidRequest("unknown item".into())),
    };
    // The ID goes into a web address, so only a real ID is allowed through.
    if id.len() != 36 || !id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        return Err(AppError::InvalidRequest("that is not a valid item".into()));
    }
    let body = serde_json::json!({ "action": action, "text": text });
    knowledge_call(
        &state,
        reqwest::Method::POST,
        &format!("/v1/knowledge/{kind}/{id}/review"),
        &[],
        Some(body),
    )
    .await
}

#[tauri::command]
pub async fn cloud_knowledge_profile_review(
    state: State<'_, Arc<AppState>>,
    summary: Option<String>,
    services: Option<Vec<String>>,
) -> AppResult<serde_json::Value> {
    let body = serde_json::json!({ "summary": summary, "services": services });
    knowledge_call(
        &state,
        reqwest::Method::PUT,
        "/v1/knowledge/profile/review",
        &[],
        Some(body),
    )
    .await
}

#[tauri::command]
pub async fn cloud_connection_files(
    state: State<'_, Arc<AppState>>,
    id: String,
) -> AppResult<Vec<crate::cloud::NameOnlyFile>> {
    state
        .cloud
        .connection_files(&id)
        .await
        .map_err(|error| AppError::Cloud(error.message()))
}

/// Opens a cloud document in the browser, where its own service checks who is signed in.
#[tauri::command]
pub async fn open_document_link(url: String) -> AppResult<()> {
    crate::cloud::check_document_link(&url).map_err(|error| AppError::Cloud(error.message()))?;
    crate::cloud::open_browser(&url);
    Ok(())
}

/// Opens the person's own mail or calendar with a draft or a meeting filled in. Only the compose
/// and new-event pages of Gmail, Outlook and Google Calendar, or a plain `mailto:` link, are opened.
#[tauri::command]
pub async fn open_prepared_link(url: String) -> AppResult<()> {
    crate::cloud::check_prepared_link(&url).map_err(|error| AppError::Cloud(error.message()))?;
    crate::cloud::open_browser(&url);
    Ok(())
}

/// Writes a meeting to a calendar file and opens it with the person's calendar app. The file name
/// is fixed and the contents are checked, so nothing else can be written or run.
#[tauri::command]
pub async fn open_calendar_file(text: String) -> AppResult<()> {
    crate::cloud::check_calendar_file(&text).map_err(|error| AppError::Cloud(error.message()))?;
    let folder = std::env::temp_dir().join("private-ai-workspace");
    std::fs::create_dir_all(&folder).map_err(|error| AppError::Internal(error.to_string()))?;
    let path = folder.join("meeting.ics");
    std::fs::write(&path, text).map_err(|error| AppError::Internal(error.to_string()))?;
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(&path).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("rundll32").args(["url.dll,FileProtocolHandler"]).arg(&path).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(&path).spawn();
    Ok(())
}

/// Saves a report as a Word or Excel file. The person picks the place in a save window opened here,
/// so the page never chooses a path. Returns the saved path, or nothing if they cancelled.
#[tauri::command]
pub async fn export_report(
    app: AppHandle,
    format: String,
    title: String,
    text: String,
) -> AppResult<Option<String>> {
    use tauri_plugin_dialog::DialogExt;
    let (extension, label) = match format.as_str() {
        "docx" => ("docx", "Word document"),
        "xlsx" => ("xlsx", "Excel workbook"),
        _ => return Err(AppError::InvalidRequest("unknown file type".into())),
    };
    if text.len() > 2_000_000 {
        return Err(AppError::InvalidRequest("the report is too large to export".into()));
    }
    let bytes = if extension == "docx" {
        crate::export::to_docx(&title, &text)
    } else {
        crate::export::to_xlsx(&text)
    }
    .map_err(AppError::Internal)?;
    let name = format!("{}.{extension}", crate::export::file_stem(&title));
    // The save window opens in the Downloads folder, where people look for a file they just made.
    let folder = dirs::download_dir();
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        let dialog = app.dialog().file().add_filter(label, &[extension]).set_file_name(name);
        let dialog = match folder {
            Some(folder) => dialog.set_directory(folder),
            None => dialog,
        };
        dialog.blocking_save_file()
    })
    .await
    .map_err(|error| AppError::Internal(error.to_string()))?;
    let Some(chosen) = chosen else { return Ok(None) };
    let mut path = chosen.into_path().map_err(|error| AppError::Filesystem(error.to_string()))?;
    if path.extension().and_then(|e| e.to_str()) != Some(extension) {
        path.set_extension(extension);
    }
    fs::write(&path, bytes).map_err(|error| AppError::Filesystem(error.to_string()))?;
    Ok(Some(path.display().to_string()))
}

#[tauri::command]
pub async fn document_preview(
    state: State<'_, Arc<AppState>>,
    document_id: String,
) -> AppResult<DocumentPreview> {
    let document = state.db.document(&document_id)?.ok_or(AppError::NotFound)?;
    let stale = fs::read(&document.path)
        .map(|bytes| content_hash(&bytes) != document.content_hash)
        .unwrap_or(true);
    let blocks = state.db.blocks_for_document(&document_id)?;
    Ok(DocumentPreview {
        document,
        blocks,
        stale,
    })
}

#[tauri::command]
pub async fn list_entities(state: State<'_, Arc<AppState>>) -> AppResult<Vec<Entity>> {
    state.db.entities()
}

#[tauri::command]
pub async fn save_entity(
    state: State<'_, Arc<AppState>>,
    name: String,
    aliases: Vec<String>,
) -> AppResult<Entity> {
    let name = name.trim().to_string();
    if name.chars().count() < 2 {
        return Err(AppError::InvalidRequest(
            "Enter a client, person or matter name.".into(),
        ));
    }
    let mut aliases = aliases
        .into_iter()
        .map(|alias| alias.trim().to_string())
        .filter(|alias| alias.chars().count() >= 2 && !alias.eq_ignore_ascii_case(&name))
        .collect::<Vec<_>>();
    aliases.sort_by_key(|alias| alias.to_lowercase());
    aliases.dedup_by(|a, b| a.eq_ignore_ascii_case(b));
    let entity = Entity {
        id: make_id(name.to_lowercase().as_bytes()),
        name,
        aliases,
    };
    state.db.upsert_entity(&entity)?;
    Ok(entity)
}

/// Removes an approved folder and everything indexed from it. Files on disk are not touched.
#[tauri::command]
pub async fn remove_root(state: State<'_, Arc<AppState>>, path: String) -> AppResult<usize> {
    state.db.remove_root(&path)
}

#[tauri::command]
pub async fn remove_entity(state: State<'_, Arc<AppState>>, id: String) -> AppResult<()> {
    state.db.remove_entity(&id)
}

#[tauri::command]
pub async fn clear_local_data(state: State<'_, Arc<AppState>>) -> AppResult<()> {
    state.cancel();
    state.db.clear_local_index()
}

#[tauri::command]
pub async fn install_sample(app: AppHandle, state: State<'_, Arc<AppState>>) -> AppResult<String> {
    let root = dirs::data_local_dir()
        .ok_or_else(|| AppError::Filesystem("no local data directory is available".into()))?
        .join("PrivateAIConnectorWorkspace")
        .join("Sample Client");
    fs::create_dir_all(&root).map_err(|error| AppError::Filesystem(error.to_string()))?;
    write_sample_files(&root)?;
    state
        .inner()
        .clone()
        .index_root(app, root.to_string_lossy().to_string())
        .await
}

fn validate_claims(claims: &[crate::models::AnswerClaim], hits: &[SearchHit]) -> AppResult<()> {
    let allowed = hits
        .iter()
        .map(|hit| hit.citation_id.as_str())
        .collect::<HashSet<_>>();
    if claims.iter().any(|claim| {
        claim.text.trim().is_empty()
            || claim.citation_ids.is_empty()
            || claim
                .citation_ids
                .iter()
                .any(|id| !allowed.contains(id.as_str()))
    }) {
        return Err(AppError::Verification(
            "The answer contained an unsupported claim or citation.".into(),
        ));
    }
    Ok(())
}

fn write_sample_files(root: &Path) -> AppResult<()> {
    let files = [
        (
            "Firm Procedures.md",
            "# New client intake\n\nConfirm the engagement letter is signed before work begins. Store the final copy in the client-year folder.\n\n# Review\n\nA second reviewer confirms names, tax years, and filing deadlines before release.",
        ),
        (
            "Turnip Engagement.txt",
            "JOHN TURNIP ENGAGEMENT\n\nThe engagement covers preparation of the 2026 federal and Illinois individual income tax returns. Payment is due within thirty days of the invoice. The signed engagement date is October 21, 2026.",
        ),
        (
            "Missing Documents.csv",
            "Document,Status,Requested\nW-2 from Acme,Received,2026-10-12\nFirst Midwest 1099-INT,Missing,2026-10-20\nProperty tax statement,Received,2026-10-18\n",
        ),
    ];
    for (name, contents) in files {
        fs::write(root.join(name), contents)
            .map_err(|error| AppError::Filesystem(error.to_string()))?;
    }
    Ok(())
}

/// Development builds only: appends a line (counts, never text) to a file the developer can read.
#[cfg(debug_assertions)]
fn debug_note(line: &str) {
    use std::io::Write;
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../backend/.dev-state/answer-debug.log"
        ))
    {
        let _ = writeln!(file, "{} {line}", Utc::now().format("%H:%M:%S"));
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_workspace_answer_keeps_only_pinned_cloud_documents() {
        let pinned = vec!["123e4567-e89b-12d3-a456-426614174000".to_string()];
        let hits = vec![
            file_hit("cloud:123e4567-e89b-12d3-a456-426614174000", "Pinned.docx", Origin::Cloud, "a"),
            file_hit("cloud:aaaaaaaa-e89b-12d3-a456-426614174000", "Other.docx", Origin::Cloud, "b"),
            file_hit("123e4567-e89b-12d3-a456-426614174000", "Looks the same but is local", Origin::Local, "c"),
        ];
        let kept = only_pinned(hits, &pinned);
        assert_eq!(kept.iter().map(|hit| hit.document_name.as_str()).collect::<Vec<_>>(), ["Pinned.docx"]);
        assert!(only_pinned(kept, &[]).is_empty());
    }

    use super::*;
    use crate::{
        cloud::testing::{serve, signed_in_cloud},
        db::Database,
        models::{DocumentResource, ParseStatus, ParsedBlock, SourceLocation},
    };

    #[test]
    fn a_whole_document_is_sent_only_when_the_question_names_it() {
        assert!(question_names_document(
            "Summarize the Turnip engagement letter",
            "Turnip Engagement.txt"
        ));
        assert!(!question_names_document(
            "tell me about mens gender breakout",
            "README-es.md"
        ));
        // One shared word is not naming the file: this exact mistake sent the wrong document.
        assert!(!question_names_document(
            "tell me about mens retreat",
            "Retreat 26 Small group leader guide.docx"
        ));
        // Half of a longer name is.
        assert!(question_names_document(
            "summarize the small group leader guide",
            "Retreat 26 Small group leader guide.docx"
        ));
        // Sharing most of a short name is talking about the same topic, not naming the file.
        assert!(!question_names_document(
            "what did the men's retreat cost us",
            "Men’s Retreat Planning"
        ));
        assert!(!question_names_document(
            "what did the men's retreat cost us",
            "Men's Retreat Budget.xlsx"
        ));
        assert!(question_names_document(
            "what is in the men's retreat budget",
            "Men's Retreat Budget.xlsx"
        ));
        // A two-word name needs both words.
        assert!(!question_names_document(
            "summarize the engagement terms",
            "Turnip Engagement.txt"
        ));
        // The extension is not part of the name.
        assert!(!question_names_document("what is in the txt", "Notes.txt"));
    }

    fn usage_reply(used: i64, limit: i64) -> String {
        format!(
            r#"{{"used_micros":{used},"limit_micros":{limit},"remaining_micros":{},"answers":3,"period_end_epoch":1796083200.0,"limit_is_default":true}}"#,
            (limit - used).max(0)
        )
    }

    #[tokio::test]
    async fn a_question_is_refused_once_the_months_allowance_is_used_up() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = serve(|request| match request.path.as_str() {
            "/v1/usage" => (200, usage_reply(15_000_000, 15_000_000), 0),
            _ => (404, "{}".into(), 0),
        })
        .await;
        let cloud = signed_in_cloud(dir.path(), &api);
        let refused = ensure_allowance(&cloud).await.unwrap_err().to_string();
        assert!(refused.contains("$15.00"), "{refused}");
        assert!(refused.contains("December 1"), "{refused}");
        assert!(refused.contains("Search still works"), "{refused}");
    }

    #[tokio::test]
    async fn a_question_goes_ahead_while_there_is_allowance_or_the_server_cannot_be_asked() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = serve(|_| (200, usage_reply(1_000_000, 15_000_000), 0)).await;
        let cloud = signed_in_cloud(dir.path(), &api);
        assert!(ensure_allowance(&cloud).await.is_ok());

        // A server that errors, a person who is signed out, and a device with no cloud at all
        // are all treated as "no limit to enforce here".
        let (broken, _) = serve(|_| (500, "{}".into(), 0)).await;
        let down = signed_in_cloud(dir.path(), &broken);
        assert!(ensure_allowance(&down).await.is_ok());
        let nothing_set_up = tempfile::tempdir().unwrap();
        let none = crate::cloud::Cloud::load(nothing_set_up.path());
        assert!(ensure_allowance(&none).await.is_ok());
    }

    #[test]
    fn ranked_passages_come_first_and_the_rest_keep_their_order() {
        let hits: Vec<SearchHit> = ["a", "b", "c", "d", "e"]
            .iter()
            .map(|name| cloud_hit(HANDBOOK_ID, name))
            .collect();
        let names = |hits: &[SearchHit]| -> Vec<String> {
            hits.iter().map(|hit| hit.document_name.clone()).collect()
        };
        assert_eq!(
            names(&apply_ranking(hits.clone(), &[3, 1])),
            ["d", "b", "a", "c", "e"]
        );
        // No ranking, an empty one, or numbers out of range leave the order alone.
        assert_eq!(
            names(&apply_ranking(hits.clone(), &[])),
            ["a", "b", "c", "d", "e"]
        );
        assert_eq!(
            names(&apply_ranking(hits.clone(), &[9, 2, 2])),
            ["c", "a", "b", "d", "e"]
        );
        // Nothing is lost or duplicated.
        assert_eq!(apply_ranking(hits, &[4, 4, 0]).len(), 5);
    }

    #[test]
    fn money_is_shown_in_dollars_and_cents() {
        assert_eq!(dollars(14_000), "$0.01");
        assert_eq!(dollars(15_000_000), "$15.00");
        assert_eq!(dollars(0), "$0.00");
    }

    const CLOUD_REPLY: &str = r#"{"results":[{"citation_id":"c1","document_id":"d1","document_name":"Handbook.docx","text":"Firm handbook: the retainer must be paid before work begins.","score":0.4}]}"#;

    fn state_with_local_file(cloud: crate::cloud::Cloud) -> AppState {
        let db = Database::open_memory().unwrap();
        let document = DocumentResource {
            id: "local-1".into(),
            name: "Retainer.txt".into(),
            path: "/Clients/Retainer.txt".into(),
            mime_type: "text/plain".into(),
            size: 1,
            modified_at: 1,
            content_hash: "h".into(),
            status: ParseStatus::Ready,
            warning: None,
            chunk_count: 1,
        };
        let block = ParsedBlock {
            text: "The retainer is due within thirty days.".into(),
            location: SourceLocation::default(),
            ocr: false,
            ocr_confidence: None,
        };
        db.upsert_document(&document, &[block]).unwrap();
        AppState::new(db, cloud)
    }

    fn cloud_hit(id: &str, name: &str) -> SearchHit {
        SearchHit {
            citation_id: format!("cloud:{id}:0"),
            document_id: format!("cloud:{id}"),
            document_name: name.into(),
            path: "Firm cloud index".into(),
            excerpt: "a snippet".into(),
            location: SourceLocation::default(),
            location_label: "Cloud".into(),
            score: 1.0,
            ocr: false,
            stale: false,
            origin: Origin::Cloud,
            link: None,
        }
    }

    const HANDBOOK_ID: &str = "6a419cce-0ea4-429c-b13e-0dd800d11b41";

    #[tokio::test]
    async fn a_cloud_document_the_question_names_is_read_in_full_from_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|request| {
            if request.path.starts_with("/v1/documents/") {
                let body = r#"{"name":"Firm Handbook.docx","total_chars":90,"too_long":false,"passages":[{"label":"Page 1","text":"Section 1. Retainers are paid up front."},{"label":"Page 2","text":"Section 2. Late fees start after thirty days."}]}"#;
                (200, body.into(), 0)
            } else {
                (404, "{}".into(), 0)
            }
        })
        .await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));
        let hits = [cloud_hit(HANDBOOK_ID, "Firm Handbook.docx")];

        let whole = read_whole_document(
            &state,
            "what do late fees say in the firm handbook",
            &hits,
            false,
        )
        .await
        .unwrap();
        let Whole::Read { name, blocks } = whole else {
            panic!("the named document should have been read in full");
        };
        assert_eq!(name, "Firm Handbook.docx");
        assert_eq!(blocks.len(), 2);
        assert!(blocks[1].text.contains("Late fees"));
        assert_eq!(blocks[1].location.label(), "Page 2");
        // The everyday model asks for up to 380,000 characters; the advanced model for more.
        let advanced =
            read_whole_document(&state, "summarize the firm handbook", &hits, true).await;
        assert!(advanced.is_ok());
        let log = log.lock().unwrap();
        assert_eq!(
            log[0],
            format!("GET /v1/documents/{HANDBOOK_ID}?max_chars=380000")
        );
        assert_eq!(
            log[1],
            format!("GET /v1/documents/{HANDBOOK_ID}?max_chars=1500000")
        );
    }

    #[tokio::test]
    async fn the_top_short_documents_are_sent_whole_so_a_total_is_not_missed() {
        let dir = tempfile::tempdir().unwrap();
        let (short, long, third) = (
            "6a419cce-0ea4-429c-b13e-0dd800d11b41",
            "7b419cce-0ea4-429c-b13e-0dd800d11b42",
            "8c419cce-0ea4-429c-b13e-0dd800d11b43",
        );
        let (api, log) = serve(move |request| {
            if request.path.contains(short) {
                let body = r#"{"name":"Budget","total_chars":40,"too_long":false,"passages":[{"label":null,"text":"Food 500"},{"label":null,"text":"Total Expenses 5,670.77"}]}"#;
                (200, body.into(), 0)
            } else {
                (200, r#"{"name":"Long","total_chars":90000,"too_long":true,"passages":[]}"#.into(), 0)
            }
        })
        .await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));
        let hits = vec![
            cloud_hit(short, "Budget"),
            cloud_hit(long, "Long"),
            cloud_hit(short, "Budget"),
            cloud_hit(third, "Third"),
        ];
        let result = expand_short_documents(&state, hits).await;
        let names: Vec<&str> = result
            .iter()
            .map(|hit| hit.document_name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Budget", "Long", "Third"],
            "the second Budget passage is folded in"
        );
        assert!(result[0].excerpt.contains("Total Expenses 5,670.77"));
        assert_eq!(result[0].location_label, "Cloud · Whole file");
        assert_eq!(
            result[1].excerpt, "a snippet",
            "a long document keeps its passage"
        );
        // Only the top two documents are fetched.
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(!log.iter().any(|line| line.contains(third)));
    }

    #[tokio::test]
    async fn a_document_the_question_does_not_name_is_not_fetched() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|_| (200, "{}".into(), 0)).await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));
        let hits = [cloud_hit(HANDBOOK_ID, "Firm Handbook.docx")];
        let whole = read_whole_document(&state, "when is payment due", &hits, false)
            .await
            .unwrap();
        assert!(matches!(whole, Whole::No));
        assert!(
            log.lock().unwrap().is_empty(),
            "nothing was requested from the server"
        );
    }

    #[tokio::test]
    async fn a_document_that_is_too_long_or_cannot_be_fetched_falls_back_to_passages() {
        let dir = tempfile::tempdir().unwrap();
        let hits = [cloud_hit(HANDBOOK_ID, "Firm Handbook.docx")];
        let (long, _) = serve(|_| (200, r#"{"name":"Firm Handbook.docx","total_chars":900000,"too_long":true,"passages":[]}"#.into(), 0)).await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &long));
        let whole = read_whole_document(&state, "summarize the firm handbook", &hits, false)
            .await
            .unwrap();
        assert!(matches!(whole, Whole::TooLong { ref name } if name == "Firm Handbook.docx"));

        for status in [404u16, 500] {
            let (broken, _) = serve(move |_| (status, "{}".into(), 0)).await;
            let state = state_with_local_file(signed_in_cloud(dir.path(), &broken));
            let whole = read_whole_document(&state, "summarize the firm handbook", &hits, false)
                .await
                .unwrap();
            assert!(matches!(whole, Whole::No), "status {status}");
        }
    }

    #[tokio::test]
    async fn a_local_file_the_question_names_is_read_in_full_from_this_device() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = serve(|_| (404, "{}".into(), 0)).await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));
        let local = SearchHit {
            origin: Origin::Local,
            document_id: "local-1".into(),
            document_name: "Retainer.txt".into(),
            ..cloud_hit(HANDBOOK_ID, "Retainer.txt")
        };
        let whole = read_whole_document(&state, "what does the retainer file say", &[local], false)
            .await
            .unwrap();
        assert!(
            matches!(whole, Whole::Read { ref blocks, .. } if blocks[0].text.contains("thirty days"))
        );
    }

    #[tokio::test]
    async fn a_question_searches_this_device_and_the_cloud_and_merges_the_results() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|_| (200, CLOUD_REPLY.into(), 0)).await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));

        let found = retrieve(&state, "retainer due", 8, Scope::All).await.unwrap();
        assert_eq!((found.local, found.cloud), (1, 1));
        assert!(found.cloud_question_sent);
        assert_eq!(found.cloud_status.state, CloudState::Ready);
        let origins: Vec<_> = found
            .hits
            .iter()
            .map(|hit| (hit.document_name.as_str(), hit.origin))
            .collect();
        assert!(origins.contains(&("Retainer.txt", Origin::Local)));
        assert!(origins.contains(&("Handbook.docx", Origin::Cloud)));
        assert_eq!(
            log.lock().unwrap().len(),
            1,
            "one cloud request per question"
        );
    }

    #[tokio::test]
    async fn when_the_cloud_is_down_the_answer_still_uses_local_files_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        let state = state_with_local_file(signed_in_cloud(dir.path(), "http://127.0.0.1:9"));
        let found = retrieve(&state, "retainer due", 8, Scope::All).await.unwrap();
        assert_eq!((found.local, found.cloud), (1, 0));
        assert_eq!(found.cloud_status.state, CloudState::Unavailable);
        assert!(
            found
                .cloud_status
                .message
                .contains("Showing local results only"),
            "{}",
            found.cloud_status.message
        );
        assert!(!found.cloud_question_sent, "the connection never opened");

        // A failing server is handled the same way.
        let (api, _) = serve(|_| (500, "boom".into(), 0)).await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));
        let found = retrieve(&state, "retainer due", 8, Scope::All).await.unwrap();
        assert_eq!(
            (found.local, found.cloud, found.cloud_status.state),
            (1, 0, CloudState::Unavailable)
        );
        assert!(found.cloud_question_sent);
    }

    #[tokio::test]
    async fn switching_the_cloud_off_sends_nothing_and_raises_no_warning() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|_| (200, CLOUD_REPLY.into(), 0)).await;
        let cloud = signed_in_cloud(dir.path(), &api);
        cloud.set_enabled(false).unwrap();
        let state = state_with_local_file(cloud);
        let found = retrieve(&state, "retainer due", 8, Scope::All).await.unwrap();
        assert_eq!(
            (found.local, found.cloud, found.cloud_question_sent),
            (1, 0, false)
        );
        assert_eq!(found.cloud_status.state, CloudState::Off);
        assert!(log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_cloud_with_no_matches_changes_nothing_about_local_results() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = serve(|_| (200, r#"{"results":[]}"#.into(), 0)).await;
        let state = state_with_local_file(signed_in_cloud(dir.path(), &api));
        let found = retrieve(&state, "retainer due", 8, Scope::All).await.unwrap();
        assert_eq!((found.local, found.cloud), (1, 0));
        assert_eq!(found.cloud_status.state, CloudState::Ready);
    }
}

// ---- Shared agents: the server runs them; the interface lists, configures and starts them ----

/// Passes one request to the agents endpoints. Only those paths are allowed, and only plain ones.
#[tauri::command]
pub async fn cloud_agents_call(
    state: State<'_, Arc<AppState>>,
    method: String,
    path: String,
    query: Option<Vec<(String, String)>>,
    body: Option<serde_json::Value>,
) -> AppResult<serde_json::Value> {
    let allowed_path = (path == "/v1/agents"
        || path == "/v1/tasks"
        || path == "/v1/schedules"
        || path == "/v1/inbox"
        || path == "/v1/actions"
        || path.starts_with("/v1/actions/")
        || path.starts_with("/v1/agents/")
        || path.starts_with("/v1/tasks/")
        || path.starts_with("/v1/schedules/"))
        && path.len() < 200
        && path.chars().all(|c| c.is_ascii_alphanumeric() || c == '/' || c == '-');
    let method = match method.as_str() {
        "GET" => reqwest::Method::GET,
        "POST" => reqwest::Method::POST,
        "PUT" => reqwest::Method::PUT,
        _ => return Err(AppError::Cloud("that request is not allowed".into())),
    };
    if !allowed_path {
        return Err(AppError::Cloud("that request is not allowed".into()));
    }
    let query = query.unwrap_or_default();
    let pairs: Vec<(&str, &str)> = query.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    knowledge_call(&state, method, &path, &pairs, body).await
}

/// Saves a file into the person's own Google Drive through the firm cloud service. The page calls
/// this only from its approval box, after the person has read the exact text; the service refuses
/// anything not marked approved, and only saves into the signed-in person's own Drive.
#[tauri::command]
pub async fn cloud_drive_save(state: State<'_, Arc<AppState>>, title: String, content: String) -> AppResult<serde_json::Value> {
    knowledge_call(
        &state,
        reqwest::Method::POST,
        "/v1/drive/save",
        &[],
        Some(serde_json::json!({ "title": title, "content": content, "approved": true })),
    )
    .await
}

/// Search of the firm's connected sources with filters, returning one row per file and the counts
/// shown beside the results.
#[tauri::command]
pub async fn cloud_search_browse(
    state: State<'_, Arc<AppState>>,
    query: String,
    filters: serde_json::Value,
) -> AppResult<serde_json::Value> {
    knowledge_call(
        &state,
        reqwest::Method::POST,
        "/v1/search/browse",
        &[],
        Some(serde_json::json!({ "query": query, "filters": filters })),
    )
    .await
}

#[cfg(test)]
mod chat_tests {
    use super::*;

    fn turn(q: &str, a: &str) -> ChatTurn {
        ChatTurn { question: q.into(), answer: a.into(), web: false }
    }

    #[test]
    fn a_first_question_is_sent_as_it_is() {
        assert_eq!(question_with_context("What is the retreat budget?", &[]), "What is the retreat budget?");
        assert_eq!(search_text("What is the retreat budget?", &[]), "What is the retreat budget?");
    }

    #[test]
    fn a_follow_up_carries_the_last_exchanges_and_is_searched_with_the_question_before_it() {
        let earlier = [turn("Tell me about the mens retreat", "It is in spring.")];
        let text = question_with_context("and the cost?", &earlier);
        assert!(text.contains("Q: Tell me about the mens retreat") && text.ends_with("New question: and the cost?"));
        assert_eq!(search_text("and the cost?", &earlier), "Tell me about the mens retreat and the cost?");
        // A long, self-contained question is searched on its own.
        assert_eq!(
            search_text("What are the payment terms in the Whitfield engagement letter exactly?", &earlier),
            "What are the payment terms in the Whitfield engagement letter exactly?"
        );
    }

    #[test]
    fn only_the_last_three_exchanges_are_kept() {
        let earlier: Vec<ChatTurn> = (1..=5).map(|n| turn(&format!("question {n}"), "answer")).collect();
        let text = question_with_context("next", &earlier);
        assert!(!text.contains("question 2") && text.contains("question 3") && text.contains("question 5"));
    }
}

#[cfg(test)]
mod chosen_files {
    use super::*;

    fn block(text: &str) -> ParsedBlock {
        ParsedBlock { text: text.into(), location: Default::default(), ocr: false, ocr_confidence: None }
    }

    #[test]
    fn the_work_says_where_it_searched_what_it_found_and_whether_the_question_left_the_device() {
        let all = search_steps(" payment terms ", None, Scope::All, &Default::default(), 2, 1, true);
        assert_eq!(all[0], "Searched on this device and in the firm cloud for “payment terms”.");
        assert!(all[1].starts_with("Found 2 passage(s) on this device and 1 in the firm cloud.") && all[1].contains("question text was sent"));
        let local = search_steps("x", None, Scope::Local, &Default::default(), 3, 0, false);
        assert!(local[0].contains("on this device only") && !local[1].contains("sent to the firm cloud"));
        let limited = search_steps("q", None, Scope::Cloud, &crate::cloud::SearchFilter::checked(Some(vec!["google_drive".into()]), Some(vec!["presentation".into()])).unwrap(), 0, 4, true);
        assert_eq!(limited[0], "Searched in the firm cloud only, limited to Google Drive, presentations, for “q”.");
        let report = search_steps("x", Some("Q3"), Scope::All, &Default::default(), 0, 0, false);
        assert_eq!(report.len(), 1);
        assert!(report[0].contains("“Q3”"));
    }

    #[test]
    fn web_research_in_the_conversation_is_handed_to_the_documents_answer_with_its_marker_and_kept_longer() {
        let research = ChatTurn { question: "find mens retreat bible verse".into(), answer: format!("Proverbs 27:17 {}", "x".repeat(2_000)), web: true };
        let text = question_with_context("how can I change my retreat plan to follow these verses", &[research]);
        assert!(text.starts_with(crate::provider::RESEARCH_MARKER));
        assert!(text.contains("Web research Q: find mens retreat bible verse") && text.contains("Proverbs 27:17"));
        assert!(text.chars().filter(|c| *c == 'x').count() == 2_000, "research answers are kept at length");
        assert!(text.ends_with("New question: how can I change my retreat plan to follow these verses"));
        // Without research, the context is unchanged: short, and only for reading the new question.
        let plain = question_with_context("and the deposit?", &[ChatTurn { question: "q".into(), answer: "a".repeat(2_000), web: false }]);
        assert!(!plain.contains(crate::provider::RESEARCH_MARKER));
        assert!(plain.chars().filter(|c| *c == 'a').count() <= 600 + 20);
    }

    #[test]
    fn the_web_tab_only_sees_earlier_answers_from_the_web_tab() {
        let earlier = [
            ChatTurn { question: "what is the retainer".into(), answer: "Secret client figure 5,000".into(), web: false },
            ChatTurn { question: "find mens retreat bible verse".into(), answer: "Proverbs 27:17".into(), web: true },
        ];
        let message = web_message("explain that verse", &earlier, &[]);
        assert!(message.contains("Proverbs 27:17") && message.ends_with("Question: explain that verse"));
        assert!(!message.contains("Secret client figure"), "answers drawn from the person's documents never go to the web tab");
        assert_eq!(web_message("hello", &[], &[]), "Question: hello");
        // A file the person attached goes ahead of the question, and only that.
        let with_file = web_message("what is this", &[], &[("Budget \"final\".txt".into(), "Total 5,000".into())]);
        assert!(with_file.starts_with("<attached_file name=\"Budget 'final'.txt\">\nTotal 5,000\n</attached_file>"));
        assert!(with_file.ends_with("Question: what is this"));
    }

    #[test]
    fn the_scope_is_what_the_person_chose_and_anything_else_means_everything() {
        assert_eq!(Scope::from_text(Some("local")), Scope::Local);
        assert_eq!(Scope::from_text(Some("cloud")), Scope::Cloud);
        assert_eq!(Scope::from_text(Some("everything")), Scope::All);
        assert_eq!(Scope::from_text(Some("")), Scope::All);
        assert_eq!(Scope::from_text(None), Scope::All);
    }

    #[test]
    fn an_attached_file_becomes_one_source_per_block_that_cannot_be_mistaken_for_an_indexed_file() {
        let hits = attachment_hits("a1", "Budget.xlsx", &[block("first"), block("second")]);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].document_id, "attachment:a1");
        assert_eq!(hits[0].citation_id, "attachment:a1:0");
        assert_eq!(hits[1].citation_id, "attachment:a1:1");
        assert!(hits.iter().all(|hit| hit.origin == Origin::Local && hit.document_name == "Budget.xlsx"));
        // Citation IDs are unique, so an answer can point at one block.
        assert_ne!(hits[0].citation_id, hits[1].citation_id);
    }

    #[test]
    fn a_tagged_file_keeps_its_own_id_and_origin_and_a_short_excerpt() {
        let long = "x".repeat(5_000);
        let hit = file_hit("cloud:abc", "Lease.docx", Origin::Cloud, &long);
        assert_eq!(hit.document_id, "cloud:abc");
        assert_eq!(hit.origin, Origin::Cloud);
        assert_eq!(hit.excerpt.chars().count(), 1_200);
        assert_eq!(hit.citation_id, "file:cloud:abc");
        assert_eq!(file_hit("d1", "Notes.md", Origin::Local, "").path, "Chosen file");
    }
}
