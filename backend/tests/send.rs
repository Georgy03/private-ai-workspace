//! Sending an email an agent prepared, from the person's own Outlook. Through the real sign-in and
//! HTTP routes, against stand-ins for Microsoft sign-in and Graph.

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
    actions::{self, Payload},
    agents::{self, AgentSettings, AgentSpec, Started},
    api::{AppState, router},
    auth::{Principal, TenantId, TokenVerifier},
    db::TenantTx,
    oauth::{OAuthSettings, ProviderConfig},
    secrets::{MemorySecrets, SecretStore, refresh_token_name},
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
    secrets: Arc<MemorySecrets>,
    mail: fakes::more::Mail,
    tenant: TenantId,
    agent: Uuid,
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
    let mail: fakes::more::Mail = Arc::new(Mutex::new(Default::default()));
    let graph = fakes::more::graph_mail(mail.clone()).await;
    let mut outlook = ProviderConfig::outlook_mail("client".into(), "secret".into());
    outlook.token_url = fakes::token_endpoint("mail-refresh", Arc::new(Mutex::new(0))).await;
    outlook.api_base = graph;
    let oauth = OAuthSettings { microsoft: None, google: None, dropbox: None, docusign: None, outlook_mail: Some(outlook), quickbooks: None, zoom: None, redirect_uri: REDIRECT.into(), state_key: STATE_KEY.to_vec() };
    let secrets = Arc::new(MemorySecrets::default());
    let app = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: env.pool("t_api", 4),
        oauth: Arc::new(oauth),
        secrets: secrets.clone(),
        embedder: None,
    });
    let tenant = workspace_cloud::db::provision_tenant(&env.provisioner().await, "Firm A", None).await.unwrap();
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let spec = AgentSpec {
        name: "Dana".into(), role: "Drafts".into(), description: "Drafts.".into(), icon: "search".into(), color: "blue".into(),
        instructions: "Draft.".into(), tools: vec!["prepare_email".into()], model: "standard".into(), effort: "low".into(),
    };
    let agent = agents::create_agent(&tx, "admin", &spec, &AgentSettings::default()).await.unwrap();
    tx.commit().await.unwrap();
    H { env, app, secrets, mail, tenant, agent }
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

async fn connect_outlook(h: &H, user: &str) -> Uuid {
    let me = token(h.tenant, user, &[]);
    let (status, response) = call(&h.app, "POST", "/v1/connections", &me, Some(json!({ "kind": "outlook_mail" }))).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = url::Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    let request = Request::builder().uri(format!("/oauth/callback?code=good-code&state={state}")).body(Body::empty()).unwrap();
    assert_eq!(h.app.clone().oneshot(request).await.unwrap().status(), StatusCode::OK);
    h.env.admin.query_one("SELECT id FROM connections WHERE kind = 'outlook_mail' AND owner = $1", &[&user]).await.unwrap().get(0)
}

fn email(to: &str, subject: &str) -> Payload {
    Payload::Email { to: vec![to.into()], cc: vec![], subject: subject.into(), body: "The budget is 5,000 dollars. Can you come on Saturday?".into() }
}

/// A task of this person's with an email prepared on it, waiting for them.
async fn prepared(h: &H, user: &str, payload: &Payload) -> Uuid {
    let who = Principal { tenant: h.tenant, user: user.into(), groups: vec![], email: Some(format!("{user}@firm.com")) };
    let mut api = h.env.api().await;
    let tx = TenantTx::begin(&mut api, h.tenant).await.unwrap();
    let Started::Queued(task) = agents::start_task(&tx, &who, h.agent, "write an email").await.unwrap() else { panic!() };
    tx.commit().await.unwrap();
    let mut worker = h.env.worker().await;
    let tx = TenantTx::begin(&mut worker, h.tenant).await.unwrap();
    assert!(actions::propose(&tx, task, user, payload).await.unwrap());
    tx.commit().await.unwrap();
    h.env.admin.query_one("SELECT id FROM agent_actions WHERE task_id = $1", &[&task]).await.unwrap().get(0)
}

