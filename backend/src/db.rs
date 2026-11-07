//! Database access that is always scoped to one firm.
//!
//! A `TenantTx` is a transaction opened for a single firm. The firm is set with
//! `set_config(..., true)`, which lasts only until the transaction ends, so a pooled connection
//! cannot carry one firm's identity into the next request. Row-level security in the database
//! (see migrations/0001_tenancy.sql) does the actual filtering.

use std::collections::HashMap;

use serde_json::Value;
use tokio_postgres::{Client, Transaction};
use uuid::Uuid;

use crate::{
    Error, Result,
    auth::{Principal, TenantId},
};

const MIGRATIONS: &[(i32, &str)] = &[
    (1, include_str!("../migrations/0001_tenancy.sql")),
    (2, include_str!("../migrations/0002_connectors.sql")),
    (3, include_str!("../migrations/0003_extraction.sql")),
    (4, include_str!("../migrations/0004_reconnect.sql")),
    (5, include_str!("../migrations/0005_name_search.sql")),
    (6, include_str!("../migrations/0006_web_links.sql")),
    (7, include_str!("../migrations/0007_sync_progress.sql")),
    (8, include_str!("../migrations/0008_read_attempts.sql")),
    (9, include_str!("../migrations/0009_usage.sql")),
    (10, include_str!("../migrations/0010_embeddings.sql")),
    (11, include_str!("../migrations/0011_company_knowledge.sql")),
    (12, include_str!("../migrations/0012_agents.sql")),
    (13, include_str!("../migrations/0013_agent_checks.sql")),
    (14, include_str!("../migrations/0014_schedules.sql")),
    (15, include_str!("../migrations/0015_retention.sql")),
    (16, include_str!("../migrations/0016_more_connectors.sql")),
    (17, include_str!("../migrations/0017_connection_scope.sql")),
    (18, include_str!("../migrations/0018_agent_actions.sql")),
    (19, include_str!("../migrations/0019_outlook_sending.sql")),
    (20, include_str!("../migrations/0020_drive_saving.sql")),
    (21, include_str!("../migrations/0021_quickbooks.sql")),
    (22, include_str!("../migrations/0022_one_google_drive.sql")),
    (23, include_str!("../migrations/0023_zoom.sql")),
    (24, include_str!("../migrations/0024_zoom_meetings.sql")),
    (25, include_str!("../migrations/0025_connector_secrets.sql")),
];

/// Words that only make a question a question. The database drops ordinary stop words ("the",
/// "about", "what") itself, but not these.
const QUESTION_WORDS: &[&str] = &[
    "tell",
    "show",
    "give",
    "find",
    "explain",
    "describe",
    "please",
    "could",
    "would",
    "can",
    "know",
    "want",
    "need",
    "looking",
    "look",
    "search",
    "summarize",
    "summary",
    "overview",
];

/// Most passages from any one file in a set of results, so one long file cannot fill the list.
const MAX_PASSAGES_PER_FILE: i64 = 3;
/// Passages written per statement when a document is stored.
const CHUNKS_PER_INSERT: usize = 500;

/// Turns a question into a search query that matches any of its content words. Documents that
/// match more of them rank higher. Returns None when the question holds nothing to search for.
///
/// Every term is made only of letters and digits and is quoted, so nothing in a question can
/// change the structure of the query.
pub(crate) fn search_query(question: &str) -> Option<String> {
    let words: Vec<String> = question
        .split(|c: char| !c.is_alphanumeric())
        .map(str::to_lowercase)
        .filter(|word| word.chars().count() >= 2 && !QUESTION_WORDS.contains(&word.as_str()))
        .take(16)
        .collect();
    if words.is_empty() {
        return None;
    }
    // "zero dock" also searches "zerodock": names are written with and without the space.
    let joined = words
        .windows(2)
        .map(|pair| format!("{}{}", pair[0], pair[1]));
    let mut terms: Vec<String> = words.iter().cloned().chain(joined).collect();
    terms.dedup();
    Some(
        terms
            .iter()
            .map(|term| format!("'{term}'"))
            .collect::<Vec<_>>()
            .join(" | "),
    )
}

/// Applies pending migrations. Run by the deployment role, never by the application roles.
pub async fn migrate(client: &mut Client) -> Result<()> {
    client
        .batch_execute("CREATE TABLE IF NOT EXISTS schema_migrations (version integer PRIMARY KEY)")
        .await?;
    for (version, sql) in MIGRATIONS {
        let applied = client
            .query_opt(
                "SELECT 1 FROM schema_migrations WHERE version = $1",
                &[version],
            )
            .await?
            .is_some();
        if applied {
            continue;
        }
        let tx = client.transaction().await?;
        tx.batch_execute(sql).await?;
        tx.execute(
            "INSERT INTO schema_migrations (version) VALUES ($1)",
            &[version],
        )
        .await?;
        tx.commit().await?;
    }
    Ok(())
}

