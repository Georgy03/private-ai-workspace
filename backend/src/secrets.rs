//! Where connector refresh tokens live. A refresh token lets us read a firm's files for as long as
//! the firm allows, so it is only ever stored encrypted, and scoped to one firm.
//!
//! The production store is `PostgresSecrets`: tokens encrypted in the database, each under its own
//! firm's KMS key (see kms.rs and migrations/0025_connector_secrets.sql). The in-memory and file
//! stores are for development and tests, and the server refuses them outside development.

use std::{collections::HashMap, path::PathBuf, sync::Mutex};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{Error, Result, auth::TenantId};

/// The only name a firm's connection token may have. Building names in one place keeps one firm
/// from ever naming another firm's secret.
pub fn refresh_token_name(tenant: TenantId, connection: Uuid) -> String {
    format!(
        "tenants/{}/connections/{connection}/refresh_token",
        tenant.0
    )
}

pub fn check_name(tenant: TenantId, name: &str) -> Result<()> {
    let prefix = format!("tenants/{}/", tenant.0);
    if name.starts_with(&prefix) && !name.contains("..") {
        Ok(())
    } else {
        Err(Error::TenantMismatch)
    }
}

#[async_trait]
pub trait SecretStore: Send + Sync {
    async fn put(&self, tenant: TenantId, name: &str, value: &str) -> Result<()>;
    async fn get(&self, tenant: TenantId, name: &str) -> Result<Option<String>>;
    async fn delete(&self, tenant: TenantId, name: &str) -> Result<()>;
}

#[derive(Default)]
pub struct MemorySecrets(Mutex<HashMap<String, String>>);

#[async_trait]
impl SecretStore for MemorySecrets {
    async fn put(&self, tenant: TenantId, name: &str, value: &str) -> Result<()> {
        check_name(tenant, name)?;
        self.0.lock().unwrap().insert(name.into(), value.into());
        Ok(())
    }

    async fn get(&self, tenant: TenantId, name: &str) -> Result<Option<String>> {
        check_name(tenant, name)?;
        Ok(self.0.lock().unwrap().get(name).cloned())
    }

    async fn delete(&self, tenant: TenantId, name: &str) -> Result<()> {
        check_name(tenant, name)?;
        self.0.lock().unwrap().remove(name);
        Ok(())
    }
}

/// Development only: one private file per secret in a directory, so the API and the worker, which
/// are separate processes, can share a connection's token on a developer's machine. The directory
/// is readable only by its owner. Production uses a real secret store instead.
pub struct FileSecrets {
    dir: PathBuf,
}

impl FileSecrets {
    pub fn new(dir: impl Into<PathBuf>) -> Result<Self> {
        let dir = dir.into();
        std::fs::create_dir_all(&dir)
            .map_err(|error| Error::Config(format!("secret directory: {error}")))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|error| Error::Config(format!("secret directory: {error}")))?;
        }
        Ok(Self { dir })
    }

    fn path(&self, name: &str) -> PathBuf {
        use sha2::{Digest, Sha256};
        let digest: String = Sha256::digest(name.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        self.dir.join(format!("{digest}.secret"))
    }
}

#[async_trait]
impl SecretStore for FileSecrets {
    async fn put(&self, tenant: TenantId, name: &str, value: &str) -> Result<()> {
        check_name(tenant, name)?;
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options
            .open(self.path(name))
            .and_then(|mut file| file.write_all(value.as_bytes()))
            .map_err(|error| Error::Config(format!("could not save secret: {error}")))
    }

    async fn get(&self, tenant: TenantId, name: &str) -> Result<Option<String>> {
        check_name(tenant, name)?;
        match std::fs::read_to_string(self.path(name)) {
            Ok(value) => Ok(Some(value)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(Error::Config(format!("could not read secret: {error}"))),
        }
    }

    async fn delete(&self, tenant: TenantId, name: &str) -> Result<()> {
        check_name(tenant, name)?;
        match std::fs::remove_file(self.path(name)) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::Config(format!("could not delete secret: {error}"))),
        }
    }
}

/// Used when no real store is configured: every call fails, so nothing is ever saved by accident.
pub struct UnconfiguredSecrets;

#[async_trait]
impl SecretStore for UnconfiguredSecrets {
    async fn put(&self, _: TenantId, _: &str, _: &str) -> Result<()> {
        Err(Error::Config("no secret store is configured".into()))
    }
    async fn get(&self, _: TenantId, _: &str) -> Result<Option<String>> {
        Err(Error::Config("no secret store is configured".into()))
    }
    async fn delete(&self, _: TenantId, _: &str) -> Result<()> {
        Err(Error::Config("no secret store is configured".into()))
    }
}

/// The production store: tokens encrypted in the database, under each firm's own KMS key. The API and
/// the worker each have one, on their own database login, and see each other's writes.
pub struct PostgresSecrets {
    pool: deadpool_postgres::Pool,
    keys: std::sync::Arc<dyn crate::kms::DataKeys>,
}

use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, AeadCore, Generate, KeyInit, Payload},
};
use zeroize::Zeroizing;

