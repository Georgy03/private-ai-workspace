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

/// Narrows a search of the firm cloud index to some connected sources ("google_drive", ...) and
/// kinds of file ("presentation" is Slides or PowerPoint). Empty means no limit.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchFilter {
    pub sources: Vec<String>,
    pub types: Vec<String>,
    /// Only these documents (the ones pinned to a workspace), by their ID in the firm cloud index.
    pub documents: Vec<String>,
}

const FILE_KINDS: &[&str] = &["document", "spreadsheet", "presentation", "pdf", "image", "other"];

impl SearchFilter {
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty() && self.types.is_empty() && self.documents.is_empty()
    }

    /// The filter the person chose, or why it cannot be used. Nothing unexpected is passed on.
    pub fn checked(sources: Option<Vec<String>>, types: Option<Vec<String>>) -> Result<SearchFilter, String> {
        let sources = sources.unwrap_or_default();
        let types = types.unwrap_or_default();
        let source_ok = |source: &String| !source.is_empty() && source.len() <= 40 && source.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
        if sources.len() > 20 || types.len() > FILE_KINDS.len() || !sources.iter().all(source_ok) || !types.iter().all(|kind| FILE_KINDS.contains(&kind.as_str())) {
            return Err("That filter is not one the firm cloud index understands.".into());
        }
        let mut filter = SearchFilter { sources, types, documents: Vec::new() };
        filter.sources.sort();
        filter.sources.dedup();
        filter.types.sort();
        filter.types.dedup();
        Ok(filter)
    }

    /// For the person: "Google Drive and Dropbox, presentations".
    pub fn describe(&self) -> String {
        let source = |source: &String| match source.as_str() {
            "google_drive" => "Google Drive".to_string(),
            "microsoft365" => "Microsoft 365".to_string(),
            "dropbox" => "Dropbox".to_string(),
            "docusign" => "DocuSign".to_string(),
            "quickbooks" => "QuickBooks".to_string(),
            other => other.replace('_', " "),
        };
        let kind = |kind: &String| match kind.as_str() {
            "document" => "documents",
            "spreadsheet" => "spreadsheets",
            "presentation" => "presentations",
            "pdf" => "PDFs",
            "image" => "images",
            _ => "other files",
        };
        let join = |items: Vec<String>| match items.len() {
            0 | 1 => items.join(""),
            _ => format!("{} and {}", items[..items.len() - 1].join(", "), items[items.len() - 1]),
        };
        let mut parts = Vec::new();
        if !self.sources.is_empty() {
            parts.push(join(self.sources.iter().map(source).collect()));
        }
        if !self.types.is_empty() {
            parts.push(join(self.types.iter().map(|k| kind(k).to_string()).collect()));
        }
        parts.join(", ")
    }
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

/// Debug builds only: what a token claims, without the token or any address, so a sign-in that "works" but is
/// refused can be told apart from one that never finished.
fn describe_token(id_token: &str) -> String {
    let claims: Option<serde_json::Value> = id_token
        .split('.')
        .nth(1)
        .and_then(|part| URL_SAFE_NO_PAD.decode(part).ok())
        .and_then(|bytes| serde_json::from_slice(&bytes).ok());
    let Some(c) = claims else { return "not a readable token".into() };
    format!(
        "token_use={} aud_is_client={} has_firm={} groups={} email_verified={} expires_in={}s iss={}",
        c["token_use"],
        c["aud"].is_string(),
        c["custom:tenant_id"].as_str().is_some_and(|v| !v.is_empty()),
        c["cognito:groups"],
        c["email_verified"],
        c["exp"].as_i64().unwrap_or(0) - Utc::now().timestamp(),
        c["iss"],
    )
}

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
fn workspace() -> String {
    "workspace".into()
}

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
    /// "workspace" (everyone at the firm, each seeing what they may open) or "private" (only the
    /// person who connected it).
    #[serde(default = "workspace")]
    pub scope: String,
    /// This person connected it.
    #[serde(default)]
    pub yours: bool,
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
    /// This person may connect a source for the whole workspace. Anyone may connect one for themselves.
    #[serde(default)]
    pub can_share_with_workspace: bool,
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

