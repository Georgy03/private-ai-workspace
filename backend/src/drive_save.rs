//! Adding one file the person approved to their own Google Drive, through the Drive API.
//!
//! The connection asks Google for the "files this app created" permission (drive.file), so this can
//! add a file but cannot read, change or delete any file it did not make.

use serde_json::{Value, json};

/// The text of a file is cut here, so one request cannot be made to carry an enormous body.
pub const MAX_CONTENT_CHARS: usize = 200_000;
const GOOGLE_DOC: &str = "application/vnd.google-apps.document";
const BOUNDARY: &str = "prism-workspace-upload-boundary-7f3a9c";

#[derive(Debug, PartialEq, Eq)]
pub enum SaveError {
    /// The sign-in is no longer good: the person has to connect again.
    Reconnect,
    /// Google understood and said no.
    Refused,
    /// Google could not be reached, or answered unexpectedly.
    Unavailable,
}

/// What was created.
#[derive(Debug, PartialEq, Eq)]
pub struct Created {
    pub id: Option<String>,
    pub link: Option<String>,
}

/// The request body: the file's name and type as JSON, then its text. Asking for the type of a Google
/// Doc makes Drive turn the plain text into a Doc.
fn multipart_body(title: &str, text: &str) -> String {
    let metadata = json!({ "name": title, "mimeType": GOOGLE_DOC });
    format!(
        "--{BOUNDARY}\r\nContent-Type: application/json; charset=UTF-8\r\n\r\n{metadata}\r\n--{BOUNDARY}\r\nContent-Type: text/plain; charset=UTF-8\r\n\r\n{text}\r\n--{BOUNDARY}--"
    )
}

/// Only an https Google link goes back to the app, which opens it in the person's browser.
fn link_ok(link: &str) -> bool {
    url::Url::parse(link).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some_and(|host| host.ends_with("google.com")))
}

/// Adds one plain-text file, which Drive turns into a Google Doc, to the person's Drive.
pub async fn create_file(http: &reqwest::Client, api_base: &str, token: &str, title: &str, text: &str) -> Result<Created, SaveError> {
    let response = http
        .post(format!("{}/upload/drive/v3/files?uploadType=multipart&fields=id,webViewLink", api_base.trim_end_matches('/')))
        .bearer_auth(token)
        .header("content-type", format!("multipart/related; boundary={BOUNDARY}"))
        .body(multipart_body(title, text))
        .send()
        .await
        .map_err(|_| SaveError::Unavailable)?;
    match response.status().as_u16() {
        200..=299 => {}
        401 | 403 => return Err(SaveError::Reconnect),
        400 | 404 | 413 | 422 => return Err(SaveError::Refused),
        _ => return Err(SaveError::Unavailable),
    }
    let file: Value = response.json().await.map_err(|_| SaveError::Unavailable)?;
    Ok(Created { id: file["id"].as_str().map(str::to_string), link: file["webViewLink"].as_str().filter(|link| link_ok(link)).map(str::to_string) })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_body_carries_the_name_as_json_and_the_text_untouched() {
        let body = multipart_body("Retreat \"budget\"", "# Heading\n\nTotal: 5,000.");
        assert!(body.contains(r#"{"mimeType":"application/vnd.google-apps.document","name":"Retreat \"budget\""}"#));
        assert!(body.contains("\r\n\r\n# Heading\n\nTotal: 5,000.\r\n--prism-workspace-upload-boundary-7f3a9c--"));
        assert_eq!(body.matches(&format!("--{BOUNDARY}\r\n")).count(), 2);
    }

    #[test]
    fn only_a_google_https_link_is_passed_back() {
        assert!(link_ok("https://docs.google.com/document/d/1/edit"));
        assert!(!link_ok("http://docs.google.com/x"));
        assert!(!link_ok("https://evil.example/x"));
        assert!(!link_ok("javascript:alert(1)"));
    }
}
