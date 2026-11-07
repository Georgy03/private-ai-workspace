//! Storing and reading company knowledge. Every read goes through the same rule as search: a
//! person sees an item only through the documents they may open, so a fact drawn from a restricted
//! file never reaches someone who cannot read that file.

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use crate::{
    Result,
    db::TenantTx,
    knowledge::{ANALYZER_VERSION, Extraction, ProfileDraft, ProfileInput, normalise},
};

/// A verified item is "stale" when nobody has confirmed it for this long.
pub const STALE_AFTER_DAYS: i64 = 180;
/// Failed readings are tried this many times before a document is left alone until it changes.
pub const MAX_ATTEMPTS: i32 = 3;
/// Documents are not retried sooner than this after a failure.
const RETRY_AFTER: &str = "30 minutes";

pub async fn analysis_enabled(tx: &TenantTx<'_>) -> Result<bool> {
    Ok(tx
        .sql()
        .query_opt("SELECT company_intelligence FROM firm_settings", &[])
        .await?
        .is_some_and(|row| row.get(0)))
}

pub async fn set_analysis_enabled(tx: &TenantTx<'_>, enabled: bool) -> Result<()> {
    tx.sql()
        .execute(
            "INSERT INTO firm_settings (tenant_id, company_intelligence) VALUES (app_tenant(), $1)
             ON CONFLICT (tenant_id) DO UPDATE SET company_intelligence = EXCLUDED.company_intelligence",
            &[&enabled],
        )
        .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct PendingDocument {
    pub id: Uuid,
    pub name: String,
    pub content_hash: String,
}

/// Documents that have not been read, whose version has changed since, that were read by an older
/// version of the instructions, or whose reading failed a while ago and may work now.
pub async fn documents_to_analyse(tx: &TenantTx<'_>, limit: i64) -> Result<Vec<PendingDocument>> {
    let sql = format!(
        "SELECT d.id, d.name, d.content_hash FROM documents d
         JOIN connections c ON c.id = d.connection_id AND c.visibility = 'workspace'
         LEFT JOIN document_analysis a ON a.tenant_id = d.tenant_id AND a.document_id = d.id
         WHERE d.status = 'indexed' AND (
               a.document_id IS NULL
            OR a.content_hash IS DISTINCT FROM d.content_hash
            OR a.analyzer_version < $1
            OR (a.status = 'failed' AND a.attempts < $2 AND a.analyzed_at < now() - interval '{RETRY_AFTER}'))
         ORDER BY a.analyzed_at NULLS FIRST, d.id LIMIT $3"
    );
    Ok(tx
        .sql()
        .query(&sql, &[&ANALYZER_VERSION, &MAX_ATTEMPTS, &limit])
        .await?
        .into_iter()
        .map(|row| PendingDocument {
            id: row.get(0),
            name: row.get(1),
            content_hash: row.get(2),
        })
        .collect())
}

/// The text of a document in reading order, cut at `limit` characters.
pub async fn document_text(tx: &TenantTx<'_>, document: Uuid, limit: usize) -> Result<String> {
    let rows = tx
        .sql()
        .query(
            "SELECT text FROM chunks WHERE document_id = $1 ORDER BY ordinal",
            &[&document],
        )
        .await?;
    let mut text = String::new();
    for row in rows {
        let piece: String = row.get(0);
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&piece);
        if text.chars().count() >= limit {
            break;
        }
    }
    Ok(text.chars().take(limit).collect())
}

pub async fn record_failure(
    tx: &TenantTx<'_>,
    document: Uuid,
    hash: &str,
    model: &str,
) -> Result<()> {
    tx.sql()
        .execute(
            "INSERT INTO document_analysis (tenant_id, document_id, content_hash, analyzer_version, status, attempts, model)
             VALUES (app_tenant(), $1, $2, $3, 'failed', 1, $4)
             ON CONFLICT (tenant_id, document_id) DO UPDATE SET
               attempts = CASE WHEN document_analysis.content_hash = EXCLUDED.content_hash
                               THEN document_analysis.attempts + 1 ELSE 1 END,
               content_hash = EXCLUDED.content_hash, analyzer_version = EXCLUDED.analyzer_version,
               status = 'failed', model = EXCLUDED.model, analyzed_at = now()",
            &[&document, &hash, &ANALYZER_VERSION, &model],
        )
        .await?;
    Ok(())
}

