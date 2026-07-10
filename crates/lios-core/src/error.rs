use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum RemoteErrorKind {
    Authentication,
    NotFound,
    Conflict,
    RateLimited,
    Server,
    Network,
    InvalidRequest,
    InvalidResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct RemoteError {
    pub kind: RemoteErrorKind,
    pub status: Option<u16>,
    pub message: String,
}

impl RemoteError {
    pub fn new(kind: RemoteErrorKind, status: Option<u16>) -> Self {
        Self {
            kind,
            status,
            message: kind.message().to_string(),
        }
    }

    pub fn from_status(status: u16) -> Self {
        let kind = match status {
            401 | 403 => RemoteErrorKind::Authentication,
            404 => RemoteErrorKind::NotFound,
            409 => RemoteErrorKind::Conflict,
            429 => RemoteErrorKind::RateLimited,
            500..=599 => RemoteErrorKind::Server,
            _ => RemoteErrorKind::InvalidRequest,
        };
        Self::new(kind, Some(status))
    }
}

impl RemoteErrorKind {
    fn message(self) -> &'static str {
        match self {
            Self::Authentication => "ModelScope authentication failed",
            Self::NotFound => "the requested ModelScope resource was not found",
            Self::Conflict => "ModelScope reported a remote conflict",
            Self::RateLimited => "ModelScope rate limit exceeded",
            Self::Server => "ModelScope service is unavailable",
            Self::Network => "network request to ModelScope failed",
            Self::InvalidRequest => "ModelScope rejected the request",
            Self::InvalidResponse => "ModelScope returned an invalid response",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum LiosError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("YAML error: {0}")]
    Yaml(#[from] serde_yaml::Error),
    #[error("database error: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("walkdir error: {0}")]
    WalkDir(#[from] walkdir::Error),
    #[error("UUID error: {0}")]
    Uuid(#[from] uuid::Error),
    #[error("crypto operation failed")]
    Crypto,
    #[error("corrupted encrypted or compressed data: {0}")]
    DataCorruption(String),
    #[error("invalid key file")]
    InvalidKeyFile,
    #[error("invalid v2 format: {0}")]
    InvalidV2Format(&'static str),
    #[error("unexpected v2 content kind: expected {expected}, got {actual}")]
    UnexpectedV2Kind { expected: u8, actual: u8 },
    #[error("path has no file name: {0}")]
    MissingFileName(PathBuf),
    #[error("path is outside of expected root: {0}")]
    InvalidRelativePath(PathBuf),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("remote error: {0}")]
    Remote(#[from] RemoteError),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, LiosError>;
