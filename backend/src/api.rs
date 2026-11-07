//! The HTTP API the desktop app calls. Every route except the health checks needs a valid ID
//! token, and the firm is read from that token only.

use std::{sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRequestParts, Path, Query, State},
    http::{HeaderName, HeaderValue, StatusCode, header, request::Parts},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tower::limit::ConcurrencyLimitLayer;
use tower_http::{set_header::SetResponseHeaderLayer, timeout::TimeoutLayer};
use uuid::Uuid;

use crate::{
    Error,
    auth::{Principal, TokenVerifier},
    db::TenantTx,
    jwks::JwksSource,
    oauth::{self, Kind, OAuthSettings},
    queue,
    secrets::{SecretStore, refresh_token_name},
};

const MAX_BODY_BYTES: usize = 16 * 1024;
const MAX_QUERY_CHARS: usize = 2_000;
const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 50;
const MAX_PASSAGE_CHARS: usize = 6_000;
const MAX_IN_FLIGHT: usize = 64;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

#[derive(Clone)]
pub struct AppState {
    pub verifier: Arc<TokenVerifier>,
    pub jwks: Option<Arc<JwksSource>>,
    pub pool: Pool,
    pub oauth: Arc<OAuthSettings>,
    pub secrets: Arc<dyn SecretStore>,
    /// Turns a question into a meaning vector. Without one, search matches words only.
    pub embedder: Option<Arc<dyn crate::embed::Embedder>>,
}

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/v1/me", get(me))
        .route("/v1/search", post(search).layer(DefaultBodyLimit::max(32 * 1024)))
        .route("/v1/connectors", get(list_connectors))
        .route("/v1/connections", get(list_connections).post(connect))
        .route("/v1/connections/{id}", axum::routing::delete(disconnect))
        .route("/v1/connections/{id}/sync", post(sync_now))
        .route("/v1/connections/{id}/files", get(connection_files))
        .route("/v1/documents/{id}", get(document))
        .route("/v1/usage", get(usage).post(record_usage))
        .route("/v1/usage/limit", axum::routing::put(set_usage_limit))
        .route("/v1/usage/team", get(usage_team))
        .merge(crate::knowledge_api::routes())
        .route("/v1/connections/{id}/scope", axum::routing::put(set_scope))
        .merge(crate::agents_api::routes())
        .merge(crate::schedules_api::routes())
        .merge(crate::browse::routes())
        .route("/oauth/callback", get(oauth_callback))
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .layer(ConcurrencyLimitLayer::new(MAX_IN_FLIGHT))
        .layer(TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            REQUEST_TIMEOUT,
        ))
        .layer(SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
        .layer(SetResponseHeaderLayer::overriding(
            HeaderName::from_static("x-content-type-options"),
            HeaderValue::from_static("nosniff"),
        ))
        .with_state(state)
}

pub enum ApiError {
    Unauthorized,
    Forbidden,
    NotFound,
    BadRequest(&'static str),
    /// A feature that this server has not been set up for.
    Unavailable(&'static str),
    Internal,
}

impl ApiError {
    pub(crate) fn internal(error: impl std::fmt::Display) -> Self {
        // Log the cause on the server; the caller only learns that something failed.
        tracing::error!(%error, "request failed");
        ApiError::Internal
    }
}

impl From<Error> for ApiError {
    fn from(error: Error) -> Self {
        ApiError::internal(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized"),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden"),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not found"),
            ApiError::Unavailable(message) => (StatusCode::SERVICE_UNAVAILABLE, message),
            ApiError::BadRequest(message) => (StatusCode::BAD_REQUEST, message),
            ApiError::Internal => (StatusCode::INTERNAL_SERVER_ERROR, "internal error"),
        };
        (status, Json(json!({ "error": message }))).into_response()
    }
}

/// The signed-in person, taken from the bearer token. Nothing else in the request can name a firm.
pub struct Authed(pub Principal);

impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let token = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .ok_or(ApiError::Unauthorized)?;
        if let (Some(kid), Some(jwks)) = (TokenVerifier::key_id(token), &state.jwks)
            && !state.verifier.has_key(&kid)
        {
            jwks.refresh(&state.verifier).await;
        }
        state
            .verifier
            .verify(token)
            .map(Authed)
            .map_err(|_| ApiError::Unauthorized)
    }
}

/// Opens a transaction for the principal's firm and refuses firms that are gone or suspended,
/// including tokens issued before a firm was offboarded.
pub(crate) async fn open<'a>(
    conn: &'a mut deadpool_postgres::Object,
    principal: &Principal,
) -> Result<TenantTx<'a>, ApiError> {
    let tx = TenantTx::begin(conn, principal.tenant).await?;
    match tx.status().await?.as_deref() {
        Some("active") => Ok(tx),
        _ => Err(ApiError::Forbidden),
    }
}

#[derive(Serialize)]
struct Me {
    tenant: Uuid,
    user: String,
    groups: Vec<String>,
}

async fn me(Authed(principal): Authed) -> Json<Me> {
    Json(Me {
        tenant: principal.tenant.0,
        user: principal.user,
        groups: principal.groups,
    })
}

