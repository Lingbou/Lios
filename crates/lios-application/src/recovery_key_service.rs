//! Recovery-key workflows shared by Desktop and CLI.

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
    pub checked_spaces: Vec<RepoConfig>,
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
                checked_spaces: Vec::new(),
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
                    checked_spaces: vec![repo.clone()],
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
            checked_spaces: vec![repo.clone()],
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
    if !config.spaces.is_empty() {
        let candidate = load_key(candidate_path, "recovery key file is invalid")?;
        if !paths.credentials.exists() {
            return Err(CommandError::new(
                CommandErrorCode::Authentication,
                "all registered Spaces must be reachable before verifying a Recovery Key",
                false,
                None,
            ));
        }
        let token = unprotect_from_file(&paths.credentials)?;
        let mut checked_spaces = Vec::with_capacity(config.spaces.len());
        for repo in config.spaces.values() {
            let repo = validate_repo(repo.clone())?;
            let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), token.clone());
            let staging = tempfile::tempdir().map_err(|_| {
                CommandError::new(
                    CommandErrorCode::Storage,
                    "temporary Recovery Key verification storage could not be created",
                    false,
                    None,
                )
            })?;
            let catalog_path = staging.path().join(CATALOG_FILE);
            adapter
                .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
                .await
                .map_err(CommandError::from)?;
            Catalog::from_staging(staging.path())
                .decrypt_tree(&candidate)
                .map_err(map_catalog_verification_error)?;
            checked_spaces.push(repo);
        }
        return Ok(RecoveryKeyVerification {
            format_valid: true,
            catalog_checked: true,
            checked_space: checked_spaces.first().cloned(),
            checked_spaces,
        });
    }
    verify_candidate_with_adapter::<ModelScopeAdapter>(candidate_path, None, None).await
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
    if expected_repo.is_some_and(|repo| !config.spaces.values().any(|saved| saved == repo)) {
        return Err(CommandError::new(
            CommandErrorCode::RemoteConflict,
            "Space registry changed during Recovery Key verification",
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
    if !config.spaces.is_empty() {
        let expected_spaces = config.spaces.clone();
        let verification = runtime_verification(paths, candidate_path).await?;
        let verified_key = load_key(candidate_path, "recovery key file is invalid")?;
        let _config_guard = config_gate.lock()?;
        let _process_guard = paths.try_lock_config().map_err(CommandError::from)?;
        let mut current = LiosConfig::load(&paths.config)?;
        if current.spaces != expected_spaces {
            return Err(CommandError::new(
                CommandErrorCode::RemoteConflict,
                "Space registry changed during Recovery Key verification",
                true,
                None,
            ));
        }
        let current_candidate = load_key(candidate_path, "recovery key file is invalid")?;
        if !verified_key.same_material(&current_candidate) {
            return Err(CommandError::new(
                CommandErrorCode::WrongKey,
                "Recovery Key file changed during verification",
                false,
                None,
            ));
        }
        current.key_file_path = Some(candidate_path.to_path_buf());
        persist_config(paths, &mut current)?;
        return Ok(verification);
    }
    import_candidate_with_context::<ModelScopeAdapter, _>(
        paths,
        config_gate,
        candidate_path,
        None,
        None,
        None,
        || {},
    )
    .await
}
