//! Recurring tasks that belong to one person. A run starts a normal agent task as that person,
//! with the access they had when the app last saw them, compares nothing it is not allowed to see,
//! and waits for their approval. Runs are skipped when nothing the last report used has changed,
//! and a schedule pauses after repeated failures or when its owner has not been seen for a while.

use std::time::Duration;

use chrono::{DateTime, Datelike, NaiveDate, NaiveDateTime, NaiveTime, TimeDelta, Weekday};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{
    Result,
    agents::{self, MAX_TASK_CHARS},
    auth::{Principal, TenantId},
    db::TenantTx,
    queue, usage,
};

/// A schedule whose owner has not used the app for this long stops running.
pub const IDENTITY_MAX_AGE_DAYS: i64 = 14;
/// A schedule pauses after this many runs in a row fail.
pub const MAX_FAILURES: i32 = 3;
const MAX_SCHEDULES_PER_PERSON: i64 = 25;
const MAX_NAME_CHARS: usize = 80;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Cadence {
    /// "daily", "weekly" or "monthly".
    pub kind: String,
    /// 0 is Monday. Weekly only.
    #[serde(default)]
    pub weekday: u32,
    /// 1 to 28, so every month has it. Monthly only.
    #[serde(default = "first")]
    pub day: u32,
    /// Minutes after midnight, in the owner's time.
    pub minutes: u32,
    /// Minutes east of UTC for the owner's time zone. The app updates it so daylight saving follows.
    #[serde(default)]
    pub offset: i32,
}

fn first() -> u32 {
    1
}

impl Cadence {
    pub fn problem(&self) -> Option<&'static str> {
        if !["daily", "weekly", "monthly"].contains(&self.kind.as_str()) {
            return Some("choose daily, weekly or monthly");
        }
        if self.weekday > 6 || !(1..=28).contains(&self.day) || self.minutes >= 1440 || !(-840..=840).contains(&self.offset) {
            return Some("that time is not valid");
        }
        None
    }

    /// The first moment strictly after `after` (seconds since 1970) that this schedule is due.
    pub fn next_after(&self, after: i64) -> i64 {
        let offset = i64::from(self.offset) * 60;
        let local = DateTime::from_timestamp(after + offset, 0).map_or(NaiveDateTime::MIN, |d| d.naive_utc());
        let at = NaiveTime::from_hms_opt(self.minutes / 60, self.minutes % 60, 0).unwrap_or_default();
        let on = |date: NaiveDate| date.and_time(at);
        let today = local.date();
        let chosen = match self.kind.as_str() {
            "daily" => {
                let candidate = on(today);
                if candidate > local { candidate } else { on(today + TimeDelta::days(1)) }
            }
            "weekly" => (0..=7)
                .map(|days| on(today + TimeDelta::days(days)))
                .find(|c| c.weekday() == weekday(self.weekday) && *c > local)
                .unwrap_or(local + TimeDelta::days(7)),
            _ => {
                let this = on(today.with_day(self.day).unwrap_or(today));
                if this > local {
                    this
                } else {
                    let (year, month) = if today.month() == 12 { (today.year() + 1, 1) } else { (today.year(), today.month() + 1) };
                    on(NaiveDate::from_ymd_opt(year, month, self.day).unwrap_or(today))
                }
            }
        };
        chosen.and_utc().timestamp() - offset
    }
}

