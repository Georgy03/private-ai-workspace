//! Cloud side of the multi-firm workspace. This crate holds the rules that keep one firm's data
//! away from another's: who a request is for (`auth`) and how the database enforces it (`db`).

pub mod actions;
pub mod agent_checks;
pub mod agent_runner;
pub mod agents;
pub mod agents_api;
pub mod api;
pub mod auth;
pub mod browse;
pub mod calc;
pub mod connectors;
pub mod db;
pub mod drive_save;
pub mod embed;
pub mod extract;
pub mod extract_process;
pub mod jwks;
pub mod kms;
pub mod knowledge;
pub mod mail;
pub mod knowledge_api;
pub mod knowledge_store;
pub mod knowledge_worker;
pub mod oauth;
pub mod ocr;
pub mod queue;
pub mod retention;
pub mod schedules;
pub mod schedules_api;
pub mod secrets;
pub mod server;
pub mod signin;
pub mod signin_api;
pub mod sync;
pub mod usage;
pub mod worker;

use std::fmt;

#[derive(Debug)]
pub enum Error {
    Db(tokio_postgres::Error),
    Token(String),
    /// A principal from one firm was used with a transaction opened for another.
    TenantMismatch,
    /// Missing or invalid start-up settings.
    Config(String),
    /// No database connection could be had from the pool.
    Pool(String),
    Connector(connectors::ConnectorError),
    Embedding(String),
    Analysis(String),
    /// Encrypting or opening a stored secret failed.
    Crypto(String),
    /// Setting up or taking away a firm's Microsoft sign-in failed.
    SignIn(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // The driver's own text is just "db error"; the server's message says what went wrong.
            Error::Db(error) => match error.as_db_error() {
                Some(server) => write!(f, "database error: {}", server.message()),
                None => write!(f, "database error: {error}"),
            },
            Error::Token(reason) => write!(f, "invalid token: {reason}"),
            Error::TenantMismatch => write!(f, "principal does not belong to this tenant"),
            Error::Config(reason) => write!(f, "configuration error: {reason}"),
            Error::Pool(reason) => write!(f, "database pool: {reason}"),
            Error::Connector(error) => write!(f, "connector: {error}"),
            Error::Embedding(reason) => write!(f, "embedding: {reason}"),
            Error::Analysis(reason) => write!(f, "analysis: {reason}"),
            Error::Crypto(reason) => write!(f, "secret store: {reason}"),
            Error::SignIn(reason) => write!(f, "sign-in setup: {reason}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<tokio_postgres::Error> for Error {
    fn from(error: tokio_postgres::Error) -> Self {
        Error::Db(error)
    }
}

impl From<connectors::ConnectorError> for Error {
    fn from(error: connectors::ConnectorError) -> Self {
        Error::Connector(error)
    }
}

pub type Result<T> = std::result::Result<T, Error>;
