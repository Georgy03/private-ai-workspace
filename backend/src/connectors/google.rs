//! Google Drive through the Drive v3 API.
//!
//! A connection reads what the connecting Google account can see: its own files, files shared
//! with it, and shared drives. The first sync lists everything; after that it follows the
//! changes feed from a start token taken before the listing began, so nothing is missed.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::extract::kind_for;

use super::{
    Access, Api, Change, ChangePage, Connector, ConnectorError, MAX_DOWNLOAD_BYTES, MAX_TEXT_BYTES,
    RemoteFile, TokenSource, decode_text, download_limit, email_principal, is_plain_text, web_link,
    with_extra_readers,
};

pub const DRIVE_BASE: &str = "https://www.googleapis.com";
const FOLDER: &str = "application/vnd.google-apps.folder";
const FILE_FIELDS: &str = "id,name,mimeType,modifiedTime,md5Checksum,version,size,trashed,webViewLink,permissions(type,role,emailAddress,domain,deleted)";

pub struct GoogleConnector {
    api: Api,
    base: String,
    /// Identities that may read every file this connection returns (see `with_extra_readers`).
    always_readable_by: Vec<String>,
}

impl GoogleConnector {
    pub fn new(tokens: Box<dyn TokenSource>, base: &str) -> Result<Self, ConnectorError> {
        Ok(Self {
            api: Api::new(tokens)?,
            base: base.trim_end_matches('/').to_string(),
            always_readable_by: Vec::new(),
        })
    }

    pub fn always_readable_by(mut self, readers: Vec<String>) -> Self {
        self.always_readable_by = readers;
        self
    }
}

fn epoch(value: &Value) -> i64 {
    value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp())
        .unwrap_or(0)
}

/// Turns Drive's permission list for one file into the identities that may read it.
pub(crate) fn permission_principals(permissions: &[Value]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for permission in permissions {
        if permission["deleted"].as_bool() == Some(true)
            || permission["role"].as_str() == Some("none")
        {
            continue;
        }
        match permission["type"].as_str() {
            Some("user") => {
                if let Some(email) = permission["emailAddress"]
                    .as_str()
                    .and_then(email_principal)
                {
                    out.insert(email);
                }
            }
            Some("group") => {
                if let Some(email) = permission["emailAddress"].as_str() {
                    out.insert(format!("group:{}", email.trim().to_lowercase()));
                }
            }
            Some("domain") => {
                if let Some(domain) = permission["domain"].as_str() {
                    out.insert(format!("domain:{}", domain.trim().to_lowercase()));
                }
            }
            // "anyone" (public or link sharing) is never treated as access.
            _ => {}
        }
    }
    out.into_iter().collect()
}

pub(crate) fn parse_file(file: &Value) -> Option<Change> {
    let id = file["id"].as_str()?.to_string();
    if file["trashed"].as_bool() == Some(true) {
        return Some(Change::Delete(id));
    }
    let mime_type = file["mimeType"].as_str().unwrap_or("").to_string();
    if mime_type == FOLDER {
        return None;
    }
    let name = file["name"].as_str().unwrap_or("").to_string();
    let access = match file["permissions"].as_array() {
        Some(permissions) => Access::Known(permission_principals(permissions)),
        // Drive only lists permissions to people who may share the file. Without them we do not
        // know who may read it, so nobody is told it exists.
        None => Access::Unknown,
    };
    Some(Change::Upsert(RemoteFile {
        external_id: id,
        path: name.clone(),
        name,
        mime_type,
        size: file["size"]
            .as_str()
            .and_then(|size| size.parse().ok())
            .unwrap_or(0),
        modified_at: epoch(&file["modifiedTime"]),
        version: file["md5Checksum"]
            .as_str()
            .or_else(|| file["version"].as_str())
            .or_else(|| file["modifiedTime"].as_str())
            .unwrap_or("")
            .to_string(),
        access: Some(access),
        web_url: web_link(&file["webViewLink"]),
    }))
}

impl GoogleConnector {
    fn url(&self, path: &str) -> String {
        format!("{}/drive/v3/{path}", self.base)
    }

