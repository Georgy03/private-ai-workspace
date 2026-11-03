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
async fn retrieve(state: &AppState, query: &str, limit: usize) -> AppResult<Retrieved> {
    let ((cloud_result, cloud_question_sent), local_result) =
        tokio::join!(state.cloud.search_tracked(query, limit), async {
            crate::entities::search(&state.db, query, limit)
        });
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
    Ok(retrieve(&state, query.trim(), 24).await?.hits)
}

#[tauri::command]
pub async fn answer_question(
    state: State<'_, Arc<AppState>>,
    question: String,
    advanced: Option<bool>,
) -> AppResult<AnswerResponse> {
    if question.trim().len() < 3 {
        return Err(AppError::InvalidRequest(
            "Enter a complete question.".into(),
        ));
    }
    let Retrieved {
        hits,
        local,
        cloud,
        cloud_status,
        cloud_question_sent,
    } = retrieve(&state, question.trim(), RERANK_CANDIDATES).await?;
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
        });
    }

    // The allowance is checked once, before anything is sent to Claude, including the ranking pass.
    ensure_allowance(&state.cloud).await?;
    let advanced = advanced.unwrap_or(false);

    // A question that names a document is answered from all of it, not from a few passages, so a
    // detailed question about one file is not missed because the answer sat in a part of it that
    // did not rank. This is decided over everything that was found, before any passages are cut,
    // and applies to local files and to files in the firm cloud index.
    let found = hits;
    let whole_document = read_whole_document(&state, question.trim(), &found, advanced).await?;
    let named = match &whole_document {
        Whole::Read { name, .. } | Whole::TooLong { name } => Some(name.clone()),
        Whole::No => None,
    };
    let mut hits = found.clone();
    if matches!(whole_document, Whole::Read { .. }) {
        // The whole file is what Claude reads, so there is nothing to rank. The other passages
        // are only kept as the list of sources.
        hits.truncate(ANSWER_PASSAGES);
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
    let mut result = provider
        .answer(question.trim(), &hits, whole, advanced)
        .await?;
    count_usage(&state.cloud, &result).await;
    if result.claims.is_empty() && whole.is_some() {
        // The whole file did not answer it, so the question is probably about something else.
        // Try again with the best passages from every source instead.
        hits = best_passages(provider.as_ref(), &state.cloud, question.trim(), found).await;
        hits = expand_short_documents(&state, hits).await;
        result = provider
            .answer(question.trim(), &hits, None, advanced)
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
    let mut summary = if cloud > 0 {
        format!(
            "{} {cloud} of the passages came from the firm cloud index.",
            result.summary
        )
    } else {
        result.summary
    };
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
    })
}

#[tauri::command]
pub async fn cloud_status(state: State<'_, Arc<AppState>>) -> AppResult<CloudStatus> {
    Ok(state.cloud.status())
}

#[tauri::command]
pub async fn cloud_sign_in(state: State<'_, Arc<AppState>>) -> AppResult<CloudStatus> {
    state
        .cloud
        .sign_in(crate::cloud::open_browser)
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
) -> AppResult<()> {
    let url = state
        .cloud
        .start_connection(&kind, &domains, reconnect.as_deref())
        .await
        .map_err(|error| AppError::Cloud(error.message()))?;
    crate::cloud::open_browser(&url);
    Ok(())
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

        let found = retrieve(&state, "retainer due", 8).await.unwrap();
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
        let found = retrieve(&state, "retainer due", 8).await.unwrap();
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
        let found = retrieve(&state, "retainer due", 8).await.unwrap();
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
        let found = retrieve(&state, "retainer due", 8).await.unwrap();
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
        let found = retrieve(&state, "retainer due", 8).await.unwrap();
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
    let allowed_path = (path == "/v1/agents" || path == "/v1/tasks" || path.starts_with("/v1/agents/") || path.starts_with("/v1/tasks/"))
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
