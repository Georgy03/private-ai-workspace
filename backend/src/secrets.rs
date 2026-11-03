//! Where connector refresh tokens live. A refresh token lets us read a firm's files for as long as
//! the firm allows, so it is kept apart from the database and scoped to one firm.
//!
//! The production store (AWS Secrets Manager, reached through the per-firm `tenant_access` role
//! described in infra/iam.tf) is not written yet. Until it is, the only working store is the
//! in-memory one, which the server refuses to use outside development.

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

#[cfg(test)]
mod tests {
    use super::*;

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
