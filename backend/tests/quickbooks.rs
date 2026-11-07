//! QuickBooks Online: the sign-in that records which company was approved, and syncing a company's
//! records and reports, first in full and then by what changed. Against stand-ins for Intuit.
//! Run with scripts/test.sh.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::Env;
use fakes::more::{self, Qbo, QboWorld};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use workspace_cloud::{
    api::{AppState, router},
    auth::{Principal, TenantId, TokenVerifier},
    connectors::{StaticToken, quickbooks::QuickbooksConnector},
    db::TenantTx,
    extract_process::UnsandboxedExtractor,
    oauth::{OAuthSettings, ProviderConfig},
    secrets::MemorySecrets,
    sync::sync_connection,
};

const PRIVATE: &[u8] = include_bytes!("fixtures/test_rsa_private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test_rsa_public.pem");
const ISSUER: &str = "https://issuer.example/pool";
const AUDIENCE: &str = "desktop-client";
const REDIRECT: &str = "https://api.example/oauth/callback";
const STATE_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

fn person(tenant: TenantId, email: &str) -> Principal {
    Principal { tenant, user: email.into(), groups: vec![], email: Some(email.into()) }
}

async fn find(env: &Env, tenant: TenantId, who: &str, query: &str) -> Vec<String> {
    let mut api = env.api().await;
    let tx = TenantTx::begin(&mut api, tenant).await.unwrap();
    let mut names: Vec<String> = tx.search(&person(tenant, who), query, 50).await.unwrap().into_iter().map(|h| h.document_name).collect();
    names.sort();
    names.dedup();
    names
}

async fn count(env: &Env, connection: Uuid) -> i64 {
    env.admin.query_one("SELECT count(*) FROM documents WHERE connection_id = $1", &[&connection]).await.unwrap().get(0)
}

fn invoice(id: u32, customer: &str, sync_token: &str, balance: f64) -> Value {
    json!({
        "Id": id.to_string(), "SyncToken": sync_token, "DocNumber": format!("{}", 1000 + id), "TxnDate": "2026-03-01",
        "CustomerRef": { "name": customer }, "CurrencyRef": { "value": "CAD" }, "TotalAmt": 1250.0, "Balance": balance,
        "MetaData": { "LastUpdatedTime": "2026-03-02T09:00:00-08:00" },
        "Line": [{ "Amount": 1250.0, "Description": "Bookkeeping retainer", "DetailType": "SalesItemLineDetail", "SalesItemLineDetail": {} }],
    })
}

async fn setup() -> (Env, Qbo, TenantId, Uuid, QuickbooksConnector) {
    let env = Env::new().await;
    let world: Qbo = Arc::new(Mutex::new(QboWorld::default()));
    {
        let mut w = world.lock().unwrap();
        w.add("Customer", json!({ "Id": "9", "SyncToken": "0", "DisplayName": "Whitfield Dental", "Balance": 250.0, "MetaData": { "LastUpdatedTime": "2026-01-01T09:00:00-08:00" } }));
        w.add("Customer", json!({ "Id": "10", "SyncToken": "0", "DisplayName": "Turnip Farms", "MetaData": { "LastUpdatedTime": "2026-01-01T09:00:00-08:00" } }));
        // More than one page of invoices, to prove paging.
        for id in 1..=105 {
            w.add("Invoice", invoice(id, if id == 7 { "Whitfield Dental" } else { "Turnip Farms" }, "0", 0.0));
        }
    }
    let (tenant, connection) = env.firm_with_connection("Firm", "quickbooks", json!({ "realm_id": "9341454", "company_name": "Firm Books Ltd" })).await;
    let base = more::quickbooks(world.clone()).await;
    let connector = QuickbooksConnector::new(Box::new(StaticToken("tok".into())), &base, "9341454", "Firm Books Ltd")
        .unwrap()
        .always_readable_by(vec!["user:alice@firm.com".into()]);
    (env, world, tenant, connection, connector)
}

