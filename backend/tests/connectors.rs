//! Connector sync against stand-ins for Microsoft Graph and the Google Drive API.
//! Run with scripts/test.sh.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use common::Env;
use fakes::{Shared, World};
use serde_json::{Value, json};
use uuid::Uuid;
use workspace_cloud::extract_process::UnsandboxedExtractor;
use workspace_cloud::{
    Error,
    auth::{Principal, TenantId},
    connectors::{
        ConnectorError, RefreshingToken, StaticToken, TokenSource, google::GoogleConnector,
        microsoft::MicrosoftConnector,
    },
    db::TenantTx,
    queue,
    sync::{SyncStats, sync_connection},
};

fn world() -> Shared {
    Arc::new(Mutex::new(World::default()))
}

fn graph_user(email: &str) -> Value {
    json!({ "roles": ["read"], "grantedToV2": { "user": { "email": email } } })
}

fn drive_user(email: &str) -> Value {
    json!({ "type": "user", "role": "reader", "emailAddress": email })
}

fn person(tenant: TenantId, email: &str) -> Principal {
    Principal {
        tenant,
        user: email.into(),
        groups: vec![],
        email: Some(email.into()),
    }
}

/// What a signed-in person with this email would find for a query.
async fn find(env: &Env, tenant: TenantId, email: &str, query: &str) -> Vec<String> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let mut names: Vec<String> = tx
        .search(&person(tenant, email), query, 20)
        .await
        .unwrap()
        .into_iter()
        .map(|hit| hit.document_name)
        .collect();
    names.sort();
    names.dedup();
    names
}

async fn microsoft(env: &Env, world: &Shared) -> (TenantId, Uuid, MicrosoftConnector) {
    let (tenant, connection) = env
        .firm_with_connection("Firm", "microsoft365", json!({ "drive_id": "d1" }))
        .await;
    (tenant, connection, microsoft_for(world).await)
}

async fn microsoft_for(world: &Shared) -> MicrosoftConnector {
    let base = fakes::graph(world.clone()).await;
    MicrosoftConnector::new(
        Box::new(StaticToken("tok".into())),
        &base,
        "d1",
        vec!["firm.com".into()],
    )
    .unwrap()
}

async fn google(env: &Env, world: &Shared) -> (TenantId, Uuid, GoogleConnector) {
    let (tenant, connection) = env
        .firm_with_connection("Firm", "google_drive", json!({}))
        .await;
    let base = fakes::drive(world.clone()).await;
    (
        tenant,
        connection,
        GoogleConnector::new(Box::new(StaticToken("tok".into())), &base).unwrap(),
    )
}

async fn connection_row(env: &Env, connection: Uuid) -> (Option<String>, String, Option<String>) {
    let row = env
        .admin
        .query_one(
            "SELECT cursor, status, last_error FROM connections WHERE id = $1",
            &[&connection],
        )
        .await
        .unwrap();
    (row.get(0), row.get(1), row.get(2))
}

async fn count(env: &Env, sql: &str, id: Uuid) -> i64 {
    env.admin.query_one(sql, &[&id]).await.unwrap().get(0)
}

#[tokio::test]
async fn microsoft_first_sync_stores_text_names_and_who_may_read() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.page_size = 2;
        w.add(
            "f1",
            "Retainer.txt",
            "text/plain",
            "The retainer is due within thirty days.",
            vec![graph_user("alice@firm.com")],
        );
        w.add(
            "f2",
            "Notes.md",
            "text/markdown",
            "Secret strategy memo for the partners.",
            vec![graph_user("bob@firm.com")],
        );
        w.add(
            "f3",
            "Scan_2024.pdf",
            "application/pdf",
            "%PDF binary",
            vec![graph_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);

    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        stats,
        SyncStats {
            stored: 3,
            metadata_only: 1,
            full_listing: true,
            ..Default::default()
        }
    );

    assert_eq!(
        find(&env, tenant, "alice@firm.com", "retainer").await,
        ["Retainer.txt"]
    );
    assert!(
        find(&env, tenant, "alice@firm.com", "strategy")
            .await
            .is_empty(),
        "alice may not read the memo"
    );
    assert_eq!(
        find(&env, tenant, "bob@firm.com", "strategy").await,
        ["Notes.md"]
    );
    // A file with no readable text is still found by its name.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "scan").await,
        ["Scan_2024.pdf"]
    );
    assert!(find(&env, tenant, "bob@firm.com", "scan").await.is_empty());
    // Someone outside the firm sees nothing at all.
    assert!(
        find(&env, tenant, "mallory@evil.example", "retainer")
            .await
            .is_empty()
    );

    let (cursor, status, error) = connection_row(&env, connection).await;
    assert!(cursor.is_some() && status == "active" && error.is_none());
    let tokens = world.lock().unwrap().tokens.clone();
    assert!(!tokens.is_empty() && tokens.iter().all(|t| t == "Bearer tok"));
}

