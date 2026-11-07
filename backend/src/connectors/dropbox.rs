//! Dropbox through the Dropbox API v2.
//!
//! A connection reads what the connecting Dropbox account can see: its own files, folders shared
//! with it and, for a business account, the team's space. The first sync lists everything; after
//! that it follows the account's change cursor. Files are identified by their lower-cased path,
//! because that is the only thing Dropbox says about a deleted file.
//!
//! Who may read a file comes from Dropbox's own sharing lists: the people and invited email
//! addresses a file or its shared folder was shared with. Files shared only with a Dropbox group
//! are not shown to the group's members, because Dropbox does not tell us who is in it without a
//! team-admin permission. That errs on the side of showing too little. A file that is not shared
//! at all is readable only by the account that connected the source. Public links are never
//! treated as access.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde_json::{Value, json};
use tokio::sync::OnceCell;
use url::Url;

use super::{
    Access, Api, Change, ChangePage, Connector, ConnectorError, MAX_DOWNLOAD_BYTES, MAX_TEXT_BYTES,
    RemoteFile, TokenSource, decode_text, download_limit, email_principal, is_plain_text,
    with_extra_readers,
};
use crate::extract::kind_for;

pub const API_BASE: &str = "https://api.dropboxapi.com";
pub const CONTENT_BASE: &str = "https://content.dropboxapi.com";
/// Pages of sharing entries read for one file; a file with more than this many people on it is
/// not something a firm's documents are.
const MAX_MEMBER_PAGES: usize = 5;

pub struct DropboxConnector {
    api: Api,
    base: String,
    content: String,
    /// Identities that may read every file this connection returns.
    always_readable_by: Vec<String>,
    /// The `Dropbox-API-Path-Root` header, set for a business account so the team's folders are
    /// listed along with the member's own.
    root: OnceCell<Option<String>>,
}

impl DropboxConnector {
    pub fn new(tokens: Box<dyn TokenSource>, base: &str, content: &str) -> Result<Self, ConnectorError> {
        Ok(Self {
            api: Api::new(tokens)?,
            base: base.trim_end_matches('/').to_string(),
            content: content.trim_end_matches('/').to_string(),
            always_readable_by: Vec::new(),
            root: OnceCell::new(),
        })
    }

    pub fn always_readable_by(mut self, readers: Vec<String>) -> Self {
        self.always_readable_by = readers;
        self
    }

    fn rpc(&self, method: &str) -> String {
        format!("{}/2/{method}", self.base)
    }

    /// The account's team root, when it has one that differs from its personal space.
    async fn headers(&self) -> Result<Vec<(&'static str, String)>, ConnectorError> {
        let root = self
            .root
            .get_or_try_init(|| async {
                // This call takes no arguments, and Dropbox wants it sent with no body at all.
                let account: Value = self
                    .api
                    .post(&self.rpc("users/get_current_account"), None, &[])
                    .await?
                    .json()
                    .await
                    .map_err(|_| ConnectorError::Other("provider returned unreadable data".into()))?;
                let info = &account["root_info"];
                Ok::<_, ConnectorError>(match (info["root_namespace_id"].as_str(), info["home_namespace_id"].as_str()) {
                    (Some(root), Some(home)) if root != home => {
                        Some(json!({ ".tag": "root", "root": root }).to_string())
                    }
                    _ => None,
                })
            })
            .await?;
        Ok(root.iter().map(|value| ("Dropbox-API-Path-Root", value.clone())).collect())
    }
}

fn epoch(value: &Value) -> i64 {
    value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp())
        .unwrap_or(0)
}

/// Dropbox does not say what kind a file is, so it comes from the name.
pub(crate) fn mime_for(name: &str) -> &'static str {
    let lower = name.to_lowercase();
    let ends = |exts: &[&str]| exts.iter().any(|e| lower.ends_with(e));
    if ends(&[".pdf"]) {
        "application/pdf"
    } else if ends(&[".docx"]) {
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    } else if ends(&[".xlsx"]) {
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet"
    } else if ends(&[".pptx"]) {
        "application/vnd.openxmlformats-officedocument.presentationml.presentation"
    } else if ends(&[".csv"]) {
        "text/csv"
    } else if ends(&[".txt", ".md", ".log"]) {
        "text/plain"
    } else if ends(&[".png"]) {
        "image/png"
    } else if ends(&[".jpg", ".jpeg"]) {
        "image/jpeg"
    } else if ends(&[".tif", ".tiff"]) {
        "image/tiff"
    } else {
        "application/octet-stream"
    }
}

