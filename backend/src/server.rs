//! Process start-up: settings from the environment, the database pool, and the TLS rules for
//! reaching the database.

use std::{sync::Arc, time::Duration};

use deadpool_postgres::{Manager, ManagerConfig, Pool, RecyclingMethod, Runtime};
use rustls::pki_types::{CertificateDer, pem::PemObject};
use tokio_postgres::NoTls;
use tokio_postgres_rustls::MakeRustlsConnect;

use crate::{
    Error, Result,
    oauth::{OAuthSettings, ProviderConfig},
    secrets::{MemorySecrets, SecretStore, UnconfiguredSecrets},
};

pub struct Settings {
    pub bind: String,
    pub database_url: String,
    pub database_ca_file: Option<String>,
    /// Development only. Skips TLS to the database and allows a non-HTTPS key URL.
    pub allow_insecure: bool,
    pub issuer: String,
    pub audience: String,
}

fn var(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| Error::Config(format!("{name} is not set")))
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            bind: std::env::var("BIND").unwrap_or_else(|_| "127.0.0.1:8080".into()),
            database_url: var("DATABASE_URL")?,
            database_ca_file: std::env::var("DATABASE_CA_FILE").ok(),
            allow_insecure: std::env::var("ALLOW_INSECURE").as_deref() == Ok("1"),
            issuer: var("COGNITO_ISSUER")?,
            audience: var("COGNITO_CLIENT_ID")?,
        })
    }

    /// Settings for the `migrate` command, which needs only the database.
    pub fn for_migration() -> Result<Self> {
        Ok(Self {
            bind: String::new(),
            database_url: var("DATABASE_URL")?,
            database_ca_file: std::env::var("DATABASE_CA_FILE").ok(),
            allow_insecure: std::env::var("ALLOW_INSECURE").as_deref() == Ok("1"),
            issuer: String::new(),
            audience: String::new(),
        })
    }

    pub fn jwks_url(&self) -> Result<String> {
        if !self.issuer.starts_with("https://") && !self.allow_insecure {
            return Err(Error::Config("COGNITO_ISSUER must be an https URL".into()));
        }
        Ok(format!(
            "{}/.well-known/jwks.json",
            self.issuer.trim_end_matches('/')
        ))
    }
}

fn tls_connector(ca_pem: &[u8]) -> Result<MakeRustlsConnect> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(ca_pem) {
        let cert = cert.map_err(|error| Error::Config(format!("bad CA file: {error}")))?;
        roots
            .add(cert)
            .map_err(|error| Error::Config(format!("bad CA certificate: {error}")))?;
    }
    if roots.is_empty() {
        return Err(Error::Config("the CA file holds no certificates".into()));
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| Error::Config(format!("TLS setup failed: {error}")))?
    .with_root_certificates(roots)
    .with_no_client_auth();
    Ok(MakeRustlsConnect::new(config))
}

/// Connector settings. Without OAUTH_STATE_KEY the connector features stay switched off.
pub fn oauth_from_env(allow_insecure: bool) -> Result<OAuthSettings> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};
    let Ok(key) = std::env::var("OAUTH_STATE_KEY") else {
        return Ok(OAuthSettings::default());
    };
    let state_key = STANDARD
        .decode(key.trim())
        .map_err(|_| Error::Config("OAUTH_STATE_KEY must be base64".into()))?;
    if state_key.len() < 32 {
        return Err(Error::Config(
            "OAUTH_STATE_KEY must be at least 32 bytes".into(),
        ));
    }
    let redirect_uri = var("OAUTH_REDIRECT_URI")?;
    if !redirect_uri.starts_with("https://") && !allow_insecure {
        return Err(Error::Config(
            "OAUTH_REDIRECT_URI must be an https URL".into(),
        ));
    }
    let pair = |id: &str, secret: &str| match (std::env::var(id), std::env::var(secret)) {
        (Ok(id), Ok(secret)) if !id.is_empty() && !secret.is_empty() => Some((id, secret)),
        _ => None,
    };
    Ok(OAuthSettings {
        microsoft: pair("MICROSOFT_CLIENT_ID", "MICROSOFT_CLIENT_SECRET")
            .map(|(id, secret)| ProviderConfig::microsoft(id, secret)),
        google: pair("GOOGLE_CLIENT_ID", "GOOGLE_CLIENT_SECRET")
            .map(|(id, secret)| ProviderConfig::google(id, secret)),
        redirect_uri,
        state_key,
    })
}

