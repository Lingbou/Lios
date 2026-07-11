use std::path::Path;

use lios_core::catalog::{Catalog, CATALOG_FILE};
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
use lios_core::credentials::unprotect_from_file;
use lios_core::crypto::KeyFile;
use lios_core::modelscope::ModelScopeAdapter;
use lios_core::storage::StorageAdapter;
use lios_core::{LiosError, RemoteErrorKind};
use serde::Serialize;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::config_mutation_gate::ConfigMutationGate;
use crate::production_config::{persist_config, validate_repo};

type ServiceResult<T> = std::result::Result<T, CommandError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryKeyStatus {
    pub key_location: Option<String>,
    pub backed_up: bool,
    pub backup_location: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RecoveryKeyVerification {
    pub format_valid: bool,
    pub catalog_checked: bool,
    pub checked_space: Option<RepoConfig>,
}

struct VerifiedRecoveryKey {
    key: KeyFile,
    verification: RecoveryKeyVerification,
}

fn sanitized_key_error(error: LiosError, message: &'static str) -> CommandError {
    let mapped = CommandError::from(error);
    CommandError::new(mapped.code, message, mapped.retryable, mapped.details)
}

fn load_key(path: &Path, message: &'static str) -> ServiceResult<KeyFile> {
    KeyFile::load_from_path(path).map_err(|error| sanitized_key_error(error, message))
}

fn map_catalog_verification_error(error: LiosError) -> CommandError {
    match error {
        LiosError::Crypto => CommandError::new(
            CommandErrorCode::WrongKey,
            "recovery key does not match the selected space",
            false,
            None,
        ),
        error => error.into(),
    }
}

pub fn recovery_key_status(config: &LiosConfig) -> RecoveryKeyStatus {
    let key_location = config
        .key_file_path
        .as_ref()
        .map(|path| path.display().to_string());
    let backup_location = config
        .backup_path
        .as_ref()
        .map(|path| path.display().to_string());
    let backed_up = config
        .key_file_path
        .as_deref()
        .zip(config.backup_path.as_deref())
        .and_then(|(active_path, backup_path)| {
            let active = KeyFile::load_from_path(active_path).ok()?;
            let backup = KeyFile::load_from_path(backup_path).ok()?;
            Some(active.same_material(&backup))
        })
        .unwrap_or(false);

    RecoveryKeyStatus {
        key_location,
        backed_up,
        backup_location,
    }
}

pub fn export_recovery_key_for_paths(
    paths: &LiosPaths,
    config_gate: &ConfigMutationGate,
    destination: &Path,
) -> ServiceResult<RecoveryKeyStatus> {
    if destination.as_os_str().is_empty() {
        return Err(CommandError::invalid_input(
            "recovery key backup destination is required",
        ));
    }
    paths.ensure_dirs()?;
    let _config_guard = config_gate.lock()?;
    let mut config = LiosConfig::load(&paths.config)?;
    let active_path = config
        .key_file_path
        .as_deref()
        .ok_or_else(|| CommandError::invalid_input("recovery key is not configured"))?;
    let active_key = load_key(active_path, "active recovery key could not be loaded")?;
    active_key
        .save_to_path(destination)
        .map_err(|error| sanitized_key_error(error, "recovery key backup could not be written"))?;

    config.backup_path = Some(destination.to_path_buf());
    if let Err(error) = persist_config(paths, &mut config) {
        let _ = std::fs::remove_file(destination);
        return Err(error);
    }
    Ok(recovery_key_status(&config))
}

async fn verify_candidate_material_with_adapter<A: StorageAdapter + ?Sized>(
    candidate_path: &Path,
    repo: Option<&RepoConfig>,
    adapter: Option<&A>,
) -> ServiceResult<VerifiedRecoveryKey> {
    let candidate = load_key(candidate_path, "recovery key file is invalid")?;
    let (Some(repo), Some(adapter)) = (repo, adapter) else {
        return Ok(VerifiedRecoveryKey {
            key: candidate,
            verification: RecoveryKeyVerification {
                format_valid: true,
                catalog_checked: false,
                checked_space: None,
            },
        });
    };

    let staging = tempfile::tempdir().map_err(|_| {
        CommandError::new(
            CommandErrorCode::Storage,
            "temporary recovery key verification storage could not be created",
            false,
            None,
        )
    })?;
    let catalog_path = staging.path().join(CATALOG_FILE);
    match adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
        .await
    {
        Ok(()) => {}
        Err(LiosError::Remote(remote)) if remote.kind == RemoteErrorKind::NotFound => {
            return Ok(VerifiedRecoveryKey {
                key: candidate,
                verification: RecoveryKeyVerification {
                    format_valid: true,
                    catalog_checked: false,
                    checked_space: Some(repo.clone()),
                },
            });
        }
        Err(error) => return Err(error.into()),
    }

    Catalog::from_staging(staging.path())
        .decrypt_tree(&candidate)
        .map_err(map_catalog_verification_error)?;
    Ok(VerifiedRecoveryKey {
        key: candidate,
        verification: RecoveryKeyVerification {
            format_valid: true,
            catalog_checked: true,
            checked_space: Some(repo.clone()),
        },
    })
}

pub async fn verify_candidate_with_adapter<A: StorageAdapter + ?Sized>(
    candidate_path: &Path,
    repo: Option<&RepoConfig>,
    adapter: Option<&A>,
) -> ServiceResult<RecoveryKeyVerification> {
    Ok(
        verify_candidate_material_with_adapter(candidate_path, repo, adapter)
            .await?
            .verification,
    )
}

async fn runtime_verification(
    paths: &LiosPaths,
    candidate_path: &Path,
) -> ServiceResult<RecoveryKeyVerification> {
    let config = LiosConfig::load(&paths.config)?;
    let Some(repo) = config.active_repo else {
        return verify_candidate_with_adapter::<ModelScopeAdapter>(candidate_path, None, None)
            .await;
    };
    if !paths.credentials.exists() {
        return verify_candidate_with_adapter::<ModelScopeAdapter>(candidate_path, None, None)
            .await;
    }

    let repo = validate_repo(repo)?;
    let token = unprotect_from_file(&paths.credentials)?;
    let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), token);
    verify_candidate_with_adapter(candidate_path, Some(&repo), Some(&adapter)).await
}

