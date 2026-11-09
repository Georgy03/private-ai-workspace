//! A person with no firm starts their own: `POST /v1/onboarding/firm`. They have signed in (Google) with an
//! email Google verified, but the invitation check had nothing to match, so they hold a token with no firm in it.
//! The only thing that token is good for is this request. It makes the firm and its own encryption key, then
//! attaches the person to it as its administrator in the user pool. After that they sign in again (the app
//! refreshes) and have a firm like anyone else.
//!
//! Anyone on the internet can reach this, so: the email must be verified, a person gets one firm, the number
//! of requests is limited in total, and a person cannot make two at once.

use std::{
    collections::{HashSet, VecDeque},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use deadpool_postgres::Pool;
use serde::{Deserialize, Serialize};
use tower::limit::ConcurrencyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::{
    Result,
    auth::{TenantId, TokenVerifier, Unassigned},
    kms::{DataKeys, provision_firm},
};

/// Firms that may be started in a minute, across everyone.
pub const PER_MINUTE: usize = 20;
const MAX_NAME_CHARS: usize = 120;

/// The user pool, as far as starting a firm needs it.
#[async_trait]
pub trait Accounts: Send + Sync {
    /// Whether the account already belongs to a firm (read fresh, not from the token).
    async fn has_firm(&self, username: &str) -> Result<bool>;
    /// Attaches the account to the firm and makes it the firm's administrator.
    async fn make_admin_of(&self, username: &str, firm: TenantId) -> Result<()>;
}

struct State_ {
    verifier: Arc<TokenVerifier>,
    accounts: Arc<dyn Accounts>,
    keys: Arc<dyn DataKeys>,
    /// Logins of the provisioner role: the only one allowed to create a firm.
    pool: Pool,
    recent: Mutex<VecDeque<Instant>>,
    per_minute: usize,
    /// People whose request is being handled, so one cannot start two firms at once.
    busy: Mutex<HashSet<String>>,
}

pub fn routes(verifier: Arc<TokenVerifier>, accounts: Arc<dyn Accounts>, keys: Arc<dyn DataKeys>, pool: Pool) -> Router {
    routes_with_limit(verifier, accounts, keys, pool, PER_MINUTE)
}

pub fn routes_with_limit(verifier: Arc<TokenVerifier>, accounts: Arc<dyn Accounts>, keys: Arc<dyn DataKeys>, pool: Pool, per_minute: usize) -> Router {
    let state = Arc::new(State_ { verifier, accounts, keys, pool, recent: Mutex::new(VecDeque::new()), per_minute, busy: Mutex::new(HashSet::new()) });
    Router::new()
        .route("/v1/onboarding/firm", post(create_firm).layer(DefaultBodyLimit::max(2048)))
        .layer(ConcurrencyLimitLayer::new(8))
        .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .with_state(state)
}

#[derive(Deserialize)]
struct Ask {
    name: String,
}

#[derive(Serialize)]
struct Made {
    firm_id: String,
}

fn say(status: StatusCode, text: &str) -> Response {
    (status, text.to_string()).into_response()
}

impl State_ {
    fn allow(&self) -> bool {
        let now = Instant::now();
        let mut recent = self.recent.lock().unwrap();
        while recent.front().is_some_and(|at| now.duration_since(*at) > Duration::from_secs(60)) {
            recent.pop_front();
        }
        if recent.len() >= self.per_minute {
            return false;
        }
        recent.push_back(now);
        true
    }
}

/// Removes the person from the busy list when the request ends, however it ends.
struct Busy<'a>(&'a State_, String);
impl Drop for Busy<'_> {
    fn drop(&mut self) {
        self.0.busy.lock().unwrap().remove(&self.1);
    }
}

/// A firm name: words separated by single spaces, no control characters, at most 120 characters.
fn clean_name(text: &str) -> Option<String> {
    let name = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!name.is_empty() && name.chars().count() <= MAX_NAME_CHARS && !name.chars().any(char::is_control)).then_some(name)
}

async fn create_firm(State(state): State<Arc<State_>>, headers: HeaderMap, Json(ask): Json<Ask>) -> Response {
    if !state.allow() {
        return say(StatusCode::TOO_MANY_REQUESTS, "too many new firms just now, try again in a minute");
    }
    let Some(token) = headers.get(header::AUTHORIZATION).and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")) else {
        return say(StatusCode::UNAUTHORIZED, "sign in first");
    };
    let person = match state.verifier.verify_unassigned(token) {
        Ok(Unassigned::New(person)) => person,
        Ok(Unassigned::AlreadyInFirm) => return say(StatusCode::CONFLICT, "this account already belongs to a firm: sign in instead"),
        Err(_) => return say(StatusCode::UNAUTHORIZED, "sign in again"),
    };
    let Some(name) = clean_name(&ask.name) else {
        return say(StatusCode::BAD_REQUEST, "give the firm a name of 1 to 120 characters");
    };
    if !state.busy.lock().unwrap().insert(person.username.clone()) {
        return say(StatusCode::CONFLICT, "your firm is already being made");
    }
    let _busy = Busy(&state, person.username.clone());

    match state.accounts.has_firm(&person.username).await {
        Ok(false) => {}
        Ok(true) => return say(StatusCode::CONFLICT, "this account already belongs to a firm: sign in instead"),
        Err(error) => {
            tracing::error!(%error, "onboarding: could not read the account");
            return say(StatusCode::SERVICE_UNAVAILABLE, "your firm could not be made, try again");
        }
    }
    let conn = match state.pool.get().await {
        Ok(conn) => conn,
        Err(error) => {
            tracing::error!(%error, "onboarding: no database connection");
            return say(StatusCode::SERVICE_UNAVAILABLE, "your firm could not be made, try again");
        }
    };
    let (firm, key) = match provision_firm(&conn, &*state.keys, &name).await {
        Ok(made) => made,
        Err(error) => {
            tracing::error!(%error, "onboarding: could not make the firm");
            return say(StatusCode::SERVICE_UNAVAILABLE, "your firm could not be made, try again");
        }
    };
    if let Err(error) = state.accounts.make_admin_of(&person.username, firm).await {
        // The person is not attached, so the firm would be one nobody can reach. Take it back.
        tracing::error!(%error, "onboarding: could not attach the person to the new firm");
        let _ = conn.execute("DELETE FROM tenants WHERE id = $1", &[&firm.0]).await;
        let _ = state.keys.discard_key(&key).await;
        return say(StatusCode::SERVICE_UNAVAILABLE, "your firm could not be made, try again");
    }
    tracing::info!(firm = %firm.0, "onboarding: a firm was started");
    Json(Made { firm_id: firm.0.to_string() }).into_response()
}

#[cfg(test)]
mod tests {
    use super::clean_name;

    #[test]
    fn firm_names_are_tidied_and_oddities_refused() {
        assert_eq!(clean_name("  Acme   Law \n LLP "), Some("Acme Law LLP".into()));
        for bad in ["", "   ", &"a".repeat(121), "bad\u{7}name"] {
            assert_eq!(clean_name(bad), None, "{bad:?}");
        }
        assert!(clean_name(&"a".repeat(120)).is_some());
    }
}
