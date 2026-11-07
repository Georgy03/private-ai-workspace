//! Shared AI workers. An agent is a saved, versioned configuration that a firm administrator
//! creates and offers to employees. Each job an employee gives it is a separate task with its own
//! conversation, so employees share an agent without ever sharing a context, and a task runs with
//! the permissions of the person who started it.

use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::{Result, auth::Principal, db::TenantTx};

/// The tools an agent may be given. All of them only read.
/// What an agent may be given. The first three only read. The last two only prepare something for
/// the person to review and send or book themselves: an agent can never send or book anything.
pub const TOOLS: &[&str] = &["search_documents", "read_document", "lookup_knowledge", "prepare_email", "prepare_meeting"];
pub const EFFORTS: &[&str] = &["low", "medium", "high"];
/// "standard" is the everyday model; "advanced" is the stronger, dearer one.
pub const MODELS: &[&str] = &["standard", "advanced"];
pub const MAX_INSTRUCTIONS_CHARS: usize = 8_000;
pub const MAX_TASK_CHARS: usize = 4_000;
const MAX_NAME_CHARS: usize = 60;

/// The part of an agent that changes between versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSpec {
    pub name: String,
    pub role: String,
    pub description: String,
    pub icon: String,
    pub color: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub model: String,
    pub effort: String,
}

impl AgentSpec {
    /// Why this cannot be saved, if it cannot.
    pub fn problem(&self) -> Option<&'static str> {
        let name = self.name.trim().chars().count();
        if name == 0 || name > MAX_NAME_CHARS {
            return Some("the name must be 1 to 60 characters");
        }
        if self.role.chars().count() > 80 || self.description.chars().count() > 400 {
            return Some("the role or description is too long");
        }
        let instructions = self.instructions.trim().chars().count();
        if instructions == 0 || instructions > MAX_INSTRUCTIONS_CHARS {
            return Some("the instructions must be 1 to 8,000 characters");
        }
        if !self.tools.iter().all(|tool| TOOLS.contains(&tool.as_str())) {
            return Some("unknown tool");
        }
        if !MODELS.contains(&self.model.as_str()) || !EFFORTS.contains(&self.effort.as_str()) {
            return Some("unknown model or effort");
        }
        if self.icon.len() > 40 || self.color.len() > 40 {
            return Some("unknown appearance");
        }
        None
    }
}

/// Who may use an agent, how much it may spend and how much it may do at once.
#[derive(Debug, Clone, Default)]
pub struct AgentSettings {
    pub status: Option<String>,
    pub audience: Option<Vec<String>>,
    /// `Some(None)` removes the limit.
    pub monthly_budget_micros: Option<Option<i64>>,
    pub max_concurrent: Option<i32>,
}

