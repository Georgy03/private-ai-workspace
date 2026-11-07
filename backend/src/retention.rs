//! How long agent work is kept. A finished task's report and the conversation behind it are kept for
//! the firm's retention period (90 days unless the firm chooses otherwise), then deleted. A week
//! before, the person it belongs to is asked whether to keep it. Their answer is final: "keep"
//! means it stays until they delete it; "let it run out" means it is deleted on its date.

use std::time::Duration;

use deadpool_postgres::Pool;
use serde_json::json;
use uuid::Uuid;

use crate::{
    Result,
    agents::{TaskView, task_views_by_ids},
    auth::{Principal, TenantId},
    db::TenantTx,
    queue,
};

pub const DEFAULT_DAYS: i32 = 90;
/// People are asked this many days before something is deleted.
pub const NOTICE_DAYS: i32 = 7;
pub const CHOICES: &[i32] = &[0, 30, 90, 180, 365];

/// The firm's retention period in days. 0 means "until the owner deletes it".
pub async fn policy(tx: &TenantTx<'_>) -> Result<i32> {
    Ok(tx
        .sql()
        .query_opt("SELECT agent_retention_days FROM firm_settings", &[])
        .await?
        .and_then(|row| row.get::<_, Option<i32>>(0))
        .unwrap_or(DEFAULT_DAYS))
}

/// Sets the firm's period. Work nobody has been asked about yet moves to the new date, with at
/// least a week's notice so nothing disappears without the owner being asked.
pub async fn set_policy(tx: &TenantTx<'_>, days: i32) -> Result<()> {
    let sql = tx.sql();
    sql.execute(
        "INSERT INTO firm_settings (tenant_id, agent_retention_days) VALUES (app_tenant(), $1)
         ON CONFLICT (tenant_id) DO UPDATE SET agent_retention_days = EXCLUDED.agent_retention_days",
        &[&days],
    )
    .await?;
    sql.execute(
        "UPDATE agent_tasks SET retention_until =
             CASE WHEN $1 = 0 THEN NULL ELSE GREATEST(finished_at + make_interval(days => $1), now() + make_interval(days => $2)) END
         WHERE finished_at IS NOT NULL AND retention_choice IS NULL AND deleted_at IS NULL",
        &[&days, &NOTICE_DAYS],
    )
    .await?;
    Ok(())
}

/// Reports of this person's that will be deleted soon and that they have not yet decided about.
pub async fn expiring(tx: &TenantTx<'_>, principal: &Principal) -> Result<Vec<TaskView>> {
    let ids: Vec<Uuid> = tx
        .sql()
        .query(
            "SELECT id FROM agent_tasks
             WHERE requested_by = $1 AND retention_choice IS NULL AND deleted_at IS NULL
               AND status IN ('done', 'failed', 'cancelled')
               AND retention_until > now() AND retention_until <= now() + make_interval(days => $2)
             ORDER BY retention_until",
            &[&principal.user, &NOTICE_DAYS],
        )
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    task_views_by_ids(tx, principal, &ids).await
}

/// Keeps a report until its owner deletes it.
pub async fn keep(tx: &TenantTx<'_>, principal: &Principal, task: Uuid) -> Result<bool> {
    Ok(tx
        .sql()
        .execute(
            "UPDATE agent_tasks SET retention_choice = 'keep', retention_until = NULL
             WHERE id = $1 AND requested_by = $2 AND deleted_at IS NULL AND status IN ('done', 'failed', 'cancelled')",
            &[&task, &principal.user],
        )
        .await?
        > 0)
}

/// The owner has been asked and chose to let it be deleted on its date: stop asking.
pub async fn let_expire(tx: &TenantTx<'_>, principal: &Principal, task: Uuid) -> Result<bool> {
    Ok(tx
        .sql()
        .execute(
            "UPDATE agent_tasks SET retention_choice = 'expire'
             WHERE id = $1 AND requested_by = $2 AND deleted_at IS NULL AND retention_until IS NOT NULL",
            &[&task, &principal.user],
        )
        .await?
        > 0)
}

/// Deletes a finished report now. The worker removes the content within a minute; until then the
/// report already shows as deleted.
pub async fn delete_now(tx: &TenantTx<'_>, principal: &Principal, task: Uuid) -> Result<bool> {
    Ok(tx
        .sql()
        .execute(
            "UPDATE agent_tasks SET retention_until = now()
             WHERE id = $1 AND requested_by = $2 AND deleted_at IS NULL AND status IN ('done', 'failed', 'cancelled')",
            &[&task, &principal.user],
        )
        .await?
        > 0)
}

/// Removes a task's content: the conversation, the activity, the request, the report and what it
/// used. What stays is the date and the cost, so budgets and allowances still add up.
pub async fn purge_task(tx: &TenantTx<'_>, task: Uuid) -> Result<bool> {
    let sql = tx.sql();
    let changed = sql
        .execute(
            "UPDATE agent_tasks SET title = 'Deleted report', instruction = '', result = NULL, edited_result = NULL,
                    sources = '[]', checks = NULL, error = NULL, sources_fingerprint = NULL,
                    deleted_at = now(), retention_until = NULL
             WHERE id = $1 AND deleted_at IS NULL AND status IN ('done', 'failed', 'cancelled')",
            &[&task],
        )
        .await?;
    if changed == 0 {
        return Ok(false);
    }
    sql.execute("DELETE FROM agent_task_messages WHERE task_id = $1", &[&task]).await?;
    sql.execute("DELETE FROM agent_task_events WHERE task_id = $1", &[&task]).await?;
    // Emails and meetings it prepared go with it, whether or not they were used.
    sql.execute("DELETE FROM agent_actions WHERE task_id = $1", &[&task]).await?;
    tx.audit("worker", "agent_task_purged", &json!({ "task": task })).await?;
    Ok(true)
}

/// Deletes everything that is due, across firms. Returns how many it deleted.
pub async fn purge_pass(pool: &Pool) -> Result<usize> {
    let due: Vec<(Uuid, Uuid)> = {
        let conn = queue::checkout(pool).await?;
        conn.query(
            "UPDATE agent_purge_queue SET claimed_until = now() + interval '5 minutes'
             WHERE task_id IN (SELECT task_id FROM agent_purge_queue
                               WHERE purge_at <= now() AND (claimed_until IS NULL OR claimed_until < now())
                               ORDER BY purge_at LIMIT 50 FOR UPDATE SKIP LOCKED)
             RETURNING task_id, tenant_id",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect()
    };
    let mut purged = 0;
    for (task, tenant) in due {
        let mut conn = queue::checkout(pool).await?;
        let tx = TenantTx::begin(&mut conn, TenantId(tenant)).await?;
        if purge_task(&tx, task).await? {
            purged += 1;
        }
        tx.commit().await?;
    }
    Ok(purged)
}

pub async fn retention_forever(pool: Pool) {
    loop {
        if let Err(error) = purge_pass(&pool).await {
            tracing::error!(%error, "deleting old agent work failed");
        }
        tokio::time::sleep(Duration::from_secs(60)).await;
    }
}
