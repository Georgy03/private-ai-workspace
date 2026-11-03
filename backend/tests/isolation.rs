//! Cross-firm tests. Each one signs in as a firm and tries to reach another firm's data, or to
//! step outside what its database role may do. Run with scripts/test.sh.

mod common;

use common::Env;
use serde_json::json;
use uuid::Uuid;
use workspace_cloud::{
    Error,
    auth::{Principal, TenantId},
    db::{self, NewDocument, TenantTx},
};

fn person(tenant: TenantId, user: &str, groups: &[&str]) -> Principal {
    Principal {
        tenant,
        user: user.into(),
        groups: groups.iter().map(|group| group.to_string()).collect(),
        email: None,
    }
}

fn error_text(error: &Error) -> String {
    match error {
        Error::Db(error) => db_message_ref(error),
        other => other.to_string(),
    }
}

fn db_message_ref(error: &tokio_postgres::Error) -> String {
    match error.as_db_error() {
        Some(db) => db.message().to_string(),
        None => format!("{error:?}"),
    }
}

async fn count(tx: &TenantTx<'_>, sql: &str) -> i64 {
    tx.sql().query_one(sql, &[]).await.unwrap().get(0)
}

fn db_message(error: tokio_postgres::Error) -> String {
    match error.as_db_error() {
        Some(db) => db.message().to_string(),
        None => format!("{error:?}"),
    }
}

#[tokio::test]
async fn search_never_returns_another_firms_files() {
    let env = Env::new().await;
    let a = env
        .firm(
            "Firm A",
            "plan-a.txt",
            "Merger plan for Acme",
            &["user:alice"],
        )
        .await;
    let b = env
        .firm(
            "Firm B",
            "plan-b.txt",
            "Merger secrets of Globex",
            &["user:alice"],
        )
        .await;

    let mut api = env.api().await;
    let alice_in_a = person(a.tenant, "alice", &[]);
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let hits = tx.search(&alice_in_a, "merger", 10).await.unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].document_id, a.document);
    tx.commit().await.unwrap();

    // A principal from firm B cannot be used with a transaction opened for firm A.
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let alice_in_b = person(b.tenant, "alice", &[]);
    assert!(matches!(
        tx.search(&alice_in_b, "merger", 10).await,
        Err(Error::TenantMismatch)
    ));
}

#[tokio::test]
async fn a_session_with_no_firm_sees_nothing() {
    let env = Env::new().await;
    let a = env
        .firm("Firm A", "a.txt", "contract text", &["user:alice"])
        .await;

    let mut worker = env.worker().await;
    let rows = worker
        .query_one("SELECT count(*) FROM documents", &[])
        .await
        .unwrap();
    assert_eq!(rows.get::<_, i64>(0), 0, "no firm set must fail closed");

    // The firm setting ends with the transaction, so a reused connection starts empty again.
    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();
    assert_eq!(count(&tx, "SELECT count(*) FROM documents").await, 1);
    tx.commit().await.unwrap();
    let rows = worker
        .query_one("SELECT count(*) FROM documents", &[])
        .await
        .unwrap();
    assert_eq!(
        rows.get::<_, i64>(0),
        0,
        "firm setting leaked past the transaction"
    );
}

#[tokio::test]
async fn naming_another_firm_in_a_query_returns_nothing() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "alpha", &["user:x"]).await;
    let b = env.firm("Firm B", "b.txt", "beta", &["user:x"]).await;

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let theirs = tx
        .sql()
        .query_one(
            "SELECT count(*) FROM documents WHERE tenant_id = $1",
            &[&b.tenant.0],
        )
        .await
        .unwrap();
    assert_eq!(theirs.get::<_, i64>(0), 0);
    assert_eq!(count(&tx, "SELECT count(*) FROM chunks").await, 1);
    assert_eq!(count(&tx, "SELECT count(*) FROM tenants").await, 1);
}

#[tokio::test]
async fn one_firm_cannot_write_change_or_delete_anothers_rows() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "alpha", &["user:x"]).await;
    let b = env.firm("Firm B", "b.txt", "beta", &["user:x"]).await;

    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();

    let forged = tx
        .sql()
        .execute(
            "INSERT INTO documents (tenant_id, connection_id, external_id, name)
             VALUES ($1, $2, 'x', 'x')",
            &[&b.tenant.0, &b.connection],
        )
        .await;
    assert!(db_message(forged.unwrap_err()).contains("row-level security"));
    drop(tx);

    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();
    let changed = tx
        .sql()
        .execute(
            "UPDATE documents SET name = 'hacked' WHERE id = $1",
            &[&b.document],
        )
        .await
        .unwrap();
    let removed = tx
        .sql()
        .execute("DELETE FROM documents WHERE id = $1", &[&b.document])
        .await
        .unwrap();
    assert_eq!((changed, removed), (0, 0));
    drop(tx);

    let still = env
        .admin
        .query_one("SELECT name FROM documents WHERE id = $1", &[&b.document])
        .await
        .unwrap();
    assert_eq!(still.get::<_, String>(0), "b.txt");
}