#[derive(Deserialize)]
struct SearchRequest {
    query: String,
    limit: Option<i64>,
    /// Set to false to match words only, for comparing the two kinds of search. Default true.
    meaning: Option<bool>,
    /// Search only inside the documents one of the caller's own reports was drawn from, for
    /// asking questions about that report. Someone else's report is not found.
    task: Option<Uuid>,
    /// Only files from these connected sources ("google_drive", "microsoft365", ...). Empty means all.
    #[serde(default)]
    sources: Vec<String>,
    /// Only these kinds of file ("document", "spreadsheet", "presentation", "pdf", "image", "other").
    /// Empty means all. Google Docs, Sheets and Slides are documents, spreadsheets and presentations.
    #[serde(default)]
    types: Vec<String>,
    /// Only inside these documents (the ones pinned to a workspace). Documents the caller may not
    /// open are never found, whatever is listed here.
    #[serde(default)]
    documents: Vec<Uuid>,
}

#[derive(Serialize)]
struct Passage {
    /// Where in the file the passage sits ("Page 3", "Slide 2"), when known.
    location_label: Option<String>,
    citation_id: Uuid,
    document_id: Uuid,
    document_name: String,
    text: String,
    score: f32,
    /// Opens the original file in Google Drive or SharePoint, when known.
    link: Option<String>,
}

#[derive(Serialize)]
struct SearchResponse {
    results: Vec<Passage>,
}

async fn search(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<SearchRequest>,
) -> Result<Json<SearchResponse>, ApiError> {
    let query = request.query.trim();
    let query_chars = query.chars().count();
    if query_chars == 0 || query_chars > MAX_QUERY_CHARS {
        return Err(ApiError::BadRequest("query must be 1 to 2000 characters"));
    }
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    if request.sources.len() > 20 || request.types.len() > crate::browse::FILE_TYPES.len() || request.documents.len() > 500 {
        return Err(ApiError::BadRequest("too many filters"));
    }
    if request.sources.iter().any(|source| source.len() > 40 || !source.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_'))
        || request.types.iter().any(|kind| !crate::browse::FILE_TYPES.contains(&kind.as_str()))
    {
        return Err(ApiError::BadRequest("unknown filter"));
    }
    let filtering = !request.sources.is_empty() || !request.types.is_empty();

    // The question's meaning vector. If the model cannot be used, the search falls back to words.
    let vector = match state
        .embedder
        .as_ref()
        .filter(|_| request.meaning != Some(false))
    {
        Some(embedder) => match embedder.embed_query(query).await {
            Ok(vector) => Some((embedder.model().to_string(), vector)),
            Err(error) => {
                tracing::warn!(%error, "searching by words only");
                None
            }
        },
        None => None,
    };
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let mut within: Option<Vec<Uuid>> = match request.task {
        None => None,
        Some(task) => {
            let task = crate::agents::get_task(&tx, &principal, task).await?.ok_or(ApiError::NotFound)?;
            let sources = task.sources.ok_or(ApiError::NotFound)?;
            Some(
                sources
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|source| source["document_id"].as_str().and_then(|id| Uuid::parse_str(id).ok()))
                    .collect(),
            )
        }
    };
    if !request.documents.is_empty() {
        // Inside a report's files and inside the pinned documents: only what is in both.
        within = Some(match within {
            Some(report) => report.into_iter().filter(|id| request.documents.contains(id)).collect(),
            None => request.documents.clone(),
        });
    }
    let mut hits = tx
        .search_hybrid(
            &principal,
            query,
            vector
                .as_ref()
                .map(|(model, vector)| (model.as_str(), vector.as_slice())),
            if within.is_some() || filtering { MAX_LIMIT * 4 } else { limit },
        )
        .await?;
    if filtering {
        // Which connected source and what kind of file each hit is, for hits the caller may open.
        let ids: Vec<Uuid> = hits.iter().map(|hit| hit.document_id).collect();
        let meta: std::collections::HashMap<Uuid, (String, String, String)> = tx
            .sql()
            .query(
                "SELECT d.id, cn.kind, d.mime_type, d.name
                 FROM documents d JOIN connections cn ON cn.id = d.connection_id
                 WHERE d.id = ANY($1) AND d.readable_by && $2::text[]",
                &[&ids, &principal.readers()],
            )
            .await
            .map_err(ApiError::internal)?
            .into_iter()
            .map(|row| (row.get(0), (row.get(1), row.get(2), row.get(3))))
            .collect();
        hits.retain(|hit| {
            meta.get(&hit.document_id).is_some_and(|(source, mime, name)| {
                (request.sources.is_empty() || request.sources.contains(source))
                    && (request.types.is_empty() || request.types.iter().any(|kind| kind == crate::browse::file_type(mime, name)))
            })
        });
        hits.truncate(limit as usize);
    }
    if let Some(documents) = &within {
        hits.retain(|hit| documents.contains(&hit.document_id));
        hits.truncate(limit as usize);
        // A vague question may match nothing in particular: then the start of each file the
        // report used is the best place to look.
        if hits.len() < 3 && !documents.is_empty() {
            let known: Vec<Uuid> = hits.iter().map(|hit| hit.chunk_id).collect();
            for hit in tx.opening_passages(&principal, documents, 2, limit).await? {
                if hits.len() >= limit as usize {
                    break;
                }
                if !known.contains(&hit.chunk_id) {
                    hits.push(hit);
                }
            }
        }
    }
    // Counts only: the question itself is never written to the log.
    tx.audit(
        &principal.user,
        "search",
        &json!({ "query_chars": query_chars, "results": hits.len() }),
    )
    .await?;
    tx.commit().await?;

    Ok(Json(SearchResponse {
        results: hits
            .into_iter()
            .map(|hit| Passage {
                location_label: hit.location_label,
                citation_id: hit.chunk_id,
                document_id: hit.document_id,
                document_name: hit.document_name,
                text: hit.text.chars().take(MAX_PASSAGE_CHARS).collect(),
                score: hit.score,
                link: hit.web_url,
            })
            .collect(),
    }))
}