/// The reason a server gave for refusing a request: its `error` text, if it is short, plain text.
/// Shown as it is, so it never carries anything but words.
fn server_reason(body: &[u8]) -> Option<String> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    let text = value.get("error")?.as_str()?.trim();
    let plain = !text.is_empty() && text.chars().count() <= 200 && text.chars().all(|c| !c.is_control());
    plain.then(|| text.to_string())
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
/// could send something else (a local file, an app link), so only the sign-in pages of the
/// supported providers are accepted. Development builds also accept this machine.
fn check_browser_url(url: &str, allow_local: bool) -> Result<(), CloudError> {
    let refuse = || CloudError::Unavailable("the sign-in address was not recognised".into());
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse())?;
    let host = parsed.host_str().unwrap_or("");
    let provider = parsed.scheme() == "https"
        && matches!(
            host,
            "accounts.google.com"
                | "login.microsoftonline.com"
                | "www.dropbox.com"
                | "account.docusign.com"
                | "account-d.docusign.com"
                | "appcenter.intuit.com"
                | "zoom.us"
        );
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

    /// Like `search_tracked`, limited to some connected sources and kinds of file.
    pub async fn search_filtered_tracked(
        &self,
        question: &str,
        limit: usize,
        filter: &SearchFilter,
    ) -> (Result<Vec<SearchHit>, CloudError>, bool) {
        let mut sent = false;
        let result = self.search_in(question, limit, None, filter, &mut sent).await;
        (result, sent)
    }

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

    /// Searches only inside the files one of the person's own reports was drawn from, so a question
    /// about that report is answered from its evidence. `task` is the report's ID.
    pub async fn search_report(
        &self,
        task: &str,
        question: &str,
        limit: usize,
    ) -> (Result<Vec<SearchHit>, CloudError>, bool) {
        let mut sent = false;
        let result = self.search_in(question, limit, Some(task), &SearchFilter::default(), &mut sent).await;
        (result, sent)
    }

    async fn search_inner(
        &self,
        question: &str,
        limit: usize,
        sent: &mut bool,
    ) -> Result<Vec<SearchHit>, CloudError> {
        self.search_in(question, limit, None, &SearchFilter::default(), sent).await
    }

    async fn search_in(
        &self,
        question: &str,
        limit: usize,
        task: Option<&str>,
        filter: &SearchFilter,
        sent: &mut bool,
    ) -> Result<Vec<SearchHit>, CloudError> {
        let mut body = serde_json::json!({ "query": question, "limit": limit });
        if let Some(task) = task {
            body["task"] = serde_json::json!(task);
        }
        if !filter.sources.is_empty() {
            body["sources"] = serde_json::json!(filter.sources);
        }
        if !filter.types.is_empty() {
            body["types"] = serde_json::json!(filter.types);
        }
        if !filter.documents.is_empty() {
            body["documents"] = serde_json::json!(filter.documents);
        }
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
                    if cfg!(debug_assertions) {
                        eprintln!("cloud: the server refused the sign-in token (401) on {path}");
                    }
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
            #[serde(default)]
            can_share_with_workspace: bool,
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
        let catalogue = serde_json::from_slice::<Catalogue>(&catalogue.body).map_err(|_| not_understood())?;
        Ok(ConnectorsView {
            connectors: catalogue.connectors,
            connections: serde_json::from_slice::<Listing>(&listing.body)
                .map_err(|_| not_understood())?
                .connections,
            can_share_with_workspace: catalogue.can_share_with_workspace,
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
        scope: Option<&str>,
    ) -> Result<String, CloudError> {
        let mut body = serde_json::json!({ "kind": kind });
        if let Some(scope) = scope {
            body["scope"] = serde_json::json!(scope);
        }
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

    /// Changes who a connection's files are for: "private" or "workspace".
    pub async fn set_scope(&self, id: &str, scope: &str) -> Result<(), CloudError> {
        check_id(id)?;
        if !matches!(scope, "private" | "workspace") {
            return Err(CloudError::Unavailable("that is not a valid choice".into()));
        }
        let body = serde_json::json!({ "scope": scope });
        let mut sent = false;
        let raw = self
            .authed(
                reqwest::Method::PUT,
                &format!("/v1/connections/{id}/scope"),
                Some(&body),
                MANAGE_TIMEOUT,
                false,
                &mut sent,
            )
            .await?;
        match raw.status {
            200 => Ok(()),
            status => Err(manage_error(status)),
        }
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
            // The server says why it refused ("connect your Outlook first"): that is what to show.
            400 | 422 => Err(CloudError::Unavailable(
                server_reason(&raw.body).unwrap_or_else(|| "the server did not accept the request".into()),
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
            if cfg!(debug_assertions) {
                // Cognito's refusal is only a code and a sentence ({"error":"invalid_grant"}), never a token.
                let body = response.text().await.unwrap_or_default();
                eprintln!("cloud: the sign-in service refused ({status}): {}", body.chars().take(300).collect::<String>());
            }
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
        self.refresh_from(settings, old).await
    }

    /// Gets a fresh ID token from these tokens' refresh token, and keeps the result as the app's sign-in.
    async fn refresh_from(&self, settings: &Settings, old: Tokens) -> Result<String, CloudError> {
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

    /// Starts a new firm for a person who has none: signs them in with Google (the account becomes theirs), asks the
    /// server to make the firm with them as its administrator, and refreshes so their token carries the firm. One
    /// browser sign-in, start to finish.
    pub async fn create_firm(
        &self,
        name: &str,
        with: &SignInChoice,
        open: impl FnOnce(&str),
    ) -> Result<(), CloudError> {
        // Only the two sign-ins that need no invitation can start a firm.
        if !matches!(with, SignInChoice::Google | SignInChoice::MicrosoftPersonal) {
            return Err(CloudError::Unavailable(
                "a firm can be started with Google or a personal Microsoft account".into(),
            ));
        }
        let name = clean_firm_name(name)?;
        // The sign-in has no firm yet, so it is not kept as the app's session while the firm is made: the rest of the
        // app would be refused with it (and would sign the person out). It is used for the one request that makes
        // the firm, then refreshed into a token that carries the firm.
        let temporary = self.browser_sign_in(with, open).await?;
        if cfg!(debug_assertions) {
            eprintln!("cloud: signed in to start a firm; {}", describe_token(&temporary.id_token));
        }
        self.finish_new_firm(&name, &temporary.id_token).await?;
        let settings = self.settings();
        let token = self.refresh_from(&settings, temporary).await.map_err(|_| {
            CloudError::Unavailable("your firm was made, but signing in to it did not finish. Use Sign in".into())
        })?;
        if cfg!(debug_assertions) {
            eprintln!("cloud: signed in; {}", describe_token(&token));
        }
        Ok(())
    }

    /// The part after sign-in, apart so it can be tried without a browser: asks the server to make the firm, with the
    /// temporary sign-in's token. Nothing is kept either way; the person signs in to the firm afterwards.
    async fn finish_new_firm(&self, name: &str, token: &str) -> Result<(), CloudError> {
        let settings = self.settings();
        let base = settings
            .api_base
            .as_deref()
            .ok_or(CloudError::NotConfigured)?;
        let url = format!("{}/v1/onboarding/firm", check_base(base, cfg!(debug_assertions))?);
        let response = self
            .http
            .post(&url)
            .bearer_auth(token)
            .timeout(MANAGE_TIMEOUT)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .map_err(|error| {
                CloudError::Unavailable(if error.is_timeout() {
                    "it took too long to answer".into()
                } else {
                    "it could not be reached".into()
                })
            })?;
        match response.status().as_u16() {
            200 => Ok(()),
            409 => Err(CloudError::Unavailable(
                "this account already belongs to a firm, or its firm is being made. Use Sign in instead".into(),
            )),
            429 => Err(CloudError::Unavailable(
                "too many firms are being started just now; wait a minute and try again".into(),
            )),
            404 => Err(CloudError::Unavailable(
                "starting a firm from the app is not switched on for this server".into(),
            )),
            401 => Err(CloudError::Unavailable("the server did not accept the sign-in; try again".into())),
            _ => Err(CloudError::Unavailable("your firm could not be made; try again".into())),
        }
    }

    /// Asks the firm's server which Microsoft sign-in belongs to this email. No one is signed in yet, so the
    /// question carries nothing but the email; the answer is only a provider name, checked before it is used.
    async fn microsoft_provider(
        &self,
        settings: &Settings,
        email: &str,
    ) -> Result<Option<String>, CloudError> {
        let base = settings
            .api_base
            .as_deref()
            .ok_or(CloudError::NotConfigured)?;
        let url = format!(
            "{}/v1/sign-in/route",
            check_base(base, cfg!(debug_assertions))?
        );
        let response = self
            .http
            .post(url)
            .timeout(Duration::from_secs(10))
            .json(&serde_json::json!({ "email": email }))
            .send()
            .await
            .map_err(|_| CloudError::Unavailable("the server could not be reached".into()))?;
        let status = response.status().as_u16();
        if status == 429 {
            return Err(CloudError::Unavailable(
                "too many sign-in attempts just now; wait a minute and try again".into(),
            ));
        }
        if !(200..300).contains(&status) {
            return Err(CloudError::Unavailable(
                "the server could not look up your sign-in; try again".into(),
            ));
        }
        let reply: serde_json::Value = response
            .json()
            .await
            .map_err(|_| CloudError::Unavailable("the sign-in lookup was not understood".into()))?;
        match reply["provider"].as_str() {
            Some(name) if valid_microsoft_provider(name) => Ok(Some(name.to_string())),
            Some(_) => Err(CloudError::Unavailable(
                "the sign-in lookup returned something unexpected".into(),
            )),
            None => Ok(None),
        }
    }

    /// Signs in through the browser. `open` is given the sign-in address to show the person. The
    /// browser comes back to a one-shot listener on this machine, which accepts exactly one
    /// callback carrying the state value generated here.
    pub async fn sign_in(
        &self,
        choice: &SignInChoice,
        open: impl FnOnce(&str),
    ) -> Result<(), CloudError> {
        let tokens = self.browser_sign_in(choice, open).await?;
        if cfg!(debug_assertions) {
            eprintln!("cloud: signed in; {}", describe_token(&tokens.id_token));
        }
        save_tokens(&self.dir, &tokens)?;
        *self.tokens.lock().unwrap() = Some(tokens);
        Ok(())
    }

    /// The browser part of signing in, returning the tokens without keeping them.
    async fn browser_sign_in(
        &self,
        choice: &SignInChoice,
        open: impl FnOnce(&str),
    ) -> Result<Tokens, CloudError> {
        let mut routing = choice.fixed_routing();
        let settings = self.settings();
        let client = settings
            .cognito_client_id
            .clone()
            .ok_or(CloudError::NotConfigured)?;
        let base = self.cognito_base(&settings)?;
        if let SignInChoice::Microsoft { email } = choice {
            let email = clean_email(email)?;
            // A firm's own Microsoft sign-in wins. Failing that, an address Microsoft issues itself (outlook.com
            // and the like) is a personal account, which has one shared sign-in.
            let provider = match self.microsoft_provider(&settings, &email).await? {
                Some(provider) => provider,
                None if is_personal_microsoft_address(&email) => PERSONAL_MICROSOFT.to_string(),
                None => {
                    return Err(CloudError::Unavailable(
                        "no Microsoft sign-in was found for that email. Ask your administrator to invite you, or choose another way to sign in".into(),
                    ));
                }
            };
            routing = Some(("identity_provider", provider));
        }

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
        if let Some((name, value)) = routing {
            url.query_pairs_mut().append_pair(name, &value);
        }
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
            .await
            .map_err(|error| match error {
                // Here a refusal is not "signed out": the browser step worked and the token was then refused.
                CloudError::SignedOut => CloudError::Unavailable(
                    "the sign-in service refused to finish the sign-in; try again".into(),
                ),
                other => other,
            })?;
        let id_token = reply["id_token"]
            .as_str()
            .ok_or_else(|| CloudError::Unavailable("the sign-in returned no identity".into()))?
            .to_string();
        let tokens = Tokens {
            id_token,
            refresh_token: reply["refresh_token"].as_str().map(str::to_string),
            expires_at: Utc::now().timestamp() + reply["expires_in"].as_i64().unwrap_or(3600),
        };
        Ok(tokens)
    }
}

/// How the person chose to sign in. Each choice only decides which page Cognito shows first: who may
/// actually get in is still decided by the server (invited people only), so a wrong choice gets nothing.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignInChoice {
    /// Cognito's own page, listing every way in.
    Any,
    Google,
    /// The person's work email: the server finds the invitation and so the firm's own Microsoft sign-in.
    Microsoft { email: String },
    /// A personal Microsoft account (`@outlook.com`, `@hotmail.com`): one shared sign-in, no firm needed first.
    MicrosoftPersonal,
    /// An email address and password (with an authenticator app).
    Password,
}

impl SignInChoice {
    /// The one extra setting that sends the browser straight to a sign-in that needs no lookup.
    fn fixed_routing(&self) -> Option<(&'static str, String)> {
        match self {
            Self::Google => Some(("identity_provider", "Google".into())),
            Self::Password => Some(("identity_provider", "COGNITO".into())),
            Self::MicrosoftPersonal => Some(("identity_provider", PERSONAL_MICROSOFT.into())),
            Self::Any | Self::Microsoft { .. } => None,
        }
    }
}

/// The sign-in provider for personal Microsoft accounts (made by `workspace-cloud add-microsoft-personal`).
const PERSONAL_MICROSOFT: &str = "MicrosoftPersonal";

/// The addresses Microsoft itself issues for personal accounts. Kept the same as the sign-up function's list
/// (infra/presignup/handler.py), which refuses any other address on this provider.
const PERSONAL_MICROSOFT_DOMAINS: &[&str] = &[
    "outlook.com", "hotmail.com", "live.com", "msn.com", "outlook.co.uk", "hotmail.co.uk", "live.co.uk",
    "outlook.ca", "hotmail.ca", "live.ca", "outlook.com.au", "hotmail.com.au", "live.com.au",
    "outlook.de", "hotmail.de", "live.de", "outlook.fr", "hotmail.fr", "live.fr",
    "outlook.it", "hotmail.it", "live.it", "outlook.es", "hotmail.es", "outlook.in", "outlook.ie", "live.ie",
];

fn is_personal_microsoft_address(email: &str) -> bool {
    email
        .rsplit_once('@')
        .is_some_and(|(_, domain)| PERSONAL_MICROSOFT_DOMAINS.contains(&domain))
}

/// A firm name tidied to single spaces, or a plain-language reason it cannot be used.
fn clean_firm_name(text: &str) -> Result<String, CloudError> {
    let name = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() || name.chars().count() > 120 || name.chars().any(char::is_control) {
        return Err(CloudError::Unavailable(
            "give your firm a name of 1 to 120 characters".into(),
        ));
    }
    Ok(name)
}

/// An email address, lowercased, or a plain-language reason it cannot be used.
fn clean_email(email: &str) -> Result<String, CloudError> {
    let bad = || CloudError::Unavailable("enter your work email, like name@yourfirm.com".into());
    let email = email.trim().to_lowercase();
    let (name, host) = email.split_once('@').ok_or_else(bad)?;
    let fine = email.len() <= 254
        && !name.is_empty()
        && host.contains('.')
        && !host.contains('@')
        && email.chars().all(|c| c.is_ascii_graphic());
    if fine {
        Ok(email)
    } else {
        Err(bad())
    }
}

/// A provider name the server may hand back: what `workspace-cloud` makes (`ms-<firm>-<6 hex>`), nothing else,
/// so a wrong or hostile reply cannot add anything to the sign-in address.
fn valid_microsoft_provider(name: &str) -> bool {
    name.strip_prefix("ms-").is_some_and(|rest| {
        !rest.is_empty()
            && rest.len() <= 29
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    })
}

fn html_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
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
        // Cognito sends the person back with `error_description` when sign-in is refused (for example, an email
        // nobody invited). Show that reason, in the browser and in the app, instead of a vague "not valid".
        let refusal = (valid && pair("code").is_none())
            .then(|| pair("error_description").or_else(|| pair("error")))
            .flatten()
            .map(|reason| reason.chars().filter(|c| !c.is_control()).take(200).collect::<String>());
        let (status, text) = if let Some(reason) = &refusal {
            (
                "400 Bad Request",
                format!("Sign-in was not completed: {}", html_escape(reason)),
            )
        } else if valid && pair("code").is_some() {
            (
                "200 OK",
                "Signed in. You can close this window and return to the app.".to_string(),
            )
        } else {
            (
                "400 Bad Request",
                "This sign-in link is not valid.".to_string(),
            )
        };
        let body = format!("<!doctype html><meta charset=utf-8><body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\"><p>{text}</p>");
        let reply = format!(
            "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = socket.write_all(reply.as_bytes()).await;
        let _ = socket.shutdown().await;
        if let Some(reason) = refusal {
            return Err(CloudError::Unavailable(format!(
                "sign-in was refused: {reason}"
            )));
        }
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
        "drive.google.com"
            | "docs.google.com"
            | "onedrive.live.com"
            | "1drv.ms"
            | "www.dropbox.com"
            | "app.docusign.com"
            | "appdemo.docusign.com"
    ) || host.ends_with(".sharepoint.com");
    if parsed.scheme() == "https" && parsed.username().is_empty() && known {
        Ok(())
    } else {
        Err(refuse())
    }
}

/// Addresses that open a person's own mail or calendar with a message or meeting filled in.
/// Only the compose and new-event pages of Gmail, Outlook and Google Calendar, or a plain `mailto:`
/// link: a prepared draft must never be able to open anything else.
pub fn check_prepared_link(url: &str) -> Result<(), CloudError> {
    let refuse = || CloudError::Unavailable("that address cannot be opened".into());
    if url.len() > 8_000 || url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return Err(refuse());
    }
    if let Some(rest) = url.strip_prefix("mailto:") {
        // The recipients, then optional cc, subject and body: nothing that is not part of a draft.
        let query = rest.split_once('?').map_or("", |(_, query)| query);
        let allowed = query.split('&').filter(|pair| !pair.is_empty()).all(|pair| {
            matches!(pair.split_once('=').map_or(pair, |(key, _)| key), "cc" | "subject" | "body")
        });
        return if allowed && !rest.is_empty() { Ok(()) } else { Err(refuse()) };
    }
    let parsed = reqwest::Url::parse(url).map_err(|_| refuse())?;
    let path = parsed.path();
    let ok = parsed.scheme() == "https"
        && parsed.username().is_empty()
        && parsed.port().is_none()
        && match parsed.host_str().unwrap_or("") {
            "mail.google.com" => path.starts_with("/mail/"),
            "calendar.google.com" => path == "/calendar/render",
            "outlook.office.com" | "outlook.live.com" => {
                path.starts_with("/mail/deeplink/compose") || path.starts_with("/calendar/0/deeplink/compose") || path.starts_with("/calendar/deeplink/compose")
            }
            _ => false,
        };
    if ok { Ok(()) } else { Err(refuse()) }
}

/// A calendar file the app writes and the person opens: a short, plain `.ics` made by the app.
pub fn check_calendar_file(text: &str) -> Result<(), CloudError> {
    let trimmed = text.trim();
    if text.len() > 40_000 || !trimmed.starts_with("BEGIN:VCALENDAR") || !trimmed.ends_with("END:VCALENDAR") || text.contains('\0') {
        return Err(CloudError::Unavailable("that is not a calendar file".into()));
    }
    Ok(())
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

    #[test]
    fn a_filter_is_checked_tidied_and_described_in_words() {
        let filter = SearchFilter::checked(Some(vec!["dropbox".into(), "google_drive".into(), "dropbox".into()]), Some(vec!["presentation".into(), "document".into()])).unwrap();
        assert_eq!(filter.sources, ["dropbox", "google_drive"]);
        assert_eq!(filter.types, ["document", "presentation"]);
        assert_eq!(filter.describe(), "Dropbox and Google Drive, documents and presentations");
        assert_eq!(SearchFilter::checked(Some(vec!["google_drive".into()]), None).unwrap().describe(), "Google Drive");
        assert!(SearchFilter::checked(None, None).unwrap().is_empty());
        for bad in [(Some(vec!["Google Drive".into()]), None), (Some(vec!["a;b".into()]), None), (None, Some(vec!["spreadsheets".into()])), (Some(vec![String::new()]), None), (Some((0..21).map(|n| format!("s{n}")).collect()), None)] {
            assert!(SearchFilter::checked(bad.0, bad.1).is_err());
        }
    }

    #[tokio::test]
    async fn a_filtered_search_sends_the_sources_and_kinds_and_an_unfiltered_one_sends_neither() {
        let dir = tempfile::tempdir().unwrap();
        let bodies = std::sync::Arc::new(std::sync::Mutex::new(Vec::<serde_json::Value>::new()));
        let seen = bodies.clone();
        let (api, _log) = serve(move |request| {
            seen.lock().unwrap().push(serde_json::from_str(&request.body).unwrap());
            (200, REAL_REPLY.into(), 0)
        })
        .await;
        let cloud = cloud_for(dir.path(), &api, None);
        sign_in_tokens(&cloud, "t", None, 3600);
        let filter = SearchFilter::checked(Some(vec!["google_drive".into()]), Some(vec!["presentation".into()])).unwrap();
        assert!(cloud.search_filtered_tracked("q", 5, &filter).await.0.is_ok());
        assert!(cloud.search_tracked("q", 5).await.0.is_ok());
        let sent = bodies.lock().unwrap();
        assert_eq!((sent[0]["sources"].clone(), sent[0]["types"].clone()), (serde_json::json!(["google_drive"]), serde_json::json!(["presentation"])));
        assert!(sent[1].get("sources").is_none() && sent[1].get("types").is_none());
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
                    .sign_in(&SignInChoice::Any, |url| {
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
                Some("private"),
            )
            .await
            .unwrap();
        assert!(url.starts_with("https://accounts.google.com/"));
        let body: serde_json::Value = serde_json::from_str(&bodies.lock().unwrap()[0]).unwrap();
        assert_eq!(body["kind"], "microsoft365");
        // The choice of who the files are for is passed on.
        assert_eq!(body["scope"], "private");
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
            let result = cloud.start_connection("google_drive", &[], None, None).await;
            assert!(
                matches!(result, Err(CloudError::Unavailable(_))),
                "{evil}: {result:?}"
            );
        }
    }

    #[test]
    fn a_refusal_shows_the_reason_the_server_gave_only_when_it_is_short_plain_words() {
        assert_eq!(server_reason(br#"{"error":"connect your Outlook first: open Connectors, Outlook email"}"#).as_deref(), Some("connect your Outlook first: open Connectors, Outlook email"));
        assert_eq!(server_reason(b"not json"), None);
        assert_eq!(server_reason(br#"{"error":""}"#), None);
        assert_eq!(server_reason(br#"{"error":"two\nlines"}"#), None);
        assert_eq!(server_reason(format!(r#"{{"error":"{}"}}"#, "x".repeat(201)).as_bytes()), None);
        assert_eq!(server_reason(br#"{"other":"x"}"#), None);
    }

    #[test]
    fn only_provider_sign_in_pages_or_this_machine_may_be_opened_in_the_browser() {
        assert!(check_browser_url(
            "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?x=1",
            false
        )
        .is_ok());
        // Dropbox and DocuSign sign in too, and nothing that only looks like them.
        for good in ["https://www.dropbox.com/oauth2/authorize?x=1", "https://account.docusign.com/oauth/auth?x=1", "https://account-d.docusign.com/oauth/auth?x=1", "https://appcenter.intuit.com/connect/oauth2?x=1", "https://zoom.us/oauth/authorize?x=1"] {
            assert!(check_browser_url(good, false).is_ok(), "{good}");
        }
        for bad in ["https://dropbox.com.evil.example/oauth2/authorize", "https://www.dropbox.com.evil.example/", "https://account.docusign.com.evil.example/", "http://www.dropbox.com/oauth2/authorize", "https://zoom.us.evil.example/oauth/authorize", "https://evilzoom.us/oauth/authorize", "http://zoom.us/oauth/authorize"] {
            assert!(check_browser_url(bad, false).is_err(), "{bad}");
        }
        assert!(check_browser_url("http://127.0.0.1:8080/x", false).is_err());
        assert!(check_browser_url("http://127.0.0.1:8080/x", true).is_ok());
        assert!(check_browser_url("http://localhost.evil.example/", true).is_err());
        assert!(
            check_browser_url("https://login.microsoftonline.com@evil.example/", false).is_err()
        );
    }

    #[test]
    fn a_prepared_draft_can_only_open_the_compose_and_new_event_pages_of_mail_and_calendar() {
        for good in [
            "https://mail.google.com/mail/?view=cm&fs=1&to=a%40b.com&su=Hi&body=Hello",
            "https://calendar.google.com/calendar/render?action=TEMPLATE&text=Review&dates=20261105T140000/20261105T150000",
            "https://outlook.office.com/mail/deeplink/compose?to=a%40b.com&subject=Hi&body=Hello",
            "https://outlook.office.com/calendar/0/deeplink/compose?subject=Review&startdt=2026-11-05T14:00:00",
            "https://outlook.live.com/mail/deeplink/compose?to=a%40b.com",
            "mailto:a@b.com?subject=Hi&body=Hello",
            "mailto:a@b.com,c@d.com?cc=e@f.com&subject=Hi",
        ] {
            assert!(check_prepared_link(good).is_ok(), "{good}");
        }
        for bad in [
            "https://evil.example/mail/?view=cm",
            "https://mail.google.com.evil.example/mail/",
            "https://mail.google.com@evil.example/mail/",
            "https://mail.google.com:8443/mail/",
            "https://mail.google.com/accounts/Logout",
            "https://calendar.google.com/calendar/u/0/r/settings",
            "http://mail.google.com/mail/?view=cm",
            "https://outlook.office.com/owa/",
            "mailto:a@b.com?attach=/etc/passwd",
            "mailto:a@b.com?body=x&bcc=spy@evil.example",
            "mailto:",
            "file:///etc/passwd",
            "javascript:alert(1)",
            "mailto:a@b.com?subject=Hi there",
            "https://mail.google.com/mail/?view=cm\nHost: x",
        ] {
            assert!(check_prepared_link(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn only_a_plain_calendar_file_is_written_and_opened() {
        assert!(check_calendar_file("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n").is_ok());
        assert!(check_calendar_file("#!/bin/sh\nrm -rf /").is_err());
        assert!(check_calendar_file("BEGIN:VCALENDAR\nEND:VCALENDAR\0").is_err());
        assert!(check_calendar_file(&format!("BEGIN:VCALENDAR{}END:VCALENDAR", "x".repeat(50_000))).is_err());
    }

    #[test]
    fn only_web_pages_of_the_supported_services_may_be_opened_as_documents() {
        for good in [
            "https://drive.google.com/open?id=abc",
            "https://docs.google.com/document/d/abc/edit",
            "https://www.dropbox.com/home/Clients/Retainer.txt",
            "https://app.docusign.com/documents/details/11111111-2222-3333-4444-555555555555",
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
            "https://www.dropbox.com.evil.example/home",
            "https://app.docusign.com@evil.example/x",
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
            .start_connection("google_drive", &[], None, None)
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
            matches!(cloud.start_connection("google_drive", &[], None, None).await, Err(CloudError::Unavailable(m)) if m.contains("not set up"))
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

    #[test]
    fn fixed_choices_route_directly_and_emails_and_provider_names_are_checked() {
        assert_eq!(SignInChoice::Any.fixed_routing(), None);
        assert_eq!(SignInChoice::Microsoft { email: "a@b.com".into() }.fixed_routing(), None);
        assert_eq!(
            SignInChoice::Google.fixed_routing(),
            Some(("identity_provider", "Google".into()))
        );
        assert_eq!(
            SignInChoice::Password.fixed_routing(),
            Some(("identity_provider", "COGNITO".into()))
        );
        assert_eq!(clean_email("  Sam@Acme-Law.COM ").unwrap(), "sam@acme-law.com");
        for bad in ["", "sam", "@acme.com", "sam@", "sam@acme", "a@b@c.com", "sa m@acme.com"] {
            assert!(clean_email(bad).is_err(), "{bad:?}");
        }
        for good in ["ms-acme-b72431", "ms-a-123456"] {
            assert!(valid_microsoft_provider(good), "{good}");
        }
        for bad in [
            "", "ms-", "Google", "COGNITO", "ms-Acme", "ms-a&scope=x", "ms-a b", "ms-a/b",
            &format!("ms-{}", "a".repeat(30)),
        ] {
            assert!(!valid_microsoft_provider(bad), "{bad:?}");
        }
        let parsed: SignInChoice =
            serde_json::from_str(r#"{"kind":"microsoft","email":"a@b.com"}"#).unwrap();
        assert_eq!(parsed, SignInChoice::Microsoft { email: "a@b.com".into() });
    }

    /// The address the browser is sent to for a choice, with the sign-in left waiting (it is then dropped).
    async fn address_for(
        cloud: Cloud,
        choice: SignInChoice,
    ) -> Result<reqwest::Url, CloudError> {
        let (tx, rx) = tokio::sync::oneshot::channel::<String>();
        let tx = StdMutex::new(Some(tx));
        let task = tokio::spawn(async move {
            cloud
                .sign_in(&choice, |url| {
                    tx.lock().unwrap().take().unwrap().send(url.to_string()).unwrap();
                })
                .await
        });
        match rx.await {
            Ok(url) => {
                task.abort();
                Ok(reqwest::Url::parse(&url).unwrap())
            }
            // The sign-in stopped before it opened the browser: that is the reason to report.
            Err(_) => Err(task.await.unwrap().unwrap_err()),
        }
    }

    fn pair(url: &reqwest::Url, name: &str) -> Option<String> {
        url.query_pairs().find(|(k, _)| k == name).map(|(_, v)| v.to_string())
    }

    #[tokio::test]
    async fn microsoft_sign_in_asks_the_server_for_the_providers_name_and_checks_the_answer() {
        let asked = Arc::new(StdMutex::new(Vec::<String>::new()));
        let log = asked.clone();
        let (server, _) = serve(move |request| {
            assert_eq!(request.path, "/v1/sign-in/route");
            log.lock().unwrap().push(request.body.clone());
            let body = if request.body.contains("sam@acme.com") {
                r#"{"provider":"ms-acme-b72431"}"#
            } else if request.body.contains("evil@acme.com") {
                r#"{"provider":"ms-a&scope=everything"}"#
            } else {
                r#"{"provider":null}"#
            };
            (200, body.into(), 0)
        })
        .await;
        let ask = |email: &str| {
            let dir = tempfile::tempdir().unwrap();
            let cloud = cloud_for(dir.path(), &server, Some("https://idp.example.com"));
            let choice = SignInChoice::Microsoft { email: email.into() };
            async move {
                let _keep = dir;
                address_for(cloud, choice).await
            }
        };
        // The browser is sent to that firm's sign-in, with the email lowercased and nothing else sent to the server.
        let url = ask(" Sam@Acme.com ").await.unwrap();
        assert_eq!(pair(&url, "identity_provider").as_deref(), Some("ms-acme-b72431"));
        assert_eq!(pair(&url, "idp_identifier"), None);
        assert_eq!(asked.lock().unwrap().clone(), [r#"{"email":"sam@acme.com"}"#]);
        // No sign-in found: a plain reason, and the browser is never opened.
        let none = ask("nobody@acme.com").await.unwrap_err().message();
        assert!(none.contains("Ask your administrator"), "{none}");
        // A strange provider name from the server is not put in the address.
        assert!(ask("evil@acme.com").await.is_err());
        // A malformed email never reaches the server.
        let calls = asked.lock().unwrap().len();
        assert!(ask("not-an-email").await.is_err());
        assert_eq!(asked.lock().unwrap().len(), calls);
    }

    #[tokio::test]
    async fn the_other_choices_open_the_right_page_without_asking_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let cloud = cloud_for(dir.path(), "https://api.example.com", Some("https://idp.example.com"));
        let google = address_for(cloud, SignInChoice::Google).await.unwrap();
        assert_eq!(pair(&google, "identity_provider").as_deref(), Some("Google"));
        let dir = tempfile::tempdir().unwrap();
        let cloud = cloud_for(dir.path(), "https://api.example.com", Some("https://idp.example.com"));
        let any = address_for(cloud, SignInChoice::Any).await.unwrap();
        assert_eq!(pair(&any, "identity_provider"), None);
        let dir = tempfile::tempdir().unwrap();
        let cloud = cloud_for(dir.path(), "https://api.example.com", Some("https://idp.example.com"));
        let password = address_for(cloud, SignInChoice::Password).await.unwrap();
        assert_eq!(pair(&password, "identity_provider").as_deref(), Some("COGNITO"));
    }

    #[tokio::test]
    async fn a_refusal_from_the_sign_in_service_is_shown_and_reported_not_hidden() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let waiting = tokio::spawn(async move { wait_for_callback(&listener, "st").await });
        let page = reqwest::get(format!(
            "http://127.0.0.1:{port}/callback?state=st&error=access_denied&error_description=PreSignUp%20failed%3A%20%3Cb%3Eno%3C%2Fb%3E"
        ))
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
        assert!(page.contains("Sign-in was not completed: PreSignUp failed: &lt;b&gt;no&lt;/b&gt;"), "{page}");
        let error = waiting.await.unwrap().unwrap_err();
        assert!(error.message().contains("PreSignUp failed"), "{}", error.message());
    }

    #[test]
    fn firm_names_are_tidied_and_bad_ones_refused() {
        assert_eq!(clean_firm_name("  Sam's   Tax \n Practice ").unwrap(), "Sam's Tax Practice");
        for bad in ["", "   ", &"a".repeat(121), "bad\u{7}name"] {
            assert!(clean_firm_name(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn starting_a_firm_posts_the_name_with_the_temporary_token_and_keeps_nothing() {
        let (server, seen) = serve(|request| match request.path.as_str() {
            "/v1/onboarding/firm" => {
                assert!(request.auth.as_deref().is_some_and(|a| a.contains("no-firm-token")));
                assert_eq!(request.body, r#"{"name":"Sam's Tax Practice"}"#);
                (200, r#"{"firm_id":"x"}"#.into(), 0)
            }
            other => panic!("unexpected request {other}"),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let cloud = cloud_for(dir.path(), &server, Some(&server));
        cloud.finish_new_firm("Sam's Tax Practice", "no-firm-token").await.unwrap();
        // The firm-less sign-in never becomes the app's session, so nothing else can be refused with it.
        assert!(cloud.tokens.lock().unwrap().is_none());
        assert_eq!(cloud.status().state, CloudState::SignedOut);
        assert_eq!(seen.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn after_the_firm_is_made_the_temporary_sign_in_is_refreshed_into_the_apps_session() {
        let (server, seen) = serve(|request| match request.path.as_str() {
            "/v1/onboarding/firm" => (200, r#"{"firm_id":"x"}"#.into(), 0),
            "/oauth2/token" => {
                let form: std::collections::HashMap<String, String> =
                    reqwest::Url::parse(&format!("http://x/?{}", request.body)).unwrap().query_pairs().into_owned().collect();
                assert_eq!(form["grant_type"], "refresh_token");
                assert_eq!(form["refresh_token"], "refresh-from-the-first-sign-in");
                (200, r#"{"id_token":"token-with-firm","expires_in":3600}"#.into(), 0)
            }
            other => panic!("unexpected request {other}"),
        })
        .await;
        let dir = tempfile::tempdir().unwrap();
        let cloud = cloud_for(dir.path(), &server, Some(&server));
        let temporary = Tokens {
            id_token: "no-firm-token".into(),
            refresh_token: Some("refresh-from-the-first-sign-in".into()),
            expires_at: Utc::now().timestamp() + 3600,
        };
        cloud.finish_new_firm("Sam", &temporary.id_token).await.unwrap();
        assert!(cloud.tokens.lock().unwrap().is_none(), "nothing is kept until the firm exists");
        let settings = cloud.settings();
        cloud.refresh_from(&settings, temporary).await.unwrap();
        let kept = cloud.tokens.lock().unwrap().clone().unwrap();
        assert_eq!(kept.id_token, "token-with-firm");
        assert_eq!(kept.refresh_token.as_deref(), Some("refresh-from-the-first-sign-in"));
        assert_eq!(cloud.status().state, CloudState::Ready);
        assert_eq!(seen.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn a_refused_firm_says_why_and_never_touches_an_existing_sign_in() {
        for (status, words) in [
            (409, "already belongs to a firm"),
            (429, "wait a minute"),
            (404, "not switched on"),
            (401, "did not accept"),
            (503, "try again"),
        ] {
            let (server, _) = serve(move |_| (status, "x".into(), 0)).await;
            let dir = tempfile::tempdir().unwrap();
            let cloud = cloud_for(dir.path(), &server, Some(&server));
            sign_in_tokens(&cloud, "someone-elses-session", Some("refresh-1"), 3600);
            let message = cloud.finish_new_firm("Sam", "no-firm-token").await.unwrap_err().message();
            assert!(message.contains(words), "{status}: {message}");
            assert_eq!(cloud.tokens.lock().unwrap().as_ref().unwrap().id_token, "someone-elses-session", "{status}");
        }
    }

    #[test]
    fn only_addresses_microsoft_issues_count_as_personal_microsoft() {
        for yes in ["sam@outlook.com", "x@hotmail.co.uk", "x@live.fr"] {
            assert!(is_personal_microsoft_address(yes), "{yes}");
        }
        for no in ["boss@company.com", "x@live.attacker.com", "x@outlook.com.evil.io", "x@gmail.com", "outlook.com"] {
            assert!(!is_personal_microsoft_address(no), "{no}");
        }
        assert_eq!(
            SignInChoice::MicrosoftPersonal.fixed_routing(),
            Some(("identity_provider", "MicrosoftPersonal".into()))
        );
    }

    #[tokio::test]
    async fn a_personal_microsoft_address_uses_the_shared_sign_in_unless_a_firm_has_its_own() {
        let (server, _) = serve(|request| {
            let body = if request.body.contains("invited@outlook.com") {
                r#"{"provider":"ms-acme-b72431"}"#
            } else {
                r#"{"provider":null}"#
            };
            (200, body.into(), 0)
        })
        .await;
        let ask = |email: &str| {
            let dir = tempfile::tempdir().unwrap();
            let cloud = cloud_for(dir.path(), &server, Some("https://idp.example.com"));
            let choice = SignInChoice::Microsoft { email: email.into() };
            async move {
                let _keep = dir;
                address_for(cloud, choice).await
            }
        };
        let personal = ask("new@outlook.com").await.unwrap();
        assert_eq!(pair(&personal, "identity_provider").as_deref(), Some("MicrosoftPersonal"));
        let firms = ask("invited@outlook.com").await.unwrap();
        assert_eq!(pair(&firms, "identity_provider").as_deref(), Some("ms-acme-b72431"));
        assert!(ask("someone@company.com").await.is_err());
    }

    #[tokio::test]
    async fn only_google_or_personal_microsoft_can_start_a_firm() {
        let dir = tempfile::tempdir().unwrap();
        let cloud = cloud_for(dir.path(), "https://api.example.com", Some("https://idp.example.com"));
        for with in [SignInChoice::Any, SignInChoice::Password, SignInChoice::Microsoft { email: "a@b.com".into() }] {
            let error = cloud.create_firm("Sam", &with, |_| panic!("the browser must not open")).await.unwrap_err();
            assert!(error.message().contains("Google or a personal Microsoft"), "{with:?}");
        }
    }
}