#[tokio::test]
async fn a_company_is_read_in_full_across_pages_and_only_the_person_who_connected_it_can_find_it() {
    let (env, world, tenant, connection, connector) = setup().await;
    let pool = env.pool("t_worker", 4);
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    // 2 customers + 105 invoices + 4 reports (two reports, this and last fiscal year).
    assert_eq!((stats.stored, stats.full_listing), (111, true), "{stats:?}");
    assert_eq!(count(&env, connection).await, 111);

    // Invoices are found by customer and by what they were for; reports by what is in them.
    assert_eq!(find(&env, tenant, "alice@firm.com", "Whitfield").await, ["Customer: Whitfield Dental (QuickBooks)", "Invoice 1007 – Whitfield Dental (QuickBooks)"]);
    assert!(find(&env, tenant, "alice@firm.com", "zebra").await.iter().all(|name| name.ends_with("(QuickBooks)")));
    assert!(!find(&env, tenant, "alice@firm.com", "zebra").await.is_empty());
    // Nobody else can: not a colleague, not another firm's staff.
    assert!(find(&env, tenant, "bob@firm.com", "Whitfield").await.is_empty());
    assert!(find(&env, tenant, "bob@firm.com", "zebra").await.is_empty());

    // It asked for JSON, with its token, and read the invoices in pages of 100.
    let w = world.lock().unwrap();
    assert!(w.accepts.iter().all(|accept| accept == "application/json"), "{:?}", w.accepts);
    assert!(w.tokens.iter().all(|token| token == "Bearer tok"));
    assert!(w.queries.iter().any(|q| q.contains("from Invoice STARTPOSITION 1 MAXRESULTS 100")));
    assert!(w.queries.iter().any(|q| q.contains("from Invoice STARTPOSITION 101 MAXRESULTS 100")));
}

#[tokio::test]
async fn after_the_first_listing_only_what_changed_is_read_and_a_deleted_record_disappears() {
    let (env, world, tenant, connection, connector) = setup().await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    world.lock().unwrap().queries.clear();
    assert_eq!(find(&env, tenant, "alice@firm.com", "disputed").await.len(), 0);

    // Invoice 7 is edited (the client disputes it) and invoice 8 is deleted.
    {
        let mut w = world.lock().unwrap();
        let mut edited = invoice(7, "Whitfield Dental", "1", 600.0);
        edited["PrivateNote"] = json!("Disputed by the client");
        w.changed.push(("Invoice".into(), edited));
        w.changed.push(("Invoice".into(), json!({ "Id": "8", "domain": "QBO", "status": "Deleted" })));
    }
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert_eq!((stats.full_listing, stats.removed), (false, 1), "{stats:?}");
    assert_eq!(stats.stored, 1, "only the edited invoice is read again; the unchanged reports are skipped: {stats:?}");
    assert_eq!(count(&env, connection).await, 110);

    // The new text is what is found, and nothing was listed again from the start.
    assert_eq!(find(&env, tenant, "alice@firm.com", "disputed").await, ["Invoice 1007 – Whitfield Dental (QuickBooks)"]);
    let w = world.lock().unwrap();
    assert!(w.queries.is_empty(), "a change-feed sync does not list records: {:?}", w.queries);
    assert_eq!(w.cdc_since.len(), 1);
}

#[tokio::test]
async fn when_the_change_feed_no_longer_reaches_back_the_listing_starts_again() {
    let (env, world, tenant, connection, connector) = setup().await;
    let pool = env.pool("t_worker", 4);
    sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    // QuickBooks answers 400 when the time asked for is more than about 30 days ago.
    world.lock().unwrap().cdc_status = Some(400);
    let stats = sync_connection(&pool, tenant, connection, &connector, &UnsandboxedExtractor).await.unwrap();
    assert!(stats.full_listing, "{stats:?}");
    assert_eq!(count(&env, connection).await, 111);
}

// ---------------------------------------------------------------- Signing in

