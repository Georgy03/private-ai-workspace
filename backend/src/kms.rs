//! The key service behind the connector token store. Each firm has its own AWS KMS key. A token is
//! never encrypted by KMS directly: KMS hands out a fresh one-time data key, the token is encrypted
//! with that, and only the data key's encrypted form is stored. Reading a token needs KMS to open
//! that data key again, which it does only for a caller allowed to use that firm's key. So a copy of
//! the database alone holds nothing readable.
//!
//! Every request to KMS carries an "encryption context" naming the firm and the secret. KMS refuses
//! to open a data key under a different context, so an encrypted row copied to another firm or
//! another name cannot be opened, even by a caller who may use both firms' keys.

use std::collections::HashMap;

use async_trait::async_trait;
use aws_sdk_kms::{
    primitives::Blob,
    types::{DataKeySpec, KeySpec, KeyUsageType, Tag},
};
use zeroize::Zeroizing;

use crate::{Error, Result, auth::TenantId};

/// Every firm key carries this tag, so a policy can allow "our keys" and nothing else (infra/iam.tf).
pub const APP_TAG: &str = "prism-workspace";

/// What a data key is bound to: one firm's one secret.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub tenant: TenantId,
    pub name: String,
}

impl Binding {
    /// The context KMS records with, and checks against, the data key.
    pub fn context(&self) -> HashMap<String, String> {
        HashMap::from([
            ("tenant_id".to_string(), self.tenant.0.to_string()),
            ("secret".to_string(), self.name.clone()),
        ])
    }

    /// The same facts as authenticated data for the cipher, so the stored token is also tied to them.
    pub fn associated_data(&self) -> Vec<u8> {
        format!("{}|{}", self.tenant.0, self.name).into_bytes()
    }
}

/// A fresh data key: the plaintext, used once and then wiped, and its encrypted form to store.
pub struct DataKey {
    pub plaintext: Zeroizing<Vec<u8>>,
    pub wrapped: Vec<u8>,
}

#[async_trait]
pub trait DataKeys: Send + Sync {
    /// A new 256-bit data key under the firm's key.
    async fn generate(&self, key_id: &str, binding: &Binding) -> Result<DataKey>;
    /// Opens a stored data key. Fails unless `binding` is the one it was made for.
    async fn decrypt(&self, key_id: &str, wrapped: &[u8], binding: &Binding) -> Result<Zeroizing<Vec<u8>>>;
    /// Makes the key for a new firm and returns its ARN. Rotation is on, so KMS replaces the key
    /// material every year without the ARN or any stored data changing.
    async fn create_firm_key(&self, tenant: TenantId, firm_name: &str) -> Result<String>;
    /// Schedules a key for deletion (the shortest wait KMS allows is seven days). For undoing a key
    /// made for a firm that then could not be created.
    async fn discard_key(&self, key_id: &str) -> Result<()>;
}

/// AWS KMS, reached with whatever credentials the process has: a task role in AWS, or keys in the
/// environment elsewhere.
pub struct AwsKms {
    client: aws_sdk_kms::Client,
}

fn service_error(action: &str, error: impl std::fmt::Display) -> Error {
    // The service's own wording says why (no permission, key disabled, wrong context) and holds
    // nothing secret, so it is kept for the log.
    Error::Crypto(format!("{action}: {error}"))
}

impl AwsKms {
    pub async fn from_env() -> Self {
        let config = aws_config::defaults(aws_config::BehaviorVersion::latest()).load().await;
        Self { client: aws_sdk_kms::Client::new(&config) }
    }

    pub fn with_client(client: aws_sdk_kms::Client) -> Self {
        Self { client }
    }
}

#[async_trait]
impl DataKeys for AwsKms {
    async fn generate(&self, key_id: &str, binding: &Binding) -> Result<DataKey> {
        let reply = self
            .client
            .generate_data_key()
            .key_id(key_id)
            .key_spec(DataKeySpec::Aes256)
            .set_encryption_context(Some(binding.context()))
            .send()
            .await
            .map_err(|error| service_error("could not make a data key", aws_sdk_kms::error::DisplayErrorContext(&error)))?;
        let plaintext = reply.plaintext.ok_or_else(|| Error::Crypto("KMS returned no data key".into()))?;
        let wrapped = reply.ciphertext_blob.ok_or_else(|| Error::Crypto("KMS returned no encrypted data key".into()))?;
        Ok(DataKey { plaintext: Zeroizing::new(plaintext.into_inner()), wrapped: wrapped.into_inner() })
    }

