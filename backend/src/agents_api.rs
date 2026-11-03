//! Shared agents over HTTP. Administrators create and configure them; employees see the ones
//! offered to them, give them tasks, and read the results of their own tasks.

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    routing::{get, post},
};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{
    agents::{self, AgentSettings, AgentSpec, MAX_TASK_CHARS, Started},
    api::{ApiError, AppState, Authed, open, require_admin},
};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/v1/agents", get(list).post(create))
        .route("/v1/agents/{id}", axum::routing::put(update))
        .route("/v1/agents/{id}/versions", get(versions))
        .route("/v1/agents/{id}/rollback", post(rollback))
        .route("/v1/agents/{id}/tasks", post(start_task))
        .route("/v1/tasks", get(tasks))
        .route("/v1/tasks/{id}", get(task))
        .route("/v1/tasks/{id}/cancel", post(cancel))
}

/// Tells a missing field from an explicit null, so a budget can be left alone or removed.
fn double_option<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Option<i64>>, D::Error> {
    Ok(Some(Option::<i64>::deserialize(d)?))
}

#[derive(Deserialize, Default)]
struct AgentRequest {
    name: Option<String>,
    role: Option<String>,
    description: Option<String>,
    icon: Option<String>,
    color: Option<String>,
    instructions: Option<String>,
    tools: Option<Vec<String>>,
    model: Option<String>,
    effort: Option<String>,
    note: Option<String>,
    status: Option<String>,
    audience: Option<Vec<String>>,
    #[serde(default, deserialize_with = "double_option")]
    monthly_budget_micros: Option<Option<i64>>,
    max_concurrent: Option<i32>,
}

impl AgentRequest {
    fn settings(&self) -> AgentSettings {
        AgentSettings {
            status: self.status.clone(),
            audience: self.audience.clone(),
            monthly_budget_micros: self.monthly_budget_micros,
            max_concurrent: self.max_concurrent,
        }
    }

    /// The spec this request describes, with anything left out taken from `base`.
    fn spec(&self, base: Option<&AgentSpec>) -> AgentSpec {
        let pick = |new: &Option<String>, old: fn(&AgentSpec) -> &String, default: &str| {
            new.clone().or_else(|| base.map(|b| old(b).clone())).unwrap_or_else(|| default.to_string())
        };
        AgentSpec {
            name: pick(&self.name, |s| &s.name, ""),
            role: pick(&self.role, |s| &s.role, ""),
            description: pick(&self.description, |s| &s.description, ""),
            icon: pick(&self.icon, |s| &s.icon, "sparkles"),
            color: pick(&self.color, |s| &s.color, "blue"),
            instructions: pick(&self.instructions, |s| &s.instructions, ""),
            tools: self.tools.clone().or_else(|| base.map(|b| b.tools.clone())).unwrap_or_default(),
            model: pick(&self.model, |s| &s.model, "standard"),
            effort: pick(&self.effort, |s| &s.effort, "medium"),
        }
    }
}

async fn list(State(state): State<AppState>, Authed(principal): Authed) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({
        "agents": agents::list_agents(&tx, &principal).await?,
        "can_manage": principal.is_admin(),
    })))
}

async fn create(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Json(request): Json<AgentRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let spec = request.spec(None);
    let settings = request.settings();
    if let Some(problem) = spec.problem().or_else(|| settings.problem()) {
        return Err(ApiError::BadRequest(problem));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let id = agents::create_agent(&tx, &principal.user, &spec, &settings).await?;
    tx.audit(&principal.user, "agent_created", &json!({ "agent": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id })))
}

async fn update(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<AgentRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let settings = request.settings();
    if let Some(problem) = settings.problem() {
        return Err(ApiError::BadRequest(problem));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let Some(current) = agents::get_agent(&tx, id).await? else {
        return Err(ApiError::NotFound);
    };
    let base = AgentSpec {
        name: current.name,
        role: current.role,
        description: current.description,
        icon: current.icon,
        color: current.color,
        instructions: current.instructions.unwrap_or_default(),
        tools: current.tools,
        model: current.model,
        effort: current.effort,
    };
    let spec = request.spec(Some(&base));
    if let Some(problem) = spec.problem() {
        return Err(ApiError::BadRequest(problem));
    }
    let note = request.note.clone().unwrap_or_default().chars().take(200).collect::<String>();
    agents::update_agent(&tx, id, &principal.user, Some(&spec), &settings, &note).await?;
    tx.audit(&principal.user, "agent_updated", &json!({ "agent": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn versions(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({ "versions": agents::versions(&tx, id).await? })))
}

#[derive(Deserialize)]
struct RollbackRequest {
    version: i32,
}

async fn rollback(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<RollbackRequest>,
) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let Some(version) = agents::rollback(&tx, id, &principal.user, request.version).await? else {
        return Err(ApiError::NotFound);
    };
    tx.audit(&principal.user, "agent_rolled_back", &json!({ "agent": id, "to": request.version })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "version": version })))
}

#[derive(Deserialize)]
struct TaskRequest {
    instruction: String,
}

async fn start_task(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<TaskRequest>,
) -> Result<Json<Value>, ApiError> {
    let length = request.instruction.trim().chars().count();
    if length == 0 || length > MAX_TASK_CHARS {
        return Err(ApiError::BadRequest("describe the task in 1 to 4,000 characters"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    match agents::start_task(&tx, &principal, id, &request.instruction).await? {
        Started::Queued(task) => {
            // Only the fact that a task began, never what it asked.
            tx.audit(&principal.user, "agent_task_started", &json!({ "agent": id, "task": task })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "id": task })))
        }
        Started::NoSuchAgent | Started::NotAvailable => Err(ApiError::NotFound),
        Started::BudgetReached => Err(ApiError::BadRequest("this agent has reached its monthly budget")),
    }
}

#[derive(Deserialize)]
struct TaskQuery {
    #[serde(default)]
    everyone: bool,
}

async fn tasks(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Query(query): Query<TaskQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({ "tasks": agents::list_tasks(&tx, &principal, query.everyone, 50).await? })))
}

async fn task(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    match agents::get_task(&tx, &principal, id).await? {
        Some(task) => Ok(Json(json!(task))),
        None => Err(ApiError::NotFound),
    }
}

async fn cancel(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !agents::cancel_task(&tx, &principal, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}