/// Creates a firm. Used by the provisioner role.
pub async fn provision_tenant(
    client: &Client,
    name: &str,
    kms_key_arn: Option<&str>,
) -> Result<TenantId> {
    let id = Uuid::new_v4();
    client
        .execute(
            "INSERT INTO tenants (id, name, kms_key_arn) VALUES ($1, $2, $3)",
            &[&id, &name, &kms_key_arn],
        )
        .await?;
    Ok(TenantId(id))
}

/// Deletes a firm and, through cascading keys, everything stored for it.
pub async fn offboard_tenant(client: &Client, tenant: TenantId) -> Result<()> {
    client
        .execute("DELETE FROM tenants WHERE id = $1", &[&tenant.0])
        .await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct Hit {
    /// Where in the file the passage sits ("Page 3", "Slide 2"), when known.
    pub location_label: Option<String>,
    pub chunk_id: Uuid,
    pub document_id: Uuid,
    pub document_name: String,
    pub text: String,
    pub score: f32,
    /// Address that opens the original file in its own service, when known.
    pub web_url: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct NewDocument {
    pub connection_id: Uuid,
    pub external_id: String,
    pub name: String,
    pub path: String,
    pub readable_by: Vec<String>,
}

pub struct TenantTx<'a> {
    tx: Transaction<'a>,
    tenant: TenantId,
}

impl<'a> TenantTx<'a> {
    pub async fn begin(client: &'a mut Client, tenant: TenantId) -> Result<Self> {
        let tx = client.transaction().await?;
        tx.execute(
            "SELECT set_config('app.tenant_id', $1, true)",
            &[&tenant.0.to_string()],
        )
        .await?;
        Ok(Self { tx, tenant })
    }

    pub fn tenant(&self) -> TenantId {
        self.tenant
    }

    /// The firm's status ("active" or "suspended"), or None if the firm no longer exists.
    pub async fn status(&self) -> Result<Option<String>> {
        let row = self
            .tx
            .query_opt("SELECT status FROM tenants WHERE id = app_tenant()", &[])
            .await?;
        Ok(row.map(|row| row.get(0)))
    }

    /// The underlying transaction, for statements this wrapper does not cover.
    pub fn sql(&self) -> &Transaction<'a> {
        &self.tx
    }

    pub async fn commit(self) -> Result<()> {
        self.tx.commit().await?;
        Ok(())
    }

    /// Searches this firm's passages and file names for any of the question's content words,
    /// limited to files the principal may read. Matches in a file's name rank above matches in
    /// its text, and no file contributes more than a few passages.
    pub async fn search(&self, principal: &Principal, query: &str, limit: i64) -> Result<Vec<Hit>> {
        if principal.tenant != self.tenant {
            return Err(Error::TenantMismatch);
        }
        let Some(tsquery) = search_query(query) else {
            return Ok(Vec::new());
        };
        let rows = self
            .tx
            .query(
                "WITH q AS (SELECT to_tsquery('english', $1) AS query),
                 candidates AS (
                   SELECT c.id AS chunk_id, d.id AS doc_id, d.name, c.text,
                          c.location ->> 'label' AS label,
                          ts_rank_cd(c.tsv, q.query)
                            + CASE WHEN d.name_tsv @@ q.query THEN 1.0::float4 ELSE 0::float4 END AS score
                   FROM chunks c
                   JOIN documents d ON d.tenant_id = c.tenant_id AND d.id = c.document_id
                   CROSS JOIN q
                   WHERE c.tsv @@ q.query AND d.readable_by && $2::text[]
                   UNION ALL
                   -- Files matched by name, shown by their first passage.
                   SELECT c.id, d.id, d.name, c.text, c.location ->> 'label',
                          1.0::float4 + ts_rank_cd(d.name_tsv, q.query)
                   FROM documents d
                   JOIN chunks c ON c.tenant_id = d.tenant_id AND c.document_id = d.id AND c.ordinal = 0
                   CROSS JOIN q
                   WHERE d.name_tsv @@ q.query AND d.readable_by && $2::text[]
                 ),
                 best AS (
                   SELECT chunk_id, doc_id, name, text, label, max(score) AS score
                   FROM candidates GROUP BY chunk_id, doc_id, name, text, label
                 ),
                 ranked AS (
                   SELECT *, row_number() OVER (PARTITION BY doc_id ORDER BY score DESC, chunk_id) AS nth
                   FROM best
                 )
                 SELECT r.chunk_id, r.doc_id, r.name, r.text, r.score, r.label,
                        COALESCE(d.web_url,
                                 CASE WHEN cn.kind = 'google_drive'
                                      THEN 'https://drive.google.com/open?id=' || d.external_id END)
                 FROM ranked r
                 JOIN documents d ON d.id = r.doc_id
                 JOIN connections cn ON cn.id = d.connection_id
                 WHERE r.nth <= $4
                 ORDER BY r.score DESC, r.chunk_id
                 LIMIT $3",
                &[&tsquery, &principal.readers(), &limit, &MAX_PASSAGES_PER_FILE],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| Hit {
                chunk_id: row.get(0),
                document_id: row.get(1),
                document_name: row.get(2),
                text: row.get(3),
                score: row.get(4),
                location_label: row.get(5),
                web_url: row.get(6),
            })
            .collect())
    }

    /// The opening passages of each of these files that the principal may read, for when a question
    /// about a report matches nothing in particular. The files are named by the report, but only the
    /// ones this person can open today are returned.
    pub async fn opening_passages(
        &self,
        principal: &Principal,
        documents: &[Uuid],
        per_file: i64,
        limit: i64,
    ) -> Result<Vec<Hit>> {
        if principal.tenant != self.tenant {
            return Err(Error::TenantMismatch);
        }
        let rows = self
            .tx
            .query(
                "SELECT c.id, d.id, d.name, c.text, c.location ->> 'label',
                        COALESCE(d.web_url,
                                 CASE WHEN cn.kind = 'google_drive'
                                      THEN 'https://drive.google.com/open?id=' || d.external_id END)
                 FROM chunks c
                 JOIN documents d ON d.tenant_id = c.tenant_id AND d.id = c.document_id
                 JOIN connections cn ON cn.id = d.connection_id
                 WHERE d.id = ANY($1) AND d.readable_by && $2::text[] AND c.ordinal < $3
                 ORDER BY c.ordinal, d.id
                 LIMIT $4",
                &[&documents, &principal.readers(), &(per_file as i32), &limit],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| Hit {
                chunk_id: row.get(0),
                document_id: row.get(1),
                document_name: row.get(2),
                text: row.get(3),
                score: 0.0,
                location_label: row.get(4),
                web_url: row.get(5),
            })
            .collect())
    }

    /// The whole text of one document, in order, when the principal may read it. The text is left
    /// out (and `too_long` set) when it is longer than `max_chars`, so a very long file is never
    /// loaded just to be refused. None when there is no such document or it is not theirs to read.
    pub async fn document_text(
        &self,
        principal: &Principal,
        document: Uuid,
        max_chars: i64,
    ) -> Result<Option<DocumentText>> {
        if principal.tenant != self.tenant {
            return Err(Error::TenantMismatch);
        }
        let Some(head) = self
            .tx
            .query_opt(
                "SELECT d.name,
                        COALESCE(d.web_url,
                                 CASE WHEN cn.kind = 'google_drive'
                                      THEN 'https://drive.google.com/open?id=' || d.external_id END),
                        (SELECT COALESCE(sum(length(c.text)), 0) FROM chunks c WHERE c.document_id = d.id)::bigint
                 FROM documents d JOIN connections cn ON cn.id = d.connection_id
                 WHERE d.id = $1 AND d.readable_by && $2::text[]",
                &[&document, &principal.readers()],
            )
            .await?
        else {
            return Ok(None);
        };
        let total_chars: i64 = head.get(2);
        let too_long = total_chars > max_chars;
        let passages = if too_long {
            Vec::new()
        } else {
            self.tx
                .query(
                    "SELECT location ->> 'label', text FROM chunks
                     WHERE document_id = $1 ORDER BY ordinal",
                    &[&document],
                )
                .await?
                .into_iter()
                .map(|row| (row.get(0), row.get(1)))
                .collect()
        };
        Ok(Some(DocumentText {
            name: head.get(0),
            web_url: head.get(1),
            total_chars,
            too_long,
            passages,
        }))
    }

    /// Records who did what. Metadata only: never document text, questions or tokens.
    pub async fn audit(&self, actor: &str, action: &str, detail: &Value) -> Result<()> {
        self.tx
            .execute(
                "INSERT INTO audit_events (tenant_id, actor, action, detail)
                 VALUES (app_tenant(), $1, $2, $3)",
                &[&actor, &action, detail],
            )
            .await?;
        Ok(())
    }

    pub async fn add_connection(&self, kind: &str, secret_ref: &str) -> Result<Uuid> {
        let row = self
            .tx
            .query_one(
                "INSERT INTO connections (tenant_id, kind, secret_ref)
                 VALUES (app_tenant(), $1, $2) RETURNING id",
                &[&kind, &secret_ref],
            )
            .await?;
        Ok(row.get(0))
    }

    pub async fn upsert_document(&self, document: &NewDocument) -> Result<Uuid> {
        let row = self
            .tx
            .query_one(
                "INSERT INTO documents (tenant_id, connection_id, external_id, name, path, readable_by)
                 VALUES (app_tenant(), $1, $2, $3, $4, $5)
                 ON CONFLICT (tenant_id, connection_id, external_id)
                 DO UPDATE SET name = EXCLUDED.name, path = EXCLUDED.path,
                               readable_by = EXCLUDED.readable_by
                 RETURNING id",
                &[
                    &document.connection_id,
                    &document.external_id,
                    &document.name,
                    &document.path,
                    &document.readable_by,
                ],
            )
            .await?;
        Ok(row.get(0))
    }

    pub async fn add_chunk(&self, document: Uuid, ordinal: i32, text: &str) -> Result<()> {
        self.tx
            .execute(
                "INSERT INTO chunks (tenant_id, document_id, ordinal, text)
                 VALUES (app_tenant(), $1, $2, $3)",
                &[&document, &ordinal, &text],
            )
            .await?;
        Ok(())
    }

    // ---- Connector sync (worker role) ----

    pub async fn connection(&self, id: Uuid) -> Result<Option<ConnectionState>> {
        let row = self
            .tx
            .query_opt(
                "SELECT kind, config, cursor, status,
                        EXTRACT(EPOCH FROM now() - permissions_refreshed_at)::float8, visibility, owner
                 FROM connections WHERE id = $1",
                &[&id],
            )
            .await?;
        Ok(row.map(|row| ConnectionState {
            kind: row.get(0),
            config: row.get(1),
            cursor: row.get(2),
            status: row.get(3),
            full_listing_age_secs: row.get(4),
            visibility: row.get(5),
            owner: row.get(6),
        }))
    }

    /// For a private connection, the one person its files are for. Called in the same transaction
    /// as every write of a file, and it holds the setting still until that transaction ends, so
    /// switching a connection to private waits for a write in progress and cannot be missed by one.
    pub async fn private_owner(&self, connection: Uuid) -> Result<Option<String>> {
        Ok(self
            .tx
            .query_opt("SELECT visibility, owner FROM connections WHERE id = $1 FOR SHARE", &[&connection])
            .await?
            .and_then(|row| (row.get::<_, String>(0) == "private").then(|| row.get::<_, Option<String>>(1)).flatten()))
    }

    /// The database clock as seconds since 1970, for marking the start of a full listing.
    pub async fn clock(&self) -> Result<f64> {
        Ok(self
            .tx
            .query_one("SELECT EXTRACT(EPOCH FROM clock_timestamp())::float8", &[])
            .await?
            .get(0))
    }

    pub async fn existing_documents(
        &self,
        connection: Uuid,
        external_ids: &[String],
    ) -> Result<HashMap<String, ExistingDocument>> {
        let rows = self
            .tx
            .query(
                "SELECT external_id, content_hash, status, parser_version, read_attempts FROM documents
                 WHERE connection_id = $1 AND external_id = ANY($2)",
                &[&connection, &external_ids],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| {
                (
                    row.get::<_, String>(0),
                    ExistingDocument {
                        version: row.get(1),
                        status: row.get(2),
                        parser_version: row.get(3),
                        read_attempts: row.get(4),
                    },
                )
            })
            .collect())
    }

    pub async fn store_document(&self, document: &StoredDocument) -> Result<Uuid> {
        let row = self
            .tx
            .query_one(
                "INSERT INTO documents (tenant_id, connection_id, external_id, name, path,
                                        mime_type, modified_at, content_hash, readable_by, status,
                                        parser_version, web_url, note, read_attempts)
                 VALUES (app_tenant(), $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13)
                 ON CONFLICT (tenant_id, connection_id, external_id) DO UPDATE SET
                   name = EXCLUDED.name, path = EXCLUDED.path, mime_type = EXCLUDED.mime_type,
                   modified_at = EXCLUDED.modified_at, content_hash = EXCLUDED.content_hash,
                   readable_by = EXCLUDED.readable_by, status = EXCLUDED.status,
                   parser_version = EXCLUDED.parser_version, web_url = EXCLUDED.web_url,
                   note = EXCLUDED.note, read_attempts = EXCLUDED.read_attempts,
                   synced_at = now()
                 RETURNING id",
                &[
                    &document.connection_id,
                    &document.external_id,
                    &document.name,
                    &document.path,
                    &document.mime_type,
                    &document.modified_at,
                    &document.version,
                    &document.readable_by,
                    &document.status,
                    &document.parser_version,
                    &document.web_url,
                    &document.note,
                    &document.read_attempts,
                ],
            )
            .await?;
        Ok(row.get(0))
    }

    pub async fn replace_chunks(&self, document: Uuid, chunks: &[Passage]) -> Result<()> {
        self.tx
            .execute("DELETE FROM chunks WHERE document_id = $1", &[&document])
            .await?;
        if chunks.is_empty() {
            return Ok(());
        }
        // A very long document is stored in batches, so no single statement carries hundreds of
        // megabytes.
        for (batch, group) in chunks.chunks(CHUNKS_PER_INSERT).enumerate() {
            // Postgres text cannot hold a NUL byte, and file readers can produce one.
            let texts: Vec<String> = group.iter().map(|c| c.text.replace('\0', "")).collect();
            let labels: Vec<Option<&str>> = group.iter().map(|c| c.label.as_deref()).collect();
            let first = (batch * CHUNKS_PER_INSERT) as i32;
            self.tx
                .execute(
                    "INSERT INTO chunks (tenant_id, document_id, ordinal, text, location)
                     SELECT app_tenant(), $1, $4 + (u.n - 1)::int, u.t,
                            CASE WHEN u.l IS NULL THEN '{}'::jsonb ELSE jsonb_build_object('label', u.l) END
                     FROM unnest($2::text[], $3::text[]) WITH ORDINALITY AS u(t, l, n)",
                    &[&document, &texts, &labels, &first],
                )
                .await?;
        }
        Ok(())
    }

    /// Whether any of the connection's documents were read by an older version of the reader.
    pub async fn needs_reparse(&self, connection: Uuid, current_version: i32) -> Result<bool> {
        Ok(self
            .tx
            .query_one(
                "SELECT EXISTS (SELECT 1 FROM documents WHERE connection_id = $1 AND parser_version < $2)",
                &[&connection, &current_version],
            )
            .await?
            .get(0))
    }

    /// Updates who may read a document and marks it as seen, without touching its text.
    pub async fn refresh_document(
        &self,
        connection: Uuid,
        external_id: &str,
        readable_by: &[String],
        web_url: Option<&str>,
    ) -> Result<()> {
        self.tx
            .execute(
                "UPDATE documents SET readable_by = $3, web_url = COALESCE($4, web_url),
                                      synced_at = now()
                 WHERE connection_id = $1 AND external_id = $2",
                &[&connection, &external_id, &readable_by, &web_url],
            )
            .await?;
        Ok(())
    }

    pub async fn delete_document(&self, connection: Uuid, external_id: &str) -> Result<u64> {
        Ok(self
            .tx
            .execute(
                "DELETE FROM documents WHERE connection_id = $1 AND external_id = $2",
                &[&connection, &external_id],
            )
            .await?)
    }

    /// Removes everything below a folder, for providers that report a deleted folder as one event.
    /// `prefix` is a folder path; only items inside it go, never a sibling that shares its start.
    pub async fn delete_tree(&self, connection: Uuid, prefix: &str) -> Result<u64> {
        let escaped = prefix.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_");
        Ok(self
            .tx
            .execute(
                "DELETE FROM documents WHERE connection_id = $1 AND external_id LIKE $2 || '/%' ESCAPE '\\'",
                &[&connection, &escaped],
            )
            .await?)
    }

    /// Removes documents a full listing did not see.
    pub async fn sweep(&self, connection: Uuid, listing_started: f64) -> Result<u64> {
        Ok(self
            .tx
            .execute(
                "DELETE FROM documents WHERE connection_id = $1 AND synced_at < to_timestamp($2)",
                &[&connection, &listing_started],
            )
            .await?)
    }

    pub async fn set_cursor(&self, connection: Uuid, cursor: Option<&str>) -> Result<()> {
        self.tx
            .execute(
                "UPDATE connections SET cursor = $2 WHERE id = $1",
                &[&connection, &cursor],
            )
            .await?;
        Ok(())
    }

    pub async fn set_connection_status(&self, connection: Uuid, status: &str) -> Result<()> {
        self.tx
            .execute(
                "UPDATE connections SET status = $2 WHERE id = $1",
                &[&connection, &status],
            )
            .await?;
        Ok(())
    }

    /// Records the end of a sync run. A completed full listing also restarts the staleness clock.
    /// Word matches and meaning matches together. Each method ranks its own candidates; the two
    /// lists are merged by rank (never by score, which means different things in each), so an
    /// exact name still comes first and a passage that only says the same thing in other words can
    /// still be found. With no question vector (no model, or it failed) this is the word search.
    pub async fn search_hybrid(
        &self,
        principal: &Principal,
        query: &str,
        question: Option<(&str, &[f32])>,
        limit: i64,
    ) -> Result<Vec<Hit>> {
        let pool = (limit * 2).max(40);
        let words = self.search(principal, query, pool).await?;
        let Some((model, vector)) = question else {
            return Ok(words.into_iter().take(limit as usize).collect());
        };
        let meaning = self
            .search_by_meaning(principal, model, vector, min_similarity(), pool)
            .await?;
        Ok(fuse_hits(words, meaning, limit as usize))
    }

    /// Passages that have no vector yet (or one from another model), oldest first.
    pub async fn passages_needing_vectors(
        &self,
        model: &str,
        limit: i64,
    ) -> Result<Vec<(Uuid, String)>> {
        Ok(self
            .tx
            .query(
                "SELECT id, text FROM chunks
                 WHERE embedding IS NULL OR embedding_model IS DISTINCT FROM $1
                 ORDER BY id LIMIT $2",
                &[&model, &limit],
            )
            .await?
            .into_iter()
            .map(|row| (row.get(0), row.get(1)))
            .collect())
    }

    pub async fn store_vector(&self, chunk: Uuid, model: &str, vector: &[f32]) -> Result<()> {
        let vector = vector.to_vec();
        self.tx
            .execute(
                "UPDATE chunks SET embedding = $2, embedding_model = $3 WHERE id = $1",
                &[&chunk, &vector, &model],
            )
            .await?;
        Ok(())
    }

    /// Passages whose meaning is closest to the question's, among files the principal may read.
    /// `min_similarity` drops passages that are merely the least unrelated.
    pub async fn search_by_meaning(
        &self,
        principal: &Principal,
        model: &str,
        question: &[f32],
        min_similarity: f32,
        limit: i64,
    ) -> Result<Vec<Hit>> {
        if principal.tenant != self.tenant {
            return Err(Error::TenantMismatch);
        }
        let question = question.to_vec();
        let rows = self
            .tx
            .query(
                "SELECT chunk_id, doc_id, name, text, sim, label, link FROM (
                   SELECT c.id AS chunk_id, d.id AS doc_id, d.name, c.text,
                          embedding_dot(c.embedding, $3::real[]) AS sim,
                          c.location ->> 'label' AS label,
                          COALESCE(d.web_url,
                                   CASE WHEN cn.kind = 'google_drive'
                                        THEN 'https://drive.google.com/open?id=' || d.external_id END) AS link
                   FROM chunks c
                   JOIN documents d ON d.tenant_id = c.tenant_id AND d.id = c.document_id
                   JOIN connections cn ON cn.id = d.connection_id
                   WHERE c.embedding IS NOT NULL AND c.embedding_model = $1
                     AND d.readable_by && $2::text[]
                 ) scored
                 WHERE sim >= $4
                 ORDER BY sim DESC, chunk_id
                 LIMIT $5",
                &[&model, &principal.readers(), &question, &min_similarity, &limit],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| Hit {
                chunk_id: row.get(0),
                document_id: row.get(1),
                document_name: row.get(2),
                text: row.get(3),
                score: row.get(4),
                location_label: row.get(5),
                web_url: row.get(6),
            })
            .collect())
    }

    /// Adds one answer to the ledger.
    pub async fn record_usage(
        &self,
        user: &str,
        model: &str,
        input_tokens: i64,
        output_tokens: i64,
        cost_micros: i64,
    ) -> Result<()> {
        self.tx
            .execute(
                "INSERT INTO usage_events (tenant_id, user_id, model, input_tokens, output_tokens, cost_micros)
                 VALUES (app_tenant(), $1, $2, $3, $4, $5)",
                &[&user, &model, &input_tokens, &output_tokens, &cost_micros],
            )
            .await?;
        Ok(())
    }

    /// What one person has used this calendar month (UTC), and when the month began and ends.
    pub async fn usage_this_month(&self, user: &str) -> Result<UsageSummary> {
        let row = self
            .tx
            .query_one(
                "WITH month AS (
                   SELECT date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC' AS start)
                 SELECT COALESCE(sum(u.cost_micros), 0)::bigint,
                        COALESCE(sum(u.input_tokens), 0)::bigint,
                        COALESCE(sum(u.output_tokens), 0)::bigint,
                        count(u.id)::bigint,
                        EXTRACT(EPOCH FROM (SELECT start FROM month))::float8,
                        EXTRACT(EPOCH FROM (SELECT start + interval '1 month' FROM month))::float8,
                        (SELECT monthly_limit_micros FROM firm_settings)
                 FROM month LEFT JOIN usage_events u
                   ON u.user_id = $1 AND u.at >= month.start",
                &[&user],
            )
            .await?;
        Ok(UsageSummary {
            used_micros: row.get(0),
            input_tokens: row.get(1),
            output_tokens: row.get(2),
            answers: row.get(3),
            period_start_epoch: row.get(4),
            period_end_epoch: row.get(5),
            firm_limit_micros: row.get(6),
        })
    }

    /// Everyone's use this month, largest first.
    pub async fn usage_by_person(&self) -> Result<Vec<(String, i64, i64)>> {
        let rows = self
            .tx
            .query(
                "SELECT user_id, sum(cost_micros)::bigint, count(*)::bigint FROM usage_events
                 WHERE at >= date_trunc('month', now() AT TIME ZONE 'UTC') AT TIME ZONE 'UTC'
                 GROUP BY user_id ORDER BY 2 DESC, 1 LIMIT 200",
                &[],
            )
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| (row.get(0), row.get(1), row.get(2)))
            .collect())
    }

    /// Sets the firm's monthly limit per person; none returns to the server default.
    pub async fn set_monthly_limit(&self, micros: Option<i64>) -> Result<()> {
        self.tx
            .execute(
                "INSERT INTO firm_settings (tenant_id, monthly_limit_micros)
                 VALUES (app_tenant(), $1)
                 ON CONFLICT (tenant_id) DO UPDATE SET monthly_limit_micros = EXCLUDED.monthly_limit_micros",
                &[&micros],
            )
            .await?;
        Ok(())
    }

    /// Marks a sync as running, with its counters at zero.
    pub async fn begin_sync(&self, connection: Uuid) -> Result<()> {
        self.tx
            .execute(
                "UPDATE connections SET sync_started_at = now(), sync_files_read = 0,
                                        sync_files_failed = 0
                 WHERE id = $1",
                &[&connection],
            )
            .await?;
        Ok(())
    }

    /// How many files this sync has looked at so far, and how many could not be read.
    pub async fn sync_progress(&self, connection: Uuid, read: i32, failed: i32) -> Result<()> {
        self.tx
            .execute(
                "UPDATE connections SET sync_files_read = $2, sync_files_failed = $3 WHERE id = $1",
                &[&connection, &read, &failed],
            )
            .await?;
        Ok(())
    }

    pub async fn finish_sync(
        &self,
        connection: Uuid,
        error: Option<&str>,
        full_listing_done: bool,
    ) -> Result<()> {
        self.tx
            .execute(
                "UPDATE connections SET
                   last_sync_at = CASE WHEN $2::text IS NULL THEN now() ELSE last_sync_at END,
                   last_error = $2, sync_started_at = NULL,
                   permissions_refreshed_at = CASE WHEN $3 THEN now() ELSE permissions_refreshed_at END
                 WHERE id = $1",
                &[&connection, &error, &full_listing_done],
            )
            .await?;
        Ok(())
    }
}

