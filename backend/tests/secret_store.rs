//! The production token store: connector tokens encrypted in the database under each firm's own key.
//! Real Postgres with the real migrations and the real AWS client, against a stand-in for KMS that
//! enforces what KMS enforces. Run with scripts/test.sh.

mod common;
mod fakes;

use std::sync::{Arc, Mutex};

use common::Env;
use fakes::kms::{Kms, KmsWorld, key_id, kms};
use tokio_postgres::error::SqlState;
use uuid::Uuid;
use workspace_cloud::{
    Error,
    auth::TenantId,
    db::{self, TenantTx},
    kms::{AwsKms, provision_firm},
    secrets::{PostgresSecrets, SecretStore, refresh_token_name},
};

struct H {
    env: Env,
    world: Kms,
    keys: Arc<AwsKms>,
    /// What the API process would hold, and what the worker process would hold.
    api: PostgresSecrets,
    worker: PostgresSecrets,
}

async fn harness() -> H {
    let env = Env::new().await;
    let world: Kms = Arc::new(Mutex::new(KmsWorld::default()));
    let endpoint = kms(world.clone()).await;
    let config = aws_sdk_kms::Config::builder()
        .behavior_version(aws_sdk_kms::config::BehaviorVersion::latest())
        .region(aws_sdk_kms::config::Region::new("us-east-2"))
        .credentials_provider(aws_sdk_kms::config::Credentials::new("AKIDTEST", "secret", None, None, "test"))
        .endpoint_url(endpoint)
        .build();
    let keys = Arc::new(AwsKms::with_client(aws_sdk_kms::Client::from_conf(config)));
    let api = PostgresSecrets::new(env.pool("t_api", 4), keys.clone());
    let worker = PostgresSecrets::new(env.pool("t_worker", 4), keys.clone());
    H { env, world, keys, api, worker }
}

/// A firm made the way production makes one: with its own key.
async fn firm(h: &H, name: &str) -> (TenantId, String) {
    provision_firm(&h.env.provisioner().await, &*h.keys, name).await.unwrap()
}

const TOKEN: &str = "refresh-token-ALPHA-1234567890";

#[tokio::test]
async fn a_token_is_stored_encrypted_under_the_firms_own_key_and_comes_back_exactly() {
    let h = harness().await;
    let ((a, a_key), (_, b_key)) = (firm(&h, "Firm A").await, firm(&h, "Firm B").await);
    assert_ne!(a_key, b_key, "each firm has its own key");
    let name = refresh_token_name(a, Uuid::new_v4());

    h.api.put(a, &name, TOKEN).await.unwrap();
    // The worker is a separate process with its own database login, and sees what the API stored.
    assert_eq!(h.worker.get(a, &name).await.unwrap().as_deref(), Some(TOKEN));

    // What is in the database is not the token, and says which key protects it.
    let row = h.env.admin.query_one("SELECT key_id, wrapped_key, nonce, ciphertext FROM connector_secrets", &[]).await.unwrap();
    let (stored_key, wrapped, nonce, ciphertext): (String, Vec<u8>, Vec<u8>, Vec<u8>) = (row.get(0), row.get(1), row.get(2), row.get(3));
    assert_eq!(stored_key, a_key);
    assert_eq!(nonce.len(), 12);
    for held in [&wrapped, &ciphertext] {
        assert!(!held.windows(TOKEN.len()).any(|window| window == TOKEN.as_bytes()), "the token is readable in the database");
    }
    // KMS was asked under A's key, with a context naming the firm and the secret.
    {
        let world = h.world.lock().unwrap();
        let (used, context) = world.generated.last().unwrap();
        assert_eq!(used, &key_id(&a_key));
        assert_eq!(context["tenant_id"], a.0.to_string());
        assert_eq!(context["secret"], name);
        let used_b = world.calls.iter().filter(|call| call.starts_with("GenerateDataKey") || call.starts_with("Decrypt")).any(|call| call.contains(&key_id(&b_key)));
        assert!(!used_b, "B's key was never used to encrypt or open A's secret");
    }

    // Replacing a token replaces everything stored for it, with a fresh data key.
    h.worker.put(a, &name, "second-token-VALUE").await.unwrap();
    assert_eq!(h.api.get(a, &name).await.unwrap().as_deref(), Some("second-token-VALUE"));
    let rows = h.env.admin.query("SELECT wrapped_key FROM connector_secrets", &[]).await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_ne!(rows[0].get::<_, Vec<u8>>(0), wrapped);

    // Nothing stored reads as none, and deleting is repeatable.
    assert_eq!(h.api.get(a, &refresh_token_name(a, Uuid::new_v4())).await.unwrap(), None);
    h.api.delete(a, &name).await.unwrap();
    assert_eq!(h.worker.get(a, &name).await.unwrap(), None);
    h.api.delete(a, &name).await.unwrap();
}

