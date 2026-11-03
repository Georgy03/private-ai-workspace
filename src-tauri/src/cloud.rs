//! The firm's cloud index: passages from connected sources (Microsoft 365, Google Drive) kept on
//! the firm's server. This module signs the user in, asks that server to search, and turns the
//! answers into the same `SearchHit`s the local search produces.
//!
//! Only the question text goes to the server. Local files are never uploaded: the server cannot
//! reach this device, and local passages are merged with cloud passages here, on the device.

use std::{
    path::{Path, PathBuf},
    sync::Mutex,
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::models::{CloudState, CloudStatus, Origin, SearchHit};

const SEARCH_TIMEOUT: Duration = Duration::from_secs(8);
const MANAGE_TIMEOUT: Duration = Duration::from_secs(15);
/// A long document is a large reply, so it is given longer than a search.
const DOCUMENT_TIMEOUT: Duration = Duration::from_secs(60);
const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(180);
/// Must match a callback URL registered on the Cognito app client (infra/variables.tf).
pub const REDIRECT_PORT: u16 = 53682;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const SETTINGS_FILE: &str = "cloud.json";
const TOKENS_FILE: &str = "cloud-tokens.json";
const KEYRING_ACCOUNT: &str = "cloud-tokens";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudError {
    NotConfigured,
    Off,
    SignedOut,
    /// The server understood the request and refused it for this person.
    NotAllowed,
    Unavailable(String),
}

impl CloudError {
    /// A sentence for the person using the app. Never contains tokens or server details.
    pub fn message(&self) -> String {
        match self {
            CloudError::NotConfigured => "The firm cloud index is not set up.".into(),
            CloudError::Off => "The firm cloud index is switched off.".into(),
            CloudError::SignedOut => "Sign in to search the firm cloud index.".into(),
            CloudError::NotAllowed => {
                "Only a firm administrator can change connected sources.".into()
            }
            CloudError::Unavailable(reason) => {
                format!("The firm cloud index is unavailable: {reason}")
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Stored {
    api_base: Option<String>,
    cognito_domain: Option<String>,
    cognito_client_id: Option<String>,
    enabled: Option<bool>,
}

#[derive(Debug, Clone, Default)]
struct Settings {
    api_base: Option<String>,
    cognito_domain: Option<String>,
    cognito_client_id: Option<String>,
    /// Development only: a ready-made token, so the app can talk to a local test server.
    dev_token: Option<String>,
    enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Tokens {
    id_token: String,
    refresh_token: Option<String>,
    /// Seconds since 1970.
    expires_at: i64,
}

pub struct Cloud {
    dir: PathBuf,
    http: reqwest::Client,
    settings: Mutex<Settings>,
    tokens: Mutex<Option<Tokens>>,
    search_timeout: Duration,
    redirect_port: u16,
}

/// The server must be reached over HTTPS. Development builds may also use plain HTTP, but only on
/// this machine.
fn check_base(base: &str, allow_local_http: bool) -> Result<String, CloudError> {
    let url = reqwest::Url::parse(base)
        .map_err(|_| CloudError::Unavailable("the server address is not valid".into()))?;
    let local = matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"));
    let allowed = url.scheme() == "https" || (allow_local_http && url.scheme() == "http" && local);
    if !allowed || url.query().is_some() || url.fragment().is_some() {
        return Err(CloudError::Unavailable(
            "the server address must use https".into(),
        ));
    }
    Ok(base.trim_end_matches('/').to_string())
}

fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("HTTPS client")
}

// ---- Token storage: a private file in development builds, the OS credential store otherwise ----

fn load_tokens(dir: &Path) -> Option<Tokens> {
    if cfg!(debug_assertions) {
        let text = std::fs::read_to_string(dir.join(TOKENS_FILE)).ok()?;
        return serde_json::from_str(&text).ok();
    }
    let text = keyring::Entry::new(crate::db::KEYRING_SERVICE, KEYRING_ACCOUNT)
        .ok()?
        .get_password()
        .ok()?;
    serde_json::from_str(&text).ok()
}

fn save_tokens(dir: &Path, tokens: &Tokens) -> Result<(), CloudError> {
    let text = serde_json::to_string(tokens).map_err(|e| CloudError::Unavailable(e.to_string()))?;
    let failed = |_| CloudError::Unavailable("could not save the sign-in".into());
    if cfg!(debug_assertions) {
        return write_private(&dir.join(TOKENS_FILE), text.as_bytes()).map_err(failed);
    }
    keyring::Entry::new(crate::db::KEYRING_SERVICE, KEYRING_ACCOUNT)
        .and_then(|entry| entry.set_password(&text))
        .map_err(|_| CloudError::Unavailable("could not save the sign-in".into()))
}

fn clear_tokens(dir: &Path) {
    if cfg!(debug_assertions) {
        let _ = std::fs::remove_file(dir.join(TOKENS_FILE));
    } else if let Ok(entry) = keyring::Entry::new(crate::db::KEYRING_SERVICE, KEYRING_ACCOUNT) {
        let _ = entry.delete_credential();
    }
}

fn write_private(path: &Path, contents: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(contents))
}

// ---- Connected sources ----

/// A connector the server offers.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorInfo {
    pub kind: String,
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Whether this server is set up for it. An unavailable connector cannot be connected.
    pub available: bool,
}

/// A source the firm has connected.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Connection {
    pub id: String,
    pub kind: String,
    /// "active" or "needs_reauth".
    pub status: String,
    pub last_sync_epoch: Option<f64>,
    pub last_error: Option<String>,
    #[serde(default)]
    pub documents: i64,
    /// A sync is running right now.
    #[serde(default)]
    pub syncing: bool,
    #[serde(default)]
    pub sync_started_epoch: Option<f64>,
    /// Files the running (or last) sync has looked at, and how many of them failed.
    #[serde(default)]
    pub sync_files_read: i64,
    #[serde(default)]
    pub sync_files_failed: i64,
    #[serde(default)]
    pub next_sync_epoch: Option<f64>,
    /// Files whose text can be searched.
    #[serde(default)]
    pub readable: i64,
    /// Why files are stored by name only, with how many of each.
    #[serde(default)]
    pub name_only_reasons: std::collections::BTreeMap<String, i64>,
}

/// One person's AI use this month and their limit, from the firm's server. Amounts are in
/// millionths of a dollar.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    pub used_micros: i64,
    pub limit_micros: i64,
    pub remaining_micros: i64,
    #[serde(default)]
    pub answers: i64,
    /// When the month ends and the allowance starts again.
    #[serde(default)]
    pub period_end_epoch: f64,
    /// The limit is the server's default, not one the firm chose.
    #[serde(default)]
    pub limit_is_default: bool,
}

/// A cloud document's whole text, in reading order.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CloudDocument {
    pub name: String,
    pub total_chars: i64,
    /// The document is longer than the limit asked for, so no text came back.
    pub too_long: bool,
    #[serde(default)]
    pub passages: Vec<CloudDocumentPassage>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct CloudDocumentPassage {
    #[serde(default)]
    pub label: Option<String>,
    pub text: String,
}

/// A file stored by name only, and why.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NameOnlyFile {
    pub name: String,
    pub reason: String,
    #[serde(default)]
    pub mime_type: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConnectorsView {
    pub connectors: Vec<ConnectorInfo>,
    pub connections: Vec<Connection>,
}

struct Raw {
    status: u16,
    body: Vec<u8>,
}

fn manage_error(status: u16) -> CloudError {
    match status {
        403 => CloudError::NotAllowed,
        404 => CloudError::Unavailable("that connection no longer exists".into()),
        503 => CloudError::Unavailable("this server is not set up for that connector".into()),
        400 | 422 => CloudError::Unavailable("the server did not accept the request".into()),
        _ => CloudError::Unavailable("the server reported a problem".into()),
    }
}

/// Connection IDs go into a web address, so only real IDs are allowed through.
fn check_id(id: &str) -> Result<(), CloudError> {
    if id.len() == 36 && id.chars().all(|c| c.is_ascii_hexdigit() || c == '-') {
        Ok(())
    } else {
        Err(CloudError::Unavailable(
            "that is not a valid connection".into(),
        ))
    }
}

/// The server hands back an address for the browser to open. A server that has been tampered with
/// could send something else (a local file, an app link), so only the sign-in pages of the two
/// supported providers are accepted. Development builds also accept this machine.
fn check_browser_url(url: &str, allow_local: bool) -> Result<(), CloudError> {
    let refuse = || CloudError::Unavailable("the sign-in address was not recognised".into());
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse())?;
    let host = parsed.host_str().unwrap_or("");
    let provider = parsed.scheme() == "https"
        && matches!(host, "accounts.google.com" | "login.microsoftonline.com");
    let local =
        allow_local && parsed.scheme() == "http" && matches!(host, "localhost" | "127.0.0.1");
    if provider || local {
        Ok(())
    } else {
        Err(refuse())
    }
}

