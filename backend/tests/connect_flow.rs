//! Connecting a source end to end: the administrator approves access, the callback stores the
//! connection and its token, the worker syncs it, and search returns the files. Also removal,
//! and everything an outsider or an ordinary user must not be able to do.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::Env;
use deadpool_postgres::Pool;
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;
use url::Url;
use uuid::Uuid;
use workspace_cloud::extract_process::UnsandboxedExtractor;
use workspace_cloud::{
    api::{AppState, router},
    auth::{TenantId, TokenVerifier},
    oauth::{OAuthSettings, ProviderConfig},
    secrets::{MemorySecrets, SecretStore, refresh_token_name},
    worker,
};

const PRIVATE: &[u8] = include_bytes!("fixtures/test_rsa_private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test_rsa_public.pem");
const ISSUER: &str = "https://issuer.example/pool";
const AUDIENCE: &str = "desktop-client";
const REDIRECT: &str = "https://api.example/oauth/callback";
const STATE_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

struct Harness {
    env: Env,
    app: Router,
    secrets: Arc<MemorySecrets>,
    oauth: OAuthSettings,
    worker_pool: Pool,
    world: fakes::Shared,
}

fn token(tenant: TenantId, user: &str, email: &str, groups: &[&str]) -> String {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    encode(
        &Header::new(Algorithm::RS256),
        &json!({
            "sub": user, "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id",
            "custom:tenant_id": tenant.0.to_string(), "cognito:groups": groups,
            "email": email, "email_verified": true,
        }),
        &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
    )
    .unwrap()
}

