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
        .route("/v1/actions", get(waiting_actions))
        .route("/v1/actions/{id}/approve", post(approve_action))
        .route("/v1/actions/{id}/dismiss", post(dismiss_action))
        .route("/v1/actions/{id}/send", post(send_action))
        .route("/v1/agents/email-sending", axum::routing::put(set_email_sending))
        .route("/v1/zoom/meeting", post(create_zoom_meeting))
        .route("/v1/zoom/meetings", get(list_zoom_meetings))
        .route("/v1/zoom/meetings/{id}/reschedule", post(reschedule_zoom_meeting))
        .route("/v1/zoom/meetings/{id}/cancel", post(cancel_zoom_meeting))
        .route("/v1/mail/send", post(send_mail).layer(axum::extract::DefaultBodyLimit::max(64 * 1024)))
        // A report is longer than the small limit the rest of the API keeps requests to.
        .route("/v1/drive/save", post(save_to_drive).layer(axum::extract::DefaultBodyLimit::max(1_000_000)))
        .route("/v1/tasks/{id}/actions", get(task_actions))
        .route("/v1/tasks/expiring", get(expiring))
        .route("/v1/tasks/{id}/keep", post(keep))
        .route("/v1/tasks/{id}/let-expire", post(let_expire))
        .route("/v1/tasks/{id}/delete", post(delete_report))
        .route("/v1/agents/retention", axum::routing::put(set_retention))
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
        "retention_days": crate::retention::policy(&tx).await?,
        "email_sending_enabled": email_sending_enabled(&tx).await?,
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
    tx.audit(&principal.user, "agent_task_cancelled", &json!({ "task": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn expiring(State(state): State<AppState>, Authed(principal): Authed) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({ "tasks": crate::retention::expiring(&tx, &principal).await?, "notice_days": crate::retention::NOTICE_DAYS })))
}

