use std::fmt;

use lios_application::CommandError;

#[derive(Debug)]
pub struct CliError {
    message: String,
}

impl CliError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    pub fn exit_code(&self) -> u8 {
        1
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
        Self {
            message: error.message,
        }
    }
}

impl From<std::io::Error> for CliError {
    fn from(error: std::io::Error) -> Self {
        Self::new(error.to_string())
    }
}

pub type CliResult<T> = Result<T, CliError>;