// ---- The server's reply ----

#[derive(Deserialize)]
struct Reply {
    results: Vec<Passage>,
}

#[derive(Deserialize)]
struct Passage {
    /// Where in the file the passage sits ("Page 3", "Slide 2"), when the server knows.
    #[serde(default)]
    location_label: Option<String>,
    citation_id: String,
    document_id: String,
    document_name: String,
    text: String,
    score: f64,
    /// Opens the original file in its own service, when the server knows where it is.
    #[serde(default)]
    link: Option<String>,
}

impl Passage {
    /// IDs are prefixed so they can never collide with a local file's ID.
    fn into_hit(self) -> SearchHit {
        SearchHit {
            citation_id: format!("cloud:{}", self.citation_id),
            document_id: format!("cloud:{}", self.document_id),
            document_name: self.document_name,
            path: "Firm cloud index".into(),
            excerpt: self.text,
            location: Default::default(),
            location_label: match self.location_label {
                Some(place) if !place.trim().is_empty() => format!("Cloud · {place}"),
                _ => "Cloud".into(),
            },
            score: self.score,
            ocr: false,
            stale: false,
            origin: Origin::Cloud,
            link: self.link.filter(|url| check_document_link(url).is_ok()),
        }
    }
}

impl Cloud {
    /// Reads settings from `cloud.json` in the workspace folder, then lets environment variables
    /// override them: PRIVATE_AI_CLOUD_API, PRIVATE_AI_COGNITO_DOMAIN, PRIVATE_AI_COGNITO_CLIENT_ID,
    /// and (development builds only) PRIVATE_AI_CLOUD_DEV_TOKEN.
    pub fn load(dir: &Path) -> Self {
        let stored: Stored = std::fs::read_to_string(dir.join(SETTINGS_FILE))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
        let settings = Settings {
            api_base: env("PRIVATE_AI_CLOUD_API").or(stored.api_base),
            cognito_domain: env("PRIVATE_AI_COGNITO_DOMAIN").or(stored.cognito_domain),
            cognito_client_id: env("PRIVATE_AI_COGNITO_CLIENT_ID").or(stored.cognito_client_id),
            dev_token: env("PRIVATE_AI_CLOUD_DEV_TOKEN").filter(|_| cfg!(debug_assertions)),
            enabled: stored.enabled.unwrap_or(true),
        };
        Self {
            dir: dir.to_path_buf(),
            http: http_client(),
            tokens: Mutex::new(load_tokens(dir)),
            settings: Mutex::new(settings),
            search_timeout: SEARCH_TIMEOUT,
            redirect_port: REDIRECT_PORT,
        }
    }

    fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn status(&self) -> CloudStatus {
        let settings = self.settings();
        let signed_in = settings.dev_token.is_some() || self.tokens.lock().unwrap().is_some();
        let configured = settings.api_base.is_some();
        let (state, message) = if !configured {
            (
                CloudState::NotConfigured,
                "The firm cloud index is not set up on this device.",
            )
        } else if !settings.enabled {
            (
                CloudState::Off,
                "The firm cloud index is switched off. Searches use local files only.",
            )
        } else if !signed_in {
            (
                CloudState::SignedOut,
                "Sign in to search the firm cloud index.",
            )
        } else {
            (
                CloudState::Ready,
                "Signed in. Searches include the firm cloud index.",
            )
        };
        CloudStatus {
            state,
            message: message.into(),
            configured,
            enabled: settings.enabled,
            signed_in,
        }
    }

    pub fn set_enabled(&self, enabled: bool) -> Result<(), CloudError> {
        let mut settings = self.settings.lock().unwrap();
        settings.enabled = enabled;
        let stored = Stored {
            api_base: settings.api_base.clone(),
            cognito_domain: settings.cognito_domain.clone(),
            cognito_client_id: settings.cognito_client_id.clone(),
            enabled: Some(enabled),
        };
        let text = serde_json::to_string_pretty(&stored)
            .map_err(|e| CloudError::Unavailable(e.to_string()))?;
        write_private(&self.dir.join(SETTINGS_FILE), text.as_bytes())
            .map_err(|_| CloudError::Unavailable("could not save the setting".into()))
    }

    pub fn sign_out(&self) {
        *self.tokens.lock().unwrap() = None;
        clear_tokens(&self.dir);
    }

    // ---- Search ----

    #[cfg(test)]
    pub async fn search(&self, question: &str, limit: usize) -> Result<Vec<SearchHit>, CloudError> {
        self.search_tracked(question, limit).await.0
    }

    /// Like `search`, and also says whether a request carrying the question actually left this
    /// device, which the privacy receipt reports.
    pub async fn search_tracked(
        &self,
        question: &str,
        limit: usize,
    ) -> (Result<Vec<SearchHit>, CloudError>, bool) {
        let mut sent = false;
        let result = self.search_inner(question, limit, &mut sent).await;
        (result, sent)
    }

    async fn search_inner(
        &self,
        question: &str,
        limit: usize,
        sent: &mut bool,
    ) -> Result<Vec<SearchHit>, CloudError> {
        let body = serde_json::json!({ "query": question, "limit": limit });
        let raw = self
            .authed(
                reqwest::Method::POST,
                "/v1/search",
                Some(&body),
                self.search_timeout,
                true,
                sent,
            )
            .await?;
        match raw.status {
            200 => {
                let reply: Reply = serde_json::from_slice(&raw.body)
                    .map_err(|_| CloudError::Unavailable("the reply was not understood".into()))?;
                Ok(reply
                    .results
                    .into_iter()
                    .take(limit)
                    .map(Passage::into_hit)
                    .collect())
            }
            403 => Err(CloudError::Unavailable(
                "this account may not use it".into(),
            )),
            _ => Err(CloudError::Unavailable(
                "the server reported a problem".into(),
            )),
        }
    }

