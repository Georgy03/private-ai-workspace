//! The HTTP API end to end: real routes, real tokens, real database. Run with scripts/test.sh.

mod common;

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use common::{Env, Firm};
use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tokio_postgres::NoTls;
use tower::ServiceExt;
use workspace_cloud::{
    api::{AppState, router},
    auth::TokenVerifier,
    db,
};

const PRIVATE: &[u8] = include_bytes!("fixtures/test_rsa_private.pem");
const OTHER_PRIVATE: &[u8] = include_bytes!("fixtures/other_rsa_private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test_rsa_public.pem");
const ISSUER: &str = "https://issuer.example/pool";
const AUDIENCE: &str = "desktop-client";

fn token(firm: &Firm, user: &str, groups: &[&str]) -> String {
    token_with(&firm.tenant.0.to_string(), user, groups, PRIVATE)
}

fn token_with(tenant: &str, user: &str, groups: &[&str], key: &[u8]) -> String {
    let exp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 600;
    encode(
        &Header::new(Algorithm::RS256),
        &json!({
            "sub": user, "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id",
            "custom:tenant_id": tenant, "cognito:groups": groups,
        }),
        &EncodingKey::from_rsa_pem(key).unwrap(),
    )
    .unwrap()
}

/// One pooled connection only, so every request after the first reuses the same database
/// session. That is the situation in which a leaked firm setting would show up.
fn app(env: &Env, pool_size: usize) -> Router {
    app_with(env, pool_size, None)
}

fn app_with(
    env: &Env,
    pool_size: usize,
    embedder: Option<Arc<dyn workspace_cloud::embed::Embedder>>,
) -> Router {
    let mut config = env.config.clone();
    config.user("t_api").password("test");
    let manager = Manager::from_config(
        config,
        NoTls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    );
    let pool: Pool = Pool::builder(manager)
        .max_size(pool_size)
        .runtime(Runtime::Tokio1)
        .build()
        .unwrap();
    router(AppState {
        verifier: Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        jwks: None,
        pool,
        oauth: Arc::new(Default::default()),
        secrets: Arc::new(workspace_cloud::secrets::MemorySecrets::default()),
        embedder,
    })
}

