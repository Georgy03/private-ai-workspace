//! The list of connections that should keep syncing, shared by the workers. A row holds only IDs
//! and times. Workers claim rows with a lease, so two workers never sync the same connection at
//! once, and a worker that dies simply lets its lease run out.

use deadpool_postgres::Object;
use tokio_postgres::Client;
use uuid::Uuid;

use crate::{Result, auth::TenantId, db::TenantTx};

/// Marks a connection as due now (used when it is first connected and by "sync now").
/// Called by the API, so it runs inside the firm's own transaction.
pub async fn enqueue(tx: &TenantTx<'_>, connection: Uuid) -> Result<()> {
    tx.sql()
        .execute(
            "INSERT INTO sync_queue (tenant_id, connection_id) VALUES (app_tenant(), $1)
             ON CONFLICT (connection_id) DO UPDATE SET run_after = now()",
            &[&connection],
        )
        .await?;
    Ok(())
}

/// Takes up to `limit` due connections and holds them for `lease_secs`.
pub async fn claim(client: &Client, limit: i64, lease_secs: i64) -> Result<Vec<(TenantId, Uuid)>> {
    let rows = client
        .query(
            "UPDATE sync_queue SET claimed_until = now() + make_interval(secs => $2)
             WHERE connection_id IN (
               SELECT connection_id FROM sync_queue
               WHERE run_after <= now() AND (claimed_until IS NULL OR claimed_until < now())
               ORDER BY run_after LIMIT $1 FOR UPDATE SKIP LOCKED)
             RETURNING tenant_id, connection_id",
            &[&limit, &(lease_secs as f64)],
        )
        .await?;
    Ok(rows
        .into_iter()
        .map(|row| (TenantId(row.get(0)), row.get(1)))
        .collect())
}

/// Releases the lease and sets when the connection is next due.
pub async fn reschedule(client: &Client, connection: Uuid, delay_secs: i64) -> Result<()> {
    client
        .execute(
            "UPDATE sync_queue SET claimed_until = NULL, run_after = now() + make_interval(secs => $2)
             WHERE connection_id = $1",
            &[&connection, &(delay_secs as f64)],
        )
        .await?;
    Ok(())
}

/// Stops syncing a connection until something enqueues it again (e.g. after it is reconnected).
pub async fn remove(client: &Client, connection: Uuid) -> Result<()> {
    client
        .execute(
            "DELETE FROM sync_queue WHERE connection_id = $1",
            &[&connection],
        )
        .await?;
    Ok(())
}

pub async fn checkout(pool: &deadpool_postgres::Pool) -> Result<Object> {
    pool.get()
        .await
        .map_err(|error| crate::Error::Pool(error.to_string()))
}