async fn ready(State(state): State<AppState>) -> StatusCode {
    let Ok(conn) = state.pool.get().await else {
        return StatusCode::SERVICE_UNAVAILABLE;
    };
    match conn.query_one("SELECT 1", &[]).await {
        Ok(_) => StatusCode::OK,
        Err(_) => StatusCode::SERVICE_UNAVAILABLE,
    }
}

// ---- Connections ----

pub(crate) fn require_admin(principal: &Principal) -> Result<(), ApiError> {
    if principal.is_admin() {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

fn valid_domain(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.contains('.')
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
}

fn valid_drive_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 256
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "!_-.".contains(c))
}

/// Which connectors this server can offer. The desktop app lists these and shows the others as
/// unavailable, so nobody clicks a button that can only fail.
async fn list_connectors(
    State(state): State<AppState>,
    Authed(principal): Authed,
) -> Json<serde_json::Value> {
    let available = |kind: Kind| state.oauth.is_enabled() && state.oauth.provider(kind).is_some();
    Json(json!({
        // Anyone may connect a source for themselves; only an administrator for everyone.
        "can_share_with_workspace": principal.is_admin(),
        "connectors": [
        {
            "kind": Kind::Dropbox.as_str(),
            "name": "Dropbox",
            "description": "Client files and shared documents in Dropbox: Word, Excel, PowerPoint, PDF and text files, with the sharing Dropbox records.",
            "available": available(Kind::Dropbox),
        },
        {
            "kind": Kind::DocuSign.as_str(),
            "name": "DocuSign",
            "description": "Contracts and agreements sent for signature: the signed documents and who has signed or is still outstanding.",
            "available": available(Kind::DocuSign),
        },
        {
            "kind": Kind::OutlookMail.as_str(),
            "name": "Outlook email",
            "description": "Send the emails your AI helpers prepare, from your own Outlook, after you have read them. It can only send what you approve, and cannot read your mail.",
            "available": available(Kind::OutlookMail),
        },
        {
            "kind": Kind::QuickBooks.as_str(),
            "name": "QuickBooks Online",
            "description": "A company's customers, vendors, invoices, bills, payments, expenses and key reports, kept up to date. Read only, and only you can search what you connect.",
            "available": available(Kind::QuickBooks),
        },
        {
            "kind": Kind::Zoom.as_str(),
            "name": "Zoom",
            "description": "Create Zoom meetings for you, after you have read and approved them, and put the join link in your emails. It cannot read your meetings, recordings or contacts.",
            "available": available(Kind::Zoom),
        },
        {
            "kind": Kind::GoogleDrive.as_str(),
            "name": "Google Drive",
            "description": "Docs, Sheets, Slides, PDFs and Office files in My Drive, shared drives and files shared with you. You can also save a report into your own Drive, after you have read and approved it.",
            "available": available(Kind::GoogleDrive),
        },
        {
            "kind": Kind::Microsoft365.as_str(),
            "name": "Microsoft 365",
            "description": "OneDrive and SharePoint document libraries: Word, Excel, PowerPoint and PDF files.",
            "available": available(Kind::Microsoft365),
        },
    ] }))
}

#[derive(Deserialize)]
struct ConnectRequest {
    kind: String,
    /// Microsoft only: a SharePoint library or OneDrive to read. Defaults to the approving
    /// administrator's own OneDrive.
    drive_id: Option<String>,
    /// Email domains of the firm, used for "anyone in the organization" sharing links.
    domains: Option<Vec<String>>,
    /// Re-authorise this connection (one that needs reconnecting) instead of adding a new one.
    reconnect: Option<Uuid>,
    /// Who the files are for: "private" (only the person connecting) or "workspace" (everyone at
    /// the firm, each seeing what the provider lets them open). Only an administrator may connect
    /// for the workspace. Left out, it is "workspace", as connections always were.
    scope: Option<String>,
}

