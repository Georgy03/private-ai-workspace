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
        .route("/v1/search", post(search))
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
        .merge(crate::agents_api::routes())
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
    let hits = tx
        .search_hybrid(
            &principal,
            query,
            vector
                .as_ref()
                .map(|(model, vector)| (model.as_str(), vector.as_slice())),
            limit,
        )
        .await?;
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
    Authed(_): Authed,
) -> Json<serde_json::Value> {
    let available = |kind: Kind| state.oauth.is_enabled() && state.oauth.provider(kind).is_some();
    Json(json!({ "connectors": [
        {
            "kind": Kind::GoogleDrive.as_str(),
            "name": "Google Drive",
            "description": "Docs, Sheets, Slides, PDFs and Office files in My Drive, shared drives and files shared with you.",
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
}

async fn connect(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<ConnectRequest>,
) -> Result<Json<serde_json::Value>, ApiError> {
    require_admin(&principal)?;
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
        let row = tx
            .sql()
            .query_opt(
                "SELECT kind, status FROM connections WHERE id = $1",
                &[&existing],
            )
            .await
            .map_err(ApiError::internal)?
            .ok_or(ApiError::NotFound)?;
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
    let signed = oauth::sign_state(
        &state.oauth.state_key,
        principal.tenant,
        &principal.user,
        kind,
        serde_json::Value::Object(config),
    );
    let url = oauth::authorization_url(provider, &state.oauth.redirect_uri, &signed, kind)?;
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
    match finish_connection(&state, started, code).await {
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
    let tokens = oauth::exchange_code(&http, provider, &state.oauth.redirect_uri, code)
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

    let connection = Uuid::new_v4();
    let secret = refresh_token_name(tenant, connection);
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = TenantTx::begin(&mut conn, tenant).await?;
    if tx.status().await?.as_deref() != Some("active") {
        return Err(ApiError::Forbidden);
    }
    tx.sql()
        .execute(
            "INSERT INTO connections (id, tenant_id, kind, secret_ref, config)
             VALUES ($1, app_tenant(), $2, $3, $4)",
            &[&connection, &started.kind, &secret, &config],
        )
        .await
        .map_err(ApiError::internal)?;
    // The token is saved before the connection becomes visible. If saving fails the
    // transaction is dropped and nothing is recorded.
    state
        .secrets
        .put(tenant, &secret, &tokens.refresh_token)
        .await?;
    queue::enqueue(&tx, connection).await?;
    tx.audit(
        &started.user,
        "connection_added",
        &json!({ "kind": started.kind, "connection": connection }),
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
    queue::enqueue(&tx, existing).await?;
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
             FROM connections c LEFT JOIN sync_queue q ON q.connection_id = c.id
             ORDER BY c.kind, c.id",
            &[],
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
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
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
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
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
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
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
