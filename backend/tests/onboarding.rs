//! A person with no firm starts their own. Real routes, real tokens and database; the user pool and the key
//! service are stand-ins that speak the real AWS protocols. Run with scripts/test.sh.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode, header},
};
use common::Env;
use fakes::{
    cognito::{SignIn, SignInWorld, cognito_and_ssm, rich_client},
    kms::{Kms, KmsWorld, kms},
};
use http_body_util::BodyExt;
use jsonwebtoken::{Algorithm, EncodingKey, Header, encode};
use serde_json::{Value, json};
use tower::ServiceExt;
use workspace_cloud::{auth::TokenVerifier, kms::AwsKms, onboarding::routes_with_limit, signin::AwsSignIn};

const PRIVATE: &[u8] = include_bytes!("fixtures/test_rsa_private.pem");
const OTHER_PRIVATE: &[u8] = include_bytes!("fixtures/other_rsa_private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test_rsa_public.pem");
const ISSUER: &str = "https://issuer.example/pool";
const AUDIENCE: &str = "desktop-client";
const POOL: &str = "us-east-2_TestPool";

struct H {
    env: Env,
    app: Router,
    pool: SignIn,
    keys: Kms,
}

async fn harness(per_minute: usize) -> H {
    let env = Env::new().await;
    let pool: SignIn = Arc::new(Mutex::new(SignInWorld { client: rich_client(POOL, "appclient123"), ..Default::default() }));
    let cognito_endpoint = cognito_and_ssm(pool.clone()).await;
    let keys: Kms = Arc::new(Mutex::new(KmsWorld::default()));
    let kms_endpoint = kms(keys.clone()).await;
    let credentials = || aws_sdk_cognitoidentityprovider::config::Credentials::new("AKIDTEST", "secret", None, None, "test");
    let cognito = aws_sdk_cognitoidentityprovider::Client::from_conf(
        aws_sdk_cognitoidentityprovider::Config::builder()
            .behavior_version(aws_sdk_cognitoidentityprovider::config::BehaviorVersion::latest())
            .region(aws_sdk_cognitoidentityprovider::config::Region::new("us-east-2"))
            .credentials_provider(credentials())
            .endpoint_url(cognito_endpoint.clone())
            .build(),
    );
    let ssm = aws_sdk_ssm::Client::from_conf(
        aws_sdk_ssm::Config::builder()
            .behavior_version(aws_sdk_ssm::config::BehaviorVersion::latest())
            .region(aws_sdk_ssm::config::Region::new("us-east-2"))
            .credentials_provider(aws_sdk_ssm::config::Credentials::new("AKIDTEST", "secret", None, None, "test"))
            .endpoint_url(cognito_endpoint)
            .build(),
    );
    let kms_client = aws_sdk_kms::Client::from_conf(
        aws_sdk_kms::Config::builder()
            .behavior_version(aws_sdk_kms::config::BehaviorVersion::latest())
            .region(aws_sdk_kms::config::Region::new("us-east-2"))
            .credentials_provider(aws_sdk_kms::config::Credentials::new("AKIDTEST", "secret", None, None, "test"))
            .endpoint_url(kms_endpoint)
            .build(),
    );
    let app = routes_with_limit(
        Arc::new(TokenVerifier::with_pem(PUBLIC, ISSUER, AUDIENCE).unwrap()),
        Arc::new(AwsSignIn::new(cognito, ssm, POOL.into(), "appclient123".into())),
        Arc::new(AwsKms::with_client(kms_client)),
        env.pool("t_prov", 2),
        per_minute,
    );
    H { env, app, pool, keys }
}

fn token(claims: Value, key: &[u8]) -> String {
    let exp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() + 600;
    let mut all = json!({ "sub": "sub-1", "iss": ISSUER, "aud": AUDIENCE, "exp": exp, "token_use": "id" });
    for (name, value) in claims.as_object().unwrap() {
        all[name] = value.clone();
    }
    encode(&Header::new(Algorithm::RS256), &all, &EncodingKey::from_rsa_pem(key).unwrap()).unwrap()
}

/// What Google gives a person who has signed in and has no firm.
fn newcomer(username: &str) -> String {
    token(json!({ "cognito:username": username, "email": "sam@gmail.com", "email_verified": true }), PRIVATE)
}

async fn start(h: &H, token: Option<&str>, name: &str) -> (StatusCode, String) {
    let mut request = Request::post("/v1/onboarding/firm").header(header::CONTENT_TYPE, "application/json");
    if let Some(token) = token {
        request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
    }
    let response = h.app.clone().oneshot(request.body(Body::from(json!({ "name": name }).to_string())).unwrap()).await.unwrap();
    let status = response.status();
    (status, String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap())
}

async fn firms(h: &H) -> Vec<(String, String)> {
    h.env.admin.query("SELECT id::text, name FROM tenants ORDER BY name", &[]).await.unwrap().iter().map(|row| (row.get(0), row.get(1))).collect()
}

fn newcomer_in_pool(h: &H, username: &str) {
    h.pool.lock().unwrap().users.insert(username.into(), String::new());
}