#[tokio::test]
async fn microsoft_later_syncs_pick_up_edits_revocations_and_deletions() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add(
            "f1",
            "Retainer.txt",
            "text/plain",
            "Old terms: net sixty.",
            vec![graph_user("alice@firm.com")],
        );
        w.add(
            "f2",
            "Memo.txt",
            "text/plain",
            "Memo about the settlement.",
            vec![graph_user("alice@firm.com")],
        );
        w.add(
            "f3",
            "Keep.txt",
            "text/plain",
            "Unchanged file.",
            vec![graph_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "sixty").await,
        ["Retainer.txt"]
    );

    // Nothing changed: nothing is stored again.
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        (stats.stored, stats.removed, stats.full_listing),
        (0, 0, false)
    );

    {
        let mut w = world.lock().unwrap();
        w.edit("f1", "New terms: net thirty.");
        w.set_permissions("f2", vec![graph_user("bob@firm.com")]); // alice loses access
        w.remove("f3");
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        (stats.stored, stats.unchanged, stats.removed),
        (1, 1, 1),
        "{stats:?}"
    );

    assert_eq!(
        find(&env, tenant, "alice@firm.com", "thirty").await,
        ["Retainer.txt"]
    );
    assert!(
        find(&env, tenant, "alice@firm.com", "sixty")
            .await
            .is_empty(),
        "old text must be gone"
    );
    assert!(
        find(&env, tenant, "alice@firm.com", "settlement")
            .await
            .is_empty(),
        "revoked access must hide the file"
    );
    assert_eq!(
        find(&env, tenant, "bob@firm.com", "settlement").await,
        ["Memo.txt"]
    );
    assert!(
        find(&env, tenant, "alice@firm.com", "unchanged")
            .await
            .is_empty(),
        "deleted file must be gone"
    );
    // Editing replaced the old passages instead of piling up new ones.
    let chunks = count(
        &env,
        "SELECT count(*) FROM chunks c JOIN documents d ON d.id = c.document_id WHERE d.connection_id = $1 AND d.external_id = 'f1'",
        connection,
    )
    .await;
    assert_eq!(chunks, 1);
}

#[tokio::test]
async fn a_sync_that_fails_part_way_resumes_where_it_stopped() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.page_size = 2;
        for n in 1..=5 {
            w.add(
                &format!("f{n}"),
                &format!("File{n}.txt"),
                "text/plain",
                &format!("content number {n}"),
                vec![graph_user("alice@firm.com")],
            );
        }
        // Every continuation page fails; the first page works.
        w.fail_path = Some(("token".into(), 500, 1000));
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);

    assert!(
        sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
            .await
            .is_err()
    );
    let stored = count(
        &env,
        "SELECT count(*) FROM documents WHERE connection_id = $1",
        connection,
    )
    .await;
    assert_eq!(stored, 2, "the first page is kept");
    let (cursor, _, error) = connection_row(&env, connection).await;
    assert!(cursor.is_some(), "the place in the listing was saved");
    assert_eq!(error.as_deref(), Some("Sync failed. Will retry."));

    world.lock().unwrap().fail_path = None;
    world.lock().unwrap().calls.clear();
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM documents WHERE connection_id = $1",
            connection
        )
        .await,
        5
    );
    assert!(!stats.full_listing, "resumed rather than starting over");
    let restarted = world
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|c| c.starts_with("GET /delta {}"))
        .count();
    assert_eq!(
        restarted, 0,
        "the listing was not restarted from the beginning"
    );
}

#[tokio::test]
async fn an_expired_cursor_relists_everything_and_removes_files_that_vanished() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        for n in 1..=3 {
            w.add(
                &format!("f{n}"),
                &format!("File{n}.txt"),
                "text/plain",
                &format!("alpha {n}"),
                vec![graph_user("alice@firm.com")],
            );
        }
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();

    {
        let mut w = world.lock().unwrap();
        w.vanish("f2"); // gone while we could not be told
        let limit = w.clock + 1;
        w.expire_cursors_before = Some(limit);
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert!(stats.full_listing);
    assert_eq!(stats.removed, 1);
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "alpha").await,
        ["File1.txt", "File3.txt"]
    );
}

#[tokio::test]
async fn withdrawn_consent_marks_the_connection_for_reconnecting() {
    let env = Env::new().await;
    let world = world();
    world
        .lock()
        .unwrap()
        .add("f1", "A.txt", "text/plain", "text", vec![]);
    world.lock().unwrap().fail_with = Some(401);
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);

    let result =
        sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await;
    assert!(matches!(
        result,
        Err(Error::Connector(ConnectorError::Unauthorized))
    ));
    let (_, status, error) = connection_row(&env, connection).await;
    assert_eq!(status, "needs_reauth");
    assert!(error.unwrap().starts_with("Reconnect required"));

    // A connection that needs reconnecting is not synced until it is reconnected.
    world.lock().unwrap().fail_with = None;
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(stats, SyncStats::default());
}

