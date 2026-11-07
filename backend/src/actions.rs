//! Emails and meetings an agent has prepared for a person to send or schedule themselves.
//!
//! An agent can only propose. The person reads exactly what would go out, may change it, and
//! approves; the desktop app then opens their own mail or calendar with it filled in and they
//! press Send. This server never connects to a mailbox or a calendar, so a proposal cannot be
//! sent, booked or changed by anyone but the person it is for.

use chrono::NaiveDateTime;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use uuid::Uuid;

use crate::{Result, auth::Principal, db::TenantTx};

pub const MAX_RECIPIENTS: usize = 25;
pub const MAX_SUBJECT_CHARS: usize = 200;
pub const MAX_BODY_CHARS: usize = 20_000;
pub const MAX_MEETING_NOTES_CHARS: usize = 5_000;
/// Proposals one task may make, so a confused or manipulated agent cannot bury a person in them.
pub const MAX_PER_TASK: i64 = 5;
const MAX_MEETING_HOURS: i64 = 14;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Payload {
    Email {
        to: Vec<String>,
        #[serde(default)]
        cc: Vec<String>,
        subject: String,
        body: String,
    },
    Meeting {
        title: String,
        /// Local time in the person's own calendar, as "2026-11-05T14:00".
        start: String,
        end: String,
        #[serde(default)]
        attendees: Vec<String>,
        #[serde(default)]
        location: String,
        #[serde(default)]
        notes: String,
    },
}

/// A plain email address: something@something.something, with nothing that could add a header or
/// a second recipient. Returns it lower-cased.
pub fn clean_address(address: &str) -> Option<String> {
    let address = address.trim().to_lowercase();
    let (name, domain) = address.split_once('@')?;
    let ok = |part: &str, extra: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric() || extra.contains(c));
    (address.len() <= 254 && ok(name, "._%+-") && ok(domain, ".-") && domain.contains('.') && !domain.starts_with('.') && !domain.ends_with('.') && !domain.contains("..")).then_some(address)
}

fn clean_addresses(list: &[String]) -> std::result::Result<Vec<String>, &'static str> {
    let mut out = Vec::new();
    for item in list {
        let address = clean_address(item).ok_or("one of the email addresses is not valid")?;
        if !out.contains(&address) {
            out.push(address);
        }
    }
    Ok(out)
}

fn clean_text(text: &str) -> String {
    text.replace('\0', "").replace("\r\n", "\n").trim().to_string()
}

fn parse_time(text: &str) -> Option<NaiveDateTime> {
    ["%Y-%m-%dT%H:%M", "%Y-%m-%dT%H:%M:%S"].iter().find_map(|format| NaiveDateTime::parse_from_str(text.trim(), format).ok())
}