async fn status_of(h: &H, id: Uuid) -> String {
    h.env.admin.query_one("SELECT status FROM agent_actions WHERE id = $1", &[&id]).await.unwrap().get(0)
}

async fn allow_sending(h: &H) {
    let (status, _) = call(&h.app, "PUT", "/v1/agents/email-sending", &token(h.tenant, "boss", &["firm_admin"]), Some(json!({ "enabled": true }))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn anyone_can_connect_their_own_outlook_for_sending_and_it_is_never_synced_or_shared() {
    let h = harness().await;
    let alice = token(h.tenant, "alice", &[]);
    // For one person only: asking for the workspace is refused.
    let (status, _) = call(&h.app, "POST", "/v1/connections", &alice, Some(json!({ "kind": "outlook_mail", "scope": "workspace" }))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let id = connect_outlook(&h, "alice").await;
    let row = h.env.admin.query_one("SELECT visibility, owner, label FROM connections WHERE id = $1", &[&id]).await.unwrap();
    assert_eq!((row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, Option<String>>(2).as_deref()), ("private".into(), "alice".into(), Some("alice@outlook.example")));
    // A mailbox has no files, so nothing is queued to sync.
    let queued: i64 = h.env.admin.query_one("SELECT count(*) FROM sync_queue WHERE connection_id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!(queued, 0);
    // Nobody else sees it in their list, and it cannot be synced.
    let (_, listed) = call(&h.app, "GET", "/v1/connections", &alice, None).await;
    assert_eq!(listed["connections"][0]["label"], "alice@outlook.example");
    let (_, others) = call(&h.app, "GET", "/v1/connections", &token(h.tenant, "boss", &["firm_admin"]), None).await;
    assert!(others["connections"].as_array().unwrap().is_empty());
    let (status, _) = call(&h.app, "POST", &format!("/v1/connections/{id}/sync"), &alice, None).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    // Connecting again replaces it: still one.
    connect_outlook(&h, "alice").await;
    let count: i64 = h.env.admin.query_one("SELECT count(*) FROM connections WHERE kind = 'outlook_mail'", &[]).await.unwrap().get(0);
    assert_eq!(count, 1);
}

#[tokio::test]
async fn an_email_is_sent_exactly_as_read_exactly_once_and_only_when_everything_allows_it() {
    let h = harness().await;
    let alice = token(h.tenant, "alice", &[]);
    let action = prepared(&h, "alice", &email("client@other.example", "Retreat budget")).await;
    let send = |body: Value| {
        let app = h.app.clone();
        let alice = alice.clone();
        async move { call(&app, "POST", &format!("/v1/actions/{action}/send"), &alice, Some(body)).await }
    };

    // Nothing is sent until the firm allows it, and until she has connected her Outlook.
    let (status, body) = send(json!({ "confirm_outside": true })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("switched off"), "{body}");
    let (status, _) = call(&h.app, "PUT", "/v1/agents/email-sending", &alice, Some(json!({ "enabled": true }))).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "an employee cannot switch it on");
    allow_sending(&h).await;
    let (status, body) = send(json!({ "confirm_outside": true })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("connect your Outlook"), "{body}");
    connect_outlook(&h, "alice").await;

    // Someone outside her organisation: she has to say she has looked.
    let (status, body) = send(json!({})).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("outside your organisation"), "{body}");
    assert!(h.mail.lock().unwrap().sent.is_empty() && status_of(&h, action).await == "proposed");

    // She edits it, and what is sent is what she edited.
    let edited = email("client@other.example", "Retreat budget, revised");
    let (status, body) = send(json!({ "confirm_outside": true, "payload": edited })).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["from"], "alice@outlook.example");
    {
        let w = h.mail.lock().unwrap();
        assert_eq!(w.sent.len(), 1);
        assert_eq!(w.sent[0]["message"]["subject"], "Retreat budget, revised");
        assert_eq!(w.sent[0]["message"]["toRecipients"][0]["emailAddress"]["address"], "client@other.example");
        assert!(w.tokens.iter().all(|t| t == "Bearer access-1"));
    }
    assert_eq!(status_of(&h, action).await, "sent");
    // Pressing it again sends nothing more.
    let (status, _) = send(json!({ "confirm_outside": true })).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(h.mail.lock().unwrap().sent.len(), 1);
    // The audit log says it was sent, to how many, and edited, but not what it said.
    let audit: String = h.env.admin.query_one("SELECT string_agg(detail::text, ' ') FROM audit_events WHERE action = 'agent_action_sent'", &[]).await.unwrap().get(0);
    assert!(audit.contains("\"edited\": true") && audit.contains("\"recipients\": 1") && !audit.contains("5,000") && !audit.contains("client@other"), "{audit}");
}

#[tokio::test]
async fn only_the_person_it_belongs_to_can_send_it_and_a_busy_day_is_capped() {
    let h = harness().await;
    allow_sending(&h).await;
    let action = prepared(&h, "alice", &email("colleague@firm.com", "Hello")).await;
    connect_outlook(&h, "alice").await;
    connect_outlook(&h, "bob").await;
    // Bob has his own Outlook connected, but this is not his email, and not an administrator's.
    for who in [token(h.tenant, "bob", &[]), token(h.tenant, "boss", &["firm_admin"])] {
        let (status, _) = call(&h.app, "POST", &format!("/v1/actions/{action}/send"), &who, Some(json!({ "confirm_outside": true }))).await;
        assert!(status == StatusCode::NOT_FOUND || status == StatusCode::BAD_REQUEST, "{status}");
    }
    assert!(h.mail.lock().unwrap().sent.is_empty());

    // Twenty sent today is the limit.
    for n in 0..20 {
        let id = prepared(&h, "alice", &email("a@firm.com", &format!("Old {n}"))).await;
        h.env.admin.execute("UPDATE agent_actions SET status = 'sent', sent_at = now() WHERE id = $1", &[&id]).await.unwrap();
    }
    let (status, body) = call(&h.app, "POST", &format!("/v1/actions/{action}/send"), &token(h.tenant, "alice", &[]), Some(json!({}))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("limit of 20"), "{body}");
    assert!(h.mail.lock().unwrap().sent.is_empty());
}

#[tokio::test]
async fn when_outlook_refuses_nothing_counts_as_sent_and_a_lapsed_sign_in_asks_for_reconnecting() {
    let h = harness().await;
    allow_sending(&h).await;
    let alice = token(h.tenant, "alice", &[]);
    let action = prepared(&h, "alice", &email("colleague@firm.com", "Hello")).await;
    let id = connect_outlook(&h, "alice").await;
    let path = format!("/v1/actions/{action}/send");
    let send = || call(&h.app, "POST", &path, &alice, Some(json!({})));

    h.mail.lock().unwrap().fail_with = Some(500);
    let (status, _) = send().await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(status_of(&h, action).await, "proposed", "still waiting: nothing was sent");

    h.mail.lock().unwrap().fail_with = Some(401);
    let (status, body) = send().await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("sign in again"), "{body}");
    let connection_status: String = h.env.admin.query_one("SELECT status FROM connections WHERE id = $1", &[&id]).await.unwrap().get(0);
    assert_eq!((connection_status.as_str(), status_of(&h, action).await.as_str()), ("needs_reauth", "proposed"));
    let (_, body) = send().await;
    assert!(body["error"].as_str().unwrap_or("").contains("reconnect your Outlook"), "{body}");
    assert!(h.mail.lock().unwrap().sent.is_empty());

    // Signing in again, it goes.
    connect_outlook(&h, "alice").await;
    h.mail.lock().unwrap().fail_with = None;
    let (status, _) = send().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(h.mail.lock().unwrap().sent.len(), 1);
    let _ = h.secrets.get(h.tenant, &refresh_token_name(h.tenant, id)).await;
}