#[tokio::test]
async fn links_to_other_hosts_are_never_followed() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.page_size = 1;
        w.foreign_next_link = true;
        w.add(
            "f1",
            "A.txt",
            "text/plain",
            "one",
            vec![graph_user("alice@firm.com")],
        );
        w.add(
            "f2",
            "B.txt",
            "text/plain",
            "two",
            vec![graph_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    let error = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("foreign link"), "{error}");
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM documents WHERE connection_id = $1",
            connection
        )
        .await,
        0
    );

    // A saved cursor that points somewhere else is discarded, not used.
    world.lock().unwrap().foreign_next_link = false;
    env.admin
        .execute(
            "UPDATE connections SET cursor = 'http://evil.invalid/steal', permissions_refreshed_at = now() WHERE id = $1",
            &[&connection],
        )
        .await
        .unwrap();
    world.lock().unwrap().tokens.clear();
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert!(stats.full_listing && stats.stored == 2);
}

#[tokio::test]
async fn access_tokens_come_from_the_refresh_token_and_are_cached() {
    let calls = Arc::new(Mutex::new(0));
    let url = fakes::token_endpoint("refresh-ok", calls.clone()).await;
    let good = RefreshingToken::new(
        url.clone(),
        "id".into(),
        "secret".into(),
        "refresh-ok".into(),
    )
    .unwrap();
    assert_eq!(good.token().await.unwrap(), "access-1");
    assert_eq!(good.token().await.unwrap(), "access-1");
    assert_eq!(
        *calls.lock().unwrap(),
        1,
        "the second call used the cached token"
    );
    good.invalidate();
    good.token().await.unwrap();
    assert_eq!(*calls.lock().unwrap(), 2);

    let revoked =
        RefreshingToken::new(url, "id".into(), "secret".into(), "revoked".into()).unwrap();
    assert!(matches!(
        revoked.token().await,
        Err(ConnectorError::Unauthorized)
    ));
}

#[tokio::test]
async fn google_lists_everything_once_then_follows_changes() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.page_size = 2;
        w.add(
            "g1",
            "Retainer.txt",
            "text/plain",
            "Retainer is due in thirty days.",
            vec![drive_user("alice@firm.com")],
        );
        w.add(
            "g2",
            "Minutes",
            "application/vnd.google-apps.document",
            "Minutes about the merger.",
            vec![json!({ "type": "domain", "role": "reader", "domain": "firm.com" })],
        );
        w.add(
            "g3",
            "Hidden.txt",
            "text/plain",
            "visible to nobody here",
            vec![],
        );
        w.items.last_mut().unwrap().hide_permissions = true;
        w.add(
            "g4",
            "Contract.pdf",
            "application/pdf",
            "%PDF",
            vec![drive_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = google(&env, &world).await;
    let pool = env.pool("t_worker", 4);

    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        (stats.stored, stats.metadata_only, stats.full_listing),
        (4, 1, true),
        "{stats:?}"
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "retainer").await,
        ["Retainer.txt"]
    );
    // A whole-firm share reaches everyone at that domain, and the Google Doc was exported.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "merger").await,
        ["Minutes"]
    );
    assert_eq!(
        find(&env, tenant, "bob@firm.com", "merger").await,
        ["Minutes"]
    );
    assert!(
        find(&env, tenant, "bob@other.com", "merger")
            .await
            .is_empty()
    );
    // Drive did not say who may read it, so nobody is shown the file.
    assert!(
        find(&env, tenant, "alice@firm.com", "nobody")
            .await
            .is_empty()
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "contract").await,
        ["Contract.pdf"]
    );
    let (cursor, _, _) = connection_row(&env, connection).await;
    assert!(cursor.unwrap().contains("\"phase\":\"changes\""));

    {
        let mut w = world.lock().unwrap();
        w.calls.clear();
        w.edit("g1", "Retainer is due in sixty days.");
        w.remove("g2");
        w.add(
            "g5",
            "New.txt",
            "text/plain",
            "A brand new file.",
            vec![drive_user("alice@firm.com")],
        );
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        (stats.stored, stats.removed, stats.full_listing),
        (2, 1, false),
        "{stats:?}"
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "sixty").await,
        ["Retainer.txt"]
    );
    assert!(
        find(&env, tenant, "alice@firm.com", "merger")
            .await
            .is_empty()
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "brand").await,
        ["New.txt"]
    );
    let listed = world
        .lock()
        .unwrap()
        .calls
        .iter()
        .filter(|c| c.starts_with("GET /files"))
        .count();
    assert_eq!(
        listed, 0,
        "the second sync read the change feed, not the whole drive"
    );
}