/// A document's full text, as passages in reading order, with the label of each ("Page 3").
#[derive(Debug, Clone)]
pub struct DocumentText {
    pub name: String,
    pub web_url: Option<String>,
    pub total_chars: i64,
    /// The document is longer than the limit that was asked for, so no text is included.
    pub too_long: bool,
    pub passages: Vec<(Option<String>, String)>,
}

/// How close in meaning a passage must be to a question to count. Unrelated text still scores
/// well above zero with this kind of model, so the bar is not low. `SEMANTIC_MIN_SIMILARITY`
/// changes it.
fn min_similarity() -> f32 {
    std::env::var("SEMANTIC_MIN_SIMILARITY")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0.5)
}

/// Merges two ranked lists of passages by reciprocal rank (the same idea the desktop uses to merge
/// local and cloud results), keeps at most a few passages per file, and returns the best `limit`.
pub fn fuse_hits(words: Vec<Hit>, meaning: Vec<Hit>, limit: usize) -> Vec<Hit> {
    const RANK_OFFSET: f32 = 60.0;
    let mut merged: HashMap<Uuid, (Hit, f32)> = HashMap::new();
    for list in [words, meaning] {
        for (rank, hit) in list.into_iter().enumerate() {
            let share = 1.0 / (RANK_OFFSET + rank as f32 + 1.0);
            merged
                .entry(hit.chunk_id)
                .and_modify(|(_, score)| *score += share)
                .or_insert((hit, share));
        }
    }
    let mut ranked: Vec<(Hit, f32)> = merged.into_values().collect();
    ranked.sort_by(|(a, x), (b, y)| y.total_cmp(x).then(a.chunk_id.cmp(&b.chunk_id)));
    let mut per_file: HashMap<Uuid, i64> = HashMap::new();
    ranked
        .into_iter()
        .filter(|(hit, _)| {
            let seen = per_file.entry(hit.document_id).or_insert(0);
            *seen += 1;
            *seen <= MAX_PASSAGES_PER_FILE
        })
        .take(limit)
        .map(|(mut hit, score)| {
            hit.score = score;
            hit
        })
        .collect()
}