#[tokio::test]
async fn a_stored_token_cannot_be_opened_for_another_name_or_firm_and_tampering_is_caught() {
    let h = harness().await;
    let ((a, _), (b, b_key)) = (firm(&h, "Firm A").await, firm(&h, "Firm B").await);
    let (first, second) = (refresh_token_name(a, Uuid::new_v4()), refresh_token_name(a, Uuid::new_v4()));
    h.api.put(a, &first, TOKEN).await.unwrap();
    h.api.put(a, &second, "the-other-connection").await.unwrap();

    // Another firm cannot even name it.
    assert!(matches!(h.api.get(b, &first).await, Err(Error::TenantMismatch)));
    assert!(matches!(h.api.put(b, &first, "x").await, Err(Error::TenantMismatch)));
    assert!(matches!(h.api.delete(b, &first).await, Err(Error::TenantMismatch)));

    // A row copied onto another of the same firm's names will not open: KMS and the cipher both
    // check the name the data key was made for.
    h.env
        .admin
        .execute(
            "UPDATE connector_secrets SET wrapped_key = s.wrapped_key, nonce = s.nonce, ciphertext = s.ciphertext
             FROM connector_secrets s WHERE s.name = $1 AND connector_secrets.name = $2",
            &[&first, &second],
        )
        .await
        .unwrap();
    let error = h.api.get(a, &second).await.unwrap_err().to_string();
    assert!(!error.contains(TOKEN) && !error.contains("the-other-connection"), "{error}");
    assert!(matches!(h.api.get(a, &second).await, Err(Error::Crypto(_))));

    // A row copied into another firm, under that firm's own name and key, will not open either.
    let stolen = refresh_token_name(b, Uuid::new_v4());
    h.env
        .admin
        .execute(
            "INSERT INTO connector_secrets (tenant_id, name, key_id, wrapped_key, nonce, ciphertext)
             SELECT $1, $2, $3, wrapped_key, nonce, ciphertext FROM connector_secrets WHERE name = $4",
            &[&b.0, &stolen, &b_key, &first],
        )
        .await
        .unwrap();
    assert!(matches!(h.api.get(b, &stolen).await, Err(Error::Crypto(_))));

    // A row that claims another firm's key is refused before KMS is asked anything.
    h.env.admin.execute("UPDATE connector_secrets SET key_id = 'someone-elses-key' WHERE name = $1", &[&first]).await.unwrap();
    let asked = h.world.lock().unwrap().calls.len();
    assert!(matches!(h.api.get(a, &first).await, Err(Error::Crypto(_))));
    assert_eq!(h.world.lock().unwrap().calls.len(), asked, "KMS must not be asked for a row under the wrong key");

    // A changed byte is caught.
    h.api.put(a, &first, TOKEN).await.unwrap();
    h.env.admin.execute("UPDATE connector_secrets SET ciphertext = set_byte(ciphertext, 0, get_byte(ciphertext, 0) # 1) WHERE name = $1", &[&first]).await.unwrap();
    assert!(matches!(h.api.get(a, &first).await, Err(Error::Crypto(_))));
    // And a restored row opens again: nothing was left damaged.
    h.api.put(a, &first, TOKEN).await.unwrap();
    assert_eq!(h.api.get(a, &first).await.unwrap().as_deref(), Some(TOKEN));
}