    async fn decrypt(&self, key_id: &str, wrapped: &[u8], binding: &Binding) -> Result<Zeroizing<Vec<u8>>> {
        let reply = self
            .client
            .decrypt()
            .key_id(key_id)
            .ciphertext_blob(Blob::new(wrapped.to_vec()))
            .set_encryption_context(Some(binding.context()))
            .send()
            .await
            .map_err(|error| service_error("could not open a data key", aws_sdk_kms::error::DisplayErrorContext(&error)))?;
        let plaintext = reply.plaintext.ok_or_else(|| Error::Crypto("KMS returned no data key".into()))?;
        Ok(Zeroizing::new(plaintext.into_inner()))
    }

    async fn create_firm_key(&self, tenant: TenantId, firm_name: &str) -> Result<String> {
        let tag = |key: &str, value: String| Tag::builder().tag_key(key).tag_value(value).build().map_err(|error| Error::Crypto(error.to_string()));
        let created = self
            .client
            .create_key()
            .description(format!("Connector tokens for firm {tenant}", tenant = tenant.0))
            .key_usage(KeyUsageType::EncryptDecrypt)
            .key_spec(KeySpec::SymmetricDefault)
            .tags(tag("app", APP_TAG.to_string())?)
            .tags(tag("tenant", tenant.0.to_string())?)
            .tags(tag("firm", firm_name.chars().take(120).collect())?)
            .send()
            .await
            .map_err(|error| service_error("could not create the firm's key", aws_sdk_kms::error::DisplayErrorContext(&error)))?;
        let metadata = created.key_metadata.ok_or_else(|| Error::Crypto("KMS returned no key".into()))?;
        let arn = metadata.arn.clone().ok_or_else(|| Error::Crypto("KMS returned no key ARN".into()))?;
        let key_id = metadata.key_id;
        // From here a failure leaves a key nobody uses, so it is scheduled for deletion again.
        let finish = async {
            self.client
                .enable_key_rotation()
                .key_id(&key_id)
                .send()
                .await
                .map_err(|error| service_error("could not turn on key rotation", aws_sdk_kms::error::DisplayErrorContext(&error)))?;
            self.client
                .create_alias()
                .alias_name(format!("alias/prism-tenant-{}", tenant.0))
                .target_key_id(&key_id)
                .send()
                .await
                .map_err(|error| service_error("could not name the firm's key", aws_sdk_kms::error::DisplayErrorContext(&error)))?;
            Ok::<(), Error>(())
        };
        if let Err(error) = finish.await {
            let _ = self.discard_key(&arn).await;
            return Err(error);
        }
        Ok(arn)
    }

    async fn discard_key(&self, key_id: &str) -> Result<()> {
        self.client
            .schedule_key_deletion()
            .key_id(key_id)
            .pending_window_in_days(7)
            .send()
            .await
            .map_err(|error| service_error("could not schedule the key's deletion", aws_sdk_kms::error::DisplayErrorContext(&error)))?;
        Ok(())
    }
}

/// Creates a firm and its key together. The key is made first, because it is named after the firm; if
/// the firm then cannot be created, the key is scheduled for deletion so nothing is left behind.
pub async fn provision_firm(client: &tokio_postgres::Client, keys: &dyn DataKeys, name: &str) -> Result<(TenantId, String)> {
    let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 200 {
        return Err(Error::Config("usage: workspace-cloud provision-firm \"Firm name\" (1 to 200 characters)".into()));
    }
    let tenant = TenantId(uuid::Uuid::new_v4());
    let key = keys.create_firm_key(tenant, &name).await?;
    let made = client
        .execute("INSERT INTO tenants (id, name, kms_key_arn) VALUES ($1, $2, $3)", &[&tenant.0, &name, &key])
        .await;
    if let Err(error) = made {
        let _ = keys.discard_key(&key).await;
        return Err(error.into());
    }
    Ok((tenant, key))
}