fn weekday(number: u32) -> Weekday {
    [Weekday::Mon, Weekday::Tue, Weekday::Wed, Weekday::Thu, Weekday::Fri, Weekday::Sat, Weekday::Sun][number.min(6) as usize]
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

#[derive(Debug, Clone, Serialize)]
pub struct ScheduleView {
    pub id: Uuid,
    pub agent_id: Uuid,
    pub agent_name: String,
    pub name: String,
    pub instruction: String,
    pub cadence: Cadence,
    pub status: String,
    pub pause_reason: Option<String>,
    pub monthly_budget_micros: Option<i64>,
    pub spent_this_month_micros: i64,
    pub next_run_epoch: Option<f64>,
    pub last_run_epoch: Option<f64>,
    pub last_task_id: Option<Uuid>,
    pub last_note: Option<String>,
    pub waiting_for_review: i64,
}

#[derive(Debug, Clone, Default)]
pub struct SchedulePatch {
    pub name: Option<String>,
    pub instruction: Option<String>,
    pub cadence: Option<Cadence>,
    pub status: Option<String>,
    /// `Some(None)` removes the limit.
    pub monthly_budget_micros: Option<Option<i64>>,
}

impl SchedulePatch {
    pub fn problem(&self) -> Option<&'static str> {
        if let Some(name) = &self.name
            && !(1..=MAX_NAME_CHARS).contains(&name.trim().chars().count())
        {
            return Some("the name must be 1 to 80 characters");
        }
        if let Some(text) = &self.instruction
            && !(1..=MAX_TASK_CHARS).contains(&text.trim().chars().count())
        {
            return Some("describe the task in 1 to 4,000 characters");
        }
        if let Some(problem) = self.cadence.as_ref().and_then(Cadence::problem) {
            return Some(problem);
        }
        if self.status.as_deref().is_some_and(|s| s != "active" && s != "paused") {
            return Some("unknown status");
        }
        if self.monthly_budget_micros.flatten().is_some_and(|b| !(0..=100_000_000_000).contains(&b)) {
            return Some("the budget is out of range");
        }
        None
    }
}

