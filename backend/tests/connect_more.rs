//! Connecting Dropbox and DocuSign: what the administrator is sent to approve, what is learned
//! from the provider afterwards, and that the worker then syncs the connection.

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
    extract::testdata,
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
    dropbox: fakes::more::Dropbox,
    docusign: fakes::more::Docusign,
}

fn admin_token(tenant: TenantId) -> String {
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
    encode(
        &Header::new(Algorithm::RS256),
        &json!({
            "sub": "admin-1", "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id",
            "custom:tenant_id": tenant.0.to_string(), "cognito:groups": ["firm_admin"],
            "email": "admin@firm.com", "email_verified": true,
        }),
        &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
    )
    .unwrap()
}

async fn harness() -> Harness {
    let env = Env::new().await;
    let dropbox: fakes::more::Dropbox = Arc::new(Mutex::new(Default::default()));
    let docusign: fakes::more::Docusign = Arc::new(Mutex::new(Default::default()));
    let dropbox_url = fakes::more::dropbox(dropbox.clone()).await;
    let docusign_url = fakes::more::docusign(docusign.clone()).await;
    let mut dbx = ProviderConfig::dropbox("client".into(), "secret".into());
    dbx.token_url = fakes::token_endpoint("dbx-refresh", Arc::new(Mutex::new(0))).await;
    dbx.api_base = dropbox_url;
    let mut ds = ProviderConfig::docusign("client".into(), "secret".into(), true);
    ds.token_url = format!("{}/token", fakes::more::docusign_token("ds-refresh-1", "ds-refresh-2").await);
    ds.userinfo_url = Some(format!("{}/userinfo", fakes::more::docusign_userinfo(docusign_url).await));
    let oauth = OAuthSettings {
        microsoft: None,
        google: None,
        dropbox: Some(dbx),
        docusign: Some(ds),
        outlook_mail: None,
        quickbooks: None,
        zoom: None,
        redirect_uri: REDIRECT.into(),
        state_key: STATE_KEY.to_vec(),
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
    Harness { env, app, secrets, oauth, worker_pool, dropbox, docusign }
}

async fn call(app: &Router, method: &str, path: &str, token: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder().method(method).uri(path).header("authorization", format!("Bearer {token}"));
    let request = match body {
        Some(body) => request.header("content-type", "application/json").body(Body::from(body.to_string())),
        None => request.body(Body::empty()),
    }
    .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn callback(app: &Router, query: &str) -> StatusCode {
    let request = Request::builder().uri(format!("/oauth/callback?{query}")).body(Body::empty()).unwrap();
    app.clone().oneshot(request).await.unwrap().status()
}

/// Starts a connection and returns the link the administrator is sent to, and its signed state.
async fn start(h: &Harness, tenant: TenantId, kind: &str) -> (Url, String) {
    let (status, response) = call(&h.app, "POST", "/v1/connections", &admin_token(tenant), Some(json!({ "kind": kind }))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    (url, state)
}

async fn connection(h: &Harness, kind: &str) -> (Uuid, Value) {
    let row = h.env.admin.query_one("SELECT id, config FROM connections WHERE kind = $1", &[&kind]).await.unwrap();
    (row.get(0), row.get(1))
}

#[tokio::test]
async fn dropbox_asks_for_read_only_access_that_lasts_and_then_syncs() {
    let h = harness().await;
    let tenant = workspace_cloud::db::provision_tenant(&h.env.provisioner().await, "Firm A", None).await.unwrap();
    h.dropbox.lock().unwrap().add("/Clients/Retainer.txt", b"The retainer is due in thirty days.", &["alice@firm.com"]);

    let (url, state) = start(&h, tenant, "dropbox").await;
    let query: std::collections::HashMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(url.host_str(), Some("www.dropbox.com"));
    // Without offline access Dropbox would give no refresh token, and the connection would die in hours.
    assert_eq!(query.get("token_access_type").map(String::as_str), Some("offline"));
    let scopes = query.get("scope").unwrap();
    assert_eq!(scopes, "files.metadata.read files.content.read sharing.read account_info.read");
    assert!(!scopes.contains("write"), "nothing that could change a file");

    assert_eq!(callback(&h.app, &format!("code=good-code&state={state}")).await, StatusCode::OK);
    let (id, config) = connection(&h, "dropbox").await;
    // The account's own email was learned, and the token went to the secret store.
    assert_eq!(config["account_email"], "owner@firm.com");
    assert_eq!(h.secrets.get(tenant, &refresh_token_name(tenant, id)).await.unwrap().as_deref(), Some("dbx-refresh"));

    let handled = worker::run_once(&h.worker_pool, h.secrets.as_ref(), &h.oauth, &UnsandboxedExtractor).await.unwrap();
    assert_eq!(handled, 1);
    let files: i64 = h.env.admin.query_one("SELECT count(*) FROM documents WHERE connection_id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(files, 1);
}

#[tokio::test]
async fn docusign_is_connected_with_the_accounts_own_address_and_a_replaced_refresh_token_is_saved() {
    let h = harness().await;
    let tenant = workspace_cloud::db::provision_tenant(&h.env.provisioner().await, "Firm A", None).await.unwrap();
    h.docusign.lock().unwrap().add(
        "11111111-2222-3333-4444-555555555555",
        "Engagement letter",
        "sent",
        "dana@firm.com",
        &[("Sam", "sam@whitfield.example", "sent")],
        testdata::pdf(&["The retainer is due within thirty days."]),
        1_772_400_000,
    );

    let (url, state) = start(&h, tenant, "docusign").await;
    assert_eq!(url.host_str(), Some("account-d.docusign.com"), "the sandbox unless the server says production");
    let scope = url.query_pairs().find(|(k, _)| k == "scope").unwrap().1.to_string();
    assert_eq!(scope, "signature extended");

    // The code is exchanged with the client in a Basic header (the stand-in refuses anything else).
    assert_eq!(callback(&h.app, &format!("code=good-code&state={state}")).await, StatusCode::OK);
    let (id, config) = connection(&h, "docusign").await;
    // The default account was chosen, and the connecting account's email learned.
    assert_eq!((config["account_id"].as_str(), config["account_email"].as_str()), (Some("acct-1"), Some("Owner@Firm.com")));
    assert!(config["base_uri"].as_str().unwrap().starts_with("http://127.0.0.1"));
    assert_eq!(h.secrets.get(tenant, &refresh_token_name(tenant, id)).await.unwrap().as_deref(), Some("ds-refresh-1"));

    // Syncing uses the refresh token, DocuSign replaces it, and the worker saves the replacement.
    worker::run_once(&h.worker_pool, h.secrets.as_ref(), &h.oauth, &UnsandboxedExtractor).await.unwrap();
    assert_eq!(h.secrets.get(tenant, &refresh_token_name(tenant, id)).await.unwrap().as_deref(), Some("ds-refresh-2"));
    let items: i64 = h.env.admin.query_one("SELECT count(*) FROM documents WHERE connection_id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(items, 2, "the signed PDF and its status summary");
    // The connecting account may read what it listed, even before anyone else is named.
    let readers: Vec<String> = h.env.admin.query_one("SELECT readable_by FROM documents WHERE connection_id = $1 LIMIT 1", &[&id]).await.unwrap().get(0);
    assert!(readers.contains(&"email:owner@firm.com".to_string()) && readers.contains(&"email:sam@whitfield.example".to_string()), "{readers:?}");

    // The saved token still works: another sync, with the replacement.
    h.env.admin.execute("UPDATE sync_queue SET run_after = now() - interval '1 minute'", &[]).await.unwrap();
    let again = worker::run_once(&h.worker_pool, h.secrets.as_ref(), &h.oauth, &UnsandboxedExtractor).await.unwrap();
    assert_eq!(again, 1);
}