/// The production secret store is not written yet, so outside development nothing can be saved.
/// In development, DEV_SECRETS_DIR makes the API and worker share tokens through private files.
pub fn secrets_from_env(allow_insecure: bool) -> Result<std::sync::Arc<dyn SecretStore>> {
    if !allow_insecure {
        return Ok(std::sync::Arc::new(UnconfiguredSecrets));
    }
    Ok(match std::env::var("DEV_SECRETS_DIR") {
        Ok(dir) if !dir.is_empty() => std::sync::Arc::new(crate::secrets::FileSecrets::new(dir)?),
        _ => std::sync::Arc::new(MemorySecrets::default()),
    })
}

/// The database connection always uses TLS and checks the server certificate. The only way
/// around that is the explicit development switch.
pub fn build_pool(settings: &Settings, max_size: usize) -> Result<Pool> {
    let config: tokio_postgres::Config = settings
        .database_url
        .parse()
        .map_err(|error| Error::Config(format!("DATABASE_URL: {error}")))?;
    let manager_config = ManagerConfig {
        recycling_method: RecyclingMethod::Fast,
    };
    let manager = match (&settings.database_ca_file, settings.allow_insecure) {
        (Some(path), _) => {
            let pem = std::fs::read(path)
                .map_err(|error| Error::Config(format!("cannot read {path}: {error}")))?;
            Manager::from_config(config, tls_connector(&pem)?, manager_config)
        }
        (None, true) => Manager::from_config(config, NoTls, manager_config),
        (None, false) => {
            return Err(Error::Config(
                "set DATABASE_CA_FILE to the database's CA bundle (for RDS, global-bundle.pem)"
                    .into(),
            ));
        }
    };
    Pool::builder(manager)
        .max_size(max_size)
        .runtime(Runtime::Tokio1)
        .wait_timeout(Some(Duration::from_secs(5)))
        .create_timeout(Some(Duration::from_secs(5)))
        .build()
        .map_err(|error| Error::Config(format!("pool: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(ca: Option<&str>, allow_insecure: bool) -> Settings {
        Settings {
            bind: "127.0.0.1:0".into(),
            database_url: "host=localhost user=app_api".into(),
            database_ca_file: ca.map(String::from),
            allow_insecure,
            issuer: "https://issuer.example/pool".into(),
            audience: "client".into(),
        }
    }

    #[test]
    fn the_database_connection_requires_tls_unless_development_says_otherwise() {
        assert!(build_pool(&settings(None, false), 1).is_err());
        assert!(build_pool(&settings(None, true), 1).is_ok());
    }

    #[test]
    fn a_missing_or_empty_ca_file_is_refused() {
        assert!(build_pool(&settings(Some("/does/not/exist.pem"), false), 1).is_err());
        assert!(tls_connector(b"").is_err());
        assert!(tls_connector(b"not a certificate").is_err());
    }

    #[test]
    fn the_key_url_must_be_https_outside_development() {
        let mut plain = settings(None, false);
        plain.issuer = "http://issuer.example/pool".into();
        assert!(plain.jwks_url().is_err());
        plain.allow_insecure = true;
        assert_eq!(
            plain.jwks_url().unwrap(),
            "http://issuer.example/pool/.well-known/jwks.json"
        );
        assert_eq!(
            settings(None, false).jwks_url().unwrap(),
            "https://issuer.example/pool/.well-known/jwks.json"
        );
    }
}
