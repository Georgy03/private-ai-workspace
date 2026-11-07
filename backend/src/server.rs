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
    secrets::{MemorySecrets, SecretStore},
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

/// Where the API listens. `BIND` says so outright. Otherwise a host that hands the service a `PORT`
/// (Railway, and most platforms) gets that port on every interface, which is how a container is
/// reached. With neither, only this machine can reach it.
pub fn bind_address(bind: Option<&str>, port: Option<&str>) -> String {
    if let Some(bind) = bind.map(str::trim).filter(|bind| !bind.is_empty()) {
        return bind.to_string();
    }
    match port.map(str::trim).and_then(|port| port.parse::<u16>().ok()).filter(|port| *port != 0) {
        Some(port) => format!("0.0.0.0:{port}"),
        None => "127.0.0.1:8080".to_string(),
    }
}

fn var(name: &str) -> Result<String> {
    std::env::var(name).map_err(|_| Error::Config(format!("{name} is not set")))
}

impl Settings {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            bind: bind_address(std::env::var("BIND").ok().as_deref(), std::env::var("PORT").ok().as_deref()),
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
        dropbox: pair("DROPBOX_CLIENT_ID", "DROPBOX_CLIENT_SECRET")
            .map(|(id, secret)| ProviderConfig::dropbox(id, secret)),
        // DOCUSIGN_ENVIRONMENT=production uses the real service; anything else the developer sandbox.
        docusign: pair("DOCUSIGN_CLIENT_ID", "DOCUSIGN_CLIENT_SECRET").map(|(id, secret)| {
            let production = std::env::var("DOCUSIGN_ENVIRONMENT").as_deref() == Ok("production");
            ProviderConfig::docusign(id, secret, !production)
        }),
        // Sending email uses the same Microsoft app, asking for the send permission only.
        outlook_mail: pair("MICROSOFT_CLIENT_ID", "MICROSOFT_CLIENT_SECRET")
            .map(|(id, secret)| ProviderConfig::outlook_mail(id, secret)),
        // QUICKBOOKS_ENVIRONMENT=production uses real companies; anything else Intuit's sandbox.
        quickbooks: pair("QUICKBOOKS_CLIENT_ID", "QUICKBOOKS_CLIENT_SECRET").map(|(id, secret)| {
            let production = std::env::var("QUICKBOOKS_ENVIRONMENT").as_deref() == Ok("production");
            ProviderConfig::quickbooks(id, secret, production)
        }),
        // Zoom refuses http://localhost, so ZOOM_REDIRECT_URI can give it its own https address.
        zoom: match pair("ZOOM_CLIENT_ID", "ZOOM_CLIENT_SECRET") {
            Some((id, secret)) => {
                let mut zoom = ProviderConfig::zoom(id, secret);
                if let Ok(own) = std::env::var("ZOOM_REDIRECT_URI").map(|v| v.trim().to_string())
                    && !own.is_empty()
                {
                    if !own.starts_with("https://") || !own.ends_with("/oauth/callback") {
                        return Err(Error::Config("ZOOM_REDIRECT_URI must be an https URL ending in /oauth/callback".into()));
                    }
                    zoom.redirect_uri = Some(own);
                }
                Some(zoom)
            }
            None => None,
        },
        redirect_uri,
        state_key,
    })
}

/// Where connector tokens are kept. Outside development that is the database, encrypted under each
/// firm's own AWS KMS key, reached with the process's AWS credentials (a task role in AWS, or the
/// standard AWS_* variables elsewhere). In development, DEV_SECRETS_DIR makes the API and worker
/// share tokens through private files.
pub async fn secrets_from_env(allow_insecure: bool, pool: &Pool) -> Result<std::sync::Arc<dyn SecretStore>> {
    if !allow_insecure {
        let keys = std::sync::Arc::new(crate::kms::AwsKms::from_env().await);
        return Ok(std::sync::Arc::new(crate::secrets::PostgresSecrets::new(pool.clone(), keys)));
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

    #[test]
    fn the_api_listens_where_it_is_told_then_on_the_hosts_port_then_only_on_this_machine() {
        assert_eq!(bind_address(Some("10.0.0.5:9000"), Some("3000")), "10.0.0.5:9000", "BIND wins");
        assert_eq!(bind_address(None, Some("3000")), "0.0.0.0:3000");
        assert_eq!(bind_address(Some("  "), Some(" 3000 ")), "0.0.0.0:3000", "blank BIND is not an address");
        assert_eq!(bind_address(None, None), "127.0.0.1:8080");
        for bad in ["", "abc", "0", "70000", "-1", "80; rm -rf"] {
            assert_eq!(bind_address(None, Some(bad)), "127.0.0.1:8080", "{bad:?} is not a port");
        }
    }

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

/// Whether this server may send document text straight to the Claude API. That route has no zero
/// data retention, so it is for local development only (`ALLOW_INSECURE=1`). A production server
/// has to reach Claude through Amazon Bedrock with zero data retention, which this server does not
/// do yet, so agents and company knowledge are off there rather than quietly less private.
pub fn direct_claude_api_allowed() -> bool {
    direct_api_allowed_by(std::env::var("ALLOW_INSECURE").ok().as_deref())
}

fn direct_api_allowed_by(setting: Option<&str>) -> bool {
    setting == Some("1")
}

#[cfg(test)]
mod direct_api_tests {
    use super::direct_api_allowed_by;

    #[test]
    fn the_direct_claude_api_is_only_for_local_development() {
        assert!(direct_api_allowed_by(Some("1")));
        assert!(!direct_api_allowed_by(None));
        assert!(!direct_api_allowed_by(Some("0")));
        assert!(!direct_api_allowed_by(Some("true")));
    }
}
