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

fn safe_unsupported_message(message: String) -> String {
    const LOCAL_PATH_PREFIXES: &[(&str, &str)] = &[
        (
            "source paths no longer exist:",
            "selected sources no longer exist",
        ),
        (
            "source path no longer exists:",
            "selected source no longer exists",
        ),
        (
            "source file no longer exists:",
            "selected source file no longer exists",
        ),
        (
            "source file changed while it was being packed:",
            "selected source file changed while it was being prepared",
        ),
        (
            "source path changed before packing:",
            "selected source changed before it was prepared",
        ),
        (
            "source path is not a file or directory:",
            "selected source is not a file or directory",
        ),
        (
            "upload source contains unsupported symbolic links or junctions:",
            "upload source contains unsupported symbolic links or junctions",
        ),
        (
            "restore path contains symlink or junction:",
            "restore destination contains unsupported symbolic links or junctions",
        ),
        (
            "blob source is not a regular file:",
            "local blob source is not a regular file",
        ),
        ("destination already exists:", "destination already exists"),
        (
            "immutable destination already exists:",
            "destination already exists",
        ),
        (
            "source file modification time predates the Unix epoch:",
            "selected source file has an unsupported modification time",
        ),
        (
            "source file modification time is out of range:",
            "selected source file has an unsupported modification time",
        ),
        ("skipped ", "some selected upload paths are unsupported"),
    ];

    LOCAL_PATH_PREFIXES
        .iter()
        .find_map(|(prefix, safe)| message.starts_with(prefix).then(|| (*safe).to_string()))
        .unwrap_or(message)
}

fn contains_absolute_local_path(message: &str) -> bool {
    let trimmed = message.trim_matches(|character: char| {
        character.is_whitespace()
            || matches!(character, '\'' | '"' | '(' | ')' | '[' | ']' | '{' | '}')
    });
    if (trimmed.starts_with('/') && !trimmed.starts_with("//"))
        || message.contains("IO error for operation on /")
        || message.contains("walkdir error for operation on /")
    {
        return true;
    }

    let bytes = message.as_bytes();
    for index in 0..bytes.len() {
        let previous_is_boundary = index == 0
            || bytes[index - 1].is_ascii_whitespace()
            || matches!(
                bytes[index - 1],
                b'\'' | b'"' | b'(' | b'[' | b'{' | b':' | b';' | b'='
            );
        if !previous_is_boundary {
            continue;
        }
        if index + 2 < bytes.len()
            && bytes[index].is_ascii_alphabetic()
            && bytes[index + 1] == b':'
            && matches!(bytes[index + 2], b'/' | b'\\')
        {
            return true;
        }
        if index + 1 < bytes.len() && bytes[index] == b'\\' && bytes[index + 1] == b'\\' {
            return true;
        }
    }
    false
}

pub(crate) fn sanitize_persisted_message(message: String) -> String {
    let sanitized = safe_unsupported_message(message);
    const SAFE_REMOTE_PREFIXES: &[&str] = &[
        "invalid remote object path in catalog:",
        "invalid recovery node descriptor path:",
        "legacy path is outside the managed objects prefix:",
        "catalog object is unavailable locally and remotely:",
        "blob oid must be exactly 64 lowercase hexadecimal characters:",
        "the desired upload list contains a delete action for ",
        "duplicate upload action for ",
        "duplicate remote action for ",
        "the same path is uploaded and deleted in one plan: ",
        "upload path is outside Lios-managed storage:",
        "delete path is outside Lios-managed storage:",
    ];
    if SAFE_REMOTE_PREFIXES
        .iter()
        .any(|prefix| sanitized.starts_with(prefix))
    {
        return sanitized;
    }
    if contains_absolute_local_path(&sanitized) {
        "local file operation failed".to_string()
    } else {
        sanitized
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
            LiosError::InvalidV1Format(_) | LiosError::UnexpectedV1Kind { .. } => Self::new(
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
            LiosError::Json(_) | LiosError::Yaml(_) => Self::new(
                CommandErrorCode::CorruptedData,
                "stored data could not be decoded",
                false,
                None,
            ),
            LiosError::MissingFileName(_) => Self::invalid_input("selected path has no file name"),
            LiosError::InvalidRelativePath(_) => {
                Self::invalid_input("path is outside the allowed location")
            }
            LiosError::InvalidTaskScopeId => {
                Self::invalid_input("invalid internal task scope identifier")
            }
            LiosError::Unsupported(message) => {
                Self::invalid_input(sanitize_persisted_message(message))
            }
            LiosError::Storage(_) => Self::new(
                CommandErrorCode::Storage,
                "local storage operation failed",
                false,
                None,
            ),
            LiosError::StorageTransaction(error) => {
                Self::new(CommandErrorCode::Storage, error.to_string(), false, None)
            }
            LiosError::Io(error) => error.into(),
            LiosError::Database(_) => Self::new(
                CommandErrorCode::Storage,
                "local task database operation failed",
                false,
                None,
            ),
            LiosError::WalkDir(error) => error.into(),
            LiosError::Uuid(_) => Self::new(
                CommandErrorCode::CorruptedData,
                "stored identifier is invalid",
                false,
                None,
            ),
        }
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        let message = if error.kind() == std::io::ErrorKind::AlreadyExists {
            "destination already exists"
        } else {
            "local file operation failed"
        };
        Self::new(CommandErrorCode::Storage, message, false, None)
    }
}