use crate::kms::Binding;

type GcmNonce = Nonce<<Aes256Gcm as AeadCore>::NonceSize>;

/// Encrypts `message` with a data key. The firm and the secret's name are part of what is signed, so
/// the result opens only for that firm and that name.
fn seal(key: &[u8], message: &[u8], binding: &Binding) -> Result<(Vec<u8>, Vec<u8>)> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Crypto("the data key has the wrong length".into()))?;
    let nonce = GcmNonce::generate();
    let aad = binding.associated_data();
    let ciphertext = cipher
        .encrypt(&nonce, Payload { msg: message, aad: &aad })
        .map_err(|_| Error::Crypto("could not encrypt the secret".into()))?;
    Ok((nonce.to_vec(), ciphertext))
}

fn open(key: &[u8], nonce: &[u8], ciphertext: &[u8], binding: &Binding) -> Result<Zeroizing<Vec<u8>>> {
    let cipher = Aes256Gcm::new_from_slice(key).map_err(|_| Error::Crypto("the data key has the wrong length".into()))?;
    let nonce = GcmNonce::try_from(nonce).map_err(|_| Error::Crypto("a stored secret is damaged".into()))?;
    let aad = binding.associated_data();
    cipher
        .decrypt(&nonce, Payload { msg: ciphertext, aad: &aad })
        .map(Zeroizing::new)
        .map_err(|_| Error::Crypto("a stored secret could not be opened: it was changed, or belongs to another firm or name".into()))
}

impl PostgresSecrets {
    pub fn new(pool: deadpool_postgres::Pool, keys: std::sync::Arc<dyn crate::kms::DataKeys>) -> Self {
        Self { pool, keys }
    }

    async fn conn(&self) -> Result<deadpool_postgres::Object> {
        self.pool.get().await.map_err(|error| Error::Pool(error.to_string()))
    }

    /// The firm's own key. A firm without one cannot store tokens: nothing falls back to a shared key.
    async fn firm_key(tx: &crate::db::TenantTx<'_>) -> Result<String> {
        let row = tx.sql().query_opt("SELECT kms_key_arn FROM tenants WHERE id = app_tenant()", &[]).await?;
        match row.and_then(|row| row.get::<_, Option<String>>(0)) {
            Some(key) if !key.is_empty() => Ok(key),
            _ => Err(Error::Crypto("this firm has no encryption key yet".into())),
        }
    }
}

#[async_trait]
impl SecretStore for PostgresSecrets {
    async fn put(&self, tenant: TenantId, name: &str, value: &str) -> Result<()> {
        check_name(tenant, name)?;
        let binding = Binding { tenant, name: name.to_string() };
        // The database is not held open while KMS is asked.
        let key_id = {
            let mut conn = self.conn().await?;
            let tx = crate::db::TenantTx::begin(&mut conn, tenant).await?;
            let key = Self::firm_key(&tx).await?;
            tx.commit().await?;
            key
        };
        let data_key = self.keys.generate(&key_id, &binding).await?;
        let (nonce, ciphertext) = seal(&data_key.plaintext, value.as_bytes(), &binding)?;
        let mut conn = self.conn().await?;
        let tx = crate::db::TenantTx::begin(&mut conn, tenant).await?;
        tx.sql()
            .execute(
                "INSERT INTO connector_secrets (tenant_id, name, key_id, wrapped_key, nonce, ciphertext)
                 VALUES (app_tenant(), $1, $2, $3, $4, $5)
                 ON CONFLICT (tenant_id, name) DO UPDATE SET key_id = excluded.key_id, wrapped_key = excluded.wrapped_key,
                   nonce = excluded.nonce, ciphertext = excluded.ciphertext, updated_at = now()",
                &[&name, &key_id, &data_key.wrapped, &nonce, &ciphertext],
            )
            .await?;
        tx.commit().await
    }

    async fn get(&self, tenant: TenantId, name: &str) -> Result<Option<String>> {
        check_name(tenant, name)?;
        let binding = Binding { tenant, name: name.to_string() };
        let (key_id, current, wrapped, nonce, ciphertext) = {
            let mut conn = self.conn().await?;
            let tx = crate::db::TenantTx::begin(&mut conn, tenant).await?;
            let row = tx
                .sql()
                .query_opt("SELECT key_id, wrapped_key, nonce, ciphertext FROM connector_secrets WHERE tenant_id = app_tenant() AND name = $1", &[&name])
                .await?;
            let Some(row) = row else { return Ok(None) };
            let current = Self::firm_key(&tx).await?;
            let fields = (row.get::<_, String>(0), current, row.get::<_, Vec<u8>>(1), row.get::<_, Vec<u8>>(2), row.get::<_, Vec<u8>>(3));
            tx.commit().await?;
            fields
        };
        // A row must have been made under this firm's own key, whatever the table says.
        if key_id != current {
            return Err(Error::Crypto("a stored secret was not made with this firm's key".into()));
        }
        let data_key = self.keys.decrypt(&key_id, &wrapped, &binding).await?;
        let value = open(&data_key, &nonce, &ciphertext, &binding)?;
        String::from_utf8(value.to_vec()).map(Some).map_err(|_| Error::Crypto("a stored secret is not text".into()))
    }

