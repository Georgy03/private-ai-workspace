//! A firm's Microsoft sign-in is set up and taken away together, through the real AWS clients against a
//! stand-in for Cognito and the parameter store. The stand-in resets every app setting an update leaves out,
//! as the real service does, so these tests fail if the code loses any of them. No database is needed.

mod fakes;

use std::sync::{Arc, Mutex};

use fakes::cognito::{SignIn, SignInWorld, cognito_and_ssm, rich_client};
use serde_json::{Value, json};
use uuid::Uuid;
use workspace_cloud::{
    Error,
    auth::TenantId,
    signin::{AwsSignIn, MicrosoftApp, PERSONAL_PROVIDER, SignInSetup, add_microsoft, add_personal_microsoft, parameter_name, provider_name, remove_microsoft, remove_personal_microsoft},
};

const POOL: &str = "us-east-2_TestPool";
const APP_CLIENT: &str = "appclient123";
const ENTRA: &str = "123e4567-e89b-12d3-a456-426614174000";

fn app() -> MicrosoftApp {
    MicrosoftApp { client_id: "ms-client".into(), client_secret: "ms-secret".into() }
}

fn firm() -> TenantId {
    TenantId(Uuid::new_v4())
}

async fn harness() -> (AwsSignIn, SignIn) {
    let world: SignIn = Arc::new(Mutex::new(SignInWorld { client: rich_client(POOL, APP_CLIENT), ..Default::default() }));
    let endpoint = cognito_and_ssm(world.clone()).await;
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
            .endpoint_url(endpoint)
            .build(),
    );
    (AwsSignIn::new(cognito, ssm, POOL.into(), APP_CLIENT.into()), world)
}

/// The client's settings without its list of providers, for comparing before and after.
fn without_providers(client: &Value) -> Value {
    let mut copy = client.clone();
    copy.as_object_mut().unwrap().remove("SupportedIdentityProviders");
    copy
}

fn providers(world: &SignIn) -> Vec<String> {
    world.lock().unwrap().client["SupportedIdentityProviders"].as_array().unwrap().iter().map(|p| p.as_str().unwrap().to_string()).collect()
}

#[tokio::test]
async fn a_firm_gets_its_own_provider_recorded_created_and_allowed_and_no_other_app_setting_changes() {
    let (setup, world) = harness().await;
    let (id, before) = (firm(), world.lock().unwrap().client.clone());
    let name = add_microsoft(&setup, id, "Acme Law LLP", ENTRA, &app()).await.unwrap();
    assert_eq!(name, provider_name("Acme Law LLP", id));

    let w = world.lock().unwrap();
    // 1. The firm is recorded where the pre sign-up function looks.
    assert_eq!(w.params[&parameter_name(&name)], id.0.to_string());
    // 2. The provider is tied to the firm's own Microsoft tenant, and maps the email and the subject.
    let made = &w.providers[&name];
    assert_eq!(made["ProviderType"], "OIDC");
    let details = &made["ProviderDetails"];
    assert_eq!(details["oidc_issuer"], format!("https://login.microsoftonline.com/{ENTRA}/v2.0"));
    assert_eq!(details["authorize_url"], format!("https://login.microsoftonline.com/{ENTRA}/oauth2/v2.0/authorize"));
    assert_eq!(details["token_url"], format!("https://login.microsoftonline.com/{ENTRA}/oauth2/v2.0/token"));
    assert_eq!(details["jwks_uri"], format!("https://login.microsoftonline.com/{ENTRA}/discovery/v2.0/keys"));
    assert_eq!((details["client_id"].as_str(), details["client_secret"].as_str()), (Some("ms-client"), Some("ms-secret")));
    assert_eq!(details["authorize_scopes"], "openid email profile");
    assert_eq!(made["AttributeMapping"], json!({ "email": "email", "username": "sub" }));
    // 3. The app allows it, and EVERY other setting is exactly as it was (the stand-in resets any that were not sent).
    assert_eq!(w.client["SupportedIdentityProviders"], json!(["COGNITO", "Google", name]));
    assert_eq!(without_providers(&w.client), without_providers(&before), "an app setting was lost or changed by the update");
    // The secret is only ever sent to Cognito, never written to the parameter store.
    assert!(w.params.values().all(|value| !value.contains("ms-secret")));
}

