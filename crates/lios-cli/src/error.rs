use std::fmt;

use lios_application::{CommandError, CommandErrorCode};
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct CliError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
    #[serde(skip)]
    exit_code: u8,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            code: "internal".to_string(),
            message: message.into(),
            retryable: false,
            details: None,
            exit_code: 7,
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            code: "invalid_input".to_string(),
            message: message.into(),
            retryable: false,
            details: None,
            exit_code: 2,
        }
    }

    pub fn task_failure(message: impl Into<String>) -> Self {
        Self {
            code: "task_failed".to_string(),
            message: message.into(),
            retryable: true,
            details: None,
            exit_code: 7,
        }
    }

    pub fn interrupted(message: impl Into<String>) -> Self {
        Self {
            code: "interrupted".to_string(),
            message: message.into(),
            retryable: true,
            details: None,
            exit_code: 130,
        }
    }

    pub fn exit_code(&self) -> u8 {
        self.exit_code
    }
}

impl fmt::Display for CliError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CliError {}

impl From<lios_core::LiosError> for CliError {
    fn from(error: lios_core::LiosError) -> Self {
        CommandError::from(error).into()
    }
}

impl From<CommandError> for CliError {
    fn from(error: CommandError) -> Self {
        let (code, exit_code) = match error.code {
            CommandErrorCode::InvalidInput => ("invalid_input", 2),
            CommandErrorCode::NotInitialized => ("not_initialized", 3),
            CommandErrorCode::AlreadyInitialized => ("already_initialized", 3),
            CommandErrorCode::Authentication => ("authentication", 3),
            CommandErrorCode::WrongKey => ("wrong_key", 3),
            CommandErrorCode::Network => ("network", 4),
            CommandErrorCode::RateLimited => ("rate_limited", 4),
            CommandErrorCode::RemoteServer => ("remote_server", 4),
            CommandErrorCode::RemoteConflict => ("conflict", 5),
            CommandErrorCode::Busy => ("busy", 5),
            CommandErrorCode::CorruptedData => ("corrupted_data", 6),
            CommandErrorCode::Storage => ("storage", 6),
            CommandErrorCode::Internal => ("internal", 7),
        };
        Self {
            code: code.to_string(),
            message: error.message,
            retryable: error.retryable,
            details: error.details,
            exit_code,
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        CommandError::from(lios_core::LiosError::Io(error)).into()
    }
}

impl From<serde_json::Error> for CliError {
    fn from(error: serde_json::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub type CliResult<T> = Result<T, CliError>;