    /// Sends a request to the firm's server as the signed-in person. An expired token is
    /// refreshed once and the request repeated; if the server still refuses it, the person is
    /// signed out. Any other status is returned for the caller to interpret. `sent` records
    /// whether a request actually left this device.
    async fn authed(
        &self,
        method: reqwest::Method,
        path: &str,
        body: Option<&serde_json::Value>,
        timeout: Duration,
        respect_enabled: bool,
        sent: &mut bool,
    ) -> Result<Raw, CloudError> {
        let settings = self.settings();
        let base = settings
            .api_base
            .as_deref()
            .ok_or(CloudError::NotConfigured)?;
        if respect_enabled && !settings.enabled {
            return Err(CloudError::Off);
        }
        let url = format!("{}{path}", check_base(base, cfg!(debug_assertions))?);

        let mut token = self.id_token(&settings).await?;
        for attempt in 0..2 {
            *sent = true;
            let mut request = self
                .http
                .request(method.clone(), &url)
                .bearer_auth(&token)
                .timeout(timeout);
            if let Some(body) = body {
                request = request.json(body);
            }
            let response = match request.send().await {
                Ok(response) => response,
                Err(error) => {
                    // A connection that never opened carried nothing. A timeout may have.
                    if error.is_connect() {
                        *sent = false;
                    }
                    return Err(CloudError::Unavailable(if error.is_timeout() {
                        "it took too long to answer".into()
                    } else {
                        "it could not be reached".into()
                    }));
                }
            };
            let status = response.status().as_u16();
            match status {
                401 if attempt == 0 && settings.dev_token.is_none() => {
                    // The token may have just expired: refresh once and try again.
                    token = self.refresh(&settings).await?;
                    continue;
                }
                401 => {
                    self.sign_out();
                    return Err(CloudError::SignedOut);
                }
                _ => {}
            }
            if response
                .content_length()
                .is_some_and(|n| n as usize > MAX_RESPONSE_BYTES)
            {
                return Err(CloudError::Unavailable("the reply was too large".into()));
            }
            let body = response
                .bytes()
                .await
                .map_err(|_| CloudError::Unavailable("the connection was interrupted".into()))?;
            if body.len() > MAX_RESPONSE_BYTES {
                return Err(CloudError::Unavailable("the reply was too large".into()));
            }
            return Ok(Raw {
                status,
                body: body.to_vec(),
            });
        }
        Err(CloudError::SignedOut)
    }

    // ---- Connected sources (Google Drive, Microsoft 365) ----

    /// Reads the connectors this server offers and the sources the firm has connected.
    pub async fn connectors_view(&self) -> Result<ConnectorsView, CloudError> {
        #[derive(Deserialize)]
        struct Catalogue {
            connectors: Vec<ConnectorInfo>,
        }
        #[derive(Deserialize)]
        struct Listing {
            connections: Vec<Connection>,
        }
        let mut sent = false;
        let timeout = MANAGE_TIMEOUT;
        let catalogue = self
            .authed(
                reqwest::Method::GET,
                "/v1/connectors",
                None,
                timeout,
                false,
                &mut sent,
            )
            .await?;
        let listing = self
            .authed(
                reqwest::Method::GET,
                "/v1/connections",
                None,
                timeout,
                false,
                &mut sent,
            )
            .await?;
        let not_understood = || CloudError::Unavailable("the reply was not understood".into());
        // A server that has no such page is an older version, not a missing connection.
        if catalogue.status == 404 || listing.status == 404 {
            return Err(CloudError::Unavailable(
                "the server needs updating before it can manage connectors".into(),
            ));
        }
        if catalogue.status != 200 || listing.status != 200 {
            return Err(manage_error(catalogue.status.max(listing.status)));
        }
        Ok(ConnectorsView {
            connectors: serde_json::from_slice::<Catalogue>(&catalogue.body)
                .map_err(|_| not_understood())?
                .connectors,
            connections: serde_json::from_slice::<Listing>(&listing.body)
                .map_err(|_| not_understood())?
                .connections,
        })
    }

