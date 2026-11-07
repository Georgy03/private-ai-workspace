//! The sign-in screen asks the server which Microsoft sign-in an email belongs to, before anyone is signed in.
//! The real AWS clients talk to a stand-in for Cognito and the parameter store. No database is needed.

mod fakes;

use std::sync::{Arc, Mutex};

use axum::{
    body::Body,
    http::{Request, StatusCode, header},
};
use fakes::cognito::{SignIn, SignInWorld, cognito_and_ssm, rich_client};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use tower::ServiceExt;
use uuid::Uuid;
use workspace_cloud::{
    auth::TenantId,
    signin::{AwsSignIn, MicrosoftApp, add_microsoft},
    signin_api::routes_with_limit,
};

const POOL: &str = "us-east-2_TestPool";
const APP_CLIENT: &str = "appclient123";
const ENTRA: &str = "123e4567-e89b-12d3-a456-426614174000";

async fn harness() -> (axum::Router, AwsSignIn, SignIn) {
    harness_with_limit(60).await
}

async fn harness_with_limit(limit: usize) -> (axum::Router, AwsSignIn, SignIn) {
    let world: SignIn = Arc::new(Mutex::new(SignInWorld { client: rich_client(POOL, APP_CLIENT), ..Default::default() }));
    let endpoint = cognito_and_ssm(world.clone()).await;
    let make = || {
        let credentials = aws_sdk_cognitoidentityprovider::config::Credentials::new("AKIDTEST", "secret", None, None, "test");
        let cognito = aws_sdk_cognitoidentityprovider::Client::from_conf(
            aws_sdk_cognitoidentityprovider::Config::builder()
                .behavior_version(aws_sdk_cognitoidentityprovider::config::BehaviorVersion::latest())
                .region(aws_sdk_cognitoidentityprovider::config::Region::new("us-east-2"))
                .credentials_provider(credentials)
                .endpoint_url(endpoint.clone())
                .build(),
        );
        let ssm = aws_sdk_ssm::Client::from_conf(
            aws_sdk_ssm::Config::builder()
                .behavior_version(aws_sdk_ssm::config::BehaviorVersion::latest())
                .region(aws_sdk_ssm::config::Region::new("us-east-2"))
                .credentials_provider(aws_sdk_ssm::config::Credentials::new("AKIDTEST", "secret", None, None, "test"))
                .endpoint_url(endpoint.clone())
                .build(),
        );
        AwsSignIn::new(cognito, ssm, POOL.into(), APP_CLIENT.into())
    };
    (routes_with_limit(Arc::new(make()), limit), make(), world)
}

async fn ask(app: &axum::Router, email: &str) -> (StatusCode, Value, Option<String>) {
    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/sign-in/route")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(json!({ "email": email }).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let cache = response.headers().get(header::CACHE_CONTROL).map(|v| v.to_str().unwrap().to_string());
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap_or(Value::Null), cache)
}

fn app_settings() -> MicrosoftApp {
    MicrosoftApp { client_id: "ms-client".into(), client_secret: "ms-secret".into() }
}

#[tokio::test]
async fn an_invited_person_is_sent_to_their_own_firms_microsoft_sign_in_and_only_theirs() {
    let (app, setup, world) = harness().await;
    let (acme, other) = (TenantId(Uuid::new_v4()), TenantId(Uuid::new_v4()));
    let acme_provider = add_microsoft(&setup, acme, "Acme", ENTRA, &app_settings()).await.unwrap();
    let other_provider = add_microsoft(&setup, other, "Other", "223e4567-e89b-12d3-a456-426614174000", &app_settings()).await.unwrap();
    {
        let mut w = world.lock().unwrap();
        w.users.insert("sam@acme.com".into(), acme.0.to_string());
        w.users.insert("lee@other.com".into(), other.0.to_string());
    }
    let (status, body, cache) = ask(&app, "sam@acme.com").await;
    assert_eq!((status, &body), (StatusCode::OK, &json!({ "provider": acme_provider })));
    assert_eq!(cache.as_deref(), Some("no-store"));
    assert_eq!(ask(&app, "lee@other.com").await.1, json!({ "provider": other_provider }));
    // Capital letters and spaces make no difference.
    assert_eq!(ask(&app, "  Sam@Acme.COM ").await.1, json!({ "provider": acme_provider }));
}

#[tokio::test]
async fn an_uninvited_email_and_an_invited_one_without_microsoft_get_exactly_the_same_answer() {
    let (app, _setup, world) = harness().await;
    world.lock().unwrap().users.insert("sam@nomicrosoft.com".into(), Uuid::new_v4().to_string());
    let invited = ask(&app, "sam@nomicrosoft.com").await;
    let stranger = ask(&app, "nobody@nowhere.com").await;
    assert_eq!(invited.0, StatusCode::OK);
    assert_eq!((&invited.0, &invited.1), (&stranger.0, &stranger.1));
    assert_eq!(stranger.1, json!({ "provider": null }));
}

#[tokio::test]
async fn a_malformed_address_never_reaches_aws() {
    let (app, _setup, world) = harness().await;
    for bad in ["", "sam", "a@b@c.com", "sam@acme", "has space@acme.com", &format!("{}@acme.com", "a".repeat(300))] {
        assert_eq!(ask(&app, bad).await.1, json!({ "provider": null }), "{bad:?}");
    }
    assert!(world.lock().unwrap().calls.is_empty(), "{:?}", world.lock().unwrap().calls);
}

#[tokio::test]
async fn an_aws_failure_is_a_plain_retry_message_that_leaks_nothing() {
    let (app, _setup, world) = harness().await;
    world.lock().unwrap().fail.insert("AdminGetUser".into());
    let response = app
        .clone()
        .oneshot(Request::post("/v1/sign-in/route").header(header::CONTENT_TYPE, "application/json").body(Body::from(r#"{"email":"sam@acme.com"}"#)).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let text = String::from_utf8(response.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(!text.contains("InternalError") && !text.contains("AdminGetUser") && !text.contains(POOL), "{text}");
}

#[tokio::test]
async fn it_is_limited_in_total_because_anyone_can_call_it() {
    let (app, _setup, world) = harness_with_limit(3).await;
    for _ in 0..3 {
        assert_eq!(ask(&app, "x@y.com").await.0, StatusCode::OK);
    }
    assert_eq!(ask(&app, "x@y.com").await.0, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(world.lock().unwrap().calls.len(), 3, "a refused request must not reach AWS");
}

#[tokio::test]
async fn an_oversize_request_is_refused() {
    let (app, _setup, _world) = harness().await;
    let big = json!({ "email": "a".repeat(5000) }).to_string();
    let response = app
        .oneshot(Request::post("/v1/sign-in/route").header(header::CONTENT_TYPE, "application/json").body(Body::from(big)).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
}