#[tokio::test]
async fn google_start_token_that_is_no_longer_valid_relists_everything() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add(
            "g1",
            "A.txt",
            "text/plain",
            "alpha one",
            vec![drive_user("alice@firm.com")],
        );
        w.add(
            "g2",
            "B.txt",
            "text/plain",
            "alpha two",
            vec![drive_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = google(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();

    {
        let mut w = world.lock().unwrap();
        w.vanish("g2");
        let limit = w.clock + 1;
        w.expire_cursors_before = Some(limit);
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert!(stats.full_listing && stats.removed == 1, "{stats:?}");
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "alpha").await,
        ["A.txt"]
    );
}

#[tokio::test]
async fn connections_are_claimed_once_at_a_time_and_each_firm_sees_only_its_own() {
    let env = Env::new().await;
    let (a, conn_a) = env
        .firm_with_connection("Firm A", "google_drive", json!({}))
        .await;
    let (b, conn_b) = env
        .firm_with_connection("Firm B", "microsoft365", json!({}))
        .await;

    let mut api = env.api().await;
    for (tenant, connection) in [(a, conn_a), (b, conn_b)] {
        let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
        queue::enqueue(&tx, connection).await.unwrap();
        tx.commit().await.unwrap();
    }
    // The API sees only its own firm's queue row.
    let tx = TenantTx::begin(&mut api, a).await.unwrap();
    let visible: i64 = tx
        .sql()
        .query_one("SELECT count(*) FROM sync_queue", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(visible, 1);
    drop(tx);

    let worker = env.worker().await;
    let first = queue::claim(&worker, 10, 60).await.unwrap();
    assert_eq!(first.len(), 2, "workers see every firm's work");
    assert!(first.contains(&(a, conn_a)) && first.contains(&(b, conn_b)));
    assert!(
        queue::claim(&worker, 10, 60).await.unwrap().is_empty(),
        "claimed work is not handed out again"
    );

    queue::reschedule(&worker, conn_a, 0).await.unwrap();
    queue::reschedule(&worker, conn_b, 3600).await.unwrap();
    assert_eq!(
        queue::claim(&worker, 10, 60).await.unwrap(),
        vec![(a, conn_a)]
    );
    queue::remove(&worker, conn_a).await.unwrap();
    queue::reschedule(&worker, conn_a, 0).await.unwrap();
    assert!(queue::claim(&worker, 10, 60).await.unwrap().is_empty());
}

#[tokio::test]
async fn syncing_one_firm_never_reads_or_writes_another_firms_connection() {
    let env = Env::new().await;
    let world_a = world();
    let world_b = world();
    world_a.lock().unwrap().add(
        "a1",
        "A-only.txt",
        "text/plain",
        "Firm A secret plan",
        vec![graph_user("alice@firm.com")],
    );
    world_b.lock().unwrap().add(
        "b1",
        "B-only.txt",
        "text/plain",
        "Firm B secret plan",
        vec![graph_user("alice@firm.com")],
    );
    let (a, conn_a, connector_a) = microsoft(&env, &world_a).await;
    let (b, conn_b, connector_b) = microsoft(&env, &world_b).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, a, conn_a, &connector_a, &UnsandboxedExtractor)
        .await
        .unwrap();
    sync_connection(&pool, b, conn_b, &connector_b, &UnsandboxedExtractor)
        .await
        .unwrap();

    // The same person, in both firms, sees only that firm's files.
    assert_eq!(
        find(&env, a, "alice@firm.com", "secret").await,
        ["A-only.txt"]
    );
    assert_eq!(
        find(&env, b, "alice@firm.com", "secret").await,
        ["B-only.txt"]
    );

    // Syncing firm B's connection as firm A is refused and writes nothing.
    let result = sync_connection(&pool, a, conn_b, &connector_b, &UnsandboxedExtractor).await;
    assert!(matches!(result, Err(Error::TenantMismatch)));
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM documents WHERE connection_id = $1",
            conn_b
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM documents WHERE connection_id = $1",
            conn_a
        )
        .await,
        1
    );
    assert_eq!(
        env.admin
            .query_one(
                "SELECT count(*) FROM documents WHERE tenant_id = $1 AND connection_id = $2",
                &[&a.0, &conn_b]
            )
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[tokio::test]
async fn disconnecting_removes_the_connections_files_and_nothing_else() {
    let env = Env::new().await;
    let world = world();
    world.lock().unwrap().add(
        "f1",
        "A.txt",
        "text/plain",
        "gamma text",
        vec![graph_user("alice@firm.com")],
    );
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let other = env
        .firm("Other", "kept.txt", "gamma elsewhere", &["user:x"])
        .await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();

    // The API role may delete a connection; its files and passages go with it.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let removed = tx
        .sql()
        .execute("DELETE FROM connections WHERE id = $1", &[&connection])
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(removed, 1);
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM documents WHERE connection_id = $1",
            connection
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM chunks WHERE tenant_id = $1",
            tenant.0
        )
        .await,
        0
    );
    assert_eq!(
        count(
            &env,
            "SELECT count(*) FROM documents WHERE tenant_id = $1",
            other.tenant.0
        )
        .await,
        1
    );

    // It may not delete another firm's connection.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let foreign = tx
        .sql()
        .execute(
            "DELETE FROM connections WHERE id = $1",
            &[&other.connection],
        )
        .await
        .unwrap();
    assert_eq!(foreign, 0);
}

