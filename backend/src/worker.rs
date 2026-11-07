//! The background process that keeps connections up to date: it claims due connections from the
//! queue, builds a connector for each from the firm's saved refresh token, runs the sync and
//! schedules the next one.

use std::{sync::Arc, time::Duration};

use deadpool_postgres::Pool;

use crate::embed::Embedder;
use crate::extract_process::Extractor;
use crate::{
    Error, Result,
    auth::TenantId,
    connectors::{
        Connector, ConnectorError, RefreshingToken, RotatedToken, docusign::DocusignConnector,
        dropbox::DropboxConnector, google::GoogleConnector, microsoft::MicrosoftConnector,
        quickbooks::QuickbooksConnector,
    },
    db::TenantTx,
    oauth::{Kind, OAuthSettings},
    queue,
    secrets::{SecretStore, refresh_token_name},
    sync::sync_connection,
};

const CLAIM_BATCH: i64 = 4;
/// A claimed connection is held this long. It must outlast a normal sync; a worker that dies
/// simply lets the lease expire and another picks the work up.
const LEASE_SECS: i64 = 1800;
const NEXT_SYNC_SECS: i64 = 300;
const RETRY_AFTER_ERROR_SECS: i64 = 900;

async fn build(
    pool: &Pool,
    secrets: &dyn SecretStore,
    oauth: &OAuthSettings,
    tenant: TenantId,
    connection: uuid::Uuid,
) -> Result<Option<(Box<dyn Connector>, Option<RotatedToken>)>> {
    let state = {
        let mut conn = queue::checkout(pool).await?;
        let tx = TenantTx::begin(&mut conn, tenant).await?;
        tx.connection(connection).await?
    };
    let Some(state) = state else { return Ok(None) };
    let kind =
        Kind::parse(&state.kind).ok_or_else(|| Error::Config("unknown connection kind".into()))?;
    let provider = oauth
        .provider(kind)
        .ok_or_else(|| Error::Config(format!("{} is not configured on this server", state.kind)))?;
    let refresh = secrets
        .get(tenant, &refresh_token_name(tenant, connection))
        .await?
        .ok_or(Error::Connector(ConnectorError::Unauthorized))?;
    let refreshing = RefreshingToken::new(
        provider.token_url.clone(),
        provider.client_id.clone(),
        provider.client_secret.clone(),
        refresh,
    )?
    .with_basic_auth(provider.basic_auth);
    let rotated = refreshing.rotated_token();
    let tokens = Box::new(refreshing);
    // The person who connected the source can read everything it returns, under their sign-in ID
    // and under the email of the provider account they connected.
    let mut connected_by: Vec<String> = state.config["connected_by"]
        .as_str()
        .map(|user| vec![format!("user:{user}")])
        .unwrap_or_default();
    if let Some(email) = state.config["account_email"].as_str().and_then(crate::connectors::email_principal) {
        connected_by.push(email);
    }
    let connector: Box<dyn Connector> = match kind {
        Kind::Microsoft365 => {
            let drive = state.config["drive_id"]
                .as_str()
                .ok_or_else(|| Error::Config("connection has no drive".into()))?;
            let domains = state.config["domains"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|d| d.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            Box::new(
                MicrosoftConnector::new(tokens, &provider.api_base, drive, domains)?
                    .always_readable_by(connected_by),
            )
        }
        Kind::GoogleDrive => Box::new(
            GoogleConnector::new(tokens, &provider.api_base)?.always_readable_by(connected_by),
        ),
        Kind::Dropbox => {
            // Files are downloaded from a different host than the one that answers other calls.
            let content = provider.api_base.replace("api.dropboxapi.com", "content.dropboxapi.com");
            Box::new(DropboxConnector::new(tokens, &provider.api_base, &content)?.always_readable_by(connected_by))
        }
        // A mailbox connected for sending has no files, and is never queued; if one ever is, stop.
        Kind::OutlookMail | Kind::Zoom => return Ok(None),
        Kind::QuickBooks => {
            let realm = state.config["realm_id"].as_str().ok_or_else(|| Error::Config("connection has no QuickBooks company".into()))?;
            let company = state.config["company_name"].as_str().unwrap_or("QuickBooks");
            Box::new(QuickbooksConnector::new(tokens, &provider.api_base, realm, company)?.always_readable_by(connected_by))
        }
        Kind::DocuSign => {
            let account = state.config["account_id"].as_str().ok_or_else(|| Error::Config("connection has no account".into()))?;
            let base = state.config["base_uri"].as_str().ok_or_else(|| Error::Config("connection has no API address".into()))?;
            Box::new(DocusignConnector::new(tokens, base, account)?.always_readable_by(connected_by))
        }
    };
    Ok(Some((connector, Some(rotated))))
}

/// Syncs every connection that is due, once. Returns how many it handled.
pub async fn run_once(
    pool: &Pool,
    secrets: &dyn SecretStore,
    oauth: &OAuthSettings,
    extractor: &dyn Extractor,
) -> Result<usize> {
    let due = {
        let conn = queue::checkout(pool).await?;
        queue::claim(&conn, CLAIM_BATCH, LEASE_SECS).await?
    };
    for (tenant, connection) in &due {
        let outcome = match build(pool, secrets, oauth, *tenant, *connection).await {
            Ok(Some((connector, rotated))) => {
                let result = sync_connection(pool, *tenant, *connection, connector.as_ref(), extractor)
                    .await
                    .map(|_| ());
                // A provider that replaces refresh tokens as they are used (DocuSign) would
                // otherwise lock the connection out once the old one lapsed.
                if let Some(new) = rotated.and_then(|slot| slot.lock().unwrap().take()) {
                    secrets.put(*tenant, &refresh_token_name(*tenant, *connection), &new).await?;
                }
                result
            }
            Ok(None) => Err(Error::TenantMismatch),
            Err(error) => Err(error),
        };
        let conn = queue::checkout(pool).await?;
        match outcome {
            Ok(()) => queue::reschedule(&conn, *connection, NEXT_SYNC_SECS).await?,
            Err(Error::Connector(ConnectorError::RateLimited(wait))) => {
                queue::reschedule(&conn, *connection, wait.as_secs().max(30) as i64).await?
            }
            // Needs the administrator to reconnect, or the connection is gone: stop trying.
            Err(Error::Connector(ConnectorError::Unauthorized) | Error::TenantMismatch) => {
                queue::remove(&conn, *connection).await?
            }
            Err(error) => {
                tracing::warn!(%error, "sync failed");
                queue::reschedule(&conn, *connection, RETRY_AFTER_ERROR_SECS).await?
            }
        }
    }
    Ok(due.len())
}

/// Passages given a vector per firm per pass. Small, so syncing is never kept waiting for long.
const VECTORS_PER_PASS: i64 = 32;

/// Gives a meaning vector to passages that do not have one yet, a few at a time for each firm.
/// Returns how many it did.
pub async fn embed_pending(pool: &Pool, embedder: &dyn Embedder) -> Result<usize> {
    // Firms with a connection are the firms with passages. The queue is the one table the worker
    // may read across firms.
    let tenants: Vec<uuid::Uuid> = {
        let conn = queue::checkout(pool).await?;
        conn.query("SELECT DISTINCT tenant_id FROM sync_queue", &[])
            .await?
            .into_iter()
            .map(|row| row.get(0))
            .collect()
    };
    let mut done = 0;
    for tenant in tenants {
        let tenant = TenantId(tenant);
        let waiting = {
            let mut conn = queue::checkout(pool).await?;
            let tx = TenantTx::begin(&mut conn, tenant).await?;
            tx.passages_needing_vectors(embedder.model(), VECTORS_PER_PASS)
                .await?
        };
        if waiting.is_empty() {
            continue;
        }
        let texts: Vec<String> = waiting.iter().map(|(_, text)| text.clone()).collect();
        let vectors = embedder
            .embed_passages(texts)
            .await
            .map_err(|error| Error::Embedding(error.to_string()))?;
        let mut conn = queue::checkout(pool).await?;
        let tx = TenantTx::begin(&mut conn, tenant).await?;
        for ((id, _), vector) in waiting.iter().zip(&vectors) {
            tx.store_vector(*id, embedder.model(), vector).await?;
        }
        tx.commit().await?;
        done += waiting.len();
    }
    Ok(done)
}

/// Keeps giving passages vectors, alongside syncing and not after it: a sync of a large drive can
/// run for a long time, and passages should become searchable by meaning while it goes on.
async fn embed_forever(pool: Pool, embedder: Arc<dyn Embedder>) {
    loop {
        match embed_pending(&pool, embedder.as_ref()).await {
            Ok(0) => tokio::time::sleep(Duration::from_secs(10)).await,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "could not embed passages");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}

pub async fn run_forever(
    pool: Pool,
    secrets: &dyn SecretStore,
    oauth: &OAuthSettings,
    extractor: &dyn Extractor,
    embedder: Option<Arc<dyn Embedder>>,
    analyzer: Option<Arc<dyn crate::knowledge::Analyzer>>,
    agent_model: Option<Arc<dyn crate::agent_runner::AgentModel>>,
) {
    // Deletes agent work whose time is up.
    tokio::spawn(crate::retention::retention_forever(pool.clone()));
    // Starts the recurring tasks that have come due.
    tokio::spawn(crate::schedules::schedules_forever(pool.clone()));
    // Runs the tasks people give to shared agents.
    if let Some(model) = agent_model {
        tokio::spawn(crate::agent_runner::agents_forever(
            pool.clone(),
            model,
            embedder.clone(),
        ));
    }
    if let Some(embedder) = embedder {
        tokio::spawn(embed_forever(pool.clone(), embedder));
    }
    // Reads documents for company knowledge, for firms that have switched it on.
    if let Some(analyzer) = analyzer {
        tokio::spawn(crate::knowledge_worker::analyse_forever(
            pool.clone(),
            analyzer,
        ));
    }
    loop {
        match run_once(&pool, secrets, oauth, extractor).await {
            Ok(0) => tokio::time::sleep(Duration::from_secs(10)).await,
            Ok(_) => {}
            Err(error) => {
                tracing::error!(%error, "worker loop failed");
                tokio::time::sleep(Duration::from_secs(30)).await;
            }
        }
    }
}