async fn keep(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !crate::retention::keep(&tx, &principal, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.audit(&principal.user, "agent_task_kept", &json!({ "task": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn let_expire(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !crate::retention::let_expire(&tx, &principal, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

async fn delete_report(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !crate::retention::delete_now(&tx, &principal, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.audit(&principal.user, "agent_task_deleted", &json!({ "task": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

#[derive(Deserialize)]
struct RetentionRequest {
    days: i32,
}

async fn set_retention(State(state): State<AppState>, Authed(principal): Authed, Json(request): Json<RetentionRequest>) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    if !crate::retention::CHOICES.contains(&request.days) {
        return Err(ApiError::BadRequest("choose 30, 90, 180 or 365 days, or keep until deleted"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    crate::retention::set_policy(&tx, request.days).await?;
    tx.audit(&principal.user, "retention_policy_changed", &json!({ "days": request.days })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "days": request.days })))
}

// ---- Emails and meetings an agent prepared for the person to send or book themselves ----

async fn waiting_actions(State(state): State<AppState>, Authed(principal): Authed) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({ "actions": crate::actions::waiting(&tx, &principal).await? })))
}

async fn task_actions(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    Ok(Json(json!({ "actions": crate::actions::for_task(&tx, &principal, id).await? })))
}

#[derive(Deserialize)]
struct ApproveRequest {
    /// What to use instead of what the agent wrote, if the person changed it.
    payload: Option<crate::actions::Payload>,
}

/// Approves what the agent prepared, with the person's changes. This does not send or book
/// anything: the desktop app opens their own mail or calendar with it, and they send it.
async fn approve_action(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<ApproveRequest>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    match crate::actions::approve(&tx, &principal, id, request.payload.as_ref()).await? {
        crate::actions::Decided::NotFound => Err(ApiError::NotFound),
        crate::actions::Decided::Invalid(reason) => Err(ApiError::BadRequest(reason)),
        crate::actions::Decided::Done(payload) => {
            // What kind and how many people, never the text.
            tx.audit(&principal.user, "agent_action_approved", &json!({ "action": id, "kind": payload.kind() })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "payload": payload })))
        }
    }
}

async fn dismiss_action(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !crate::actions::dismiss(&tx, &principal, id).await? {
        return Err(ApiError::NotFound);
    }
    tx.audit(&principal.user, "agent_action_dismissed", &json!({ "action": id })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "ok": true })))
}

// ---- Sending an email an agent prepared, from the person's own Outlook ----

/// The most emails one person may send this way in a day.
const MAX_SENT_PER_DAY: i64 = 20;

async fn email_sending_enabled(tx: &crate::db::TenantTx<'_>) -> crate::Result<bool> {
    Ok(tx.sql().query_opt("SELECT allow_email_sending FROM firm_settings", &[]).await?.is_some_and(|row| row.get(0)))
}

#[derive(Deserialize)]
struct EmailSendingRequest {
    enabled: bool,
}

/// An administrator decides whether emails prepared by helpers may be sent from the app at all.
/// Off until they say so.
async fn set_email_sending(State(state): State<AppState>, Authed(principal): Authed, Json(request): Json<EmailSendingRequest>) -> Result<Json<Value>, ApiError> {
    require_admin(&principal)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    tx.sql()
        .execute(
            "INSERT INTO firm_settings (tenant_id, allow_email_sending) VALUES (app_tenant(), $1)
             ON CONFLICT (tenant_id) DO UPDATE SET allow_email_sending = EXCLUDED.allow_email_sending",
            &[&request.enabled],
        )
        .await
        .map_err(ApiError::internal)?;
    tx.audit(&principal.user, "email_sending_switched", &json!({ "enabled": request.enabled })).await?;
    tx.commit().await?;
    Ok(Json(json!({ "enabled": request.enabled })))
}

#[derive(Deserialize)]
struct SendRequest {
    /// What the person changed it to, if anything. What is sent is this, exactly.
    payload: Option<crate::actions::Payload>,
    /// They have looked at the people outside their organisation and still want it sent.
    #[serde(default)]
    confirm_outside: bool,
}

/// Sends an email an agent prepared, from the person's own connected Outlook, after they have read it.
/// It is sent at most once, only for the person it belongs to, and only if their firm allows it.
async fn send_action(
    State(state): State<AppState>,
    Authed(principal): Authed,
    Path(id): Path<Uuid>,
    Json(request): Json<SendRequest>,
) -> Result<Json<Value>, ApiError> {
    use crate::{actions::ToSend, connectors::RefreshingToken, mail, oauth::Kind, secrets::refresh_token_name};
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !email_sending_enabled(&tx).await? {
        return Err(ApiError::BadRequest("sending email from the app is switched off for your firm: ask an administrator"));
    }
    // The person's own Outlook, connected for sending.
    let mailbox = tx
        .sql()
        .query_opt(
            "SELECT id, status, label FROM connections WHERE kind = 'outlook_mail' AND owner = $1 AND visibility = 'private'",
            &[&principal.user],
        )
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::BadRequest("connect your Outlook first"))?;
    let (mailbox_id, status, from): (Uuid, String, Option<String>) = (mailbox.get(0), mailbox.get(1), mailbox.get(2));
    if status != "active" {
        return Err(ApiError::BadRequest("reconnect your Outlook first"));
    }
    let sent_today: i64 = tx
        .sql()
        .query_one("SELECT count(*) FROM agent_actions WHERE requested_by = $1 AND status = 'sent' AND sent_at > now() - interval '1 day'", &[&principal.user])
        .await
        .map_err(ApiError::internal)?
        .get(0);
    if sent_today >= MAX_SENT_PER_DAY {
        return Err(ApiError::BadRequest("you have reached the limit of 20 emails sent from the app in a day"));
    }
    // Held until we finish, so a second press waits and then finds it already sent.
    let ready = match crate::actions::lock_for_sending(&tx, &principal, id, request.payload.as_ref()).await? {
        ToSend::Ready(ready) => ready,
        ToSend::NotFound => return Err(ApiError::NotFound),
        ToSend::Invalid(reason) => return Err(ApiError::BadRequest(reason)),
    };
    if !ready.outside.is_empty() && !request.confirm_outside {
        return Err(ApiError::BadRequest("confirm that you have checked the people outside your organisation"));
    }
    let provider = state.oauth.provider(Kind::OutlookMail).ok_or(ApiError::Unavailable("sending is not set up on this server"))?;
    let refresh = state
        .secrets
        .get(principal.tenant, &refresh_token_name(principal.tenant, mailbox_id))
        .await?
        .ok_or(ApiError::BadRequest("reconnect your Outlook first"))?;
    let refreshing = RefreshingToken::new(provider.token_url.clone(), provider.client_id.clone(), provider.client_secret.clone(), refresh).map_err(ApiError::internal)?;
    let rotated = refreshing.rotated_token();
    let outcome = match crate::connectors::TokenSource::token(&refreshing).await {
        Ok(access) => {
            let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(ApiError::internal)?;
            mail::send(&http, &provider.api_base, &access, &ready.email).await
        }
        Err(crate::connectors::ConnectorError::Unauthorized) => Err(mail::SendError::Reconnect),
        Err(_) => Err(mail::SendError::Unavailable),
    };
    // Microsoft may replace the refresh token as it is used; keep the new one.
    let replaced = rotated.lock().unwrap().take();
    if let Some(new) = replaced {
        let _ = state.secrets.put(principal.tenant, &refresh_token_name(principal.tenant, mailbox_id), &new).await;
    }
    match outcome {
        Ok(()) => {
            crate::actions::mark_sent(&tx, id, &ready, from.as_deref().unwrap_or("")).await?;
            // That it was sent, to how many, and whether it was edited: never what it said.
            tx.audit(
                &principal.user,
                "agent_action_sent",
                &json!({ "action": id, "recipients": ready.email_recipients(), "edited": ready.edited, "outside": ready.outside.len() }),
            )
            .await?;
            tx.commit().await?;
            Ok(Json(json!({ "sent": true, "from": from })))
        }
        Err(mail::SendError::Reconnect) => {
            tx.sql()
                .execute("UPDATE connections SET status = 'needs_reauth', last_error = 'Sign in to Outlook again to keep sending' WHERE id = $1", &[&mailbox_id])
                .await
                .map_err(ApiError::internal)?;
            tx.commit().await?;
            Err(ApiError::BadRequest("Outlook needs you to sign in again: reconnect it, then try again"))
        }
        Err(mail::SendError::Refused) => Err(ApiError::BadRequest("Outlook did not accept that message")),
        Err(mail::SendError::Unavailable) => Err(ApiError::Unavailable("could not reach Outlook: nothing was sent, try again")),
    }
}


// ---- Saving a file the person approved into their own Google Drive, through the Drive API ----

/// The most files one person may save this way in a day.
const MAX_SAVED_PER_DAY: i64 = 30;

#[derive(Deserialize)]
struct SaveRequest {
    title: String,
    content: String,
    /// The person has read exactly this text and approved saving it. The app sends it only from its
    /// approval box; a request without it is refused.
    #[serde(default)]
    approved: bool,
}

/// Saves a file the person wrote and approved into their own Google Drive, through the Drive API.
/// What is saved is exactly the text in this request. It goes into the Drive of the signed-in
/// person, using the Google Drive connection they made themselves: never someone else's.
async fn save_to_drive(State(state): State<AppState>, Authed(principal): Authed, Json(request): Json<SaveRequest>) -> Result<Json<Value>, ApiError> {
    use crate::{connectors::RefreshingToken, drive_save, oauth::Kind, secrets::refresh_token_name};
    if !request.approved {
        return Err(ApiError::BadRequest("approve the file before it is saved"));
    }
    let title = request.title.replace(['\r', '\n', '\0'], " ").trim().to_string();
    let content = request.content.replace('\0', "").replace("\r\n", "\n");
    if title.is_empty() || title.chars().count() > 200 {
        return Err(ApiError::BadRequest("the file name must be 1 to 200 characters"));
    }
    if content.trim().is_empty() || content.chars().count() > drive_save::MAX_CONTENT_CHARS {
        return Err(ApiError::BadRequest("the file must have some text, and no more than 200,000 characters"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    // The person's own Google Drive connection: one connected by someone else is never used.
    let drive = tx
        .sql()
        .query_opt("SELECT id, status FROM connections WHERE kind = 'google_drive' AND owner = $1 ORDER BY (status = 'active') DESC, last_sync_at DESC NULLS LAST LIMIT 1", &[&principal.user])
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::BadRequest("connect your own Google Drive first: open Connectors, Google Drive"))?;
    let (drive_id, status): (Uuid, String) = (drive.get(0), drive.get(1));
    if status != "active" {
        return Err(ApiError::BadRequest("reconnect Google Drive first: open Connectors, Google Drive"));
    }
    let saved_today: i64 = tx
        .sql()
        .query_one("SELECT count(*) FROM audit_events WHERE actor = $1 AND action = 'drive_file_saved' AND at > now() - interval '1 day'", &[&principal.user])
        .await
        .map_err(ApiError::internal)?
        .get(0);
    if saved_today >= MAX_SAVED_PER_DAY {
        return Err(ApiError::BadRequest("you have reached the limit of 30 files saved to Drive from the app in a day"));
    }
    let provider = state.oauth.provider(Kind::GoogleDrive).ok_or(ApiError::Unavailable("Google Drive is not set up on this server"))?;
    let refresh = state
        .secrets
        .get(principal.tenant, &refresh_token_name(principal.tenant, drive_id))
        .await?
        .ok_or(ApiError::BadRequest("reconnect Google Drive first: open Connectors, Google Drive"))?;
    let refreshing = RefreshingToken::new(provider.token_url.clone(), provider.client_id.clone(), provider.client_secret.clone(), refresh).map_err(ApiError::internal)?;
    let rotated = refreshing.rotated_token();
    let outcome = match crate::connectors::TokenSource::token(&refreshing).await {
        Ok(access) => {
            let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(60)).build().map_err(ApiError::internal)?;
            drive_save::create_file(&http, &provider.api_base, &access, &title, &content).await
        }
        Err(crate::connectors::ConnectorError::Unauthorized) => Err(drive_save::SaveError::Reconnect),
        Err(_) => Err(drive_save::SaveError::Unavailable),
    };
    let replaced = rotated.lock().unwrap().take();
    if let Some(new) = replaced {
        let _ = state.secrets.put(principal.tenant, &refresh_token_name(principal.tenant, drive_id), &new).await;
    }
    match outcome {
        Ok(created) => {
            // That a file was saved, and how long it was: never its name or text.
            tx.audit(&principal.user, "drive_file_saved", &json!({ "characters": content.chars().count() })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "saved": true, "link": created.link })))
        }
        // Drive refused the permission: a connection made before saving was offered has only the
        // reading permission. Reading keeps working, so the connection is not marked as broken.
        Err(drive_save::SaveError::Reconnect) => Err(ApiError::BadRequest("Google Drive did not allow saving: reconnect it (Connectors, Google Drive), then try again")),
        Err(drive_save::SaveError::Refused) => Err(ApiError::BadRequest("Google Drive did not accept that file")),
        Err(drive_save::SaveError::Unavailable) => Err(ApiError::Unavailable("could not reach Google Drive: nothing was saved, try again")),
    }
}


