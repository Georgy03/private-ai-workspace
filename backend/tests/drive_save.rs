//! Saving a file the person approved into their own Google Drive, through the Drive API, using the
//! one Google Drive connection. Through the real sign-in and HTTP routes, against stand-ins for
//! Google sign-in and the Drive upload.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::Env;
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use workspace_cloud::{
    api::{AppState, router},
    auth::{TenantId, TokenVerifier},
    oauth::{OAuthSettings, ProviderConfig},
    secrets::MemorySecrets,
};

const PRIVATE: &[u8] = include_bytes!("fixtures/test_rsa_private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test_rsa_public.pem");
const ISSUER: &str = "https://issuer.example/pool";
const AUDIENCE: &str = "desktop-client";
const REDIRECT: &str = "https://api.example/oauth/callback";
const STATE_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

struct H {
    env: Env,
    app: Router,
    drive: fakes::more::DriveUpload,
    tenant: TenantId,
}

fn token(tenant: TenantId, user: &str, groups: &[&str]) -> String {
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
    encode(
        &Header::new(Algorithm::RS256),
        &json!({
            "sub": user, "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id",
            "custom:tenant_id": tenant.0.to_string(), "cognito:groups": groups,
            "email": format!("{user}@firm.com"), "email_verified": true,
        }),
        &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
    )
    .unwrap()
}

async fn harness() -> H {
    let env = Env::new().await;
    let drive: fakes::more::DriveUpload = Arc::new(Mutex::new(Default::default()));
    let mut google = ProviderConfig::google("client".into(), "secret".into());
    google.token_url = fakes::token_endpoint("drive-refresh", Arc::new(Mutex::new(0))).await;
    google.api_base = fakes::more::drive_upload(drive.clone()).await;
    let oauth = OAuthSettings { microsoft: None, google: Some(google), dropbox: None, docusign: None, outlook_mail: None, quickbooks: None, zoom: None, redirect_uri: REDIRECT.into(), state_key: STATE_KEY.to_vec() };
    let app = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: env.pool("t_api", 4),
        oauth: Arc::new(oauth),
        secrets: Arc::new(MemorySecrets::default()),
        embedder: None,
    });
    let tenant = workspace_cloud::db::provision_tenant(&env.provisioner().await, "Firm A", None).await.unwrap();
    H { env, app, drive, tenant }
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