    async fn delete(&self, tenant: TenantId, name: &str) -> Result<()> {
        check_name(tenant, name)?;
        let mut conn = self.conn().await?;
        let tx = crate::db::TenantTx::begin(&mut conn, tenant).await?;
        tx.sql().execute("DELETE FROM connector_secrets WHERE tenant_id = app_tenant() AND name = $1", &[&name]).await?;
        tx.commit().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_tokens_open_only_for_their_own_firm_and_name_and_never_when_changed() {
        let (a, b) = (TenantId(Uuid::new_v4()), TenantId(Uuid::new_v4()));
        let key = [7u8; 32];
        let mine = Binding { tenant: a, name: refresh_token_name(a, Uuid::new_v4()) };
        let (nonce, sealed) = seal(&key, b"the token", &mine).unwrap();
        assert_eq!(nonce.len(), 12);
        assert!(!sealed.windows(9).any(|window| window == b"the token"));
        assert_eq!(open(&key, &nonce, &sealed, &mine).unwrap().as_slice(), b"the token");
        // Another firm, another name, another key, a changed byte or a changed nonce: none of them open it.
        let other_firm = Binding { tenant: b, name: mine.name.clone() };
        let other_name = Binding { tenant: a, name: refresh_token_name(a, Uuid::new_v4()) };
        assert!(open(&key, &nonce, &sealed, &other_firm).is_err());
        assert!(open(&key, &nonce, &sealed, &other_name).is_err());
        assert!(open(&[8u8; 32], &nonce, &sealed, &mine).is_err());
        let mut damaged = sealed.clone();
        damaged[0] ^= 1;
        assert!(open(&key, &nonce, &damaged, &mine).is_err());
        let mut other_nonce = nonce.clone();
        other_nonce[0] ^= 1;
        assert!(open(&key, &other_nonce, &sealed, &mine).is_err());
        assert!(open(&key, &nonce[..5], &sealed, &mine).is_err(), "a short nonce is refused, not padded");
        // Sealing the same token twice gives different bytes: the nonce is never reused.
        let (second_nonce, second) = seal(&key, b"the token", &mine).unwrap();
        assert_ne!(second_nonce, nonce);
        assert_ne!(second, sealed);
        // A key of the wrong length is an error, not a weaker cipher.
        assert!(seal(&[1u8; 16], b"x", &mine).is_err());
    }

    #[tokio::test]
    async fn a_firm_can_only_name_its_own_secrets() {
        let (a, b) = (TenantId(Uuid::new_v4()), TenantId(Uuid::new_v4()));
        let store = MemorySecrets::default();
        let mine = refresh_token_name(a, Uuid::new_v4());
        store.put(a, &mine, "token").await.unwrap();
        assert_eq!(store.get(a, &mine).await.unwrap().as_deref(), Some("token"));
        // Another firm cannot read, overwrite or delete it, even knowing the exact name.
        assert!(matches!(
            store.get(b, &mine).await,
            Err(Error::TenantMismatch)
        ));
        assert!(matches!(
            store.put(b, &mine, "x").await,
            Err(Error::TenantMismatch)
        ));
        assert!(matches!(
            store.delete(b, &mine).await,
            Err(Error::TenantMismatch)
        ));
        // Names that try to climb out of the firm's area are refused.
        let escape = format!("tenants/{}/../{}/x", a.0, b.0);
        assert!(store.get(a, &escape).await.is_err());
        assert!(store.get(a, "somewhere/else").await.is_err());
        assert_eq!(store.get(a, &mine).await.unwrap().as_deref(), Some("token"));
    }

    #[tokio::test]
    async fn file_secrets_are_private_shared_between_processes_and_firm_scoped() {
        let dir = std::env::temp_dir().join(format!("secrets-test-{}", Uuid::new_v4()));
        let (a, b) = (TenantId(Uuid::new_v4()), TenantId(Uuid::new_v4()));
        let name = refresh_token_name(a, Uuid::new_v4());
        let writer = FileSecrets::new(&dir).unwrap();
        writer.put(a, &name, "token").await.unwrap();
        // A second store on the same directory (as a second process would have) sees it.
        let reader = FileSecrets::new(&dir).unwrap();
        assert_eq!(
            reader.get(a, &name).await.unwrap().as_deref(),
            Some("token")
        );
        assert!(matches!(
            reader.get(b, &name).await,
            Err(Error::TenantMismatch)
        ));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &std::path::Path| {
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777
            };
            assert_eq!(mode(&dir), 0o700);
            assert_eq!(mode(&writer.path(&name)), 0o600);
        }
        // The name is not used as a path, so it cannot escape the directory.
        assert!(writer.path("tenants/x/../../etc/passwd").starts_with(&dir));
        writer.delete(a, &name).await.unwrap();
        assert_eq!(reader.get(a, &name).await.unwrap(), None);
        writer.delete(a, &name).await.unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