#[tokio::test]
async fn a_firm_cannot_have_two_microsoft_sign_ins_and_a_bad_tenant_id_changes_nothing() {
    let (setup, world) = harness().await;
    let id = firm();
    add_microsoft(&setup, id, "Acme", ENTRA, &app()).await.unwrap();
    let calls = world.lock().unwrap().calls.len();
    let again = add_microsoft(&setup, id, "Acme", "223e4567-e89b-12d3-a456-426614174000", &app()).await.unwrap_err();
    assert!(matches!(&again, Error::SignIn(reason) if reason.contains("already has Microsoft sign-in")), "{again}");
    assert_eq!(world.lock().unwrap().providers.len(), 1);
    assert_eq!(world.lock().unwrap().calls.len(), calls + 1, "only the lookup of the firm's providers happened");

    let before = world.lock().unwrap().calls.len();
    for bad in ["common", "", "not-a-guid", "00000000-0000-0000-0000-000000000000"] {
        assert!(add_microsoft(&setup, firm(), "Other", bad, &app()).await.is_err(), "{bad:?}");
    }
    assert_eq!(world.lock().unwrap().calls.len(), before, "a refused tenant ID must not reach AWS at all");
}

#[tokio::test]
async fn two_firms_with_one_name_get_separate_providers_and_each_is_removed_alone() {
    let (setup, world) = harness().await;
    let (a, b) = (firm(), firm());
    let original = world.lock().unwrap().client.clone();
    let first = add_microsoft(&setup, a, "Acme", ENTRA, &app()).await.unwrap();
    let second = add_microsoft(&setup, b, "Acme", "223e4567-e89b-12d3-a456-426614174000", &app()).await.unwrap();
    assert_ne!(first, second);
    assert_eq!(providers(&world), ["COGNITO", "Google", first.as_str(), second.as_str()]);

    // Removing the first firm's sign-in leaves the second's alone, and the app goes back to what it was plus the second.
    assert_eq!(remove_microsoft(&setup, a).await.unwrap(), std::slice::from_ref(&first));
    assert_eq!(providers(&world), ["COGNITO", "Google", second.as_str()]);
    assert!(!world.lock().unwrap().providers.contains_key(&first) && world.lock().unwrap().providers.contains_key(&second));
    assert!(!world.lock().unwrap().params.contains_key(&parameter_name(&first)) && world.lock().unwrap().params.contains_key(&parameter_name(&second)));
    assert_eq!(remove_microsoft(&setup, b).await.unwrap(), [second]);
    assert_eq!(world.lock().unwrap().client, original, "after both are removed the app is exactly as it began");
    // Removing from a firm that has none is a quiet no-op.
    assert!(remove_microsoft(&setup, firm()).await.unwrap().is_empty());
}

#[tokio::test]
async fn removing_stops_sign_ins_first_by_deleting_the_firms_record_before_anything_else() {
    let (setup, world) = harness().await;
    let id = firm();
    add_microsoft(&setup, id, "Acme", ENTRA, &app()).await.unwrap();
    world.lock().unwrap().calls.clear();
    remove_microsoft(&setup, id).await.unwrap();
    let calls = world.lock().unwrap().calls.clone();
    let position = |what: &str| calls.iter().position(|call| call.starts_with(what)).unwrap_or_else(|| panic!("{what} was never called: {calls:?}"));
    assert!(position("DeleteParameter") < position("UpdateUserPoolClient"), "{calls:?}");
    assert!(position("UpdateUserPoolClient") < position("DeleteIdentityProvider"), "{calls:?}");
}