/// A link that opens the file in Dropbox's web app. The path is encoded, so nothing in a file name
/// can change where the link goes.
fn web_url(path_display: &str) -> Option<String> {
    let mut url = Url::parse("https://www.dropbox.com/home").ok()?;
    url.path_segments_mut().ok()?.extend(path_display.split('/').filter(|part| !part.is_empty()));
    Some(url.into())
}

/// One entry of a listing as a change, or None for folders and anything else that is not a file.
pub(crate) fn parse_entry(entry: &Value) -> Vec<Change> {
    let Some(path) = entry["path_lower"].as_str() else { return Vec::new() };
    match entry[".tag"].as_str() {
        // A deleted entry may be a file or a folder with files in it, and says which of the two
        // nowhere, so both are removed.
        Some("deleted") => vec![Change::Delete(path.to_string()), Change::DeleteTree(path.to_string())],
        Some("file") => {
            let name = entry["name"].as_str().unwrap_or("").to_string();
            let mime_type = if entry["is_downloadable"].as_bool() == Some(false) {
                // Dropbox Paper and similar: listed, but there is nothing to download.
                "application/x-dropbox-paper".to_string()
            } else {
                mime_for(&name).to_string()
            };
            // Only shared files report sharing details. Anything else is private to the account.
            let access = if entry["sharing_info"].is_null() { Some(Access::Known(Vec::new())) } else { None };
            vec![Change::Upsert(RemoteFile {
                external_id: path.to_string(),
                path: entry["path_display"].as_str().unwrap_or(path).to_string(),
                name,
                mime_type,
                size: entry["size"].as_u64().unwrap_or(0),
                modified_at: epoch(&entry["server_modified"]),
                version: entry["content_hash"].as_str().or_else(|| entry["rev"].as_str()).unwrap_or("").to_string(),
                access,
                web_url: entry["path_display"].as_str().and_then(web_url),
            })]
        }
        _ => Vec::new(),
    }
}

/// The people a sharing answer names: members and invited email addresses.
pub(crate) fn member_principals(page: &Value) -> Vec<String> {
    let mut out = BTreeSet::new();
    for user in page["users"].as_array().into_iter().flatten() {
        if let Some(principal) = user["user"]["email"].as_str().and_then(email_principal) {
            out.insert(principal);
        }
    }
    for invitee in page["invitees"].as_array().into_iter().flatten() {
        if let Some(principal) = invitee["invitee"]["email"].as_str().and_then(email_principal) {
            out.insert(principal);
        }
    }
    out.into_iter().collect()
}

#[async_trait]
impl Connector for DropboxConnector {
    async fn changes(&self, cursor: Option<&str>) -> Result<ChangePage, ConnectorError> {
        let headers = self.headers().await?;
        let body = match cursor {
            None => {
                let first = json!({
                    "path": "", "recursive": true, "limit": 1000,
                    "include_mounted_folders": true,
                    "include_non_downloadable_files": true,
                    "include_has_explicit_shared_members": true,
                });
                self.api.post_json(&self.rpc("files/list_folder"), &first, &headers).await?
            }
            Some(cursor) => {
                self.api
                    .post_json(&self.rpc("files/list_folder/continue"), &json!({ "cursor": cursor }), &headers)
                    .await?
            }
        };
        Ok(ChangePage {
            changes: body["entries"].as_array().into_iter().flatten().flat_map(parse_entry).collect(),
            cursor: body["cursor"].as_str().ok_or_else(|| ConnectorError::Other("provider sent no cursor".into()))?.to_string(),
            has_more: body["has_more"].as_bool().unwrap_or(false),
        })
    }

    async fn access(&self, file: &RemoteFile) -> Result<Access, ConnectorError> {
        let base = match &file.access {
            Some(known) => known.clone(),
            None => {
                let headers = self.headers().await?;
                let mut readers = BTreeSet::new();
                let first = self
                    .api
                    .post_json(
                        &self.rpc("sharing/list_file_members"),
                        &json!({ "file": file.external_id, "include_inherited": true, "limit": 300 }),
                        &headers,
                    )
                    .await;
                let mut page = match first {
                    Ok(page) => page,
                    // Dropbox shows sharing only to people who may manage it. The account that
                    // connected the source still opens the file, so it stays readable to them alone.
                    Err(ConnectorError::Forbidden) => {
                        return Ok(with_extra_readers(Access::Known(Vec::new()), &self.always_readable_by));
                    }
                    Err(other) => return Err(other),
                };
                for _ in 0..MAX_MEMBER_PAGES {
                    readers.extend(member_principals(&page));
                    let Some(next) = page["cursor"].as_str().filter(|c| !c.is_empty()) else { break };
                    page = self
                        .api
                        .post_json(&self.rpc("sharing/list_file_members/continue"), &json!({ "cursor": next }), &headers)
                        .await?;
                }
                Access::Known(readers.into_iter().collect())
            }
        };
        // Dropbox only lists sharing for files that are shared, and the account that connected the
        // source can open everything it lists.
        Ok(with_extra_readers(base, &self.always_readable_by))
    }