async fn harness(enabled: bool) -> Harness {
    let env = Env::new().await;
    let world: fakes::Shared = Arc::new(Mutex::new(Default::default()));
    let graph = fakes::graph(world.clone()).await;
    let token_url = fakes::token_endpoint("refresh-ok", Arc::new(Mutex::new(0))).await;
    let mut microsoft = ProviderConfig::microsoft("client".into(), "secret".into());
    microsoft.token_url = token_url;
    microsoft.api_base = graph;
    let oauth = if enabled {
        OAuthSettings {
            microsoft: Some(microsoft),
            google: None,
            dropbox: None,
            docusign: None,
        outlook_mail: None,
        quickbooks: None,
        zoom: None,
            redirect_uri: REDIRECT.into(),
            state_key: STATE_KEY.to_vec(),
        }
    } else {
        OAuthSettings::default()
    };
    let secrets = Arc::new(MemorySecrets::default());
    let app = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: env.pool("t_api", 4),
        oauth: Arc::new(oauth.clone()),
        secrets: secrets.clone(),
        embedder: None,
    });
    let worker_pool = env.pool("t_worker", 4);
    Harness {
        env,
        app,
        secrets,
        oauth,
        worker_pool,
        world,
    }
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn callback(app: &Router, query: &str) -> StatusCode {
    let request = Request::builder()
        .uri(format!("/oauth/callback?{query}"))
        .body(Body::empty())
        .unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

/// Starts a connection as an administrator and returns the signed state the provider would echo.
async fn start(h: &Harness, tenant: TenantId, body: Value) -> String {
    let admin = token(tenant, "admin-1", "admin@firm.com", &["firm_admin"]);
    let (status, response) =
        call(&h.app, "POST", "/v1/connections", Some(&admin), Some(body)).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    url.query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string()
}

async fn firm(h: &Harness, name: &str) -> TenantId {
    workspace_cloud::db::provision_tenant(&h.env.provisioner().await, name, None)
        .await
        .unwrap()
}

async fn scalar(env: &Env, sql: &str, id: Uuid) -> i64 {
    env.admin.query_one(sql, &[&id]).await.unwrap().get(0)
}

#[tokio::test]
async fn connect_then_sync_then_search_then_disconnect() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    h.world.lock().unwrap().add(
        "f1",
        "Retainer.txt",
        "text/plain",
        "The retainer is due within thirty days.",
        vec![
            json!({ "roles": ["read"], "grantedToV2": { "user": { "email": "alice@firm.com" } } }),
        ],
    );

    // 1. An administrator starts the connection and gets a link to the provider.
    let state = start(
        &h,
        tenant,
        json!({ "kind": "microsoft365", "domains": ["Firm.com"] }),
    )
    .await;

    // 2. The provider sends them back with a one-time code.
    assert_eq!(
        callback(&h.app, &format!("code=good-code&state={state}")).await,
        StatusCode::OK
    );

    let admin = token(tenant, "admin-1", "admin@firm.com", &["firm_admin"]);
    let (_, listed) = call(&h.app, "GET", "/v1/connections", Some(&admin), None).await;
    let connection: Uuid = listed["connections"][0]["id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(listed["connections"][0]["kind"], "microsoft365");

    // The saved connection has the drive learned from the provider and the firm's domain, and
    // its token went to the secret store, not the database.
    let row = h
        .env
        .admin
        .query_one(
            "SELECT config, secret_ref FROM connections WHERE id = $1",
            &[&connection],
        )
        .await
        .unwrap();
    let config: Value = row.get(0);
    assert_eq!(
        (config["drive_id"].as_str(), config["domains"][0].as_str()),
        (Some("drive-xyz"), Some("firm.com"))
    );
    let name = refresh_token_name(tenant, connection);
    assert_eq!(row.get::<_, String>(1), name);
    assert_eq!(
        h.secrets.get(tenant, &name).await.unwrap().as_deref(),
        Some("refresh-ok")
    );
    let leaked = h.env.admin
        .query_one("SELECT count(*) FROM connections WHERE config::text LIKE '%refresh-ok%' OR secret_ref LIKE '%refresh-ok%'", &[])
        .await
        .unwrap()
        .get::<_, i64>(0);
    assert_eq!(leaked, 0, "the token must not be in the database");

    // 3. The worker syncs it.
    assert_eq!(
        worker::run_once(
            &h.worker_pool,
            h.secrets.as_ref(),
            &h.oauth,
            &UnsandboxedExtractor
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        worker::run_once(
            &h.worker_pool,
            h.secrets.as_ref(),
            &h.oauth,
            &UnsandboxedExtractor
        )
        .await
        .unwrap(),
        0,
        "not due again yet"
    );
    let (_, listed) = call(&h.app, "GET", "/v1/connections", Some(&admin), None).await;
    assert_eq!(listed["connections"][0]["documents"], 1);
    assert_eq!(listed["connections"][0]["status"], "active");
    assert!(listed["connections"][0]["last_error"].is_null());

    // The administrator who connected the source can find its files too.
    let (_, found) = call(
        &h.app,
        "POST",
        "/v1/search",
        Some(&admin),
        Some(json!({ "query": "retainer" })),
    )
    .await;
    assert_eq!(found["results"][0]["document_name"], "Retainer.txt");
    assert_eq!(config["connected_by"], "admin-1");

    // 4. Search over HTTP returns the file to the person it is shared with, and only them.
    let alice = token(tenant, "alice-sub", "alice@firm.com", &[]);
    let (_, found) = call(
        &h.app,
        "POST",
        "/v1/search",
        Some(&alice),
        Some(json!({ "query": "retainer" })),
    )
    .await;
    assert_eq!(found["results"][0]["document_name"], "Retainer.txt");
    let bob = token(tenant, "bob-sub", "bob@firm.com", &[]);
    let (_, found) = call(
        &h.app,
        "POST",
        "/v1/search",
        Some(&bob),
        Some(json!({ "query": "retainer" })),
    )
    .await;
    assert_eq!(found["results"].as_array().unwrap().len(), 0);

    // 5. Disconnecting removes the files, the passages and the saved token.
    let (status, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/connections/{connection}"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert_eq!(
        scalar(
            &h.env,
            "SELECT count(*) FROM documents WHERE connection_id = $1",
            connection
        )
        .await,
        0
    );
    assert!(h.secrets.get(tenant, &name).await.unwrap().is_none());
    let (_, found) = call(
        &h.app,
        "POST",
        "/v1/search",
        Some(&alice),
        Some(json!({ "query": "retainer" })),
    )
    .await;
    assert_eq!(found["results"].as_array().unwrap().len(), 0);
    let actions: Vec<String> = h
        .env
        .admin
        .query(
            "SELECT action FROM audit_events WHERE tenant_id = $1 ORDER BY id",
            &[&tenant.0],
        )
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(
        actions.contains(&"connection_added".to_string())
            && actions.contains(&"connection_removed".to_string()),
        "{actions:?}"
    );
}

#[tokio::test]
async fn only_administrators_may_connect_remove_or_sync() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    let connection: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections", &[])
        .await
        .unwrap()
        .get(0);

    let member = token(tenant, "member", "m@firm.com", &[]);
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&member),
        Some(json!({ "kind": "microsoft365" })),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/connections/{connection}"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/connections/{connection}/sync"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // The list of files stored by name only shows file names, so it is for administrators.
    let (status, _) = call(
        &h.app,
        "GET",
        &format!("/v1/connections/{connection}/files"),
        Some(&member),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let admin = token(tenant, "admin", "a@firm.com", &["firm_admin"]);
    let (status, files) = call(
        &h.app,
        "GET",
        &format!("/v1/connections/{connection}/files"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(
        (status, files["files"].as_array().map(Vec::len)),
        (StatusCode::OK, Some(0))
    );
    // Members may see that a source is connected.
    let (status, listed) = call(&h.app, "GET", "/v1/connections", Some(&member), None).await;
    assert_eq!(
        (status, listed["connections"].as_array().unwrap().len()),
        (StatusCode::OK, 1)
    );
    assert_eq!(
        scalar(
            &h.env,
            "SELECT count(*) FROM connections WHERE id = $1",
            connection
        )
        .await,
        1
    );

    // Nobody signed out may do any of it.
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        None,
        Some(json!({ "kind": "microsoft365" })),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn an_administrator_never_reaches_another_firms_connections() {
    let h = harness(true).await;
    let (a, b) = (firm(&h, "Firm A").await, firm(&h, "Firm B").await);
    let state = start(&h, a, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    let connection: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections", &[])
        .await
        .unwrap()
        .get(0);

    let admin_b = token(b, "admin-b", "admin@b.com", &["firm_admin"]);
    let (_, listed) = call(&h.app, "GET", "/v1/connections", Some(&admin_b), None).await;
    assert_eq!(listed["connections"].as_array().unwrap().len(), 0);
    let (status, _) = call(
        &h.app,
        "DELETE",
        &format!("/v1/connections/{connection}"),
        Some(&admin_b),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/connections/{connection}/sync"),
        Some(&admin_b),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        scalar(
            &h.env,
            "SELECT count(*) FROM connections WHERE id = $1",
            connection
        )
        .await,
        1
    );
    assert!(
        h.secrets
            .get(a, &refresh_token_name(a, connection))
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn a_forged_or_failed_callback_connects_nothing() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;

    for query in [
        format!("code=good-code&state={state}x"), // tampered
        "code=good-code&state=garbage".to_string(),
        "code=good-code".to_string(), // no state
        format!("state={state}"),     // no code
        format!("code=good-code&state={state}&error=access_denied"),
        format!("code=bad-code&state={state}"), // provider rejects the code
        format!("code=no-refresh&state={state}"), // provider grants no long-lived access
    ] {
        assert_eq!(
            callback(&h.app, &query).await,
            StatusCode::BAD_REQUEST,
            "{query}"
        );
    }
    assert_eq!(
        h.env
            .admin
            .query_one("SELECT count(*) FROM connections", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );

    // A state signed with another key (someone guessing the format) is refused too.
    let forged = workspace_cloud::oauth::sign_state(
        b"another key another key another!!",
        tenant,
        "attacker",
        workspace_cloud::oauth::Kind::Microsoft365,
        json!({}),
    );
    assert_eq!(
        callback(&h.app, &format!("code=good-code&state={forged}")).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.env
            .admin
            .query_one("SELECT count(*) FROM connections", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[tokio::test]
async fn nothing_is_recorded_when_the_token_cannot_be_saved() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;
    let broken = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: h.env.pool("t_api", 2),
        oauth: Arc::new(h.oauth.clone()),
        secrets: Arc::new(workspace_cloud::secrets::UnconfiguredSecrets),
        embedder: None,
    });
    assert_eq!(
        callback(&broken, &format!("code=good-code&state={state}")).await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        h.env
            .admin
            .query_one("SELECT count(*) FROM connections", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
    assert_eq!(
        h.env
            .admin
            .query_one("SELECT count(*) FROM sync_queue", &[])
            .await
            .unwrap()
            .get::<_, i64>(0),
        0
    );
}

#[tokio::test]
async fn requests_with_bad_options_or_no_setup_are_refused() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let admin = token(tenant, "admin", "a@firm.com", &["firm_admin"]);
    for body in [
        json!({ "kind": "dropbox" }),
        json!({ "kind": "google_drive" }), // not configured here
        json!({ "kind": "microsoft365", "domains": ["not a domain"] }),
        json!({ "kind": "microsoft365", "domains": ["a.com", "b.com", "c.com", "d.com", "e.com", "f.com", "g.com", "h.com", "i.com", "j.com", "k.com"] }),
        json!({ "kind": "microsoft365", "drive_id": "../../etc" }),
        json!({ "kind": "microsoft365", "drive_id": "" }),
    ] {
        let (status, _) = call(
            &h.app,
            "POST",
            "/v1/connections",
            Some(&admin),
            Some(body.clone()),
        )
        .await;
        assert!(
            status.is_client_error() || status == StatusCode::SERVICE_UNAVAILABLE,
            "{body}"
        );
    }

    let off = harness(false).await;
    let tenant = firm(&off, "Firm A").await;
    let admin = token(tenant, "admin", "a@firm.com", &["firm_admin"]);
    let (status, _) = call(
        &off.app,
        "POST",
        "/v1/connections",
        Some(&admin),
        Some(json!({ "kind": "microsoft365" })),
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        callback(&off.app, "code=x&state=y").await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[tokio::test]
async fn a_connection_whose_token_is_gone_stops_syncing() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    let connection: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections", &[])
        .await
        .unwrap()
        .get(0);
    h.secrets
        .delete(tenant, &refresh_token_name(tenant, connection))
        .await
        .unwrap();

    assert_eq!(
        worker::run_once(
            &h.worker_pool,
            h.secrets.as_ref(),
            &h.oauth,
            &UnsandboxedExtractor
        )
        .await
        .unwrap(),
        1
    );
    // It was taken off the work list rather than retried forever.
    assert_eq!(
        scalar(
            &h.env,
            "SELECT count(*) FROM sync_queue WHERE connection_id = $1",
            connection
        )
        .await,
        0
    );
}

#[tokio::test]
async fn sync_now_puts_a_connection_back_on_the_list() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    let connection: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        worker::run_once(
            &h.worker_pool,
            h.secrets.as_ref(),
            &h.oauth,
            &UnsandboxedExtractor
        )
        .await
        .unwrap(),
        1
    );
    assert_eq!(
        worker::run_once(
            &h.worker_pool,
            h.secrets.as_ref(),
            &h.oauth,
            &UnsandboxedExtractor
        )
        .await
        .unwrap(),
        0
    );

    let admin = token(tenant, "admin-1", "admin@firm.com", &["firm_admin"]);
    let (status, _) = call(
        &h.app,
        "POST",
        &format!("/v1/connections/{connection}/sync"),
        Some(&admin),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(
        worker::run_once(
            &h.worker_pool,
            h.secrets.as_ref(),
            &h.oauth,
            &UnsandboxedExtractor
        )
        .await
        .unwrap(),
        1
    );
}

#[tokio::test]
async fn a_pdf_in_a_connected_drive_is_searchable_by_content_with_its_page() {
    use workspace_cloud::extract::testdata;
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    h.world.lock().unwrap().add_bytes(
        "p1",
        "Document 7.pdf",
        "application/pdf",
        testdata::pdf(&["Cover page.", "The retainer is due within thirty days."]),
        vec![
            json!({ "roles": ["read"], "grantedToV2": { "user": { "email": "alice@firm.com" } } }),
        ],
    );
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    worker::run_once(
        &h.worker_pool,
        h.secrets.as_ref(),
        &h.oauth,
        &UnsandboxedExtractor,
    )
    .await
    .unwrap();

    let alice = token(tenant, "alice-sub", "alice@firm.com", &[]);
    let (status, found) = call(
        &h.app,
        "POST",
        "/v1/search",
        Some(&alice),
        Some(json!({ "query": "when is the retainer due" })),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let top = &found["results"][0];
    assert_eq!(top["document_name"], "Document 7.pdf");
    assert_eq!(top["location_label"], "Page 2");
    assert!(
        top["text"]
            .as_str()
            .unwrap()
            .contains("retainer is due within thirty days")
    );
}

#[tokio::test]
async fn the_connector_list_says_which_connectors_this_server_offers() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let member = token(tenant, "member", "m@firm.com", &[]);

    // Any signed-in person may see the list; nobody signed out may.
    let (status, _) = call(&h.app, "GET", "/v1/connectors", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, list) = call(&h.app, "GET", "/v1/connectors", Some(&member), None).await;
    assert_eq!(status, StatusCode::OK);
    let by_kind = |kind: &str| {
        list["connectors"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["kind"] == kind)
            .cloned()
            .unwrap()
    };
    // The test server is set up for Microsoft only.
    assert_eq!(by_kind("microsoft365")["available"], true);
    assert_eq!(by_kind("google_drive")["available"], false);
    // Dropbox and DocuSign are offered too, and unavailable here for the same reason.
    assert_eq!(by_kind("dropbox")["available"], false);
    assert_eq!(by_kind("docusign")["available"], false);
    assert!(
        by_kind("google_drive")["name"]
            .as_str()
            .unwrap()
            .contains("Google")
    );

    let off = harness(false).await;
    let tenant = firm(&off, "Firm B").await;
    let (_, list) = call(
        &off.app,
        "GET",
        "/v1/connectors",
        Some(&token(tenant, "m", "m@b.com", &[])),
        None,
    )
    .await;
    assert!(
        list["connectors"]
            .as_array()
            .unwrap()
            .iter()
            .all(|c| c["available"] == false)
    );
}

#[tokio::test]
async fn reconnecting_revives_the_same_connection_instead_of_adding_another() {
    let h = harness(true).await;
    let tenant = firm(&h, "Firm A").await;
    let state = start(&h, tenant, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    let connection: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections", &[])
        .await
        .unwrap()
        .get(0);
    let admin = token(tenant, "admin-1", "admin@firm.com", &["firm_admin"]);
    let reconnect = |id: Uuid| json!({ "kind": "microsoft365", "reconnect": id });

    // A connection that is working does not need reconnecting.
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&admin),
        Some(reconnect(connection)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Access is withdrawn: the sync marks it, and the old token is gone.
    h.env
        .admin
        .execute("UPDATE connections SET status = 'needs_reauth', last_error = 'Reconnect required' WHERE id = $1", &[&connection])
        .await
        .unwrap();
    h.env
        .admin
        .execute("DELETE FROM sync_queue", &[])
        .await
        .unwrap();
    h.secrets
        .delete(tenant, &refresh_token_name(tenant, connection))
        .await
        .unwrap();

    // Not an administrator, the wrong kind, and a connection that is not there are all refused.
    let member = token(tenant, "member", "m@firm.com", &[]);
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&member),
        Some(reconnect(connection)),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    // A Google connection cannot be "reconnected" through the Microsoft sign-in.
    let google: Uuid = h
        .env
        .admin
        .query_one(
            "INSERT INTO connections (tenant_id, kind, secret_ref, status) VALUES ($1, 'google_drive', 's', 'needs_reauth') RETURNING id",
            &[&tenant.0],
        )
        .await
        .unwrap()
        .get(0);
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&admin),
        Some(reconnect(google)),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&admin),
        Some(reconnect(Uuid::new_v4())),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    let (status, response) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&admin),
        Some(reconnect(connection)),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .unwrap()
        .1
        .to_string();
    assert_eq!(
        callback(&h.app, &format!("code=good-code&state={state}")).await,
        StatusCode::OK
    );

    // Same connection, revived: one row, active, error cleared, a new token, back on the work list.
    let rows = h
        .env
        .admin
        .query(
            "SELECT id, status, last_error FROM connections WHERE kind = 'microsoft365'",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, Uuid>(0), connection);
    assert_eq!(rows[0].get::<_, String>(1), "active");
    assert!(rows[0].get::<_, Option<String>>(2).is_none());
    assert_eq!(
        h.secrets
            .get(tenant, &refresh_token_name(tenant, connection))
            .await
            .unwrap()
            .as_deref(),
        Some("refresh-ok")
    );
    assert_eq!(
        scalar(
            &h.env,
            "SELECT count(*) FROM sync_queue WHERE connection_id = $1",
            connection
        )
        .await,
        1
    );
    let actions: Vec<String> = h
        .env
        .admin
        .query("SELECT action FROM audit_events ORDER BY id", &[])
        .await
        .unwrap()
        .iter()
        .map(|r| r.get(0))
        .collect();
    assert!(
        actions.contains(&"connection_reconnected".to_string()),
        "{actions:?}"
    );

    // Using the same sign-in link again does nothing: the connection no longer needs it.
    assert_eq!(
        callback(&h.app, &format!("code=good-code&state={state}")).await,
        StatusCode::BAD_REQUEST
    );
}

#[tokio::test]
async fn an_administrator_cannot_reconnect_another_firms_connection() {
    let h = harness(true).await;
    let (a, b) = (firm(&h, "Firm A").await, firm(&h, "Firm B").await);
    let state = start(&h, a, json!({ "kind": "microsoft365" })).await;
    callback(&h.app, &format!("code=good-code&state={state}")).await;
    let connection: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections", &[])
        .await
        .unwrap()
        .get(0);
    h.env
        .admin
        .execute("UPDATE connections SET status = 'needs_reauth'", &[])
        .await
        .unwrap();

    let admin_b = token(b, "admin-b", "admin@b.com", &["firm_admin"]);
    let body = json!({ "kind": "microsoft365", "reconnect": connection });
    let (status, _) = call(
        &h.app,
        "POST",
        "/v1/connections",
        Some(&admin_b),
        Some(body),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let status: String = h
        .env
        .admin
        .query_one("SELECT status FROM connections", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(status, "needs_reauth");
}