/// Replaces what one document contributes. Evidence from this document's earlier reading is
/// removed first, and anything left without evidence is cleaned up: a suggestion simply goes away,
/// but a fact a person had verified is kept and marked outdated so it is looked at again.
pub async fn store_analysis(
    tx: &TenantTx<'_>,
    document: Uuid,
    hash: &str,
    model: &str,
    cleaned: &Extraction,
) -> Result<()> {
    let sql = tx.sql();
    let previous: Option<String> = sql
        .query_opt(
            "SELECT content_hash FROM document_analysis WHERE document_id = $1",
            &[&document],
        )
        .await?
        .map(|row| row.get(0));
    if previous.as_deref().is_some_and(|before| before != hash) {
        // The source changed since a person confirmed things drawn from it.
        sql.execute(
            "UPDATE facts SET needs_review = true WHERE status = 'verified'
               AND id IN (SELECT fact_id FROM fact_evidence WHERE document_id = $1)",
            &[&document],
        )
        .await?;
    }
    for table in ["entity_evidence", "fact_evidence", "relationship_evidence"] {
        sql.execute(
            &format!("DELETE FROM {table} WHERE document_id = $1"),
            &[&document],
        )
        .await?;
    }
    sql.execute(
        "INSERT INTO document_analysis (tenant_id, document_id, content_hash, analyzer_version, status, attempts, doc_type, summary, model)
         VALUES (app_tenant(), $1, $2, $3, 'done', 0, $4, $5, $6)
         ON CONFLICT (tenant_id, document_id) DO UPDATE SET
           content_hash = EXCLUDED.content_hash, analyzer_version = EXCLUDED.analyzer_version,
           status = 'done', attempts = 0, doc_type = EXCLUDED.doc_type, summary = EXCLUDED.summary,
           model = EXCLUDED.model, analyzed_at = now()",
        &[&document, &hash, &ANALYZER_VERSION, &cleaned.doc_type, &cleaned.summary, &model],
    )
    .await?;

    let mut ids: HashMap<String, Uuid> = HashMap::new();
    for entity in &cleaned.entities {
        let roles: Vec<String> = if entity.role == "none" {
            Vec::new()
        } else {
            vec![entity.role.clone()]
        };
        let id: Uuid = sql
            .query_one(
                "INSERT INTO entities (tenant_id, kind, name, normalized_name, roles)
                 VALUES (app_tenant(), $1, $2, $3, $4)
                 ON CONFLICT (tenant_id, kind, normalized_name) DO UPDATE SET
                   last_seen_at = now(),
                   roles = (SELECT COALESCE(array_agg(DISTINCT r), '{}') FROM unnest(entities.roles || EXCLUDED.roles) r)
                 RETURNING id",
                &[&entity.kind, &entity.name, &normalise(&entity.name), &roles],
            )
            .await?
            .get(0);
        ids.insert(normalise(&entity.name), id);
        sql.execute(
            "INSERT INTO entity_evidence (tenant_id, entity_id, document_id, quote)
             VALUES (app_tenant(), $1, $2, $3)
             ON CONFLICT (tenant_id, entity_id, document_id) DO UPDATE SET quote = EXCLUDED.quote",
            &[&id, &document, &entity.quote],
        )
        .await?;
    }
    for fact in &cleaned.facts {
        let subject: Option<Uuid> = ids.get(&normalise(&fact.subject)).copied();
        let id: Uuid = sql
            .query_one(
                "INSERT INTO facts (tenant_id, kind, statement, normalized, subject_entity_id)
                 VALUES (app_tenant(), $1, $2, $3, $4)
                 ON CONFLICT (tenant_id, kind, normalized) DO UPDATE SET
                   last_seen_at = now(),
                   subject_entity_id = COALESCE(facts.subject_entity_id, EXCLUDED.subject_entity_id),
                   -- A fact that lost all its evidence and has found some again comes back, to be checked.
                   needs_review = facts.needs_review OR facts.status = 'outdated',
                   status = CASE WHEN facts.status = 'outdated' THEN 'verified' ELSE facts.status END
                 RETURNING id",
                &[&fact.kind, &fact.statement, &normalise(&fact.statement), &subject],
            )
            .await?
            .get(0);
        sql.execute(
            "INSERT INTO fact_evidence (tenant_id, fact_id, document_id, quote, content_hash)
             VALUES (app_tenant(), $1, $2, $3, $4)
             ON CONFLICT (tenant_id, fact_id, document_id) DO UPDATE SET
               quote = EXCLUDED.quote, content_hash = EXCLUDED.content_hash",
            &[&id, &document, &fact.quote, &hash],
        )
        .await?;
    }
    for link in &cleaned.relationships {
        let (Some(from), Some(to)) = (
            ids.get(&normalise(&link.subject)),
            ids.get(&normalise(&link.object)),
        ) else {
            continue;
        };
        let id: Uuid = sql
            .query_one(
                "INSERT INTO relationships (tenant_id, subject_entity_id, predicate, object_entity_id)
                 VALUES (app_tenant(), $1, $2, $3)
                 ON CONFLICT (tenant_id, subject_entity_id, predicate, object_entity_id)
                   DO UPDATE SET predicate = EXCLUDED.predicate
                 RETURNING id",
                &[from, &link.predicate, to],
            )
            .await?
            .get(0);
        sql.execute(
            "INSERT INTO relationship_evidence (tenant_id, relationship_id, document_id, quote)
             VALUES (app_tenant(), $1, $2, $3)
             ON CONFLICT (tenant_id, relationship_id, document_id) DO UPDATE SET quote = EXCLUDED.quote",
            &[&id, &document, &link.quote],
        )
        .await?;
    }
    // Clean up what no document supports any more.
    sql.execute(
        "UPDATE facts SET status = 'outdated', needs_review = true
         WHERE status = 'verified' AND NOT EXISTS (SELECT 1 FROM fact_evidence e WHERE e.fact_id = facts.id)",
        &[],
    )
    .await?;
    sql.execute(
        "DELETE FROM facts WHERE status = 'suggested'
           AND NOT EXISTS (SELECT 1 FROM fact_evidence e WHERE e.fact_id = facts.id)",
        &[],
    )
    .await?;
    sql.execute(
        "DELETE FROM relationships WHERE status = 'suggested'
           AND NOT EXISTS (SELECT 1 FROM relationship_evidence e WHERE e.relationship_id = relationships.id)",
        &[],
    )
    .await?;
    sql.execute(
        "DELETE FROM entities WHERE status = 'suggested'
           AND NOT EXISTS (SELECT 1 FROM entity_evidence e WHERE e.entity_id = entities.id)",
        &[],
    )
    .await?;
    Ok(())
}

