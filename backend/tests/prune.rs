//! Clearing out sign-ups that never made a firm, through the real AWS client against a stand-in for the user pool.
//! No database is needed.

mod fakes;

use std::sync::{Arc, Mutex};

use fakes::cognito::{SignIn, SignInWorld, cognito_and_ssm, rich_client};
use workspace_cloud::{prune::prune, signin::AwsSignIn};

const POOL: &str = "us-east-2_TestPool";
/// The stand-in's idea of "now" for new accounts.
const NOW: i64 = 1_900_000_000;
const DAY: i64 = 86_400;
const FIRM: &str = "11111111-1111-4111-8111-111111111111";

async fn harness() -> (AwsSignIn, SignIn) {
    let world: SignIn = Arc::new(Mutex::new(SignInWorld { client: rich_client(POOL, "appclient123"), ..Default::default() }));
    let endpoint = cognito_and_ssm(world.clone()).await;
    let cognito = aws_sdk_cognitoidentityprovider::Client::from_conf(
        aws_sdk_cognitoidentityprovider::Config::builder()
            .behavior_version(aws_sdk_cognitoidentityprovider::config::BehaviorVersion::latest())
            .region(aws_sdk_cognitoidentityprovider::config::Region::new("us-east-2"))
            .credentials_provider(aws_sdk_cognitoidentityprovider::config::Credentials::new("AKIDTEST", "secret", None, None, "test"))
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
    (AwsSignIn::new(cognito, ssm, POOL.into(), "appclient123".into()), world)
}

/// A pool with one of each kind of account.
fn populate(world: &SignIn) {
    let mut w = world.lock().unwrap();
    // Never candidates: someone in a firm, an invited person who has not signed in yet, a native account.
    w.users.insert("member_old".into(), FIRM.into());
    w.meta.insert("member_old".into(), ("EXTERNAL_PROVIDER".into(), NOW - 400 * DAY));
    w.users.insert("invited_old".into(), FIRM.into());
    w.meta.insert("invited_old".into(), ("FORCE_CHANGE_PASSWORD".into(), NOW - 400 * DAY));
    w.users.insert("native_nofirm_old".into(), String::new());
    w.meta.insert("native_nofirm_old".into(), ("CONFIRMED".into(), NOW - 400 * DAY));
    // Firm-less outside sign-ups: two old, one recent.
    for (name, age) in [("abandoned_a", 90), ("abandoned_b", 31), ("recent", 2)] {
        w.users.insert(name.into(), String::new());
        w.meta.insert(name.into(), ("EXTERNAL_PROVIDER".into(), NOW - age * DAY));
    }
}

fn names(world: &SignIn) -> Vec<String> {
    world.lock().unwrap().users.keys().cloned().collect()
}

#[tokio::test]
async fn by_default_it_only_lists_and_deletes_nothing() {
    let (pool, world) = harness().await;
    populate(&world);
    let before = names(&world);
    let report = prune(&pool, 30, NOW, false).await.unwrap();
    let mut found: Vec<_> = report.candidates.iter().map(|n| n.username.as_str()).collect();
    found.sort();
    assert_eq!(found, ["abandoned_a", "abandoned_b"]);
    assert_eq!((report.too_new, report.deleted.len(), report.spared.len()), (1, 0, 0));
    assert_eq!(names(&world), before, "a dry run must change nothing");
    assert!(world.lock().unwrap().calls.iter().all(|call| !call.starts_with("AdminDeleteUser")));
}

#[tokio::test]
async fn deleting_removes_only_old_firmless_outside_sign_ups() {
    let (pool, world) = harness().await;
    populate(&world);
    let report = prune(&pool, 30, NOW, true).await.unwrap();
    assert_eq!(report.deleted.len(), 2);
    assert_eq!(names(&world), ["invited_old", "member_old", "native_nofirm_old", "recent"]);
    // Run again: nothing left to do, nothing breaks.
    assert!(prune(&pool, 30, NOW, true).await.unwrap().candidates.is_empty());
}

#[tokio::test]
async fn a_person_who_gets_a_firm_between_the_listing_and_the_deletion_is_spared() {
    let (pool, world) = harness().await;
    populate(&world);
    world.lock().unwrap().attach_after_list = Some(("abandoned_a".into(), FIRM.into()));
    let report = prune(&pool, 30, NOW, true).await.unwrap();
    assert_eq!(report.spared, ["abandoned_a"]);
    assert_eq!(report.deleted, ["abandoned_b"]);
    assert!(names(&world).contains(&"abandoned_a".to_string()));
}

#[tokio::test]
async fn every_page_of_the_pool_is_read() {
    let (pool, world) = harness().await;
    {
        let mut w = world.lock().unwrap();
        w.page_size = 2;
        for n in 0..7 {
            w.users.insert(format!("old_{n}"), String::new());
            w.meta.insert(format!("old_{n}"), ("EXTERNAL_PROVIDER".into(), NOW - 60 * DAY));
        }
    }
    assert_eq!(prune(&pool, 30, NOW, true).await.unwrap().deleted.len(), 7);
    assert!(names(&world).is_empty());
}

#[tokio::test]
async fn the_wait_cannot_be_shorter_than_a_day_so_nobody_mid_sign_up_is_swept_away() {
    let (pool, world) = harness().await;
    populate(&world);
    assert!(prune(&pool, 0, NOW, true).await.is_err());
    assert_eq!(world.lock().unwrap().calls.len(), 0, "a refused wait must not reach AWS");
    // One day is the shortest, and a two-day-old account is then a candidate.
    assert!(prune(&pool, 1, NOW, false).await.unwrap().candidates.iter().any(|n| n.username == "recent"));
}