async fn connect(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<ConnectRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut scope = match request.scope.as_deref() {
        None | Some("workspace") => "workspace",
        Some("private") => "private",
        Some(_) => return Err(ApiError::BadRequest("unknown scope")),
    };
    // A mailbox is one person's. It is never opened to the workspace, and anyone may connect their own.
    if ["outlook_mail", "quickbooks", "zoom"].contains(&request.kind.as_str()) {
        if request.scope.as_deref() == Some("workspace") {
            return Err(ApiError::BadRequest("this is connected for one person only"));
        }
        scope = "private";
    }
    // Connecting for the whole workspace is an administrator's decision; connecting only for
    // yourself is anyone's. Reconnecting keeps the scope the connection already has.
    if request.reconnect.is_none() && scope == "workspace" {
        require_admin(&principal)?;
    }
    if !state.oauth.is_enabled() {
        return Err(ApiError::Unavailable(
            "connectors are not set up on this server",
        ));
    }
    let kind = Kind::parse(&request.kind).ok_or(ApiError::BadRequest("unknown kind"))?;
    let provider = state.oauth.provider(kind).ok_or(ApiError::Unavailable(
        "that connector is not set up on this server",
    ))?;
    let mut config = serde_json::Map::new();
    if let Some(drive) = &request.drive_id {
        if kind != Kind::Microsoft365 || !valid_drive_id(drive) {
            return Err(ApiError::BadRequest("invalid drive_id"));
        }
        config.insert("drive_id".into(), json!(drive));
    }
    if let Some(domains) = &request.domains {
        let domains: Vec<String> = domains.iter().map(|d| d.trim().to_lowercase()).collect();
        if domains.len() > 10 || !domains.iter().all(|d| valid_domain(d)) {
            return Err(ApiError::BadRequest("invalid domains"));
        }
        config.insert("domains".into(), json!(domains));
    }
    if let Some(existing) = request.reconnect {
        let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
        let tx = open(&mut conn, &principal).await?;
        may_manage(&tx, &principal, existing).await?;
        let row = tx
            .sql()
            .query_opt(
                "SELECT kind, status, visibility FROM connections WHERE id = $1",
                &[&existing],
            )
            .await
            .map_err(ApiError::internal)?
            .ok_or(ApiError::NotFound)?;
        scope = if row.get::<_, String>(2) == "private" { "private" } else { "workspace" };
        if row.get::<_, String>(0) != request.kind {
            return Err(ApiError::BadRequest("that connection is a different kind"));
        }
        if row.get::<_, String>(1) != "needs_reauth" {
            return Err(ApiError::BadRequest(
                "that connection does not need reconnecting",
            ));
        }
        config.insert("reconnect".into(), json!(existing));
    }
    config.insert("scope".into(), json!(scope));
    let signed = oauth::sign_state(
        &state.oauth.state_key,
        principal.tenant,
        &principal.user,
        kind,
        serde_json::Value::Object(config),
    );
    let url = oauth::authorization_url(provider, provider.redirect(&state.oauth.redirect_uri), &signed, kind)?;
    Ok(Json(json!({ "authorization_url": url })))
}

fn page(status: StatusCode, message: &'static str) -> Response {
    (
        status,
        Html(format!(
            "<!doctype html><meta charset=utf-8><title>Connection</title>\
             <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\"><p>{message}</p>"
        )),
    )
        .into_response()
}

#[derive(Deserialize)]
struct CallbackQuery {
    /// QuickBooks says which company was approved.
    #[serde(rename = "realmId")]
    realm_id: Option<String>,
    code: Option<String>,
    state: Option<String>,
    error: Option<String>,
}

/// Where the provider sends the administrator after they approve (or refuse) access.
async fn oauth_callback(
    State(state): State<AppState>,
    Query(query): Query<CallbackQuery>,
) -> Response {
    const FAILED: &str =
        "We could not connect that account. Close this window and try again from the app.";
    if !state.oauth.is_enabled() {
        return page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Connectors are not set up on this server.",
        );
    }
    let (Some(code), Some(signed)) = (query.code.as_deref(), query.state.as_deref()) else {
        return page(StatusCode::BAD_REQUEST, FAILED);
    };
    if query.error.is_some() {
        return page(StatusCode::BAD_REQUEST, FAILED);
    }
    let Ok(started) = oauth::verify_state(&state.oauth.state_key, signed) else {
        return page(StatusCode::BAD_REQUEST, FAILED);
    };
    match finish_connection(&state, started, code, query.realm_id.as_deref()).await {
        Ok(()) => page(
            StatusCode::OK,
            "Connected. You can close this window and return to the app.",
        ),
        Err(ApiError::Forbidden) => page(StatusCode::FORBIDDEN, FAILED),
        Err(_) => page(StatusCode::BAD_REQUEST, FAILED),
    }
}