#[tokio::test]
async fn the_person_who_connected_a_personal_drive_can_find_their_own_files() {
    // Personal OneDrive often reports no sharing entries at all for a file only its owner can
    // open. Without a rule for the connecting account, a solo user could not search their own files.
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add(
            "p1",
            "Taxes 2025.txt",
            "text/plain",
            "Notes for my 2025 taxes.",
            vec![],
        );
        w.add(
            "p2",
            "Shared.txt",
            "text/plain",
            "Notes shared with a friend.",
            vec![graph_user("friend@example.com")],
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let connector = connector.always_readable_by(vec!["user:me@example.com".into()]);
    sync_connection(
        &env.pool("t_worker", 4),
        tenant,
        connection,
        &connector,
        &UnsandboxedExtractor,
    )
    .await
    .unwrap();

    assert_eq!(
        find(&env, tenant, "me@example.com", "taxes").await,
        ["Taxes 2025.txt"]
    );
    assert_eq!(
        find(&env, tenant, "me@example.com", "notes").await,
        ["Shared.txt", "Taxes 2025.txt"]
    );
    // The friend still sees only what was shared with them, and a stranger sees nothing.
    assert_eq!(
        find(&env, tenant, "friend@example.com", "notes").await,
        ["Shared.txt"]
    );
    assert!(
        find(&env, tenant, "stranger@example.com", "notes")
            .await
            .is_empty()
    );
}

#[tokio::test]
async fn files_google_hides_permissions_for_are_visible_to_the_connecting_account_only() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add(
            "g1",
            "Visible.txt",
            "text/plain",
            "keyword here",
            vec![drive_user("other@firm.com")],
        );
        w.add(
            "g2",
            "Hidden perms.txt",
            "text/plain",
            "keyword there",
            vec![],
        );
        w.items.last_mut().unwrap().hide_permissions = true;
    }
    let (tenant, connection, connector) = google(&env, &world).await;
    let connector = connector.always_readable_by(vec!["user:me@firm.com".into()]);
    sync_connection(
        &env.pool("t_worker", 4),
        tenant,
        connection,
        &connector,
        &UnsandboxedExtractor,
    )
    .await
    .unwrap();

    assert_eq!(
        find(&env, tenant, "me@firm.com", "keyword").await,
        ["Hidden perms.txt", "Visible.txt"]
    );
    assert_eq!(
        find(&env, tenant, "other@firm.com", "keyword").await,
        ["Visible.txt"]
    );
}

const DOCX_MIME: &str = "application/vnd.openxmlformats-officedocument.wordprocessingml.document";

#[tokio::test]
async fn word_pdf_and_excel_content_is_found_without_naming_the_file() {
    use workspace_cloud::extract::testdata;
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        let alice = vec![graph_user("alice@firm.com")];
        w.add_bytes(
            "w1",
            "Scan_0042.docx",
            DOCX_MIME,
            testdata::docx(&[&["The retainer is due within thirty days."]]),
            alice.clone(),
        );
        w.add_bytes(
            "p1",
            "Document 7.pdf",
            "application/pdf",
            testdata::pdf(&["Interest accrues at one percent a month."]),
            alice.clone(),
        );
        w.add_bytes(
            "x1",
            "Ledger.xlsx",
            "",
            testdata::xlsx("Fees", &[&["Acme", "5000"]]),
            alice.clone(),
        );
        // Types the reader cannot use are still found by name only.
        w.add_bytes(
            "o1",
            "Old Letter.doc",
            "application/msword",
            b"binary".to_vec(),
            alice.clone(),
        );
        w.add_bytes(
            "s1",
            "Photo Scan.pdf",
            "application/pdf",
            testdata::pdf(&[""]),
            alice,
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let stats = sync_connection(
        &env.pool("t_worker", 4),
        tenant,
        connection,
        &connector,
        &UnsandboxedExtractor,
    )
    .await
    .unwrap();
    assert_eq!(
        (stats.stored, stats.read_by_reader, stats.metadata_only),
        (5, 3, 2),
        "{stats:?}"
    );

    // By what is inside, with none of the file name in the question.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "when is the retainer due").await,
        ["Scan_0042.docx"]
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "interest one percent").await,
        ["Document 7.pdf"]
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "acme 5000").await,
        ["Ledger.xlsx"]
    );
    // The old and scanned files have no readable text, but their names still work.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "old letter").await,
        ["Old Letter.doc"]
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "photo scan").await,
        ["Photo Scan.pdf"]
    );
    // Permissions apply to what is inside a file exactly as they do to its name.
    assert!(
        find(&env, tenant, "bob@firm.com", "retainer")
            .await
            .is_empty()
    );

    // Passages say where in the file they come from.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let hits = tx
        .search(&person(tenant, "alice@firm.com"), "interest", 5)
        .await
        .unwrap();
    assert_eq!(hits[0].location_label.as_deref(), Some("Page 1"));
    let hits = tx
        .search(&person(tenant, "alice@firm.com"), "acme", 5)
        .await
        .unwrap();
    assert_eq!(hits[0].location_label.as_deref(), Some("Sheet: Fees"));
}