    /// Asks the server to start connecting a source and returns the provider's sign-in address,
    /// which the caller opens in the browser. `reconnect` names an existing connection that needs
    /// to be authorised again.
    pub async fn start_connection(
        &self,
        kind: &str,
        domains: &[String],
        reconnect: Option<&str>,
    ) -> Result<String, CloudError> {
        let mut body = serde_json::json!({ "kind": kind });
        if !domains.is_empty() {
            body["domains"] = serde_json::json!(domains);
        }
        if let Some(id) = reconnect {
            check_id(id)?;
            body["reconnect"] = serde_json::json!(id);
        }
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::POST,
                "/v1/connections",
                Some(&body),
                MANAGE_TIMEOUT,
                false,
                &mut sent,
            )
            .await?;
        if raw.status != 200 {
            return Err(manage_error(raw.status));
        }
        let url = serde_json::from_slice::<serde_json::Value>(&raw.body)
            .ok()
            .and_then(|value| value["authorization_url"].as_str().map(str::to_string))
            .ok_or_else(|| CloudError::Unavailable("the reply was not understood".into()))?;
        check_browser_url(&url, cfg!(debug_assertions))?;
        Ok(url)
    }

    pub async fn disconnect(&self, id: &str) -> Result<(), CloudError> {
        check_id(id)?;
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::DELETE,
                &format!("/v1/connections/{id}"),
                None,
                MANAGE_TIMEOUT,
                false,
                &mut sent,
            )
            .await?;
        match raw.status {
            200 | 204 => Ok(()),
            status => Err(manage_error(status)),
        }
    }

    pub async fn connection_files(&self, id: &str) -> Result<Vec<NameOnlyFile>, CloudError> {
        #[derive(Deserialize)]
        struct Listing {
            files: Vec<NameOnlyFile>,
        }
        check_id(id)?;
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::GET,
                &format!("/v1/connections/{id}/files"),
                None,
                MANAGE_TIMEOUT,
                false,
                &mut sent,
            )
            .await?;
        match raw.status {
            200 => serde_json::from_slice::<Listing>(&raw.body)
                .map(|listing| listing.files)
                .map_err(|_| CloudError::Unavailable("the reply was not understood".into())),
            status => Err(manage_error(status)),
        }
    }

    /// The whole text of one cloud document, if this person may read it. `Ok(None)` means there is
    /// no such document for them. Documents longer than `max_chars` come back without text.
    pub async fn document_text(
        &self,
        id: &str,
        max_chars: usize,
    ) -> Result<Option<CloudDocument>, CloudError> {
        check_id(id)?;
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::GET,
                &format!("/v1/documents/{id}?max_chars={max_chars}"),
                None,
                DOCUMENT_TIMEOUT,
                true,
                &mut sent,
            )
            .await?;
        match raw.status {
            200 => serde_json::from_slice(&raw.body)
                .map(Some)
                .map_err(|_| CloudError::Unavailable("the reply was not understood".into())),
            404 => Ok(None),
            status => Err(manage_error(status)),
        }
    }

    /// A company-knowledge request. The answers are passed on as the server wrote them; the
    /// interface decides how to show them. `query` pairs are percent-encoded here.
    pub async fn knowledge(
        &self,
        method: reqwest::Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&serde_json::Value>,
    ) -> Result<serde_json::Value, CloudError> {
        let mut url = reqwest::Url::parse("http://placeholder/").expect("a fixed address");
        url.query_pairs_mut()
            .extend_pairs(query.iter().filter(|(_, value)| !value.is_empty()));
        let target = match url.query() {
            Some(encoded) => format!("{path}?{encoded}"),
            None => path.to_string(),
        };
        let mut sent = false;
        let raw = self
            .authed(method, &target, body, MANAGE_TIMEOUT, true, &mut sent)
            .await?;
        match raw.status {
            200 => serde_json::from_slice(&raw.body)
                .map_err(|_| CloudError::Unavailable("the reply was not understood".into())),
            404 => Err(CloudError::Unavailable(
                "that item no longer exists, or it is not one you can see".into(),
            )),
            status => Err(manage_error(status)),
        }
    }

    /// What this person has used this month and their limit.
    pub async fn usage(&self) -> Result<Usage, CloudError> {
        self.usage_request(reqwest::Method::GET, None).await
    }

    /// Adds one answer to this person's use. The server prices it.
    pub async fn record_usage(
        &self,
        model: &str,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Result<Usage, CloudError> {
        let body = serde_json::json!({
            "model": model, "input_tokens": input_tokens, "output_tokens": output_tokens,
        });
        self.usage_request(reqwest::Method::POST, Some(&body)).await
    }

    /// Sets the firm's monthly limit per person, in cents; none returns to the server default.
    /// Only administrators may.
    pub async fn set_usage_limit(&self, cents: Option<i64>) -> Result<Usage, CloudError> {
        let body = serde_json::json!({ "monthly_limit_cents": cents });
        let path = "/v1/usage/limit";
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::PUT,
                path,
                Some(&body),
                MANAGE_TIMEOUT,
                false,
                &mut sent,
            )
            .await?;
        Self::read_usage(raw)
    }

    async fn usage_request(
        &self,
        method: reqwest::Method,
        body: Option<&serde_json::Value>,
    ) -> Result<Usage, CloudError> {
        let mut sent = false;
        let raw = self
            .authed(method, "/v1/usage", body, MANAGE_TIMEOUT, true, &mut sent)
            .await?;
        Self::read_usage(raw)
    }

    fn read_usage(raw: Raw) -> Result<Usage, CloudError> {
        match raw.status {
            200 => serde_json::from_slice(&raw.body)
                .map_err(|_| CloudError::Unavailable("the reply was not understood".into())),
            status => Err(manage_error(status)),
        }
    }

    pub async fn sync_now(&self, id: &str) -> Result<(), CloudError> {
        check_id(id)?;
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::POST,
                &format!("/v1/connections/{id}/sync"),
                None,
                MANAGE_TIMEOUT,
                false,
                &mut sent,
            )
            .await?;
        match raw.status {
            200 | 202 | 204 => Ok(()),
            status => Err(manage_error(status)),
        }
    }

    async fn id_token(&self, settings: &Settings) -> Result<String, CloudError> {
        if let Some(token) = &settings.dev_token {
            return Ok(token.clone());
        }
        let current = self.tokens.lock().unwrap().clone();
        match current {
            None => Err(CloudError::SignedOut),
            Some(tokens) if tokens.expires_at > Utc::now().timestamp() + 60 => Ok(tokens.id_token),
            Some(_) => self.refresh(settings).await,
        }
    }

    // ---- Sign-in (Cognito hosted UI, authorization code with PKCE) ----

    fn cognito_base(&self, settings: &Settings) -> Result<String, CloudError> {
        let domain = settings
            .cognito_domain
            .as_deref()
            .ok_or(CloudError::NotConfigured)?;
        let base = if domain.contains("://") {
            domain.to_string()
        } else {
            format!("https://{domain}")
        };
        check_base(&base, cfg!(debug_assertions))
    }

    async fn token_request(
        &self,
        settings: &Settings,
        form: &[(&str, &str)],
    ) -> Result<serde_json::Value, CloudError> {
        let base = self.cognito_base(settings)?;
        let response = self
            .http
            .post(format!("{base}/oauth2/token"))
            .timeout(Duration::from_secs(15))
            .form(form)
            .send()
            .await
            .map_err(|_| {
                CloudError::Unavailable("the sign-in service could not be reached".into())
            })?;
        let status = response.status();
        if status.as_u16() == 400 || status.as_u16() == 401 {
            return Err(CloudError::SignedOut);
        }
        if !status.is_success() {
            return Err(CloudError::Unavailable(
                "the sign-in service reported a problem".into(),
            ));
        }
        response
            .json()
            .await
            .map_err(|_| CloudError::Unavailable("the sign-in reply was not understood".into()))
    }

    async fn refresh(&self, settings: &Settings) -> Result<String, CloudError> {
        let old = self
            .tokens
            .lock()
            .unwrap()
            .clone()
            .ok_or(CloudError::SignedOut)?;
        let (Some(refresh), Some(client)) = (
            old.refresh_token.clone(),
            settings.cognito_client_id.clone(),
        ) else {
            self.sign_out();
            return Err(CloudError::SignedOut);
        };
        let reply = self
            .token_request(
                settings,
                &[
                    ("grant_type", "refresh_token"),
                    ("client_id", &client),
                    ("refresh_token", &refresh),
                ],
            )
            .await
            .inspect_err(|error| {
                if *error == CloudError::SignedOut {
                    self.sign_out();
                }
            })?;
        let id_token = reply["id_token"]
            .as_str()
            .ok_or(CloudError::SignedOut)?
            .to_string();
        let tokens = Tokens {
            id_token: id_token.clone(),
            // Cognito does not return a new refresh token when refreshing, so keep the old one.
            refresh_token: reply["refresh_token"]
                .as_str()
                .map(str::to_string)
                .or(Some(refresh)),
            expires_at: Utc::now().timestamp() + reply["expires_in"].as_i64().unwrap_or(3600),
        };
        save_tokens(&self.dir, &tokens)?;
        *self.tokens.lock().unwrap() = Some(tokens);
        Ok(id_token)
    }

    /// Signs in through the browser. `open` is given the sign-in address to show the person. The
    /// browser comes back to a one-shot listener on this machine, which accepts exactly one
    /// callback carrying the state value generated here.
    pub async fn sign_in(&self, open: impl FnOnce(&str)) -> Result<(), CloudError> {
        let settings = self.settings();
        let client = settings
            .cognito_client_id
            .clone()
            .ok_or(CloudError::NotConfigured)?;
        let base = self.cognito_base(&settings)?;

        let mut random = [0u8; 48];
        rand::fill(&mut random);
        let verifier = URL_SAFE_NO_PAD.encode(random);
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let mut state_bytes = [0u8; 24];
        rand::fill(&mut state_bytes);
        let state = URL_SAFE_NO_PAD.encode(state_bytes);

        let listener = TcpListener::bind(("127.0.0.1", self.redirect_port))
            .await
            .map_err(|_| CloudError::Unavailable("the sign-in port is already in use".into()))?;
        let redirect = format!(
            "http://127.0.0.1:{}/callback",
            listener.local_addr().unwrap().port()
        );
        let mut url = reqwest::Url::parse(&format!("{base}/oauth2/authorize"))
            .map_err(|_| CloudError::NotConfigured)?;
        url.query_pairs_mut()
            .append_pair("client_id", &client)
            .append_pair("response_type", "code")
            .append_pair("scope", "openid email profile")
            .append_pair("redirect_uri", &redirect)
            .append_pair("code_challenge", &challenge)
            .append_pair("code_challenge_method", "S256")
            .append_pair("state", &state);
        open(url.as_str());

        let code = tokio::time::timeout(SIGN_IN_TIMEOUT, wait_for_callback(&listener, &state))
            .await
            .map_err(|_| CloudError::Unavailable("sign-in took too long".into()))??;
        let reply = self
            .token_request(
                &settings,
                &[
                    ("grant_type", "authorization_code"),
                    ("client_id", &client),
                    ("code", &code),
                    ("redirect_uri", &redirect),
                    ("code_verifier", &verifier),
                ],
            )
            .await?;
        let id_token = reply["id_token"]
            .as_str()
            .ok_or_else(|| CloudError::Unavailable("the sign-in returned no identity".into()))?
            .to_string();
        let tokens = Tokens {
            id_token,
            refresh_token: reply["refresh_token"].as_str().map(str::to_string),
            expires_at: Utc::now().timestamp() + reply["expires_in"].as_i64().unwrap_or(3600),
        };
        save_tokens(&self.dir, &tokens)?;
        *self.tokens.lock().unwrap() = Some(tokens);
        Ok(())
    }
}

