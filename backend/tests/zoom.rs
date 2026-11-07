//! Zoom: signing in records whose account it is, and a meeting the person approved is created in their
//! own Zoom and gives back its join link. Through the real sign-in and HTTP routes, against stand-ins
//! for Zoom's sign-in and API. Run with scripts/test.sh.

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
    zoom: fakes::more::Zoom,
    tenant: TenantId,
}

fn token(tenant: TenantId, user: &str) -> String {
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
    encode(
        &Header::new(Algorithm::RS256),
        &json!({
            "sub": user, "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id",
            "custom:tenant_id": tenant.0.to_string(), "cognito:groups": Vec::<String>::new(),
            "email": format!("{user}@firm.com"), "email_verified": true,
        }),
        &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
    )
    .unwrap()
}

async fn harness() -> H {
    let env = Env::new().await;
    let zoom: fakes::more::Zoom = Arc::new(Mutex::new(Default::default()));
    let mut provider = ProviderConfig::zoom("client".into(), "secret".into());
    provider.token_url = fakes::token_endpoint("zoom-refresh", Arc::new(Mutex::new(0))).await;
    provider.api_base = fakes::more::zoom(zoom.clone()).await;
    let oauth = OAuthSettings { microsoft: None, google: None, dropbox: None, docusign: None, outlook_mail: None, quickbooks: None, zoom: Some(provider), redirect_uri: REDIRECT.into(), state_key: STATE_KEY.to_vec() };
    let app = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: env.pool("t_api", 4),
        oauth: Arc::new(oauth),
        secrets: Arc::new(MemorySecrets::default()),
        embedder: None,
    });
    let tenant = workspace_cloud::db::provision_tenant(&env.provisioner().await, "Firm A", None).await.unwrap();
    H { env, app, zoom, tenant }
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