async fn call(
    app: &Router,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<String>,
) -> (StatusCode, Value) {
    let mut request = Request::builder().method(method).uri(path);
    if let Some(token) = token {
        request = request.header("authorization", format!("Bearer {token}"));
    }
    let request = match body {
        Some(body) => request
            .header("content-type", "application/json")
            .body(Body::from(body)),
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

async fn search(app: &Router, token: Option<&str>, query: &str) -> (StatusCode, Value) {
    call(
        app,
        "POST",
        "/v1/search",
        token,
        Some(json!({ "query": query }).to_string()),
    )
    .await
}

fn names(body: &Value) -> Vec<String> {
    let mut names: Vec<String> = body["results"]
        .as_array()
        .map(|results| {
            results
                .iter()
                .map(|result| result["document_name"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[tokio::test]
async fn health_is_open_and_everything_else_needs_a_valid_token() {
    let env = Env::new().await;
    let app = app(&env, 2);
    assert_eq!(
        call(&app, "GET", "/healthz", None, None).await.0,
        StatusCode::OK
    );
    assert_eq!(
        call(&app, "GET", "/readyz", None, None).await.0,
        StatusCode::OK
    );

    let firm = env
        .firm("Firm A", "a.txt", "contract", &["user:alice"])
        .await;
    let good = token(&firm, "alice", &[]);
    let forged = token_with(&firm.tenant.0.to_string(), "alice", &[], OTHER_PRIVATE);
    for bad in [None, Some("garbage"), Some(forged.as_str())] {
        assert_eq!(
            search(&app, bad, "contract").await.0,
            StatusCode::UNAUTHORIZED,
            "{bad:?}"
        );
        assert_eq!(
            call(&app, "GET", "/v1/me", bad, None).await.0,
            StatusCode::UNAUTHORIZED
        );
    }
    // A header that is not a bearer token is refused too.
    let request = Request::builder()
        .uri("/v1/me")
        .header("authorization", format!("Basic {good}"))
        .body(Body::empty())
        .unwrap();
    assert_eq!(
        app.clone().oneshot(request).await.unwrap().status(),
        StatusCode::UNAUTHORIZED
    );

    let (status, me) = call(&app, "GET", "/v1/me", Some(&good), None).await;
    assert_eq!(
        (status, me["user"].as_str()),
        (StatusCode::OK, Some("alice"))
    );
}

#[tokio::test]
async fn each_firm_only_ever_gets_its_own_files_even_on_a_shared_connection() {
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
    // One connection, same user ID in both firms, alternating requests.
    let app = app(&env, 1);
    for _ in 0..3 {
        let (_, for_a) = search(&app, Some(&token(&a, "alice", &[])), "merger").await;
        assert_eq!(names(&for_a), ["plan-a.txt"]);
        let (_, for_b) = search(&app, Some(&token(&b, "alice", &[])), "merger").await;
        assert_eq!(names(&for_b), ["plan-b.txt"]);
    }
}

#[tokio::test]
async fn people_only_see_what_the_source_system_lets_them_read() {
    let env = Env::new().await;
    let firm = env
        .firm("Firm A", "mine.txt", "contract terms", &["user:alice"])
        .await;
    let mut worker = env.worker().await;
    let tx = db::TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    for (name, readers) in [("partners.txt", "group:partners"), ("bobs.txt", "user:bob")] {
        let document = tx
            .upsert_document(&db::NewDocument {
                connection_id: firm.connection,
                external_id: name.into(),
                name: name.into(),
                path: String::new(),
                readable_by: vec![readers.into()],
            })
            .await
            .unwrap();
        tx.add_chunk(document, 0, "contract terms").await.unwrap();
    }
    tx.commit().await.unwrap();

    let app = app(&env, 2);
    let (_, alice) = search(&app, Some(&token(&firm, "alice", &[])), "contract").await;
    assert_eq!(names(&alice), ["mine.txt"]);
    let (_, partner) = search(
        &app,
        Some(&token(&firm, "alice", &["partners"])),
        "contract",
    )
    .await;
    assert_eq!(names(&partner), ["mine.txt", "partners.txt"]);
    let (_, bob) = search(&app, Some(&token(&firm, "bob", &[])), "contract").await;
    assert_eq!(names(&bob), ["bobs.txt"]);
    let (_, nobody) = search(&app, Some(&token(&firm, "mallory", &[])), "contract").await;
    assert!(names(&nobody).is_empty());
}

#[tokio::test]
async fn searches_are_audited_without_recording_the_question() {
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "a.txt",
            "confidential settlement",
            &["user:alice"],
        )
        .await;
    let app = app(&env, 2);
    let secret = "settlement with Globex";
    let (status, _) = search(&app, Some(&token(&firm, "alice", &[])), secret).await;
    assert_eq!(status, StatusCode::OK);

    let rows = env
        .admin
        .query(
            "SELECT actor, action, detail::text FROM audit_events WHERE tenant_id = $1",
            &[&firm.tenant.0],
        )
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, String>(0), "alice");
    assert_eq!(rows[0].get::<_, String>(1), "search");
    let detail: String = rows[0].get(2);
    assert!(
        detail.contains("query_chars") && !detail.contains("Globex"),
        "{detail}"
    );
}

#[tokio::test]
async fn bad_requests_are_refused_before_touching_the_database() {
    let env = Env::new().await;
    let firm = env
        .firm("Firm A", "a.txt", "contract", &["user:alice"])
        .await;
    let app = app(&env, 2);
    let good = token(&firm, "alice", &[]);

    assert_eq!(
        search(&app, Some(&good), "   ").await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        search(&app, Some(&good), &"a".repeat(2001)).await.0,
        StatusCode::BAD_REQUEST
    );
    let (status, _) = call(
        &app,
        "POST",
        "/v1/search",
        Some(&good),
        Some("{not json".into()),
    )
    .await;
    assert!(status.is_client_error());
    let huge = json!({ "query": "x".repeat(20_000) }).to_string();
    let (status, _) = call(&app, "POST", "/v1/search", Some(&good), Some(huge)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);

    // Nothing above reached the audit log.
    let count: i64 = env
        .admin
        .query_one("SELECT count(*) FROM audit_events", &[])
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0);

    // An odd limit is clamped rather than trusted.
    let (status, _) = call(
        &app,
        "POST",
        "/v1/search",
        Some(&good),
        Some(json!({ "query": "contract", "limit": 1_000_000 }).to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn tokens_for_removed_or_suspended_firms_are_refused() {
    let env = Env::new().await;
    let suspended = env
        .firm("Firm S", "s.txt", "contract", &["user:alice"])
        .await;
    let removed = env
        .firm("Firm R", "r.txt", "contract", &["user:alice"])
        .await;
    let app = app(&env, 2);
    let (suspended_token, removed_token) = (
        token(&suspended, "alice", &[]),
        token(&removed, "alice", &[]),
    );
    assert_eq!(
        search(&app, Some(&suspended_token), "contract").await.0,
        StatusCode::OK
    );

    env.admin
        .execute(
            "UPDATE tenants SET status = 'suspended' WHERE id = $1",
            &[&suspended.tenant.0],
        )
        .await
        .unwrap();
    db::offboard_tenant(&env.provisioner().await, removed.tenant)
        .await
        .unwrap();

    assert_eq!(
        search(&app, Some(&suspended_token), "contract").await.0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        search(&app, Some(&removed_token), "contract").await.0,
        StatusCode::FORBIDDEN
    );
    // A token naming a firm that never existed is refused the same way.
    let unknown = token_with(
        "00000000-0000-4000-8000-000000000000",
        "alice",
        &[],
        PRIVATE,
    );
    assert_eq!(
        search(&app, Some(&unknown), "contract").await.0,
        StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn a_token_signed_with_a_newly_rotated_key_is_accepted_after_one_refresh() {
    use jsonwebtoken::jwk::Jwk;
    use workspace_cloud::jwks::JwksSource;

    let jwk = |kid: &str| {
        let key = EncodingKey::from_rsa_pem(PRIVATE).unwrap();
        let mut jwk =
            serde_json::to_value(Jwk::from_encoding_key(&key, Algorithm::RS256).unwrap()).unwrap();
        jwk["kid"] = json!(kid);
        jwk
    };
    let both = json!({ "keys": [jwk("old"), jwk("new")] }).to_string();
    let only_old = json!({ "keys": [jwk("old")] }).to_string();

    // A stand-in for the identity provider's key endpoint.
    let served = both.clone();
    let provider = Router::new().route(
        "/jwks.json",
        axum::routing::get(move || async move { served }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/jwks.json", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });

    let env = Env::new().await;
    let firm = env
        .firm("Firm A", "a.txt", "contract", &["user:alice"])
        .await;
    let verifier = Arc::new(TokenVerifier::new(ISSUER, AUDIENCE));
    verifier.set_jwks(&only_old).unwrap();
    let mut config = env.config.clone();
    config.user("t_api").password("test");
    let pool = Pool::builder(Manager::from_config(
        config,
        NoTls,
        ManagerConfig {
            recycling_method: RecyclingMethod::Fast,
        },
    ))
    .max_size(2)
    .runtime(Runtime::Tokio1)
    .build()
    .unwrap();
    let app = router(AppState {
        verifier,
        jwks: Some(Arc::new(JwksSource::new(url).unwrap())),
        pool,
        oauth: Arc::new(Default::default()),
        secrets: Arc::new(workspace_cloud::secrets::MemorySecrets::default()),
        embedder: None,
    });

    let sign = |kid: &str| {
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(kid.into());
        let exp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            + 600;
        encode(
            &header,
            &json!({
                "sub": "alice", "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id",
                "custom:tenant_id": firm.tenant.0.to_string(),
            }),
            &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
        )
        .unwrap()
    };
    assert_eq!(
        search(&app, Some(&sign("old")), "contract").await.0,
        StatusCode::OK
    );
    assert_eq!(
        search(&app, Some(&sign("new")), "contract").await.0,
        StatusCode::OK
    );
    // A key ID nobody publishes is still refused after the refresh.
    assert_eq!(
        search(&app, Some(&sign("forged")), "contract").await.0,
        StatusCode::UNAUTHORIZED
    );
}

async fn add_file(env: &Env, firm: &Firm, name: &str, passages: &[&str], readers: &[&str]) {
    let mut worker = env.worker().await;
    let tx = db::TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    let document = tx
        .upsert_document(&db::NewDocument {
            connection_id: firm.connection,
            external_id: name.into(),
            name: name.into(),
            path: String::new(),
            readable_by: readers.iter().map(|r| r.to_string()).collect(),
        })
        .await
        .unwrap();
    for (ordinal, text) in passages.iter().enumerate() {
        tx.add_chunk(document, ordinal as i32, text).await.unwrap();
    }
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_question_phrased_as_a_question_still_finds_files_by_content_and_by_name() {
    let env = Env::new().await;
    // The firm needs one file to exist; the others are added below.
    let firm = env
        .firm(
            "Firm A",
            "Unrelated.txt",
            "quarterly tax estimates",
            &["user:alice"],
        )
        .await;
    add_file(
        &env,
        &firm,
        "2026 Men's Retreat MASTER",
        &["Weekend plan and cabin list."],
        &["user:alice"],
    )
    .await;
    add_file(
        &env,
        &firm,
        "Copy of Men\u{2019}s Retreat Theme",
        &["Theme: finding rest in the quiet."],
        &["user:alice"],
    )
    .await;
    add_file(
        &env,
        &firm,
        "Budget.txt",
        &["The mens retreat budget is four hundred dollars."],
        &["user:alice"],
    )
    .await;
    add_file(
        &env,
        &firm,
        "Youth Camp.txt",
        &["Camp schedule for the youth."],
        &["user:alice"],
    )
    .await;
    add_file(
        &env,
        &firm,
        "Board Memo.txt",
        &["Retreat location for the board."],
        &["user:bob"],
    )
    .await;
    let app = app(&env, 2);
    let alice = token(&firm, "alice", &[]);

    // The exact question that found nothing before: "tell" and the curly apostrophe broke it.
    let (status, found) = search(&app, Some(&alice), "tell me about men\u{2019}s retreat?").await;
    assert_eq!(status, StatusCode::OK);
    let found_names = names(&found);
    assert_eq!(
        found_names,
        [
            "2026 Men's Retreat MASTER",
            "Budget.txt",
            "Copy of Men\u{2019}s Retreat Theme"
        ],
        "files whose names or text match any of the content words, and nothing else"
    );
    // A file that matches by name ranks above one that only mentions the words.
    let order: Vec<&str> = found["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["document_name"].as_str().unwrap())
        .collect();
    let budget = order.iter().position(|n| *n == "Budget.txt").unwrap();
    assert!(order.iter().position(|n| n.contains("MASTER")).unwrap() < budget);
    assert!(order.iter().position(|n| n.contains("Theme")).unwrap() < budget);
    // Permissions still apply: Bob's file is not alice's, and unrelated files stay out.
    assert!(
        !found_names.contains(&"Board Memo.txt".to_string())
            && !found_names.contains(&"Youth Camp.txt".to_string())
    );

    // The same question with the plain apostrophe, and with fewer words, behaves the same.
    for question in [
        "tell me about men's retreat?",
        "men retreat",
        "Mens Retreat",
    ] {
        let (_, again) = search(&app, Some(&alice), question).await;
        assert!(
            names(&again).contains(&"2026 Men's Retreat MASTER".to_string()),
            "{question}"
        );
    }
}

#[tokio::test]
async fn one_long_file_cannot_fill_the_results() {
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "Other.txt",
            "the retreat schedule is posted",
            &["user:alice"],
        )
        .await;
    let many: Vec<String> = (0..8)
        .map(|n| format!("retreat passage number {n}"))
        .collect();
    let many: Vec<&str> = many.iter().map(String::as_str).collect();
    add_file(&env, &firm, "Long Notes.txt", &many, &["user:alice"]).await;
    let app = app(&env, 2);
    let (_, found) = search(&app, Some(&token(&firm, "alice", &[])), "retreat").await;
    let from_long = found["results"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["document_name"] == "Long Notes.txt")
        .count();
    assert_eq!(from_long, 3, "at most three passages from any one file");
    assert!(
        names(&found).contains(&"Other.txt".to_string()),
        "another file still gets a place"
    );
}

#[tokio::test]
async fn a_question_with_nothing_to_search_for_or_with_hostile_characters_is_handled_calmly() {
    let env = Env::new().await;
    let firm = env
        .firm("Firm A", "a.txt", "contract terms", &["user:alice"])
        .await;
    let app = app(&env, 2);
    let alice = token(&firm, "alice", &[]);
    for question in [
        "tell me please",
        "?!",
        "x') | !secret & (a:* <-> b); DROP TABLE documents; --",
        "\"unbalanced",
        "\\",
    ] {
        let (status, body) = search(&app, Some(&alice), question).await;
        assert_eq!(status, StatusCode::OK, "{question:?}: {body}");
        assert!(body["results"].is_array());
    }
    // The table is still there.
    let (_, found) = search(&app, Some(&alice), "contract").await;
    assert_eq!(names(&found), ["a.txt"]);
}

fn report(model: &str, input: i64, output: i64) -> Option<String> {
    Some(json!({ "model": model, "input_tokens": input, "output_tokens": output }).to_string())
}

#[tokio::test]
async fn answers_are_priced_by_the_server_and_counted_per_person_per_month() {
    let env = Env::new().await;
    let firm = env.firm("Firm A", "a.txt", "text", &["user:alice"]).await;
    let app = app(&env, 1);
    let alice = token(&firm, "alice", &[]);
    let bob = token(&firm, "bob", &[]);

    let (status, view) = call(&app, "GET", "/v1/usage", Some(&alice), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["used_micros"], 0);
    assert_eq!(
        view["limit_micros"], 15_000_000,
        "the default is fifteen dollars"
    );
    assert_eq!(view["limit_is_default"], true);

    // 3,500 tokens in and 700 out on Sonnet 5.5 cost 1.4 cents, whatever the caller claims.
    let body = json!({ "model": "claude-sonnet-5-5", "input_tokens": 3500, "output_tokens": 700, "cost_micros": 1 });
    let (status, view) = call(
        &app,
        "POST",
        "/v1/usage",
        Some(&alice),
        Some(body.to_string()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (view["used_micros"].as_i64(), view["answers"].as_i64()),
        (Some(14_000), Some(1))
    );
    call(
        &app,
        "POST",
        "/v1/usage",
        Some(&alice),
        report("claude-haiku-5-5", 3500, 700),
    )
    .await;
    let (_, view) = call(&app, "GET", "/v1/usage", Some(&alice), None).await;
    assert_eq!(view["used_micros"], 14_700);
    assert_eq!(view["remaining_micros"], 15_000_000 - 14_700);

    // Another person in the same firm starts from nothing.
    let (_, view) = call(&app, "GET", "/v1/usage", Some(&bob), None).await;
    assert_eq!(view["used_micros"], 0);

    // Nonsense is refused.
    for bad in [
        report("claude-sonnet-5-5", -1, 0),
        report("claude-sonnet-5-5", 0, 6_000_000),
        report("", 1, 1),
    ] {
        let (status, _) = call(&app, "POST", "/v1/usage", Some(&alice), bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, _) = call(&app, "GET", "/v1/usage", None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn only_an_administrator_sets_the_limit_and_it_applies_to_that_firm_alone() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "text", &["user:alice"]).await;
    let b = env.firm("Firm B", "b.txt", "text", &["user:bob"]).await;
    let app = app(&env, 1);
    let (member, admin, other) = (
        token(&a, "alice", &[]),
        token(&a, "boss", &["firm_admin"]),
        token(&b, "bob", &[]),
    );

    let set = |cents: Value| Some(json!({ "monthly_limit_cents": cents }).to_string());
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/usage/limit",
        Some(&member),
        set(json!(200)),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a member cannot raise their own limit"
    );

    let (status, view) = call(
        &app,
        "PUT",
        "/v1/usage/limit",
        Some(&admin),
        set(json!(200)),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (
            view["limit_micros"].as_i64(),
            view["limit_is_default"].clone()
        ),
        (Some(2_000_000), json!(false))
    );
    let (_, view) = call(&app, "GET", "/v1/usage", Some(&member), None).await;
    assert_eq!(view["limit_micros"], 2_000_000);
    let (_, view) = call(&app, "GET", "/v1/usage", Some(&other), None).await;
    assert_eq!(view["limit_micros"], 15_000_000, "firm B keeps the default");

    for bad in [json!(-1), json!(10_000_001)] {
        let (status, _) = call(&app, "PUT", "/v1/usage/limit", Some(&admin), set(bad)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (_, view) = call(
        &app,
        "PUT",
        "/v1/usage/limit",
        Some(&admin),
        set(Value::Null),
    )
    .await;
    assert_eq!(
        view["limit_is_default"], true,
        "null returns to the default"
    );
    // The change is in the audit log, without anything but the number.
    let audited: i64 = env
        .admin
        .query_one(
            "SELECT count(*) FROM audit_events WHERE action = 'usage_limit_changed'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(audited, 2);
}

#[tokio::test]
async fn the_team_view_is_for_administrators_and_never_crosses_firms() {
    let env = Env::new().await;
    let a = env.firm("Firm A", "a.txt", "text", &["user:alice"]).await;
    let b = env.firm("Firm B", "b.txt", "text", &["user:bob"]).await;
    let app = app(&env, 1);
    let (alice, boss, bob) = (
        token(&a, "alice", &[]),
        token(&a, "boss", &["firm_admin"]),
        token(&b, "bob", &[]),
    );
    call(
        &app,
        "POST",
        "/v1/usage",
        Some(&alice),
        report("claude-sonnet-5-5", 1000, 100),
    )
    .await;
    call(
        &app,
        "POST",
        "/v1/usage",
        Some(&bob),
        report("claude-sonnet-5-5", 9000, 900),
    )
    .await;

    let (status, _) = call(&app, "GET", "/v1/usage/team", Some(&alice), None).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, team) = call(&app, "GET", "/v1/usage/team", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    let people = team["people"].as_array().unwrap();
    assert_eq!(people.len(), 1, "only Firm A's people: {team}");
    assert_eq!(people[0]["user"], "alice");
    assert_eq!(people[0]["used_micros"], 3_000);
}

#[tokio::test]
async fn the_usage_ledger_cannot_be_rewritten_by_the_api_role() {
    let env = Env::new().await;
    let row = env
        .admin
        .query_one(
            "SELECT has_table_privilege('app_api', 'usage_events', 'UPDATE'),
                    has_table_privilege('app_api', 'usage_events', 'DELETE'),
                    has_table_privilege('app_worker', 'usage_events', 'UPDATE')
                 OR has_table_privilege('app_worker', 'usage_events', 'DELETE')",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (
            row.get::<_, bool>(0),
            row.get::<_, bool>(1),
            row.get::<_, bool>(2)
        ),
        (false, false, false)
    );
}

/// A document of three passages, readable by Alice only.
async fn long_document(env: &Env) -> (Firm, uuid::Uuid) {
    let firm = env
        .firm("Firm A", "plan.txt", "placeholder", &["user:alice"])
        .await;
    let mut worker = env.worker().await;
    let tx = db::TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    tx.replace_chunks(
        firm.document,
        &[
            db::Passage {
                text: "Section 1. The retainer is due on signing.".into(),
                label: Some("Page 1".into()),
            },
            db::Passage {
                text: "Section 2. Late fees start after thirty days.".into(),
                label: Some("Page 2".into()),
            },
            db::Passage {
                text: "Section 3. Either side may end the agreement in writing.".into(),
                label: None,
            },
        ],
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let id = firm.document;
    (firm, id)
}

#[tokio::test]
async fn the_whole_document_comes_back_in_order_to_someone_who_may_read_it() {
    let env = Env::new().await;
    let (firm, id) = long_document(&env).await;
    let app = app(&env, 1);
    let alice = token(&firm, "alice", &[]);

    let (status, body) = call(
        &app,
        "GET",
        &format!("/v1/documents/{id}"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["too_long"], false);
    let passages = body["passages"].as_array().unwrap();
    let texts: Vec<&str> = passages
        .iter()
        .map(|p| p["text"].as_str().unwrap())
        .collect();
    assert_eq!(
        texts,
        [
            "Section 1. The retainer is due on signing.",
            "Section 2. Late fees start after thirty days.",
            "Section 3. Either side may end the agreement in writing.",
        ]
    );
    assert_eq!(passages[0]["label"], "Page 1");
    assert_eq!(passages[2]["label"], Value::Null);
    assert_eq!(body["total_chars"], 42 + 45 + 56);

    // The audit log records that it was read, and how long it was, but never the text.
    let audit: String = env
        .admin
        .query_one(
            "SELECT detail::text FROM audit_events WHERE action = 'document_read'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(
        audit.contains("143") && !audit.contains("retainer"),
        "{audit}"
    );
}

#[tokio::test]
async fn nobody_else_can_read_a_whole_document_and_a_missing_one_looks_the_same() {
    let env = Env::new().await;
    let (firm, id) = long_document(&env).await;
    let other = env.firm("Firm B", "b.txt", "text", &["user:bob"]).await;
    let app = app(&env, 1);
    let path = format!("/v1/documents/{id}");

    // Another person in the same firm, who is not on the file's list.
    let (status, body) = call(&app, "GET", &path, Some(&token(&firm, "bob", &[])), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.to_string().contains("Section"), "{body}");
    // Another firm, even with the right document ID.
    let (status, _) = call(&app, "GET", &path, Some(&token(&other, "alice", &[])), None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // No such document, and no sign-in.
    let missing = format!("/v1/documents/{}", uuid::Uuid::new_v4());
    let (status, _) = call(
        &app,
        "GET",
        &missing,
        Some(&token(&firm, "alice", &[])),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = call(&app, "GET", &path, None, None).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn a_document_longer_than_the_limit_is_reported_without_its_text() {
    let env = Env::new().await;
    let (firm, id) = long_document(&env).await;
    let app = app(&env, 1);
    let alice = token(&firm, "alice", &[]);

    let (status, body) = call(
        &app,
        "GET",
        &format!("/v1/documents/{id}?max_chars=100"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        (body["too_long"].clone(), body["total_chars"].clone()),
        (json!(true), json!(143))
    );
    assert_eq!(
        body["passages"].as_array().unwrap().len(),
        0,
        "no text is sent for a document that is too long"
    );
    // Exactly the length is allowed.
    let (_, body) = call(
        &app,
        "GET",
        &format!("/v1/documents/{id}?max_chars=143"),
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(body["too_long"], false);
}

/// An embedder that answers with chosen vectors, so a test can say exactly what "close in meaning"
/// is without a model.
struct Fixed(Vec<f32>);

#[async_trait::async_trait]
impl workspace_cloud::embed::Embedder for Fixed {
    fn model(&self) -> &str {
        "fixed-test"
    }
    async fn embed_passages(
        &self,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f32>>, workspace_cloud::embed::EmbedError> {
        Ok(texts.iter().map(|_| self.0.clone()).collect())
    }
    async fn embed_query(&self, _: &str) -> Result<Vec<f32>, workspace_cloud::embed::EmbedError> {
        Ok(self.0.clone())
    }
}

/// A unit vector pointing mostly along one axis; `leans` blends in a second axis.
fn vector(axis: usize, leans: Option<(usize, f32)>) -> Vec<f32> {
    let mut v = vec![0.0f32; workspace_cloud::embed::DIMENSIONS];
    v[axis] = 1.0;
    if let Some((other, amount)) = leans {
        v[other] = amount;
    }
    workspace_cloud::embed::normalise(v)
}

async fn put_vector(env: &Env, firm: &Firm, vector: &[f32], model: &str) {
    let mut worker = env.worker().await;
    let tx = db::TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    let chunk: uuid::Uuid = tx
        .sql()
        .query_one(
            "SELECT id FROM chunks WHERE document_id = $1 LIMIT 1",
            &[&firm.document],
        )
        .await
        .unwrap()
        .get(0);
    tx.store_vector(chunk, model, vector).await.unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn a_passage_with_none_of_the_questions_words_is_found_by_meaning() {
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "fees.txt",
            "Interest accrues on unpaid balances after thirty days.",
            &["user:alice"],
        )
        .await;
    // The passage's vector points the same way as the question's.
    put_vector(&env, &firm, &vector(3, None), "fixed-test").await;
    let alice = token(&firm, "alice", &[]);

    // Words alone find nothing: the question shares no word with the passage.
    let plain = app(&env, 1);
    let (_, body) = search(&plain, Some(&alice), "what happens if the client pays late").await;
    assert!(names(&body).is_empty(), "{body}");

    // With meaning search on, the passage is found.
    let smart = app_with(&env, 1, Some(Arc::new(Fixed(vector(3, None)))));
    let (status, body) = search(&smart, Some(&alice), "what happens if the client pays late").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(names(&body), ["fees.txt"]);

    // A caller can ask for words only, which is how the two are compared.
    let (_, body) = call(
        &smart,
        "POST",
        "/v1/search",
        Some(&alice),
        Some(
            json!({ "query": "what happens if the client pays late", "meaning": false })
                .to_string(),
        ),
    )
    .await;
    assert!(names(&body).is_empty(), "{body}");
}

#[tokio::test]
async fn meaning_search_ignores_unrelated_passages_other_models_and_files_the_person_may_not_read()
{
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "fees.txt",
            "Interest accrues after thirty days.",
            &["user:alice"],
        )
        .await;
    let other_firm = env
        .firm(
            "Firm B",
            "b.txt",
            "Interest accrues after thirty days.",
            &["user:alice"],
        )
        .await;
    let question = Arc::new(Fixed(vector(3, None)));
    let app = app_with(&env, 1, Some(question));
    let alice = token(&firm, "alice", &[]);

    // Pointing the other way (nothing in common): below the similarity bar.
    put_vector(&env, &firm, &vector(4, None), "fixed-test").await;
    let (_, body) = search(&app, Some(&alice), "anything at all").await;
    assert!(
        names(&body).is_empty(),
        "an unrelated passage is not returned: {body}"
    );

    // Close in meaning but made by a different model: never compared.
    put_vector(&env, &firm, &vector(3, None), "some-other-model").await;
    let (_, body) = search(&app, Some(&alice), "anything at all").await;
    assert!(names(&body).is_empty(), "{body}");

    // The right model: found by its owner, not by someone not on the file's list.
    put_vector(&env, &firm, &vector(3, None), "fixed-test").await;
    let (_, body) = search(&app, Some(&alice), "anything at all").await;
    assert_eq!(names(&body), ["fees.txt"]);
    let (_, body) = search(&app, Some(&token(&firm, "bob", &[])), "anything at all").await;
    assert!(names(&body).is_empty(), "not on the file's list: {body}");

    // Another firm's identical passage, with the same vector, is invisible to Firm A and the
    // other way round.
    put_vector(&env, &other_firm, &vector(3, None), "fixed-test").await;
    let (_, body) = search(&app, Some(&alice), "anything at all").await;
    assert_eq!(names(&body), ["fees.txt"], "only Firm A's own file: {body}");
    let (_, body) = search(
        &app,
        Some(&token(&other_firm, "alice", &[])),
        "anything at all",
    )
    .await;
    assert_eq!(names(&body), ["b.txt"]);
}

async fn unembedded(env: &Env) -> i64 {
    env.admin
        .query_one("SELECT count(*) FROM chunks WHERE embedding IS NULL", &[])
        .await
        .unwrap()
        .get(0)
}

#[tokio::test]
async fn the_worker_gives_every_waiting_passage_a_vector_and_does_not_redo_them() {
    use workspace_cloud::{embed::HashingEmbedder, queue, worker::embed_pending};
    let env = Env::new().await;
    let a = env
        .firm(
            "Firm A",
            "a.txt",
            "The retainer is due on signing.",
            &["user:alice"],
        )
        .await;
    let b = env
        .firm(
            "Firm B",
            "b.txt",
            "Quarterly picnic at the lake.",
            &["user:bob"],
        )
        .await;
    // Firms with a connection are found through the sync queue.
    for firm in [&a, &b] {
        let mut worker = env.worker().await;
        let tx = db::TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
        queue::enqueue(&tx, firm.connection).await.unwrap();
        tx.commit().await.unwrap();
    }
    let pool = env.pool("t_worker", 2);
    assert_eq!(unembedded(&env).await, 2);
    assert_eq!(embed_pending(&pool, &HashingEmbedder).await.unwrap(), 2);
    assert_eq!(unembedded(&env).await, 0);
    let row = env
        .admin
        .query_one(
            "SELECT array_length(embedding, 1), embedding_model FROM chunks LIMIT 1",
            &[],
        )
        .await
        .unwrap();
    assert_eq!(
        (row.get::<_, i32>(0), row.get::<_, String>(1)),
        (
            workspace_cloud::embed::DIMENSIONS as i32,
            "hashing-test".to_string()
        )
    );
    assert_eq!(
        embed_pending(&pool, &HashingEmbedder).await.unwrap(),
        0,
        "nothing left to do"
    );
}

/// Seeds what the model found in the firm's one document, as the worker would store it.
async fn seed_knowledge(env: &Env, firm: &Firm) {
    use workspace_cloud::knowledge::{ExtractedEntity, ExtractedFact, Extraction};
    let mut worker = env.worker().await;
    let tx = db::TenantTx::begin(&mut worker, firm.tenant).await.unwrap();
    let extraction = Extraction {
        doc_type: "contract".into(),
        summary: "A roofing engagement.".into(),
        entities: vec![ExtractedEntity {
            name: "Maple Roofing LLC".into(),
            kind: "organization".into(),
            role: "vendor".into(),
            quote: "Maple Roofing".into(),
        }],
        facts: vec![ExtractedFact {
            kind: "fact".into(),
            statement: "The price is $18,400.".into(),
            subject: "Maple Roofing LLC".into(),
            quote: "The price is $18,400".into(),
        }],
        relationships: Vec::new(),
    };
    workspace_cloud::knowledge_store::store_analysis(
        &tx,
        firm.document,
        "",
        "scripted",
        &extraction,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}

#[tokio::test]
async fn only_an_administrator_sees_the_overview_and_switches_analysis_on() {
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "letter.docx",
            "Maple Roofing. The price is $18,400.",
            &["user:alice"],
        )
        .await;
    seed_knowledge(&env, &firm).await;
    let app = app(&env, 1);
    let (alice, boss) = (
        token(&firm, "alice", &[]),
        token(&firm, "boss", &["firm_admin"]),
    );

    let (status, _) = call(&app, "GET", "/v1/knowledge", Some(&alice), None).await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "counts come from every document, so staff cannot see them"
    );
    let (status, body) = call(&app, "GET", "/v1/knowledge", Some(&boss), None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["enabled"], false, "off until switched on");
    assert_eq!(body["overview"]["facts_total"], 1);
    assert_eq!(body["overview"]["facts_unverified"], 1);

    let on = Some(json!({ "enabled": true }).to_string());
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/knowledge/settings",
        Some(&alice),
        on.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(&app, "PUT", "/v1/knowledge/settings", Some(&boss), on).await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(&app, "GET", "/v1/knowledge", Some(&boss), None).await;
    assert_eq!(body["enabled"], true);
    let audited: i64 = env
        .admin
        .query_one(
            "SELECT count(*) FROM audit_events WHERE action = 'company_knowledge_switched'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(audited, 1);
}

#[tokio::test]
async fn staff_see_only_confirmed_knowledge_and_an_administrator_confirms_it() {
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "letter.docx",
            "Maple Roofing. The price is $18,400.",
            &["user:alice"],
        )
        .await;
    seed_knowledge(&env, &firm).await;
    let app = app(&env, 1);
    let (alice, boss) = (
        token(&firm, "alice", &[]),
        token(&firm, "boss", &["firm_admin"]),
    );

    let (_, staff) = call(&app, "GET", "/v1/knowledge/facts", Some(&alice), None).await;
    assert_eq!(
        staff["facts"].as_array().unwrap().len(),
        0,
        "nothing is confirmed yet"
    );
    // Asking for suggestions does not get a member any.
    let (_, staff) = call(
        &app,
        "GET",
        "/v1/knowledge/facts?status=suggested",
        Some(&alice),
        None,
    )
    .await;
    assert_eq!(staff["facts"].as_array().unwrap().len(), 0);
    let (_, admin) = call(&app, "GET", "/v1/knowledge/facts", Some(&boss), None).await;
    let facts = admin["facts"].as_array().unwrap();
    // The administrator can open the file too in this setup only because "user:boss" is not on it.
    assert_eq!(
        facts.len(),
        0,
        "an administrator is held to document permissions as well"
    );

    // The administrator is on the file's list here, so they can review it.
    let firm_b = env
        .firm(
            "Firm B",
            "x.docx",
            "Maple Roofing. The price is $18,400.",
            &["user:boss"],
        )
        .await;
    seed_knowledge(&env, &firm_b).await;
    let boss_b = token(&firm_b, "boss", &["firm_admin"]);
    let (_, admin) = call(&app, "GET", "/v1/knowledge/facts", Some(&boss_b), None).await;
    let id = admin["facts"][0]["id"].as_str().unwrap().to_string();
    assert_eq!(admin["facts"][0]["status"], "suggested");
    assert_eq!(
        admin["facts"][0]["evidence"][0]["quote"],
        "The price is $18,400"
    );

    let review = |action: &str| Some(json!({ "action": action }).to_string());
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/facts/{id}/review"),
        Some(&token(&firm_b, "alice", &[])),
        review("confirm"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "only an administrator reviews"
    );
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/facts/{id}/review"),
        Some(&boss_b),
        review("maybe"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/facts/{id}/review"),
        Some(&boss_b),
        Some(json!({ "action": "correct", "text": "x" }).to_string()),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a correction needs real text"
    );
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/facts/{id}/review"),
        Some(&boss_b),
        review("confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/facts/{}/review", uuid::Uuid::new_v4()),
        Some(&boss_b),
        review("confirm"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let reviewed: i64 = env
        .admin
        .query_one(
            "SELECT count(*) FROM audit_events WHERE action = 'fact_reviewed'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(reviewed, 1);
    // The audit log holds the action and the fact's id, never its text.
    let detail: String = env
        .admin
        .query_one(
            "SELECT detail::text FROM audit_events WHERE action = 'fact_reviewed'",
            &[],
        )
        .await
        .unwrap()
        .get(0);
    assert!(!detail.contains("18,400"), "{detail}");

    // Now a member of that firm who may open the file sees the confirmed fact.
    let (_, staff) = call(
        &app,
        "GET",
        "/v1/knowledge/facts",
        Some(&token(&firm_b, "boss", &[])),
        None,
    )
    .await;
    assert_eq!(staff["facts"][0]["statement"], "The price is $18,400.");
    assert_eq!(staff["facts"][0]["status"], "verified");
    // Another firm, even with the fact's ID, finds nothing.
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/facts/{id}/review"),
        Some(&boss),
        review("reject"),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn entities_can_be_listed_filtered_and_reviewed_and_bad_filters_are_refused() {
    let env = Env::new().await;
    let firm = env
        .firm(
            "Firm A",
            "letter.docx",
            "Maple Roofing. The price is $18,400.",
            &["user:boss"],
        )
        .await;
    seed_knowledge(&env, &firm).await;
    let app = app(&env, 1);
    let boss = token(&firm, "boss", &["firm_admin"]);

    let (_, body) = call(
        &app,
        "GET",
        "/v1/knowledge/entities?kind=organization&q=maple",
        Some(&boss),
        None,
    )
    .await;
    let list = body["entities"].as_array().unwrap();
    assert_eq!(
        (
            list.len(),
            list[0]["name"].clone(),
            list[0]["roles"][0].clone()
        ),
        (1, json!("Maple Roofing LLC"), json!("vendor"))
    );
    let (_, body) = call(
        &app,
        "GET",
        "/v1/knowledge/entities?kind=person",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(body["entities"].as_array().unwrap().len(), 0);
    let (status, _) = call(
        &app,
        "GET",
        "/v1/knowledge/entities?kind=dragon",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let id = list[0]["id"].as_str().unwrap();
    let rename = Some(json!({ "action": "correct", "text": "Maple Roofing Co." }).to_string());
    let (status, _) = call(
        &app,
        "POST",
        &format!("/v1/knowledge/entities/{id}/review"),
        Some(&boss),
        rename,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(
        &app,
        "GET",
        "/v1/knowledge/entities?status=verified",
        Some(&boss),
        None,
    )
    .await;
    assert_eq!(body["entities"][0]["name"], "Maple Roofing Co.");

    // The profile is confirmed by an administrator, with a correction.
    env.admin
        .execute("INSERT INTO company_profile (tenant_id, summary) SELECT id, 'A guess.' FROM tenants WHERE name = 'Firm A'", &[])
        .await
        .unwrap();
    let correct = Some(
        json!({ "summary": "We replace roofs.", "services": ["Roof replacement", " "] })
            .to_string(),
    );
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/knowledge/profile/review",
        Some(&token(&firm, "alice", &[])),
        correct.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _) = call(
        &app,
        "PUT",
        "/v1/knowledge/profile/review",
        Some(&boss),
        correct,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (_, body) = call(&app, "GET", "/v1/knowledge", Some(&boss), None).await;
    assert_eq!(body["profile"]["status"], "verified");
    assert_eq!(body["profile"]["summary"], "We replace roofs.");
    assert_eq!(body["profile"]["services"], json!(["Roof replacement"]));
}