impl Payload {
    pub fn kind(&self) -> &'static str {
        match self {
            Payload::Email { .. } => "email",
            Payload::Meeting { .. } => "meeting",
        }
    }

    /// The payload cleaned up, or why it cannot be used. The reason is written for the agent, so it
    /// can correct itself, and for the person editing.
    pub fn cleaned(&self) -> std::result::Result<Payload, &'static str> {
        match self {
            Payload::Email { to, cc, subject, body } => {
                let to = clean_addresses(to)?;
                let cc: Vec<String> = clean_addresses(cc)?.into_iter().filter(|a| !to.contains(a)).collect();
                if to.is_empty() {
                    return Err("an email needs at least one recipient");
                }
                if to.len() + cc.len() > MAX_RECIPIENTS {
                    return Err("too many recipients");
                }
                let subject = clean_text(subject).replace('\n', " ");
                let body = clean_text(body);
                if subject.is_empty() || subject.chars().count() > MAX_SUBJECT_CHARS {
                    return Err("the subject must be 1 to 200 characters");
                }
                if body.is_empty() || body.chars().count() > MAX_BODY_CHARS {
                    return Err("the message must be 1 to 20,000 characters");
                }
                Ok(Payload::Email { to, cc, subject, body })
            }
            Payload::Meeting { title, start, end, attendees, location, notes } => {
                let (Some(from), Some(until)) = (parse_time(start), parse_time(end)) else {
                    return Err("times must look like 2026-11-05T14:00");
                };
                if until <= from {
                    return Err("the meeting must end after it starts");
                }
                if (until - from).num_hours() > MAX_MEETING_HOURS {
                    return Err("a meeting cannot be longer than 14 hours");
                }
                let attendees = clean_addresses(attendees)?;
                if attendees.len() > MAX_RECIPIENTS {
                    return Err("too many attendees");
                }
                let title = clean_text(title).replace('\n', " ");
                let location = clean_text(location).replace('\n', " ");
                let notes = clean_text(notes);
                if title.is_empty() || title.chars().count() > MAX_SUBJECT_CHARS || location.chars().count() > MAX_SUBJECT_CHARS {
                    return Err("the title must be 1 to 200 characters");
                }
                if notes.chars().count() > MAX_MEETING_NOTES_CHARS {
                    return Err("the notes are too long");
                }
                Ok(Payload::Meeting {
                    title,
                    start: from.format("%Y-%m-%dT%H:%M").to_string(),
                    end: until.format("%Y-%m-%dT%H:%M").to_string(),
                    attendees,
                    location,
                    notes,
                })
            }
        }
    }

    fn addresses(&self) -> Vec<&str> {
        match self {
            Payload::Email { to, cc, .. } => to.iter().chain(cc).map(String::as_str).collect(),
            Payload::Meeting { attendees, .. } => attendees.iter().map(String::as_str).collect(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ActionView {
    pub id: Uuid,
    pub task_id: Uuid,
    /// The task it came from, and the agent that made it.
    pub task_title: String,
    pub agent_name: String,
    #[serde(flatten)]
    pub payload: Payload,
    pub status: String,
    pub edited: bool,
    /// People outside the requester's own email domain, so the person can look twice before sending.
    pub outside: Vec<String>,
    pub created_epoch: f64,
}

fn domain_of(email: &str) -> Option<&str> {
    email.rsplit_once('@').map(|(_, domain)| domain)
}

/// Addresses at a different domain from the person's own. If their own email is unknown, none are
/// flagged, because there is nothing to compare with.
pub(crate) fn outside(payload: &Payload, own: Option<&str>) -> Vec<String> {
    let Some(own) = own.and_then(domain_of) else { return Vec::new() };
    payload.addresses().into_iter().filter(|a| domain_of(a) != Some(own)).map(str::to_string).collect()
}

const COLUMNS: &str = "a.id, a.task_id, t.title, v.name, a.payload, a.status, a.edited, EXTRACT(EPOCH FROM a.created_at)::float8, t.requester_email";
const FROM: &str = "FROM agent_actions a JOIN agent_tasks t ON t.tenant_id = a.tenant_id AND t.id = a.task_id
        JOIN agent_versions v ON v.tenant_id = t.tenant_id AND v.agent_id = t.agent_id AND v.version = t.agent_version";

fn view(row: &tokio_postgres::Row) -> Option<ActionView> {
    let payload: Payload = serde_json::from_value(row.get::<_, Value>(4)).ok()?;
    let own: Option<String> = row.get(8);
    Some(ActionView {
        id: row.get(0),
        task_id: row.get(1),
        task_title: row.get(2),
        agent_name: row.get(3),
        outside: outside(&payload, own.as_deref()),
        payload,
        status: row.get(5),
        edited: row.get(6),
        created_epoch: row.get(7),
    })
}

/// Records a proposal from a task. False if the task has made as many as it may.
pub async fn propose(tx: &TenantTx<'_>, task: Uuid, requested_by: &str, payload: &Payload) -> Result<bool> {
    let sql = tx.sql();
    let made: i64 = sql.query_one("SELECT count(*) FROM agent_actions WHERE task_id = $1", &[&task]).await?.get(0);
    if made >= MAX_PER_TASK {
        return Ok(false);
    }
    sql.execute(
        "INSERT INTO agent_actions (tenant_id, task_id, requested_by, kind, payload, created_at) VALUES (app_tenant(), $1, $2, $3, $4, clock_timestamp())",
        &[&task, &requested_by, &payload.kind(), &json!(payload)],
    )
    .await?;
    Ok(true)
}

/// What is waiting for this person, newest first, so the draft they have just asked for is on top.
pub async fn waiting(tx: &TenantTx<'_>, principal: &Principal) -> Result<Vec<ActionView>> {
    Ok(tx
        .sql()
        .query(&format!("SELECT {COLUMNS} {FROM} WHERE a.requested_by = $1 AND a.status = 'proposed' ORDER BY a.created_at DESC"), &[&principal.user])
        .await?
        .iter()
        .filter_map(view)
        .collect())
}

/// What one of this person's tasks prepared, and what became of it.
pub async fn for_task(tx: &TenantTx<'_>, principal: &Principal, task: Uuid) -> Result<Vec<ActionView>> {
    Ok(tx
        .sql()
        .query(&format!("SELECT {COLUMNS} {FROM} WHERE a.requested_by = $1 AND a.task_id = $2 ORDER BY a.created_at"), &[&principal.user, &task])
        .await?
        .iter()
        .filter_map(view)
        .collect())
}

pub enum Decided {
    Done(Payload),
    /// No such proposal, it is not this person's, or it has been decided already.
    NotFound,
    Invalid(&'static str),
}

/// Approves a proposal, with whatever the person changed. They then send it themselves.
pub async fn approve(tx: &TenantTx<'_>, principal: &Principal, id: Uuid, final_text: Option<&Payload>) -> Result<Decided> {
    let sql = tx.sql();
    let Some(row) = sql
        .query_opt("SELECT kind, payload FROM agent_actions WHERE id = $1 AND requested_by = $2 AND status = 'proposed' FOR UPDATE", &[&id, &principal.user])
        .await?
    else {
        return Ok(Decided::NotFound);
    };
    let original: Payload = match serde_json::from_value(row.get::<_, Value>(1)) {
        Ok(original) => original,
        Err(_) => return Ok(Decided::NotFound),
    };
    let chosen = final_text.unwrap_or(&original);
    // An edit cannot turn an email into a meeting.
    if chosen.kind() != original.kind() {
        return Ok(Decided::Invalid("an email cannot become a meeting"));
    }
    let cleaned = match chosen.cleaned() {
        Ok(cleaned) => cleaned,
        Err(reason) => return Ok(Decided::Invalid(reason)),
    };
    let edited = cleaned != original.cleaned().unwrap_or(original.clone());
    sql.execute(
        "UPDATE agent_actions SET status = 'approved', decided_at = now(), payload = $2, edited = $3 WHERE id = $1",
        &[&id, &json!(cleaned), &edited],
    )
    .await?;
    Ok(Decided::Done(cleaned))
}

pub async fn dismiss(tx: &TenantTx<'_>, principal: &Principal, id: Uuid) -> Result<bool> {
    Ok(tx
        .sql()
        .execute(
            "UPDATE agent_actions SET status = 'dismissed', decided_at = now() WHERE id = $1 AND requested_by = $2 AND status = 'proposed'",
            &[&id, &principal.user],
        )
        .await?
        > 0)
}

/// An email ready to send: cleaned, and with the people outside the person's organisation.
pub struct Sending {
    pub email: Payload,
    pub edited: bool,
    pub outside: Vec<String>,
}

/// Finds the person's own email that is waiting to be sent and holds it until the transaction ends,
/// so two attempts cannot both send it. `final_text` is what they changed it to, if anything.
pub async fn lock_for_sending(tx: &TenantTx<'_>, principal: &Principal, id: Uuid, final_text: Option<&Payload>) -> Result<ToSend> {
    let sql = tx.sql();
    let Some(row) = sql
        .query_opt(
            "SELECT a.payload, t.requester_email FROM agent_actions a
             JOIN agent_tasks t ON t.tenant_id = a.tenant_id AND t.id = a.task_id
             WHERE a.id = $1 AND a.requested_by = $2 AND a.status = 'proposed' AND a.kind = 'email' FOR UPDATE OF a",
            &[&id, &principal.user],
        )
        .await?
    else {
        return Ok(ToSend::NotFound);
    };
    let Ok(original) = serde_json::from_value::<Payload>(row.get::<_, Value>(0)) else { return Ok(ToSend::NotFound) };
    let chosen = final_text.unwrap_or(&original);
    if chosen.kind() != "email" {
        return Ok(ToSend::Invalid("only an email can be sent"));
    }
    let email = match chosen.cleaned() {
        Ok(cleaned) => cleaned,
        Err(reason) => return Ok(ToSend::Invalid(reason)),
    };
    let edited = email != original.cleaned().unwrap_or(original.clone());
    let own: Option<String> = row.get(1);
    let outside = outside(&email, own.as_deref());
    Ok(ToSend::Ready(Sending { email, edited, outside }))
}

impl Sending {
    pub fn email_recipients(&self) -> usize {
        self.email.addresses().len()
    }
}

pub enum ToSend {
    Ready(Sending),
    NotFound,
    Invalid(&'static str),
}

/// Records that it was sent, from which address, exactly as sent.
pub async fn mark_sent(tx: &TenantTx<'_>, id: Uuid, sent: &Sending, from: &str) -> Result<()> {
    tx.sql()
        .execute(
            "UPDATE agent_actions SET status = 'sent', decided_at = now(), sent_at = now(), sent_from = $2, payload = $3, edited = $4 WHERE id = $1",
            &[&id, &from, &json!(sent.email), &sent.edited],
        )
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn email(to: &[&str], subject: &str, body: &str) -> Payload {
        Payload::Email { to: to.iter().map(|s| s.to_string()).collect(), cc: vec![], subject: subject.into(), body: body.into() }
    }

    #[test]
    fn only_plain_addresses_are_accepted_and_nothing_can_add_a_header_or_a_second_recipient() {
        assert_eq!(clean_address(" Alice@Firm.COM "), Some("alice@firm.com".into()));
        for bad in ["alice", "a@b", "a@@b.com", "a b@c.com", "a@b.com,c@d.com", "a@b.com\nBcc: x@y.com", "\"a\"@b.com", "<a@b.com>", "a@.com", "a@b..com", "@b.com"] {
            assert_eq!(clean_address(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn an_email_is_cleaned_and_refused_when_it_cannot_be_sent_sensibly() {
        let cleaned = Payload::Email { to: vec!["A@x.com".into(), "a@x.com".into()], cc: vec!["a@x.com".into(), "b@x.com".into()], subject: "Hi\nthere".into(), body: " Hello\r\nworld \0".into() }.cleaned().unwrap();
        assert_eq!(cleaned, Payload::Email { to: vec!["a@x.com".into()], cc: vec!["b@x.com".into()], subject: "Hi there".into(), body: "Hello\nworld".into() });
        assert!(email(&[], "s", "b").cleaned().is_err());
        assert!(email(&["a@x.com"], "", "b").cleaned().is_err());
        assert!(email(&["a@x.com"], "s", "  ").cleaned().is_err());
        assert!(email(&["not an address"], "s", "b").cleaned().is_err());
        let many: Vec<String> = (0..26).map(|n| format!("p{n}@x.com")).collect();
        assert!(Payload::Email { to: many, cc: vec![], subject: "s".into(), body: "b".into() }.cleaned().is_err());
        assert!(email(&["a@x.com"], &"s".repeat(201), "b").cleaned().is_err());
    }

    #[test]
    fn a_meeting_needs_real_times_that_run_forwards_for_a_sensible_length() {
        let meeting = |start: &str, end: &str| Payload::Meeting { title: "Review".into(), start: start.into(), end: end.into(), attendees: vec!["a@x.com".into()], location: "".into(), notes: "".into() };
        let ok = meeting("2026-11-05T14:00", "2026-11-05T15:30:00").cleaned().unwrap();
        let Payload::Meeting { start, end, .. } = ok else { panic!() };
        assert_eq!((start.as_str(), end.as_str()), ("2026-11-05T14:00", "2026-11-05T15:30"));
        assert!(meeting("2026-11-05T15:00", "2026-11-05T14:00").cleaned().is_err());
        assert!(meeting("2026-11-05T14:00", "2026-11-05T14:00").cleaned().is_err());
        assert!(meeting("tomorrow", "later").cleaned().is_err());
        assert!(meeting("2026-11-05T06:00", "2026-11-06T06:00").cleaned().is_err(), "longer than 14 hours");
    }

    #[test]
    fn people_outside_the_requesters_own_domain_are_flagged() {
        let p = Payload::Email { to: vec!["colleague@firm.com".into(), "client@other.example".into()], cc: vec![], subject: "s".into(), body: "b".into() };
        assert_eq!(outside(&p, Some("me@firm.com")), ["client@other.example"]);
        assert!(outside(&p, None).is_empty());
    }
}