/// One person's AI use in the current calendar month.
#[derive(Debug, Clone)]
pub struct UsageSummary {
    pub used_micros: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub answers: i64,
    pub period_start_epoch: f64,
    pub period_end_epoch: f64,
    /// The firm's own limit, if it has set one.
    pub firm_limit_micros: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct ConnectionState {
    pub kind: String,
    pub config: Value,
    pub cursor: Option<String>,
    pub status: String,
    /// Seconds since a full listing last completed, or None if none has.
    pub full_listing_age_secs: Option<f64>,
    /// "workspace" or "private".
    pub visibility: String,
    /// The person who connected it. Always set for a private connection.
    pub owner: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ExistingDocument {
    pub version: String,
    pub status: String,
    pub parser_version: i32,
    /// Reads in a row that failed in a way that might not repeat.
    pub read_attempts: i32,
}

/// One passage of a document, with where in the file it sits when that is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Passage {
    pub text: String,
    pub label: Option<String>,
}

#[derive(Debug, Clone)]
pub struct StoredDocument {
    pub connection_id: Uuid,
    pub external_id: String,
    pub name: String,
    pub path: String,
    pub mime_type: String,
    pub modified_at: i64,
    pub version: String,
    pub readable_by: Vec<String>,
    pub web_url: Option<String>,
    /// Why the file is stored by name only (see `sync::NOTE_*`); none when it was read.
    pub note: Option<String>,
    pub status: String,
    pub parser_version: i32,
    pub read_attempts: i32,
}

#[cfg(test)]
mod tests {
    use super::{Hit, MAX_PASSAGES_PER_FILE, fuse_hits, search_query};
    use uuid::Uuid;