// ---- Reading, always through the documents a person may open ----

#[derive(Debug, Clone, Serialize)]
pub struct Evidence {
    pub document_id: Uuid,
    pub document_name: String,
    pub quote: String,
    pub link: Option<String>,
    /// The document has changed since this quote was taken.
    pub source_changed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct EntityView {
    pub id: Uuid,
    pub kind: String,
    pub name: String,
    pub roles: Vec<String>,
    pub status: String,
    pub mentions: i64,
    pub evidence: Vec<Evidence>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FactView {
    pub id: Uuid,
    pub kind: String,
    pub statement: String,
    pub subject: Option<String>,
    pub status: String,
    pub needs_review: bool,
    /// Verified, but not confirmed for longer than the freshness period.
    pub stale: bool,
    pub verified_by: Option<String>,
    pub verified_age_days: Option<i64>,
    pub evidence: Vec<Evidence>,
}

const LINK: &str = "COALESCE(d.web_url, CASE WHEN cn.kind = 'google_drive' THEN 'https://drive.google.com/open?id=' || d.external_id END)";

async fn evidence_for(
    tx: &TenantTx<'_>,
    table: &str,
    key: &str,
    ids: &[Uuid],
    readers: &[String],
    with_hash: bool,
) -> Result<HashMap<Uuid, Vec<Evidence>>> {
    let changed = if with_hash {
        "(ev.content_hash IS DISTINCT FROM d.content_hash)"
    } else {
        "false"
    };
    let sql = format!(
        "SELECT ev.{key}, d.id, d.name, ev.quote, {LINK}, {changed}
         FROM {table} ev
         JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
         JOIN connections cn ON cn.id = d.connection_id
         WHERE ev.{key} = ANY($1) AND d.readable_by && $2::text[]
         ORDER BY d.name"
    );
    let mut grouped: HashMap<Uuid, Vec<Evidence>> = HashMap::new();
    for row in tx.sql().query(&sql, &[&ids, &readers]).await? {
        let owner: Uuid = row.get(0);
        let list = grouped.entry(owner).or_default();
        if list.len() < 5 {
            list.push(Evidence {
                document_id: row.get(1),
                document_name: row.get(2),
                quote: row.get(3),
                link: row.get(4),
                source_changed: row.get(5),
            });
        }
    }
    Ok(grouped)
}

pub async fn list_entities(
    tx: &TenantTx<'_>,
    readers: &[String],
    statuses: &[String],
    kind: Option<&str>,
    query: Option<&str>,
    limit: i64,
) -> Result<Vec<EntityView>> {
    let like = query.map(|q| format!("%{}%", normalise(q)));
    let rows = tx
        .sql()
        .query(
            "SELECT e.id, e.kind, e.name, e.roles, e.status, (
                SELECT count(*) FROM entity_evidence ev
                JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
                WHERE ev.entity_id = e.id AND d.readable_by && $1::text[]) AS mentions
             FROM entities e
             WHERE e.status = ANY($2) AND ($3::text IS NULL OR e.kind = $3)
               AND ($4::text IS NULL OR e.normalized_name LIKE $4)
               AND EXISTS (SELECT 1 FROM entity_evidence ev
                           JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
                           WHERE ev.entity_id = e.id AND d.readable_by && $1::text[])
             ORDER BY mentions DESC, e.name LIMIT $5",
            &[&readers, &statuses, &kind, &like, &limit],
        )
        .await?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.get(0)).collect();
    let mut evidence =
        evidence_for(tx, "entity_evidence", "entity_id", &ids, readers, false).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let id: Uuid = row.get(0);
            EntityView {
                id,
                kind: row.get(1),
                name: row.get(2),
                roles: row.get(3),
                status: row.get(4),
                mentions: row.get(5),
                evidence: evidence.remove(&id).unwrap_or_default(),
            }
        })
        .collect())
}