#[tokio::test]
async fn rows_cannot_point_at_another_firms_parent() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "alpha", &["user:x"]).await;
    let b = env.firm("Firm B", "b.txt", "beta", &["user:x"]).await;

    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();
    let result = tx
        .add_chunk(b.document, 9, "planted in firm B's document")
        .await;
    assert!(
        error_text(&result.unwrap_err()).contains("foreign key"),
        "a chunk for firm A must not attach to firm B's document"
    );
}

#[tokio::test]
async fn files_are_hidden_unless_the_source_system_allows_the_user() {
    let env = Env::new().await;
    let a = env
        .firm("Firm A", "one.txt", "contract one", &["user:alice"])
        .await;
    let a_tenant = a.tenant;
    {
        let mut worker = env.worker().await;
        let tx = TenantTx::begin(&mut worker, a_tenant).await.unwrap();
        for (name, readers) in [
            ("two.txt", vec!["group:partners".to_string()]),
            ("unknown.txt", vec![]),
            ("bob.txt", vec!["user:bob".to_string()]),
        ] {
            let document = tx
                .upsert_document(&NewDocument {
                    connection_id: a.connection,
                    external_id: name.into(),
                    name: name.into(),
                    path: String::new(),
                    readable_by: readers,
                })
                .await
                .unwrap();
            tx.add_chunk(document, 0, "contract text").await.unwrap();
        }
        tx.commit().await.unwrap();
    }

    let mut api = env.api().await;
    let mut names_for = async |principal: Principal| {
        let tx = TenantTx::begin(&mut api, a_tenant).await.unwrap();
        let mut names = tx
            .search(&principal, "contract", 10)
            .await
            .unwrap()
            .into_iter()
            .map(|hit| hit.document_name)
            .collect::<Vec<_>>();
        names.sort();
        names
    };
    assert_eq!(names_for(person(a_tenant, "alice", &[])).await, ["one.txt"]);
    assert_eq!(
        names_for(person(a_tenant, "alice", &["partners"])).await,
        ["one.txt", "two.txt"]
    );
    assert_eq!(names_for(person(a_tenant, "bob", &[])).await, ["bob.txt"]);
    // A file with no known readers is hidden from everyone, including its own firm's admins.
    assert!(
        !names_for(person(a_tenant, "carol", &["partners"]))
            .await
            .contains(&"unknown.txt".to_string())
    );
    assert!(names_for(person(a_tenant, "nobody", &[])).await.is_empty());
}

#[tokio::test]
async fn the_audit_log_is_append_only() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "alpha", &["user:x"]).await;
    let b = env.firm("Firm B", "b.txt", "beta", &["user:x"]).await;

    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    tx.audit("alice", "search", &json!({ "results": 3 }))
        .await
        .unwrap();
    assert_eq!(count(&tx, "SELECT count(*) FROM audit_events").await, 1);
    let update = tx
        .sql()
        .execute("UPDATE audit_events SET action = 'x'", &[])
        .await;
    assert!(db_message(update.unwrap_err()).contains("permission denied"));
    drop(tx);

    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let delete = tx.sql().execute("DELETE FROM audit_events", &[]).await;
    assert!(db_message(delete.unwrap_err()).contains("permission denied"));
    drop(tx);

    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let forged = tx
        .sql()
        .execute(
            "INSERT INTO audit_events (tenant_id, actor, action) VALUES ($1, 'mallory', 'x')",
            &[&b.tenant.0],
        )
        .await;
    assert!(db_message(forged.unwrap_err()).contains("row-level security"));
}