async fn finish_connection(
    state: &AppState,
    started: oauth::State,
    code: &str,
    realm_id: Option<&str>,
) -> Result<(), ApiError> {
    let tenant = crate::auth::TenantId(started.tenant);
    let kind = Kind::parse(&started.kind).ok_or(ApiError::BadRequest("kind"))?;
    let provider = state
        .oauth
        .provider(kind)
        .ok_or(ApiError::BadRequest("kind"))?;
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(ApiError::internal)?;
    let tokens = oauth::exchange_code(&http, provider, provider.redirect(&state.oauth.redirect_uri), code)
        .await
        .map_err(|_| ApiError::BadRequest("exchange"))?;

    let mut config = started.config.clone();
    config["connected_by"] = json!(started.user);
    if kind == Kind::Microsoft365 && config["drive_id"].is_null() {
        // No library was named, so read the approving administrator's own OneDrive.
        let drive: serde_json::Value = http
            .get(format!(
                "{}/v1.0/me/drive",
                provider.api_base.trim_end_matches('/')
            ))
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(ApiError::internal)?
            .error_for_status()
            .map_err(|_| ApiError::BadRequest("drive"))?
            .json()
            .await
            .map_err(ApiError::internal)?;
        let id = drive["id"]
            .as_str()
            .filter(|id| valid_drive_id(id))
            .ok_or(ApiError::BadRequest("drive"))?;
        config["drive_id"] = json!(id);
    }

    let mut label: Option<String> = None;
    if kind == Kind::OutlookMail {
        // Which address mail will be sent from, so the person can see it and trust it.
        let me: serde_json::Value = http
            .get(format!("{}/v1.0/me?$select=mail,userPrincipalName", provider.api_base.trim_end_matches('/')))
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(ApiError::internal)?
            .error_for_status()
            .map_err(|_| ApiError::BadRequest("account"))?
            .json()
            .await
            .map_err(ApiError::internal)?;
        label = me["mail"].as_str().or_else(|| me["userPrincipalName"].as_str()).map(str::to_string);
        if let Some(address) = &label {
            config["account_email"] = json!(address);
        }
    }
    if kind == Kind::Zoom {
        // Whose Zoom this is, so the person can see which account meetings will be created in.
        let me: serde_json::Value = http
            .get(format!("{}/v2/users/me", provider.api_base.trim_end_matches('/')))
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(ApiError::internal)?
            .error_for_status()
            .map_err(|_| ApiError::BadRequest("account"))?
            .json()
            .await
            .map_err(ApiError::internal)?;
        label = me["email"].as_str().or_else(|| me["display_name"].as_str()).map(|text| text.chars().take(120).collect::<String>());
        if let Some(address) = &label {
            config["account_email"] = json!(address);
        }
    }
    if kind == Kind::QuickBooks {
        // Which company was approved, and what it is called, so the person can see which books these are.
        let realm = realm_id.filter(|realm| crate::connectors::quickbooks::safe_realm(realm)).ok_or(ApiError::BadRequest("company"))?;
        let info: serde_json::Value = http
            .get(format!("{}/v3/company/{realm}/companyinfo/{realm}?minorversion=75", provider.api_base.trim_end_matches('/')))
            .bearer_auth(&tokens.access_token)
            .header("accept", "application/json")
            .send()
            .await
            .map_err(ApiError::internal)?
            .error_for_status()
            .map_err(|_| ApiError::BadRequest("company"))?
            .json()
            .await
            .map_err(ApiError::internal)?;
        let name = info["CompanyInfo"]["CompanyName"].as_str().unwrap_or("QuickBooks company").chars().take(120).collect::<String>();
        config["realm_id"] = json!(realm);
        config["company_name"] = json!(name);
        label = Some(name);
    }
    if kind == Kind::Dropbox {
        // The account's email lets the person who connected it find their own files, which Dropbox
        // lists no sharing for when they are not shared.
        let account: serde_json::Value = http
            .post(format!("{}/2/users/get_current_account", provider.api_base.trim_end_matches('/')))
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(ApiError::internal)?
            .error_for_status()
            .map_err(|_| ApiError::BadRequest("account"))?
            .json()
            .await
            .map_err(ApiError::internal)?;
        if let Some(email) = account["email"].as_str() {
            config["account_email"] = json!(email);
        }
    }
    if kind == Kind::DocuSign {
        // DocuSign's API address depends on the account, and it says which one at sign-in.
        let url = provider.userinfo_url.as_deref().ok_or(ApiError::BadRequest("userinfo"))?;
        let info: serde_json::Value = http
            .get(url)
            .bearer_auth(&tokens.access_token)
            .send()
            .await
            .map_err(ApiError::internal)?
            .error_for_status()
            .map_err(|_| ApiError::BadRequest("userinfo"))?
            .json()
            .await
            .map_err(ApiError::internal)?;
        let accounts = info["accounts"].as_array().cloned().unwrap_or_default();
        let chosen = accounts.iter().find(|a| a["is_default"].as_bool() == Some(true)).or_else(|| accounts.first()).ok_or(ApiError::BadRequest("account"))?;
        let id = chosen["account_id"].as_str().filter(|id| !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')).ok_or(ApiError::BadRequest("account"))?;
        let base = chosen["base_uri"].as_str().ok_or(ApiError::BadRequest("account"))?;
        // Calls carry our token to this address, so it must be DocuSign's own.
        if !oauth::docusign_address_ok(base, url.starts_with("http://")) {
            return Err(ApiError::BadRequest("account"));
        }
        config["account_id"] = json!(id);
        config["base_uri"] = json!(base.trim_end_matches('/'));
        if let Some(email) = info["email"].as_str() {
            config["account_email"] = json!(email);
        }
    }

    if let Some(existing) = started.config["reconnect"]
        .as_str()
        .and_then(|v| Uuid::parse_str(v).ok())
    {
        return reconnect_connection(state, tenant, &started, existing, &tokens.refresh_token)
            .await;
    }
    config
        .as_object_mut()
        .map(|object| object.remove("reconnect"));

    if matches!(kind, Kind::OutlookMail | Kind::Zoom) {
        let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
        let tx = TenantTx::begin(&mut conn, tenant).await?;
        let old: Vec<Uuid> = tx
            .sql()
            .query("DELETE FROM connections WHERE kind = $1 AND owner = $2 RETURNING id", &[&kind.as_str(), &started.user])
            .await
            .map_err(ApiError::internal)?
            .into_iter()
            .map(|row| row.get(0))
            .collect();
        tx.commit().await?;
        for id in old {
            let _ = state.secrets.delete(tenant, &refresh_token_name(tenant, id)).await;
        }
    }
    let connection = Uuid::new_v4();
    let visibility = if started.config["scope"] == "private" { "private" } else { "workspace" };
    let secret = refresh_token_name(tenant, connection);
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = TenantTx::begin(&mut conn, tenant).await?;
    if tx.status().await?.as_deref() != Some("active") {
        return Err(ApiError::Forbidden);
    }
    tx.sql()
        .execute(
            "INSERT INTO connections (id, tenant_id, kind, secret_ref, config, visibility, owner, label)
             VALUES ($1, app_tenant(), $2, $3, $4, $5, $6, $7)",
            &[&connection, &started.kind, &secret, &config, &visibility, &started.user, &label],
        )
        .await
        .map_err(ApiError::internal)?;
    // The token is saved before the connection becomes visible. If saving fails the
    // transaction is dropped and nothing is recorded.
    state
        .secrets
        .put(tenant, &secret, &tokens.refresh_token)
        .await?;
    if kind.syncs_files() {
        queue::enqueue(&tx, connection).await?;
    }
    tx.audit(
        &started.user,
        "connection_added",
        &json!({ "kind": started.kind, "connection": connection, "scope": visibility }),
    )
    .await?;
    if let Err(error) = tx.commit().await {
        let _ = state.secrets.delete(tenant, &secret).await;
        return Err(error.into());
    }
    Ok(())
}

/// Gives an existing connection a new token and puts it back to work.
async fn reconnect_connection(
    state: &AppState,
    tenant: crate::auth::TenantId,
    started: &oauth::State,
    existing: Uuid,
    refresh_token: &str,
) -> Result<(), ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = TenantTx::begin(&mut conn, tenant).await?;
    if tx.status().await?.as_deref() != Some("active") {
        return Err(ApiError::Forbidden);
    }
    let updated = tx
        .sql()
        .execute(
            "UPDATE connections SET status = 'active', last_error = NULL
             WHERE id = $1 AND kind = $2 AND status = 'needs_reauth'",
            &[&existing, &started.kind],
        )
        .await
        .map_err(ApiError::internal)?;
    if updated == 0 {
        return Err(ApiError::NotFound);
    }
    state
        .secrets
        .put(tenant, &refresh_token_name(tenant, existing), refresh_token)
        .await?;
    if Kind::parse(&started.kind).is_some_and(Kind::syncs_files) {
        queue::enqueue(&tx, existing).await?;
    }
    tx.audit(
        &started.user,
        "connection_reconnected",
        &json!({ "kind": started.kind, "connection": existing }),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn list_connections(
    State(state): State<AppState>,
    Authed(principal): Authed,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let rows = tx
        .sql()
        .query(
            "SELECT c.id, c.kind, c.status, EXTRACT(EPOCH FROM c.last_sync_at)::float8, c.last_error,
                    (SELECT count(*) FROM documents d WHERE d.connection_id = c.id),
                    (c.sync_started_at IS NOT NULL AND q.claimed_until > now()),
                    EXTRACT(EPOCH FROM c.sync_started_at)::float8,
                    c.sync_files_read, c.sync_files_failed,
                    EXTRACT(EPOCH FROM q.run_after)::float8,
                    (SELECT count(*) FROM documents d WHERE d.connection_id = c.id AND d.status = 'indexed'),
                    (SELECT COALESCE(jsonb_object_agg(note, n), '{}'::jsonb)
                     FROM (SELECT note, count(*) AS n FROM documents d
                           WHERE d.connection_id = c.id AND d.note IS NOT NULL GROUP BY note) t)
             , c.visibility, (c.owner = $1), c.label
             FROM connections c LEFT JOIN sync_queue q ON q.connection_id = c.id
             WHERE c.visibility = 'workspace' OR c.owner = $1
             ORDER BY c.kind, c.id",
            &[&principal.user],
        )
        .await
        .map_err(ApiError::internal)?;
    let connections: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            let syncing: bool = row.get(6);
            json!({
                "id": row.get::<_, Uuid>(0),
                "kind": row.get::<_, String>(1),
                "status": row.get::<_, String>(2),
                "last_sync_epoch": row.get::<_, Option<f64>>(3),
                "last_error": row.get::<_, Option<String>>(4),
                "documents": row.get::<_, i64>(5),
                "syncing": syncing,
                "sync_started_epoch": if syncing { row.get::<_, Option<f64>>(7) } else { None },
                "sync_files_read": row.get::<_, i32>(8),
                "sync_files_failed": row.get::<_, i32>(9),
                "next_sync_epoch": row.get::<_, Option<f64>>(10),
                "readable": row.get::<_, i64>(11),
                "name_only_reasons": row.get::<_, serde_json::Value>(12),
                "scope": row.get::<_, String>(13),
                // Only the person who connected it can change or remove a private connection, and
                // only an administrator a workspace one.
                "yours": row.get::<_, Option<bool>>(14).unwrap_or(false),
                // For a mailbox, the address it sends from.
                "label": row.get::<_, Option<String>>(15),
            })
        })
        .collect();
    Ok(Json(json!({ "connections": connections })))
}

