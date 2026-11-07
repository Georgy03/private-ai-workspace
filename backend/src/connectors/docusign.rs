//! DocuSign through the eSignature REST API.
//!
//! Each envelope becomes two searchable items: its documents combined into one PDF, and a short
//! text summary of its status (who sent it, who has signed, who is still outstanding), which is
//! what questions like "what is waiting for a signature?" need.
//!
//! Who may read an envelope is the person who sent it and everyone it was sent to, by email
//! address, as DocuSign records them. The account that connected the source can also read what it
//! lists. Everything this connector does is a read.

use std::{collections::HashMap, sync::Mutex};

use async_trait::async_trait;
use chrono::{DateTime, SecondsFormat};
use serde_json::{Value, json};

use super::{
    Access, Api, Change, ChangePage, Connector, ConnectorError, MAX_DOWNLOAD_BYTES, RemoteFile,
    TokenSource, email_principal, web_link, with_extra_readers,
};

const FIRST_LISTING: &str = "2000-01-01T00:00:00Z";
/// Envelopes listed again on each run beyond the last one seen, so a change made while a listing
/// was running is not missed. Unchanged ones are recognised by their version and not re-read.
const OVERLAP_SECS: i64 = 24 * 3600;
const PAGE: usize = 100;
const STATUSES: &str = "sent,delivered,completed,declined,voided,deleted";
const RECIPIENT_GROUPS: &[&str] = &["signers", "carbonCopies", "certifiedDeliveries", "agents", "editors", "intermediaries", "inPersonSigners"];
const MAX_REMEMBERED_SUMMARIES: usize = 2_000;
const SUMMARY_SUFFIX: &str = ":summary";

pub struct DocusignConnector {
    api: Api,
    /// The account's API address, as DocuSign reported it at sign-in.
    base: String,
    account: String,
    always_readable_by: Vec<String>,
    /// Summaries built while listing, so reading them needs no further call.
    summaries: Mutex<HashMap<String, String>>,
}

impl DocusignConnector {
    pub fn new(tokens: Box<dyn TokenSource>, base_uri: &str, account_id: &str) -> Result<Self, ConnectorError> {
        Ok(Self {
            api: Api::new(tokens)?,
            base: base_uri.trim_end_matches('/').to_string(),
            account: account_id.to_string(),
            always_readable_by: Vec::new(),
            summaries: Mutex::new(HashMap::new()),
        })
    }

    pub fn always_readable_by(mut self, readers: Vec<String>) -> Self {
        self.always_readable_by = readers;
        self
    }

    fn url(&self, path: &str) -> String {
        format!("{}/restapi/v2.1/accounts/{}/{path}", self.base, self.account)
    }
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn iso(epoch: i64) -> String {
    DateTime::from_timestamp(epoch, 0).map_or_else(|| FIRST_LISTING.to_string(), |d| d.to_rfc3339_opts(SecondsFormat::Secs, true))
}

fn epoch(value: &Value) -> i64 {
    value.as_str().and_then(|text| DateTime::parse_from_rfc3339(text).ok()).map_or(0, |time| time.timestamp())
}

/// Envelope IDs go into addresses, so only what an ID looks like is accepted.
fn safe_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
}

#[derive(Debug, Clone, PartialEq)]
struct Person {
    name: String,
    email: String,
    role: &'static str,
    status: String,
    signed: Option<String>,
}

fn people(envelope: &Value) -> Vec<Person> {
    let mut out = Vec::new();
    for group in RECIPIENT_GROUPS {
        let role = match *group {
            "signers" | "inPersonSigners" => "signer",
            "carbonCopies" => "copy",
            "certifiedDeliveries" => "receives a copy to sign",
            "agents" => "agent",
            "editors" => "editor",
            _ => "intermediary",
        };
        for item in envelope["recipients"][group].as_array().into_iter().flatten() {
            out.push(Person {
                name: item["name"].as_str().or_else(|| item["signerName"].as_str()).unwrap_or("").to_string(),
                email: item["email"].as_str().or_else(|| item["signerEmail"].as_str()).unwrap_or("").to_string(),
                role,
                status: item["status"].as_str().unwrap_or("").to_string(),
                signed: item["signedDateTime"].as_str().map(str::to_string),
            });
        }
    }
    out
}

