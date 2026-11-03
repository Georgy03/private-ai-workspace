//! Microsoft 365 (OneDrive and SharePoint document libraries) through Microsoft Graph.
//!
//! One connection reads one drive (a SharePoint library or a OneDrive), listed with the delta
//! API so later syncs only see what changed.

use std::collections::BTreeSet;

use async_trait::async_trait;
use serde_json::Value;

use crate::extract::kind_for;

use super::{
    Access, Api, Change, ChangePage, Connector, ConnectorError, MAX_DOWNLOAD_BYTES, MAX_TEXT_BYTES,
    RemoteFile, TokenSource, decode_text, download_limit, email_principal, is_plain_text,
    same_origin, web_link, with_extra_readers,
};

pub const GRAPH_BASE: &str = "https://graph.microsoft.com";
const MAX_PERMISSION_PAGES: usize = 10;

pub struct MicrosoftConnector {
    api: Api,
    base: String,
    drive_id: String,
    /// Email domains of the firm. "Anyone in the organization" links grant these domains.
    domains: Vec<String>,
    /// Identities that may read every file this connection returns (see `with_extra_readers`).
    always_readable_by: Vec<String>,
}

impl MicrosoftConnector {
    pub fn new(
        tokens: Box<dyn TokenSource>,
        base: &str,
        drive_id: &str,
        domains: Vec<String>,
    ) -> Result<Self, ConnectorError> {
        Ok(Self {
            api: Api::new(tokens)?,
            base: base.trim_end_matches('/').to_string(),
            drive_id: drive_id.to_string(),
            domains: domains
                .into_iter()
                .map(|d| d.trim().to_lowercase())
                .collect(),
            always_readable_by: Vec::new(),
        })
    }

    pub fn always_readable_by(mut self, readers: Vec<String>) -> Self {
        self.always_readable_by = readers;
        self
    }

    fn drive(&self) -> String {
        format!("{}/v1.0/drives/{}", self.base, self.drive_id)
    }
}

fn epoch(value: &Value) -> i64 {
    value
        .as_str()
        .and_then(|text| chrono::DateTime::parse_from_rfc3339(text).ok())
        .map(|time| time.timestamp())
        .unwrap_or(0)
}

/// One entry of a delta page: a file to store, a file that is gone, or nothing (folders).
pub(crate) fn parse_item(item: &Value) -> Option<Change> {
    let id = item["id"].as_str()?.to_string();
    if item.get("deleted").is_some() {
        return Some(Change::Delete(id));
    }
    if item.get("root").is_some() || item.get("file").is_none() {
        return None;
    }
    let version = item["cTag"]
        .as_str()
        .or_else(|| item["eTag"].as_str())
        .or_else(|| item["lastModifiedDateTime"].as_str())
        .unwrap_or("")
        .to_string();
    let name = item["name"].as_str().unwrap_or("").to_string();
    Some(Change::Upsert(RemoteFile {
        external_id: id,
        path: name.clone(),
        name,
        mime_type: item["file"]["mimeType"].as_str().unwrap_or("").to_string(),
        size: item["size"].as_u64().unwrap_or(0),
        modified_at: epoch(&item["lastModifiedDateTime"]),
        version,
        access: None,
        web_url: web_link(&item["webUrl"]),
    }))
}

fn identity_principals(identity: &Value, out: &mut BTreeSet<String>) {
    if let Some(email) = identity["user"]["email"].as_str().and_then(email_principal) {
        out.insert(email);
    }
    // SharePoint users often arrive as a login name such as "i:0#.f|membership|alice@firm.com".
    if let Some(login) = identity["siteUser"]["loginName"].as_str()
        && let Some(email) = login.rsplit('|').next().and_then(email_principal)
    {
        out.insert(email);
    }
    if let Some(email) = identity["siteUser"]["email"]
        .as_str()
        .and_then(email_principal)
    {
        out.insert(email);
    }
    if let Some(id) = identity["group"]["id"].as_str() {
        out.insert(format!("group:{}", id.to_lowercase()));
    }
    if let Some(email) = identity["group"]["email"].as_str() {
        out.insert(format!("group:{}", email.trim().to_lowercase()));
    }
    // SharePoint groups ("siteGroup") need their members looked up, so they grant nobody here.
}

/// Turns Graph's permission list for one file into the identities that may read it.
pub(crate) fn permission_principals(permissions: &[Value], domains: &[String]) -> Vec<String> {
    let mut out = BTreeSet::new();
    for permission in permissions {
        let has_role = permission["roles"]
            .as_array()
            .is_some_and(|roles| !roles.is_empty());
        if !has_role {
            continue;
        }
        identity_principals(&permission["grantedToV2"], &mut out);
        for identity in permission["grantedToIdentitiesV2"]
            .as_array()
            .into_iter()
            .flatten()
        {
            identity_principals(identity, &mut out);
        }
        // Sharing links: "organization" reaches everyone at the firm; "anonymous" is never
        // treated as access, because a link is not a reason to show a file in search.
        if permission["link"]["scope"].as_str() == Some("organization") {
            out.extend(domains.iter().map(|domain| format!("domain:{domain}")));
        }
    }
    out.into_iter().collect()
}