/// Connects Google Drive as this person, for themselves or for the whole workspace (administrators).
async fn connect(h: &H, user: &str, groups: &[&str], scope: &str) -> Uuid {
    let me = token(h.tenant, user, groups);
    let (status, response) = call(&h.app, "POST", "/v1/connections", &me, Some(json!({ "kind": "google_drive", "scope": scope }))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = url::Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    // One sign-in asks for reading and for saving, and offline access so it keeps working.
    assert_eq!(query["scope"], "https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/drive.file");
    assert_eq!(query["access_type"], "offline");
    let request = Request::builder().uri(format!("/oauth/callback?code=good-code&state={}", query["state"])).body(Body::empty()).unwrap();
    assert_eq!(h.app.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
    h.env.admin.query_one("SELECT id FROM connections WHERE kind = 'google_drive' AND owner = $1 ORDER BY created_at DESC LIMIT 1", &[&user]).await.unwrap().get(0)
}

fn file(title: &str) -> Value {
    json!({ "title": title, "content": "# Retreat budget\n\nTotal: 5,000 dollars.", "approved": true })
}

#[tokio::test]
async fn a_file_is_saved_exactly_as_approved_into_the_persons_own_drive() {
    let h = harness().await;
    let alice = token(h.tenant, "alice", &[]);
    let save = |body: Value| {
        let (app, alice) = (h.app.clone(), alice.clone());
        async move { call(&app, "POST", "/v1/drive/save", &alice, Some(body)).await }
    };

    // No Google Drive of her own yet, and not approved: nothing is saved.
    let (status, body) = save(file("Retreat budget")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("connect your own Google Drive"), "{body}");
    connect(&h, "alice", &[], "private").await;
    assert_eq!(save(json!({ "title": "x", "content": "y", "approved": false })).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(save(json!({ "title": "x", "content": "y" })).await.0, StatusCode::BAD_REQUEST);
    assert!(h.drive.lock().unwrap().uploads.is_empty(), "nothing is created before it is approved");

    // Approved: exactly the approved name and text are uploaded, as a Google Doc.
    let (status, body) = save(file("Retreat budget")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["saved"], true);
    assert_eq!(body["link"], "https://docs.google.com/document/d/file-1/edit");
    {
        let world = h.drive.lock().unwrap();
        assert_eq!(world.uploads.len(), 1);
        let (kind, upload) = &world.uploads[0];
        assert!(kind.starts_with("multipart/related; boundary="), "{kind}");
        assert!(upload.contains(r#""name":"Retreat budget""#) && upload.contains(r#""mimeType":"application/vnd.google-apps.document""#), "{upload}");
        assert!(upload.contains("\r\n\r\n# Retreat budget\n\nTotal: 5,000 dollars.\r\n--"), "{upload}");
        assert!(world.tokens.iter().all(|t| t == "Bearer access-1"));
    }
    // The audit record says a file was saved and how long it was, not what it was called or said.
    let detail: Value = h.env.admin.query_one("SELECT detail FROM audit_events WHERE action = 'drive_file_saved'", &[]).await.unwrap().get(0);
    assert_eq!(detail, json!({ "characters": 39 }));
    assert!(!detail.to_string().contains("Retreat"));

    // Bad names and empty or oversized text are refused before anything is sent.
    for bad in [json!({ "title": " ", "content": "x", "approved": true }), json!({ "title": "t", "content": "  ", "approved": true }), json!({ "title": "t".repeat(201), "content": "x", "approved": true }), json!({ "title": "t", "content": "x".repeat(200_001), "approved": true })] {
        let (status, body) = save(bad.clone()).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{} {body}", bad["title"].as_str().unwrap_or("").len());
    }
    assert_eq!(h.drive.lock().unwrap().uploads.len(), 1);
}

#[tokio::test]
async fn a_file_is_never_saved_into_someone_elses_drive() {
    let h = harness().await;
    // An administrator connects Google Drive for the whole workspace.
    connect(&h, "boss", &["firm_admin"], "workspace").await;
    // Alice has no Drive of her own, so the workspace connection (the boss's Drive) is not used.
    let (status, body) = call(&h.app, "POST", "/v1/drive/save", &token(h.tenant, "alice", &[]), Some(file("Mine"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("connect your own Google Drive"), "{body}");
    assert!(h.drive.lock().unwrap().uploads.is_empty());
    // The boss's own connection is theirs to save into, shared with the workspace or not.
    let (status, _) = call(&h.app, "POST", "/v1/drive/save", &token(h.tenant, "boss", &["firm_admin"]), Some(file("Boss's file"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.drive.lock().unwrap().uploads.len(), 1);
}

#[tokio::test]
async fn it_copes_with_refusals_outages_and_a_connection_made_before_saving_was_offered() {
    let h = harness().await;
    let alice = token(h.tenant, "alice", &[]);
    let id = connect(&h, "alice", &[], "private").await;
    let save = |title: &'static str| {
        let (app, alice) = (h.app.clone(), alice.clone());
        async move { call(&app, "POST", "/v1/drive/save", &alice, Some(file(title))).await }
    };
    assert_eq!(save("Fine").await.0, StatusCode::OK);

    // Google says no: nothing is claimed saved.
    h.drive.lock().unwrap().fail_with = Some(400);
    assert_eq!(save("Refused").await.0, StatusCode::BAD_REQUEST);
    let saved: i64 = h.env.admin.query_one("SELECT count(*) FROM audit_events WHERE action = 'drive_file_saved'", &[]).await.unwrap().get(0);
    assert_eq!(saved, 1);

    // Unreachable: try again later.
    h.drive.lock().unwrap().fail_with = Some(503);
    assert_eq!(save("Later").await.0, StatusCode::SERVICE_UNAVAILABLE);

    // A connection made before saving was offered has only the reading permission: Drive answers
    // 403. She is told to reconnect, and reading keeps working: the connection is not marked broken.
    h.drive.lock().unwrap().fail_with = Some(403);
    let (status, body) = save("Again").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("reconnect it"), "{body}");
    let state: String = h.env.admin.query_one("SELECT status FROM connections WHERE id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(state, "active");
    h.drive.lock().unwrap().fail_with = None;
    assert_eq!(save("After reconnecting").await.0, StatusCode::OK);
    assert_eq!(h.drive.lock().unwrap().uploads.len(), 2);
}