async fn connect(h: &H, user: &str) -> Uuid {
    let (status, response) = call(&h.app, "POST", "/v1/connections", &token(h.tenant, user), Some(json!({ "kind": "zoom" }))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = url::Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let query: std::collections::HashMap<_, _> = url.query_pairs().map(|(k, v)| (k.to_string(), v.to_string())).collect();
    // Zoom takes its permissions from the app's settings, so the link asks for none.
    assert!(!query.contains_key("scope"), "{query:?}");
    assert!(url.as_str().starts_with("https://zoom.us/oauth/authorize"));
    let request = Request::builder().uri(format!("/oauth/callback?code=good-code&state={}", query["state"])).body(Body::empty()).unwrap();
    assert_eq!(h.app.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
    h.env.admin.query_one("SELECT id FROM connections WHERE kind = 'zoom' AND owner = $1", &[&user]).await.unwrap().get(0)
}

fn meeting() -> Value {
    json!({ "topic": "Planning call", "start": "2026-11-10T10:00", "duration_minutes": 45, "timezone": "America/Chicago", "agenda": "Retreat budget", "approved": true })
}

#[tokio::test]
async fn zoom_is_connected_for_one_person_alone_and_never_synced() {
    let h = harness().await;
    let alice = token(h.tenant, "alice");
    let (status, _) = call(&h.app, "POST", "/v1/connections", &alice, Some(json!({ "kind": "zoom", "scope": "workspace" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let id = connect(&h, "alice").await;
    let row = h.env.admin.query_one("SELECT visibility, owner, label FROM connections WHERE id = $1", &[&id]).await.unwrap();
    assert_eq!((row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, Option<String>>(2).as_deref()), ("private".into(), "alice".into(), Some("alice@zoom.example")));
    let queued: i64 = h.env.admin.query_one("SELECT count(*) FROM sync_queue WHERE connection_id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(queued, 0);
    let (status, _) = call(&h.app, "POST", &format!("/v1/connections/{id}/sync"), &alice, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Connecting again replaces it: still one.
    connect(&h, "alice").await;
    let count: i64 = h.env.admin.query_one("SELECT count(*) FROM connections WHERE kind = 'zoom'", &[]).await.unwrap().get(0);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn an_approved_meeting_is_created_in_the_persons_own_zoom_and_only_its_join_link_comes_back() {
    let h = harness().await;
    let alice = token(h.tenant, "alice");
    let create = |body: Value| {
        let (app, alice) = (h.app.clone(), alice.clone());
        async move { call(&app, "POST", "/v1/zoom/meeting", &alice, Some(body)).await }
    };

    // Not connected: said plainly. Not approved: refused. Nothing is created either way.
    let (status, body) = create(meeting()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("connect your Zoom"), "{body}");
    connect(&h, "alice").await;
    let mut unapproved = meeting();
    unapproved["approved"] = json!(false);
    assert_eq!(create(unapproved).await.0, StatusCode::BAD_REQUEST);
    assert!(h.zoom.lock().unwrap().meetings.is_empty());

    // Approved: created exactly as asked, and the host's start link never leaves the server.
    let (status, body) = create(meeting()).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["join_url"], "https://us06web.zoom.us/j/81234567890?pwd=abc123");
    assert_eq!((body["meeting_id"].as_str(), body["passcode"].as_str()), (Some("81234567890"), Some("abc123")));
    assert!(!body.to_string().contains("SECRET-HOST-TOKEN"), "{body}");
    {
        let world = h.zoom.lock().unwrap();
        assert_eq!(world.meetings.len(), 1);
        let sent = &world.meetings[0];
        assert_eq!((sent["topic"].as_str(), sent["start_time"].as_str(), sent["duration"].as_u64(), sent["timezone"].as_str()), (Some("Planning call"), Some("2026-11-10T10:00:00"), Some(45), Some("America/Chicago")));
        assert_eq!(sent["type"], 2);
        assert!(world.tokens.iter().all(|t| t == "Bearer access-1"));
    }
    // The record says a meeting was created and how long, not what it was called.
    let detail: Value = h.env.admin.query_one("SELECT detail FROM audit_events WHERE action = 'zoom_meeting_created'", &[]).await.unwrap().get(0);
    assert_eq!(detail, json!({ "minutes": 45 }));

    // Things that are not a meeting are refused before Zoom is asked.
    let bad = |change: &dyn Fn(&mut Value)| {
        let mut value = meeting();
        change(&mut value);
        value
    };
    for refused in [
        bad(&|v| v["start"] = json!("tomorrow")),
        bad(&|v| v["topic"] = json!("  ")),
        bad(&|v| v["duration_minutes"] = json!(2)),
        bad(&|v| v["duration_minutes"] = json!(5000)),
        bad(&|v| v["timezone"] = json!("America/Chicago; DROP")),
        bad(&|v| v["agenda"] = json!("x".repeat(2_001))),
    ] {
        assert_eq!(create(refused).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(h.zoom.lock().unwrap().meetings.len(), 1);

    // Someone who has not connected their own Zoom cannot create meetings in anyone else's.
    let (status, _) = call(&h.app, "POST", "/v1/zoom/meeting", &token(h.tenant, "bob"), Some(meeting())).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(h.zoom.lock().unwrap().meetings.len(), 1);
}

#[tokio::test]
async fn zoom_trouble_never_counts_as_a_created_meeting_and_a_lapsed_sign_in_asks_for_reconnecting() {
    let h = harness().await;
    let alice = token(h.tenant, "alice");
    let id = connect(&h, "alice").await;
    let create = || {
        let (app, alice) = (h.app.clone(), alice.clone());
        async move { call(&app, "POST", "/v1/zoom/meeting", &alice, Some(meeting())).await }
    };
    h.zoom.lock().unwrap().fail_with = Some(400);
    assert_eq!(create().await.0, StatusCode::BAD_REQUEST);
    h.zoom.lock().unwrap().fail_with = Some(503);
    assert_eq!(create().await.0, StatusCode::SERVICE_UNAVAILABLE);
    let made: i64 = h.env.admin.query_one("SELECT count(*) FROM audit_events WHERE action = 'zoom_meeting_created'", &[]).await.unwrap().get(0);
    assert_eq!(made, 0);

    h.zoom.lock().unwrap().fail_with = Some(401);
    let (status, body) = create().await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("sign in again"), "{body}");
    let state: String = h.env.admin.query_one("SELECT status FROM connections WHERE id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(state, "needs_reauth");
    h.zoom.lock().unwrap().fail_with = None;
    assert_eq!(create().await.0, StatusCode::BAD_REQUEST, "a connection that needs signing in again is not used");
}

async fn make_meeting(h: &H, user: &str) -> Uuid {
    let (status, body) = call(&h.app, "POST", "/v1/zoom/meeting", &token(h.tenant, user), Some(meeting())).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, list) = call(&h.app, "GET", "/v1/zoom/meetings", &token(h.tenant, user), None).await;
    assert_eq!(status, StatusCode::OK);
    list["meetings"].as_array().unwrap().last().unwrap()["id"].as_str().unwrap().parse().unwrap()
}

#[tokio::test]
async fn only_the_meetings_a_person_made_from_the_app_are_listed_moved_or_cancelled_and_only_after_approval() {
    let h = harness().await;
    let (alice, bob) = (token(h.tenant, "alice"), token(h.tenant, "bob"));
    connect(&h, "alice").await;
    connect(&h, "bob").await;
    // Far enough ahead that it is always still to come.
    let mut future = meeting();
    future["start"] = json!("2999-11-10T10:00");
    let (status, _) = call(&h.app, "POST", "/v1/zoom/meeting", &alice, Some(future)).await;
    assert_eq!(status, StatusCode::OK);

    let (_, list) = call(&h.app, "GET", "/v1/zoom/meetings", &alice, None).await;
    let first = &list["meetings"][0];
    assert_eq!((first["topic"].as_str(), first["start"].as_str(), first["duration_minutes"].as_u64(), first["meeting_id"].as_str()), (Some("Planning call"), Some("2999-11-10T10:00:00"), Some(45), Some("81234567890")));
    assert!(!list.to_string().contains("SECRET-HOST-TOKEN"));
    let id = first["id"].as_str().unwrap().to_string();
    // Nobody else sees it or can touch it.
    assert_eq!(call(&h.app, "GET", "/v1/zoom/meetings", &bob, None).await.1["meetings"].as_array().unwrap().len(), 0);
    let move_to = json!({ "start": "2999-11-12T14:30", "timezone": "America/Chicago", "duration_minutes": 60, "approved": true });
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/reschedule"), &bob, Some(move_to.clone())).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/cancel"), &bob, Some(json!({ "approved": true }))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{}/cancel", Uuid::new_v4()), &alice, Some(json!({ "approved": true }))).await.0, StatusCode::NOT_FOUND);

    // Without approval, or with something that is not a time, Zoom is not asked.
    let mut unapproved = move_to.clone();
    unapproved["approved"] = json!(false);
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/reschedule"), &alice, Some(unapproved)).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/reschedule"), &alice, Some(json!({ "start": "soon", "timezone": "America/Chicago", "approved": true }))).await.0, StatusCode::BAD_REQUEST);
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/cancel"), &alice, Some(json!({}))).await.0, StatusCode::BAD_REQUEST);
    {
        let world = h.zoom.lock().unwrap();
        assert!(world.patches.is_empty() && world.deleted.is_empty());
    }

    // Approved: moved exactly as asked, in her own Zoom, with her own sign-in.
    let (status, body) = call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/reschedule"), &alice, Some(move_to)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    {
        let world = h.zoom.lock().unwrap();
        assert_eq!(world.patches.len(), 1);
        let (zoom_id, sent) = &world.patches[0];
        assert_eq!(zoom_id, "81234567890");
        assert_eq!((sent["start_time"].as_str(), sent["timezone"].as_str(), sent["duration"].as_u64()), (Some("2999-11-12T14:30:00"), Some("America/Chicago"), Some(60)));
    }
    let (_, list) = call(&h.app, "GET", "/v1/zoom/meetings", &alice, None).await;
    assert_eq!((list["meetings"][0]["start"].as_str(), list["meetings"][0]["duration_minutes"].as_u64()), (Some("2999-11-12T14:30:00"), Some(60)));
    // A move with no new length keeps the old one.
    let (status, _) = call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/reschedule"), &alice, Some(json!({ "start": "2999-11-13T09:00", "timezone": "America/Chicago", "approved": true }))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.zoom.lock().unwrap().patches[1].1["duration"], 60);

    // Cancelled: removed from Zoom and from the list, and it cannot be cancelled twice.
    let (status, body) = call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/cancel"), &alice, Some(json!({ "approved": true }))).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(h.zoom.lock().unwrap().deleted, ["81234567890"]);
    assert_eq!(call(&h.app, "GET", "/v1/zoom/meetings", &alice, None).await.1["meetings"].as_array().unwrap().len(), 0);
    assert_eq!(call(&h.app, "POST", &format!("/v1/zoom/meetings/{id}/cancel"), &alice, Some(json!({ "approved": true }))).await.0, StatusCode::NOT_FOUND);

    // The record says what happened and never what the meeting was called.
    let details: Vec<(String, Value)> = h.env.admin.query("SELECT action, detail FROM audit_events WHERE action LIKE 'zoom_meeting_%' ORDER BY at", &[]).await.unwrap().iter().map(|r| (r.get(0), r.get(1))).collect();
    assert_eq!(details.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["zoom_meeting_created", "zoom_meeting_moved", "zoom_meeting_moved", "zoom_meeting_cancelled"]);
    assert!(!format!("{details:?}").contains("Planning"));
}

#[tokio::test]
async fn zoom_trouble_leaves_a_meeting_as_it_was_and_a_meeting_already_gone_from_zoom_is_dropped() {
    let h = harness().await;
    let alice = token(h.tenant, "alice");
    let id = connect(&h, "alice").await;
    let meeting_id = make_meeting(&h, "alice").await;
    let move_to = json!({ "start": "2999-11-12T14:30", "timezone": "America/Chicago", "approved": true });
    let (reschedule_path, cancel_path) = (format!("/v1/zoom/meetings/{meeting_id}/reschedule"), format!("/v1/zoom/meetings/{meeting_id}/cancel"));
    let reschedule = || call(&h.app, "POST", &reschedule_path, &alice, Some(move_to.clone()));
    let cancel = || call(&h.app, "POST", &cancel_path, &alice, Some(json!({ "approved": true })));
    let scheduled = || async { h.env.admin.query_one("SELECT status || ' ' || start_local FROM zoom_meetings WHERE id = $1", &[&meeting_id]).await.unwrap().get::<_, String>(0) };
    let before = scheduled().await;

    h.zoom.lock().unwrap().fail_with = Some(503);
    assert_eq!(reschedule().await.0, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(cancel().await.0, StatusCode::SERVICE_UNAVAILABLE);
    h.zoom.lock().unwrap().fail_with = Some(400);
    assert_eq!(reschedule().await.0, StatusCode::BAD_REQUEST);
    assert_eq!(scheduled().await, before);
    let counted: i64 = h.env.admin.query_one("SELECT count(*) FROM audit_events WHERE action IN ('zoom_meeting_moved', 'zoom_meeting_cancelled')", &[]).await.unwrap().get(0);
    assert_eq!(counted, 0);

    // A lapsed sign-in asks for reconnecting.
    h.zoom.lock().unwrap().fail_with = Some(401);
    let (status, body) = cancel().await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("sign in again"), "{body}");
    let state: String = h.env.admin.query_one("SELECT status FROM connections WHERE id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(state, "needs_reauth");
    assert_eq!(scheduled().await, before);

    // Already deleted in Zoom: cancelling agrees with it; moving says so and drops it from the list.
    h.env.admin.execute("UPDATE connections SET status = 'active' WHERE id = $1", &[&id]).await.unwrap();
    {
        let mut world = h.zoom.lock().unwrap();
        world.fail_with = None;
        world.missing = true;
    }
    let (status, body) = reschedule().await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("no longer in your Zoom"), "{body}");
    assert!(scheduled().await.starts_with("cancelled"));
    assert_eq!(call(&h.app, "GET", "/v1/zoom/meetings", &alice, None).await.1["meetings"].as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn zoom_can_be_given_its_own_https_return_address_without_changing_anyone_elses() {
    let mut h = harness().await;
    let mut oauth = OAuthSettings { microsoft: None, google: None, dropbox: None, docusign: None, outlook_mail: None, quickbooks: None, zoom: None, redirect_uri: REDIRECT.into(), state_key: STATE_KEY.to_vec() };
    let mut zoom = ProviderConfig::zoom("client".into(), "secret".into());
    zoom.redirect_uri = Some("https://tunnel.example/oauth/callback".into());
    oauth.zoom = Some(zoom);
    oauth.google = Some(ProviderConfig::google("g".into(), "s".into()));
    h.app = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: h.env.pool("t_api", 4),
        oauth: Arc::new(oauth),
        secrets: Arc::new(MemorySecrets::default()),
        embedder: None,
    });
    let alice = token(h.tenant, "alice");
    let redirect_of = |response: &Value| {
        let url = url::Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
        url.query_pairs().find(|(k, _)| k == "redirect_uri").map(|(_, v)| v.to_string())
    };
    let (status, response) = call(&h.app, "POST", "/v1/connections", &alice, Some(json!({ "kind": "zoom" }))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(redirect_of(&response).as_deref(), Some("https://tunnel.example/oauth/callback"));
    let (status, response) = call(&h.app, "POST", "/v1/connections", &alice, Some(json!({ "kind": "google_drive", "scope": "private" }))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    assert_eq!(redirect_of(&response).as_deref(), Some(REDIRECT));
}