#[tokio::test]
async fn files_read_by_an_older_reader_are_read_again_once() {
    use workspace_cloud::extract::{PARSER_VERSION, testdata};
    let env = Env::new().await;
    let world = world();
    world.lock().unwrap().add_bytes(
        "w1",
        "Letter.docx",
        DOCX_MIME,
        testdata::docx(&[&["Settlement terms are confidential."]]),
        vec![graph_user("alice@firm.com")],
    );
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();

    // Put the database back as an earlier version, which knew nothing about Word files, left it.
    env.admin
        .batch_execute(
            "UPDATE documents SET parser_version = 0, status = 'metadata_only';
             DELETE FROM chunks;
             INSERT INTO chunks (tenant_id, document_id, ordinal, text)
               SELECT tenant_id, id, 0, 'Letter docx' FROM documents;
             UPDATE connections SET permissions_refreshed_at = now() - interval '11 minutes';",
        )
        .await
        .unwrap();
    assert!(
        find(&env, tenant, "alice@firm.com", "settlement")
            .await
            .is_empty()
    );

    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert!(
        stats.full_listing,
        "old files must trigger a re-read: {stats:?}"
    );
    assert_eq!(stats.read_by_reader, 1);
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "settlement").await,
        ["Letter.docx"]
    );
    let version: i32 = env
        .admin
        .query_one("SELECT parser_version FROM documents", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(version, PARSER_VERSION);

    // Once is enough: the next sync is an ordinary one.
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert!(!stats.full_listing, "{stats:?}");
}

struct Scripted(std::sync::Mutex<Vec<workspace_cloud::extract_process::Outcome>>);

#[async_trait::async_trait]
impl workspace_cloud::extract_process::Extractor for Scripted {
    async fn extract(
        &self,
        _: Vec<u8>,
        _: &str,
        _: &str,
    ) -> workspace_cloud::extract_process::Outcome {
        self.0.lock().unwrap().remove(0)
    }
}

#[tokio::test]
async fn a_reader_that_times_out_is_tried_again_but_a_damaged_file_is_not() {
    use workspace_cloud::extract::{ExtractError, testdata};
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        for (id, name) in [("a", "Slow.pdf"), ("b", "Damaged.pdf")] {
            w.add_bytes(
                id,
                name,
                "application/pdf",
                testdata::pdf(&["text"]),
                vec![graph_user("alice@firm.com")],
            );
        }
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    let flaky = Scripted(std::sync::Mutex::new(vec![
        Err(ExtractError::TimedOut),
        Err(ExtractError::Unreadable("broken".into())),
    ]));
    let stats = sync_connection(&pool, tenant, connection, &connector, &flaky)
        .await
        .unwrap();
    assert_eq!(
        (stats.stored, stats.read_by_reader, stats.metadata_only),
        (2, 0, 2)
    );

    let rows = env
        .admin
        .query(
            "SELECT name, content_hash = '', parser_version FROM documents ORDER BY name",
            &[],
        )
        .await
        .unwrap();
    // Slow.pdf has no version and reader version 0, so it will be read again. Damaged.pdf will not.
    assert_eq!(
        (
            rows[1].get::<_, String>(0),
            rows[1].get::<_, bool>(1),
            rows[1].get::<_, i32>(2)
        ),
        ("Slow.pdf".to_string(), true, 0)
    );
    assert_eq!(
        (
            rows[0].get::<_, String>(0),
            rows[0].get::<_, bool>(1),
            rows[0].get::<_, i32>(2)
        ),
        (
            "Damaged.pdf".to_string(),
            false,
            workspace_cloud::extract::PARSER_VERSION
        )
    );
    // Both are still found by name meanwhile.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "slow").await,
        ["Slow.pdf"]
    );

    env.admin
        .execute(
            "UPDATE connections SET permissions_refreshed_at = now() - interval '11 minutes'",
            &[],
        )
        .await
        .unwrap();
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert!(
        stats.full_listing && stats.read_by_reader == 1,
        "only the timed-out file is read again: {stats:?}"
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "text").await,
        ["Slow.pdf"]
    );
}