#[async_trait]
impl Connector for MicrosoftConnector {
    async fn changes(&self, cursor: Option<&str>) -> Result<ChangePage, ConnectorError> {
        let page = match cursor {
            Some(link) if same_origin(&self.base, link) => self.api.get_json(link, &[]).await,
            // A saved link that points elsewhere is not one of ours: start over rather than
            // send our token to it.
            Some(_) => return Err(ConnectorError::CursorExpired),
            None => {
                self.api
                    .get_json(&format!("{}/root/delta", self.drive()), &[("$top", "200")])
                    .await
            }
        }?;
        let changes = page["value"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(parse_item)
            .collect();
        if let Some(next) = page["@odata.nextLink"].as_str() {
            if !same_origin(&self.base, next) {
                return Err(ConnectorError::Other(
                    "provider returned a foreign link".into(),
                ));
            }
            return Ok(ChangePage {
                changes,
                cursor: next.to_string(),
                has_more: true,
            });
        }
        let delta = page["@odata.deltaLink"]
            .as_str()
            .filter(|link| same_origin(&self.base, link))
            .ok_or_else(|| ConnectorError::Other("provider sent no continuation link".into()))?;
        Ok(ChangePage {
            changes,
            cursor: delta.to_string(),
            has_more: false,
        })
    }

    async fn access(&self, file: &RemoteFile) -> Result<Access, ConnectorError> {
        let access = self.permissions_for(file).await?;
        Ok(with_extra_readers(access, &self.always_readable_by))
    }

    async fn text(&self, file: &RemoteFile) -> Result<Option<String>, ConnectorError> {
        if !is_plain_text(&file.mime_type, &file.name) || file.size as usize > MAX_TEXT_BYTES {
            return Ok(None);
        }
        let url = format!("{}/items/{}/content", self.drive(), file.external_id);
        Ok(self
            .api
            .get_bytes(&url, &[])
            .await?
            .map(|bytes| decode_text(&bytes)))
    }

    async fn bytes(&self, file: &RemoteFile) -> Result<Option<Vec<u8>>, ConnectorError> {
        if kind_for(&file.name, &file.mime_type).is_none()
            || file.size as usize > download_limit(&file.name, &file.mime_type)
        {
            return Ok(None);
        }
        let url = format!("{}/items/{}/content", self.drive(), file.external_id);
        self.api
            .get_bytes_capped(&url, &[], MAX_DOWNLOAD_BYTES)
            .await
    }
}

impl MicrosoftConnector {
    /// What Graph says about who may read the file.
    async fn permissions_for(&self, file: &RemoteFile) -> Result<Access, ConnectorError> {
        let mut permissions: Vec<Value> = Vec::new();
        let mut url = format!("{}/items/{}/permissions", self.drive(), file.external_id);
        for _ in 0..MAX_PERMISSION_PAGES {
            let page = match self.api.get_json(&url, &[]).await {
                Ok(page) => page,
                Err(ConnectorError::NotFound | ConnectorError::Forbidden) => {
                    return Ok(Access::Unknown);
                }
                Err(error) => return Err(error),
            };
            permissions.extend(page["value"].as_array().cloned().unwrap_or_default());
            match page["@odata.nextLink"].as_str() {
                Some(next) if same_origin(&self.base, next) => url = next.to_string(),
                Some(_) => return Ok(Access::Unknown),
                None => {
                    return Ok(Access::Known(permission_principals(
                        &permissions,
                        &self.domains,
                    )));
                }
            }
        }
        // More pages than we are willing to read: refuse to guess.
        Ok(Access::Unknown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn delta_entries_become_files_deletions_or_nothing() {
        let file = json!({
            "id": "f1", "name": "Retainer.txt", "size": 42, "cTag": "c1", "eTag": "e1",
            "lastModifiedDateTime": "2026-10-01T12:00:00Z", "file": { "mimeType": "text/plain" }
        });
        let Some(Change::Upsert(parsed)) = parse_item(&file) else {
            panic!("file")
        };
        assert_eq!(
            (
                parsed.name.as_str(),
                parsed.version.as_str(),
                parsed.size,
                parsed.modified_at
            ),
            ("Retainer.txt", "c1", 42, 1_790_856_000)
        );
        assert_eq!(
            parse_item(&json!({ "id": "f2", "deleted": {} })),
            Some(Change::Delete("f2".into()))
        );
        assert_eq!(
            parse_item(&json!({ "id": "d1", "name": "Folder", "folder": {} })),
            None
        );
        assert_eq!(
            parse_item(&json!({ "id": "r", "root": {}, "folder": {} })),
            None
        );
        assert_eq!(parse_item(&json!({ "name": "no id", "file": {} })), None);
    }

    #[test]
    fn permissions_map_to_the_people_who_may_read() {
        let permissions = vec![
            json!({ "roles": ["owner"], "grantedToV2": { "user": { "email": "Alice@Firm.com" } } }),
            json!({ "roles": ["read"], "grantedToV2": { "group": { "id": "AB-12", "email": "Partners@firm.com" } } }),
            json!({ "roles": ["write"], "grantedToIdentitiesV2": [
                { "user": { "email": "bob@firm.com" } },
                { "siteUser": { "loginName": "i:0#.f|membership|carol@firm.com" } },
                { "user": { "displayName": "No Email" } },
                { "siteGroup": { "id": "7" } },
            ] }),
            json!({ "roles": ["read"], "link": { "scope": "organization" } }),
            json!({ "roles": ["read"], "link": { "scope": "anonymous" } }),
            json!({ "roles": [], "grantedToV2": { "user": { "email": "norole@firm.com" } } }),
        ];
        let readers = permission_principals(&permissions, &["firm.com".into()]);
        assert_eq!(
            readers,
            [
                "domain:firm.com",
                "email:alice@firm.com",
                "email:bob@firm.com",
                "email:carol@firm.com",
                "group:ab-12",
                "group:partners@firm.com",
            ]
        );
        // Without configured domains, an organization link grants nobody.
        let readers = permission_principals(&permissions, &[]);
        assert!(!readers.iter().any(|reader| reader.starts_with("domain:")));
    }

    #[test]
    fn no_permissions_means_no_readers() {
        assert!(permission_principals(&[], &["firm.com".into()]).is_empty());
    }
}
