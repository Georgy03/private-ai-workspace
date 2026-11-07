//! Connectors read files and their permissions from an outside system (Microsoft 365, Google
//! Drive). A connector only reads; the sync engine in `crate::sync` decides what to store.

pub mod docusign;
pub mod dropbox;
pub mod quickbooks;
pub mod google;
pub mod microsoft;

use std::{
    sync::Mutex,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use serde_json::Value;
use url::Url;

/// There is no limit on how much a firm may store or how long a document may be. These two sizes
/// only protect the worker's memory, which holds a file while it is read; a file larger than this
/// is indexed by name only. They are far above any real document.
pub const MAX_TEXT_BYTES: usize = 512 * 1024 * 1024;
pub const MAX_DOWNLOAD_BYTES: usize = 512 * 1024 * 1024;
/// Pictures are read with OCR, which is slow, and a photo bigger than this is not a scanned page.
pub const MAX_IMAGE_BYTES: usize = 40 * 1024 * 1024;
/// Pictures smaller than this are icons and logos: nothing to read, so they are not downloaded.
pub const MIN_IMAGE_BYTES: u64 = 5 * 1024;

/// The most a file of this kind is downloaded for reading.
pub fn download_limit(name: &str, mime: &str) -> usize {
    match crate::extract::kind_for(name, mime) {
        Some(crate::extract::Kind::Image) => MAX_IMAGE_BYTES,
        _ => MAX_DOWNLOAD_BYTES,
    }
}
const CHUNK_TARGET_CHARS: usize = 1_200;

#[derive(Debug)]
pub enum ConnectorError {
    /// The saved position is no longer valid; start the listing again from the beginning.
    CursorExpired,
    /// The provider refused our credentials (revoked consent, expired refresh token).
    Unauthorized,
    /// Ask again after this long.
    RateLimited(Duration),
    NotFound,
    /// The provider understood the request and refused it (HTTP 403).
    Forbidden,
    /// The provider rejected the request itself (HTTP 400), e.g. a stale page token.
    BadRequest,
    Other(String),
}

impl std::fmt::Display for ConnectorError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConnectorError::CursorExpired => write!(f, "the provider's change cursor expired"),
            ConnectorError::Unauthorized => write!(f, "the provider rejected our credentials"),
            ConnectorError::RateLimited(wait) => write!(f, "rate limited for {}s", wait.as_secs()),
            ConnectorError::NotFound => write!(f, "not found"),
            ConnectorError::Forbidden => write!(f, "the provider refused access"),
            ConnectorError::BadRequest => write!(f, "the provider rejected the request"),
            ConnectorError::Other(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for ConnectorError {}

/// Who may read a file, as far as the provider told us. `Unknown` hides the file from everyone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Access {
    Known(Vec<String>),
    Unknown,
}

impl Access {
    pub fn readers(&self) -> Vec<String> {
        match self {
            Access::Known(readers) => readers.clone(),
            Access::Unknown => Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct RemoteFile {
    pub external_id: String,
    pub name: String,
    pub path: String,
    pub mime_type: String,
    pub size: u64,
    pub modified_at: i64,
    /// Changes whenever the file's content changes (not its permissions).
    pub version: String,
    /// Permissions delivered together with the listing, when the provider does that.
    pub access: Option<Access>,
    /// Where a person opens the file in the provider's own service.
    pub web_url: Option<String>,
}

/// Only web addresses are kept: a provider reply must not be able to hand the desktop app
/// anything else to open.
pub fn web_link(value: &serde_json::Value) -> Option<String> {
    value
        .as_str()
        .filter(|url| url.starts_with("https://"))
        .map(str::to_owned)
}

#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Upsert(RemoteFile),
    Delete(String),
    /// Everything inside this folder is gone (for providers that report a deleted folder once).
    DeleteTree(String),
}

#[derive(Debug, Default)]
pub struct ChangePage {
    pub changes: Vec<Change>,
    /// Save this once the page is stored; pass it to the next call.
    pub cursor: String,
    pub has_more: bool,
}

#[async_trait]
pub trait Connector: Send + Sync {
    /// The next page of changes after `cursor` (None starts a full listing).
    async fn changes(&self, cursor: Option<&str>) -> Result<ChangePage, ConnectorError>;
    /// Who may read this file.
    async fn access(&self, file: &RemoteFile) -> Result<Access, ConnectorError>;
    /// The file's text, or None when its type cannot be read as text here.
    async fn text(&self, file: &RemoteFile) -> Result<Option<String>, ConnectorError>;
    /// The raw bytes of a file that needs the file reader (PDF, Word, Excel, PowerPoint), or None
    /// when it is not worth downloading (too large, or a type the reader does not handle).
    async fn bytes(&self, _file: &RemoteFile) -> Result<Option<Vec<u8>>, ConnectorError> {
        Ok(None)
    }
}

// ---- Access tokens ----

#[async_trait]
pub trait TokenSource: Send + Sync {
    async fn token(&self) -> Result<String, ConnectorError>;
    /// Forget a cached token the provider has just rejected.
    fn invalidate(&self) {}
}

/// A fixed token. For tests and local development.
pub struct StaticToken(pub String);

#[async_trait]
impl TokenSource for StaticToken {
    async fn token(&self) -> Result<String, ConnectorError> {
        Ok(self.0.clone())
    }
}

/// A refresh token the provider replaced while we used it. Providers that rotate them (DocuSign)
/// invalidate the old one eventually, so the worker saves the new one when a sync ends.
pub type RotatedToken = std::sync::Arc<Mutex<Option<String>>>;

/// Exchanges a long-lived refresh token for short-lived access tokens, and caches them.
pub struct RefreshingToken {
    http: reqwest::Client,
    token_url: String,
    client_id: String,
    client_secret: String,
    refresh_token: Mutex<String>,
    /// Send the client ID and secret as a Basic authorization header instead of in the form.
    basic_auth: bool,
    rotated: RotatedToken,
    cached: Mutex<Option<(String, Instant)>>,
}

impl RefreshingToken {
    pub fn new(
        token_url: String,
        client_id: String,
        client_secret: String,
        refresh_token: String,
    ) -> Result<Self, ConnectorError> {
        Ok(Self {
            http: http_client()?,
            token_url,
            client_id,
            client_secret,
            refresh_token: Mutex::new(refresh_token),
            basic_auth: false,
            rotated: Default::default(),
            cached: Mutex::new(None),
        })
    }

    pub fn with_basic_auth(mut self, basic: bool) -> Self {
        self.basic_auth = basic;
        self
    }

    /// Where a replacement refresh token will appear, if the provider issues one.
    pub fn rotated_token(&self) -> RotatedToken {
        self.rotated.clone()
    }
}

#[async_trait]
impl TokenSource for RefreshingToken {
    async fn token(&self) -> Result<String, ConnectorError> {
        if let Some((token, expires)) = self.cached.lock().unwrap().as_ref()
            && Instant::now() < *expires
        {
            return Ok(token.clone());
        }
        let current = self.refresh_token.lock().unwrap().clone();
        let mut request = self.http.post(&self.token_url);
        let mut form = vec![("grant_type", "refresh_token"), ("refresh_token", current.as_str())];
        if self.basic_auth {
            request = request.basic_auth(&self.client_id, Some(&self.client_secret));
        } else {
            form.push(("client_id", self.client_id.as_str()));
            form.push(("client_secret", self.client_secret.as_str()));
        }
        let response = request
            .form(&form)
            .send()
            .await
            .map_err(|_| ConnectorError::Other("could not reach the sign-in service".into()))?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        if status == reqwest::StatusCode::BAD_REQUEST || status == reqwest::StatusCode::UNAUTHORIZED
        {
            // invalid_grant: consent was withdrawn or the refresh token expired.
            return Err(ConnectorError::Unauthorized);
        }
        let token = body["access_token"]
            .as_str()
            .filter(|_| status.is_success())
            .ok_or_else(|| ConnectorError::Other(format!("token endpoint returned {status}")))?
            .to_string();
        // A provider that rotates refresh tokens sends a new one; keep using it, and let the worker
        // save it when this sync ends.
        if let Some(next) = body["refresh_token"].as_str().filter(|next| !next.is_empty() && *next != current) {
            *self.refresh_token.lock().unwrap() = next.to_string();
            *self.rotated.lock().unwrap() = Some(next.to_string());
        }
        let lifetime = body["expires_in"]
            .as_u64()
            .unwrap_or(300)
            .saturating_sub(60);
        *self.cached.lock().unwrap() = Some((
            token.clone(),
            Instant::now() + Duration::from_secs(lifetime),
        ));
        Ok(token)
    }

    fn invalidate(&self) {
        *self.cached.lock().unwrap() = None;
    }
}

// ---- HTTP ----

pub(crate) fn http_client() -> Result<reqwest::Client, ConnectorError> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        // Download links redirect to a storage host; those are followed, but never with our
        // token (the token is only attached to calls we make ourselves).
        .redirect(reqwest::redirect::Policy::limited(5))
        .build()
        .map_err(|error| ConnectorError::Other(format!("could not start HTTPS: {error}")))
}

/// Whether `candidate` is on the same host and port as `base`. Provider responses contain URLs
/// to follow (next pages, delta links); following one that points elsewhere would send our
/// access token to a stranger.
pub(crate) fn same_origin(base: &str, candidate: &str) -> bool {
    match (Url::parse(base), Url::parse(candidate)) {
        (Ok(base), Ok(candidate)) => {
            base.scheme() == candidate.scheme()
                && base.host_str() == candidate.host_str()
                && base.port_or_known_default() == candidate.port_or_known_default()
        }
        _ => false,
    }
}

pub(crate) struct Api {
    pub http: reqwest::Client,
    pub tokens: Box<dyn TokenSource>,
}

impl Api {
    pub fn new(tokens: Box<dyn TokenSource>) -> Result<Self, ConnectorError> {
        Ok(Self {
            http: http_client()?,
            tokens,
        })
    }

    /// Asks for JSON in every request, for services (QuickBooks) that answer in XML otherwise.
    pub fn asking_for_json(mut self) -> Result<Self, ConnectorError> {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.insert(reqwest::header::ACCEPT, reqwest::header::HeaderValue::from_static("application/json"));
        self.http = reqwest::Client::builder()
            .timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::limited(5))
            .default_headers(headers)
            .build()
            .map_err(|error| ConnectorError::Other(format!("could not start HTTPS: {error}")))?;
        Ok(self)
    }

    /// GET with the access token, retrying brief failures and honouring rate limits.
    pub async fn get(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<reqwest::Response, ConnectorError> {
        let mut refreshed = false;
        for attempt in 0..4u32 {
            let token = self.tokens.token().await?;
            let result = self
                .http
                .get(url)
                .query(query)
                .bearer_auth(&token)
                .send()
                .await;
            let response = match result {
                Ok(response) => response,
                Err(_) if attempt < 3 => {
                    tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt))).await;
                    continue;
                }
                Err(_) => return Err(ConnectorError::Other("could not reach the provider".into())),
            };
            let status = response.status();
            match status.as_u16() {
                200..=299 => return Ok(response),
                401 if !refreshed => {
                    refreshed = true;
                    self.tokens.invalidate();
                }
                401 => return Err(ConnectorError::Unauthorized),
                400 => return Err(ConnectorError::BadRequest),
                403 => return Err(ConnectorError::Forbidden),
                404 => return Err(ConnectorError::NotFound),
                410 => return Err(ConnectorError::CursorExpired),
                429 | 503 => {
                    let wait = response
                        .headers()
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(Duration::from_secs(5));
                    if attempt < 3 && wait <= Duration::from_secs(10) {
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    return Err(ConnectorError::RateLimited(wait));
                }
                500..=599 if attempt < 3 => {
                    tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt))).await;
                }
                _ => {
                    return Err(ConnectorError::Other(format!(
                        "provider returned HTTP {status}"
                    )));
                }
            }
        }
        Err(ConnectorError::Other("provider kept failing".into()))
    }

    /// POST with the access token, for providers whose API is calls with a JSON body (Dropbox).
    /// `headers` are added to every attempt. Retries and rate limits work as for `get`.
    pub async fn post(
        &self,
        url: &str,
        body: Option<&Value>,
        headers: &[(&str, String)],
    ) -> Result<reqwest::Response, ConnectorError> {
        let mut refreshed = false;
        for attempt in 0..4u32 {
            let token = self.tokens.token().await?;
            let mut request = self.http.post(url).bearer_auth(&token);
            for (name, value) in headers {
                request = request.header(*name, value);
            }
            request = match body {
                Some(body) => request.json(body),
                // Dropbox's download calls carry their arguments in a header and no body.
                None => request,
            };
            let response = match request.send().await {
                Ok(response) => response,
                Err(_) if attempt < 3 => {
                    tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt))).await;
                    continue;
                }
                Err(_) => return Err(ConnectorError::Other("could not reach the provider".into())),
            };
            let status = response.status();
            match status.as_u16() {
                200..=299 => return Ok(response),
                401 if !refreshed => {
                    refreshed = true;
                    self.tokens.invalidate();
                }
                401 => return Err(ConnectorError::Unauthorized),
                400 => return Err(ConnectorError::BadRequest),
                403 => return Err(ConnectorError::Forbidden),
                404 => return Err(ConnectorError::NotFound),
                // Dropbox reports "this cursor is no longer valid", "no such file" and "no
                // permission" as HTTP 409 with the reason in the body.
                409 => {
                    let reason = response.text().await.unwrap_or_default();
                    return Err(if reason.contains("reset") {
                        ConnectorError::CursorExpired
                    } else if reason.contains("not_found") {
                        ConnectorError::NotFound
                    } else if reason.contains("no_permission") || reason.contains("access_error") {
                        ConnectorError::Forbidden
                    } else {
                        ConnectorError::BadRequest
                    });
                }
                429 | 503 => {
                    let wait = response
                        .headers()
                        .get("retry-after")
                        .and_then(|value| value.to_str().ok())
                        .and_then(|value| value.parse::<u64>().ok())
                        .map(Duration::from_secs)
                        .unwrap_or(Duration::from_secs(5));
                    if attempt < 3 && wait <= Duration::from_secs(10) {
                        tokio::time::sleep(wait).await;
                        continue;
                    }
                    return Err(ConnectorError::RateLimited(wait));
                }
                500..=599 if attempt < 3 => {
                    tokio::time::sleep(Duration::from_millis(200 * 2u64.pow(attempt))).await;
                }
                _ => return Err(ConnectorError::Other(format!("provider returned HTTP {status}"))),
            }
        }
        Err(ConnectorError::Other("provider kept failing".into()))
    }

    pub async fn post_json(
        &self,
        url: &str,
        body: &Value,
        headers: &[(&str, String)],
    ) -> Result<Value, ConnectorError> {
        self.post(url, Some(body), headers)
            .await?
            .json()
            .await
            .map_err(|_| ConnectorError::Other("provider returned unreadable data".into()))
    }

    /// A POST whose answer is a file, read up to `cap` bytes; None if it is bigger.
    pub async fn post_bytes_capped(
        &self,
        url: &str,
        headers: &[(&str, String)],
        cap: usize,
    ) -> Result<Option<Vec<u8>>, ConnectorError> {
        let mut response = self.post(url, None, headers).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ConnectorError::Other("download was interrupted".into()))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > cap {
                return Ok(None);
            }
        }
        Ok(Some(body))
    }

    pub async fn get_json(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, ConnectorError> {
        self.get(url, query)
            .await?
            .json()
            .await
            .map_err(|_| ConnectorError::Other("provider returned unreadable data".into()))
    }

    /// Reads a body, giving up past the size cap rather than loading it.
    pub async fn get_bytes(
        &self,
        url: &str,
        query: &[(&str, &str)],
    ) -> Result<Option<Vec<u8>>, ConnectorError> {
        self.get_bytes_capped(url, query, MAX_TEXT_BYTES).await
    }

    pub async fn get_bytes_capped(
        &self,
        url: &str,
        query: &[(&str, &str)],
        cap: usize,
    ) -> Result<Option<Vec<u8>>, ConnectorError> {
        let mut response = self.get(url, query).await?;
        let mut body = Vec::new();
        while let Some(chunk) = response
            .chunk()
            .await
            .map_err(|_| ConnectorError::Other("download was interrupted".into()))?
        {
            body.extend_from_slice(&chunk);
            if body.len() > cap {
                return Ok(None);
            }
        }
        Ok(Some(body))
    }
}

