//! The one request the desktop app makes before anyone is signed in: "which Microsoft sign-in is mine?".
//! It answers with the name of the provider of the firm the email was invited to, or null. It never tells an
//! uninvited email from an invited one whose firm has no Microsoft sign-in, and it is limited in total, because
//! anyone on the internet can call it.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::{HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde::{Deserialize, Serialize};
use tower::limit::ConcurrencyLimitLayer;
use tower_http::set_header::SetResponseHeaderLayer;

use crate::signin::SignInRouting;

/// Requests allowed in a minute, across everyone.
pub const PER_MINUTE: usize = 60;
const MAX_EMAIL: usize = 254;

struct RouteState {
    lookup: Arc<dyn SignInRouting>,
    recent: Mutex<VecDeque<Instant>>,
    per_minute: usize,
}

impl RouteState {
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

#[derive(Deserialize)]
struct Ask {
    email: String,
}

#[derive(Serialize)]
struct Answer {
    provider: Option<String>,
}

pub fn routes(lookup: Arc<dyn SignInRouting>) -> Router {
    routes_with_limit(lookup, PER_MINUTE)
}

pub fn routes_with_limit(lookup: Arc<dyn SignInRouting>, per_minute: usize) -> Router {
    let state = Arc::new(RouteState { lookup, recent: Mutex::new(VecDeque::new()), per_minute });
    Router::new()
        .route("/v1/sign-in/route", post(route).layer(DefaultBodyLimit::max(1024)))
        .layer(ConcurrencyLimitLayer::new(16))
        .layer(SetResponseHeaderLayer::overriding(header::CACHE_CONTROL, HeaderValue::from_static("no-store")))
        .with_state(state)
}

/// Looks like name@host.tld with nothing else, so odd input never reaches AWS.
fn clean_email(text: &str) -> Option<String> {
    let email = text.trim().to_lowercase();
    let (name, host) = email.split_once('@')?;
    let fine = email.len() <= MAX_EMAIL
        && !name.is_empty()
        && host.contains('.')
        && !host.contains('@')
        && email.chars().all(|c| c.is_ascii_graphic());
    fine.then_some(email)
}

async fn route(State(state): State<Arc<RouteState>>, Json(ask): Json<Ask>) -> Response {
    if !state.allow() {
        return (StatusCode::TOO_MANY_REQUESTS, "too many requests, try again in a minute").into_response();
    }
    // A malformed address is answered like an unknown one.
    let Some(email) = clean_email(&ask.email) else {
        return Json(Answer { provider: None }).into_response();
    };
    match state.lookup.provider_for_email(&email).await {
        Ok(provider) => Json(Answer { provider }).into_response(),
        Err(error) => {
            tracing::error!(%error, "sign-in route lookup failed");
            (StatusCode::SERVICE_UNAVAILABLE, "sign-in could not be looked up, try again").into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::clean_email;

    #[test]
    fn only_plain_addresses_are_looked_up() {
        assert_eq!(clean_email("  Sam@Acme.COM "), Some("sam@acme.com".into()));
        for bad in ["", "sam", "@acme.com", "sam@acme", "a@b@c.com", "sa m@acme.com", &format!("{}@acme.com", "a".repeat(250))] {
            assert_eq!(clean_email(bad), None, "{bad:?}");
        }
    }
}