pub async fn list_facts(
    tx: &TenantTx<'_>,
    readers: &[String],
    statuses: &[String],
    kind: Option<&str>,
    only_needing_review: bool,
    limit: i64,
) -> Result<Vec<FactView>> {
    let rows = tx
        .sql()
        .query(
            &format!(
                "SELECT f.id, f.kind, f.statement, s.name, f.status, f.needs_review, f.verified_by,
                        EXTRACT(DAY FROM now() - f.verified_at)::bigint
                 FROM facts f LEFT JOIN entities s ON s.tenant_id = f.tenant_id AND s.id = f.subject_entity_id
                 WHERE f.status = ANY($2) AND ($3::text IS NULL OR f.kind = $3)
                   AND (NOT $4 OR f.needs_review OR f.status IN ('suggested', 'outdated')
                        OR (f.status = 'verified' AND f.verified_at < now() - interval '{STALE_AFTER_DAYS} days'))
                   AND EXISTS (SELECT 1 FROM fact_evidence ev
                               JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
                               WHERE ev.fact_id = f.id AND d.readable_by && $1::text[])
                 ORDER BY f.needs_review DESC, f.last_seen_at DESC, f.id LIMIT $5"
            ),
            &[&readers, &statuses, &kind, &only_needing_review, &limit],
        )
        .await?;
    let ids: Vec<Uuid> = rows.iter().map(|row| row.get(0)).collect();
    let mut evidence = evidence_for(tx, "fact_evidence", "fact_id", &ids, readers, true).await?;
    Ok(rows
        .into_iter()
        .map(|row| {
            let id: Uuid = row.get(0);
            let status: String = row.get(4);
            let age: Option<i64> = row.get(7);
            FactView {
                id,
                kind: row.get(1),
                statement: row.get(2),
                subject: row.get(3),
                stale: status == "verified" && age.is_some_and(|days| days >= STALE_AFTER_DAYS),
                status,
                needs_review: row.get(5),
                verified_by: row.get(6),
                verified_age_days: age,
                evidence: evidence.remove(&id).unwrap_or_default(),
            }
        })
        .collect())
}