    async fn text(&self, file: &RemoteFile) -> Result<Option<String>, ConnectorError> {
        if !is_plain_text(&file.mime_type, &file.name) || file.size as usize > MAX_TEXT_BYTES {
            return Ok(None);
        }
        Ok(self.download(file, MAX_TEXT_BYTES).await?.map(|bytes| decode_text(&bytes)))
    }

    async fn bytes(&self, file: &RemoteFile) -> Result<Option<Vec<u8>>, ConnectorError> {
        if kind_for(&file.name, &file.mime_type).is_none() || file.size as usize > download_limit(&file.name, &file.mime_type) {
            return Ok(None);
        }
        self.download(file, MAX_DOWNLOAD_BYTES).await
    }
}

impl DropboxConnector {
    async fn download(&self, file: &RemoteFile, cap: usize) -> Result<Option<Vec<u8>>, ConnectorError> {
        let mut headers = self.headers().await?;
        headers.push(("Dropbox-API-Arg", json!({ "path": file.external_id }).to_string()));
        self.api.post_bytes_capped(&format!("{}/2/files/download", self.content), &headers, cap).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_become_changes_and_deleted_entries_remove_a_file_or_a_whole_folder() {
        let file = json!({
            ".tag": "file", "name": "Retreat Budget.xlsx", "path_lower": "/clients/retreat budget.xlsx",
            "path_display": "/Clients/Retreat Budget.xlsx", "size": 2048, "content_hash": "abc",
            "server_modified": "2026-03-01T10:00:00Z", "is_downloadable": true,
        });
        let Change::Upsert(parsed) = parse_entry(&file).remove(0) else { panic!() };
        assert_eq!(parsed.external_id, "/clients/retreat budget.xlsx");
        assert_eq!((parsed.version.as_str(), parsed.size), ("abc", 2048));
        assert_eq!(parsed.web_url.as_deref(), Some("https://www.dropbox.com/home/Clients/Retreat%20Budget.xlsx"));
        // Not shared: private to the account, with no call to make.
        assert_eq!(parsed.access, Some(Access::Known(Vec::new())));

        let shared = json!({ ".tag": "file", "name": "a.txt", "path_lower": "/a.txt", "sharing_info": { "read_only": false } });
        let Change::Upsert(parsed) = parse_entry(&shared).remove(0) else { panic!() };
        assert_eq!(parsed.access, None, "a shared file's members are looked up");

        let gone = parse_entry(&json!({ ".tag": "deleted", "path_lower": "/clients" }));
        assert_eq!(gone, vec![Change::Delete("/clients".into()), Change::DeleteTree("/clients".into())]);
        assert!(parse_entry(&json!({ ".tag": "folder", "path_lower": "/clients" })).is_empty());

        let paper = json!({ ".tag": "file", "name": "Notes.paper", "path_lower": "/notes.paper", "is_downloadable": false });
        let Change::Upsert(parsed) = parse_entry(&paper).remove(0) else { panic!() };
        assert_eq!(kind_for(&parsed.name, &parsed.mime_type), None, "nothing to download: found by name");
    }

    #[test]
    fn a_link_cannot_be_redirected_by_a_file_name() {
        let link = web_url("/Clients/..//evil?x=1#y").unwrap();
        assert!(link.starts_with("https://www.dropbox.com/home/"), "{link}");
        assert!(!link.contains('?') && !link.contains('#'), "{link}");
    }

    #[test]
    fn sharing_lists_become_the_people_who_may_read() {
        let page = json!({
            "users": [{ "user": { "email": "Alice@Firm.com" }, "access_type": { ".tag": "editor" } }],
            "invitees": [{ "invitee": { ".tag": "email", "email": "bob@firm.com" } }],
            // Groups cannot be expanded without a team permission, so they grant nothing.
            "groups": [{ "group": { "group_name": "Partners", "group_id": "g:1" } }],
        });
        assert_eq!(member_principals(&page), ["email:alice@firm.com", "email:bob@firm.com"]);
    }

    #[test]
    fn kinds_come_from_the_name() {
        assert_eq!(mime_for("A.PDF"), "application/pdf");
        assert_eq!(mime_for("notes.md"), "text/plain");
        assert_eq!(mime_for("blob"), "application/octet-stream");
    }
}