#[tokio::test]
async fn each_role_can_only_do_its_own_job() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "alpha", &["user:x"]).await;

    // The search API cannot write file data or read connector secret names.
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let write = tx
        .add_chunk(a.document, 5, "api must not write")
        .await
        .unwrap_err();
    assert!(
        error_text(&write).contains("permission denied"),
        "{write:?}"
    );
    drop(tx);
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    let secret = tx
        .sql()
        .query("SELECT secret_ref FROM connections", &[])
        .await;
    assert!(db_message(secret.unwrap_err()).contains("permission denied"));
    drop(tx);
    let tx = TenantTx::begin(&mut api, a.tenant).await.unwrap();
    assert_eq!(
        count(&tx, "SELECT count(*) FROM (SELECT kind FROM connections) c").await,
        1
    );
    drop(tx);

    // The provisioner manages firms but cannot read any firm's files.
    let prov = env.provisioner().await;
    let read = prov.query("SELECT count(*) FROM documents", &[]).await;
    assert!(db_message(read.unwrap_err()).contains("permission denied"));

    // Nobody but the provisioner can create firms.
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();
    let create = tx
        .sql()
        .execute(
            "INSERT INTO tenants (id, name) VALUES (gen_random_uuid(), 'rogue')",
            &[],
        )
        .await;
    assert!(db_message(create.unwrap_err()).contains("permission denied"));
}

#[tokio::test]
async fn offboarding_deletes_one_firm_and_nothing_else() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "alpha", &["user:x"]).await;
    let b = env.firm("Firm B", "b.txt", "beta", &["user:x"]).await;
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();
    tx.audit("system", "sync", &json!({})).await.unwrap();
    tx.commit().await.unwrap();

    db::offboard_tenant(&env.provisioner().await, a.tenant)
        .await
        .unwrap();

    for table in [
        "connections",
        "documents",
        "chunks",
        "audit_events",
        "tenants",
    ] {
        let column = if table == "tenants" {
            "id"
        } else {
            "tenant_id"
        };
        let sql =
            |tenant: Uuid| format!("SELECT count(*) FROM {table} WHERE {column} = '{tenant}'");
        let gone: i64 = env
            .admin
            .query_one(&sql(a.tenant.0), &[])
            .await
            .unwrap()
            .get(0);
        let kept: i64 = env
            .admin
            .query_one(&sql(b.tenant.0), &[])
            .await
            .unwrap()
            .get(0);
        assert_eq!(gone, 0, "{table} still holds firm A data");
        assert!(
            kept > 0 || table == "audit_events",
            "{table} lost firm B data"
        );
    }
}

#[tokio::test]
async fn application_roles_cannot_bypass_the_rules() {
    let env = Env::new().await;
    let roles = env
        .admin
        .query(
            "SELECT rolname, rolsuper, rolbypassrls FROM pg_roles
             WHERE rolname IN ('app_api', 'app_worker', 'app_provisioner', 't_api', 't_worker', 't_prov')",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(roles.len(), 6);
    for role in roles {
        assert!(
            !role.get::<_, bool>(1) && !role.get::<_, bool>(2),
            "{}",
            role.get::<_, String>(0)
        );
    }
    let owners = env
        .admin
        .query(
            "SELECT DISTINCT tableowner FROM pg_tables WHERE schemaname = 'public'",
            &[],
        )
        .await
        .unwrap();
    for owner in owners {
        let owner: String = owner.get(0);
        assert!(
            !owner.starts_with("app_") && !owner.starts_with("t_"),
            "{owner} owns tables"
        );
    }
    let forced = env
        .admin
        .query_one(
            "SELECT bool_and(relrowsecurity AND relforcerowsecurity) FROM pg_class
             WHERE relname IN ('tenants','connections','documents','chunks','audit_events','sync_queue',
                             'usage_events','firm_settings',
                             'document_analysis','entities','entity_evidence','facts','fact_evidence',
                             'relationships','relationship_evidence','company_profile',
                             'agents','agent_versions','agent_tasks','agent_task_events',
                             'agent_task_messages','agent_queue')",
            &[],
        )
        .await
        .unwrap();
    assert!(
        forced.get::<_, bool>(0),
        "every firm table must force row-level security"
    );
}

#[tokio::test]
async fn text_with_a_nul_byte_is_stored_instead_of_failing_the_sync() {
    let env = Env::new().await;
    let a = env
        .firm("Firm A", "one.txt", "contract one", &["user:alice"])
        .await;
    let mut worker = env.worker().await;
    let tx = TenantTx::begin(&mut worker, a.tenant).await.unwrap();
    tx.replace_chunks(
        a.document,
        &[db::Passage {
            text: "men\0s breakout".into(),
            label: None,
        }],
    )
    .await
    .expect("a NUL byte in extracted text must not abort the sync");
    let stored: String = tx
        .sql()
        .query_one(
            "SELECT text FROM chunks WHERE document_id = $1",
            &[&a.document],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(stored, "mens breakout");
}