/// Waits for the browser to arrive with the right state. Anything else (a favicon request, a
/// wrong state from some other page) is answered and ignored.
async fn wait_for_callback(listener: &TcpListener, state: &str) -> Result<String, CloudError> {
    loop {
        let (mut socket, _) = listener
            .accept()
            .await
            .map_err(|_| CloudError::Unavailable("the sign-in listener failed".into()))?;
        let mut buffer = vec![0u8; 8192];
        let read = tokio::time::timeout(Duration::from_secs(5), socket.read(&mut buffer))
            .await
            .ok()
            .and_then(Result::ok)
            .unwrap_or(0);
        let request = String::from_utf8_lossy(&buffer[..read]).to_string();
        let target = request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .unwrap_or("");
        let parsed = reqwest::Url::parse(&format!("http://127.0.0.1{target}")).ok();
        let pair = |name: &str| {
            parsed.as_ref().and_then(|url| {
                url.query_pairs()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.to_string())
            })
        };
        let valid = parsed.as_ref().is_some_and(|url| url.path() == "/callback")
            && pair("state").as_deref() == Some(state);
        let (status, text) = if valid && pair("code").is_some() {
            (
                "200 OK",
                "Signed in. You can close this window and return to the app.",
            )
        } else {
            ("400 Bad Request", "This sign-in link is not valid.")
        };
        let body = format!("<!doctype html><meta charset=utf-8><body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\"><p>{text}</p>");
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(reply.as_bytes()).await;
        let _ = socket.shutdown().await;
        if valid {
            if let Some(code) = pair("code") {
                return Ok(code);
            }
        }
    }
}

/// The server names the page that opens a file. A server that has been tampered with could name
/// something else (a local file, a look-alike site), so only web pages of the two supported
/// services are accepted, both when results arrive and again when the address is opened.
pub fn check_document_link(url: &str) -> Result<(), CloudError> {
    let refuse = || CloudError::Unavailable("the document address was not recognised".into());
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse())?;
    let host = parsed.host_str().unwrap_or("");
    let known = matches!(
        host,
        "drive.google.com" | "docs.google.com" | "onedrive.live.com" | "1drv.ms"
    ) || host.ends_with(".sharepoint.com");
    if parsed.scheme() == "https" && parsed.username().is_empty() && known {
        Ok(())
    } else {
        Err(refuse())
    }
}

