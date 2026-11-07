//! Sending an email an agent prepared, from the person's own Outlook, through Microsoft Graph.
//!
//! The only thing done with the mailbox is to send what the person approved: the connection asks
//! for the send permission and nothing else, so it cannot read, list or delete anything in it.

use serde_json::{Value, json};

use crate::actions::Payload;

/// What Graph is asked to send: exactly the approved message, as plain text, saved in the person's
/// Sent Items like any other email they send.
pub fn graph_message(email: &Payload) -> Option<Value> {
    let Payload::Email { to, cc, subject, body } = email else { return None };
    let people = |list: &[String]| -> Vec<Value> { list.iter().map(|address| json!({ "emailAddress": { "address": address } })).collect() };
    Some(json!({
        "message": {
            "subject": subject,
            "body": { "contentType": "Text", "content": body },
            "toRecipients": people(to),
            "ccRecipients": people(cc),
        },
        "saveToSentItems": true,
    }))
}

#[derive(Debug, PartialEq, Eq)]
pub enum SendError {
    /// The sign-in is no longer good: the person has to connect again.
    Reconnect,
    /// Outlook refused the message itself.
    Refused,
    /// Outlook could not be reached, or answered unexpectedly.
    Unavailable,
}

/// Sends it. Graph answers 202 when the message has been accepted for sending.
pub async fn send(http: &reqwest::Client, graph_base: &str, access_token: &str, email: &Payload) -> Result<(), SendError> {
    let body = graph_message(email).ok_or(SendError::Refused)?;
    let response = http
        .post(format!("{}/v1.0/me/sendMail", graph_base.trim_end_matches('/')))
        .bearer_auth(access_token)
        .json(&body)
        .send()
        .await
        .map_err(|_| SendError::Unavailable)?;
    match response.status().as_u16() {
        200..=299 => Ok(()),
        401 | 403 => Err(SendError::Reconnect),
        400 | 413 | 422 => Err(SendError::Refused),
        _ => Err(SendError::Unavailable),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_sent_is_exactly_the_approved_one() {
        let email = Payload::Email { to: vec!["a@x.com".into()], cc: vec!["b@x.com".into()], subject: "Hi".into(), body: "Hello\nthere".into() };
        let sent = graph_message(&email).unwrap();
        assert_eq!(sent["message"]["subject"], "Hi");
        assert_eq!(sent["message"]["body"], json!({ "contentType": "Text", "content": "Hello\nthere" }));
        assert_eq!(sent["message"]["toRecipients"], json!([{ "emailAddress": { "address": "a@x.com" } }]));
        assert_eq!(sent["message"]["ccRecipients"], json!([{ "emailAddress": { "address": "b@x.com" } }]));
        assert_eq!(sent["saveToSentItems"], true);
        // Nothing but an email is ever sent.
        let meeting = Payload::Meeting { title: "t".into(), start: "2026-11-05T14:00".into(), end: "2026-11-05T15:00".into(), attendees: vec![], location: "".into(), notes: "".into() };
        assert!(graph_message(&meeting).is_none());
    }
}