#[derive(Deserialize)]
struct DocumentQuery {
    /// Longest text, in characters, the caller will accept. Longer documents come back without text.
    max_chars: Option<i64>,
}

/// Most characters of one document the server will ever return: about half a million tokens.
const MAX_DOCUMENT_CHARS: i64 = 2_000_000;
const DEFAULT_DOCUMENT_CHARS: i64 = 400_000;

/// A whole document, for answering detailed questions about it. Only a person who may read the
/// file in its own service gets it; anyone else, and a document that does not exist, both get 404
/// so the reply does not reveal that a file exists.
async fn document(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    axum::extract::Query(query): axum::extract::Query<DocumentQuery>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let max = query
        .max_chars
        .unwrap_or(DEFAULT_DOCUMENT_CHARS)
        .clamp(1, MAX_DOCUMENT_CHARS);
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let found = tx
        .document_text(&principal, id, max)
        .await?
        .ok_or(ApiError::NotFound)?;
    // Counts only: never the text.
    tx.audit(
        &principal.user,
        "document_read",
        &json!({ "document": id, "chars": found.total_chars, "sent": !found.too_long }),
    )
    .await?;
    tx.commit().await?;
    let passages: Vec<serde_json::Value> = found
        .passages
        .into_iter()
        .map(|(label, text)| json!({ "label": label, "text": text }))
        .collect();
    Ok(Json(json!({
        "document_id": id,
        "name": found.name,
        "link": found.web_url,
        "total_chars": found.total_chars,
        "too_long": found.too_long,
        "passages": passages,
    })))
}