impl From<walkdir::Error> for CommandError {
    fn from(_error: walkdir::Error) -> Self {
        Self::new(
            CommandErrorCode::Storage,
            "local directory scan failed",
            false,
            None,
        )
    }
}

impl From<std::path::StripPrefixError> for CommandError {
    fn from(_error: std::path::StripPrefixError) -> Self {
        Self::invalid_input("path is outside the allowed location")
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use httpmock::Method::POST;
    use httpmock::MockServer;
    use lios_core::modelscope::ModelScopeAdapter;
    use lios_core::storage::StorageTransactionError;
    use lios_core::tasks::{TaskRecord, TaskStore};
    use lios_core::{LiosError, RemoteError, RemoteErrorKind};
    use serde_json::json;
    use tempfile::tempdir;

    use super::{sanitize_persisted_message, CommandError, CommandErrorCode};

    fn missing_walkdir_error(path: &Path) -> walkdir::Error {
        walkdir::WalkDir::new(path)
            .into_iter()
            .next()
            .expect("walkdir should inspect its root")
            .unwrap_err()
    }

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
            CommandError::from(LiosError::InvalidV1Format("truncated envelope")).code,
            CommandErrorCode::CorruptedData
        );
        assert_eq!(
            CommandError::from(LiosError::UnexpectedV1Kind {
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

    #[test]
    fn local_error_serialization_uses_stable_messages_without_absolute_paths() {
        for (sentinel, marker) in [
            (
                r"C:\Users\LIOS_WINDOWS_PRIVATE\Documents\secret.bin",
                "LIOS_WINDOWS_PRIVATE",
            ),
            (
                "/home/LIOS_UNIX_PRIVATE/Documents/secret.bin",
                "LIOS_UNIX_PRIVATE",
            ),
        ] {
            let path = PathBuf::from(sentinel);
            let strip_error = path.strip_prefix(path.join("not-a-prefix")).unwrap_err();
            let cases = vec![
                (
                    "unsupported",
                    CommandError::from(LiosError::Unsupported(format!(
                        "source path no longer exists: {}",
                        path.display()
                    ))),
                    CommandErrorCode::InvalidInput,
                    "selected source no longer exists",
                ),
                (
                    "changed source",
                    CommandError::from(LiosError::Unsupported(format!(
                        "source file changed while it was being packed: {}",
                        path.display()
                    ))),
                    CommandErrorCode::InvalidInput,
                    "selected source file changed while it was being prepared",
                ),
                (
                    "unsupported source link",
                    CommandError::from(LiosError::Unsupported(format!(
                        "upload source contains unsupported symbolic links or junctions: {}",
                        path.display()
                    ))),
                    CommandErrorCode::InvalidInput,
                    "upload source contains unsupported symbolic links or junctions",
                ),
                (
                    "unsupported restore link",
                    CommandError::from(LiosError::Unsupported(format!(
                        "restore path contains symlink or junction: {}",
                        path.display()
                    ))),
                    CommandErrorCode::InvalidInput,
                    "restore destination contains unsupported symbolic links or junctions",
                ),
                (
                    "unsupported source timestamp",
                    CommandError::from(LiosError::Unsupported(format!(
                        "source file modification time is out of range: {}",
                        path.display()
                    ))),
                    CommandErrorCode::InvalidInput,
                    "selected source file has an unsupported modification time",
                ),
                (
                    "skipped source",
                    CommandError::from(LiosError::Unsupported(format!(
                        "skipped 1 path: {}",
                        path.display()
                    ))),
                    CommandErrorCode::InvalidInput,
                    "some selected upload paths are unsupported",
                ),
                (
                    "missing file name",
                    CommandError::from(LiosError::MissingFileName(path.clone())),
                    CommandErrorCode::InvalidInput,
                    "selected path has no file name",
                ),
                (
                    "invalid relative path",
                    CommandError::from(LiosError::InvalidRelativePath(path.clone())),
                    CommandErrorCode::InvalidInput,
                    "path is outside the allowed location",
                ),
                (
                    "I/O",
                    CommandError::from(LiosError::Io(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!("cannot open {}", path.display()),
                    ))),
                    CommandErrorCode::Storage,
                    "local file operation failed",
                ),
                (
                    "existing destination",
                    CommandError::from(LiosError::Io(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        format!("destination already exists: {}", path.display()),
                    ))),
                    CommandErrorCode::Storage,
                    "destination already exists",
                ),
                (
                    "walkdir",
                    CommandError::from(LiosError::WalkDir(missing_walkdir_error(&path))),
                    CommandErrorCode::Storage,
                    "local directory scan failed",
                ),
                (
                    "strip prefix",
                    CommandError::from(strip_error),
                    CommandErrorCode::InvalidInput,
                    "path is outside the allowed location",
                ),
                (
                    "storage",
                    CommandError::from(LiosError::Storage(format!(
                        "staging operation failed at {}",
                        path.display()
                    ))),
                    CommandErrorCode::Storage,
                    "local storage operation failed",
                ),
            ];

            for (operation, error, expected_code, expected_message) in cases {
                assert_eq!(error.code, expected_code, "{operation}");
                assert!(!error.retryable, "{operation}");
                assert_eq!(error.message, expected_message, "{operation}");
                let serialized = serde_json::to_string(&error).unwrap();
                assert!(!serialized.contains(marker), "{operation}: {serialized}");
            }
        }
    }

    #[test]
    fn safe_validation_context_remains_actionable() {
        let endpoint_message =
            "ModelScope endpoint must be https://modelscope.cn or https://www.modelscope.cn";
        let endpoint = CommandError::from(LiosError::Unsupported(endpoint_message.to_string()));
        assert_eq!(endpoint.code, CommandErrorCode::InvalidInput);
        assert_eq!(endpoint.message, endpoint_message);

        for message in [
            "source file changed before upload: folder/a.bin",
            "persisted source file snapshot is incomplete: a.bin",
        ] {
            let error = CommandError::from(LiosError::Unsupported(message.to_string()));
            assert_eq!(error.message, message);
        }

        let transaction = CommandError::from(LiosError::StorageTransaction(
            StorageTransactionError::UnmanagedUploadPath(
                "objects/files/outside-managed-prefix.enc".to_string(),
            ),
        ));
        assert_eq!(transaction.code, CommandErrorCode::Storage);
        assert_eq!(
            transaction.message,
            "upload path is outside Lios-managed storage: objects/files/outside-managed-prefix.enc"
        );

        for message in [
            "invalid remote object path in catalog: /objects/x",
            "upload path is outside Lios-managed storage: /objects/x",
            "invalid remote object path in catalog: C:/objects/x",
            r"upload path is outside Lios-managed storage: C:\objects\x",
        ] {
            assert_eq!(sanitize_persisted_message(message.to_string()), message);
        }
        for path in ["/data/alice/secret.bin", "/workspace/private/secret.bin"] {
            assert_eq!(
                sanitize_persisted_message(path.to_string()),
                "local file operation failed"
            );
        }
        assert_eq!(
            sanitize_persisted_message(
                "blob source is not a regular file: /data/alice/secret.bin".to_string()
            ),
            "local blob source is not a regular file"
        );
    }

    #[test]
    fn invalid_blob_oid_validation_context_remains_actionable() {
        let message = "blob oid must be exactly 64 lowercase hexadecimal characters: C:/objects/x";
        assert_eq!(sanitize_persisted_message(message.to_string()), message);
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