    #[test]
    fn questions_become_any_word_searches_without_filler() {
        assert_eq!(
            search_query("tell me about men\u{2019}s retreat?").as_deref(),
            Some("'me' | 'about' | 'men' | 'retreat' | 'meabout' | 'aboutmen' | 'menretreat'")
        );
        assert_eq!(
            search_query("When is payment due?").as_deref(),
            Some("'when' | 'is' | 'payment' | 'due' | 'whenis' | 'ispayment' | 'paymentdue'")
        );
    }

    fn hit(chunk: u128, document: u128) -> Hit {
        Hit {
            chunk_id: Uuid::from_u128(chunk),
            document_id: Uuid::from_u128(document),
            document_name: format!("doc{document}"),
            text: format!("passage {chunk}"),
            score: 0.0,
            location_label: None,
            web_url: None,
        }
    }

    fn ids(hits: &[Hit]) -> Vec<u128> {
        hits.iter().map(|h| h.chunk_id.as_u128()).collect()
    }

    #[test]
    fn a_passage_found_by_both_methods_outranks_one_found_by_either() {
        let fused = fuse_hits(
            vec![hit(1, 1), hit(2, 2), hit(3, 3)],
            vec![hit(9, 9), hit(2, 2)],
            10,
        );
        assert_eq!(ids(&fused)[0], 2, "found by both: {:?}", ids(&fused));
        // Everything from both lists is kept, once.
        let mut all = ids(&fused);
        all.sort();
        assert_eq!(all, [1, 2, 3, 9]);
    }