// ---- Text ----

/// Adds identities that can certainly read every file a connection returns. Whoever connected a
/// source can open everything the provider lists for it, because the provider only lists what
/// that account can open. This also keeps a solo user's own files findable when the provider
/// reports no sharing entries for them, which is common on personal OneDrive.
pub(crate) fn with_extra_readers(access: Access, extra: &[String]) -> Access {
    if extra.is_empty() {
        return access;
    }
    let mut readers = access.readers();
    for reader in extra {
        if !readers.contains(reader) {
            readers.push(reader.clone());
        }
    }
    readers.sort();
    Access::Known(readers)
}

/// Types read directly as text. Everything else is indexed by name only until a sandboxed
/// parser for PDF and Office files exists on the server.
pub fn is_plain_text(mime_type: &str, name: &str) -> bool {
    let name = name.to_lowercase();
    mime_type.starts_with("text/")
        || matches!(mime_type, "application/json" | "application/xml")
        || [".txt", ".md", ".csv", ".json", ".xml", ".log"]
            .iter()
            .any(|extension| name.ends_with(extension))
}

pub fn decode_text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_start_matches('\u{feff}')
        .replace('\0', "")
}

/// Splits text into passages of roughly 1,200 characters, on paragraph breaks where possible.
pub fn chunk_text(text: &str) -> Vec<String> {
    let mut chunks = Vec::new();
    let mut current = String::new();
    let flush = |current: &mut String, chunks: &mut Vec<String>| {
        let piece = current.trim();
        if !piece.is_empty() {
            chunks.push(piece.to_string());
        }
        current.clear();
    };
    for paragraph in text.replace("\r\n", "\n").split("\n\n") {
        let paragraph = paragraph.trim();
        if paragraph.is_empty() {
            continue;
        }
        if current.chars().count() + paragraph.chars().count() + 2 > CHUNK_TARGET_CHARS {
            flush(&mut current, &mut chunks);
        }
        if paragraph.chars().count() > CHUNK_TARGET_CHARS {
            let characters: Vec<char> = paragraph.chars().collect();
            for piece in characters.chunks(CHUNK_TARGET_CHARS) {
                chunks.push(piece.iter().collect::<String>().trim().to_string());
            }
            continue;
        }
        if !current.is_empty() {
            current.push_str("\n\n");
        }
        current.push_str(paragraph);
    }
    flush(&mut current, &mut chunks);
    chunks.retain(|chunk| !chunk.is_empty());
    chunks
}

