use lios_core::{LiosError, RemoteError, RemoteErrorKind};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum CommandErrorCode {
    NotInitialized,
    AlreadyInitialized,
    Authentication,
    Network,
    WrongKey,
    RemoteConflict,
    RateLimited,
    RemoteServer,
    CorruptedData,
    InvalidInput,
    Storage,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CommandError {
    pub code: CommandErrorCode,
    pub message: String,
    pub retryable: bool,
    pub details: Option<Value>,
}

impl CommandError {
    pub fn new(
        code: CommandErrorCode,
        message: impl Into<String>,
        retryable: bool,
        details: Option<Value>,
    ) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
            details,
        }
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new(CommandErrorCode::InvalidInput, message, false, None)
    }

    pub fn not_initialized(message: impl Into<String>) -> Self {
        Self::new(CommandErrorCode::NotInitialized, message, false, None)
    }

    pub fn already_initialized(message: impl Into<String>) -> Self {
        Self::new(CommandErrorCode::AlreadyInitialized, message, false, None)
    }
}

impl std::fmt::Display for CommandError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for CommandError {}

impl From<RemoteError> for CommandError {
    fn from(error: RemoteError) -> Self {
        let (code, retryable) = match error.kind {
            RemoteErrorKind::Authentication => (CommandErrorCode::Authentication, false),
            RemoteErrorKind::Network => (CommandErrorCode::Network, true),
            RemoteErrorKind::Conflict => (CommandErrorCode::RemoteConflict, false),
            RemoteErrorKind::RateLimited => (CommandErrorCode::RateLimited, true),
            RemoteErrorKind::Server => (CommandErrorCode::RemoteServer, true),
            RemoteErrorKind::InvalidRequest => (CommandErrorCode::InvalidInput, false),
            RemoteErrorKind::InvalidResponse => (CommandErrorCode::Storage, false),
            RemoteErrorKind::NotFound => (CommandErrorCode::Storage, false),
        };
        let details = Some(json!({
            "kind": error.kind,
            "status": error.status,
        }));
        Self::new(code, error.message, retryable, details)
    }
}

impl From<LiosError> for CommandError {
    fn from(error: LiosError) -> Self {
        match error {
            LiosError::Remote(error) => error.into(),
            LiosError::InvalidKeyFile => {
                Self::new(CommandErrorCode::WrongKey, "invalid key file", false, None)
            }
            LiosError::Crypto => Self::new(
                CommandErrorCode::CorruptedData,
                "encrypted data could not be decrypted",
                false,
                None,
            ),
            LiosError::InvalidV2Format(_) | LiosError::UnexpectedV2Kind { .. } => Self::new(
                CommandErrorCode::CorruptedData,
                "encrypted data has an invalid format",
                false,
                None,
            ),
            LiosError::DataCorruption(_) => Self::new(
                CommandErrorCode::CorruptedData,
                "encrypted or compressed data is corrupted",
                false,
                None,
            ),
            LiosError::Json(error) => Self::new(
                CommandErrorCode::CorruptedData,
                error.to_string(),
                false,
                None,
            ),
            LiosError::Yaml(error) => Self::new(
                CommandErrorCode::CorruptedData,
                error.to_string(),
                false,
                None,
            ),
            LiosError::MissingFileName(path) | LiosError::InvalidRelativePath(path) => {
                Self::invalid_input(path.display().to_string())
            }
            LiosError::Unsupported(message) => Self::invalid_input(message),
            LiosError::Storage(message) => {
                Self::new(CommandErrorCode::Storage, message, false, None)
            }
            LiosError::StorageTransaction(error) => {
                Self::new(CommandErrorCode::Storage, error.to_string(), false, None)
            }
            LiosError::Io(error) => error.into(),
            LiosError::Database(error) => {
                Self::new(CommandErrorCode::Storage, error.to_string(), false, None)
            }
            LiosError::WalkDir(error) => {
                Self::new(CommandErrorCode::Storage, error.to_string(), false, None)
            }
            LiosError::Uuid(error) => Self::new(
                CommandErrorCode::CorruptedData,
                error.to_string(),
                false,
                None,
            ),
        }
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::new(CommandErrorCode::Storage, error.to_string(), false, None)
    }
}

impl From<walkdir::Error> for CommandError {
    fn from(error: walkdir::Error) -> Self {
        Self::new(CommandErrorCode::Storage, error.to_string(), false, None)
    }
}