/// The sender and everyone the envelope was sent to.
pub(crate) fn readers(envelope: &Value) -> Vec<String> {
    let mut out: Vec<String> = envelope["sender"]["email"]
        .as_str()
        .and_then(email_principal)
        .into_iter()
        .chain(people(envelope).iter().filter_map(|p| email_principal(&p.email)))
        .collect();
    out.sort();
    out.dedup();
    out
}

/// A plain description of where an envelope stands.
pub(crate) fn summary(envelope: &Value) -> String {
    let subject = envelope["emailSubject"].as_str().unwrap_or("(no subject)");
    let status = envelope["status"].as_str().unwrap_or("unknown");
    let mut text = format!("DocuSign envelope: {subject}\nStatus: {status}\n");
    let sender = &envelope["sender"];
    if let Some(name) = sender["userName"].as_str().or_else(|| sender["email"].as_str()) {
        text.push_str(&format!("Sent by: {name} ({})", sender["email"].as_str().unwrap_or("")));
        if let Some(sent) = envelope["sentDateTime"].as_str() {
            text.push_str(&format!(" on {sent}"));
        }
        text.push('\n');
    }
    if let Some(done) = envelope["completedDateTime"].as_str() {
        text.push_str(&format!("Completed on: {done}\n"));
    }
    if let Some(changed) = envelope["statusChangedDateTime"].as_str() {
        text.push_str(&format!("Status last changed: {changed}\n"));
    }
    let everyone = people(envelope);
    if !everyone.is_empty() {
        text.push_str("People on this envelope:\n");
        for person in &everyone {
            text.push_str(&format!("- {} ({}): {}, {}", person.name, person.email, person.role, person.status));
            if let Some(signed) = &person.signed {
                text.push_str(&format!(", signed on {signed}"));
            }
            text.push('\n');
        }
    }
    let waiting: Vec<&str> = everyone
        .iter()
        .filter(|p| p.role == "signer" && !["completed", "signed", "declined"].contains(&p.status.as_str()))
        .map(|p| if p.name.is_empty() { p.email.as_str() } else { p.name.as_str() })
        .collect();
    if ["sent", "delivered"].contains(&status) && !waiting.is_empty() {
        text.push_str(&format!("Waiting for a signature from: {}\n", waiting.join(", ")));
    }
    text.push_str(&format!("Envelope ID: {}\n", envelope["envelopeId"].as_str().unwrap_or("")));
    text
}

fn app_link(base: &str, id: &str) -> Option<String> {
    let host = if base.contains("demo") { "appdemo.docusign.com" } else { "app.docusign.com" };
    web_link(&json!(format!("https://{host}/documents/details/{id}")))
}

