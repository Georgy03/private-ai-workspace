//! Connecting a Microsoft or Google account. An administrator is sent to the provider to approve
//! read-only access; the provider sends them back with a one-time code, which this server
//! exchanges for a long-lived refresh token.
//!
//! The "state" value ties the round trip to the firm and the administrator who started it. It is
//! signed, so a callback cannot be forged or redirected to another firm, and it expires quickly.

use std::time::{SystemTime, UNIX_EPOCH};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use hmac::{Hmac, KeyInit, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::Sha256;
use url::Url;
use uuid::Uuid;

use crate::{Error, Result, auth::TenantId};

type HmacSha256 = Hmac<Sha256>;
const STATE_LIFETIME_SECS: u64 = 600;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Microsoft365,
    GoogleDrive,
}

impl Kind {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "microsoft365" => Some(Kind::Microsoft365),
            "google_drive" => Some(Kind::GoogleDrive),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Microsoft365 => "microsoft365",
            Kind::GoogleDrive => "google_drive",
        }
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    /// Base URL of the provider's file API (Graph or Drive).
    pub api_base: String,
}

impl ProviderConfig {
    pub fn microsoft(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
            auth_url: "https://login.microsoftonline.com/common/oauth2/v2.0/authorize".into(),
            token_url: "https://login.microsoftonline.com/common/oauth2/v2.0/token".into(),
            // Files.Read.All covers OneDrive and SharePoint libraries for work accounts and
            // OneDrive for personal accounts. Sites.Read.All is left out on purpose: personal
            // Microsoft accounts cannot grant it, which would break their sign-in.
            scopes: ["offline_access", "Files.Read.All", "User.Read"]
                .map(String::from)
                .to_vec(),
            api_base: crate::connectors::microsoft::GRAPH_BASE.into(),
        }
    }

    pub fn google(client_id: String, client_secret: String) -> Self {
        Self {
            client_id,
            client_secret,
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".into(),
            token_url: "https://oauth2.googleapis.com/token".into(),
            scopes: vec!["https://www.googleapis.com/auth/drive.readonly".into()],
            api_base: crate::connectors::google::DRIVE_BASE.into(),
        }
    }
}

#[derive(Clone, Default)]
pub struct OAuthSettings {
    pub microsoft: Option<ProviderConfig>,
    pub google: Option<ProviderConfig>,
    /// Where the provider sends the administrator back to, registered with the provider.
    pub redirect_uri: String,
    pub state_key: Vec<u8>,
}

impl OAuthSettings {
    pub fn provider(&self, kind: Kind) -> Option<&ProviderConfig> {
        match kind {
            Kind::Microsoft365 => self.microsoft.as_ref(),
            Kind::GoogleDrive => self.google.as_ref(),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.state_key.len() >= 32
            && !self.redirect_uri.is_empty()
            && (self.microsoft.is_some() || self.google.is_some())
    }
}

#[derive(Serialize, Deserialize, Debug, PartialEq)]
pub struct State {
    pub tenant: Uuid,
    pub user: String,
    pub kind: String,
    pub config: Value,
    pub exp: u64,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
}

fn mac(key: &[u8]) -> HmacSha256 {
    HmacSha256::new_from_slice(key).expect("HMAC accepts any key length")
}

pub fn sign_state(key: &[u8], tenant: TenantId, user: &str, kind: Kind, config: Value) -> String {
    let state = State {
        tenant: tenant.0,
        user: user.into(),
        kind: kind.as_str().into(),
        config,
        exp: now() + STATE_LIFETIME_SECS,
    };
    let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&state).unwrap());
    let signature = {
        let mut mac = mac(key);
        mac.update(payload.as_bytes());
        URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
    };
    format!("{payload}.{signature}")
}

pub fn verify_state(key: &[u8], value: &str) -> Result<State> {
    let bad = || Error::Token("invalid or expired state".into());
    let (payload, signature) = value.split_once('.').ok_or_else(bad)?;
    let signature = URL_SAFE_NO_PAD.decode(signature).map_err(|_| bad())?;
    let mut mac = mac(key);
    mac.update(payload.as_bytes());
    // verify_slice compares in constant time.
    mac.verify_slice(&signature).map_err(|_| bad())?;
    let state: State = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(payload).map_err(|_| bad())?)
        .map_err(|_| bad())?;
    if state.exp < now() {
        return Err(bad());
    }
    Ok(state)
}