#[tokio::test]
async fn an_email_written_in_the_chat_is_sent_exactly_as_read_only_when_everything_allows_it() {
    let h = harness().await;
    let alice = token(h.tenant, "alice", &[]);
    let mail = |body: Value| {
        let (app, alice) = (h.app.clone(), alice.clone());
        async move { call(&app, "POST", "/v1/mail/send", &alice, Some(body)).await }
    };
    let message = |to: &str| json!({ "to": [to], "subject": "Zoom meeting on November 10", "body": "Hello,\n\nCould we meet by Zoom on November 10 at 10:00 AM?\n\nBest regards,", "approved": true });

    // Off until the firm allows it, then not connected, then not approved: nothing goes out.
    let (status, body) = mail(message("colleague@firm.com")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("switched off"), "{body}");
    allow_sending(&h).await;
    let (status, body) = mail(message("colleague@firm.com")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("connect your Outlook"), "{body}");
    connect_outlook(&h, "alice").await;
    let mut unapproved = message("colleague@firm.com");
    unapproved["approved"] = json!(false);
    assert_eq!(mail(unapproved).await.0, StatusCode::BAD_REQUEST);
    assert!(h.mail.lock().unwrap().sent.is_empty(), "nothing is sent before it is approved");

    // A colleague at the same organisation: sent exactly as written.
    let (status, body) = mail(message("colleague@firm.com")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["sent"], true);
    {
        let world = h.mail.lock().unwrap();
        assert_eq!(world.sent.len(), 1);
        let sent = &world.sent[0];
        assert_eq!(sent["message"]["subject"], "Zoom meeting on November 10");
        assert_eq!(sent["message"]["body"]["content"], "Hello,\n\nCould we meet by Zoom on November 10 at 10:00 AM?\n\nBest regards,");
        assert_eq!(sent["message"]["toRecipients"], json!([{ "emailAddress": { "address": "colleague@firm.com" } }]));
    }

    // Someone outside the organisation needs her say-so first.
    let (status, body) = mail(message("client@other.example")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(body["error"].as_str().unwrap_or("").contains("outside your organisation"), "{body}");
    assert_eq!(h.mail.lock().unwrap().sent.len(), 1);
    let mut confirmed = message("client@other.example");
    confirmed["confirm_outside"] = json!(true);
    assert_eq!(mail(confirmed).await.0, StatusCode::OK);
    assert_eq!(h.mail.lock().unwrap().sent.len(), 2);

    // Addresses that could add a header or a second recipient, and empty messages, are refused.
    for bad in [json!({ "to": ["a@b.com\nBcc: x@y.com"], "subject": "s", "body": "b", "approved": true }), json!({ "to": [], "subject": "s", "body": "b", "approved": true }), json!({ "to": ["a@b.com"], "subject": "", "body": "b", "approved": true }), json!({ "to": ["a@b.com"], "subject": "s", "body": " ", "approved": true })] {
        assert_eq!(mail(bad).await.0, StatusCode::BAD_REQUEST);
    }
    assert_eq!(h.mail.lock().unwrap().sent.len(), 2);

    // The record says that it was sent and to how many, not what it said.
    let detail: Value = h.env.admin.query_one("SELECT detail FROM audit_events WHERE action = 'chat_email_sent' ORDER BY id LIMIT 1", &[]).await.unwrap().get(0);
    assert_eq!(detail, json!({ "recipients": 1, "outside": 0 }));
    assert!(!detail.to_string().contains("Zoom"));

    // Someone who has not connected their own Outlook cannot send from anyone else's.
    let (status, _) = call(&h.app, "POST", "/v1/mail/send", &token(h.tenant, "bob", &[]), Some(message("colleague@firm.com"))).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(h.mail.lock().unwrap().sent.len(), 2);
}
