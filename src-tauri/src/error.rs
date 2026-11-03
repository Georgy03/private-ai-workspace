use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("database error: {0}")]
    Database(String),
    #[error("filesystem error: {0}")]
    Filesystem(String),
    #[error("parser error: {0}")]
    Parser(String),
    #[error("indexing cancelled")]
    Cancelled,
    #[error("provider error: {0}")]
    Verification(String),
    /// A problem reaching or using the firm's cloud service. The message is already a sentence.
    #[error("{0}")]
    Cloud(String),
    #[error("invalid request: {0}")]
    InvalidRequest(String),
    #[error("resource not found")]
    NotFound,
    #[error("internal error: {0}")]
    Internal(String),
}

impl Serialize for AppError {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl From<anyhow::Error> for AppError {
    fn from(value: anyhow::Error) -> Self {
        Self::Internal(value.to_string())
    }
}

pub type AppResult<T> = Result<T, AppError>;