pub fn authorization_url(
    provider: &ProviderConfig,
    redirect_uri: &str,
    state: &str,
    kind: Kind,
) -> Result<String> {
    let mut url = Url::parse(&provider.auth_url)
        .map_err(|error| Error::Config(format!("auth URL: {error}")))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("client_id", &provider.client_id)
            .append_pair("response_type", "code")
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("scope", &provider.scopes.join(" "))
            .append_pair("state", state);
        if kind == Kind::GoogleDrive {
            // Without these Google returns no refresh token.
            query
                .append_pair("access_type", "offline")
                .append_pair("prompt", "consent");
        }
    }
    Ok(url.into())
}

pub struct Tokens {
    pub access_token: String,
    pub refresh_token: String,
}

pub async fn exchange_code(
    http: &reqwest::Client,
    provider: &ProviderConfig,
    redirect_uri: &str,
    code: &str,
) -> Result<Tokens> {
    let response = http
        .post(&provider.token_url)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", provider.client_id.as_str()),
            ("client_secret", provider.client_secret.as_str()),
        ])
        .send()
        .await
        .map_err(|_| Error::Token("could not reach the provider".into()))?;
    let ok = response.status().is_success();
    let body: Value = response.json().await.unwrap_or(Value::Null);
    let field = |name: &str| body[name].as_str().filter(|_| ok).map(str::to_string);
    match (field("access_token"), field("refresh_token")) {
        (Some(access_token), Some(refresh_token)) => Ok(Tokens {
            access_token,
            refresh_token,
        }),
        _ => Err(Error::Token(
            "the provider did not grant long-lived access".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

    #[test]
    fn state_round_trips_and_carries_the_firm_and_administrator() {
        let tenant = TenantId(Uuid::new_v4());
        let signed = sign_state(
            KEY,
            tenant,
            "admin-1",
            Kind::GoogleDrive,
            json!({ "domains": ["firm.com"] }),
        );
        let state = verify_state(KEY, &signed).unwrap();
        assert_eq!(
            (state.tenant, state.user.as_str(), state.kind.as_str()),
            (tenant.0, "admin-1", "google_drive")
        );
        assert_eq!(state.config["domains"][0], "firm.com");
    }

    #[test]
    fn forged_tampered_wrong_key_and_expired_states_are_refused() {
        let tenant = TenantId(Uuid::new_v4());
        let signed = sign_state(KEY, tenant, "admin", Kind::Microsoft365, json!({}));
        assert!(verify_state(b"another key another key another!", &signed).is_err());
        assert!(verify_state(KEY, "garbage").is_err());
        assert!(verify_state(KEY, "a.b").is_err());

        // Swap in a payload for a different firm but keep the original signature.
        let (_, signature) = signed.split_once('.').unwrap();
        let other = State {
            tenant: Uuid::new_v4(),
            user: "admin".into(),
            kind: "microsoft365".into(),
            config: json!({}),
            exp: now() + 600,
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&other).unwrap());
        assert!(verify_state(KEY, &format!("{payload}.{signature}")).is_err());

        // Correctly signed, but out of date.
        let old = State {
            exp: now() - 1,
            ..other
        };
        let payload = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&old).unwrap());
        let mut m = mac(KEY);
        m.update(payload.as_bytes());
        let signature = URL_SAFE_NO_PAD.encode(m.finalize().into_bytes());
        assert!(verify_state(KEY, &format!("{payload}.{signature}")).is_err());
    }

    #[test]
    fn the_authorization_link_asks_for_read_only_offline_access() {
        let provider = ProviderConfig::google("client".into(), "secret".into());
        let link = authorization_url(
            &provider,
            "https://api.example/oauth/callback",
            "STATE",
            Kind::GoogleDrive,
        )
        .unwrap();
        let url = Url::parse(&link).unwrap();
        let get = |name: &str| {
            url.query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.to_string())
        };
        assert_eq!(
            get("scope").as_deref(),
            Some("https://www.googleapis.com/auth/drive.readonly")
        );
        assert_eq!(
            (get("state").as_deref(), get("access_type").as_deref()),
            (Some("STATE"), Some("offline"))
        );
        assert!(
            get("client_secret").is_none(),
            "the secret must never appear in a browser link"
        );
        let ms = authorization_url(
            &ProviderConfig::microsoft("c".into(), "s".into()),
            "https://x/cb",
            "S",
            Kind::Microsoft365,
        )
        .unwrap();
        assert!(ms.contains("offline_access") && !ms.contains("access_type"));
    }
}