const VIEW: &str = "SELECT s.id, s.agent_id, v.name, s.name, s.instruction, s.cadence, s.status, s.pause_reason,
        s.monthly_budget_micros,
        (SELECT COALESCE(sum(t.cost_micros), 0)::bigint FROM agent_tasks t
          WHERE t.schedule_id = s.id AND t.created_at >= date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'),
        EXTRACT(EPOCH FROM c.next_run_at)::float8, EXTRACT(EPOCH FROM s.last_run_at)::float8, s.last_task_id, s.last_note,
        (SELECT count(*) FROM agent_tasks t WHERE t.schedule_id = s.id AND t.review_status = 'pending')
 FROM agent_schedules s
 JOIN agents a ON a.tenant_id = s.tenant_id AND a.id = s.agent_id
 JOIN agent_versions v ON v.tenant_id = a.tenant_id AND v.agent_id = a.id AND v.version = a.current_version
 LEFT JOIN agent_schedule_clock c ON c.schedule_id = s.id";

fn view(row: &tokio_postgres::Row) -> ScheduleView {
    ScheduleView {
        id: row.get(0),
        agent_id: row.get(1),
        agent_name: row.get(2),
        name: row.get(3),
        instruction: row.get(4),
        cadence: serde_json::from_value(row.get::<_, Value>(5)).unwrap_or(Cadence { kind: "weekly".into(), weekday: 4, day: 1, minutes: 540, offset: 0 }),
        status: row.get(6),
        pause_reason: row.get(7),
        monthly_budget_micros: row.get(8),
        spent_this_month_micros: row.get(9),
        next_run_epoch: row.get(10),
        last_run_epoch: row.get(11),
        last_task_id: row.get(12),
        last_note: row.get(13),
        waiting_for_review: row.get(14),
    }
}

async fn set_clock(tx: &TenantTx<'_>, schedule: Uuid, at: i64) -> Result<()> {
    tx.sql()
        .execute(
            "INSERT INTO agent_schedule_clock (schedule_id, tenant_id, next_run_at) VALUES ($1, app_tenant(), to_timestamp($2::float8))
             ON CONFLICT (schedule_id) DO UPDATE SET next_run_at = EXCLUDED.next_run_at, claimed_until = NULL",
            &[&schedule, &(at as f64)],
        )
        .await?;
    Ok(())
}

pub enum Created {
    Made(Uuid),
    NoSuchAgent,
    NotAvailable,
    TooMany,
}

pub async fn create(
    tx: &TenantTx<'_>,
    principal: &Principal,
    agent: Uuid,
    name: &str,
    instruction: &str,
    cadence: &Cadence,
    budget: Option<i64>,
) -> Result<Created> {
    let Some(found) = agents::get_agent(tx, agent).await? else { return Ok(Created::NoSuchAgent) };
    if found.status != "available" || !agents::allowed(&found.audience, principal) {
        return Ok(Created::NotAvailable);
    }
    let count: i64 = tx.sql().query_one("SELECT count(*) FROM agent_schedules WHERE owner = $1", &[&principal.user]).await?.get(0);
    if count >= MAX_SCHEDULES_PER_PERSON {
        return Ok(Created::TooMany);
    }
    let id: Uuid = tx
        .sql()
        .query_one(
            "INSERT INTO agent_schedules (tenant_id, agent_id, owner, owner_groups, owner_email, name, instruction, cadence, monthly_budget_micros)
             VALUES (app_tenant(), $1, $2, $3, $4, $5, $6, $7, $8) RETURNING id",
            &[&agent, &principal.user, &principal.groups, &principal.email, &name.trim(), &instruction.trim(), &json!(cadence), &budget],
        )
        .await?
        .get(0);
    set_clock(tx, id, cadence.next_after(now())).await?;
    Ok(Created::Made(id))
}

/// A person's own schedules. Asking also refreshes the access the schedules run with, and the
/// time-zone offset, because that is how the app tells the server the person is still here.
pub async fn list(tx: &TenantTx<'_>, principal: &Principal, offset: Option<i32>) -> Result<Vec<ScheduleView>> {
    refresh_identity(tx, principal).await?;
    let rows = tx.sql().query(&format!("{VIEW} WHERE s.owner = $1 ORDER BY s.created_at"), &[&principal.user]).await?;
    let mut views: Vec<ScheduleView> = rows.iter().map(view).collect();
    if let Some(offset) = offset.filter(|o| (-840..=840).contains(o)) {
        for item in views.iter_mut().filter(|v| v.cadence.offset != offset) {
            item.cadence.offset = offset;
            tx.sql().execute("UPDATE agent_schedules SET cadence = $2 WHERE id = $1", &[&item.id, &json!(item.cadence)]).await?;
            if item.status == "active" {
                let next = item.cadence.next_after(now());
                set_clock(tx, item.id, next).await?;
                item.next_run_epoch = Some(next as f64);
            }
        }
    }
    Ok(views)
}

/// Records that this person is here, with the access they have now.
pub async fn refresh_identity(tx: &TenantTx<'_>, principal: &Principal) -> Result<()> {
    tx.sql()
        .execute(
            "UPDATE agent_schedules SET owner_groups = $2, owner_email = $3, identity_refreshed_at = now()
             WHERE owner = $1 AND identity_refreshed_at < now() - interval '10 minutes'",
            &[&principal.user, &principal.groups, &principal.email],
        )
        .await?;
    Ok(())
}

/// Changes one of the person's own schedules. False if there is none.
pub async fn update(tx: &TenantTx<'_>, principal: &Principal, id: Uuid, patch: &SchedulePatch) -> Result<bool> {
    let sql = tx.sql();
    let Some(row) = sql.query_opt("SELECT cadence, status FROM agent_schedules WHERE id = $1 AND owner = $2 FOR UPDATE", &[&id, &principal.user]).await? else {
        return Ok(false);
    };
    let old: Cadence = serde_json::from_value(row.get::<_, Value>(0)).unwrap_or(Cadence { kind: "weekly".into(), weekday: 4, day: 1, minutes: 540, offset: 0 });
    let cadence = patch.cadence.clone().unwrap_or(old);
    let status: String = patch.status.clone().unwrap_or_else(|| row.get(1));
    let resumed = patch.status.as_deref() == Some("active");
    sql.execute(
        "UPDATE agent_schedules SET
            name = COALESCE($2, name), instruction = COALESCE($3, instruction), cadence = $4, status = $5,
            pause_reason = CASE WHEN $5 = 'active' THEN NULL WHEN $6 THEN pause_reason ELSE pause_reason END,
            consecutive_failures = CASE WHEN $7 THEN 0 ELSE consecutive_failures END,
            monthly_budget_micros = CASE WHEN $8 THEN $9 ELSE monthly_budget_micros END,
            owner_groups = $10, owner_email = $11, identity_refreshed_at = now()
         WHERE id = $1",
        &[
            &id,
            &patch.name.as_deref().map(str::trim),
            &patch.instruction.as_deref().map(str::trim),
            &json!(cadence),
            &status,
            &(status == "paused"),
            &resumed,
            &patch.monthly_budget_micros.is_some(),
            &patch.monthly_budget_micros.flatten(),
            &principal.groups,
            &principal.email,
        ],
    )
    .await?;
    if status == "active" {
        set_clock(tx, id, cadence.next_after(now())).await?;
    } else {
        sql.execute("DELETE FROM agent_schedule_clock WHERE schedule_id = $1", &[&id]).await?;
    }
    Ok(true)
}

pub async fn remove(tx: &TenantTx<'_>, principal: &Principal, id: Uuid) -> Result<bool> {
    Ok(tx.sql().execute("DELETE FROM agent_schedules WHERE id = $1 AND owner = $2", &[&id, &principal.user]).await? > 0)
}

/// What happened when a schedule came due.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Launched {
    Started(Uuid),
    /// Not run this time, and why.
    Skipped(String),
    /// Switched off until its owner turns it back on, and why.
    Paused(String),
}