// ---- An email the person wrote or drafted in the chat, sent from their own Outlook ----

#[derive(Deserialize)]
struct MailRequest {
    to: Vec<String>,
    #[serde(default)]
    cc: Vec<String>,
    subject: String,
    body: String,
    /// The person has read exactly this message and pressed Send. The app sends it only from its
    /// preview; a request without it is refused.
    #[serde(default)]
    approved: bool,
    /// They have looked at the people outside their organisation and still want it sent.
    #[serde(default)]
    confirm_outside: bool,
}

/// Sends an email the person has just read, from their own connected Outlook, to the people they
/// chose. It is sent only for the signed-in person, only if their firm allows sending from the app,
/// and a message to people outside their organisation needs their say-so.
async fn send_mail(State(state): State<AppState>, Authed(principal): Authed, Json(request): Json<MailRequest>) -> Result<Json<Value>, ApiError> {
    use crate::{actions::Payload, connectors::RefreshingToken, mail, oauth::Kind, secrets::refresh_token_name};
    if !request.approved {
        return Err(ApiError::BadRequest("approve the email before it is sent"));
    }
    let email = Payload::Email { to: request.to, cc: request.cc, subject: request.subject, body: request.body }
        .cleaned()
        .map_err(ApiError::BadRequest)?;
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if !email_sending_enabled(&tx).await? {
        return Err(ApiError::BadRequest("sending email from the app is switched off for your firm: ask an administrator"));
    }
    let mailbox = tx
        .sql()
        .query_opt("SELECT id, status, label FROM connections WHERE kind = 'outlook_mail' AND owner = $1 AND visibility = 'private'", &[&principal.user])
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::BadRequest("connect your Outlook first: open Connectors, Outlook email"))?;
    let (mailbox_id, status, from): (Uuid, String, Option<String>) = (mailbox.get(0), mailbox.get(1), mailbox.get(2));
    if status != "active" {
        return Err(ApiError::BadRequest("reconnect your Outlook first: open Connectors, Outlook email"));
    }
    // Emails from helpers and emails from the chat count together toward the day's limit.
    let sent_today: i64 = tx
        .sql()
        .query_one(
            "SELECT (SELECT count(*) FROM agent_actions WHERE requested_by = $1 AND status = 'sent' AND sent_at > now() - interval '1 day')
                  + (SELECT count(*) FROM audit_events WHERE actor = $1 AND action = 'chat_email_sent' AND at > now() - interval '1 day')",
            &[&principal.user],
        )
        .await
        .map_err(ApiError::internal)?
        .get(0);
    if sent_today >= MAX_SENT_PER_DAY {
        return Err(ApiError::BadRequest("you have reached the limit of 20 emails sent from the app in a day"));
    }
    let outside = crate::actions::outside(&email, principal.email.as_deref());
    if !outside.is_empty() && !request.confirm_outside {
        return Err(ApiError::BadRequest("confirm that you have checked the people outside your organisation"));
    }
    let provider = state.oauth.provider(Kind::OutlookMail).ok_or(ApiError::Unavailable("sending is not set up on this server"))?;
    let refresh = state
        .secrets
        .get(principal.tenant, &refresh_token_name(principal.tenant, mailbox_id))
        .await?
        .ok_or(ApiError::BadRequest("reconnect your Outlook first: open Connectors, Outlook email"))?;
    let refreshing = RefreshingToken::new(provider.token_url.clone(), provider.client_id.clone(), provider.client_secret.clone(), refresh).map_err(ApiError::internal)?;
    let rotated = refreshing.rotated_token();
    let outcome = match crate::connectors::TokenSource::token(&refreshing).await {
        Ok(access) => {
            let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(ApiError::internal)?;
            mail::send(&http, &provider.api_base, &access, &email).await
        }
        Err(crate::connectors::ConnectorError::Unauthorized) => Err(mail::SendError::Reconnect),
        Err(_) => Err(mail::SendError::Unavailable),
    };
    let replaced = rotated.lock().unwrap().take();
    if let Some(new) = replaced {
        let _ = state.secrets.put(principal.tenant, &refresh_token_name(principal.tenant, mailbox_id), &new).await;
    }
    match outcome {
        Ok(()) => {
            // That an email went out and to how many people: never what it said.
            let recipients = match &email {
                Payload::Email { to, cc, .. } => to.len() + cc.len(),
                _ => 0,
            };
            tx.audit(&principal.user, "chat_email_sent", &json!({ "recipients": recipients, "outside": outside.len() })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "sent": true, "from": from })))
        }
        Err(mail::SendError::Reconnect) => {
            tx.sql()
                .execute("UPDATE connections SET status = 'needs_reauth', last_error = 'Sign in to Outlook again to keep sending' WHERE id = $1", &[&mailbox_id])
                .await
                .map_err(ApiError::internal)?;
            tx.commit().await?;
            Err(ApiError::BadRequest("Outlook needs you to sign in again: reconnect it, then try again"))
        }
        Err(mail::SendError::Refused) => Err(ApiError::BadRequest("Outlook did not accept that message")),
        Err(mail::SendError::Unavailable) => Err(ApiError::Unavailable("could not reach Outlook: nothing was sent, try again")),
    }
}


