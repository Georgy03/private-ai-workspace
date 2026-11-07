//! Dropbox and DocuSign sync against stand-ins for their APIs. Run with scripts/test.sh.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use common::Env;
use fakes::more::{self, Docusign, DocusignWorld, Dropbox, DropboxWorld};
use serde_json::json;
use uuid::Uuid;
use workspace_cloud::{
    auth::{Principal, TenantId},
    connectors::{
        ConnectorError, RefreshingToken, StaticToken, TokenSource, docusign::DocusignConnector,
        dropbox::DropboxConnector,
    },
    db::TenantTx,
    extract::testdata,
    extract_process::UnsandboxedExtractor,
    sync::sync_connection,
};

fn person(tenant: TenantId, email: &str) -> Principal {
    Principal { tenant, user: email.into(), groups: vec![], email: Some(email.into()) }
}

/// What a signed-in person with this email would find for a query.
async fn find(env: &Env, tenant: TenantId, email: &str, query: &str) -> Vec<String> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let mut names: Vec<String> = tx.search(&person(tenant, email), query, 20).await.unwrap().into_iter().map(|h| h.document_name).collect();
    names.sort();
    names.dedup();
    names
}

async fn count(env: &Env, connection: Uuid) -> i64 {
    env.admin.query_one("SELECT count(*) FROM documents WHERE connection_id = $1", &[&connection]).await.unwrap().get(0)
}

// ---------------------------------------------------------------- Dropbox

async fn dropbox(env: &Env, world: &Dropbox) -> (TenantId, Uuid, DropboxConnector) {
    let (tenant, connection) = env.firm_with_connection("Firm", "dropbox", json!({})).await;
    let base = more::dropbox(world.clone()).await;
    let connector = DropboxConnector::new(Box::new(StaticToken("tok".into())), &base, &base)
        .unwrap()
        // The account that connected it, by the email Dropbox reported.
        .always_readable_by(vec!["email:owner@firm.com".into()]);
    (tenant, connection, connector)
}

fn dropbox_world() -> Dropbox {
    Arc::new(Mutex::new(DropboxWorld::default()))
}

#[tokio::test]
async fn dropbox_files_are_read_and_only_the_right_people_can_find_them() {
    let env = Env::new().await;
    let world = dropbox_world();
    {
        let mut w = world.lock().unwrap();
        w.page_size = 2;
        // Not shared: Dropbox lists no sharing, so only the connecting account may read it.
        w.add("/Private/Diary.txt", b"The zebra plan is secret.", &[]);
        w.add("/Clients/Retainer.txt", b"The retainer is due in thirty days.", &["alice@firm.com"]);
        w.add("/Clients/Contract.pdf", &testdata::pdf(&["Interest accrues at one percent a month."]), &["alice@firm.com", "bob@firm.com"]);
        w.add("/Clients/Notes.docx", &testdata::docx(&[&["Parking is behind the building."]]), &["Alice@Firm.com"]);
    }
    let (tenant, connection, connector) = dropbox(&env, &world).await;
    let pool = env.pool("t_worker", 4);

    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert_eq!((stats.stored, stats.full_listing), (4, true), "{stats:?}");

    assert_eq!(find(&env, tenant, "alice@firm.com", "retainer").await, ["Retainer.txt"]);
    // Word and PDF files are found by what is inside them.
    assert_eq!(find(&env, tenant, "alice@firm.com", "interest").await, ["Contract.pdf"]);
    assert_eq!(find(&env, tenant, "alice@firm.com", "parking").await, ["Notes.docx"]);
    assert_eq!(find(&env, tenant, "bob@firm.com", "interest").await, ["Contract.pdf"]);
    // Bob was not shared the retainer, and nobody but the connecting account can read the private file.
    assert!(find(&env, tenant, "bob@firm.com", "retainer").await.is_empty());
    assert!(find(&env, tenant, "alice@firm.com", "zebra").await.is_empty());
    assert_eq!(find(&env, tenant, "owner@firm.com", "zebra").await, ["Diary.txt"]);
    // Only shared files cost a sharing lookup.
    let calls = world.lock().unwrap().calls.clone();
    assert_eq!(calls.iter().filter(|c| c.contains("list_file_members")).count(), 3, "{calls:?}");
    // The link opens the file in Dropbox.
    let link: String = env.admin.query_one("SELECT web_url FROM documents WHERE name = 'Retainer.txt'", &[]).await.unwrap().get(0);
    assert_eq!(link, "https://www.dropbox.com/home/Clients/Retainer.txt");
}