#[tokio::test]
async fn very_large_files_are_not_downloaded_and_google_uploads_are_read_too() {
    use workspace_cloud::extract::testdata;
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add_bytes(
            "h",
            "Huge.pdf",
            "application/pdf",
            testdata::pdf(&["never read"]),
            vec![graph_user("alice@firm.com")],
        );
        w.items.last_mut().unwrap().claimed_size = Some(600 * 1024 * 1024);
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    sync_connection(
        &env.pool("t_worker", 4),
        tenant,
        connection,
        &connector,
        &UnsandboxedExtractor,
    )
    .await
    .unwrap();
    assert!(
        world
            .lock()
            .unwrap()
            .calls
            .iter()
            .all(|c| !c.contains("/content/h")),
        "the file must not be downloaded"
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "huge").await,
        ["Huge.pdf"]
    );
    assert!(
        find(&env, tenant, "alice@firm.com", "never read")
            .await
            .is_empty()
    );

    let drive_world = self::world();
    {
        let mut w = drive_world.lock().unwrap();
        w.add_bytes(
            "g1",
            "Memo.pdf",
            "application/pdf",
            testdata::pdf(&["Quarterly estimates are due in April."]),
            vec![drive_user("alice@firm.com")],
        );
        w.add(
            "g2",
            "Minutes",
            "application/vnd.google-apps.document",
            "Minutes about the merger.",
            vec![drive_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = google(&env, &drive_world).await;
    let stats = sync_connection(
        &env.pool("t_worker", 4),
        tenant,
        connection,
        &connector,
        &UnsandboxedExtractor,
    )
    .await
    .unwrap();
    assert_eq!(stats.read_by_reader, 1, "{stats:?}");
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "estimates due april").await,
        ["Memo.pdf"]
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "merger").await,
        ["Minutes"],
        "Google's own formats are still exported as text"
    );
}

#[tokio::test]
async fn a_real_sync_works_through_the_restricted_reader_program() {
    use workspace_cloud::{extract::testdata, extract_process::SubprocessExtractor};
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        let alice = vec![graph_user("alice@firm.com")];
        w.add_bytes(
            "w1",
            "Letter.docx",
            DOCX_MIME,
            testdata::docx(&[&["Fees are due within thirty days."]]),
            alice.clone(),
        );
        w.add_bytes(
            "p1",
            "Terms.pdf",
            "application/pdf",
            testdata::pdf(&["Late payments accrue interest."]),
            alice.clone(),
        );
        w.add_bytes(
            "bad",
            "Broken.pdf",
            "application/pdf",
            b"%PDF-1.4 not really".to_vec(),
            alice,
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let reader = SubprocessExtractor::new(env!("CARGO_BIN_EXE_workspace-extract"));
    let stats = sync_connection(
        &env.pool("t_worker", 4),
        tenant,
        connection,
        &connector,
        &reader,
    )
    .await
    .unwrap();
    assert_eq!(
        (stats.stored, stats.read_by_reader, stats.metadata_only),
        (3, 2, 1),
        "{stats:?}"
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "fees due thirty days").await,
        ["Letter.docx"]
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "interest late payments").await,
        ["Terms.pdf"]
    );
    // The damaged file did not stop the others, and is still found by name.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "broken").await,
        ["Broken.pdf"]
    );
}

/// The address each search result would open, for the first match of a query.
async fn link_for(env: &Env, tenant: TenantId, email: &str, query: &str) -> Option<String> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let hits = tx.search(&person(tenant, email), query, 5).await.unwrap();
    hits.into_iter().next().expect("a match").web_url
}