#[derive(Serialize)]
struct UsageView {
    used_micros: i64,
    limit_micros: i64,
    remaining_micros: i64,
    answers: i64,
    input_tokens: i64,
    output_tokens: i64,
    period_start_epoch: f64,
    period_end_epoch: f64,
    /// The limit is the server's default, not one the firm chose.
    limit_is_default: bool,
}

fn usage_view(summary: crate::db::UsageSummary) -> UsageView {
    let limit = summary
        .firm_limit_micros
        .unwrap_or_else(crate::usage::default_limit_micros);
    UsageView {
        used_micros: summary.used_micros,
        limit_micros: limit,
        remaining_micros: (limit - summary.used_micros).max(0),
        answers: summary.answers,
        input_tokens: summary.input_tokens,
        output_tokens: summary.output_tokens,
        period_start_epoch: summary.period_start_epoch,
        period_end_epoch: summary.period_end_epoch,
        limit_is_default: summary.firm_limit_micros.is_none(),
    }
}

/// What the signed-in person has used this month, and their limit. Anyone may read their own.
async fn usage(
    State(state): State<AppState>,
    Authed(principal): Authed,
) -> Result<Json<UsageView>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let summary = tx.usage_this_month(&principal.user).await?;
    Ok(Json(usage_view(summary)))
}

#[derive(Deserialize)]
struct UsageReport {
    model: String,
    input_tokens: i64,
    output_tokens: i64,
}

/// Tokens in a single answer can never exceed the largest context a model accepts.
const MAX_REPORTED_TOKENS: i64 = 5_000_000;

/// Records one answer against the signed-in person. The price is the server's, not the caller's.
async fn record_usage(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(report): Json<UsageReport>,
) -> Result<Json<UsageView>, ApiError> {
    if report.model.is_empty() || report.model.len() > 100 {
        return Err(ApiError::BadRequest("model must be 1 to 100 characters"));
    }
    if !(0..=MAX_REPORTED_TOKENS).contains(&report.input_tokens)
        || !(0..=MAX_REPORTED_TOKENS).contains(&report.output_tokens)
    {
        return Err(ApiError::BadRequest("token counts are out of range"));
    }
    let cost = crate::usage::cost_micros(
        &report.model,
        report.input_tokens as u64,
        report.output_tokens as u64,
    );
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    tx.record_usage(
        &principal.user,
        &report.model,
        report.input_tokens,
        report.output_tokens,
        cost,
    )
    .await?;
    let summary = tx.usage_this_month(&principal.user).await?;
    tx.commit().await?;
    Ok(Json(usage_view(summary)))
}

#[derive(Deserialize)]
struct LimitRequest {
    /// Cents per person per month; null returns to the server default.
    monthly_limit_cents: Option<i64>,
}

