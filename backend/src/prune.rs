//! Clearing out abandoned sign-ups. Open sign-up (onboarding.rs) gives anyone who signs in with Google or a personal
//! Microsoft account an account with no firm. Most go on to make a firm; some never do, and those would pile up in the
//! user pool. This lists such accounts that are old enough and, only when told to, deletes them.
//!
//! It touches only accounts that signed in through an outside provider and belong to no firm, so an invited person, a
//! firm administrator, or anyone attached to a firm is never a candidate. Each account is looked at again just before it
//! is deleted, so a person who is making their firm right now is spared.

use async_trait::async_trait;

use crate::Result;

/// An account that signed in through Google or Microsoft and belongs to no firm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Newcomer {
    pub username: String,
    pub email: String,
    /// Seconds since 1970 when the account was made.
    pub created: i64,
}

#[async_trait]
pub trait PoolUsers: Send + Sync {
    /// Every outside-provider account in the pool that has no firm, however new.
    async fn firmless_federated(&self) -> Result<Vec<Newcomer>>;
    /// Whether the account belongs to a firm now (read fresh).
    async fn has_firm(&self, username: &str) -> Result<bool>;
    async fn delete(&self, username: &str) -> Result<()>;
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Old enough to be cleared.
    pub candidates: Vec<Newcomer>,
    /// Firm-less but newer than the cut-off, so left alone.
    pub too_new: usize,
    /// Deleted (always empty unless deleting was asked for).
    pub deleted: Vec<String>,
    /// Looked at again and found to have a firm by then.
    pub spared: Vec<String>,
}

/// The shortest wait allowed: a person in the middle of signing up must never be swept away.
pub const MIN_DAYS: u32 = 1;

pub async fn prune(users: &dyn PoolUsers, older_than_days: u32, now: i64, delete: bool) -> Result<Report> {
    if older_than_days < MIN_DAYS {
        return Err(crate::Error::Config(format!("--days must be at least {MIN_DAYS}")));
    }
    let cutoff = now - i64::from(older_than_days) * 86_400;
    let mut report = Report::default();
    for newcomer in users.firmless_federated().await? {
        if newcomer.created > cutoff {
            report.too_new += 1;
        } else {
            report.candidates.push(newcomer);
        }
    }
    if delete {
        for newcomer in &report.candidates {
            if users.has_firm(&newcomer.username).await? {
                report.spared.push(newcomer.username.clone());
            } else {
                users.delete(&newcomer.username).await?;
                report.deleted.push(newcomer.username.clone());
            }
        }
    }
    Ok(report)
}