#[tokio::test]
async fn a_failed_step_undoes_the_earlier_ones_so_nothing_half_made_is_left() {
    for failing in ["CreateIdentityProvider", "UpdateUserPoolClient"] {
        let (setup, world) = harness().await;
        let before = world.lock().unwrap().client.clone();
        world.lock().unwrap().fail.insert(failing.to_string());
        let error = add_microsoft(&setup, firm(), "Acme", ENTRA, &app()).await.unwrap_err();
        assert!(matches!(error, Error::SignIn(_)), "{failing}: {error}");
        let w = world.lock().unwrap();
        assert!(w.providers.is_empty(), "{failing}: a provider was left behind");
        assert!(w.params.is_empty(), "{failing}: the firm's record was left behind");
        assert_eq!(w.client, before, "{failing}: the app's settings changed");
    }
}

#[tokio::test]
async fn a_record_that_already_exists_is_never_overwritten_and_nothing_is_created_over_it() {
    let (setup, world) = harness().await;
    let id = firm();
    // Someone else's record under exactly the name this firm would get.
    let name = provider_name("Acme", id);
    world.lock().unwrap().params.insert(parameter_name(&name), Uuid::new_v4().to_string());
    let error = add_microsoft(&setup, id, "Acme", ENTRA, &app()).await.unwrap_err();
    assert!(matches!(error, Error::SignIn(_)), "{error}");
    assert!(world.lock().unwrap().providers.is_empty());
    assert_ne!(world.lock().unwrap().params[&parameter_name(&name)], id.0.to_string(), "the other record must be untouched");
}

#[tokio::test]
async fn finding_a_firms_providers_reads_every_page_of_the_record() {
    let (setup, world) = harness().await;
    let id = firm();
    for n in 0..25 {
        world.lock().unwrap().params.insert(parameter_name(&format!("ms-other-{n:02}")), Uuid::new_v4().to_string());
    }
    world.lock().unwrap().params.insert(parameter_name("ms-zzz-last"), id.0.to_string());
    assert_eq!(setup.providers_of(id).await.unwrap(), ["ms-zzz-last"]);
    assert!(setup.providers_of(firm()).await.unwrap().is_empty());
}

#[tokio::test]
async fn personal_microsoft_is_one_shared_provider_on_the_consumers_address_and_changes_nothing_else() {
    let (setup, world) = harness().await;
    let before = world.lock().unwrap().client.clone();
    add_personal_microsoft(&setup, &app()).await.unwrap();
    {
        let w = world.lock().unwrap();
        let details = &w.providers[PERSONAL_PROVIDER]["ProviderDetails"];
        // Signing in goes through "consumers"; the token's issuer is Microsoft's fixed personal tenant.
        assert_eq!(details["authorize_url"], "https://login.microsoftonline.com/consumers/oauth2/v2.0/authorize");
        assert_eq!(details["token_url"], "https://login.microsoftonline.com/consumers/oauth2/v2.0/token");
        assert_eq!(details["jwks_uri"], "https://login.microsoftonline.com/consumers/discovery/v2.0/keys");
        assert_eq!(details["oidc_issuer"], "https://login.microsoftonline.com/9188040d-6c67-4c5b-b112-36a304b66dad/v2.0");
        assert_eq!(w.providers[PERSONAL_PROVIDER]["AttributeMapping"], json!({ "email": "email", "username": "sub" }));
        assert_eq!(w.client["SupportedIdentityProviders"], json!(["COGNITO", "Google", PERSONAL_PROVIDER]));
        assert_eq!(without_providers(&w.client), without_providers(&before));
        assert!(w.params.is_empty(), "it belongs to no firm, so nothing is recorded for one");
    }
    // Turning it on twice is refused by AWS and changes nothing; turning it off puts the app back.
    assert!(add_personal_microsoft(&setup, &app()).await.is_err());
    remove_personal_microsoft(&setup).await.unwrap();
    assert_eq!(world.lock().unwrap().client, before);
    assert!(world.lock().unwrap().providers.is_empty());
}

#[tokio::test]
async fn a_personal_provider_that_cannot_be_allowed_on_the_app_is_not_left_behind() {
    let (setup, world) = harness().await;
    world.lock().unwrap().fail.insert("UpdateUserPoolClient".into());
    assert!(add_personal_microsoft(&setup, &app()).await.is_err());
    assert!(world.lock().unwrap().providers.is_empty());
}