// ---- A Zoom meeting the person approved, created in their own Zoom account ----

/// The most meetings one person may create this way in a day.
const MAX_MEETINGS_PER_DAY: i64 = 20;
/// The most changes (moves and cancellations) one person may make in a day.
const MAX_CHANGES_PER_DAY: i64 = 50;

#[derive(Deserialize)]
struct MeetingRequest {
    topic: String,
    /// When it starts, in the person's own time zone: "2026-11-10T10:00".
    start: String,
    #[serde(default = "thirty")]
    duration_minutes: u32,
    /// The time zone `start` is in, as Zoom names them ("America/Chicago").
    timezone: String,
    #[serde(default)]
    agenda: String,
    /// The person has read the meeting and chose to create it.
    #[serde(default)]
    approved: bool,
}

fn thirty() -> u32 {
    30
}

/// The meeting's start as Zoom wants it, or None if it is not a real date and time.
fn zoom_start(text: &str) -> Option<String> {
    ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"]
        .iter()
        .find_map(|format| chrono::NaiveDateTime::parse_from_str(text.trim(), format).ok())
        .map(|time| time.format("%Y-%m-%dT%H:%M:%S").to_string())
}

/// A time zone name: letters, digits and the few marks they use, nothing else.
fn zone_ok(zone: &str) -> bool {
    !zone.is_empty() && zone.len() <= 64 && zone.chars().all(|c| c.is_ascii_alphanumeric() || "/_+-".contains(c))
}