#[tokio::test]
async fn results_carry_a_link_that_opens_the_original_file() {
    let env = Env::new().await;
    let graph_world = world();
    graph_world.lock().unwrap().add(
        "f1",
        "Retainer.txt",
        "text/plain",
        "The retainer is due within thirty days.",
        vec![graph_user("alice@firm.com")],
    );
    let (tenant, connection, connector) = microsoft(&env, &graph_world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        link_for(&env, tenant, "alice@firm.com", "retainer")
            .await
            .as_deref(),
        Some("https://firm.sharepoint.com/docs/Retainer.txt")
    );

    let env = Env::new().await;
    let drive_world = world();
    drive_world.lock().unwrap().add(
        "g1",
        "Minutes.txt",
        "text/plain",
        "Minutes about the merger.",
        vec![drive_user("alice@firm.com")],
    );
    let (tenant, connection, connector) = google(&env, &drive_world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();
    assert_eq!(
        link_for(&env, tenant, "alice@firm.com", "merger")
            .await
            .as_deref(),
        Some("https://docs.google.com/file/d/g1/view")
    );

    // Files stored before links were kept still open: a Google Drive link is built from the ID.
    env.admin
        .execute("UPDATE documents SET web_url = NULL", &[])
        .await
        .unwrap();
    assert_eq!(
        link_for(&env, tenant, "alice@firm.com", "merger")
            .await
            .as_deref(),
        Some("https://drive.google.com/open?id=g1")
    );
}

#[tokio::test]
async fn a_sync_records_why_a_file_is_stored_by_name_only_and_how_far_it_got() {
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add(
            "f1",
            "Retainer.txt",
            "text/plain",
            "The retainer is due within thirty days.",
            vec![graph_user("alice@firm.com")],
        );
        w.add(
            "f2",
            "Photo.png",
            "image/png",
            "not text",
            vec![graph_user("alice@firm.com")],
        );
        w.add(
            "f3",
            "Blank.txt",
            "text/plain",
            "",
            vec![graph_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor)
        .await
        .unwrap();

    let rows = env
        .admin
        .query(
            "SELECT name, note FROM documents WHERE connection_id = $1 ORDER BY name",
            &[&connection],
        )
        .await
        .unwrap();
    let notes: Vec<(String, Option<String>)> =
        rows.iter().map(|row| (row.get(0), row.get(1))).collect();
    assert_eq!(
        notes,
        [
            ("Blank.txt".to_string(), Some("no_text".to_string())),
            ("Photo.png".to_string(), Some("no_text".to_string())),
            ("Retainer.txt".to_string(), None),
        ]
    );

    // After the sync: not running, and every file it looked at is counted.
    let state = env
        .admin
        .query_one(
            "SELECT sync_started_at IS NULL, sync_files_read, sync_files_failed
             FROM connections WHERE id = $1",
            &[&connection],
        )
        .await
        .unwrap();
    assert!(
        state.get::<_, bool>(0),
        "a finished sync is not shown as running"
    );
    assert_eq!((state.get::<_, i32>(1), state.get::<_, i32>(2)), (3, 0));
}

struct AlwaysCrashes(std::sync::atomic::AtomicU32);

#[async_trait::async_trait]
impl workspace_cloud::extract_process::Extractor for AlwaysCrashes {
    async fn extract(
        &self,
        _: Vec<u8>,
        _: &str,
        _: &str,
    ) -> workspace_cloud::extract_process::Outcome {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err(workspace_cloud::extract::ExtractError::Crashed(
            "out of memory".into(),
        ))
    }
}

#[tokio::test]
async fn a_file_that_keeps_crashing_the_reader_is_given_up_on_after_three_tries() {
    use workspace_cloud::extract::testdata;
    let env = Env::new().await;
    let world = world();
    world.lock().unwrap().add_bytes(
        "b",
        "Bomb.pdf",
        "application/pdf",
        testdata::pdf(&["text"]),
        vec![graph_user("alice@firm.com")],
    );
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    let reader = AlwaysCrashes(std::sync::atomic::AtomicU32::new(0));
    for _ in 0..5 {
        // Each sync lists everything again, as the periodic full listing does.
        env.admin
            .execute(
                "UPDATE connections SET permissions_refreshed_at = now() - interval '11 minutes'",
                &[],
            )
            .await
            .unwrap();
        sync_connection(&pool, tenant, connection, &connector, &reader)
            .await
            .unwrap();
    }
    assert_eq!(
        reader.0.load(std::sync::atomic::Ordering::SeqCst),
        3,
        "tried three times, then left alone"
    );
    let row = env
        .admin
        .query_one(
            "SELECT note, read_attempts, content_hash <> '' FROM documents",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, Option<String>>(0),
            row.get::<_, i32>(1),
            row.get::<_, bool>(2)
        ),
        (Some("unreadable".to_string()), 0, true)
    );
}

#[tokio::test]
async fn scanned_pdfs_and_pictures_are_found_by_what_they_say_once_ocr_reads_them() {
    use std::sync::Arc;
    use workspace_cloud::ocr::{Ocr, OcrExtractor, TesseractOcr};
    let Some(engine) = TesseractOcr::detect().filter(|engine| engine.reads_pdfs()) else {
        eprintln!("skipped: Tesseract and Poppler are not installed");
        return;
    };
    let env = Env::new().await;
    let world = world();
    {
        let mut w = world.lock().unwrap();
        w.add_bytes(
            "s1",
            "Scanned engagement letter.pdf",
            "application/pdf",
            include_bytes!("fixtures/scan_two_pages.pdf").to_vec(),
            vec![graph_user("alice@firm.com")],
        );
        w.add_bytes(
            "s2",
            "Photo of notice.png",
            "image/png",
            include_bytes!("fixtures/scan_notice.png").to_vec(),
            vec![graph_user("alice@firm.com")],
        );
    }
    let (tenant, connection, connector) = microsoft(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    let reader = OcrExtractor::new(UnsandboxedExtractor, Some(Arc::new(engine) as Arc<dyn Ocr>));
    let stats = sync_connection(&pool, tenant, connection, &connector, &reader)
        .await
        .unwrap();
    assert_eq!((stats.stored, stats.metadata_only), (2, 0), "{stats:?}");

    // Words that appear only inside the scans find them.
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "hourly billing").await,
        ["Scanned engagement letter.pdf"]
    );
    assert_eq!(
        find(&env, tenant, "alice@firm.com", "tuesday conference").await,
        ["Photo of notice.png"]
    );
    // The page a passage came from says it was scanned.
    let labels: Vec<Option<String>> = env
        .admin
        .query(
            "SELECT c.location ->> 'label' FROM chunks c JOIN documents d ON d.id = c.document_id
             WHERE d.name LIKE 'Scanned%' ORDER BY c.ordinal",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(
        labels,
        [
            Some("Page 1 (scanned)".to_string()),
            Some("Page 2 (scanned)".to_string())
        ]
    );
}