struct H {
    env: Env,
    app: Router,
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
    let world: Qbo = Arc::new(Mutex::new(QboWorld::default()));
    let mut provider = ProviderConfig::quickbooks("client".into(), "secret".into(), false);
    provider.token_url = fakes::token_endpoint("qbo-refresh", Arc::new(Mutex::new(0))).await;
    provider.api_base = more::quickbooks(world).await;
    let oauth = OAuthSettings { microsoft: None, google: None, dropbox: None, docusign: None, outlook_mail: None, quickbooks: Some(provider), zoom: None, redirect_uri: REDIRECT.into(), state_key: STATE_KEY.to_vec() };
    let app = router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool: env.pool("t_api", 4),
        oauth: Arc::new(oauth),
        secrets: Arc::new(MemorySecrets::default()),
        embedder: None,
    });
    let tenant = workspace_cloud::db::provision_tenant(&env.provisioner().await, "Firm A", None).await.unwrap();
    H { env, app, tenant }
}

async fn post_connect(h: &H, user: &str, body: Value) -> (StatusCode, Value) {
    let request = Request::builder()
        .method("POST")
        .uri("/v1/connections")
        .header("authorization", format!("Bearer {}", token(h.tenant, user)))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap();
    let response = h.app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null))
}

async fn callback(h: &H, state: &str, extra: &str) -> StatusCode {
    let request = Request::builder().uri(format!("/oauth/callback?code=good-code&state={state}{extra}")).body(Body::empty()).unwrap();
    h.app.clone().oneshot(request).await.unwrap().status()
}

async fn started(h: &H, user: &str, body: Value) -> (Value, String) {
    let (status, response) = post_connect(h, user, body).await;
    assert_eq!(status, StatusCode::OK, "{response}");
    let url = url::Url::parse(response["authorization_url"].as_str().unwrap()).unwrap();
    let state = url.query_pairs().find(|(k, _)| k == "state").unwrap().1.to_string();
    let scope = url.query_pairs().find(|(k, _)| k == "scope").unwrap().1.to_string();
    (json!({ "scope": scope, "url": url.as_str() }), state)
}

#[tokio::test]
async fn approving_a_company_records_which_one_and_it_is_always_private_to_the_person() {
    let h = harness().await;
    // Asking for the whole workspace is refused: books are connected for one person.
    let (status, _) = post_connect(&h, "alice", json!({ "kind": "quickbooks", "scope": "workspace" })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (asked, state) = started(&h, "alice", json!({ "kind": "quickbooks" })).await;
    assert_eq!(asked["scope"], "com.intuit.quickbooks.accounting");
    assert!(asked["url"].as_str().unwrap().starts_with("https://appcenter.intuit.com/connect/oauth2"));
    // Without the company ID QuickBooks gives back, nothing is connected.
    assert_eq!(callback(&h, &state, "").await, StatusCode::BAD_REQUEST);
    assert_eq!(callback(&h, &state, "&realmId=12/../3").await, StatusCode::BAD_REQUEST);
    let none: i64 = h.env.admin.query_one("SELECT count(*) FROM connections WHERE kind = 'quickbooks'", &[]).await.unwrap().get(0);
    assert_eq!(none, 0);

    assert_eq!(callback(&h, &state, "&realmId=9341454").await, StatusCode::OK);
    let row = h.env.admin.query_one("SELECT visibility, owner, label, config FROM connections WHERE kind = 'quickbooks'", &[]).await.unwrap();
    assert_eq!((row.get::<_, String>(0), row.get::<_, String>(1), row.get::<_, Option<String>>(2).as_deref()), ("private".into(), "alice".into(), Some("Firm Books Ltd")));
    let config: Value = row.get(3);
    assert_eq!((config["realm_id"].as_str(), config["company_name"].as_str()), (Some("9341454"), Some("Firm Books Ltd")));
    // It is queued to be read, and nobody else sees it in their list.
    let queued: i64 = h.env.admin.query_one("SELECT count(*) FROM sync_queue", &[]).await.unwrap().get(0);
    assert_eq!(queued, 1);

    // The same person can connect a second company: it is not replaced.
    let (_, again) = started(&h, "alice", json!({ "kind": "quickbooks" })).await;
    assert_eq!(callback(&h, &again, "&realmId=1111222233").await, StatusCode::OK);
    let both: i64 = h.env.admin.query_one("SELECT count(*) FROM connections WHERE kind = 'quickbooks'", &[]).await.unwrap().get(0);
    assert_eq!(both, 2);
}