pub async fn verify_recovery_key_for_paths(
    paths: &LiosPaths,
    candidate_path: &Path,
) -> ServiceResult<RecoveryKeyVerification> {
    runtime_verification(paths, candidate_path).await
}

pub async fn import_candidate_with_adapter<A: StorageAdapter + ?Sized>(
    paths: &LiosPaths,
    config_gate: &ConfigMutationGate,
    candidate_path: &Path,
    repo: Option<&RepoConfig>,
    adapter: Option<&A>,
) -> ServiceResult<RecoveryKeyVerification> {
    import_candidate_with_context(
        paths,
        config_gate,
        candidate_path,
        repo,
        repo,
        adapter,
        || {},
    )
    .await
}

#[cfg(test)]
pub async fn import_candidate_with_adapter_after_verification<
    A: StorageAdapter + ?Sized,
    F: FnOnce(),
>(
    paths: &LiosPaths,
    config_gate: &ConfigMutationGate,
    candidate_path: &Path,
    repo: Option<&RepoConfig>,
    adapter: Option<&A>,
    after_verification: F,
) -> ServiceResult<RecoveryKeyVerification> {
    import_candidate_with_context(
        paths,
        config_gate,
        candidate_path,
        repo,
        repo,
        adapter,
        after_verification,
    )
    .await
}

async fn import_candidate_with_context<A: StorageAdapter + ?Sized, F: FnOnce()>(
    paths: &LiosPaths,
    config_gate: &ConfigMutationGate,
    candidate_path: &Path,
    expected_repo: Option<&RepoConfig>,
    verification_repo: Option<&RepoConfig>,
    adapter: Option<&A>,
    after_verification: F,
) -> ServiceResult<RecoveryKeyVerification> {
    let verified =
        verify_candidate_material_with_adapter(candidate_path, verification_repo, adapter).await?;
    after_verification();
    let _config_guard = config_gate.lock()?;
    let mut config = LiosConfig::load(&paths.config)?;
    if config.active_repo.as_ref() != expected_repo {
        return Err(CommandError::new(
            CommandErrorCode::RemoteConflict,
            "active space changed during recovery key verification",
            false,
            None,
        ));
    }
    let current_candidate = load_key(candidate_path, "recovery key file is invalid")?;
    if !verified.key.same_material(&current_candidate) {
        return Err(CommandError::new(
            CommandErrorCode::WrongKey,
            "recovery key file changed during verification",
            false,
            None,
        ));
    }
    config.key_file_path = Some(candidate_path.to_path_buf());
    persist_config(paths, &mut config)?;
    Ok(verified.verification)
}

pub async fn import_recovery_key_for_paths(
    paths: &LiosPaths,
    config_gate: &ConfigMutationGate,
    candidate_path: &Path,
) -> ServiceResult<RecoveryKeyVerification> {
    let config = LiosConfig::load(&paths.config)?;
    let expected_repo = config.active_repo;
    let Some(repo) = expected_repo.as_ref() else {
        return import_candidate_with_context::<ModelScopeAdapter, _>(
            paths,
            config_gate,
            candidate_path,
            None,
            None,
            None,
            || {},
        )
        .await;
    };
    if !paths.credentials.exists() {
        return import_candidate_with_context::<ModelScopeAdapter, _>(
            paths,
            config_gate,
            candidate_path,
            Some(repo),
            None,
            None,
            || {},
        )
        .await;
    }

    let verification_repo = validate_repo(repo.clone())?;
    let token = unprotect_from_file(&paths.credentials)?;
    let adapter = ModelScopeAdapter::new(verification_repo.endpoint.clone(), token);
    import_candidate_with_context(
        paths,
        config_gate,
        candidate_path,
        Some(repo),
        Some(&verification_repo),
        Some(&adapter),
        || {},
    )
    .await
}
