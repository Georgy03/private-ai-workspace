//! Connecting a source "only for me" or "for the entire workspace": who may do each, who finds
//! the files, and who may manage the connection.

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
use uuid::Uuid;
use workspace_cloud::extract_process::UnsandboxedExtractor;
use workspace_cloud::{
    api::{AppState, router},
    auth::{Principal, TenantId, TokenVerifier},
    db::TenantTx,
    oauth::{OAuthSettings, ProviderConfig},
    secrets::MemorySecrets,
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

async fn harness() -> Harness {
    let env = Env::new().await;
    let world: fakes::Shared = Arc::new(Mutex::new(Default::default()));
    let graph = fakes::graph(world.clone()).await;
    let mut microsoft = ProviderConfig::microsoft("client".into(), "secret".into());
    microsoft.token_url = fakes::token_endpoint("refresh-ok", Arc::new(Mutex::new(0))).await;
    microsoft.api_base = graph;
    let oauth = OAuthSettings {
        microsoft: Some(microsoft),
        google: None,
        dropbox: None,
        docusign: None,
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
    let tenant = workspace_cloud::db::provision_tenant(&env.provisioner().await, "Firm A", None).await.unwrap();
    let worker_pool = env.pool("t_worker", 4);
    {
        let mut w = world.lock().unwrap();
        // Shared by the provider with Alice and Bob alike.
        w.add(
            "f1",
            "Retainer.txt",
            "text/plain",
            "The retainer is due within thirty days.",
            vec![
                json!({ "roles": ["read"], "grantedToV2": { "user": { "email": "alice@firm.com" } } }),
                json!({ "roles": ["read"], "grantedToV2": { "user": { "email": "bob@firm.com" } } }),
            ],
        );
    }
    Harness { env, app, secrets, oauth, worker_pool, world, tenant }
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

/// Connects Microsoft as this person, with this scope, through the whole sign-in round trip.
async fn connect(h: &Harness, user: &str, groups: &[&str], scope: Option<&str>) -> (StatusCode, Option<Uuid>) {
    let me = token(h.tenant, user, groups);
    let mut body = json!({ "kind": "microsoft365" });
    if let Some(scope) = scope {
        body["scope"] = json!(scope);
    }
    let (status, response) = call(&h.app, "POST", "/v1/connections", &me, Some(body)).await;
    if status != StatusCode::OK {
        return (status, None);
    }
    let url = url::Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    assert_eq!(callback(&h.app, &format!("code=good-code&state={state}")).await, StatusCode::OK);
    let id: Uuid = h
        .env
        .admin
        .query_one("SELECT id FROM connections WHERE owner = $1 ORDER BY created_at DESC LIMIT 1", &[&user])
        .await
        .unwrap()
        .get(0);
    (status, Some(id))
}

async fn sync(h: &Harness) {
    h.env.admin.execute("UPDATE sync_queue SET run_after = now() - interval '1 minute', claimed_until = NULL", &[]).await.unwrap();
    worker::run_once(&h.worker_pool, h.secrets.as_ref(), &h.oauth, &UnsandboxedExtractor).await.unwrap();
}

fn person(tenant: TenantId, user: &str, groups: &[&str]) -> Principal {
    Principal {
        tenant,
        user: user.into(),
        groups: groups.iter().map(|g| g.to_string()).collect(),
        email: Some(format!("{user}@firm.com")),
    }
}

async fn finds(h: &Harness, user: &str, groups: &[&str]) -> Vec<String> {
    let mut api = h.env.api().await;
    let tx = TenantTx::begin(&mut api, h.tenant).await.unwrap();
    let mut names: Vec<String> = tx.search(&person(h.tenant, user, groups), "retainer", 20).await.unwrap().into_iter().map(|hit| hit.document_name).collect();
    names.sort();
    names.dedup();
    names
}

#[tokio::test]
async fn anyone_may_connect_a_source_for_themselves_but_only_an_administrator_for_everyone() {
    let h = harness().await;
    // An ordinary employee: only for themselves. For the workspace, or with nothing said (which has
    // always meant the workspace), they are refused and nothing is started.
    assert_eq!(connect(&h, "alice", &[], Some("workspace")).await.0, StatusCode::FORBIDDEN);
    assert_eq!(connect(&h, "alice", &[], None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(connect(&h, "alice", &[], Some("everyone")).await.0, StatusCode::BAD_REQUEST);
    let (status, mine) = connect(&h, "alice", &[], Some("private")).await;
    assert_eq!(status, StatusCode::OK);
    let row = h.env.admin.query_one("SELECT visibility, owner FROM connections WHERE id = $1", &[&mine.unwrap()]).await.unwrap();
    assert_eq!((row.get::<_, String>(0), row.get::<_, String>(1)), ("private".to_string(), "alice".to_string()));
    // An administrator may do either.
    let (status, workspace) = connect(&h, "boss", &["firm_admin"], Some("workspace")).await;
    assert_eq!(status, StatusCode::OK);
    let visibility: String = h.env.admin.query_one("SELECT visibility FROM connections WHERE id = $1", &[&workspace.unwrap()]).await.unwrap().get(0);
    assert_eq!(visibility, "workspace");
    assert_eq!(connect(&h, "boss", &["firm_admin"], Some("private")).await.0, StatusCode::OK);
}

#[tokio::test]
async fn files_from_a_private_connection_are_found_only_by_the_person_who_connected_it() {
    let h = harness().await;
    connect(&h, "alice", &[], Some("private")).await;
    sync(&h).await;
    // The provider says Bob may open the retainer too, but this connection is Alice's alone.
    assert_eq!(finds(&h, "alice", &[]).await, ["Retainer.txt"]);
    assert!(finds(&h, "bob", &[]).await.is_empty());
    assert!(finds(&h, "boss", &["firm_admin"]).await.is_empty(), "not even an administrator");
    // A whole document is no easier to reach than a search.
    let doc: Uuid = h.env.admin.query_one("SELECT id FROM documents LIMIT 1", &[]).await.unwrap().get(0);
    let (status, _) = call(&h.app, "GET", &format!("/v1/documents/{doc}"), &token(h.tenant, "bob", &[]), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&h.app, "GET", &format!("/v1/documents/{doc}"), &token(h.tenant, "alice", &[]), None).await;
    assert_eq!(status, StatusCode::OK);

    // For the workspace, the provider's own sharing decides: Bob was shared the file, so he finds it.
    let (_, _) = connect(&h, "boss", &["firm_admin"], Some("workspace")).await;
    sync(&h).await;
    assert_eq!(finds(&h, "bob", &[]).await, ["Retainer.txt"]);
}

#[tokio::test]
async fn nobody_else_sees_or_manages_a_private_connection_and_workspace_ones_stay_with_administrators() {
    let h = harness().await;
    let (_, private) = connect(&h, "alice", &[], Some("private")).await;
    let (_, workspace) = connect(&h, "boss", &["firm_admin"], Some("workspace")).await;
    let (private, workspace) = (private.unwrap(), workspace.unwrap());
    let listed = |user: &str, groups: &[&str]| {
        let me = token(h.tenant, user, groups);
        let app = h.app.clone();
        async move {
            let (_, body) = call(&app, "GET", "/v1/connections", &me, None).await;
            body["connections"].as_array().unwrap().iter().map(|c| (c["id"].as_str().unwrap().to_string(), c["scope"].as_str().unwrap().to_string(), c["yours"].as_bool().unwrap())).collect::<Vec<_>>()
        }
    };
    // Alice sees her own and the firm's; Bob and the administrator see only the firm's.
    let alice = listed("alice", &[]).await;
    assert!(alice.contains(&(private.to_string(), "private".into(), true)) && alice.iter().any(|c| c.0 == workspace.to_string()), "{alice:?}");
    for (user, groups) in [("bob", &[][..]), ("boss", &["firm_admin"][..])] {
        let seen = listed(user, groups).await;
        assert!(!seen.iter().any(|c| c.0 == private.to_string()), "{user} saw it: {seen:?}");
        assert!(seen.iter().any(|c| c.0 == workspace.to_string()));
    }
    // Managing: the owner, and only the owner, of a private connection.
    let (bob, boss, alice_t) = (token(h.tenant, "bob", &[]), token(h.tenant, "boss", &["firm_admin"]), token(h.tenant, "alice", &[]));
    for path in [format!("/v1/connections/{private}/sync"), format!("/v1/connections/{private}/files")] {
        let method = if path.ends_with("sync") { "POST" } else { "GET" };
        assert_eq!(call(&h.app, method, &path, &bob, None).await.0, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(call(&h.app, method, &path, &boss, None).await.0, StatusCode::NOT_FOUND, "an administrator is told it does not exist: {path}");
    }
    assert_eq!(call(&h.app, "DELETE", &format!("/v1/connections/{private}"), &boss, None).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&h.app, "POST", &format!("/v1/connections/{private}/sync"), &alice_t, None).await.0, StatusCode::ACCEPTED);
    // A workspace connection is the administrators', not an ordinary employee's.
    assert_eq!(call(&h.app, "DELETE", &format!("/v1/connections/{workspace}"), &alice_t, None).await.0, StatusCode::FORBIDDEN);
    assert_eq!(call(&h.app, "DELETE", &format!("/v1/connections/{private}"), &alice_t, None).await.0, StatusCode::NO_CONTENT);
    assert_eq!(call(&h.app, "DELETE", &format!("/v1/connections/{workspace}"), &boss, None).await.0, StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn changing_who_a_connection_is_for_takes_effect_at_once_when_narrowing_and_at_the_next_sync_when_widening() {
    let h = harness().await;
    let (_, id) = connect(&h, "boss", &["firm_admin"], Some("workspace")).await;
    let id = id.unwrap();
    sync(&h).await;
    assert_eq!(finds(&h, "bob", &[]).await, ["Retainer.txt"]);

    let boss = token(h.tenant, "boss", &["firm_admin"]);
    let put = |scope: &str| json!({ "scope": scope });
    // Only the person who connected it may change it.
    let (other_admin, bob) = (token(h.tenant, "boss2", &["firm_admin"]), token(h.tenant, "bob", &[]));
    assert_eq!(call(&h.app, "PUT", &format!("/v1/connections/{id}/scope"), &other_admin, Some(put("private"))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&h.app, "PUT", &format!("/v1/connections/{id}/scope"), &bob, Some(put("private"))).await.0, StatusCode::NOT_FOUND);
    assert_eq!(call(&h.app, "PUT", &format!("/v1/connections/{id}/scope"), &boss, Some(put("somebody"))).await.0, StatusCode::BAD_REQUEST);

    // Narrowing hides every file immediately, before any sync runs.
    assert_eq!(call(&h.app, "PUT", &format!("/v1/connections/{id}/scope"), &boss, Some(put("private"))).await.0, StatusCode::OK);
    assert!(finds(&h, "bob", &[]).await.is_empty());
    assert_eq!(finds(&h, "boss", &["firm_admin"]).await, ["Retainer.txt"]);
    // A sync that runs while it is private keeps it private.
    sync(&h).await;
    assert!(finds(&h, "bob", &[]).await.is_empty());

    // Widening gives each file its real sharing back once the sync has run.
    assert_eq!(call(&h.app, "PUT", &format!("/v1/connections/{id}/scope"), &boss, Some(put("workspace"))).await.0, StatusCode::OK);
    assert!(finds(&h, "bob", &[]).await.is_empty(), "nothing is exposed before the sync");
    sync(&h).await;
    assert_eq!(finds(&h, "bob", &[]).await, ["Retainer.txt"]);

    // An ordinary employee can narrow their own, but never widen it.
    let (_, mine) = connect(&h, "alice", &[], Some("private")).await;
    let alice = token(h.tenant, "alice", &[]);
    assert_eq!(call(&h.app, "PUT", &format!("/v1/connections/{}/scope", mine.unwrap()), &alice, Some(put("workspace"))).await.0, StatusCode::FORBIDDEN);
    let _ = &h.world;
}