#[tokio::test]
async fn the_database_itself_keeps_firms_apart_even_if_the_code_were_wrong() {
    let h = harness().await;
    let ((a, _), (b, _)) = (firm(&h, "Firm A").await, firm(&h, "Firm B").await);
    let name = refresh_token_name(a, Uuid::new_v4());
    h.api.put(a, &name, TOKEN).await.unwrap();
    let (blob, nonce) = (vec![0u8; 8], vec![0u8; 12]);
    let insert = "INSERT INTO connector_secrets (tenant_id, name, key_id, wrapped_key, nonce, ciphertext) VALUES ($1, $2, 'k', $3, $4, $3)";

    // Working as firm B, the API login sees none of A's rows and cannot write into A.
    for login in ["t_api", "t_worker"] {
        let mut conn = h.env.as_user(login).await;
        let tx = TenantTx::begin(&mut conn, b).await.unwrap();
        let seen: i64 = tx.sql().query_one("SELECT count(*) FROM connector_secrets", &[]).await.unwrap().get(0);
        assert_eq!(seen, 0, "{login} saw another firm's secret");
        let changed = tx.sql().execute("UPDATE connector_secrets SET ciphertext = '\\x00'", &[]).await.unwrap();
        assert_eq!(changed, 0);
        let removed = tx.sql().execute("DELETE FROM connector_secrets", &[]).await.unwrap();
        assert_eq!(removed, 0);
    }
    let mut conn = h.env.api().await;
    let tx = TenantTx::begin(&mut conn, b).await.unwrap();
    let error = tx.sql().execute(insert, &[&a.0, &refresh_token_name(a, Uuid::new_v4()), &blob, &nonce]).await.unwrap_err();
    assert_eq!(error.code(), Some(&SqlState::INSUFFICIENT_PRIVILEGE), "{error}");

    // A secret cannot be filed under another firm's name, or with a path that climbs out.
    for bad in [refresh_token_name(b, Uuid::new_v4()), format!("tenants/{}/../{}/x", a.0, b.0), "somewhere/else".to_string()] {
        let mut conn = h.env.api().await;
        let tx = TenantTx::begin(&mut conn, a).await.unwrap();
        let error = tx.sql().execute(insert, &[&a.0, &bad, &blob, &nonce]).await.unwrap_err();
        assert_eq!(error.code(), Some(&SqlState::CHECK_VIOLATION), "{bad}: {error}");
    }
    // Every secret still opens for its own firm, and the other firm is unaffected.
    assert_eq!(h.api.get(a, &name).await.unwrap().as_deref(), Some(TOKEN));
    // The people who run the API cannot read the rows without a firm chosen at all.
    let bare = h.env.api().await;
    let none: i64 = bare.query_one("SELECT count(*) FROM connector_secrets", &[]).await.map(|r| r.get(0)).unwrap_or(0);
    assert_eq!(none, 0);
    // Deleting a firm deletes its secrets with it.
    db::offboard_tenant(&h.env.provisioner().await, a).await.unwrap();
    let left: i64 = h.env.admin.query_one("SELECT count(*) FROM connector_secrets", &[]).await.unwrap().get(0);
    assert_eq!(left, 0);
}

#[tokio::test]
async fn a_firm_without_a_key_cannot_store_anything_and_nothing_falls_back_to_a_shared_key() {
    let h = harness().await;
    let keyless = db::provision_tenant(&h.env.provisioner().await, "No key yet", None).await.unwrap();
    let name = refresh_token_name(keyless, Uuid::new_v4());
    let error = h.api.put(keyless, &name, TOKEN).await.unwrap_err();
    assert!(matches!(&error, Error::Crypto(reason) if reason.contains("no encryption key")), "{error}");
    let stored: i64 = h.env.admin.query_one("SELECT count(*) FROM connector_secrets", &[]).await.unwrap().get(0);
    assert_eq!(stored, 0);
    assert!(h.world.lock().unwrap().calls.is_empty(), "KMS must not be asked when the firm has no key");
}