    async fn start_token(&self) -> Result<String, ConnectorError> {
        let body = self
            .api
            .get_json(
                &self.url("changes/startPageToken"),
                &[("supportsAllDrives", "true")],
            )
            .await?;
        body["startPageToken"]
            .as_str()
            .map(str::to_string)
            .ok_or_else(|| ConnectorError::Other("provider sent no start token".into()))
    }

    async fn list_page(
        &self,
        page: Option<&str>,
        start: &str,
    ) -> Result<ChangePage, ConnectorError> {
        let fields = format!("nextPageToken,files({FILE_FIELDS})");
        let query_text = format!("trashed = false and mimeType != '{FOLDER}'");
        let mut query = vec![
            ("q", query_text.as_str()),
            ("fields", fields.as_str()),
            ("pageSize", "200"),
            ("supportsAllDrives", "true"),
            ("includeItemsFromAllDrives", "true"),
            ("corpora", "allDrives"),
        ];
        if let Some(token) = page {
            query.push(("pageToken", token));
        }
        let body = self.api.get_json(&self.url("files"), &query).await?;
        let changes = body["files"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_file)
            .collect();
        let (cursor, has_more) = match body["nextPageToken"].as_str() {
            Some(next) => (
                json!({ "phase": "list", "page": next, "start": start }),
                true,
            ),
            None => (json!({ "phase": "changes", "page": start }), false),
        };
        Ok(ChangePage {
            changes,
            cursor: cursor.to_string(),
            has_more,
        })
    }

    async fn changes_page(&self, token: &str) -> Result<ChangePage, ConnectorError> {
        let fields =
            format!("nextPageToken,newStartPageToken,changes(fileId,removed,file({FILE_FIELDS}))");
        let body = self
            .api
            .get_json(
                &self.url("changes"),
                &[
                    ("pageToken", token),
                    ("fields", fields.as_str()),
                    ("pageSize", "200"),
                    ("includeRemoved", "true"),
                    ("supportsAllDrives", "true"),
                    ("includeItemsFromAllDrives", "true"),
                ],
            )
            .await
            // A start token that Google no longer recognises means we must list everything again.
            .map_err(|error| match error {
                ConnectorError::BadRequest | ConnectorError::NotFound => {
                    ConnectorError::CursorExpired
                }
                other => other,
            })?;
        let changes = body["changes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|change| {
                let id = change["fileId"].as_str()?;
                if change["removed"].as_bool() == Some(true) {
                    return Some(Change::Delete(id.to_string()));
                }
                parse_file(&change["file"])
            })
            .collect();
        if let Some(next) = body["nextPageToken"].as_str() {
            return Ok(ChangePage {
                changes,
                cursor: json!({ "phase": "changes", "page": next }).to_string(),
                has_more: true,
            });
        }
        let start = body["newStartPageToken"]
            .as_str()
            .ok_or_else(|| ConnectorError::Other("provider sent no new start token".into()))?;
        Ok(ChangePage {
            changes,
            cursor: json!({ "phase": "changes", "page": start }).to_string(),
            has_more: false,
        })
    }
}

#[async_trait]
impl Connector for GoogleConnector {
    async fn changes(&self, cursor: Option<&str>) -> Result<ChangePage, ConnectorError> {
        let Some(cursor) = cursor else {
            // Take the changes start token first, then list: anything that changes while the
            // listing runs is then picked up by the changes feed afterwards.
            let start = self.start_token().await?;
            return self.list_page(None, &start).await;
        };
        let parsed: Value =
            serde_json::from_str(cursor).map_err(|_| ConnectorError::CursorExpired)?;
        let page = parsed["page"]
            .as_str()
            .ok_or(ConnectorError::CursorExpired)?;
        match parsed["phase"].as_str() {
            Some("list") => {
                let start = parsed["start"]
                    .as_str()
                    .ok_or(ConnectorError::CursorExpired)?;
                self.list_page(Some(page), start).await
            }
            Some("changes") => self.changes_page(page).await,
            _ => Err(ConnectorError::CursorExpired),
        }
    }

