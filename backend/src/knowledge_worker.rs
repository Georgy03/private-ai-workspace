//! The background loop that reads documents for company knowledge. It only works for firms whose
//! administrator has switched analysis on, a few documents at a time and alongside syncing.

use std::{sync::Arc, time::Duration};

use deadpool_postgres::Pool;
use futures_util::StreamExt;
use uuid::Uuid;

use crate::{
    Error, Result,
    auth::TenantId,
    db::TenantTx,
    knowledge::{Analyzer, MAX_DOCUMENT_CHARS, clean},
    knowledge_store as store, queue,
};

/// Documents read per firm per pass, and how many are with the model at once.
const BATCH: i64 = 6;
const CONCURRENCY: usize = 3;
/// A profile needs at least this many documents read before it says anything.
const MIN_DOCUMENTS_FOR_PROFILE: usize = 10;
/// A drafted profile is rewritten after this many more documents have been read.
const REFRESH_AFTER_DOCUMENTS: i64 = 25;

/// Reads one batch of documents for each firm that has analysis on. Returns how many it read.
pub async fn analyse_pass(pool: &Pool, analyzer: &dyn Analyzer) -> Result<usize> {
    // Firms with a connection are the firms with documents; the queue is the one table the worker
    // may read across firms.
    let tenants: Vec<Uuid> = {
        let conn = queue::checkout(pool).await?;
        conn.query("SELECT DISTINCT tenant_id FROM sync_queue", &[])
            .await?
            .into_iter()
            .map(|row| row.get(0))
            .collect()
    };
    let mut read = 0;
    for tenant in tenants {
        let tenant = TenantId(tenant);
        let pending = {
            let mut conn = queue::checkout(pool).await?;
            let tx = TenantTx::begin(&mut conn, tenant).await?;
            // Whatever was learned from a file that has since become private to one person goes.
            if store::forget_private(&tx).await? > 0 {
                tx.commit().await?;
                continue;
            }
            if !store::analysis_enabled(&tx).await? {
                continue;
            }
            let mut pending = Vec::new();
            for document in store::documents_to_analyse(&tx, BATCH).await? {
                let text = store::document_text(&tx, document.id, MAX_DOCUMENT_CHARS).await?;
                pending.push((document, text));
            }
            pending
        };
        if pending.is_empty() {
            refresh_profile(pool, tenant, analyzer).await?;
            continue;
        }
        let results: Vec<_> = futures_util::stream::iter(pending)
            .map(|(document, text)| async move {
                let outcome = analyzer
                    .analyze(&document.name, &text)
                    .await
                    .map(|raw| clean(&text, raw));
                (document, outcome)
            })
            .buffer_unordered(CONCURRENCY)
            .collect()
            .await;
        for (document, outcome) in results {
            let mut conn = queue::checkout(pool).await?;
            let tx = TenantTx::begin(&mut conn, tenant).await?;
            let stored = match outcome {
                Ok(cleaned) => {
                    store::store_analysis(
                        &tx,
                        document.id,
                        &document.content_hash,
                        analyzer.model(),
                        &cleaned,
                    )
                    .await
                }
                Err(error) => {
                    tracing::warn!(%error, "could not read a document for company knowledge");
                    store::record_failure(
                        &tx,
                        document.id,
                        &document.content_hash,
                        analyzer.model(),
                    )
                    .await
                }
            };
            match stored {
                // The document may have been removed while it was being read; that is not a failure.
                Err(error) => tracing::warn!(%error, "could not save what a document says"),
                Ok(()) => {
                    tx.commit().await?;
                    read += 1;
                }
            }
        }
    }
    Ok(read)
}

/// Writes the company profile when enough has been read to say something, and again as more is
/// read. A profile an administrator has confirmed is left alone.
async fn refresh_profile(pool: &Pool, tenant: TenantId, analyzer: &dyn Analyzer) -> Result<()> {
    let (input, new_since) = {
        let mut conn = queue::checkout(pool).await?;
        let tx = TenantTx::begin(&mut conn, tenant).await?;
        let current = store::profile(&tx).await?;
        let (input, new_since) = store::profile_input(&tx).await?;
        let wanted = input.documents.len() >= MIN_DOCUMENTS_FOR_PROFILE
            && match current {
                None => true,
                Some(profile) => {
                    profile.status != "verified" && new_since >= REFRESH_AFTER_DOCUMENTS
                }
            };
        if !wanted {
            return Ok(());
        }
        (input, new_since)
    };
    let draft = analyzer
        .profile(&input)
        .await
        .map_err(|error| Error::Analysis(error.to_string()))?;
    let mut conn = queue::checkout(pool).await?;
    let tx = TenantTx::begin(&mut conn, tenant).await?;
    store::save_profile(&tx, &draft, input.documents.len() as i32, analyzer.model()).await?;
    tx.commit().await?;
    tracing::info!(new_since, "company profile drafted");
    Ok(())
}

pub async fn analyse_forever(pool: Pool, analyzer: Arc<dyn Analyzer>) {
    loop {
        match analyse_pass(&pool, analyzer.as_ref()).await {
            Ok(0) => tokio::time::sleep(Duration::from_secs(20)).await,
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "company knowledge pass failed");
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
        }
    }
}