impl From<std::path::StripPrefixError> for CommandError {
    fn from(error: std::path::StripPrefixError) -> Self {
        Self::invalid_input(error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use lios_core::modelscope::ModelScopeAdapter;
    use lios_core::tasks::{TaskRecord, TaskStore};
    use lios_core::{LiosError, RemoteError, RemoteErrorKind};
    use serde_json::json;
    use tempfile::tempdir;

    use super::{CommandError, CommandErrorCode};

    #[test]
    fn serializes_stable_command_error_shape() {
        let error = CommandError::new(
            CommandErrorCode::NotInitialized,
            "space is not initialized",
            false,
            Some(json!({ "path": "catalog.enc" })),
        );

        assert_eq!(
            serde_json::to_value(error).unwrap(),
            json!({
                "code": "NotInitialized",
                "message": "space is not initialized",
                "retryable": false,
                "details": { "path": "catalog.enc" }
            })
        );
    }

    #[test]
    fn maps_typed_remote_errors_consistently() {
        let cases = [
            (
                RemoteErrorKind::Authentication,
                Some(401),
                CommandErrorCode::Authentication,
                false,
            ),
            (
                RemoteErrorKind::Network,
                None,
                CommandErrorCode::Network,
                true,
            ),
            (
                RemoteErrorKind::Conflict,
                Some(409),
                CommandErrorCode::RemoteConflict,
                false,
            ),
            (
                RemoteErrorKind::RateLimited,
                Some(429),
                CommandErrorCode::RateLimited,
                true,
            ),
            (
                RemoteErrorKind::Server,
                Some(503),
                CommandErrorCode::RemoteServer,
                true,
            ),
        ];

        for (kind, status, code, retryable) in cases {
            let error = CommandError::from(LiosError::Remote(RemoteError::new(kind, status)));
            assert_eq!(error.code, code);
            assert_eq!(error.retryable, retryable);
            assert_eq!(error.details.unwrap()["status"], json!(status));
        }
    }

    #[test]
    fn distinguishes_wrong_key_and_corrupted_data() {
        assert_eq!(
            CommandError::from(LiosError::InvalidKeyFile).code,
            CommandErrorCode::WrongKey
        );
        assert_eq!(
            CommandError::from(LiosError::Crypto).code,
            CommandErrorCode::CorruptedData
        );
        assert_eq!(
            CommandError::from(LiosError::InvalidV2Format("truncated envelope")).code,
            CommandErrorCode::CorruptedData
        );
        assert_eq!(
            CommandError::from(LiosError::UnexpectedV2Kind {
                expected: 1,
                actual: 2,
            })
            .code,
            CommandErrorCode::CorruptedData
        );
        assert_eq!(
            CommandError::from(LiosError::DataCorruption(
                "incomplete zstd frame".to_string(),
            ))
            .code,
            CommandErrorCode::CorruptedData
        );
        assert_eq!(
            CommandError::from(LiosError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "output denied",
            )))
            .code,
            CommandErrorCode::Storage
        );
    }

    #[tokio::test]
    async fn remote_secrets_never_reach_command_serialization_or_task_storage() {
        let server = MockServer::start();
        server.mock(|when, then| {
            when.method(POST).path("/api/v1/login");
            then.status(401).body(
                "Authorization: Bearer ms-token-secret; Cookie: m_session_id=ms-token-secret; https://example.test/file?X-Amz-Credential=signed-secret",
            );
        });
        let error = ModelScopeAdapter::new(server.base_url(), "request-token")
            .whoami()
            .await
            .unwrap_err();
        let LiosError::Remote(remote) = error else {
            panic!("expected typed remote error");
        };
        let remote_json = serde_json::to_string(&remote).unwrap();
        let command = CommandError::from(remote);
        let command_json = serde_json::to_string(&command).unwrap();
        let temp = tempdir().unwrap();
        let store = TaskStore::open(temp.path().join("tasks.db")).unwrap();
        let mut task = TaskRecord::queued("secret safety", 1);
        task.error = Some(command.message.clone());
        store.insert(&task).unwrap();
        let persisted = store.list().unwrap()[0].error.clone().unwrap();

        for secret in [
            "ms-token-secret",
            "Authorization",
            "Cookie",
            "X-Amz-Credential",
            "signed-secret",
        ] {
            assert!(!remote_json.contains(secret), "{remote_json}");
            assert!(!command_json.contains(secret), "{command_json}");
            assert!(!persisted.contains(secret), "{persisted}");
        }
        assert_eq!(
            command.details,
            Some(json!({ "kind": "Authentication", "status": 401 }))
        );
    }
}