    async fn access(&self, file: &RemoteFile) -> Result<Access, ConnectorError> {
        // Drive hides sharing details from people who cannot share a file. The account that
        // connected the source can open everything it lists, so it is always a reader.
        Ok(with_extra_readers(
            file.access.clone().unwrap_or(Access::Unknown),
            &self.always_readable_by,
        ))
    }

    async fn text(&self, file: &RemoteFile) -> Result<Option<String>, ConnectorError> {
        let export = match file.mime_type.as_str() {
            "application/vnd.google-apps.document" | "application/vnd.google-apps.presentation" => {
                Some("text/plain")
            }
            "application/vnd.google-apps.spreadsheet" => Some("text/csv"),
            other if other.starts_with("application/vnd.google-apps.") => return Ok(None),
            _ => None,
        };
        let id = &file.external_id;
        let bytes = match export {
            Some(target) => {
                self.api
                    .get_bytes(
                        &self.url(&format!("files/{id}/export")),
                        &[("mimeType", target)],
                    )
                    .await?
            }
            None if is_plain_text(&file.mime_type, &file.name)
                && (file.size as usize) <= MAX_TEXT_BYTES =>
            {
                self.api
                    .get_bytes(
                        &self.url(&format!("files/{id}")),
                        &[("alt", "media"), ("supportsAllDrives", "true")],
                    )
                    .await?
            }
            None => return Ok(None),
        };
        Ok(bytes.map(|bytes| decode_text(&bytes)))
    }

    async fn bytes(&self, file: &RemoteFile) -> Result<Option<Vec<u8>>, ConnectorError> {
        // Google's own formats are exported as text instead, so only uploaded files come here.
        if file.mime_type.starts_with("application/vnd.google-apps.")
            || kind_for(&file.name, &file.mime_type).is_none()
            || file.size as usize > download_limit(&file.name, &file.mime_type)
        {
            return Ok(None);
        }
        let url = self.url(&format!("files/{}", file.external_id));
        self.api
            .get_bytes_capped(
                &url,
                &[("alt", "media"), ("supportsAllDrives", "true")],
                MAX_DOWNLOAD_BYTES,
            )
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_map_to_the_people_who_may_read() {
        let permissions = vec![
            json!({ "type": "user", "role": "owner", "emailAddress": "Alice@Firm.com" }),
            json!({ "type": "group", "role": "reader", "emailAddress": "Partners@firm.com" }),
            json!({ "type": "domain", "role": "reader", "domain": "Firm.com" }),
            json!({ "type": "anyone", "role": "reader" }),
            json!({ "type": "user", "role": "writer", "emailAddress": "gone@firm.com", "deleted": true }),
            json!({ "type": "user", "role": "none", "emailAddress": "none@firm.com" }),
        ];
        assert_eq!(
            permission_principals(&permissions),
            [
                "domain:firm.com",
                "email:alice@firm.com",
                "group:partners@firm.com"
            ]
        );
    }

    #[test]
    fn files_folders_and_trash_are_told_apart() {
        let shared = json!({
            "id": "1", "name": "Notes.txt", "mimeType": "text/plain", "size": "12",
            "modifiedTime": "2026-10-01T12:00:00Z", "md5Checksum": "abc",
            "permissions": [{ "type": "user", "role": "owner", "emailAddress": "a@firm.com" }]
        });
        let Some(Change::Upsert(file)) = parse_file(&shared) else {
            panic!("file")
        };
        assert_eq!((file.size, file.version.as_str()), (12, "abc"));
        assert_eq!(
            file.access,
            Some(Access::Known(vec!["email:a@firm.com".into()]))
        );

        // No permission list: we cannot tell who may read it, so it stays hidden.
        let hidden = json!({ "id": "2", "name": "x", "mimeType": "text/plain" });
        let Some(Change::Upsert(file)) = parse_file(&hidden) else {
            panic!("file")
        };
        assert_eq!(file.access, Some(Access::Unknown));

        assert_eq!(parse_file(&json!({ "id": "3", "mimeType": FOLDER })), None);
        assert_eq!(
            parse_file(&json!({ "id": "4", "trashed": true })),
            Some(Change::Delete("4".into()))
        );
    }
}