/// How Zoom answered a call made for the person.
enum ZoomReply {
    /// It worked; the body is empty for a change or a cancellation.
    Done(Value),
    /// The meeting is no longer in Zoom.
    Gone,
    /// The sign-in has lapsed.
    Reconnect,
    /// Zoom did not accept it.
    Refused,
    Unavailable,
}

/// Makes one call to the signed-in person's own Zoom, with their own sign-in, and keeps the new
/// refresh token Zoom hands back. Returns the connection used too. Nothing is committed here.
async fn zoom_call(
    state: &AppState,
    tx: &crate::db::TenantTx<'_>,
    principal: &crate::auth::Principal,
    method: reqwest::Method,
    path: &str,
    body: Option<Value>,
) -> Result<(Uuid, ZoomReply), ApiError> {
    use crate::{connectors::RefreshingToken, oauth::Kind, secrets::refresh_token_name};
    let zoom = tx
        .sql()
        .query_opt("SELECT id, status FROM connections WHERE kind = 'zoom' AND owner = $1 AND visibility = 'private'", &[&principal.user])
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::BadRequest("connect your Zoom first: open Connectors, Zoom"))?;
    let (zoom_id, status): (Uuid, String) = (zoom.get(0), zoom.get(1));
    if status != "active" {
        return Err(ApiError::BadRequest("reconnect your Zoom first: open Connectors, Zoom"));
    }
    let provider = state.oauth.provider(Kind::Zoom).ok_or(ApiError::Unavailable("Zoom is not set up on this server"))?;
    let refresh = state
        .secrets
        .get(principal.tenant, &refresh_token_name(principal.tenant, zoom_id))
        .await?
        .ok_or(ApiError::BadRequest("reconnect your Zoom first: open Connectors, Zoom"))?;
    let refreshing = RefreshingToken::new(provider.token_url.clone(), provider.client_id.clone(), provider.client_secret.clone(), refresh)
        .map_err(ApiError::internal)?
        .with_basic_auth(provider.basic_auth);
    let rotated = refreshing.rotated_token();
    let reply = match crate::connectors::TokenSource::token(&refreshing).await {
        Ok(access) => {
            let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(30)).build().map_err(ApiError::internal)?;
            let mut request = http.request(method, format!("{}{}", provider.api_base.trim_end_matches('/'), path)).bearer_auth(&access);
            if let Some(body) = &body {
                request = request.json(body);
            }
            match request.send().await {
                Ok(response) => match response.status().as_u16() {
                    204 => ZoomReply::Done(Value::Null),
                    200..=299 => response.json::<Value>().await.map(ZoomReply::Done).unwrap_or(ZoomReply::Unavailable),
                    401 | 403 => ZoomReply::Reconnect,
                    404 | 410 => ZoomReply::Gone,
                    400 | 422 => ZoomReply::Refused,
                    _ => ZoomReply::Unavailable,
                },
                Err(_) => ZoomReply::Unavailable,
            }
        }
        Err(crate::connectors::ConnectorError::Unauthorized) => ZoomReply::Reconnect,
        Err(_) => ZoomReply::Unavailable,
    };
    // Zoom replaces the refresh token each time it is used; the new one is kept.
    let replaced = rotated.lock().unwrap().take();
    if let Some(new) = replaced {
        let _ = state.secrets.put(principal.tenant, &refresh_token_name(principal.tenant, zoom_id), &new).await;
    }
    Ok((zoom_id, reply))
}

