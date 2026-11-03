//! The company knowledge endpoints. Administrators see everything the analysis found, including
//! what still needs checking, and can confirm, correct or reject it. Everyone else sees only what
//! has been confirmed, and only through documents they may open.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post, put},
};
use serde::Deserialize;
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    api::{ApiError, AppState, Authed, open, require_admin},
    knowledge::{ENTITY_KINDS, FACT_KINDS},
    knowledge_store::{self as store, Review, Reviewed},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/knowledge", get(overview))
        .route("/v1/knowledge/settings", put(set_settings))
        .route("/v1/knowledge/entities", get(entities))
        .route("/v1/knowledge/facts", get(facts))
        .route("/v1/knowledge/facts/{id}/review", post(review_fact))
        .route("/v1/knowledge/entities/{id}/review", post(review_entity))
        .route("/v1/knowledge/profile/review", put(review_profile))
}

const MAX_LIST: i64 = 100;

/// Everything for the knowledge profile screen. Counts here come from every document the firm has
/// connected, so only administrators may see them.
async fn overview(
    State(state): State<AppState>,
    Authed(principal): Authed,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let enabled = store::analysis_enabled(&tx).await?;
    let overview = store::overview(&tx).await?;
    let profile = store::profile(&tx).await?;
    Ok(Json(json!({
        "enabled": enabled,
        // Whether this server has a model to read documents with.
        "analysis_available": std::env::var("ANTHROPIC_API_KEY").is_ok_and(|key| !key.trim().is_empty()),
        "stale_after_days": store::STALE_AFTER_DAYS,
        "overview": overview,
        "profile": profile,
    })))
}

#[derive(Deserialize)]
struct SettingsRequest {
    enabled: bool,
}

/// Switches reading documents for company knowledge on or off. Turning it on lets the server send
/// the text of the firm's documents to an AI model, so it is an administrator's decision.
async fn set_settings(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<SettingsRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    store::set_analysis_enabled(&tx, request.enabled).await?;
    tx.audit(
        &principal.user,
        "company_knowledge_switched",
        &json!({ "enabled": request.enabled }),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({ "enabled": request.enabled })))
}

#[derive(Deserialize)]
struct ListQuery {
    status: Option<String>,
    kind: Option<String>,
    q: Option<String>,
    review_only: Option<bool>,
    limit: Option<i64>,
}

/// Which statuses a caller may ask for. Staff see confirmed items only.
fn statuses(principal: &crate::auth::Principal, asked: Option<&str>) -> Vec<String> {
    if !principal.is_admin() {
        return vec!["verified".into()];
    }
    match asked {
        Some("all") => ["suggested", "verified", "rejected", "outdated"]
            .map(String::from)
            .to_vec(),
        Some(one @ ("suggested" | "verified" | "rejected" | "outdated")) => vec![one.into()],
        // By default what is still in play: not what has been rejected.
        _ => ["suggested", "verified", "outdated"]
            .map(String::from)
            .to_vec(),
    }
}

async fn entities(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if query
        .kind
        .as_deref()
        .is_some_and(|kind| !ENTITY_KINDS.contains(&kind))
    {
        return Err(ApiError::BadRequest("unknown kind"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let list = store::list_entities(
        &tx,
        &principal.readers(),
        &statuses(&principal, query.status.as_deref()),
        query.kind.as_deref(),
        query.q.as_deref().filter(|text| !text.trim().is_empty()),
        query.limit.unwrap_or(50).clamp(1, MAX_LIST),
    )
    .await?;
    Ok(Json(json!({ "entities": list })))
}

async fn facts(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Query(query): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    if query
        .kind
        .as_deref()
        .is_some_and(|kind| !FACT_KINDS.contains(&kind))
    {
        return Err(ApiError::BadRequest("unknown kind"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let list = store::list_facts(
        &tx,
        &principal.readers(),
        &statuses(&principal, query.status.as_deref()),
        query.kind.as_deref(),
        principal.is_admin() && query.review_only == Some(true),
        query.limit.unwrap_or(50).clamp(1, MAX_LIST),
    )
    .await?;
    Ok(Json(json!({ "facts": list })))
}

#[derive(Deserialize)]
struct ReviewRequest {
    /// "confirm", "reject" or "correct".
    action: String,
    /// The corrected statement (facts) or name (entities), for "correct".
    text: Option<String>,
}

fn parse_review(request: &ReviewRequest) -> Result<Review, ApiError> {
    match request.action.as_str() {
        "confirm" => Ok(Review::Confirm),
        "reject" => Ok(Review::Reject),
        "correct" => match request.text.as_deref().map(str::trim) {
            Some(text) if (3..=400).contains(&text.chars().count()) => {
                Ok(Review::Correct(text.to_string()))
            }
            _ => Err(ApiError::BadRequest(
                "a correction needs 3 to 400 characters of text",
            )),
        },
        _ => Err(ApiError::BadRequest(
            "action must be confirm, reject or correct",
        )),
    }
}

fn outcome(reviewed: Reviewed) -> Result<Json<Value>, ApiError> {
    match reviewed {
        Reviewed::Done => Ok(Json(json!({ "ok": true }))),
        // The same answer for an item that does not exist and one the caller cannot see.
        Reviewed::NotFound => Err(ApiError::NotFound),
        Reviewed::Duplicate => Err(ApiError::BadRequest("that is already another item")),
    }
}

async fn review_fact(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<ReviewRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let review = parse_review(&request)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let reviewed =
        store::review_fact(&tx, &principal.readers(), id, &principal.user, &review).await?;
    if reviewed == Reviewed::Done {
        tx.audit(
            &principal.user,
            "fact_reviewed",
            &json!({ "fact": id, "action": request.action }),
        )
        .await?;
        tx.commit().await?;
    }
    outcome(reviewed)
}

async fn review_entity(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<ReviewRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let review = parse_review(&request)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let reviewed =
        store::review_entity(&tx, &principal.readers(), id, &principal.user, &review).await?;
    if reviewed == Reviewed::Done {
        tx.audit(
            &principal.user,
            "entity_reviewed",
            &json!({ "entity": id, "action": request.action }),
        )
        .await?;
        tx.commit().await?;
    }
    outcome(reviewed)
}

#[derive(Deserialize)]
struct ProfileReview {
    summary: Option<String>,
    services: Option<Vec<String>>,
}

/// An administrator confirms the company profile, optionally with corrections.
async fn review_profile(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<ProfileReview>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let summary = request.summary.as_deref().map(str::trim);
    if summary.is_some_and(|text| text.is_empty() || text.chars().count() > 2000) {
        return Err(ApiError::BadRequest(
            "the summary must be 1 to 2000 characters",
        ));
    }
    let services: Option<Vec<String>> = request.services.map(|list| {
        list.into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .take(40)
            .collect()
    });
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !store::review_profile(&tx, &principal.user, summary, services.as_deref()).await? {
        return Err(ApiError::NotFound);
    }
    tx.audit(&principal.user, "profile_reviewed", &json!({}))
        .await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}