async fn set_usage_limit(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<LimitRequest>,
) -> Result<Json<UsageView>, ApiError> {
    require_admin(&principal)?;
    if request
        .monthly_limit_cents
        .is_some_and(|cents| !(0..=10_000_000).contains(&cents))
    {
        return Err(ApiError::BadRequest(
            "the limit must be between 0 and 100,000 dollars",
        ));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    tx.set_monthly_limit(request.monthly_limit_cents.map(|cents| cents * 10_000))
        .await?;
    tx.audit(
        &principal.user,
        "usage_limit_changed",
        &json!({ "monthly_limit_cents": request.monthly_limit_cents }),
    )
    .await?;
    let summary = tx.usage_this_month(&principal.user).await?;
    tx.commit().await?;
    Ok(Json(usage_view(summary)))
}

/// Each person's use this month, for administrators.
async fn usage_team(
    State(state): State<AppState>,
    Authed(principal): Authed,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let people: Vec<serde_json::Value> = tx
        .usage_by_person()
        .await?
        .into_iter()
        .map(|(user, micros, answers)| json!({ "user": user, "used_micros": micros, "answers": answers }))
        .collect();
    Ok(Json(json!({ "people": people })))
}

/// Files stored by name only, with the reason, so an administrator can see why a file cannot be
/// searched by its content. Names are shown to administrators only.
async fn connection_files(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    may_manage(&tx, &principal, id).await?;
    let rows = tx
        .sql()
        .query(
            "SELECT d.name, d.note, d.mime_type FROM documents d
             WHERE d.connection_id = $1 AND d.note IS NOT NULL
             ORDER BY d.note, d.name LIMIT 300",
            &[&id],
        )
        .await
        .map_err(ApiError::internal)?;
    let files: Vec<serde_json::Value> = rows
        .iter()
        .map(|row| {
            json!({
                "name": row.get::<_, String>(0),
                "reason": row.get::<_, String>(1),
                "mime_type": row.get::<_, String>(2),
            })
        })
        .collect();
    Ok(Json(json!({ "files": files })))
}

async fn disconnect(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    may_manage(&tx, &principal, id).await?;
    // Deleting the connection removes its files and passages through the cascading keys.
    let removed = tx
        .sql()
        .execute("DELETE FROM connections WHERE id = $1", &[&id])
        .await
        .map_err(ApiError::internal)?;
    if removed == 0 {
        return Err(ApiError::NotFound);
    }
    tx.audit(
        &principal.user,
        "connection_removed",
        &json!({ "connection": id }),
    )
    .await?;
    tx.commit().await?;
    // After the data is gone, drop the saved token too. A failure here is logged, not hidden.
    if let Err(error) = state
        .secrets
        .delete(principal.tenant, &refresh_token_name(principal.tenant, id))
        .await
    {
        tracing::error!(%error, "connection removed but its saved token could not be deleted");
    }
    Ok(StatusCode::NO_CONTENT)
}

async fn sync_now(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    may_manage(&tx, &principal, id).await?;
    let is_mailbox: bool = tx
        .sql()
        .query_opt("SELECT kind IN ('outlook_mail', 'zoom') FROM connections WHERE id = $1", &[&id])
        .await
        .map_err(ApiError::internal)?
        .is_some_and(|row| row.get(0));
    if is_mailbox {
        return Err(ApiError::BadRequest("this connection has no files to sync"));
    }
    let exists = tx
        .sql()
        .query_opt("SELECT 1 FROM connections WHERE id = $1", &[&id])
        .await
        .map_err(ApiError::internal)?
        .is_some();
    if !exists {
        return Err(ApiError::NotFound);
    }
    queue::enqueue(&tx, id).await?;
    tx.audit(
        &principal.user,
        "sync_requested",
        &json!({ "connection": id }),
    )
    .await?;
    tx.commit().await?;
    Ok(StatusCode::ACCEPTED)
}

/// Whether this person may change or remove a connection. A workspace connection belongs to the
/// administrators; a private one belongs to the person who connected it alone, not even to an
/// administrator, who is told it does not exist.
pub(crate) async fn may_manage(tx: &crate::db::TenantTx<'_>, principal: &Principal, id: Uuid) -> Result<(), ApiError> {
    let row = tx
        .sql()
        .query_opt("SELECT visibility, owner FROM connections WHERE id = $1", &[&id])
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::NotFound)?;
    if row.get::<_, String>(0) == "private" {
        if row.get::<_, Option<String>>(1).as_deref() == Some(principal.user.as_str()) {
            Ok(())
        } else {
            Err(ApiError::NotFound)
        }
    } else {
        require_admin(principal)
    }
}

#[derive(Deserialize)]
struct ScopeRequest {
    scope: String,
}

/// Changes who a connection's files are for. Only the person who connected it may, and only an
/// administrator may open it to the whole workspace. Going private hides every file at once; going
/// to the workspace brings each file's own sharing back at the next sync, which starts now.
async fn set_scope(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<ScopeRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let scope = match request.scope.as_str() {
        "private" => "private",
        "workspace" => {
            require_admin(&principal)?;
            "workspace"
        }
        _ => return Err(ApiError::BadRequest("unknown scope")),
    };
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let changed = tx
        .sql()
        .execute(
            "UPDATE connections SET visibility = $2 WHERE id = $1 AND owner = $3",
            &[&id, &scope, &principal.user],
        )
        .await
        .map_err(ApiError::internal)?;
    if changed == 0 {
        return Err(ApiError::NotFound);
    }
    queue::enqueue(&tx, id).await?;
    tx.audit(&principal.user, "connection_scope_changed", &json!({ "connection": id, "scope": scope })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "scope": scope })))
}