/// Records that Zoom needs the person to sign in again, and says so.
async fn zoom_reconnect(tx: crate::db::TenantTx<'_>, zoom_id: Uuid, doing: &str) -> ApiError {
    let marked = tx
        .sql()
        .execute("UPDATE connections SET status = 'needs_reauth', last_error = $2 WHERE id = $1", &[&zoom_id, &format!("Sign in to Zoom again to keep {doing}")])
        .await;
    if marked.is_ok() {
        let _ = tx.commit().await;
    }
    ApiError::BadRequest("Zoom needs you to sign in again: reconnect it, then try again")
}

async fn zoom_count_today(tx: &crate::db::TenantTx<'_>, user: &str, actions: &[&str]) -> Result<i64, ApiError> {
    let actions: Vec<String> = actions.iter().map(|action| action.to_string()).collect();
    Ok(tx
        .sql()
        .query_one("SELECT count(*) FROM audit_events WHERE actor = $1 AND action = ANY($2) AND at > now() - interval '1 day'", &[&user, &actions])
        .await
        .map_err(ApiError::internal)?
        .get(0))
}

/// Creates a Zoom meeting in the signed-in person's own account, after they approved it, and gives
/// back the link people join with. It never reads their meetings, recordings or contacts.
async fn create_zoom_meeting(State(state): State<AppState>, Authed(principal): Authed, Json(request): Json<MeetingRequest>) -> Result<Json<Value>, ApiError> {
    if !request.approved {
        return Err(ApiError::BadRequest("approve the meeting before it is created"));
    }
    let topic = request.topic.replace(['\r', '\n', '\0'], " ").trim().to_string();
    let agenda = request.agenda.replace('\0', "").trim().to_string();
    let start = zoom_start(&request.start).ok_or(ApiError::BadRequest("the start must look like 2026-11-10T10:00"))?;
    if topic.is_empty() || topic.chars().count() > 200 {
        return Err(ApiError::BadRequest("the meeting name must be 1 to 200 characters"));
    }
    if !(5..=480).contains(&request.duration_minutes) {
        return Err(ApiError::BadRequest("a meeting must last between 5 minutes and 8 hours"));
    }
    if !zone_ok(&request.timezone) || agenda.chars().count() > 2_000 {
        return Err(ApiError::BadRequest("the time zone or the agenda is not valid"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    if zoom_count_today(&tx, &principal.user, &["zoom_meeting_created"]).await? >= MAX_MEETINGS_PER_DAY {
        return Err(ApiError::BadRequest("you have reached the limit of 20 Zoom meetings created from the app in a day"));
    }
    let body = json!({ "topic": topic, "type": 2, "start_time": start, "duration": request.duration_minutes, "timezone": request.timezone, "agenda": agenda });
    let (zoom_id, reply) = zoom_call(&state, &tx, &principal, reqwest::Method::POST, "/v2/users/me/meetings", Some(body)).await?;
    match reply {
        ZoomReply::Done(meeting) => {
            // Only a Zoom link is passed back, never the host's start link, which carries a login token.
            let join = meeting["join_url"].as_str().filter(|url| url::Url::parse(url).is_ok_and(|parsed| parsed.scheme() == "https" && parsed.host_str().is_some_and(|host| host.ends_with("zoom.us") || host.ends_with("zoomgov.com"))));
            let Some(join) = join else { return Err(ApiError::Unavailable("Zoom did not return a link: try again")) };
            let meeting_id = meeting["id"].to_string().trim_matches('"').to_string();
            // Kept so it can be moved or cancelled later: the host's start link is not.
            tx.sql()
                .execute(
                    "INSERT INTO zoom_meetings (tenant_id, owner, zoom_id, topic, start_local, timezone, duration_minutes, join_url) VALUES (app_tenant(), $1, $2, $3, $4, $5, $6, $7)",
                    &[&principal.user, &meeting_id, &topic, &start, &request.timezone, &(request.duration_minutes as i32), &join],
                )
                .await
                .map_err(ApiError::internal)?;
            // That a meeting was created: never its name or agenda.
            tx.audit(&principal.user, "zoom_meeting_created", &json!({ "minutes": request.duration_minutes })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "join_url": join, "meeting_id": meeting_id, "passcode": meeting["password"].as_str() })))
        }
        ZoomReply::Reconnect => Err(zoom_reconnect(tx, zoom_id, "creating meetings").await),
        ZoomReply::Refused | ZoomReply::Gone => Err(ApiError::BadRequest("Zoom did not accept that meeting")),
        ZoomReply::Unavailable => Err(ApiError::Unavailable("could not reach Zoom: nothing was created, try again")),
    }
}

