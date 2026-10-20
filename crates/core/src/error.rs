//! Error type shared by every StoryForge layer.

use std::path::PathBuf;

pub type Result<T> = std::result::Result<T, SfError>;

#[derive(Debug, thiserror::Error)]
pub enum SfError {
    #[error("database error: {0}")]
    Sql(#[from] rusqlite::Error),

    #[error("io error at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("io error: {0}")]
    IoPlain(#[from] std::io::Error),

    #[error("json error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("http error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid input: {0}")]
    Invalid(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("configuration error: {0}")]
    Config(String),

    #[error("{0}")]
    Other(String),
}

impl SfError {
    pub fn io(path: impl Into<PathBuf>, source: std::io::Error) -> Self {
        SfError::Io {
            path: path.into(),
            source,
        }
    }

    /// HTTP status the JSON API should answer with for this error.
    pub fn status(&self) -> u16 {
        match self {
            SfError::NotFound(_) => 404,
            SfError::Invalid(_) => 400,
            SfError::Conflict(_) => 409,
            SfError::Config(_) => 500,
            _ => 500,
        }
    }
}