impl DocusignConnector {
    fn changes_for(&self, envelope: &Value) -> Vec<Change> {
        let Some(id) = envelope["envelopeId"].as_str().filter(|id| safe_id(id)) else { return Vec::new() };
        let summary_id = format!("{id}{SUMMARY_SUFFIX}");
        let status = envelope["status"].as_str().unwrap_or("");
        if status == "deleted" {
            return vec![Change::Delete(id.to_string()), Change::Delete(summary_id)];
        }
        let subject = envelope["emailSubject"].as_str().unwrap_or("(no subject)").to_string();
        let changed = envelope["statusChangedDateTime"].as_str().or_else(|| envelope["lastModifiedDateTime"].as_str()).unwrap_or("");
        let who = people(envelope);
        // Changes when the envelope's status does, or when someone on it signs or declines.
        let version = format!("{status}|{changed}|{}", who.iter().map(|p| p.status.as_str()).collect::<Vec<_>>().join(","));
        let modified_at = epoch(&envelope["statusChangedDateTime"]).max(epoch(&envelope["sentDateTime"]));
        let access = Access::Known(readers(envelope));
        let link = app_link(&self.base, id);
        let text = summary(envelope);
        let mut remembered = self.summaries.lock().unwrap();
        if remembered.len() >= MAX_REMEMBERED_SUMMARIES {
            remembered.clear();
        }
        let size = text.len() as u64;
        remembered.insert(summary_id.clone(), text);
        vec![
            Change::Upsert(RemoteFile {
                external_id: id.to_string(),
                name: format!("{subject}.pdf"),
                path: format!("DocuSign/{subject}"),
                mime_type: "application/pdf".into(),
                size: 0,
                modified_at,
                version: version.clone(),
                access: Some(access.clone()),
                web_url: link.clone(),
            }),
            Change::Upsert(RemoteFile {
                external_id: summary_id,
                name: format!("{subject} (signing status)"),
                path: format!("DocuSign/{subject}"),
                mime_type: "text/plain".into(),
                size,
                modified_at,
                version,
                access: Some(access),
                web_url: link,
            }),
        ]
    }
}

#[async_trait]
impl Connector for DocusignConnector {
    async fn changes(&self, cursor: Option<&str>) -> Result<ChangePage, ConnectorError> {
        let (since, start, run) = match cursor {
            None => (FIRST_LISTING.to_string(), 0usize, now()),
            Some(cursor) => {
                let parsed: Value = serde_json::from_str(cursor).map_err(|_| ConnectorError::CursorExpired)?;
                (
                    parsed["since"].as_str().ok_or(ConnectorError::CursorExpired)?.to_string(),
                    parsed["start"].as_u64().ok_or(ConnectorError::CursorExpired)? as usize,
                    parsed["run"].as_i64().ok_or(ConnectorError::CursorExpired)?,
                )
            }
        };
        let start_text = start.to_string();
        let count = PAGE.to_string();
        let body = self
            .api
            .get_json(
                &self.url("envelopes"),
                &[
                    ("from_date", since.as_str()),
                    ("status", STATUSES),
                    ("include", "recipients"),
                    ("count", count.as_str()),
                    ("start_position", start_text.as_str()),
                    ("order_by", "last_modified"),
                    ("order", "asc"),
                ],
            )
            .await?;
        let changes = body["envelopes"].as_array().into_iter().flatten().flat_map(|e| self.changes_for(e)).collect();
        let returned = body["envelopes"].as_array().map_or(0, Vec::len);
        let total = body["totalSetSize"].as_str().and_then(|t| t.parse::<usize>().ok()).or_else(|| body["totalSetSize"].as_u64().map(|t| t as usize)).unwrap_or(0);
        let next = start + returned;
        if returned > 0 && next < total {
            return Ok(ChangePage { changes, cursor: json!({ "since": since, "start": next, "run": run }).to_string(), has_more: true });
        }
        // Done: the next sync starts from when this one began, a little earlier.
        Ok(ChangePage { changes, cursor: json!({ "since": iso(run - OVERLAP_SECS), "start": 0, "run": now() }).to_string(), has_more: false })
    }

    async fn access(&self, file: &RemoteFile) -> Result<Access, ConnectorError> {
        Ok(with_extra_readers(file.access.clone().unwrap_or(Access::Unknown), &self.always_readable_by))
    }

    async fn text(&self, file: &RemoteFile) -> Result<Option<String>, ConnectorError> {
        let Some(id) = file.external_id.strip_suffix(SUMMARY_SUFFIX).filter(|id| safe_id(id)) else { return Ok(None) };
        if let Some(text) = self.summaries.lock().unwrap().remove(&file.external_id) {
            return Ok(Some(text));
        }
        // Not remembered (the listing ran in an earlier pass): ask again.
        let envelope = self.api.get_json(&self.url(&format!("envelopes/{id}")), &[("include", "recipients")]).await?;
        Ok(Some(summary(&envelope)))
    }