/// Lower-cases and trims an email address, and rejects anything that is not one.
pub fn email_principal(address: &str) -> Option<String> {
    let address = address.trim().to_lowercase();
    (address.contains('@') && !address.contains(char::is_whitespace))
        .then(|| format!("email:{address}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_web_addresses_are_kept_as_links() {
        use serde_json::json;
        assert_eq!(
            web_link(&json!("https://docs.google.com/d/1")).as_deref(),
            Some("https://docs.google.com/d/1")
        );
        for refused in [
            json!("file:///etc/passwd"),
            json!("javascript:alert(1)"),
            json!("http://example.com"),
            json!(null),
            json!(5),
        ] {
            assert_eq!(web_link(&refused), None, "{refused}");
        }
    }

    #[test]
    fn only_links_on_the_providers_own_host_are_followed() {
        let base = "https://graph.microsoft.com";
        assert!(same_origin(
            base,
            "https://graph.microsoft.com/v1.0/drives/x/root/delta?token=1"
        ));
        assert!(!same_origin(base, "https://evil.example/v1.0/drives"));
        assert!(!same_origin(base, "http://graph.microsoft.com/v1.0"));
        assert!(!same_origin(
            base,
            "https://graph.microsoft.com.evil.example/"
        ));
        assert!(!same_origin(base, "https://graph.microsoft.com:8443/"));
        assert!(!same_origin(base, "not a url"));
    }

    #[test]
    fn text_is_split_on_paragraphs_and_long_runs_are_cut() {
        let paragraph = "word ".repeat(100); // 500 characters
        let text = format!("{paragraph}\n\n{paragraph}\n\n{paragraph}");
        let chunks = chunk_text(&text);
        assert_eq!(
            chunks.len(),
            2,
            "two paragraphs fit in one chunk, the third starts another"
        );
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= CHUNK_TARGET_CHARS)
        );

        let long = "x".repeat(CHUNK_TARGET_CHARS * 2 + 10);
        let chunks = chunk_text(&long);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks.concat().len(), long.len());

        assert!(chunk_text("   \n\n  ").is_empty());
        assert_eq!(chunk_text("one\r\n\r\ntwo"), ["one\n\ntwo"]);
    }

    #[test]
    fn text_decoding_drops_byte_order_marks_and_nul_bytes() {
        assert_eq!(decode_text(b"\xef\xbb\xbfhello\0 world"), "hello world");
        assert_eq!(decode_text(&[0xff, b'a']), "\u{fffd}a");
    }

    #[test]
    fn only_text_like_files_are_read_directly() {
        assert!(is_plain_text("text/plain", "a"));
        assert!(is_plain_text("application/octet-stream", "notes.MD"));
        assert!(!is_plain_text("application/pdf", "scan.pdf"));
        assert!(!is_plain_text(
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            "a.docx"
        ));
    }

    #[test]
    fn email_principals_are_lower_cased_and_validated() {
        assert_eq!(
            email_principal(" Alice@Firm.COM "),
            Some("email:alice@firm.com".into())
        );
        assert_eq!(email_principal("not-an-email"), None);
        assert_eq!(email_principal("a b@c.d"), None);
    }
}