    #[test]
    fn a_passage_only_the_meaning_search_finds_is_still_returned() {
        let fused = fuse_hits(Vec::new(), vec![hit(5, 5)], 10);
        assert_eq!(ids(&fused), [5]);
        assert!(fused[0].score > 0.0);
    }

    #[test]
    fn no_file_fills_the_list_and_the_limit_is_respected() {
        let one_file: Vec<Hit> = (1..=10).map(|n| hit(n, 100)).collect();
        let fused = fuse_hits(one_file.clone(), vec![hit(50, 7), hit(51, 8)], 10);
        let from_big_file = fused
            .iter()
            .filter(|h| h.document_id == Uuid::from_u128(100))
            .count();
        assert_eq!(from_big_file, MAX_PASSAGES_PER_FILE as usize);
        assert_eq!(
            fused.len(),
            MAX_PASSAGES_PER_FILE as usize + 2,
            "the other two files are kept"
        );
        assert_eq!(
            fuse_hits(one_file, vec![hit(50, 7), hit(51, 8)], 4).len(),
            4
        );
    }

    #[test]
    fn nothing_searchable_means_no_query() {
        for question in ["", "?", "a", "tell show please", "   ...   "] {
            assert_eq!(search_query(question), None, "{question:?}");
        }
    }

    #[test]
    fn quotes_and_operators_in_a_question_cannot_change_the_query() {
        let query = search_query("x') | !secret & (drop:* <-> table); --").unwrap();
        assert!(
            query
                .chars()
                .all(|c| c.is_alphanumeric() || " '|".contains(c)),
            "{query}"
        );
        assert!(!query.contains('!') && !query.contains('&') && !query.contains(':'));
        // Every term is quoted on its own.
        assert!(query.split(" | ").all(|term| term.starts_with('\'')
            && term.ends_with('\'')
            && term.matches('\'').count() == 2));
    }

    #[test]
    fn long_questions_are_cut_to_sixteen_words() {
        let question = (0..40)
            .map(|n| format!("word{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let query = search_query(&question).unwrap();
        assert!(query.contains("'word15'") && !query.contains("'word16'"));
    }
}