/// A fingerprint of the files a report used, as they are now and as this person may open them. If
/// it is unchanged next time, the report would say the same thing.
pub async fn fingerprint(tx: &TenantTx<'_>, readers: &[String], sources: &Value) -> Result<String> {
    let ids: Vec<Uuid> = sources
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s["document_id"].as_str().and_then(|id| Uuid::parse_str(id).ok()))
        .collect();
    let rows = tx
        .sql()
        .query(
            "SELECT d.id::text || ':' || d.content_hash || ':' || d.modified_at FROM documents d
             WHERE d.id = ANY($1) AND d.readable_by && $2::text[] ORDER BY d.id",
            &[&ids, &readers],
        )
        .await?;
    let mut hash = Sha256::new();
    for row in rows {
        hash.update(row.get::<_, String>(0).as_bytes());
        hash.update(b"\n");
    }
    Ok(hash.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn date_text(epoch: i64) -> String {
    DateTime::from_timestamp(epoch, 0).map_or_else(String::new, |d| d.format("%Y-%m-%d").to_string())
}

/// Starts the task for a schedule. `force` is for "run now" and skips the unchanged-files shortcut.
pub async fn launch(tx: &TenantTx<'_>, schedule: Uuid, principal: &Principal, force: bool) -> Result<Launched> {
    let sql = tx.sql();
    let Some(row) = sql
        .query_opt(
            "SELECT agent_id, name, instruction, monthly_budget_micros, last_task_id FROM agent_schedules WHERE id = $1 FOR UPDATE",
            &[&schedule],
        )
        .await?
    else {
        return Ok(Launched::Paused("This schedule no longer exists".into()));
    };
    let (agent_id, name, instruction, budget, last): (Uuid, String, String, Option<i64>, Option<Uuid>) =
        (row.get(0), row.get(1), row.get(2), row.get(3), row.get(4));
    let skip = |note: &str| Launched::Skipped(note.to_string());
    let Some(agent) = agents::get_agent(tx, agent_id).await? else {
        return Ok(Launched::Paused("The agent was removed".into()));
    };
    if agent.status != "available" {
        return Ok(Launched::Paused("The agent is paused or archived".into()));
    }
    if !agents::allowed(&agent.audience, principal) {
        return Ok(Launched::Paused("You no longer have access to this agent".into()));
    }
    if agent.monthly_budget_micros.is_some_and(|b| agent.spent_this_month_micros >= b) {
        return Ok(skip("Skipped: the agent has reached its monthly budget"));
    }
    let spent: i64 = sql
        .query_one(
            "SELECT COALESCE(sum(cost_micros), 0)::bigint FROM agent_tasks
             WHERE schedule_id = $1 AND created_at >= date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'",
            &[&schedule],
        )
        .await?
        .get(0);
    if budget.is_some_and(|b| spent >= b) {
        return Ok(skip("Skipped: this schedule has reached its monthly budget"));
    }
    let used = tx.usage_this_month(&principal.user).await?;
    if used.used_micros >= used.firm_limit_micros.unwrap_or_else(usage::default_limit_micros) {
        return Ok(skip("Skipped: you have reached your monthly AI allowance"));
    }
    // The last report, if it finished, is what this one is compared with.
    let previous = match last {
        Some(id) => sql
            .query_opt("SELECT sources, sources_fingerprint FROM agent_tasks WHERE id = $1 AND status = 'done'", &[&id])
            .await?
            .map(|r| (id, r.get::<_, Value>(0), r.get::<_, Option<String>>(1))),
        None => None,
    };
    if !force
        && let Some((_, sources, Some(before))) = &previous
        // A report that used no files says nothing about whether anything changed, so it is never skipped.
        && sources.as_array().is_some_and(|used| !used.is_empty())
        && fingerprint(tx, &principal.readers(), sources).await? == *before
    {
        return Ok(skip("Skipped: nothing the last report used has changed"));
    }
    let title: String = format!("{name}, {}", date_text(now())).chars().take(80).collect();
    let task: Uuid = sql
        .query_one(
            "INSERT INTO agent_tasks (tenant_id, agent_id, agent_version, requested_by, requester_groups, requester_email,
                                      title, instruction, schedule_id, previous_task_id)
             VALUES (app_tenant(), $1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
            &[&agent_id, &agent.version, &principal.user, &principal.groups, &principal.email, &title, &instruction, &schedule, &previous.as_ref().map(|p| p.0)],
        )
        .await?
        .get(0);
    sql.execute("INSERT INTO agent_queue (tenant_id, task_id) VALUES (app_tenant(), $1)", &[&task]).await?;
    agents::event(tx, task, "created", &format!("Started by the schedule \"{name}\"")).await?;
    sql.execute("UPDATE agent_schedules SET last_run_at = now(), last_task_id = $2, last_note = NULL WHERE id = $1", &[&schedule, &task]).await?;
    Ok(Launched::Started(task))
}

/// Runs a person's schedule now, on top of its normal times.
pub async fn run_now(tx: &TenantTx<'_>, principal: &Principal, id: Uuid) -> Result<Option<Launched>> {
    let owned: bool = tx.sql().query_one("SELECT EXISTS (SELECT 1 FROM agent_schedules WHERE id = $1 AND owner = $2)", &[&id, &principal.user]).await?.get(0);
    if !owned {
        return Ok(None);
    }
    refresh_identity(tx, principal).await?;
    Ok(Some(launch(tx, id, principal, true).await?))
}

/// Called when a scheduled task ends: a success clears the count of failures, and a run of
/// failures pauses the schedule so it does not spend money on something broken.
pub async fn record_outcome(tx: &TenantTx<'_>, task: Uuid, ok: bool) -> Result<()> {
    let sql = tx.sql();
    let Some(row) = sql.query_opt("SELECT schedule_id FROM agent_tasks WHERE id = $1", &[&task]).await? else { return Ok(()) };
    let Some(schedule) = row.get::<_, Option<Uuid>>(0) else { return Ok(()) };
    if ok {
        sql.execute("UPDATE agent_schedules SET consecutive_failures = 0 WHERE id = $1", &[&schedule]).await?;
        return Ok(());
    }
    let failures: Option<i32> = sql
        .query_opt("UPDATE agent_schedules SET consecutive_failures = consecutive_failures + 1 WHERE id = $1 RETURNING consecutive_failures", &[&schedule])
        .await?
        .map(|r| r.get(0));
    if failures.is_some_and(|n| n >= MAX_FAILURES) {
        sql.execute(
            "UPDATE agent_schedules SET status = 'paused', pause_reason = $2 WHERE id = $1",
            &[&schedule, &format!("Paused after {MAX_FAILURES} failed runs in a row")],
        )
        .await?;
        sql.execute("DELETE FROM agent_schedule_clock WHERE schedule_id = $1", &[&schedule]).await?;
    }
    Ok(())
}

/// Starts every schedule that is due, across firms. Returns how many it looked at.
pub async fn due_pass(pool: &Pool) -> Result<usize> {
    let due: Vec<(Uuid, Uuid)> = {
        let conn = queue::checkout(pool).await?;
        conn.query(
            "UPDATE agent_schedule_clock SET claimed_until = now() + interval '2 minutes'
             WHERE schedule_id IN (SELECT schedule_id FROM agent_schedule_clock
                                   WHERE next_run_at <= now() AND (claimed_until IS NULL OR claimed_until < now())
                                   ORDER BY next_run_at LIMIT 20 FOR UPDATE SKIP LOCKED)
             RETURNING schedule_id, tenant_id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
    };
    let count = due.len();
    for (schedule, tenant) in due {
        if let Err(error) = run_due(pool, TenantId(tenant), schedule).await {
            tracing::error!(%error, %schedule, "could not start a scheduled task");
            // Try again in a few minutes rather than every pass.
            let conn = queue::checkout(pool).await?;
            conn.execute("UPDATE agent_schedule_clock SET claimed_until = now() + interval '5 minutes' WHERE schedule_id = $1", &[&schedule]).await?;
        }
    }
    Ok(count)
}

async fn run_due(pool: &Pool, tenant: TenantId, schedule: Uuid) -> Result<()> {
    let mut conn = queue::checkout(pool).await?;
    let tx = TenantTx::begin(&mut conn, tenant).await?;
    let sql = tx.sql();
    let Some(row) = sql
        .query_opt(
            "SELECT owner, owner_groups, owner_email, status, cadence,
                    identity_refreshed_at < now() - make_interval(days => $2)
             FROM agent_schedules WHERE id = $1",
            &[&schedule, &(IDENTITY_MAX_AGE_DAYS as i32)],
        )
        .await?
    else {
        sql.execute("DELETE FROM agent_schedule_clock WHERE schedule_id = $1", &[&schedule]).await?;
        tx.commit().await?;
        return Ok(());
    };
    let principal = Principal { tenant, user: row.get(0), groups: row.get(1), email: row.get(2) };
    let cadence: Cadence = serde_json::from_value(row.get::<_, Value>(4)).unwrap_or(Cadence { kind: "weekly".into(), weekday: 4, day: 1, minutes: 540, offset: 0 });
    let outcome = if row.get::<_, String>(3) != "active" {
        Launched::Paused("Paused".into())
    } else if row.get::<_, bool>(5) {
        Launched::Paused("Open the app to keep this running: you have not been seen for two weeks".into())
    } else {
        launch(&tx, schedule, &principal, false).await?
    };
    match outcome {
        Launched::Started(task) => {
            set_clock(&tx, schedule, cadence.next_after(now())).await?;
            tx.audit("worker", "schedule_run", &json!({ "schedule": schedule, "task": task })).await?;
        }
        Launched::Skipped(note) => {
            sql.execute("UPDATE agent_schedules SET last_note = $2 WHERE id = $1", &[&schedule, &note]).await?;
            set_clock(&tx, schedule, cadence.next_after(now())).await?;
            tx.audit("worker", "schedule_skipped", &json!({ "schedule": schedule, "why": note })).await?;
        }
        Launched::Paused(reason) => {
            sql.execute("UPDATE agent_schedules SET status = 'paused', pause_reason = $2 WHERE id = $1", &[&schedule, &reason]).await?;
            sql.execute("DELETE FROM agent_schedule_clock WHERE schedule_id = $1", &[&schedule]).await?;
            tx.audit("worker", "schedule_paused", &json!({ "schedule": schedule, "why": reason })).await?;
        }
    }
    tx.commit().await?;
    Ok(())
}

pub async fn schedules_forever(pool: Pool) {
    loop {
        match due_pass(&pool).await {
            Ok(0) => tokio::time::sleep(Duration::from_secs(30)).await,
            Ok(_) => tokio::time::sleep(Duration::from_secs(2)).await,
            Err(error) => {
                tracing::error!(%error, "schedule loop failed");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}

// ---- Review: a scheduled report waits for its owner's decision ----

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Review {
    Approve,
    Reject,
}

/// Approves or rejects a finished scheduled report, optionally with the owner's edited version.
/// False if there is nothing of theirs waiting for review.
pub async fn review(tx: &TenantTx<'_>, principal: &Principal, task: Uuid, decision: Review, edited: Option<&str>) -> Result<bool> {
    let status = if decision == Review::Approve { "approved" } else { "rejected" };
    Ok(tx
        .sql()
        .execute(
            "UPDATE agent_tasks SET review_status = $2, reviewed_at = now(), edited_result = COALESCE($3, edited_result)
             WHERE id = $1 AND requested_by = $4 AND status = 'done' AND review_status = 'pending'",
            &[&task, &status, &edited, &principal.user],
        )
        .await?
        > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(date: &str, time: &str) -> i64 {
        NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M").unwrap().and_utc().timestamp()
    }

    fn weekly_friday_9() -> Cadence {
        Cadence { kind: "weekly".into(), weekday: 4, day: 1, minutes: 9 * 60, offset: 0 }
    }

    #[test]
    fn a_weekly_schedule_runs_on_its_day_and_time() {
        // 2026-11-04 is a Wednesday; the next Friday is 2026-11-06.
        assert_eq!(weekly_friday_9().next_after(at("2026-11-04", "12:00")), at("2026-11-06", "09:00"));
        // Just before 9 on a Friday it is that same morning; just after, a week later.
        assert_eq!(weekly_friday_9().next_after(at("2026-11-06", "08:59")), at("2026-11-06", "09:00"));
        assert_eq!(weekly_friday_9().next_after(at("2026-11-06", "09:00")), at("2026-11-13", "09:00"));
    }

    #[test]
    fn the_owners_time_zone_is_respected() {
        // 9am in UTC-5 is 14:00 UTC.
        let cadence = Cadence { offset: -300, ..weekly_friday_9() };
        assert_eq!(cadence.next_after(at("2026-11-04", "12:00")), at("2026-11-06", "14:00"));
        // 9am in UTC+10 on Friday is Thursday 23:00 UTC.
        let sydney = Cadence { offset: 600, ..weekly_friday_9() };
        assert_eq!(sydney.next_after(at("2026-11-04", "12:00")), at("2026-11-05", "23:00"));
    }

    #[test]
    fn daily_and_monthly_schedules() {
        let daily = Cadence { kind: "daily".into(), weekday: 0, day: 1, minutes: 7 * 60 + 30, offset: 0 };
        assert_eq!(daily.next_after(at("2026-11-04", "07:00")), at("2026-11-04", "07:30"));
        assert_eq!(daily.next_after(at("2026-11-04", "08:00")), at("2026-11-05", "07:30"));
        let monthly = Cadence { kind: "monthly".into(), weekday: 0, day: 15, minutes: 600, offset: 0 };
        assert_eq!(monthly.next_after(at("2026-11-04", "08:00")), at("2026-11-15", "10:00"));
        assert_eq!(monthly.next_after(at("2026-11-15", "10:00")), at("2026-12-15", "10:00"));
        assert_eq!(monthly.next_after(at("2026-12-20", "10:00")), at("2027-01-15", "10:00"));
    }

    #[test]
    fn bad_times_are_refused() {
        assert!(Cadence { kind: "hourly".into(), ..weekly_friday_9() }.problem().is_some());
        assert!(Cadence { minutes: 1440, ..weekly_friday_9() }.problem().is_some());
        assert!(Cadence { kind: "monthly".into(), day: 31, ..weekly_friday_9() }.problem().is_some());
        assert!(weekly_friday_9().problem().is_none());
    }
}