/// Opens an address in the person's default browser.
pub fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Helpers shared by tests in this module and in `commands`.
#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    pub struct Request {
        pub method: String,
        pub path: String,
        pub auth: Option<String>,
        pub body: String,
    }

    /// A throwaway HTTP server. The handler returns (status, body, delay in ms).
    pub async fn serve(
        handler: impl Fn(&Request) -> (u16, String, u64) + Send + Sync + 'static,
    ) -> (String, Arc<StdMutex<Vec<String>>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let log = Arc::new(StdMutex::new(Vec::new()));
        let (handler, seen) = (Arc::new(handler), log.clone());
        tokio::spawn(async move {
            loop {
                let Ok((mut socket, _)) = listener.accept().await else {
                    return;
                };
                let (handler, seen) = (handler.clone(), seen.clone());
                tokio::spawn(async move {
                    let mut data = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let (head_end, length) = loop {
                        let n = socket.read(&mut chunk).await.unwrap_or(0);
                        if n == 0 {
                            return;
                        }
                        data.extend_from_slice(&chunk[..n]);
                        let text = String::from_utf8_lossy(&data).to_string();
                        if let Some(end) = text.find("\r\n\r\n") {
                            let length = text[..end]
                                .lines()
                                .find_map(|l| {
                                    l.to_lowercase()
                                        .strip_prefix("content-length:")
                                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                                })
                                .unwrap_or(0);
                            if data.len() >= end + 4 + length {
                                break (end, length);
                            }
                        }
                    };
                    let text = String::from_utf8_lossy(&data).to_string();
                    let mut first = text.lines().next().unwrap().split_whitespace();
                    let request = Request {
                        method: first.next().unwrap().into(),
                        path: first.next().unwrap().into(),
                        auth: text.lines().find_map(|l| {
                            l.to_lowercase()
                                .starts_with("authorization:")
                                .then(|| l[14..].trim().to_string())
                        }),
                        body: text[head_end + 4..head_end + 4 + length].to_string(),
                    };
                    seen.lock()
                        .unwrap()
                        .push(format!("{} {}", request.method, request.path));
                    let (status, body, delay) = handler(&request);
                    tokio::time::sleep(Duration::from_millis(delay)).await;
                    let reply = format!(
                        "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = socket.write_all(reply.as_bytes()).await;
                    let _ = socket.shutdown().await;
                });
            }
        });
        (base, log)
    }

    /// A client for the server at `api`, already signed in, with a short timeout.
    pub fn signed_in_cloud(dir: &Path, api: &str) -> Cloud {
        let stored = Stored {
            api_base: Some(api.into()),
            enabled: Some(true),
            ..Default::default()
        };
        std::fs::write(
            dir.join(SETTINGS_FILE),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();
        let mut cloud = Cloud::load(dir);
        cloud.settings.lock().unwrap().dev_token = Some("test-token".into());
        cloud.search_timeout = Duration::from_millis(400);
        cloud
    }
}

#[cfg(test)]
mod tests {
    use super::testing::serve;
    use super::*;
    use std::sync::{Arc, Mutex as StdMutex};

    fn cloud_for(dir: &Path, api: &str, cognito: Option<&str>) -> Cloud {
        let stored = Stored {
            api_base: Some(api.into()),
            cognito_domain: cognito.map(str::to_string),
            cognito_client_id: cognito.map(|_| "client-1".into()),
            enabled: Some(true),
        };
        std::fs::write(
            dir.join(SETTINGS_FILE),
            serde_json::to_string(&stored).unwrap(),
        )
        .unwrap();
        let mut cloud = Cloud::load(dir);
        cloud.settings.lock().unwrap().dev_token = None;
        cloud.redirect_port = 0;
        cloud.search_timeout = Duration::from_millis(400);
        cloud
    }

    fn sign_in_tokens(cloud: &Cloud, id_token: &str, refresh: Option<&str>, lifetime: i64) {
        let tokens = Tokens {
            id_token: id_token.into(),
            refresh_token: refresh.map(str::to_string),
            expires_at: Utc::now().timestamp() + lifetime,
        };
        save_tokens(&cloud.dir, &tokens).unwrap();
        *cloud.tokens.lock().unwrap() = Some(tokens);
    }

    /// What the real server returned in backend/scripts/smoke.sh.
    const REAL_REPLY: &str = r#"{"results":[{"citation_id":"6a419cce-0ea4-429c-b13e-0dd800d11b41","document_id":"ceddd06a-a195-449f-866a-f39238481f7b","document_name":"Retainer.txt","text":"The retainer is due within thirty days.","score":0.05}]}"#;

    #[tokio::test]
    async fn search_sends_the_question_with_the_users_token_and_maps_the_reply() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|request| {
            assert_eq!(request.auth.as_deref(), Some("Bearer token-1"));
            let body: serde_json::Value = serde_json::from_str(&request.body).unwrap();
            assert_eq!(
                (body["query"].as_str(), body["limit"].as_u64()),
                (Some("when is the retainer due"), Some(8))
            );
            (200, REAL_REPLY.into(), 0)
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "token-1", None, 3600);

        let (result, sent) = cloud.search_tracked("when is the retainer due", 8).await;
        let hits = result.unwrap();
        assert!(sent);
        assert_eq!(log.lock().unwrap().as_slice(), ["POST /v1/search"]);
        let hit = &hits[0];
        assert_eq!(
            (hit.origin, hit.document_name.as_str()),
            (Origin::Cloud, "Retainer.txt")
        );
        assert!(hit.citation_id.starts_with("cloud:") && hit.document_id.starts_with("cloud:"));
        assert_eq!(hit.excerpt, "The retainer is due within thirty days.");
    }

    #[tokio::test]
    async fn nothing_is_sent_when_the_cloud_is_off_unconfigured_or_signed_out() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|_| (200, REAL_REPLY.into(), 0)).await;
        let cloud = cloud_for(dir.path(), &api, None);
        let (result, sent) = cloud.search_tracked("q", 5).await;
        assert_eq!((result.unwrap_err(), sent), (CloudError::SignedOut, false));
        sign_in_tokens(&cloud, "t", None, 3600);
        cloud.set_enabled(false).unwrap();
        let (result, sent) = cloud.search_tracked("q", 5).await;
        assert_eq!((result.unwrap_err(), sent), (CloudError::Off, false));
        assert_eq!(cloud.status().state, CloudState::Off);

        let unconfigured = Cloud::load(tempfile::tempdir().unwrap().path());
        let mut settings = unconfigured.settings.lock().unwrap().clone();
        settings.api_base = None;
        *unconfigured.settings.lock().unwrap() = settings;
        assert_eq!(
            unconfigured.search("q", 5).await.unwrap_err(),
            CloudError::NotConfigured
        );
        assert!(
            log.lock().unwrap().is_empty(),
            "no request may leave when the cloud is not usable"
        );
        // The switch survives a restart.
        assert_eq!(Cloud::load(dir.path()).status().state, CloudState::Off);
    }

    #[tokio::test]
    async fn an_expired_token_is_refreshed_once_and_the_search_retried() {
        let dir = tempfile::tempdir().unwrap();
        let (idp, idp_log) = serve(|request| {
            assert!(
                request.body.contains("grant_type=refresh_token")
                    && request.body.contains("refresh_token=refresh-1")
            );
            (200, r#"{"id_token":"token-2","expires_in":3600}"#.into(), 0)
        })
        .await;
        let (api, _) = serve(|request| match request.auth.as_deref() {
            Some("Bearer token-2") => (200, REAL_REPLY.into(), 0),
            _ => (401, "{}".into(), 0),
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, Some(&idp));
        // Expired locally, so the refresh happens before the first request.
        sign_in_tokens(&cloud, "token-1", Some("refresh-1"), -10);
        assert_eq!(cloud.search("q", 5).await.unwrap().len(), 1);
        assert_eq!(idp_log.lock().unwrap().len(), 1);
        // The new token and the old refresh token were saved.
        let saved = load_tokens(dir.path()).unwrap();
        assert_eq!(
            (saved.id_token.as_str(), saved.refresh_token.as_deref()),
            ("token-2", Some("refresh-1"))
        );

        // A token the server rejects although it looks fresh: one refresh, then success.
        sign_in_tokens(&cloud, "stale", Some("refresh-1"), 3600);
        assert_eq!(cloud.search("q", 5).await.unwrap().len(), 1);
        assert_eq!(idp_log.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_token_that_cannot_be_refreshed_signs_the_person_out() {
        let dir = tempfile::tempdir().unwrap();
        let (idp, _) = serve(|_| (400, r#"{"error":"invalid_grant"}"#.into(), 0)).await;
        let (api, _) = serve(|_| (401, "{}".into(), 0)).await;
        let cloud = cloud_for(dir.path(), &api, Some(&idp));
        sign_in_tokens(&cloud, "old", Some("revoked"), 3600);
        let (result, sent) = cloud.search_tracked("q", 5).await;
        assert_eq!(
            (result.unwrap_err(), sent),
            (CloudError::SignedOut, true),
            "the rejected request did carry the question"
        );
        assert!(!cloud.status().signed_in);
        assert!(
            load_tokens(dir.path()).is_none(),
            "the saved sign-in is removed"
        );
    }

    #[tokio::test]
    async fn server_trouble_never_becomes_an_answer_or_leaks_details() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = serve(|request| match request.path.as_str() {
            "/v1/search" => (500, "internal secret detail".into(), 0),
            _ => (404, "{}".into(), 0),
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let error = cloud.search("q", 5).await.unwrap_err();
        assert!(matches!(&error, CloudError::Unavailable(_)));
        assert!(!error.message().contains("secret"), "{}", error.message());

        // Too slow, not reachable, nonsense reply, forbidden.
        let (slow, _) = serve(|_| (200, REAL_REPLY.into(), 1500)).await;
        let cloud = cloud_for(dir.path(), &slow, None);
        assert!(
            matches!(cloud.search("q", 5).await, Err(CloudError::Unavailable(m)) if m.contains("too long"))
        );
        let cloud = cloud_for(dir.path(), "http://127.0.0.1:9", None);
        let (result, sent) = cloud.search_tracked("q", 5).await;
        assert!(matches!(result, Err(CloudError::Unavailable(m)) if m.contains("reached")));
        assert!(
            !sent,
            "nothing left the device when the connection never opened"
        );
        let (junk, _) = serve(|_| (200, "not json".into(), 0)).await;
        assert!(matches!(
            cloud_for(dir.path(), &junk, None).search("q", 5).await,
            Err(CloudError::Unavailable(_))
        ));
        let (denied, _) = serve(|_| (403, "{}".into(), 0)).await;
        assert!(
            matches!(cloud_for(dir.path(), &denied, None).search("q", 5).await, Err(CloudError::Unavailable(m)) if m.contains("may not"))
        );
    }

    #[test]
    fn only_https_is_accepted_except_plain_http_to_this_machine_in_development() {
        assert!(check_base("https://api.example.com/", false).is_ok());
        assert_eq!(
            check_base("https://api.example.com/", false).unwrap(),
            "https://api.example.com"
        );
        assert!(check_base("http://api.example.com", false).is_err());
        assert!(
            check_base("http://api.example.com", true).is_err(),
            "plain http to another host is never allowed"
        );
        assert!(check_base("http://localhost:8080", false).is_err());
        assert!(check_base("http://127.0.0.1:8080", true).is_ok());
        assert!(check_base("https://api.example.com?x=1", false).is_err());
        assert!(check_base("ftp://x", true).is_err());
        assert!(check_base("not a url", true).is_err());
    }

    #[tokio::test]
    async fn signing_in_uses_pkce_ignores_wrong_callbacks_and_remembers_the_person() {
        let dir = tempfile::tempdir().unwrap();
        let challenge_seen = Arc::new(StdMutex::new(String::new()));
        let seen = challenge_seen.clone();
        let (idp, _) = serve(move |request| {
            assert_eq!(request.path, "/oauth2/token");
            let form: std::collections::HashMap<String, String> =
                reqwest::Url::parse(&format!("http://x/?{}", request.body))
                    .unwrap()
                    .query_pairs()
                    .into_owned()
                    .collect();
            assert_eq!(form["grant_type"], "authorization_code");
            assert_eq!(form["code"], "good-code");
            // The verifier sent now must hash to the challenge sent when sign-in started.
            let hashed = URL_SAFE_NO_PAD.encode(Sha256::digest(form["code_verifier"].as_bytes()));
            assert_eq!(hashed, *seen.lock().unwrap());
            (
                200,
                r#"{"id_token":"id-1","refresh_token":"refresh-1","expires_in":3600}"#.into(),
                0,
            )
        })
        .await;
        let cloud = cloud_for(dir.path(), "https://api.example.com", Some(&idp));
        assert_eq!(cloud.status().state, CloudState::SignedOut);

        let browser = tokio::spawn({
            let challenge_seen = challenge_seen.clone();
            async move { challenge_seen }
        });
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let tx = StdMutex::new(Some(tx));
        let result = tokio::spawn({
            let cloud = Arc::new(cloud);
            let handle = cloud.clone();
            async move {
                let outcome = handle
                    .sign_in(|url| {
                        tx.lock()
                            .unwrap()
                            .take()
                            .unwrap()
                            .send(url.to_string())
                            .unwrap();
                    })
                    .await;
                (outcome, cloud)
            }
        });
        let url = reqwest::Url::parse(&rx.await.unwrap()).unwrap();
        let get = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .unwrap()
                .1
                .to_string()
        };
        assert_eq!(
            (
                get("code_challenge_method").as_str(),
                get("response_type").as_str()
            ),
            ("S256", "code")
        );
        assert_eq!(get("client_id"), "client-1");
        *browser.await.unwrap().lock().unwrap() = get("code_challenge");

        let redirect = get("redirect_uri");
        let http = reqwest::Client::new();
        // A callback with the wrong state, and a stray request, must not complete the sign-in.
        let wrong = http
            .get(format!("{redirect}?code=evil&state=wrong"))
            .send()
            .await
            .unwrap();
        assert_eq!(wrong.status().as_u16(), 400);
        let stray = http
            .get(redirect.replace("/callback", "/favicon.ico"))
            .send()
            .await
            .unwrap();
        assert_eq!(stray.status().as_u16(), 400);
        let good = http
            .get(format!("{redirect}?code=good-code&state={}", get("state")))
            .send()
            .await
            .unwrap();
        assert_eq!(good.status().as_u16(), 200);

        let (outcome, cloud) = result.await.unwrap();
        outcome.unwrap();
        assert_eq!(cloud.status().state, CloudState::Ready);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(TOKENS_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600);
        }
        assert!(
            Cloud::load(dir.path()).status().signed_in,
            "the sign-in survives a restart"
        );
        cloud.sign_out();
        assert!(!Cloud::load(dir.path()).status().signed_in);
    }

    /// Runs against a real server, to check that this client and the backend agree on the wire
    /// format. Start one with backend/scripts/dev-connect.sh (it writes backend/.env.devcloud), then:
    ///   PRIVATE_AI_CLOUD_API=... PRIVATE_AI_CLOUD_DEV_TOKEN=... cargo test live_server -- --ignored
    #[tokio::test]
    #[ignore]
    async fn live_server_returns_passages() {
        let dir = tempfile::tempdir().unwrap();
        let cloud = Cloud::load(dir.path());
        let query = std::env::var("PRIVATE_AI_LIVE_QUERY").unwrap_or_else(|_| "retainer".into());
        let (result, sent) = cloud.search_tracked(&query, 5).await;
        let hits = result.expect("the live server should answer");
        assert!(
            sent && !hits.is_empty(),
            "expected at least one passage for {query:?}"
        );
        assert!(hits
            .iter()
            .all(|hit| hit.origin == Origin::Cloud && hit.citation_id.starts_with("cloud:")));
    }

    #[tokio::test]
    async fn the_page_or_slide_the_server_reports_appears_in_the_citation() {
        let dir = tempfile::tempdir().unwrap();
        let reply = r#"{"results":[
            {"citation_id":"a","document_id":"d","document_name":"Terms.pdf","text":"Interest accrues.","score":0.3,"location_label":"Page 2"},
            {"citation_id":"b","document_id":"e","document_name":"Notes.txt","text":"Plain text.","score":0.2,"location_label":null},
            {"citation_id":"c","document_id":"f","document_name":"Old.txt","text":"Older server.","score":0.1}
        ]}"#;
        let (api, _) = serve(move |_| (200, reply.into(), 0)).await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let hits = cloud.search("q", 5).await.unwrap();
        let labels: Vec<_> = hits.iter().map(|h| h.location_label.as_str()).collect();
        assert_eq!(labels, ["Cloud · Page 2", "Cloud", "Cloud"]);
    }

    const CATALOGUE: &str = r#"{"connectors":[
        {"kind":"google_drive","name":"Google Drive","description":"Docs and PDFs.","available":true},
        {"kind":"microsoft365","name":"Microsoft 365","description":"OneDrive and SharePoint.","available":false}]}"#;
    const LISTING: &str = r#"{"connections":[
        {"id":"6a419cce-0ea4-429c-b13e-0dd800d11b41","kind":"google_drive","status":"active","last_sync_epoch":1793000000.5,"last_error":null,"documents":954},
        {"id":"ceddd06a-a195-449f-866a-f39238481f7b","kind":"microsoft365","status":"needs_reauth","last_sync_epoch":null,"last_error":"Reconnect required","documents":0}]}"#;

    #[tokio::test]
    async fn the_connector_list_and_connected_sources_are_read_from_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|request| {
            assert_eq!(request.auth.as_deref(), Some("Bearer token-1"));
            match request.path.as_str() {
                "/v1/connectors" => (200, CATALOGUE.into(), 0),
                "/v1/connections" => (200, LISTING.into(), 0),
                _ => (404, "{}".into(), 0),
            }
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "token-1", None, 3600);
        // Managing sources does not depend on the search switch.
        cloud.set_enabled(false).unwrap();

        let view = cloud.connectors_view().await.unwrap();
        assert_eq!(
            log.lock().unwrap().as_slice(),
            ["GET /v1/connectors", "GET /v1/connections"]
        );
        assert_eq!(view.connectors.len(), 2);
        assert!(view.connectors[0].available && !view.connectors[1].available);
        assert_eq!(view.connections[0].documents, 954);
        assert_eq!(view.connections[1].status, "needs_reauth");
        assert_eq!(
            view.connections[1].last_error.as_deref(),
            Some("Reconnect required")
        );
        assert_eq!(view.connections[0].last_sync_epoch, Some(1793000000.5));
    }

    #[tokio::test]
    async fn starting_a_connection_returns_the_provider_sign_in_and_refuses_any_other_address() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = Arc::new(StdMutex::new(Vec::<String>::new()));
        let seen = bodies.clone();
        let reply = Arc::new(StdMutex::new(
            r#"{"authorization_url":"https://accounts.google.com/o/oauth2/v2/auth?client_id=x&state=y"}"#.to_string(),
        ));
        let to_send = reply.clone();
        let (api, _) = serve(move |request| {
            assert_eq!(
                (request.method.as_str(), request.path.as_str()),
                ("POST", "/v1/connections")
            );
            seen.lock().unwrap().push(request.body.clone());
            (200, to_send.lock().unwrap().clone(), 0)
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);

        let url = cloud
            .start_connection(
                "microsoft365",
                &["firm.com".to_string()],
                Some("6a419cce-0ea4-429c-b13e-0dd800d11b41"),
            )
            .await
            .unwrap();
        assert!(url.starts_with("https://accounts.google.com/"));
        let body: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
        assert_eq!(body["kind"], "microsoft365");
        assert_eq!(body["domains"][0], "firm.com");
        assert_eq!(body["reconnect"], "6a419cce-0ea4-429c-b13e-0dd800d11b41");

        // A reply that points the browser anywhere else is refused, not opened.
        for evil in [
            "https://evil.example/login",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "ms-msdt:/id PCWDiagnostic",
            "http://accounts.google.com/o/oauth2",
            "https://accounts.google.com.evil.example/",
            "not a url",
        ] {
            *reply.lock().unwrap() = format!(r#"{{"authorization_url":"{evil}"}}"#);
            let result = cloud.start_connection("google_drive", &[], None).await;
            assert!(
                matches!(result, Err(CloudError::Unavailable(_))),
                "{evil}: {result:?}"
            );
        }
    }

    #[test]
    fn only_provider_sign_in_pages_or_this_machine_may_be_opened_in_the_browser() {
        assert!(check_browser_url(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?x=1",
            false
        )
        .is_ok());
        assert!(check_browser_url("http://127.0.0.1:8080/x", false).is_err());
        assert!(check_browser_url("http://127.0.0.1:8080/x", true).is_ok());
        assert!(check_browser_url("http://localhost.evil.example/", true).is_err());
        assert!(
            check_browser_url("https://login.microsoftonline.com@evil.example/", false).is_err()
        );
    }

    #[test]
    fn only_web_pages_of_the_supported_services_may_be_opened_as_documents() {
        for good in [
            "https://drive.google.com/open?id=abc",
            "https://docs.google.com/document/d/abc/edit",
            "https://firm.sharepoint.com/sites/team/Shared%20Documents/Plan.docx",
            "https://firm-my.sharepoint.com/personal/a_firm_com/Documents/Plan.docx",
            "https://onedrive.live.com/redir?resid=1",
        ] {
            assert!(check_document_link(good).is_ok(), "{good}");
        }
        for bad in [
            "http://drive.google.com/open?id=abc",
            "file:///Users/me/secret.txt",
            "javascript:alert(1)",
            "https://drive.google.com.evil.example/open",
            "https://evil.example/firm.sharepoint.com",
            "https://sharepoint.com.evil.example/x",
            "https://drive.google.com@evil.example/",
            "not a url",
        ] {
            assert!(check_document_link(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_result_keeps_its_link_only_when_the_address_is_acceptable() {
        let passage = |link: &str| Passage {
            location_label: None,
            citation_id: "c".into(),
            document_id: "d".into(),
            document_name: "Plan".into(),
            text: "text".into(),
            score: 1.0,
            link: Some(link.into()),
        };
        let kept = passage("https://drive.google.com/open?id=abc").into_hit();
        assert_eq!(
            kept.link.as_deref(),
            Some("https://drive.google.com/open?id=abc")
        );
        assert_eq!(passage("file:///etc/passwd").into_hit().link, None);
    }

    #[tokio::test]
    async fn disconnecting_and_syncing_use_the_right_requests_and_refuse_odd_ids() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(
            |request| match (request.method.as_str(), request.path.as_str()) {
                ("DELETE", p) if p.starts_with("/v1/connections/") => (204, String::new(), 0),
                ("POST", p) if p.ends_with("/sync") => (202, "{}".into(), 0),
                _ => (404, "{}".into(), 0),
            },
        )
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let id = "6a419cce-0ea4-429c-b13e-0dd800d11b41";
        cloud.disconnect(id).await.unwrap();
        cloud.sync_now(id).await.unwrap();
        assert_eq!(
            log.lock().unwrap().as_slice(),
            [
                format!("DELETE /v1/connections/{id}"),
                format!("POST /v1/connections/{id}/sync")
            ]
        );
        // An ID that could change the address is never sent.
        for bad in [
            "../v1/search",
            "x/../../y",
            "",
            "6a419cce/0ea4",
            "id with spaces",
        ] {
            assert!(cloud.disconnect(bad).await.is_err(), "{bad}");
            assert!(cloud.sync_now(bad).await.is_err(), "{bad}");
        }
        assert_eq!(log.lock().unwrap().len(), 2, "nothing else was sent");
    }

    #[tokio::test]
    async fn the_connections_list_carries_sync_progress_and_name_only_reasons() {
        let dir = tempfile::tempdir().unwrap();
        let id = "6a419cce-0ea4-429c-b13e-0dd800d11b41";
        let (api, _) = serve(move |request| match request.path.as_str() {
            "/v1/connectors" => (200, r#"{"connectors":[]}"#.into(), 0),
            "/v1/connections" => (
                200,
                format!(
                    r#"{{"connections":[{{"id":"{id}","kind":"google_drive","status":"active","last_sync_epoch":null,"last_error":null,"documents":900,"syncing":true,"sync_started_epoch":1000.0,"sync_files_read":420,"sync_files_failed":2,"next_sync_epoch":null,"readable":540,"name_only_reasons":{{"no_text":300,"unsupported_type":60}}}}]}}"#
                ),
                0,
            ),
            p if p.ends_with("/files") => (
                200,
                r#"{"files":[{"name":"Scan.pdf","reason":"no_text","mime_type":"application/pdf"}]}"#.into(),
                0,
            ),
            _ => (404, "{}".into(), 0),
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let view = cloud.connectors_view().await.unwrap();
        let connection = &view.connections[0];
        assert!(connection.syncing);
        assert_eq!(
            (connection.sync_files_read, connection.readable),
            (420, 540)
        );
        assert_eq!(connection.name_only_reasons["no_text"], 300);
        let files = cloud.connection_files(id).await.unwrap();
        assert_eq!(files[0].name, "Scan.pdf");
        assert!(cloud.connection_files("../x").await.is_err());
    }

    #[tokio::test]
    async fn company_knowledge_requests_are_encoded_and_answers_passed_on() {
        let dir = tempfile::tempdir().unwrap();
        let (api, log) = serve(|request| match request.path.split('?').next().unwrap() {
            "/v1/knowledge/facts" => (
                200,
                r#"{"facts":[{"id":"x","statement":"The price is $18,400."}]}"#.into(),
                0,
            ),
            "/v1/knowledge/missing" => (404, "{}".into(), 0),
            "/v1/knowledge/closed" => (403, "{}".into(), 0),
            _ => (500, "{}".into(), 0),
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let reply = cloud
            .knowledge(
                reqwest::Method::GET,
                "/v1/knowledge/facts",
                &[("q", "late fee & 50%"), ("status", ""), ("kind", "policy")],
                None,
            )
            .await
            .unwrap();
        assert_eq!(reply["facts"][0]["statement"], "The price is $18,400.");
        // Text from the person is percent-encoded, and an empty filter is left out.
        let first = log.lock().unwrap()[0].clone();
        assert_eq!(
            first,
            "GET /v1/knowledge/facts?q=late+fee+%26+50%25&kind=policy"
        );

        let gone = cloud
            .knowledge(reqwest::Method::GET, "/v1/knowledge/missing", &[], None)
            .await;
        assert!(gone.unwrap_err().message().contains("no longer exists"));
        let closed = cloud
            .knowledge(reqwest::Method::GET, "/v1/knowledge/closed", &[], None)
            .await;
        assert_eq!(closed.unwrap_err(), CloudError::NotAllowed);
    }

    #[tokio::test]
    async fn managing_sources_explains_why_it_was_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (forbidden, _) = serve(|_| (403, "{}".into(), 0)).await;
        let cloud = cloud_for(dir.path(), &forbidden, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let error = cloud
            .start_connection("google_drive", &[], None)
            .await
            .unwrap_err();
        assert_eq!(error, CloudError::NotAllowed);
        assert!(error.message().contains("administrator"));

        let (missing, _) = serve(|_| (404, "{}".into(), 0)).await;
        let cloud = cloud_for(dir.path(), &missing, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let id = "6a419cce-0ea4-429c-b13e-0dd800d11b41";
        assert!(
            matches!(cloud.disconnect(id).await, Err(CloudError::Unavailable(m)) if m.contains("no longer exists"))
        );

        let (unset, _) = serve(|_| (503, r#"{"error":"x"}"#.into(), 0)).await;
        let cloud = cloud_for(dir.path(), &unset, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        assert!(
            matches!(cloud.start_connection("google_drive", &[], None).await, Err(CloudError::Unavailable(m)) if m.contains("not set up"))
        );

        // Signed out: nothing is sent, and the person is told to sign in.
        let (api, log) = serve(|_| (200, "{}".into(), 0)).await;
        let fresh = tempfile::tempdir().unwrap(); // no saved sign-in here
        let signed_out = cloud_for(fresh.path(), &api, None);
        assert_eq!(
            signed_out.connectors_view().await.unwrap_err(),
            CloudError::SignedOut
        );
        assert!(log.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_server_without_the_connector_pages_is_called_out_of_date() {
        let dir = tempfile::tempdir().unwrap();
        let (api, _) = serve(|_| (404, "{}".into(), 0)).await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let error = cloud.connectors_view().await.unwrap_err();
        assert!(
            matches!(&error, CloudError::Unavailable(m) if m.contains("needs updating")),
            "{error:?}"
        );
        assert!(!error.message().contains("no longer exists"));
    }
}