impl AgentSettings {
    pub fn problem(&self) -> Option<&'static str> {
        if self.status.as_deref().is_some_and(|s| !["available", "paused", "archived"].contains(&s)) {
            return Some("unknown status");
        }
        if let Some(audience) = &self.audience
            && (audience.is_empty() || audience.len() > 50 || audience.iter().any(|a| a.is_empty() || a.len() > 200))
        {
            return Some("the audience must name 1 to 50 people or groups, or everyone");
        }
        if self.monthly_budget_micros.flatten().is_some_and(|b| !(0..=100_000_000_000).contains(&b)) {
            return Some("the budget is out of range");
        }
        if self.max_concurrent.is_some_and(|n| !(1..=20).contains(&n)) {
            return Some("tasks at once must be between 1 and 20");
        }
        None
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentView {
    pub id: Uuid,
    pub name: String,
    pub role: String,
    pub description: String,
    pub icon: String,
    pub color: String,
    pub status: String,
    pub version: i32,
    pub tools: Vec<String>,
    pub model: String,
    pub effort: String,
    pub audience: Vec<String>,
    /// Instructions are for administrators; employees see what an agent is for, not how it is told.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub monthly_budget_micros: Option<i64>,
    pub spent_this_month_micros: i64,
    pub max_concurrent: i32,
    pub running_now: i64,
}

const AGENT_COLUMNS: &str = "a.id, v.name, v.role, v.description, v.icon, v.color, a.status, a.current_version,
        v.tools, v.model, v.effort, a.audience, v.instructions, a.monthly_budget_micros, a.max_concurrent,
        (SELECT COALESCE(sum(t.cost_micros), 0)::bigint FROM agent_tasks t
          WHERE t.agent_id = a.id AND t.created_at >= date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'),
        (SELECT count(*) FROM agent_tasks t JOIN agent_queue q ON q.task_id = t.id
          WHERE t.agent_id = a.id AND t.status = 'running' AND q.claimed_until > now())";
const AGENT_FROM: &str = "FROM agents a JOIN agent_versions v
        ON v.tenant_id = a.tenant_id AND v.agent_id = a.id AND v.version = a.current_version";

fn agent_view(row: &tokio_postgres::Row, with_instructions: bool) -> AgentView {
    AgentView {
        id: row.get(0),
        name: row.get(1),
        role: row.get(2),
        description: row.get(3),
        icon: row.get(4),
        color: row.get(5),
        status: row.get(6),
        version: row.get(7),
        tools: row.get(8),
        model: row.get(9),
        effort: row.get(10),
        audience: row.get(11),
        instructions: with_instructions.then(|| row.get(12)),
        monthly_budget_micros: row.get(13),
        max_concurrent: row.get(14),
        spent_this_month_micros: row.get(15),
        running_now: row.get(16),
    }
}

/// Whether a person may give an agent work.
pub(crate) fn allowed(audience: &[String], principal: &Principal) -> bool {
    let readers = principal.readers();
    audience.iter().any(|entry| entry == "everyone" || readers.contains(entry))
}

pub async fn create_agent(
    tx: &TenantTx<'_>,
    by: &str,
    spec: &AgentSpec,
    settings: &AgentSettings,
) -> Result<Uuid> {
    let sql = tx.sql();
    let audience = settings.audience.clone().unwrap_or_else(|| vec!["everyone".into()]);
    let budget = settings.monthly_budget_micros.flatten();
    let max = settings.max_concurrent.unwrap_or(3);
    let id: Uuid = sql
        .query_one(
            "INSERT INTO agents (tenant_id, audience, monthly_budget_micros, max_concurrent, created_by)
             VALUES (app_tenant(), $1, $2, $3, $4) RETURNING id",
            &[&audience, &budget, &max, &by],
        )
        .await?
        .get(0);
    insert_version(tx, id, 1, spec, by, "First version").await?;
    Ok(id)
}

async fn insert_version(tx: &TenantTx<'_>, agent: Uuid, version: i32, spec: &AgentSpec, by: &str, note: &str) -> Result<()> {
    tx.sql()
        .execute(
            "INSERT INTO agent_versions (tenant_id, agent_id, version, name, role, description, icon, color,
                                         instructions, tools, model, effort, note, created_by)
             VALUES (app_tenant(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)",
            &[
                &agent, &version, &spec.name.trim(), &spec.role.trim(), &spec.description.trim(), &spec.icon,
                &spec.color, &spec.instructions.trim(), &spec.tools, &spec.model, &spec.effort, &note, &by,
            ],
        )
        .await?;
    Ok(())
}

/// Saves a new version if the spec differs from the current one, and applies the settings.
/// Returns false when there is no such agent.
pub async fn update_agent(
    tx: &TenantTx<'_>,
    id: Uuid,
    by: &str,
    spec: Option<&AgentSpec>,
    settings: &AgentSettings,
    note: &str,
) -> Result<bool> {
    let sql = tx.sql();
    let Some(current) = sql.query_opt("SELECT current_version FROM agents WHERE id = $1 FOR UPDATE", &[&id]).await? else {
        return Ok(false);
    };
    let mut version: i32 = current.get(0);
    if let Some(spec) = spec
        && current_spec(tx, id).await?.as_ref() != Some(spec)
    {
        let next: i32 = sql
            .query_one("SELECT COALESCE(max(version), 0) + 1 FROM agent_versions WHERE agent_id = $1", &[&id])
            .await?
            .get(0);
        insert_version(tx, id, next, spec, by, note).await?;
        version = next;
    }
    sql.execute(
        "UPDATE agents SET current_version = $2,
            status = COALESCE($3, status), audience = COALESCE($4, audience),
            monthly_budget_micros = CASE WHEN $5 THEN $6 ELSE monthly_budget_micros END,
            max_concurrent = COALESCE($7, max_concurrent)
         WHERE id = $1",
        &[
            &id,
            &version,
            &settings.status,
            &settings.audience,
            &settings.monthly_budget_micros.is_some(),
            &settings.monthly_budget_micros.flatten(),
            &settings.max_concurrent,
        ],
    )
    .await?;
    Ok(true)
}

async fn current_spec(tx: &TenantTx<'_>, id: Uuid) -> Result<Option<AgentSpec>> {
    Ok(tx
        .sql()
        .query_opt(
            &format!("SELECT v.name, v.role, v.description, v.icon, v.color, v.instructions, v.tools, v.model, v.effort {AGENT_FROM} WHERE a.id = $1"),
            &[&id],
        )
        .await?
        .map(|row| spec_from(&row)))
}

fn spec_from(row: &tokio_postgres::Row) -> AgentSpec {
    AgentSpec {
        name: row.get(0),
        role: row.get(1),
        description: row.get(2),
        icon: row.get(3),
        color: row.get(4),
        instructions: row.get(5),
        tools: row.get(6),
        model: row.get(7),
        effort: row.get(8),
    }
}

/// Goes back to an earlier version by saving a copy of it as the newest, so the history only ever
/// grows and a rollback can itself be rolled back.
pub async fn rollback(tx: &TenantTx<'_>, id: Uuid, by: &str, to_version: i32) -> Result<Option<i32>> {
    let sql = tx.sql();
    let Some(row) = sql
        .query_opt(
            "SELECT name, role, description, icon, color, instructions, tools, model, effort
             FROM agent_versions WHERE agent_id = $1 AND version = $2",
            &[&id, &to_version],
        )
        .await?
    else {
        return Ok(None);
    };
    let spec = spec_from(&row);
    let next: i32 = sql
        .query_one("SELECT COALESCE(max(version), 0) + 1 FROM agent_versions WHERE agent_id = $1", &[&id])
        .await?
        .get(0);
    insert_version(tx, id, next, &spec, by, &format!("Rolled back to version {to_version}")).await?;
    sql.execute("UPDATE agents SET current_version = $2 WHERE id = $1", &[&id, &next]).await?;
    Ok(Some(next))
}

#[derive(Debug, Clone, Serialize)]
pub struct VersionView {
    pub version: i32,
    pub name: String,
    pub instructions: String,
    pub tools: Vec<String>,
    pub model: String,
    pub effort: String,
    pub note: String,
    pub created_by: String,
    pub created_epoch: f64,
    pub current: bool,
}

pub async fn versions(tx: &TenantTx<'_>, id: Uuid) -> Result<Vec<VersionView>> {
    Ok(tx
        .sql()
        .query(
            "SELECT v.version, v.name, v.instructions, v.tools, v.model, v.effort, v.note, v.created_by,
                    EXTRACT(EPOCH FROM v.created_at)::float8, v.version = a.current_version
             FROM agent_versions v JOIN agents a ON a.tenant_id = v.tenant_id AND a.id = v.agent_id
             WHERE v.agent_id = $1 ORDER BY v.version DESC",
            &[&id],
        )
        .await?
        .into_iter()
        .map(|row| VersionView {
            version: row.get(0),
            name: row.get(1),
            instructions: row.get(2),
            tools: row.get(3),
            model: row.get(4),
            effort: row.get(5),
            note: row.get(6),
            created_by: row.get(7),
            created_epoch: row.get(8),
            current: row.get(9),
        })
        .collect())
}

/// The agents a person may see. Administrators see every agent, including paused and archived ones
/// and their instructions; everyone else sees the available agents they are allowed to use.
pub async fn list_agents(tx: &TenantTx<'_>, principal: &Principal) -> Result<Vec<AgentView>> {
    let admin = principal.is_admin();
    let rows = tx
        .sql()
        .query(&format!("SELECT {AGENT_COLUMNS} {AGENT_FROM} ORDER BY a.created_at, v.name"), &[])
        .await?;
    Ok(rows
        .iter()
        .map(|row| (agent_view(row, admin), row))
        .filter(|(view, _)| admin || (view.status == "available" && allowed(&view.audience, principal)))
        .map(|(mut view, _)| {
            if !admin {
                // Budgets and spending are the firm's business, not the employee's.
                view.monthly_budget_micros = None;
                view.spent_this_month_micros = 0;
                view.audience = Vec::new();
            }
            view
        })
        .collect())
}

pub async fn get_agent(tx: &TenantTx<'_>, id: Uuid) -> Result<Option<AgentView>> {
    Ok(tx
        .sql()
        .query_opt(&format!("SELECT {AGENT_COLUMNS} {AGENT_FROM} WHERE a.id = $1"), &[&id])
        .await?
        .map(|row| agent_view(&row, true)))
}

// ---- Tasks ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Started {
    Queued(Uuid),
    NoSuchAgent,
    /// The agent is paused or archived, or this person is not on its audience.
    NotAvailable,
    BudgetReached,
}

/// Queues a task for an agent on behalf of a person. The task remembers who they are, so it can
/// only ever read what they may read.
pub async fn start_task(tx: &TenantTx<'_>, principal: &Principal, agent: Uuid, instruction: &str) -> Result<Started> {
    let Some(view) = get_agent(tx, agent).await? else {
        return Ok(Started::NoSuchAgent);
    };
    if view.status != "available" || !allowed(&view.audience, principal) {
        // The same answer for an agent that is not theirs to use as for one that does not exist.
        return Ok(Started::NotAvailable);
    }
    if view.monthly_budget_micros.is_some_and(|budget| view.spent_this_month_micros >= budget) {
        return Ok(Started::BudgetReached);
    }
    let title: String = instruction.trim().chars().take(80).collect();
    let sql = tx.sql();
    let id: Uuid = sql
        .query_one(
            "INSERT INTO agent_tasks (tenant_id, agent_id, agent_version, requested_by, requester_groups,
                                      requester_email, title, instruction)
             VALUES (app_tenant(), $1, $2, $3, $4, $5, $6, $7) RETURNING id",
            &[&agent, &view.version, &principal.user, &principal.groups, &principal.email, &title, &instruction.trim()],
        )
        .await?
        .get(0);
    sql.execute("INSERT INTO agent_queue (tenant_id, task_id) VALUES (app_tenant(), $1)", &[&id]).await?;
    event(tx, id, "created", &format!("{} was given the task", view.name)).await?;
    Ok(Started::Queued(id))
}

