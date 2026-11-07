//! Recurring tasks over HTTP. Every route is about the caller's own schedules and reports: there is
//! no administrator view of anyone else's.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post, put},
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    agents::MAX_TASK_CHARS,
    api::{ApiError, AppState, Authed, open},
    schedules::{self, Cadence, Created, Launched, Review, SchedulePatch},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/schedules", get(list).post(create))
        .route("/v1/schedules/{id}", put(update))
        .route("/v1/schedules/{id}/remove", post(remove))
        .route("/v1/schedules/{id}/run", post(run))
        .route("/v1/inbox", get(inbox))
        .route("/v1/tasks/{id}/review", post(review))
}

fn double_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<i64>>, D::Error> {
    Ok(Some(Option::<i64>::deserialize(d)?))
}

#[derive(Deserialize)]
struct ListQuery {
    offset: Option<i32>,
}

async fn list(State(state): State<AppState>, Authed(principal): Authed, Query(query): Query<ListQuery>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let list = schedules::list(&tx, &principal, query.offset).await?;
    tx.commit().await?;
    Ok(Json(json!({ "schedules": list })))
}

#[derive(Deserialize)]
struct CreateRequest {
    agent_id: Uuid,
    name: String,
    instruction: String,
    cadence: Cadence,
    monthly_budget_micros: Option<i64>,
}

async fn create(State(state): State<AppState>, Authed(principal): Authed, Json(request): Json<CreateRequest>) -> Result<Json<Value>, ApiError> {
    let patch = SchedulePatch {
        name: Some(request.name.clone()),
        instruction: Some(request.instruction.clone()),
        cadence: Some(request.cadence.clone()),
        monthly_budget_micros: Some(request.monthly_budget_micros),
        ..Default::default()
    };
    if let Some(problem) = patch.problem() {
        return Err(ApiError::BadRequest(problem));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    match schedules::create(&tx, &principal, request.agent_id, &request.name, &request.instruction, &request.cadence, request.monthly_budget_micros).await? {
        Created::Made(id) => {
            tx.audit(&principal.user, "schedule_created", &json!({ "schedule": id, "agent": request.agent_id })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "id": id })))
        }
        Created::NoSuchAgent | Created::NotAvailable => Err(ApiError::NotFound),
        Created::TooMany => Err(ApiError::BadRequest("you have reached the limit of 25 schedules")),
    }
}

#[derive(Deserialize)]
struct UpdateRequest {
    name: Option<String>,
    instruction: Option<String>,
    cadence: Option<Cadence>,
    status: Option<String>,
    #[serde(default, deserialize_with = "double_option")]
    monthly_budget_micros: Option<Option<i64>>,
}

async fn update(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>, Json(request): Json<UpdateRequest>) -> Result<Json<Value>, ApiError> {
    let patch = SchedulePatch {
        name: request.name,
        instruction: request.instruction,
        cadence: request.cadence,
        status: request.status,
        monthly_budget_micros: request.monthly_budget_micros,
    };
    if let Some(problem) = patch.problem() {
        return Err(ApiError::BadRequest(problem));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !schedules::update(&tx, &principal, id, &patch).await? {
        return Err(ApiError::NotFound);
    }
    tx.audit(&principal.user, "schedule_updated", &json!({ "schedule": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn remove(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !schedules::remove(&tx, &principal, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.audit(&principal.user, "schedule_removed", &json!({ "schedule": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn run(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    match schedules::run_now(&tx, &principal, id).await? {
        None => Err(ApiError::NotFound),
        Some(Launched::Started(task)) => {
            tx.audit(&principal.user, "schedule_run_now", &json!({ "schedule": id, "task": task })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "id": task })))
        }
        Some(Launched::Skipped(_)) => Err(ApiError::BadRequest("it could not run now: check the budget and your monthly allowance")),
        Some(Launched::Paused(_)) => Err(ApiError::BadRequest("it could not run now: the agent is not available to you")),
    }
}

async fn inbox(State(state): State<AppState>, Authed(principal): Authed) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({ "tasks": crate::agents::inbox(&tx, &principal).await? })))
}

#[derive(Deserialize)]
struct ReviewRequest {
    action: String,
    edited_result: Option<String>,
}

async fn review(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>, Json(request): Json<ReviewRequest>) -> Result<Json<Value>, ApiError> {
    let decision = match request.action.as_str() {
        "approve" => Review::Approve,
        "reject" => Review::Reject,
        _ => return Err(ApiError::BadRequest("approve or reject")),
    };
    if request.edited_result.as_deref().is_some_and(|text| text.chars().count() > MAX_TASK_CHARS * 20) {
        return Err(ApiError::BadRequest("the edited report is too long"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !schedules::review(&tx, &principal, id, decision, request.edited_result.as_deref()).await? {
        return Err(ApiError::NotFound);
    }
    // The decision, never the report.
    tx.audit(&principal.user, "report_reviewed", &json!({ "task": id, "action": request.action })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}