/// The meetings this person made from the app that are still on: the only ones the app can change.
async fn list_zoom_meetings(State(state): State<AppState>, Authed(principal): Authed) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let rows = tx
        .sql()
        .query(
            "SELECT id, topic, start_local, timezone, duration_minutes, join_url, zoom_id FROM zoom_meetings \
             WHERE owner = $1 AND status = 'scheduled' AND start_local::timestamp > (now() AT TIME ZONE 'UTC') - interval '2 days' \
             ORDER BY start_local::timestamp LIMIT 50",
            &[&principal.user],
        )
        .await
        .map_err(ApiError::internal)?;
    tx.commit().await?;
    let meetings: Vec<Value> = rows
        .iter()
        .map(|row| {
            json!({
                "id": row.get::<_, Uuid>(0), "topic": row.get::<_, String>(1), "start": row.get::<_, String>(2),
                "timezone": row.get::<_, String>(3), "duration_minutes": row.get::<_, i32>(4),
                "join_url": row.get::<_, String>(5), "meeting_id": row.get::<_, String>(6),
            })
        })
        .collect();
    Ok(Json(json!({ "meetings": meetings })))
}

#[derive(Deserialize)]
struct RescheduleRequest {
    start: String,
    duration_minutes: Option<u32>,
    timezone: String,
    #[serde(default)]
    approved: bool,
}

#[derive(Deserialize)]
struct CancelRequest {
    #[serde(default)]
    approved: bool,
}

/// One of this person's own scheduled meetings: its Zoom ID, or "not found" for anyone else's.
async fn own_meeting(tx: &crate::db::TenantTx<'_>, user: &str, id: Uuid) -> Result<(String, i32), ApiError> {
    let row = tx
        .sql()
        .query_opt("SELECT zoom_id, duration_minutes FROM zoom_meetings WHERE id = $1 AND owner = $2 AND status = 'scheduled'", &[&id, &user])
        .await
        .map_err(ApiError::internal)?
        .ok_or(ApiError::NotFound)?;
    Ok((row.get(0), row.get(1)))
}