#[tokio::test]
async fn a_new_person_starts_a_firm_with_its_own_key_and_becomes_its_administrator() {
    let h = harness(60).await;
    newcomer_in_pool(&h, "Google_123");
    let (status, body) = start(&h, Some(&newcomer("Google_123")), "  Sam's   Tax Practice ").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let firm_id = serde_json::from_str::<Value>(&body).unwrap()["firm_id"].as_str().unwrap().to_string();

    // The firm exists, with a tidy name and its own key.
    let rows = h.env.admin.query("SELECT name, kms_key_arn FROM tenants WHERE id::text = $1", &[&firm_id]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].get::<_, String>(0), "Sam's Tax Practice");
    assert!(!rows[0].get::<_, String>(1).is_empty());
    assert!(h.keys.lock().unwrap().tags.values().any(|tags| tags.contains(&("tenant".to_string(), firm_id.clone()))), "the key is tagged with the firm");
    // The person now belongs to that firm and administers it.
    let pool = h.pool.lock().unwrap();
    assert_eq!(pool.users["Google_123"], firm_id);
    assert_eq!(pool.groups["Google_123"], ["firm_admin"]);
}

#[tokio::test]
async fn someone_who_already_has_a_firm_cannot_make_another() {
    let h = harness(60).await;
    // The token already carries a firm.
    let member = token(json!({ "cognito:username": "u", "email": "a@b.com", "email_verified": true, "custom:tenant_id": "11111111-1111-4111-8111-111111111111" }), PRIVATE);
    assert_eq!(start(&h, Some(&member), "Another").await.0, StatusCode::CONFLICT);
    // The token is old but the account has since been attached to a firm.
    h.pool.lock().unwrap().users.insert("Google_9".into(), "22222222-2222-4222-8222-222222222222".into());
    assert_eq!(start(&h, Some(&newcomer("Google_9")), "Another").await.0, StatusCode::CONFLICT);
    assert!(firms(&h).await.is_empty());
    assert!(h.keys.lock().unwrap().calls.iter().all(|call| !call.starts_with("CreateKey")), "no key was made");
}

#[tokio::test]
async fn an_unverified_email_a_forged_token_and_no_token_are_all_refused_before_anything_is_made() {
    let h = harness(60).await;
    newcomer_in_pool(&h, "Google_123");
    let unverified = token(json!({ "cognito:username": "Google_123", "email": "sam@gmail.com", "email_verified": false }), PRIVATE);
    let no_email = token(json!({ "cognito:username": "Google_123" }), PRIVATE);
    let forged = token(json!({ "cognito:username": "Google_123", "email": "sam@gmail.com", "email_verified": true }), OTHER_PRIVATE);
    let access = token(json!({ "cognito:username": "Google_123", "email": "sam@gmail.com", "email_verified": true, "token_use": "access" }), PRIVATE);
    for (what, tok) in [("unverified", Some(unverified)), ("no email", Some(no_email)), ("forged", Some(forged)), ("access token", Some(access)), ("garbage", Some("not.a.token".to_string())), ("none", None)] {
        assert_eq!(start(&h, tok.as_deref(), "Sam").await.0, StatusCode::UNAUTHORIZED, "{what}");
    }
    assert!(firms(&h).await.is_empty());
    assert!(h.keys.lock().unwrap().calls.is_empty());
}

#[tokio::test]
async fn a_bad_firm_name_is_refused_before_a_key_is_made() {
    let h = harness(60).await;
    newcomer_in_pool(&h, "Google_123");
    for bad in ["", "    ", &"a".repeat(121), "bad\u{7}name"] {
        assert_eq!(start(&h, Some(&newcomer("Google_123")), bad).await.0, StatusCode::BAD_REQUEST, "{bad:?}");
    }
    assert!(h.keys.lock().unwrap().calls.is_empty());
    assert_eq!(h.pool.lock().unwrap().users["Google_123"], "", "the person is still without a firm");
}

#[tokio::test]
async fn if_the_person_cannot_be_attached_nothing_is_left_behind() {
    let h = harness(60).await;
    newcomer_in_pool(&h, "Google_123");
    h.pool.lock().unwrap().fail.insert("AdminUpdateUserAttributes".into());
    let (status, body) = start(&h, Some(&newcomer("Google_123")), "Sam").await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.contains("AdminUpdateUserAttributes") && !body.contains(POOL), "{body}");
    assert!(firms(&h).await.is_empty(), "the firm row was taken back");
    assert_eq!(h.keys.lock().unwrap().scheduled_deletion.len(), 1, "the key was scheduled for deletion");
    // Trying again once the fault is gone works.
    h.pool.lock().unwrap().fail.clear();
    assert_eq!(start(&h, Some(&newcomer("Google_123")), "Sam").await.0, StatusCode::OK);
}

#[tokio::test]
async fn starting_firms_is_limited_in_total() {
    let h = harness(2).await;
    for n in 0..3 {
        newcomer_in_pool(&h, &format!("Google_{n}"));
    }
    assert_eq!(start(&h, Some(&newcomer("Google_0")), "A").await.0, StatusCode::OK);
    assert_eq!(start(&h, Some(&newcomer("Google_1")), "B").await.0, StatusCode::OK);
    assert_eq!(start(&h, Some(&newcomer("Google_2")), "C").await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(firms(&h).await.len(), 2);
}