// ---- Review: confirm, reject or correct ----

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Review {
    Confirm,
    Reject,
    /// Confirm with a corrected statement or name.
    Correct(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reviewed {
    Done,
    /// No such item, or none the reviewer can see evidence for.
    NotFound,
    /// The corrected text is already another item.
    Duplicate,
}

/// Reviews a fact. A person can only review what they can see, which means a document they may open
/// supports it.
pub async fn review_fact(
    tx: &TenantTx<'_>,
    readers: &[String],
    id: Uuid,
    reviewer: &str,
    review: &Review,
) -> Result<Reviewed> {
    let visible = "EXISTS (SELECT 1 FROM fact_evidence ev JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
                           WHERE ev.fact_id = facts.id AND d.readable_by && $2::text[])";
    let sql = tx.sql();
    let changed = match review {
        Review::Confirm => {
            sql.execute(
                &format!("UPDATE facts SET status = 'verified', needs_review = false, verified_by = $3, verified_at = now()
                          WHERE id = $1 AND {visible}"),
                &[&id, &readers, &reviewer],
            )
            .await?
        }
        Review::Reject => {
            sql.execute(
                &format!("UPDATE facts SET status = 'rejected', needs_review = false, verified_by = $3, verified_at = now()
                          WHERE id = $1 AND {visible}"),
                &[&id, &readers, &reviewer],
            )
            .await?
        }
        Review::Correct(statement) => {
            let normalized = normalise(statement);
            let taken = sql
                .query_opt(
                    "SELECT 1 FROM facts f WHERE f.id <> $1 AND f.normalized = $2
                       AND f.kind = (SELECT kind FROM facts WHERE id = $1)",
                    &[&id, &normalized],
                )
                .await?
                .is_some();
            if taken {
                return Ok(Reviewed::Duplicate);
            }
            sql.execute(
                &format!("UPDATE facts SET statement = $4, normalized = $5, status = 'verified', needs_review = false,
                                           verified_by = $3, verified_at = now()
                          WHERE id = $1 AND {visible}"),
                &[&id, &readers, &reviewer, &statement.trim(), &normalized],
            )
            .await?
        }
    };
    Ok(if changed == 0 {
        Reviewed::NotFound
    } else {
        Reviewed::Done
    })
}

pub async fn review_entity(
    tx: &TenantTx<'_>,
    readers: &[String],
    id: Uuid,
    reviewer: &str,
    review: &Review,
) -> Result<Reviewed> {
    let visible = "EXISTS (SELECT 1 FROM entity_evidence ev JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
                           WHERE ev.entity_id = entities.id AND d.readable_by && $2::text[])";
    let sql = tx.sql();
    let changed = match review {
        Review::Confirm => {
            sql.execute(
                &format!("UPDATE entities SET status = 'verified', verified_by = $3, verified_at = now() WHERE id = $1 AND {visible}"),
                &[&id, &readers, &reviewer],
            )
            .await?
        }
        Review::Reject => {
            sql.execute(
                &format!("UPDATE entities SET status = 'rejected', verified_by = $3, verified_at = now() WHERE id = $1 AND {visible}"),
                &[&id, &readers, &reviewer],
            )
            .await?
        }
        Review::Correct(name) => {
            let normalized = normalise(name);
            let taken = sql
                .query_opt(
                    "SELECT 1 FROM entities e WHERE e.id <> $1 AND e.normalized_name = $2
                       AND e.kind = (SELECT kind FROM entities WHERE id = $1)",
                    &[&id, &normalized],
                )
                .await?
                .is_some();
            if taken {
                return Ok(Reviewed::Duplicate);
            }
            sql.execute(
                &format!("UPDATE entities SET name = $4, normalized_name = $5, status = 'verified', verified_by = $3, verified_at = now()
                          WHERE id = $1 AND {visible}"),
                &[&id, &readers, &reviewer, &name.trim(), &normalized],
            )
            .await?
        }
    };
    Ok(if changed == 0 {
        Reviewed::NotFound
    } else {
        Reviewed::Done
    })
}

// ---- The company profile and the overview ----

#[derive(Debug, Clone, Serialize)]
pub struct ProfileView {
    pub summary: String,
    pub services: Vec<String>,
    pub terminology: serde_json::Value,
    pub status: String,
    pub based_on: i32,
    pub model: String,
    pub generated_epoch: f64,
    pub verified_by: Option<String>,
}

pub async fn profile(tx: &TenantTx<'_>) -> Result<Option<ProfileView>> {
    Ok(tx
        .sql()
        .query_opt(
            "SELECT summary, services, terminology, status, based_on, model,
                    EXTRACT(EPOCH FROM generated_at)::float8, verified_by FROM company_profile",
            &[],
        )
        .await?
        .map(|row| ProfileView {
            summary: row.get(0),
            services: row.get(1),
            terminology: row.get(2),
            status: row.get(3),
            based_on: row.get(4),
            model: row.get(5),
            generated_epoch: row.get(6),
            verified_by: row.get(7),
        }))
}

/// Stores a newly drafted profile, unless a person has already confirmed one.
pub async fn save_profile(
    tx: &TenantTx<'_>,
    draft: &ProfileDraft,
    based_on: i32,
    model: &str,
) -> Result<()> {
    let terminology = serde_json::to_value(&draft.terminology).unwrap_or_default();
    tx.sql()
        .execute(
            "INSERT INTO company_profile (tenant_id, summary, services, terminology, based_on, model)
             VALUES (app_tenant(), $1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id) DO UPDATE SET
               summary = EXCLUDED.summary, services = EXCLUDED.services, terminology = EXCLUDED.terminology,
               based_on = EXCLUDED.based_on, model = EXCLUDED.model, generated_at = now()
             WHERE company_profile.status <> 'verified'",
            &[&draft.summary, &draft.services, &terminology, &based_on, &model],
        )
        .await?;
    Ok(())
}

/// A person confirms the profile as it is, or with their corrections.
pub async fn review_profile(
    tx: &TenantTx<'_>,
    reviewer: &str,
    summary: Option<&str>,
    services: Option<&[String]>,
) -> Result<bool> {
    let changed = tx
        .sql()
        .execute(
            "UPDATE company_profile SET status = 'verified', verified_by = $1, verified_at = now(),
                    summary = COALESCE($2, summary), services = COALESCE($3, services)",
            &[&reviewer, &summary, &services],
        )
        .await?;
    Ok(changed > 0)
}

/// What the profile is drawn from, and how many documents have been read since it was written.
pub async fn profile_input(tx: &TenantTx<'_>) -> Result<(ProfileInput, i64)> {
    let sql = tx.sql();
    let documents = sql
        .query(
            "SELECT d.name, a.doc_type, a.summary FROM document_analysis a
             JOIN documents d ON d.tenant_id = a.tenant_id AND d.id = a.document_id
             JOIN connections c ON c.id = d.connection_id AND c.visibility = 'workspace'
             WHERE a.status = 'done' AND a.summary <> '' ORDER BY a.analyzed_at DESC LIMIT 120",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| {
            (
                row.get(0),
                row.get(1),
                row.get::<_, String>(2).chars().take(200).collect(),
            )
        })
        .collect();
    let entities = sql
        .query(
            "SELECT e.name, e.kind, count(ev.document_id) AS n FROM entities e
             JOIN entity_evidence ev ON ev.entity_id = e.id
             JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
             JOIN connections c ON c.id = d.connection_id AND c.visibility = 'workspace'
             WHERE e.status <> 'rejected' GROUP BY e.id ORDER BY n DESC, e.name LIMIT 40",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| (row.get(0), row.get(1), row.get(2)))
        .collect();
    let facts = sql
        .query(
            "SELECT f.statement FROM facts f WHERE f.status <> 'rejected'
               AND EXISTS (SELECT 1 FROM fact_evidence ev
                           JOIN documents d ON d.tenant_id = ev.tenant_id AND d.id = ev.document_id
                           JOIN connections c ON c.id = d.connection_id AND c.visibility = 'workspace'
                           WHERE ev.fact_id = f.id)
             ORDER BY f.last_seen_at DESC LIMIT 40",
            &[],
        )
        .await?
        .into_iter()
        .map(|row| row.get(0))
        .collect();
    let new_since: i64 = sql
        .query_one(
            "SELECT count(*) FROM document_analysis a WHERE a.status = 'done'
               AND a.analyzed_at > COALESCE((SELECT generated_at FROM company_profile), '-infinity')",
            &[],
        )
        .await?
        .get(0);
    Ok((
        ProfileInput {
            documents,
            entities,
            facts,
        },
        new_since,
    ))
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct Overview {
    pub documents_total: i64,
    pub documents_analysed: i64,
    pub documents_failed: i64,
    pub document_types: Vec<(String, i64)>,
    pub entities_by_kind: Vec<(String, i64)>,
    pub entities_unverified: i64,
    pub facts_total: i64,
    pub facts_verified: i64,
    pub facts_unverified: i64,
    pub facts_needing_review: i64,
    pub facts_stale: i64,
    pub relationships: i64,
}

/// Counts for the knowledge profile screen. These are drawn from every document in the firm's
/// connected sources, so they are for administrators only.
pub async fn overview(tx: &TenantTx<'_>) -> Result<Overview> {
    let sql = tx.sql();
    let one = |row: tokio_postgres::Row| -> Vec<i64> {
        (0..row.len()).map(|i| row.get::<_, i64>(i)).collect()
    };
    let docs = one(sql
        .query_one(
            "SELECT (SELECT count(*) FROM documents d JOIN connections c ON c.id = d.connection_id
                      WHERE d.status = 'indexed' AND c.visibility = 'workspace'),
                    (SELECT count(*) FROM document_analysis a JOIN documents d ON d.tenant_id = a.tenant_id AND d.id = a.document_id
                      JOIN connections c ON c.id = d.connection_id AND c.visibility = 'workspace' WHERE a.status = 'done'),
                    (SELECT count(*) FROM document_analysis a JOIN documents d ON d.tenant_id = a.tenant_id AND d.id = a.document_id
                      JOIN connections c ON c.id = d.connection_id AND c.visibility = 'workspace' WHERE a.status = 'failed')",
            &[],
        )
        .await?);
    let facts = one(
        sql.query_one(
            &format!(
                "SELECT count(*) FILTER (WHERE status <> 'rejected'),
                        count(*) FILTER (WHERE status = 'verified'),
                        count(*) FILTER (WHERE status = 'suggested'),
                        count(*) FILTER (WHERE needs_review OR status = 'outdated'),
                        count(*) FILTER (WHERE status = 'verified' AND verified_at < now() - interval '{STALE_AFTER_DAYS} days')
                 FROM facts"
            ),
            &[],
        )
        .await?,
    );
    let pairs = |rows: Vec<tokio_postgres::Row>| {
        rows.into_iter()
            .map(|row| (row.get::<_, String>(0), row.get::<_, i64>(1)))
            .collect()
    };
    Ok(Overview {
        documents_total: docs[0],
        documents_analysed: docs[1],
        documents_failed: docs[2],
        document_types: pairs(
            sql.query("SELECT doc_type, count(*) FROM document_analysis WHERE status = 'done' GROUP BY 1 ORDER BY 2 DESC, 1", &[]).await?,
        ),
        entities_by_kind: pairs(
            sql.query("SELECT kind, count(*) FROM entities WHERE status <> 'rejected' GROUP BY 1 ORDER BY 2 DESC, 1", &[]).await?,
        ),
        entities_unverified: sql.query_one("SELECT count(*) FROM entities WHERE status = 'suggested'", &[]).await?.get(0),
        facts_total: facts[0],
        facts_verified: facts[1],
        facts_unverified: facts[2],
        facts_needing_review: facts[3],
        facts_stale: facts[4],
        relationships: sql.query_one("SELECT count(*) FROM relationships WHERE status <> 'rejected'", &[]).await?.get(0),
    })
}

/// Removes what was learned from files that are now private to one person, so nothing from them
/// reaches the firm-wide profile, counts or lists. An unconfirmed suggestion left with no
/// supporting file disappears; a confirmed one is kept and marked outdated, as when a file changes.
pub async fn forget_private(tx: &TenantTx<'_>) -> Result<u64> {
    let sql = tx.sql();
    let private = "SELECT d.id FROM documents d JOIN connections c ON c.id = d.connection_id WHERE c.visibility = 'private'";
    let mut removed = 0;
    for table in ["fact_evidence", "entity_evidence", "relationship_evidence", "document_analysis"] {
        removed += sql.execute(&format!("DELETE FROM {table} WHERE document_id IN ({private})"), &[]).await?;
    }
    if removed > 0 {
        sql.execute("DELETE FROM facts f WHERE f.status = 'suggested' AND NOT EXISTS (SELECT 1 FROM fact_evidence ev WHERE ev.fact_id = f.id)", &[]).await?;
        sql.execute("UPDATE facts f SET status = 'outdated' WHERE f.status = 'verified' AND NOT EXISTS (SELECT 1 FROM fact_evidence ev WHERE ev.fact_id = f.id)", &[]).await?;
        sql.execute("DELETE FROM relationships r WHERE NOT EXISTS (SELECT 1 FROM relationship_evidence ev WHERE ev.relationship_id = r.id)", &[]).await?;
        sql.execute("DELETE FROM entities e WHERE e.status = 'suggested' AND NOT EXISTS (SELECT 1 FROM entity_evidence ev WHERE ev.entity_id = e.id)", &[]).await?;
    }
    Ok(removed)
}