/// Moves one of the person's own meetings to another time, after they approved it. The link stays the same.
async fn reschedule_zoom_meeting(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>, Json(request): Json<RescheduleRequest>) -> Result<Json<Value>, ApiError> {
    if !request.approved {
        return Err(ApiError::BadRequest("approve the change before the meeting is moved"));
    }
    let start = zoom_start(&request.start).ok_or(ApiError::BadRequest("the start must look like 2026-11-10T10:00"))?;
    if !zone_ok(&request.timezone) || request.duration_minutes.is_some_and(|minutes| !(5..=480).contains(&minutes)) {
        return Err(ApiError::BadRequest("the time zone or the length is not valid"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let (zoom_meeting, old_minutes) = own_meeting(&tx, &principal.user, id).await?;
    if zoom_count_today(&tx, &principal.user, &["zoom_meeting_moved", "zoom_meeting_cancelled"]).await? >= MAX_CHANGES_PER_DAY {
        return Err(ApiError::BadRequest("you have reached the limit of 50 Zoom changes made from the app in a day"));
    }
    let minutes = request.duration_minutes.map(|m| m as i32).unwrap_or(old_minutes);
    let body = json!({ "start_time": start, "timezone": request.timezone, "duration": minutes });
    let (zoom_id, reply) = zoom_call(&state, &tx, &principal, reqwest::Method::PATCH, &format!("/v2/meetings/{zoom_meeting}"), Some(body)).await?;
    match reply {
        ZoomReply::Done(_) => {
            tx.sql()
                .execute("UPDATE zoom_meetings SET start_local = $2, timezone = $3, duration_minutes = $4, changed_at = now() WHERE id = $1", &[&id, &start, &request.timezone, &minutes])
                .await
                .map_err(ApiError::internal)?;
            tx.audit(&principal.user, "zoom_meeting_moved", &json!({ "minutes": minutes })).await?;
            tx.commit().await?;
            Ok(Json(json!({ "start": start, "timezone": request.timezone, "duration_minutes": minutes })))
        }
        ZoomReply::Gone => {
            tx.sql().execute("UPDATE zoom_meetings SET status = 'cancelled', changed_at = now() WHERE id = $1", &[&id]).await.map_err(ApiError::internal)?;
            tx.commit().await?;
            Err(ApiError::BadRequest("that meeting is no longer in your Zoom"))
        }
        ZoomReply::Reconnect => Err(zoom_reconnect(tx, zoom_id, "changing meetings").await),
        ZoomReply::Refused => Err(ApiError::BadRequest("Zoom did not accept that change")),
        ZoomReply::Unavailable => Err(ApiError::Unavailable("could not reach Zoom: nothing was changed, try again")),
    }
}

/// Cancels one of the person's own meetings, after they approved it.
async fn cancel_zoom_meeting(State(state): State<AppState>, Authed(principal): Authed, Path(id): Path<Uuid>, Json(request): Json<CancelRequest>) -> Result<Json<Value>, ApiError> {
    if !request.approved {
        return Err(ApiError::BadRequest("approve the cancellation before the meeting is cancelled"));
    }
    let mut conn = state.pool.get().await.map_err(ApiError::internal)?;
    let tx = open(&mut conn, &principal).await?;
    let (zoom_meeting, _) = own_meeting(&tx, &principal.user, id).await?;
    if zoom_count_today(&tx, &principal.user, &["zoom_meeting_moved", "zoom_meeting_cancelled"]).await? >= MAX_CHANGES_PER_DAY {
        return Err(ApiError::BadRequest("you have reached the limit of 50 Zoom changes made from the app in a day"));
    }
    let (zoom_id, reply) = zoom_call(&state, &tx, &principal, reqwest::Method::DELETE, &format!("/v2/meetings/{zoom_meeting}"), None).await?;
    match reply {
        // Already gone from Zoom counts as cancelled.
        ZoomReply::Done(_) | ZoomReply::Gone => {
            tx.sql().execute("UPDATE zoom_meetings SET status = 'cancelled', changed_at = now() WHERE id = $1", &[&id]).await.map_err(ApiError::internal)?;
            tx.audit(&principal.user, "zoom_meeting_cancelled", &json!({})).await?;
            tx.commit().await?;
            Ok(Json(json!({ "cancelled": true })))
        }
        ZoomReply::Reconnect => Err(zoom_reconnect(tx, zoom_id, "changing meetings").await),
        ZoomReply::Refused => Err(ApiError::BadRequest("Zoom did not accept that cancellation")),
        ZoomReply::Unavailable => Err(ApiError::Unavailable("could not reach Zoom: nothing was cancelled, try again")),
    }
}