    async fn bytes(&self, file: &RemoteFile) -> Result<Option<Vec<u8>>, ConnectorError> {
        if file.external_id.ends_with(SUMMARY_SUFFIX) || !safe_id(&file.external_id) {
            return Ok(None);
        }
        self.api.get_bytes_capped(&self.url(&format!("envelopes/{}/documents/combined", file.external_id)), &[], MAX_DOWNLOAD_BYTES).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope() -> Value {
        json!({
            "envelopeId": "11111111-2222-3333-4444-555555555555", "status": "sent",
            "emailSubject": "Engagement letter - Whitfield", "sentDateTime": "2026-03-01T10:00:00Z",
            "statusChangedDateTime": "2026-03-02T09:00:00Z",
            "sender": { "userName": "Dana Partner", "email": "Dana@Firm.com" },
            "recipients": {
                "signers": [
                    { "name": "Dana Whitfield", "email": "dana@whitfield.example", "status": "completed", "signedDateTime": "2026-03-02T09:00:00Z" },
                    { "name": "Sam Whitfield", "email": "sam@whitfield.example", "status": "sent" },
                ],
                "carbonCopies": [{ "name": "Records", "email": "records@firm.com", "status": "completed" }],
            },
        })
    }

    #[test]
    fn the_sender_and_everyone_it_was_sent_to_may_read_it() {
        assert_eq!(
            readers(&envelope()),
            ["email:dana@firm.com", "email:dana@whitfield.example", "email:records@firm.com", "email:sam@whitfield.example"]
        );
    }

    #[test]
    fn the_summary_says_who_has_signed_and_who_is_still_outstanding() {
        let text = summary(&envelope());
        assert!(text.contains("Status: sent") && text.contains("Sent by: Dana Partner"));
        assert!(text.contains("Dana Whitfield (dana@whitfield.example): signer, completed, signed on 2026-03-02T09:00:00Z"));
        assert!(text.contains("Waiting for a signature from: Sam Whitfield"), "{text}");
        let mut done = envelope();
        done["status"] = json!("completed");
        assert!(!summary(&done).contains("Waiting for a signature"));
    }

    #[test]
    fn each_envelope_becomes_a_pdf_and_a_status_summary_and_a_deleted_one_removes_both() {
        let connector = DocusignConnector::new(Box::new(super::super::StaticToken("t".into())), "https://na3.docusign.net", "acct").unwrap();
        let changes = connector.changes_for(&envelope());
        let [Change::Upsert(pdf), Change::Upsert(status)] = &changes[..] else { panic!("{changes:?}") };
        assert_eq!(pdf.external_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(status.external_id, "11111111-2222-3333-4444-555555555555:summary");
        assert_eq!(pdf.access, status.access);
        assert_eq!(pdf.web_url.as_deref(), Some("https://app.docusign.com/documents/details/11111111-2222-3333-4444-555555555555"));
        // Someone signing changes the version, so the summary is read again.
        let mut signed = envelope();
        signed["recipients"]["signers"][1]["status"] = json!("completed");
        let Change::Upsert(after) = &connector.changes_for(&signed)[0] else { panic!() };
        assert_ne!(after.version, pdf.version);

        let mut deleted = envelope();
        deleted["status"] = json!("deleted");
        assert_eq!(connector.changes_for(&deleted).len(), 2);
        // An ID that is not an ID is never put in an address.
        let mut odd = envelope();
        odd["envelopeId"] = json!("../../users");
        assert!(connector.changes_for(&odd).is_empty());
    }

    #[test]
    fn times_round_trip() {
        assert_eq!(iso(0), "1970-01-01T00:00:00Z");
        assert_eq!(epoch(&json!("2026-03-02T09:00:00Z")), 1_772_442_000);
    }
}
