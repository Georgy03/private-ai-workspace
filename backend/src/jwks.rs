//! Fetches the identity provider's signing keys, and re-fetches when a token names a key we do
//! not have yet (the provider rotates keys). Refreshes are rate limited so a stream of forged key
//! IDs cannot turn into a stream of outbound requests.

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use crate::{Error, Result, auth::TokenVerifier};

const MIN_REFRESH_GAP: Duration = Duration::from_secs(60);

pub struct JwksSource {
    http: reqwest::Client,
    url: String,
    last_refresh: Mutex<Option<Instant>>,
}

impl JwksSource {
    pub fn new(url: String) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| Error::Token(format!("could not start HTTPS: {error}")))?;
        Ok(Self {
            http,
            url,
            last_refresh: Mutex::new(None),
        })
    }

    pub async fn load(&self, verifier: &TokenVerifier) -> Result<()> {
        let body = self
            .http
            .get(&self.url)
            .send()
            .await
            .and_then(|response| response.error_for_status())
            .map_err(|error| Error::Token(format!("could not fetch signing keys: {error}")))?
            .text()
            .await
            .map_err(|error| Error::Token(format!("could not read signing keys: {error}")))?;
        verifier.set_jwks(&body)
    }

    /// Refreshes unless one happened in the last minute. Failures keep the keys we already have.
    pub async fn refresh(&self, verifier: &TokenVerifier) {
        {
            let mut last = self.last_refresh.lock().unwrap();
            if last.is_some_and(|at| at.elapsed() < MIN_REFRESH_GAP) {
                return;
            }
            *last = Some(Instant::now());
        }
        if let Err(error) = self.load(verifier).await {
            tracing::warn!(%error, "signing key refresh failed");
        }
    }
}