#[tokio::test]
async fn trouble_with_the_key_service_is_an_error_and_never_a_half_stored_secret() {
    let h = harness().await;
    let (a, a_key) = firm(&h, "Firm A").await;
    let name = refresh_token_name(a, Uuid::new_v4());
    h.api.put(a, &name, TOKEN).await.unwrap();

    // The process loses the right to use the firm's key: reads and writes both fail, plainly.
    h.world.lock().unwrap().denied.insert(key_id(&a_key));
    let read = h.api.get(a, &name).await.unwrap_err().to_string();
    assert!(read.contains("could not open a data key") && !read.contains(TOKEN), "{read}");
    let write = h.api.put(a, &name, "new-value").await.unwrap_err().to_string();
    assert!(write.contains("could not make a data key"), "{write}");

    // The failed write changed nothing: the old token is still there once access returns.
    h.world.lock().unwrap().denied.clear();
    assert_eq!(h.api.get(a, &name).await.unwrap().as_deref(), Some(TOKEN));

    // A firm whose row names a key KMS does not know gets an error, not a guess.
    h.env.admin.execute("UPDATE tenants SET kms_key_arn = 'arn:aws:kms:us-east-2:111122223333:key/missing' WHERE id = $1", &[&a.0]).await.unwrap();
    assert!(matches!(h.api.put(a, &name, "x").await, Err(Error::Crypto(_))));
}

#[tokio::test]
async fn a_new_firm_gets_its_own_rotating_key_and_a_failed_creation_leaves_no_key_behind() {
    let h = harness().await;
    let (tenant, key) = firm(&h, "Firm A").await;
    {
        let world = h.world.lock().unwrap();
        let id = key_id(&key);
        assert!(world.rotation.contains(&id), "rotation is on");
        assert_eq!(world.aliases.get(&format!("alias/prism-tenant-{}", tenant.0)), Some(&id));
        assert!(world.tags[&id].contains(&("tenant".to_string(), tenant.0.to_string())), "keys are tagged tenant=<firm id>, as infra/iam.tf expects");
        assert!(world.tags[&id].contains(&("app".to_string(), workspace_cloud::kms::APP_TAG.to_string())));
    }
    let stored: String = h.env.admin.query_one("SELECT kms_key_arn FROM tenants WHERE id = $1", &[&tenant.0]).await.unwrap().get(0);
    assert_eq!(stored, key);

    // If the key cannot be named, it is scheduled for deletion and no firm is made.
    h.world.lock().unwrap().fail_create_alias = true;
    let before: i64 = h.env.admin.query_one("SELECT count(*) FROM tenants", &[]).await.unwrap().get(0);
    assert!(provision_firm(&h.env.provisioner().await, &*h.keys, "Firm B").await.is_err());
    h.world.lock().unwrap().fail_create_alias = false;
    assert_eq!(h.world.lock().unwrap().scheduled_deletion.len(), 1);
    // If the firm cannot be saved (a login that may not create firms), the key is scheduled for deletion too.
    assert!(provision_firm(&h.env.api().await, &*h.keys, "Firm C").await.is_err());
    assert_eq!(h.world.lock().unwrap().scheduled_deletion.len(), 2);
    let after: i64 = h.env.admin.query_one("SELECT count(*) FROM tenants", &[]).await.unwrap().get(0);
    assert_eq!(before, after);
    // And a name that is not a name is refused before any key is made.
    let keys_before = h.world.lock().unwrap().keys.len();
    assert!(provision_firm(&h.env.provisioner().await, &*h.keys, "   ").await.is_err());
    assert_eq!(h.world.lock().unwrap().keys.len(), keys_before);
}