#[tokio::test]
async fn dropbox_changes_edits_deletions_and_whole_deleted_folders_are_followed() {
    let env = Env::new().await;
    let world = dropbox_world();
    {
        let mut w = world.lock().unwrap();
        w.add("/Docs/a.txt", b"alpha text", &["alice@firm.com"]);
        w.add("/Docs/b.txt", b"bravo text", &["alice@firm.com"]);
        w.add("/Docs-old/c.txt", b"charlie text", &["alice@firm.com"]);
        w.add("/Other/d.txt", b"delta text", &["alice@firm.com"]);
    }
    let (tenant, connection, connector) = dropbox(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert_eq!(count(&env, connection).await, 4);

    // An edit changes the content; a deleted file disappears.
    {
        let mut w = world.lock().unwrap();
        w.edit("/Other/d.txt", b"delta rewritten with news");
        w.remove("/Docs/b.txt");
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(!stats.full_listing, "{stats:?}");
    assert_eq!(find(&env, tenant, "alice@firm.com", "rewritten").await, ["d.txt"]);
    assert!(find(&env, tenant, "alice@firm.com", "bravo").await.is_empty());
    assert_eq!(count(&env, connection).await, 3);

    // A deleted folder is reported once, and takes what was inside it, but not a folder whose
    // name merely starts the same way.
    world.lock().unwrap().remove_folder("/Docs");
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(find(&env, tenant, "alice@firm.com", "alpha").await.is_empty());
    assert_eq!(find(&env, tenant, "alice@firm.com", "charlie").await, ["c.txt"]);
    assert_eq!(count(&env, connection).await, 2);
}

#[tokio::test]
async fn dropbox_forgetting_its_cursor_relists_everything_and_removes_what_vanished() {
    let env = Env::new().await;
    let world = dropbox_world();
    {
        let mut w = world.lock().unwrap();
        w.add("/a.txt", b"alpha", &["alice@firm.com"]);
        w.add("/b.txt", b"bravo", &["alice@firm.com"]);
    }
    let (tenant, connection, connector) = dropbox(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    {
        let mut w = world.lock().unwrap();
        w.files.retain(|f| f.path != "/b.txt");
        w.reset_next = true;
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(stats.full_listing, "{stats:?}");
    assert_eq!(count(&env, connection).await, 1);
}

#[tokio::test]
async fn a_file_dropbox_refuses_to_show_sharing_for_is_visible_to_nobody_but_the_connecting_account() {
    let env = Env::new().await;
    let world = dropbox_world();
    {
        let mut w = world.lock().unwrap();
        w.add("/Shared/Locked.txt", b"locked content", &["alice@firm.com"]);
        w.files[0].no_permission = true;
    }
    let (tenant, connection, connector) = dropbox(&env, &world).await;
    sync_connection(&env.pool("t_worker", 4), tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(find(&env, tenant, "alice@firm.com", "locked").await.is_empty());
    assert_eq!(find(&env, tenant, "owner@firm.com", "locked").await, ["Locked.txt"]);
}

#[tokio::test]
async fn a_business_account_lists_the_teams_space_and_a_personal_one_does_not() {
    let env = Env::new().await;
    let world = dropbox_world();
    world.lock().unwrap().add("/a.txt", b"alpha", &[]);
    let (tenant, connection, connector) = dropbox(&env, &world).await;
    sync_connection(&env.pool("t_worker", 4), tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(world.lock().unwrap().root_headers.is_empty(), "a personal account needs no team root");

    let world = dropbox_world();
    {
        let mut w = world.lock().unwrap();
        w.team_root = true;
        w.add("/Team Folder/a.txt", b"alpha", &[]);
    }
    let (tenant, connection, connector) = dropbox(&env, &world).await;
    sync_connection(&env.pool("t_worker", 4), tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    let headers = world.lock().unwrap().root_headers.clone();
    assert!(!headers.is_empty() && headers.iter().all(|h| h.contains("team-ns") && h.contains("\"root\"")), "{headers:?}");
}

// ---------------------------------------------------------------- DocuSign

fn docusign_world() -> Docusign {
    Arc::new(Mutex::new(DocusignWorld::default()))
}

const ID1: &str = "11111111-2222-3333-4444-555555555555";
const ID2: &str = "66666666-7777-8888-9999-000000000000";
const T0: i64 = 1_772_400_000; // 2026-03-01

async fn docusign(env: &Env, world: &Docusign) -> (TenantId, Uuid, DocusignConnector) {
    let (tenant, connection) = env.firm_with_connection("Firm", "docusign", json!({ "account_id": "acct-1" })).await;
    let base = more::docusign(world.clone()).await;
    let connector = DocusignConnector::new(Box::new(StaticToken("tok".into())), &base, "acct-1").unwrap();
    (tenant, connection, connector)
}

#[tokio::test]
async fn envelopes_are_found_by_their_signed_text_and_by_who_is_still_to_sign() {
    let env = Env::new().await;
    let world = docusign_world();
    {
        let mut w = world.lock().unwrap();
        w.page_size = 1;
        w.add(
            ID1,
            "Engagement letter - Whitfield",
            "sent",
            "dana@firm.com",
            &[("Dana Whitfield", "dana@whitfield.example", "completed"), ("Sam Whitfield", "sam@whitfield.example", "sent")],
            testdata::pdf(&["The retainer is due within thirty days of signing."]),
            T0,
        );
        w.add(
            ID2,
            "Lease - Elm Street",
            "completed",
            "lee@firm.com",
            &[("Pat Tenant", "pat@tenant.example", "completed")],
            testdata::pdf(&["The deposit is two months of rent."]),
            T0 + 100,
        );
    }
    let (tenant, connection, connector) = docusign(&env, &world).await;
    let stats = sync_connection(&env.pool("t_worker", 4), tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    // A PDF and a status summary for each of two envelopes.
    assert_eq!((stats.stored, stats.full_listing), (4, true), "{stats:?}");

    // The sender and the people it was sent to may read it; nobody else.
    assert_eq!(find(&env, tenant, "dana@firm.com", "retainer").await, ["Engagement letter - Whitfield.pdf"]);
    assert_eq!(find(&env, tenant, "sam@whitfield.example", "retainer").await, ["Engagement letter - Whitfield.pdf"]);
    assert!(find(&env, tenant, "lee@firm.com", "retainer").await.is_empty());
    assert!(find(&env, tenant, "pat@tenant.example", "retainer").await.is_empty());
    // The status summary answers "waiting for a signature".
    let waiting = find(&env, tenant, "dana@firm.com", "waiting signature Sam").await;
    assert!(waiting.contains(&"Engagement letter - Whitfield (signing status)".to_string()), "{waiting:?}");
    // A finished one has nothing outstanding.
    assert!(!find(&env, tenant, "lee@firm.com", "waiting signature").await.iter().any(|n| n.contains("Lease")));
    let link: String = env.admin.query_one("SELECT web_url FROM documents WHERE name = 'Lease - Elm Street.pdf'", &[]).await.unwrap().get(0);
    assert_eq!(link, format!("https://app.docusign.com/documents/details/{ID2}").replace("app.docusign", "app.docusign"));
}

#[tokio::test]
async fn a_signature_updates_the_status_a_later_sync_asks_only_for_recent_changes_and_deletions_remove_both_items() {
    let env = Env::new().await;
    let world = docusign_world();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    world.lock().unwrap().add(
        ID1,
        "Engagement letter",
        "sent",
        "dana@firm.com",
        &[("Sam Whitfield", "sam@whitfield.example", "sent")],
        testdata::pdf(&["Retainer terms apply."]),
        now - 3 * 86_400,
    );
    world.lock().unwrap().add(ID2, "Old lease", "completed", "dana@firm.com", &[("Pat", "pat@x.example", "completed")], testdata::pdf(&["Old lease text."]), now - 200 * 86_400);
    let (tenant, connection, connector) = docusign(&env, &world).await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert_eq!(count(&env, connection).await, 4);
    assert!(!find(&env, tenant, "dana@firm.com", "waiting signature").await.is_empty());

    // The recipient signs: the summary changes, and the PDF (unchanged document) is not re-read.
    world.lock().unwrap().update(ID1, "completed", &[("Sam Whitfield", "sam@whitfield.example", "completed")], now);
    world.lock().unwrap().calls.clear();
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(find(&env, tenant, "dana@firm.com", "waiting signature").await.is_empty(), "nobody is outstanding now");
    let calls = world.lock().unwrap().calls.clone();
    assert_eq!(calls.iter().filter(|c| c.as_str() == "GET envelopes").count(), 1, "{calls:?}");
    // The second listing starts from the day before the first one began, not from the beginning.
    let from_dates = world.lock().unwrap().from_dates.clone();
    assert_eq!(from_dates[0], "2000-01-01T00:00:00Z");
    assert_ne!(from_dates[1], from_dates[0], "{from_dates:?}");

    // A deleted envelope takes its PDF and its summary.
    world.lock().unwrap().update(ID1, "deleted", &[], now + 10);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert_eq!(count(&env, connection).await, 2);
}

#[tokio::test]
async fn docusign_wants_the_client_in_a_basic_header_and_a_replaced_refresh_token_is_kept() {
    let url = more::docusign_token("refresh-1", "refresh-2").await;
    let token = RefreshingToken::new(format!("{url}/token"), "id".into(), "secret".into(), "refresh-1".into()).unwrap();
    // Without the Basic header DocuSign refuses.
    assert!(matches!(token.token().await, Err(ConnectorError::Other(_)) | Err(ConnectorError::Unauthorized)));

    let token = RefreshingToken::new(format!("{url}/token"), "id".into(), "secret".into(), "refresh-1".into()).unwrap().with_basic_auth(true);
    let slot = token.rotated_token();
    assert_eq!(token.token().await.unwrap(), "access-2");
    // The new refresh token is held for the worker to save.
    assert_eq!(slot.lock().unwrap().clone().as_deref(), Some("refresh-2"));
}