pub async fn event(tx: &TenantTx<'_>, task: Uuid, kind: &str, summary: &str) -> Result<()> {
    tx.sql()
        .execute(
            "INSERT INTO agent_task_events (tenant_id, task_id, kind, summary) VALUES (app_tenant(), $1, $2, $3)",
            &[&task, &kind, &summary.chars().take(300).collect::<String>()],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
pub struct EventView {
    pub kind: String,
    pub summary: String,
    pub epoch: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TaskView {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub agent_name: String,
    pub requested_by: String,
    pub title: String,
    pub status: String,
    pub turns: i32,
    pub cost_micros: i64,
    pub created_epoch: f64,
    pub started_epoch: Option<f64>,
    pub finished_epoch: Option<f64>,
    /// The request, the report and what it was drawn from belong to the person who asked: they can
    /// contain anything that person may read. Others, administrators included, get none of them.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub instruction: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sources: Option<Value>,
    /// How the report held up against the documents the agent read. Private like the report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checks: Option<Value>,
    /// Set for a report made by a schedule: waiting for its owner, approved or rejected.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_status: Option<String>,
    /// The owner's own corrected version of the report.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub edited_result: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_id: Option<Uuid>,
    /// When the work is due to be deleted, if it will be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletes_epoch: Option<f64>,
    /// What the owner said when asked: keep it, or let it run out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retention_choice: Option<String>,
    /// Its content has been deleted; only the dates and cost remain.
    pub deleted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<EventView>,
}

const TASK_COLUMNS: &str = "t.id, t.agent_id, v.name, t.requested_by, t.title, t.status, t.turns, t.cost_micros,
        EXTRACT(EPOCH FROM t.created_at)::float8, EXTRACT(EPOCH FROM t.started_at)::float8,
        EXTRACT(EPOCH FROM t.finished_at)::float8, t.instruction, t.result, t.sources, t.error, t.checks,
        t.review_status, t.edited_result, t.schedule_id,
        EXTRACT(EPOCH FROM t.retention_until)::float8, t.retention_choice,
        COALESCE(t.deleted_at IS NOT NULL OR t.retention_until <= now(), false)";
const TASK_FROM: &str = "FROM agent_tasks t JOIN agent_versions v
        ON v.tenant_id = t.tenant_id AND v.agent_id = t.agent_id AND v.version = t.agent_version";

fn task_view(row: &tokio_postgres::Row, own: bool) -> TaskView {
    let gone: bool = row.get(21);
    // What a task asked, found and wrote is private to its owner. Others, administrators included,
    // see that it happened, when, and what it cost: not even its title, which is the request's start.
    let title = if gone { "Deleted report".to_string() } else if own { row.get(4) } else { "Task".to_string() };
    let keep = own && !gone;
    TaskView {
        id: row.get(0),
        agent_id: row.get(1),
        agent_name: row.get(2),
        requested_by: row.get(3),
        title,
        status: row.get(5),
        turns: row.get(6),
        cost_micros: row.get(7),
        created_epoch: row.get(8),
        started_epoch: row.get(9),
        finished_epoch: row.get(10),
        instruction: keep.then(|| row.get(11)),
        result: if keep { row.get(12) } else { None },
        sources: keep.then(|| row.get(13)),
        error: if gone { None } else { row.get(14) },
        checks: if keep { row.get(15) } else { None },
        review_status: if own { row.get(16) } else { None },
        edited_result: if keep { row.get(17) } else { None },
        schedule_id: row.get(18),
        deletes_epoch: if own && !gone { row.get(19) } else { None },
        retention_choice: if own { row.get(20) } else { None },
        deleted: gone,
        events: Vec::new(),
    }
}

/// A person's own tasks, newest first; or, for an administrator, everyone's (without the contents).
pub async fn list_tasks(tx: &TenantTx<'_>, principal: &Principal, everyone: bool, limit: i64) -> Result<Vec<TaskView>> {
    let everyone = everyone && principal.is_admin();
    let rows = tx
        .sql()
        .query(
            &format!("SELECT {TASK_COLUMNS} {TASK_FROM} WHERE ($1 OR t.requested_by = $2) ORDER BY t.created_at DESC LIMIT $3"),
            &[&everyone, &principal.user, &limit],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let own = row.get::<_, String>(3) == principal.user;
            let mut view = task_view(row, own);
            // The list never carries the long text, only the title.
            view.result = None;
            view.sources = None;
            view.checks = None;
            view.edited_result = None;
            view
        })
        .collect())
}

/// One task with its history. Administrators may look at any task's progress and cost, but only the
/// person who asked sees what was asked and what came back.
pub async fn get_task(tx: &TenantTx<'_>, principal: &Principal, id: Uuid) -> Result<Option<TaskView>> {
    let Some(row) = tx
        .sql()
        .query_opt(&format!("SELECT {TASK_COLUMNS} {TASK_FROM} WHERE t.id = $1"), &[&id])
        .await?
    else {
        return Ok(None);
    };
    let own = row.get::<_, String>(3) == principal.user;
    if !own && !principal.is_admin() {
        return Ok(None);
    }
    let mut view = task_view(&row, own);
    if !view.deleted {
        view.events = tx
            .sql()
            .query("SELECT kind, summary, EXTRACT(EPOCH FROM at)::float8 FROM agent_task_events WHERE task_id = $1 ORDER BY id", &[&id])
            .await?
            .into_iter()
            .map(|row| {
                let kind: String = row.get(0);
                // The owner sees what was read; anyone else sees only that work happened, because a
                // file's name can say more than the person looking may be allowed to know.
                let summary: String = if own {
                    row.get(1)
                } else {
                    match kind.as_str() {
                        "tool" => "Used a tool".to_string(),
                        "created" => "Task created".to_string(),
                        "cancelled" => "Stopped".to_string(),
                        "failed" => "Failed".to_string(),
                        "started" => "Started working".to_string(),
                        _ => "Finished".to_string(),
                    }
                };
                EventView { kind, summary, epoch: row.get(2) }
            })
            .collect();
    }
    Ok(Some(view))
}

/// Stops a task that has not finished. The person who started it, or an administrator, may.
pub async fn cancel_task(tx: &TenantTx<'_>, principal: &Principal, id: Uuid) -> Result<bool> {
    let sql = tx.sql();
    let changed = sql
        .execute(
            "UPDATE agent_tasks SET status = 'cancelled', finished_at = now(), retention_until = agent_retention_until()
             WHERE id = $1 AND status IN ('queued', 'running') AND ($2 OR requested_by = $3)",
            &[&id, &principal.is_admin(), &principal.user],
        )
        .await?;
    if changed == 0 {
        return Ok(false);
    }
    sql.execute("DELETE FROM agent_queue WHERE task_id = $1", &[&id]).await?;
    event(tx, id, "cancelled", &format!("Stopped by {}", principal.user)).await?;
    Ok(true)
}

/// Finished scheduled reports waiting for this person's decision, newest first.
pub async fn inbox(tx: &TenantTx<'_>, principal: &Principal) -> Result<Vec<TaskView>> {
    let rows = tx
        .sql()
        .query(
            &format!("SELECT {TASK_COLUMNS} {TASK_FROM} WHERE t.requested_by = $1 AND t.review_status = 'pending' AND t.deleted_at IS NULL
               AND (t.retention_until IS NULL OR t.retention_until > now()) ORDER BY t.finished_at DESC LIMIT 50"),
            &[&principal.user],
        )
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let mut view = task_view(row, true);
            view.result = None;
            view.sources = None;
            view.checks = None;
            view.edited_result = None;
            view
        })
        .collect())
}

/// Several of a person's own tasks by ID, newest deadline first, without their long text.
pub async fn task_views_by_ids(tx: &TenantTx<'_>, principal: &Principal, ids: &[Uuid]) -> Result<Vec<TaskView>> {
    let rows = tx
        .sql()
        .query(&format!("SELECT {TASK_COLUMNS} {TASK_FROM} WHERE t.id = ANY($1) AND t.requested_by = $2 ORDER BY t.retention_until"), &[&ids, &principal.user])
        .await?;
    Ok(rows
        .iter()
        .map(|row| {
            let mut view = task_view(row, true);
            view.result = None;
            view.sources = None;
            view.checks = None;
            view.edited_result = None;
            view.instruction = None;
            view
        })
        .collect())
}
