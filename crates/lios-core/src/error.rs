use std::path::PathBuf;

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
    #[error("invalid key file")]
    InvalidKeyFile,
    #[error("path has no file name: {0}")]
    MissingFileName(PathBuf),
    #[error("path is outside of expected root: {0}")]
    InvalidRelativePath(PathBuf),
    #[error("storage error: {0}")]
    Storage(String),
    #[error("unsupported operation: {0}")]
    Unsupported(String),
}

pub type Result<T> = std::result::Result<T, LiosError>;
