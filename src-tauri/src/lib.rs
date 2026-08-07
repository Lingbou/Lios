pub use lios_application::{
    app_log, catalog_mutation_gate, catalog_probe, catalog_sync, command_error,
    config_mutation_gate, download_service, production_config, recovery_key_service, task_manager,
};

pub mod command_surface;
mod task_center;

#[cfg(test)]
#[path = "../build_support.rs"]
mod build_support;

#[cfg(test)]
use std::collections::HashMap;
use std::collections::HashSet;
use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};

use app_log::AppLogger;
use catalog_mutation_gate::CatalogMutationGate;
use catalog_probe::ensure_space_can_initialize;
#[cfg(test)]
use catalog_sync::{
    catalog_baseline_from_downloaded_catalog, persist_sync_checkpoints, plan_catalog_sync,
    plan_catalog_sync_with_baseline, SyncWork,
};
use catalog_sync::{sync_current_catalog, CatalogBaseline};
use command_error::{CommandError, CommandErrorCode};
use command_surface::with_registered_commands;
use config_mutation_gate::ConfigMutationGate;
use download_service::prepare_download_task;
use lios_application::service::Application;
use lios_application::space_registry::SpaceRegistry;
use lios_core::cache::{cleanup_temporary_staging, prune_unreferenced_staging, CacheCleanupReport};
use lios_core::catalog::{
    Catalog, CatalogRebuildOutcome, CatalogRebuildReport, CatalogRemoteFile, CatalogSelection,
    CatalogTreeNode, ConflictAction, ConflictResolution, DriveItem, SourceFileSnapshot,
    UploadConflict, CATALOG_FILE,
};
#[cfg(test)]
use lios_core::catalog_transaction::{
    CatalogBlobCheckpointState, CatalogTransactionPhase, CatalogTransactionProgress,
};
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
use lios_core::credentials::{protect_to_file, unprotect_from_file};
use lios_core::crypto::KeyFile;
use lios_core::modelscope::{DatasetRepoSummary, ModelScopeAdapter, ModelScopeUserSummary};
use lios_core::pack::PackOptions;
use lios_core::space_lock::SpaceLockError;
#[cfg(test)]
use lios_core::storage::CatalogSyncFile;
use lios_core::storage::{RepoRevision, StorageAdapter, StorageObject};
#[cfg(test)]
use lios_core::tasks::{CheckpointState, TaskItemState, TaskObjectCheckpoint};
use lios_core::tasks::{TaskRecord, TaskSpec, TaskState, TaskStore, TaskSummary};
use production_config::{configured_endpoint, prepare_startup_config, validate_repo, SetupWarning};
use recovery_key_service::{
    export_recovery_key_for_paths, import_recovery_key_for_paths, recovery_key_status,
    verify_recovery_key_for_paths, RecoveryKeyStatus, RecoveryKeyVerification,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
#[cfg(test)]
use task_center::TaskUpdateEvent;
use task_center::{
    emit_removed_tasks, emit_task, list_task_items_for_paths, task_summaries_for_paths,
    task_summary_for_paths, webview_safe_task_summary, TaskItemsPageDto,
};
use task_manager::{persist_submission, snapshot_upload_sources, TaskManager, TaskScope};
#[cfg(test)]
use task_manager::{retry_backoff, TransferMetrics};
use tauri::Manager;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

type CommandResult<T> = std::result::Result<T, CommandError>;

struct AppContext {
    paths: LiosPaths,
    app_log: AppLogger,
    catalog_mutation_gate: CatalogMutationGate,
    config_mutation_gate: ConfigMutationGate,
    task_lifecycle_gate: Mutex<TaskLifecycleState>,
    task_manager: TaskManager,
}

#[derive(Default)]
struct TaskLifecycleState {
    active_workers: HashSet<Uuid>,
}

impl AppContext {
    fn new() -> Self {
        Self::from_paths(LiosPaths::default_user())
    }

    fn from_paths(paths: LiosPaths) -> Self {
        let app_log = AppLogger::new(&paths);
        app_log.log(
            "info",
            "app_started",
            serde_json::json!({ "version": env!("CARGO_PKG_VERSION") }),
        );
        Self {
            paths,
            app_log,
            catalog_mutation_gate: CatalogMutationGate::default(),
            config_mutation_gate: ConfigMutationGate::default(),
            task_lifecycle_gate: Mutex::new(TaskLifecycleState::default()),
            task_manager: TaskManager::default(),
        }
    }
}

#[derive(Serialize)]
struct PathsDto {
    home: String,
    config: String,
    database: String,
    staging: String,
    logs: String,
    credentials: String,
}

#[derive(Serialize)]
struct SetupSnapshot {
    paths: PathsDto,
    config: LiosConfig,
    recovery_key: RecoveryKeyStatus,
    has_token: bool,
    spaces: Vec<RegisteredSpaceDto>,
    warning: Option<SetupWarning>,
}

#[derive(Serialize)]
struct RegisteredSpaceDto {
    space_name: String,
    namespace: String,
    dataset: String,
    endpoint: String,
    task_space_id: String,
}

impl RegisteredSpaceDto {
    fn new(space_name: String, repo: RepoConfig) -> Self {
        let task_space_id = TaskScope::from_repo(&repo).space_id;
        Self {
            space_name,
            namespace: repo.namespace,
            dataset: repo.dataset,
            endpoint: repo.endpoint,
            task_space_id,
        }
    }
}

#[derive(Serialize)]
struct CatalogLoadResult {
    local_path: String,
    bytes: u64,
    tree: CatalogTreeNode,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct CatalogRebuildPreviewResult {
    revision: String,
    tree: CatalogTreeNode,
    report: CatalogRebuildReport,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct DatasetRepoListResult {
    user: ModelScopeUserSummary,
    repositories: Vec<DatasetRepoSummaryDto>,
}

#[derive(Serialize)]
struct DatasetRepoSummaryDto {
    namespace: String,
    dataset: String,
    endpoint: String,
    visibility: Option<String>,
    updated_at: Option<String>,
    description: Option<String>,
    task_space_id: String,
}

impl From<DatasetRepoSummary> for DatasetRepoSummaryDto {
    fn from(repo: DatasetRepoSummary) -> Self {
        let task_space_id = TaskScope::from_repo(&RepoConfig {
            namespace: repo.namespace.clone(),
            dataset: repo.dataset.clone(),
            endpoint: repo.endpoint.clone(),
        })
        .space_id;
        Self {
            namespace: repo.namespace,
            dataset: repo.dataset,
            endpoint: repo.endpoint,
            visibility: repo.visibility,
            updated_at: repo.updated_at,
            description: repo.description,
            task_space_id,
        }
    }
}

fn to_err<E>(error: E) -> CommandError
where
    CommandError: From<E>,
{
    error.into()
}

fn paths_dto(paths: &LiosPaths) -> PathsDto {
    PathsDto {
        home: paths.home.display().to_string(),
        config: paths.config.display().to_string(),
        database: paths.database.display().to_string(),
        staging: paths.staging.display().to_string(),
        logs: paths.logs.display().to_string(),
        credentials: paths.credentials.display().to_string(),
    }
}

fn recovery_log_details(catalog_checked: bool, repo: Option<&RepoConfig>) -> serde_json::Value {
    let scope = repo.map(TaskScope::from_repo);
    serde_json::json!({
        "catalog_checked": catalog_checked,
        "account_id": scope.as_ref().map(|scope| scope.account_id.as_str()),
        "space_id": scope.as_ref().map(|scope| scope.space_id.as_str()),
    })
}

fn load_config(paths: &LiosPaths) -> CommandResult<LiosConfig> {
    LiosConfig::load(&paths.config).map_err(to_err)
}

fn task_store(paths: &LiosPaths) -> CommandResult<TaskStore> {
    TaskStore::open(&paths.database).map_err(to_err)
}

#[cfg(test)]
fn task_interrupt_core(paths: &LiosPaths, id: Uuid) -> lios_core::Result<Option<TaskState>> {
    let state = TaskStore::open(&paths.database)?
        .get_summary(id)?
        .map(|task| task.state);
    match state {
        Some(TaskState::Paused) => Ok(Some(TaskState::Paused)),
        Some(TaskState::Canceled) => Ok(Some(TaskState::Canceled)),
        _ => Ok(None),
    }
}

#[cfg(test)]
fn insert_task(paths: &LiosPaths, task: &TaskRecord) -> CommandResult<()> {
    task_store(paths)?.insert(task).map_err(to_err)
}

#[cfg(test)]
fn update_task_phase(paths: &LiosPaths, id: Uuid, phase: Option<String>) -> CommandResult<()> {
    task_store(paths)?.update_phase(id, phase).map_err(to_err)
}

#[cfg(test)]
fn transaction_phase_label(phase: CatalogTransactionPhase) -> &'static str {
    match phase {
        CatalogTransactionPhase::ValidateBlobs => "validating",
        CatalogTransactionPhase::UploadBlobs => "uploading",
        CatalogTransactionPhase::Prepublish => "uploading",
        CatalogTransactionPhase::ProbeCatalog => "checking",
        CatalogTransactionPhase::Publish => "committing",
        CatalogTransactionPhase::Cleanup => "cleaning",
    }
}

#[cfg(test)]
fn persist_transaction_progress(
    paths: &LiosPaths,
    app: Option<&tauri::AppHandle>,
    task: &mut TaskRecord,
    metrics: &mut TransferMetrics,
    progress: CatalogTransactionProgress,
) -> lios_core::Result<()> {
    if let Some(checkpoint) = &progress.blob_checkpoint {
        TaskStore::open(&paths.database)?.upsert_checkpoint(&TaskObjectCheckpoint {
            task_id: task.id,
            remote_path: checkpoint.path.clone(),
            oid: checkpoint.oid.clone(),
            size: checkpoint.size,
            state: match checkpoint.state {
                CatalogBlobCheckpointState::Uploaded => CheckpointState::Uploaded,
                CatalogBlobCheckpointState::Committed => CheckpointState::Committed,
            },
        })?;
    }
    let state = match progress.phase {
        CatalogTransactionPhase::ValidateBlobs
        | CatalogTransactionPhase::UploadBlobs
        | CatalogTransactionPhase::Prepublish
        | CatalogTransactionPhase::ProbeCatalog => TaskState::Running,
        CatalogTransactionPhase::Publish | CatalogTransactionPhase::Cleanup => {
            TaskState::Committing
        }
    };
    let phase = transaction_phase_label(progress.phase).to_string();
    let phase_changed = task.phase.as_deref() != Some(phase.as_str());
    let observation = metrics.observe(
        progress.bytes_done,
        progress.bytes_total,
        phase_changed || progress.completed_items >= progress.total_items,
    );
    if !observation.should_publish {
        return Ok(());
    }
    task.state = state.clone();
    task.phase = Some(phase.clone());
    task.progress_done = progress.completed_items;
    task.progress_total = progress.total_items;
    task.bytes_done = progress.bytes_done;
    task.bytes_total = progress.bytes_total;
    task.speed_bps = observation.speed_bps;
    task.eta_seconds = observation.eta_seconds;
    let store = TaskStore::open(&paths.database)?;
    if task.label == "upload" {
        let item_phase = match progress.phase {
            CatalogTransactionPhase::ValidateBlobs
            | CatalogTransactionPhase::UploadBlobs
            | CatalogTransactionPhase::Prepublish
            | CatalogTransactionPhase::ProbeCatalog => "uploading",
            CatalogTransactionPhase::Publish | CatalogTransactionPhase::Cleanup => "committing",
        };
        store.update_items_state(
            task.id,
            TaskItemState::Running,
            Some(item_phase.to_string()),
            None,
            false,
        )?;
    }
    if !store.set_transaction_state(task.id, state)? {
        return Err(lios_core::LiosError::Unsupported(
            "task was interrupted before the catalog transaction phase could start".to_string(),
        ));
    }
    store.update_phase(task.id, Some(phase))?;
    store.update_transfer(
        task.id,
        task.progress_done,
        task.progress_total,
        task.bytes_done,
        task.bytes_total,
        task.speed_bps,
    )?;
    store.update_eta(task.id, task.eta_seconds)?;
    if let Some(app) = app {
        emit_task(app, paths, task.id);
    }
    Ok(())
}

#[cfg(test)]
fn update_task_state(
    paths: &LiosPaths,
    id: Uuid,
    state: TaskState,
    error: Option<String>,
) -> CommandResult<()> {
    task_store(paths)?
        .update_state(id, state, error)
        .map_err(to_err)
}

#[cfg(test)]
fn update_terminal_task_items(
    paths: &LiosPaths,
    task_id: Uuid,
    state: &TaskState,
    error: Option<String>,
) -> CommandResult<()> {
    let store = task_store(paths)?;
    match state {
        TaskState::Completed => store
            .update_items_state(task_id, TaskItemState::Completed, None, None, true)
            .map_err(to_err),
        TaskState::Failed => store
            .update_items_state(
                task_id,
                TaskItemState::Failed,
                None,
                Some(error.unwrap_or_else(|| "task failed".to_string())),
                false,
            )
            .map_err(to_err),
        TaskState::Canceled => store
            .update_items_state(
                task_id,
                TaskItemState::Canceled,
                None,
                Some("task canceled".to_string()),
                false,
            )
            .map_err(to_err),
        _ => Ok(()),
    }
}

fn read_token(paths: &LiosPaths) -> CommandResult<String> {
    unprotect_from_file(&paths.credentials).map_err(to_err)
}

fn key_from_config(config: &LiosConfig) -> CommandResult<KeyFile> {
    let path = config
        .key_file_path
        .clone()
        .ok_or_else(|| CommandError::invalid_input("key file is not configured"))?;
    KeyFile::load_from_path(path).map_err(to_err)
}

fn reset_staging(paths: &LiosPaths) -> CommandResult<()> {
    paths.ensure_dirs().map_err(to_err)?;
    if paths.staging.exists() {
        let staging = paths.staging.canonicalize().map_err(to_err)?;
        let home = paths.home.canonicalize().map_err(to_err)?;
        if !staging.starts_with(home) {
            return Err(CommandError::invalid_input(
                "refusing to clear staging outside ~/.lios",
            ));
        }
        fs::remove_dir_all(&paths.staging).map_err(to_err)?;
    }
    fs::create_dir_all(&paths.staging).map_err(to_err)
}

fn remote_to_staging_path(staging: &Path, remote_path: &str) -> CommandResult<PathBuf> {
    let relative = Path::new(remote_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CommandError::invalid_input(format!(
            "invalid remote object path in catalog: {remote_path}"
        )));
    }
    Ok(staging.join(relative))
}

#[cfg(test)]
fn sha256_hex_file(path: &Path) -> CommandResult<String> {
    sha256_hex_file_cancellable(path, || false)?.ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::Internal,
            "file hashing was unexpectedly canceled",
            false,
            None,
        )
    })
}

fn sha256_hex_file_cancellable(
    path: &Path,
    mut should_cancel: impl FnMut() -> bool,
) -> CommandResult<Option<String>> {
    let mut file = fs::File::open(path).map_err(to_err)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        if should_cancel() {
            return Ok(None);
        }
        let read = file.read(&mut buffer).map_err(to_err)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(Some(hex::encode(hasher.finalize())))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LocalRemoteFileValidation {
    Valid,
    Invalid,
    Canceled,
}

async fn validate_local_remote_file(
    local_path: &Path,
    file: &CatalogRemoteFile,
    cancellation: &CancellationToken,
) -> CommandResult<LocalRemoteFileValidation> {
    if cancellation.is_cancelled() {
        return Ok(LocalRemoteFileValidation::Canceled);
    }
    let metadata = match tokio::fs::metadata(local_path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(LocalRemoteFileValidation::Invalid)
        }
        Err(error) => return Err(to_err(error)),
    };
    if !metadata.is_file()
        || file
            .expected_size
            .is_some_and(|expected| metadata.len() != expected)
    {
        return Ok(LocalRemoteFileValidation::Invalid);
    }
    let Some(expected) = file.sha256.clone() else {
        return Ok(LocalRemoteFileValidation::Valid);
    };
    let path = local_path.to_path_buf();
    let cancellation = cancellation.clone();
    let actual = tokio::task::spawn_blocking(move || {
        sha256_hex_file_cancellable(&path, || cancellation.is_cancelled())
    })
    .await
    .map_err(|error| {
        CommandError::new(
            CommandErrorCode::Internal,
            format!("local verification worker failed: {error}"),
            false,
            None,
        )
    })??;
    match actual {
        Some(actual) if actual == expected => Ok(LocalRemoteFileValidation::Valid),
        Some(_) => Ok(LocalRemoteFileValidation::Invalid),
        None => Ok(LocalRemoteFileValidation::Canceled),
    }
}

fn current_catalog_references(
    paths: &LiosPaths,
    strict: bool,
) -> CommandResult<Option<Vec<String>>> {
    let catalog_path = paths.staging.join(CATALOG_FILE);
    if !catalog_path.exists() {
        return Ok(None);
    }
    let config = match load_config(paths) {
        Ok(config) => config,
        Err(error) if strict => return Err(error),
        Err(_) => return Ok(None),
    };
    let key = match key_from_config(&config) {
        Ok(key) => key,
        Err(error) if strict => return Err(error),
        Err(_) => return Ok(None),
    };
    let catalog = Catalog::from_staging(paths.staging.clone());
    let mut references = vec![CATALOG_FILE.to_string()];
    match catalog.remote_files_for_selection(&CatalogSelection::All, &key) {
        Ok(files) => references.extend(files.into_iter().map(|file| file.path)),
        Err(error) if strict => return Err(to_err(error)),
        Err(_) => return Ok(None),
    }
    Ok(Some(references))
}

fn cleanup_current_staging_cache(
    paths: &LiosPaths,
    prune_unreferenced: bool,
    strict: bool,
) -> CommandResult<CacheCleanupReport> {
    paths.ensure_dirs().map_err(to_err)?;
    if prune_unreferenced {
        if let Some(references) = current_catalog_references(paths, strict)? {
            return prune_unreferenced_staging(&paths.staging, references).map_err(to_err);
        }
    }
    cleanup_temporary_staging(&paths.staging).map_err(to_err)
}

#[cfg(test)]
async fn activate_new_task(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    mut task: TaskRecord,
) -> CommandResult<TaskRecord> {
    let mut lifecycle = gate.lock().await;
    insert_task(paths, &task)?;
    update_task_state(paths, task.id, TaskState::Running, None)?;
    lifecycle.active_workers.insert(task.id);
    task.state = TaskState::Running;
    Ok(task)
}

#[cfg(test)]
async fn activate_existing_task(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    task_id: Uuid,
    state: TaskState,
) -> CommandResult<()> {
    set_task_state(paths, gate, task_id, state, None).await
}

#[cfg(test)]
async fn set_task_state(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    task_id: Uuid,
    state: TaskState,
    error: Option<String>,
) -> CommandResult<()> {
    let _lifecycle = gate.lock().await;
    update_task_state(paths, task_id, state, error)
}

#[cfg(test)]
async fn interrupt_task_state(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    task_id: Uuid,
    state: TaskState,
) -> CommandResult<()> {
    let _lifecycle = gate.lock().await;
    if task_store(paths)?
        .interrupt_task(task_id, state)
        .map_err(to_err)?
    {
        Ok(())
    } else {
        Err(CommandError::invalid_input(
            "task cannot be interrupted after catalog publication has started",
        ))
    }
}

fn cleanup_is_safe(paths: &LiosPaths, lifecycle: &TaskLifecycleState) -> CommandResult<bool> {
    if !lifecycle.active_workers.is_empty() {
        return Ok(false);
    }
    Ok(!task_store(paths)?
        .list_summaries()
        .map_err(to_err)?
        .iter()
        .any(|task| task_state_is_active(&task.state)))
}

async fn cleanup_if_idle<T>(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    cleanup: impl FnOnce() -> CommandResult<T>,
) -> CommandResult<Option<T>> {
    let lifecycle = gate.lock().await;
    if !cleanup_is_safe(paths, &lifecycle)? {
        return Ok(None);
    }
    cleanup().map(Some)
}

#[cfg(test)]
async fn finish_active_worker(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    catalog_mutation_gate: &CatalogMutationGate,
    task_id: Uuid,
    intended_state: TaskState,
    error: Option<String>,
) -> CommandResult<TaskState> {
    finish_worker(
        paths,
        gate,
        catalog_mutation_gate,
        task_id,
        intended_state,
        error,
        true,
    )
    .await
}

#[cfg(test)]
async fn finish_worker(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    catalog_mutation_gate: &CatalogMutationGate,
    task_id: Uuid,
    intended_state: TaskState,
    error: Option<String>,
    preserve_control_state: bool,
) -> CommandResult<TaskState> {
    let _shared_staging_guard = catalog_mutation_gate.lock_shared_staging().await;
    let mut lifecycle = gate.lock().await;
    let result = (|| {
        let current_state = task_store(paths)?
            .get_summary(task_id)
            .map_err(to_err)?
            .map(|task| task.state);
        let final_state = match (preserve_control_state, current_state) {
            (true, Some(TaskState::Canceled)) => TaskState::Canceled,
            (true, Some(TaskState::Paused)) => TaskState::Paused,
            _ => intended_state,
        };
        let final_error = match final_state {
            TaskState::Failed | TaskState::Completed => error,
            _ => None,
        };
        if !preserve_control_state && final_state == TaskState::Completed {
            task_store(paths)?
                .mark_checkpoints_committed(task_id)
                .map_err(to_err)?;
        }
        update_task_phase(paths, task_id, None)?;
        update_task_state(paths, task_id, final_state.clone(), final_error.clone())?;
        update_terminal_task_items(paths, task_id, &final_state, final_error)?;
        Ok(final_state)
    })();
    lifecycle.active_workers.remove(&task_id);
    if cleanup_is_safe(paths, &lifecycle).unwrap_or(false) {
        let _ = cleanup_current_staging_cache(paths, true, false);
    }
    result
}

#[cfg(test)]
async fn finish_committed_worker(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    catalog_mutation_gate: &CatalogMutationGate,
    task_id: Uuid,
) -> CommandResult<TaskState> {
    let _shared_staging_guard = catalog_mutation_gate.lock_shared_staging().await;
    let mut lifecycle = gate.lock().await;
    let result = (|| {
        let mut store = TaskStore::open(&paths.database).map_err(to_err)?;
        if store.complete_reconciled_commit(task_id).map_err(to_err)?
            || store
                .get_summary(task_id)
                .map_err(to_err)?
                .is_some_and(|task| task.state == TaskState::Completed)
        {
            Ok(TaskState::Completed)
        } else {
            Err(CommandError::new(
                CommandErrorCode::CorruptedData,
                "committed task changed state before atomic finalization",
                false,
                None,
            ))
        }
    })();
    lifecycle.active_workers.remove(&task_id);
    if cleanup_is_safe(paths, &lifecycle).unwrap_or(false) {
        let _ = cleanup_current_staging_cache(paths, true, false);
    }
    result
}

async fn clear_task_record(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    task_id: Uuid,
) -> CommandResult<()> {
    let lifecycle = gate.lock().await;
    if lifecycle.active_workers.contains(&task_id) {
        return Err(CommandError::invalid_input(
            "active worker task cannot be cleared",
        ));
    }
    let store = task_store(paths)?;
    let task = store.get_summary(task_id).map_err(to_err)?;
    if task
        .as_ref()
        .is_some_and(|task| task_state_blocks_clear(&task.state))
    {
        return Err(CommandError::invalid_input("active task cannot be cleared"));
    }
    if let Some(spec) = store.load_spec(task_id).map_err(to_err)? {
        if let Some(cleanup_label) = terminal_staging_cleanup_label(&spec) {
            let task_paths = paths
                .for_task(spec.account_id(), spec.space_id(), task_id)
                .map_err(to_err)?;
            if let Err(error) = cleanup_terminal_task_staging(&task_paths, &spec, task_id) {
                append_task_warning(
                    &task_paths,
                    task_id,
                    &format!("{cleanup_label} staging cleanup failed: {}", error.message),
                )?;
                return Err(error);
            }
        }
    }
    let result = store.delete(task_id).map_err(to_err);
    drop(lifecycle);
    result
}

fn task_state_blocks_clear(state: &TaskState) -> bool {
    matches!(
        state,
        TaskState::Queued
            | TaskState::Preparing
            | TaskState::Running
            | TaskState::Paused
            | TaskState::Retrying
            | TaskState::Committing
    )
}

fn task_state_is_active(state: &TaskState) -> bool {
    match state {
        TaskState::Queued
        | TaskState::Preparing
        | TaskState::Running
        | TaskState::Paused
        | TaskState::Retrying
        | TaskState::Committing => true,
        TaskState::Failed | TaskState::Completed | TaskState::Canceled => false,
    }
}

fn terminal_staging_cleanup_label(spec: &TaskSpec) -> Option<&'static str> {
    match spec {
        TaskSpec::VerifySpace { .. } => Some("verification"),
        TaskSpec::RebuildCatalog { .. } => Some("catalog rebuild"),
        TaskSpec::Copy { .. }
        | TaskSpec::Sync { .. }
        | TaskSpec::Upload { .. }
        | TaskSpec::Delete { .. }
        | TaskSpec::Download { .. } => None,
    }
}

fn cleanup_terminal_task_staging(
    paths: &LiosPaths,
    spec: &TaskSpec,
    task_id: Uuid,
) -> CommandResult<()> {
    if terminal_staging_cleanup_label(spec).is_none() {
        return Ok(());
    }
    let Some(task) = task_store(paths)?.get_summary(task_id).map_err(to_err)? else {
        return Ok(());
    };
    if task_state_is_active(&task.state) || !paths.staging.exists() {
        return Ok(());
    }
    remove_scoped_staging_directory(paths, spec.account_id(), spec.space_id(), task_id)
}

fn remove_scoped_staging_directory(
    paths: &LiosPaths,
    account_id: &str,
    space_id: &str,
    scope_id: Uuid,
) -> CommandResult<()> {
    if !paths.staging.exists() {
        return Ok(());
    }
    let expected_staging = paths
        .home
        .join("staging")
        .join(account_id)
        .join(space_id)
        .join(scope_id.to_string());
    if paths.staging != expected_staging {
        return Err(CommandError::invalid_input(
            "refusing to clear an unexpected task staging path",
        ));
    }
    let staging_root = paths.home.join("staging");
    let account_dir = staging_root.join(account_id);
    let space_dir = account_dir.join(space_id);
    for directory in [
        paths.home.as_path(),
        staging_root.as_path(),
        account_dir.as_path(),
        space_dir.as_path(),
        paths.staging.as_path(),
    ] {
        let metadata = match fs::symlink_metadata(directory) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(to_err(error)),
        };
        if !metadata.is_dir() || metadata_is_link_or_junction(&metadata) {
            return Err(CommandError::invalid_input(
                "refusing to clear task staging through a link or junction",
            ));
        }
    }
    match fs::remove_dir_all(&paths.staging) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(to_err(error)),
    }
}

fn append_task_warning(paths: &LiosPaths, task_id: Uuid, warning: &str) -> CommandResult<()> {
    let store = task_store(paths)?;
    let Some(task) = store.get_summary(task_id).map_err(to_err)? else {
        return Ok(());
    };
    let message = task
        .error
        .as_deref()
        .map(|existing| format!("{existing}; {warning}"))
        .unwrap_or_else(|| warning.to_string());
    store
        .update_state(task_id, task.state, Some(message))
        .map_err(to_err)
}

fn cleanup_terminal_task_staging_and_record(
    paths: &LiosPaths,
    spec: &TaskSpec,
    task_id: Uuid,
) -> CommandResult<()> {
    let Some(cleanup_label) = terminal_staging_cleanup_label(spec) else {
        return Ok(());
    };
    if let Err(error) = cleanup_terminal_task_staging(paths, spec, task_id) {
        append_task_warning(
            paths,
            task_id,
            &format!("{cleanup_label} staging cleanup failed: {}", error.message),
        )?;
    }
    Ok(())
}

fn cleanup_terminal_task_staging_after_restart(paths: &LiosPaths) -> lios_core::Result<Vec<Uuid>> {
    let mut store = TaskStore::open(&paths.database)?;
    for task in store.list_summaries()? {
        if task_state_is_active(&task.state) {
            continue;
        }
        let Some(spec) = store.load_spec(task.id)? else {
            continue;
        };
        if terminal_staging_cleanup_label(&spec).is_none() {
            continue;
        }
        let task_paths = paths.for_task(spec.account_id(), spec.space_id(), task.id)?;
        cleanup_terminal_task_staging_and_record(&task_paths, &spec, task.id)
            .map_err(|error| lios_core::LiosError::Storage(error.message))?;
    }
    store.prune_terminal_history()
}

async fn cleanup_terminal_task_staging_after_restart_async(
    paths: LiosPaths,
) -> lios_core::Result<Vec<Uuid>> {
    tokio::task::spawn_blocking(move || cleanup_terminal_task_staging_after_restart(&paths))
        .await
        .map_err(|error| {
            lios_core::LiosError::Storage(format!(
                "terminal task staging cleanup worker failed: {error}"
            ))
        })?
}

fn start_terminal_task_staging_cleanup(app: &tauri::AppHandle, paths: &LiosPaths) {
    let app = app.clone();
    let paths = paths.clone();
    tauri::async_runtime::spawn(async move {
        if let Ok(removed) = cleanup_terminal_task_staging_after_restart_async(paths).await {
            emit_removed_tasks(&app, removed);
        }
    });
}

fn metadata_is_link_or_junction(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;

        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    false
}

struct StartupTaskRecovery {
    queued: Vec<Uuid>,
    reconcile: Vec<Uuid>,
}

#[cfg(test)]
#[derive(Default)]
struct StartupSpaceWork {
    reconcile: Vec<Uuid>,
    queued: Vec<Uuid>,
}

fn submission_summary(task: &TaskRecord) -> CommandResult<TaskSummary> {
    let item_count = u64::try_from(task.items.len()).map_err(|_| {
        CommandError::new(
            CommandErrorCode::Internal,
            "task item count exceeds the supported range",
            false,
            None,
        )
    })?;
    Ok(webview_safe_task_summary(TaskSummary {
        id: task.id,
        account_id: task.account_id.clone(),
        space_id: task.space_id.clone(),
        state: task.state.clone(),
        label: task.label.clone(),
        phase: task.phase.clone(),
        progress_total: task.progress_total,
        progress_done: task.progress_done,
        bytes_total: task.bytes_total,
        bytes_done: task.bytes_done,
        speed_bps: task.speed_bps,
        eta_seconds: task.eta_seconds,
        attempt: task.attempt,
        created_at: task.created_at.clone(),
        updated_at: task.updated_at.clone(),
        error: task.error.clone(),
        item_count,
        can_retry: false,
    }))
}

fn submit_and_spawn(
    app: &tauri::AppHandle,
    state: &AppContext,
    spec: TaskSpec,
    source_files: &[SourceFileSnapshot],
) -> CommandResult<TaskSummary> {
    let task = persist_submission(&state.paths, &spec, source_files).map_err(to_err)?;
    let summary = submission_summary(&task)?;
    emit_task(app, &state.paths, task.id);
    start_shared_worker(&state.paths)?;
    Ok(summary)
}

fn start_shared_worker(paths: &LiosPaths) -> CommandResult<()> {
    if paths.worker_running().map_err(CommandError::from)? {
        return Ok(());
    }
    let _ = fs::remove_file(paths.worker_stop_path());
    paths.ensure_worker_control_dir()?;
    let current = std::env::current_exe().map_err(to_err)?;
    let extension = current.extension().map(|value| value.to_os_string());
    let mut worker = current.with_file_name("lios-worker");
    if let Some(extension) = extension {
        worker.set_extension(extension);
    }
    if !worker.is_file() {
        return Err(CommandError::new(
            CommandErrorCode::Storage,
            "lios-worker is missing from the Desktop installation",
            false,
            None,
        ));
    }
    let home_root = paths.home.parent().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::Storage,
            "Lios Home has no parent directory for worker startup",
            false,
            None,
        )
    })?;
    ProcessCommand::new(worker)
        .arg("--home")
        .arg(home_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(to_err)?;
    Ok(())
}

fn recover_startup_tasks(paths: &LiosPaths) -> CommandResult<StartupTaskRecovery> {
    let mut store = TaskStore::open(&paths.database)?;
    let mut candidate_spaces = HashSet::new();
    for task in store.list_summaries()? {
        if !task_state_is_active(&task.state) {
            continue;
        }
        let space_id = match store.load_spec(task.id) {
            Ok(Some(spec)) => spec.space_id().to_string(),
            Ok(None) | Err(_) => task.space_id,
        };
        candidate_spaces.insert(space_id);
    }

    let mut recovered_spaces = HashSet::new();
    for space_id in candidate_spaces {
        let _process_space_lock = match paths.try_lock_space(&space_id) {
            Ok(lock) => lock,
            Err(SpaceLockError::Busy { .. } | SpaceLockError::InvalidSpaceId) => continue,
            Err(error) => return Err(CommandError::from(error)),
        };
        store
            .recover_after_restart_in_space(&space_id, "legacy task cannot be resumed")
            .map_err(to_err)?;
        recovered_spaces.insert(space_id);
    }

    let mut queued = Vec::new();
    let mut reconcile = Vec::new();
    for (task, spec) in store.list_startup_summaries_with_specs()? {
        if !recovered_spaces.contains(spec.space_id()) {
            continue;
        }
        match task.state {
            TaskState::Queued => queued.push(task.id),
            TaskState::Committing => reconcile.push(task.id),
            _ => {}
        }
    }
    Ok(StartupTaskRecovery { queued, reconcile })
}

#[cfg(test)]
fn group_startup_tasks(
    paths: &LiosPaths,
    recovery: StartupTaskRecovery,
) -> lios_core::Result<Vec<StartupSpaceWork>> {
    let store = TaskStore::open(&paths.database)?;
    let mut groups = Vec::<StartupSpaceWork>::new();
    let mut positions = HashMap::<String, usize>::new();
    for (task_id, needs_reconciliation) in recovery
        .reconcile
        .into_iter()
        .map(|task_id| (task_id, true))
        .chain(recovery.queued.into_iter().map(|task_id| (task_id, false)))
    {
        let spec = store.load_spec(task_id)?.ok_or_else(|| {
            lios_core::LiosError::DataCorruption(
                "startup task has no persisted specification".to_string(),
            )
        })?;
        let space_id = spec.space_id().to_string();
        let position = *positions.entry(space_id.clone()).or_insert_with(|| {
            groups.push(StartupSpaceWork::default());
            groups.len() - 1
        });
        if needs_reconciliation {
            groups[position].reconcile.push(task_id);
        } else {
            groups[position].queued.push(task_id);
        }
    }
    Ok(groups)
}

#[cfg(test)]
fn reconciliation_error_should_wait(error: &CommandError) -> bool {
    matches!(
        error.code,
        CommandErrorCode::Authentication
            | CommandErrorCode::Network
            | CommandErrorCode::RateLimited
            | CommandErrorCode::RemoteServer
            | CommandErrorCode::Storage
    )
}

#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupReconciliationOutcome {
    Continue,
    Replay,
    Stop,
}

#[cfg(test)]
fn startup_reconciliation_terminal_error(
    outcome: StartupReconciliationOutcome,
) -> Option<Option<(CommandErrorCode, bool)>> {
    match outcome {
        StartupReconciliationOutcome::Continue => Some(None),
        StartupReconciliationOutcome::Replay => None,
        StartupReconciliationOutcome::Stop => Some(Some((CommandErrorCode::RemoteConflict, false))),
    }
}

#[cfg(test)]
async fn retry_storage_operation<T>(
    mut operation: impl FnMut() -> CommandResult<T>,
) -> CommandResult<T> {
    let mut attempt = 1;
    loop {
        match operation() {
            Ok(value) => return Ok(value),
            Err(error) if error.code == CommandErrorCode::Storage => {
                tokio::time::sleep(retry_backoff(attempt)).await;
                attempt = attempt.saturating_add(1);
            }
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
fn observed_task_interrupt(
    paths: &LiosPaths,
    task_id: Uuid,
    cancellation: &CancellationToken,
) -> lios_core::Result<Option<TaskState>> {
    let persisted = task_interrupt_core(paths, task_id)?;
    if persisted.is_some() {
        return Ok(persisted);
    }
    Ok(cancellation.is_cancelled().then_some(TaskState::Canceled))
}

fn recovery_metadata_objects(
    remote_objects: &[StorageObject],
) -> CommandResult<Vec<StorageObject>> {
    let mut selected = remote_objects
        .iter()
        .filter(|object| {
            object.path.starts_with("recovery/nodes/")
                || (object.path.starts_with("objects/files/")
                    && object.path.ends_with("/manifest.enc"))
        })
        .cloned()
        .collect::<Vec<_>>();
    selected.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(selected)
}

#[cfg(test)]
fn validate_rebuild_revision(
    expected_revision: Option<&str>,
    current: &RepoRevision,
) -> CommandResult<String> {
    let expected = expected_revision
        .map(str::trim)
        .filter(|revision| !revision.is_empty())
        .ok_or_else(|| {
            CommandError::invalid_input("catalog rebuild requires a confirmed preview")
        })?;
    let current_commit = verification_commit_id(current)?;
    if expected != current_commit {
        return Err(CommandError::new(
            CommandErrorCode::RemoteConflict,
            "remote space changed after the rebuild preview",
            false,
            Some(serde_json::json!({
                "preview_revision": expected,
                "current_revision": current_commit,
            })),
        ));
    }
    Ok(expected.to_string())
}

#[cfg(test)]
fn plan_catalog_rebuild_sync(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    remote_objects: Vec<StorageObject>,
    expected_revision: RepoRevision,
) -> CommandResult<SyncWork> {
    let mut work = plan_catalog_sync(
        paths,
        catalog,
        key,
        CatalogBaseline {
            catalog_sha256: None,
            referenced_paths: HashSet::new(),
            remote_objects,
        },
    )?;
    work.delete.clear();
    work.expected_revision = Some(expected_revision);
    Ok(work)
}

fn catalog_rebuild_warnings(report: &CatalogRebuildReport) -> Vec<String> {
    if report.unreferenced_managed_objects == 0 {
        Vec::new()
    } else {
        vec![format!(
            "发现 {} 个未被重建 catalog 引用的远端对象，未执行删除",
            report.unreferenced_managed_objects
        )]
    }
}

#[cfg(test)]
fn discard_staged_catalog_for_rebuild(paths: &LiosPaths) -> CommandResult<()> {
    let catalog_path = paths.staging.join(CATALOG_FILE);
    let metadata = match fs::symlink_metadata(&catalog_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(to_err(error)),
    };
    if !metadata.is_file() || metadata_is_link_or_junction(&metadata) {
        return Err(CommandError::new(
            CommandErrorCode::CorruptedData,
            "staged catalog rebuild output is not a regular file",
            false,
            Some(serde_json::json!({ "path": CATALOG_FILE })),
        ));
    }
    fs::remove_file(catalog_path).map_err(to_err)
}

#[allow(clippy::too_many_arguments)]
async fn download_recovery_metadata(
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
    remote_objects: &[StorageObject],
    staging_dir: &Path,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(u64, u64, u64, u64) -> CommandResult<()>,
) -> CommandResult<bool> {
    let metadata_objects = recovery_metadata_objects(remote_objects)?;
    let total_objects = u64::try_from(metadata_objects.len()).map_err(|_| {
        CommandError::new(
            CommandErrorCode::CorruptedData,
            "recovery metadata object count overflowed",
            false,
            None,
        )
    })?;
    let total_bytes = metadata_objects.iter().try_fold(0u64, |total, object| {
        total.checked_add(object.size).ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::CorruptedData,
                "recovery metadata byte count overflowed",
                false,
                None,
            )
        })
    })?;
    let mut completed_objects = 0u64;
    let mut completed_bytes = 0u64;
    on_progress(0, total_objects, 0, total_bytes)?;
    for object in metadata_objects {
        if cancellation.is_cancelled() {
            return Ok(false);
        }
        let local_path = remote_to_staging_path(staging_dir, &object.path)?;
        let expected = CatalogRemoteFile {
            path: object.path.clone(),
            expected_size: Some(object.size),
            sha256: object.sha256.clone(),
        };
        let completed_before_object = completed_bytes;
        let mut progress_error = None;
        let download = adapter.download_object_with_progress(
            &repo.namespace,
            &repo.dataset,
            &object.path,
            &local_path,
            |object_bytes| {
                if progress_error.is_none() {
                    let bytes_done = completed_before_object.saturating_add(object_bytes);
                    if let Err(error) =
                        on_progress(completed_objects, total_objects, bytes_done, total_bytes)
                    {
                        progress_error = Some(error);
                    }
                }
            },
        );
        tokio::select! {
            result = download => result.map_err(to_err)?,
            _ = cancellation.cancelled() => return Ok(false),
        }
        if let Some(error) = progress_error {
            return Err(error);
        }
        match validate_local_remote_file(&local_path, &expected, cancellation).await? {
            LocalRemoteFileValidation::Valid => {}
            LocalRemoteFileValidation::Invalid => {
                return Err(CommandError::new(
                    CommandErrorCode::CorruptedData,
                    format!("downloaded recovery metadata is invalid: {}", object.path),
                    false,
                    Some(serde_json::json!({ "path": object.path })),
                ))
            }
            LocalRemoteFileValidation::Canceled => return Ok(false),
        }
        completed_objects = completed_objects.checked_add(1).ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::CorruptedData,
                "recovery metadata progress overflowed",
                false,
                None,
            )
        })?;
        completed_bytes = completed_bytes.checked_add(object.size).ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::CorruptedData,
                "recovery metadata byte progress overflowed",
                false,
                None,
            )
        })?;
        on_progress(
            completed_objects,
            total_objects,
            completed_bytes,
            total_bytes,
        )?;
    }
    Ok(true)
}

#[allow(clippy::too_many_arguments)]
async fn build_catalog_rebuild_snapshot(
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
    key: &KeyFile,
    paths: &LiosPaths,
    remote_objects: Vec<StorageObject>,
    cancellation: &CancellationToken,
    on_progress: impl FnMut(u64, u64, u64, u64) -> CommandResult<()>,
) -> CommandResult<Option<(Catalog, CatalogRebuildReport)>> {
    if !download_recovery_metadata(
        adapter,
        repo,
        &remote_objects,
        &paths.staging,
        cancellation,
        on_progress,
    )
    .await?
    {
        return Ok(None);
    }
    rebuild_staged_catalog(key, paths, remote_objects, cancellation).await
}

async fn rebuild_staged_catalog(
    key: &KeyFile,
    paths: &LiosPaths,
    remote_objects: Vec<StorageObject>,
    cancellation: &CancellationToken,
) -> CommandResult<Option<(Catalog, CatalogRebuildReport)>> {
    let staging_dir = paths.staging.clone();
    let key = key.clone();
    let cancellation = cancellation.clone();
    let outcome = tokio::task::spawn_blocking(move || {
        Catalog::rebuild_from_recovery_with_cancel(&key, staging_dir, &remote_objects, || {
            cancellation.is_cancelled()
        })
    })
    .await
    .map_err(|error| {
        CommandError::new(
            CommandErrorCode::Internal,
            format!("catalog rebuild worker failed: {error}"),
            false,
            None,
        )
    })?
    .map_err(to_err)?;
    match outcome {
        CatalogRebuildOutcome::Completed { catalog, report } => Ok(Some((catalog, report))),
        CatalogRebuildOutcome::Canceled => Ok(None),
    }
}

#[cfg(test)]
fn map_remote_integrity_error(error: lios_core::LiosError) -> CommandError {
    match error {
        lios_core::LiosError::DataCorruption(reason) => CommandError::new(
            CommandErrorCode::CorruptedData,
            format!("space verification failed: {reason}"),
            false,
            Some(serde_json::json!({ "reason": reason })),
        ),
        error => to_err(error),
    }
}

fn verification_commit_id(revision: &RepoRevision) -> CommandResult<&str> {
    revision
        .commit_id
        .as_deref()
        .filter(|commit_id| !commit_id.trim().is_empty())
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::Storage,
                "remote revision has no immutable commit id",
                false,
                None,
            )
        })
}

fn ensure_verification_revision_unchanged(
    started: &RepoRevision,
    finished: &RepoRevision,
) -> CommandResult<()> {
    if started.branch != finished.branch || started.commit_id != finished.commit_id {
        return Err(CommandError::new(
            CommandErrorCode::RemoteConflict,
            "remote space changed while verification was running",
            false,
            Some(serde_json::json!({
                "started_revision": started.commit_id,
                "finished_revision": finished.commit_id,
            })),
        ));
    }
    Ok(())
}

async fn ensure_verification_snapshot_is_current(
    branch_adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
    started: &RepoRevision,
    cancellation: &CancellationToken,
) -> CommandResult<bool> {
    let Some(finished) =
        head_revision_with_cancellation(branch_adapter, repo, cancellation).await?
    else {
        return Ok(false);
    };
    ensure_verification_revision_unchanged(started, &finished)?;
    Ok(true)
}

async fn head_revision_with_cancellation(
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
    cancellation: &CancellationToken,
) -> CommandResult<Option<RepoRevision>> {
    tokio::select! {
        biased;
        _ = cancellation.cancelled() => Ok(None),
        result = adapter.head_revision(&repo.namespace, &repo.dataset) => {
            result.map(Some).map_err(to_err)
        }
    }
}

#[tauri::command]
fn current_setup(state: tauri::State<'_, AppContext>) -> CommandResult<SetupSnapshot> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let (config, warning) = {
        let _config_guard = state.config_mutation_gate.lock()?;
        let mut config = load_config(&state.paths)?;
        let warning = prepare_startup_config(&state.paths, &mut config)?;
        (config, warning)
    };
    let spaces = config
        .spaces
        .iter()
        .map(|(name, repo)| RegisteredSpaceDto::new(name.clone(), repo.clone()))
        .collect();
    Ok(SetupSnapshot {
        paths: paths_dto(&state.paths),
        recovery_key: recovery_key_status(&config),
        config,
        has_token: state.paths.credentials.exists(),
        spaces,
        warning,
    })
}

#[tauri::command]
fn setup_token(state: tauri::State<'_, AppContext>, token: String) -> CommandResult<()> {
    state.paths.ensure_dirs().map_err(to_err)?;
    protect_to_file(token.trim(), &state.paths.credentials).map_err(to_err)
}

#[tauri::command]
async fn create_dataset_repo(
    state: tauri::State<'_, AppContext>,
    name: String,
    namespace: String,
    dataset: String,
    endpoint: String,
) -> CommandResult<()> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let repo = validate_repo(RepoConfig {
        namespace,
        dataset,
        endpoint,
    })?;
    let application = Application::new(state.paths.clone())?;
    application.create_dataset_repo(repo.clone()).await?;
    if let Err(error) = application.initialize_space(repo.clone()).await {
        return Err(CommandError::new(
            error.code,
            format!(
                "Repository {}/{} was created but not registered; retry with `lios space init {} {}/{}`: {}",
                repo.namespace, repo.dataset, name, repo.namespace, repo.dataset, error.message
            ),
            error.retryable,
            error.details,
        ));
    }
    SpaceRegistry::new(state.paths.clone()).add(&name, repo)?;
    Ok(())
}

#[tauri::command]
async fn list_dataset_repos(
    state: tauri::State<'_, AppContext>,
    endpoint: Option<String>,
) -> CommandResult<DatasetRepoListResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let config = load_config(&state.paths)?;
    let endpoint = configured_endpoint(&config, endpoint)?;
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(endpoint, token);
    let user = adapter.whoami().await.map_err(to_err)?;
    let repositories = adapter
        .list_dataset_repos_for_owner(Some(&user.username))
        .await
        .map_err(to_err)?
        .into_iter()
        .map(DatasetRepoSummaryDto::from)
        .collect();
    Ok(DatasetRepoListResult { user, repositories })
}

#[tauri::command]
async fn initialize_space(
    state: tauri::State<'_, AppContext>,
    space_name: String,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let _process_space_lock = state
        .paths
        .try_lock_space(&TaskScope::from_repo(&repo).space_id)
        .map_err(CommandError::from)?;
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), token);
    if !adapter
        .repo_exists(&repo.namespace, &repo.dataset)
        .await
        .map_err(to_err)?
    {
        return Err(CommandError::invalid_input(
            "space was not found or is not visible",
        ));
    }
    let _space_mutation_guard = state
        .task_manager
        .acquire_space(TaskScope::from_repo(&repo).space_id)
        .await;
    let _catalog_mutation_guard = state.catalog_mutation_gate.lock_mutation().await;
    ensure_space_can_initialize(
        &adapter,
        &repo.namespace,
        &repo.dataset,
        &state.paths.staging,
    )
    .await?;
    let baseline = CatalogBaseline {
        catalog_sha256: None,
        referenced_paths: HashSet::new(),
        remote_objects: adapter
            .list_objects(&repo.namespace, &repo.dataset, "")
            .await
            .map_err(to_err)?,
    };
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    reset_staging(&state.paths)?;
    let catalog = Catalog::initialize_empty(&repo.dataset, &key, state.paths.staging.clone())
        .map_err(to_err)?;
    let warnings =
        sync_current_catalog(&state.paths, &catalog, &key, &adapter, &repo, baseline).await?;
    let bytes = fs::metadata(catalog.encrypted_catalog_path())
        .map_err(to_err)?
        .len();
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: catalog.encrypted_catalog_path().display().to_string(),
        bytes,
        tree,
        warnings,
    })
}

#[tauri::command]
async fn load_space_catalog(
    state: tauri::State<'_, AppContext>,
    space_name: String,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let snapshot = Application::new(state.paths.clone())?
        .open_space(repo)
        .await?;
    Ok(CatalogLoadResult {
        local_path: snapshot.local_path.display().to_string(),
        bytes: snapshot.bytes,
        tree: snapshot.tree,
        warnings: snapshot.warnings,
    })
}

#[tauri::command]
async fn preview_upload_conflicts(
    state: tauri::State<'_, AppContext>,
    space_name: String,
    parent_node_id: String,
    paths: Vec<String>,
) -> CommandResult<Vec<UploadConflict>> {
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    Application::new(state.paths.clone())?
        .preview_upload_conflicts_in(repo, &parent_node_id, &paths)
        .await
}

#[tauri::command]
async fn create_folder(
    state: tauri::State<'_, AppContext>,
    space_name: String,
    parent_node_id: String,
    name: String,
) -> CommandResult<CatalogLoadResult> {
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let snapshot = Application::new(state.paths.clone())?
        .create_folder_in(repo, &parent_node_id, &name)
        .await?;
    Ok(CatalogLoadResult {
        local_path: snapshot.local_path.display().to_string(),
        bytes: snapshot.bytes,
        tree: snapshot.tree,
        warnings: snapshot.warnings,
    })
}

#[tauri::command]
async fn rename_node(
    state: tauri::State<'_, AppContext>,
    space_name: String,
    node_id: String,
    new_name: String,
) -> CommandResult<CatalogLoadResult> {
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let snapshot = Application::new(state.paths.clone())?
        .rename_node_in(repo, &node_id, &new_name)
        .await?;
    Ok(CatalogLoadResult {
        local_path: snapshot.local_path.display().to_string(),
        bytes: snapshot.bytes,
        tree: snapshot.tree,
        warnings: snapshot.warnings,
    })
}

#[tauri::command]
async fn search_catalog(
    state: tauri::State<'_, AppContext>,
    space_name: String,
    query: String,
) -> CommandResult<Vec<DriveItem>> {
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    Application::new(state.paths.clone())?
        .search_in(repo, &query)
        .await
}

#[tauri::command]
fn export_recovery_key(
    state: tauri::State<'_, AppContext>,
    destination: String,
) -> CommandResult<RecoveryKeyStatus> {
    let status = export_recovery_key_for_paths(
        &state.paths,
        &state.config_mutation_gate,
        Path::new(&destination),
    )?;
    state.app_log.log(
        "info",
        "recovery_key_exported",
        serde_json::json!({ "catalog_checked": false }),
    );
    Ok(status)
}

#[tauri::command]
async fn verify_recovery_key(
    state: tauri::State<'_, AppContext>,
    path: String,
) -> CommandResult<RecoveryKeyVerification> {
    verify_recovery_key_for_paths(&state.paths, Path::new(&path)).await
}

#[tauri::command]
async fn import_recovery_key(
    state: tauri::State<'_, AppContext>,
    path: String,
) -> CommandResult<RecoveryKeyVerification> {
    let verification =
        import_recovery_key_for_paths(&state.paths, &state.config_mutation_gate, Path::new(&path))
            .await?;
    state.app_log.log(
        "info",
        "recovery_key_imported",
        recovery_log_details(
            verification.catalog_checked,
            verification.checked_space.as_ref(),
        ),
    );
    Ok(verification)
}

#[tauri::command]
async fn enqueue_upload_to_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    space_name: String,
    parent_node_id: String,
    paths: Vec<String>,
    mut conflict_resolutions: Vec<ConflictResolution>,
) -> CommandResult<TaskSummary> {
    if paths.is_empty() {
        return Err(CommandError::invalid_input("upload paths cannot be empty"));
    }
    let mut upload_paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    if upload_paths
        .iter()
        .any(|path| !path.is_absolute() || !path.exists())
    {
        return Err(CommandError::invalid_input(
            "upload paths must be existing absolute paths",
        ));
    }
    let skipped_sources = conflict_resolutions
        .iter()
        .filter(|resolution| resolution.action == ConflictAction::Skip)
        .map(|resolution| resolution.source_path.clone())
        .collect::<HashSet<_>>();
    upload_paths.retain(|path| !skipped_sources.contains(path.to_string_lossy().as_ref()));
    conflict_resolutions.retain(|resolution| resolution.action != ConflictAction::Skip);
    if upload_paths.is_empty() {
        return Err(CommandError::invalid_input(
            "all selected upload paths were skipped",
        ));
    }
    let config = load_config(&state.paths)?;
    key_from_config(&config)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let scope = TaskScope::from_repo(&repo);
    let source_snapshot = snapshot_upload_sources(&upload_paths).map_err(to_err)?;
    let spec = TaskSpec::Upload {
        account_id: scope.account_id,
        space_id: scope.space_id,
        repo,
        parent_node_id,
        source_paths: upload_paths,
        source_snapshot: Some(source_snapshot.clone()),
        chunk_size: config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
        conflict_resolutions,
    };
    submit_and_spawn(&app, state.inner(), spec, &source_snapshot.files)
}

#[tauri::command]
async fn enqueue_delete_nodes(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    space_name: String,
    node_ids: Vec<String>,
) -> CommandResult<TaskSummary> {
    if node_ids.is_empty() {
        return Err(CommandError::invalid_input(
            "delete selection cannot be empty",
        ));
    }
    let config = load_config(&state.paths)?;
    key_from_config(&config)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let scope = TaskScope::from_repo(&repo);
    let spec = TaskSpec::Delete {
        account_id: scope.account_id,
        space_id: scope.space_id,
        repo,
        node_ids,
    };
    submit_and_spawn(&app, state.inner(), spec, &[])
}

#[tauri::command]
async fn enqueue_download(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    space_name: String,
    node_ids: Vec<String>,
    output_dir: String,
) -> CommandResult<TaskSummary> {
    let prepared = prepare_download_task(node_ids, output_dir)?;
    let CatalogSelection::Nodes(node_ids) = prepared.selection else {
        return Err(CommandError::invalid_input(
            "download task must contain node selections",
        ));
    };
    let config = load_config(&state.paths)?;
    key_from_config(&config)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let scope = TaskScope::from_repo(&repo);
    let spec = TaskSpec::Download {
        account_id: scope.account_id,
        space_id: scope.space_id,
        repo,
        node_ids,
        output_dir: prepared.output_dir,
    };
    submit_and_spawn(&app, state.inner(), spec, &[])
}

#[tauri::command]
async fn enqueue_verify_space(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    space_name: String,
    full: bool,
) -> CommandResult<TaskSummary> {
    let config = load_config(&state.paths)?;
    key_from_config(&config)?;
    read_token(&state.paths)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let scope = TaskScope::from_repo(&repo);
    let spec = TaskSpec::VerifySpace {
        account_id: scope.account_id,
        space_id: scope.space_id,
        repo,
        full,
    };
    submit_and_spawn(&app, state.inner(), spec, &[])
}

#[tauri::command]
async fn preview_rebuild_catalog(
    state: tauri::State<'_, AppContext>,
    space_name: String,
) -> CommandResult<CatalogRebuildPreviewResult> {
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let scope = TaskScope::from_repo(&repo);
    let _space_permit = state
        .task_manager
        .acquire_space(scope.space_id.clone())
        .await;
    let branch_adapter = ModelScopeAdapter::new(repo.endpoint.clone(), read_token(&state.paths)?);
    let cancellation = CancellationToken::new();
    let started_revision = head_revision_with_cancellation(&branch_adapter, &repo, &cancellation)
        .await?
        .ok_or_else(|| CommandError::invalid_input("catalog rebuild preview was canceled"))?;
    let revision = verification_commit_id(&started_revision)?.to_string();
    let adapter = branch_adapter.clone().with_revision(revision.clone());
    let remote_objects = adapter
        .list_objects(&repo.namespace, &repo.dataset, "")
        .await
        .map_err(to_err)?;
    if remote_objects
        .iter()
        .any(|object| object.path == CATALOG_FILE)
    {
        return Err(CommandError::already_initialized(
            "remote catalog still exists; rebuild preview is only available for a missing catalog",
        ));
    }

    let preview_id = Uuid::new_v4();
    let preview_paths = state
        .paths
        .for_task(&scope.account_id, &scope.space_id, preview_id)
        .map_err(to_err)?;
    preview_paths.ensure_dirs().map_err(to_err)?;
    let operation = async {
        let rebuilt = build_catalog_rebuild_snapshot(
            &adapter,
            &repo,
            &key,
            &preview_paths,
            remote_objects,
            &cancellation,
            |_done, _total, _bytes_done, _bytes_total| Ok(()),
        )
        .await?
        .ok_or_else(|| CommandError::invalid_input("catalog rebuild preview was canceled"))?;
        ensure_verification_snapshot_is_current(
            &branch_adapter,
            &repo,
            &started_revision,
            &cancellation,
        )
        .await?
        .then_some(())
        .ok_or_else(|| CommandError::invalid_input("catalog rebuild preview was canceled"))?;
        let (catalog, report) = rebuilt;
        let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
        let warnings = catalog_rebuild_warnings(&report);
        Ok(CatalogRebuildPreviewResult {
            revision,
            tree,
            report,
            warnings,
        })
    }
    .await;
    let cleanup = remove_scoped_staging_directory(
        &preview_paths,
        &scope.account_id,
        &scope.space_id,
        preview_id,
    );
    match operation {
        Ok(preview) => {
            cleanup?;
            Ok(preview)
        }
        Err(error) => {
            let _ = cleanup;
            Err(error)
        }
    }
}

#[tauri::command]
async fn enqueue_rebuild_catalog(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    space_name: String,
    expected_revision: String,
) -> CommandResult<TaskSummary> {
    let config = load_config(&state.paths)?;
    key_from_config(&config)?;
    read_token(&state.paths)?;
    let repo = SpaceRegistry::new(state.paths.clone()).resolve(&space_name)?;
    let expected_revision = expected_revision.trim();
    if expected_revision.is_empty() {
        return Err(CommandError::invalid_input(
            "catalog rebuild requires a confirmed preview revision",
        ));
    }
    let scope = TaskScope::from_repo(&repo);
    let spec = TaskSpec::RebuildCatalog {
        account_id: scope.account_id,
        space_id: scope.space_id,
        repo,
        expected_revision: Some(expected_revision.to_string()),
    };
    submit_and_spawn(&app, state.inner(), spec, &[])
}

#[tauri::command]
fn list_tasks(state: tauri::State<'_, AppContext>) -> CommandResult<Vec<TaskSummary>> {
    task_summaries_for_paths(&state.paths)
}

#[tauri::command]
fn get_task(
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Option<TaskSummary>> {
    task_summary_for_paths(&state.paths, task_id)
}

#[tauri::command]
fn list_task_items(
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
    offset: u64,
    limit: u64,
) -> CommandResult<TaskItemsPageDto> {
    list_task_items_for_paths(&state.paths, task_id, offset, limit)
}

#[tauri::command]
async fn cleanup_local_cache(
    state: tauri::State<'_, AppContext>,
) -> CommandResult<CacheCleanupReport> {
    let _shared_staging_guard = state.catalog_mutation_gate.lock_shared_staging().await;
    match cleanup_if_idle(&state.paths, &state.task_lifecycle_gate, || {
        cleanup_current_staging_cache(&state.paths, true, false)
    })
    .await?
    {
        Some(report) => Ok(report),
        None => Err(CommandError::invalid_input(
            "active tasks must finish before cleaning local cache",
        )),
    }
}

#[tauri::command]
async fn pause_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<()> {
    let summary = task_summary_for_paths(&state.paths, task_id)?
        .ok_or_else(|| CommandError::invalid_input("task was not found"))?;
    if summary.state == TaskState::Queued {
        Application::new(state.paths.clone())?
            .pause_task(task_id)
            .await?;
    } else {
        state.paths.ensure_worker_control_dir()?;
        fs::write(state.paths.worker_pause_path(task_id), b"pause\n").map_err(to_err)?;
    }
    emit_task(&app, &state.paths, task_id);
    Ok(())
}

#[tauri::command]
async fn resume_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<()> {
    Application::new(state.paths.clone())?.requeue_paused_task(task_id)?;
    start_shared_worker(&state.paths)?;
    emit_task(&app, &state.paths, task_id);
    Ok(())
}

#[tauri::command]
async fn retry_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<()> {
    Application::new(state.paths.clone())?.requeue_failed_task(task_id)?;
    emit_task(&app, &state.paths, task_id);
    start_shared_worker(&state.paths)?;
    Ok(())
}

#[tauri::command]
async fn cancel_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<()> {
    let summary = task_summary_for_paths(&state.paths, task_id)?
        .ok_or_else(|| CommandError::invalid_input("task was not found"))?;
    if matches!(summary.state, TaskState::Queued | TaskState::Paused) {
        Application::new(state.paths.clone())?
            .cancel_task(task_id)
            .await?;
    } else {
        state.paths.ensure_worker_control_dir()?;
        fs::write(state.paths.worker_cancel_path(task_id), b"cancel\n").map_err(to_err)?;
    }
    emit_task(&app, &state.paths, task_id);
    Ok(())
}

#[tauri::command]
async fn clear_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<()> {
    clear_task_record(&state.paths, &state.task_lifecycle_gate, task_id).await?;
    emit_removed_tasks(&app, vec![task_id]);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    macro_rules! generate_tauri_handler {
        ($($command:ident),* $(,)?) => {
            tauri::generate_handler![$($command),*]
        };
    }

    let builder = tauri::Builder::default();
    #[cfg(not(test))]
    let builder = builder.plugin(tauri_plugin_dialog::init());
    builder
        .manage(AppContext::new())
        .setup(|app| {
            let handle = app.handle().clone();
            let paths = app.state::<AppContext>().paths.clone();
            let recovery = recover_startup_tasks(&paths)?;
            app.state::<AppContext>().app_log.log(
                "info",
                "startup_task_recovery",
                serde_json::json!({
                    "queued": recovery.queued.len(),
                    "reconcile": recovery.reconcile.len(),
                    "total": recovery.queued.len() + recovery.reconcile.len(),
                }),
            );
            start_terminal_task_staging_cleanup(&handle, &paths);
            if !recovery.queued.is_empty() || !recovery.reconcile.is_empty() {
                start_shared_worker(&paths)?;
            }
            Ok(())
        })
        .invoke_handler(with_registered_commands!(generate_tauri_handler))
        .run(tauri::generate_context!())
        .expect("failed to run Lios desktop app");
}

#[cfg(test)]
mod task_center_backend_tests {
    use std::path::PathBuf;

    use lios_core::catalog::SourceFileSnapshot;
    use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
    use lios_core::tasks::{
        TaskItem, TaskItemState, TaskRecord, TaskSpec, TaskState, TaskStore, TaskSummary,
    };
    use lios_core::LiosError;
    use serde_json::json;
    use tempfile::tempdir;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;
    use uuid::Uuid;

    use super::{
        activate_new_task, cleanup_is_safe, clear_task_record, finish_active_worker,
        interrupt_task_state, list_task_items_for_paths, observed_task_interrupt, paths_dto,
        persist_submission, recover_startup_tasks, recovery_key_status, submission_summary,
        task_summaries_for_paths, AppContext, CatalogMutationGate, CommandError, CommandErrorCode,
        SetupSnapshot, TaskLifecycleState, TaskUpdateEvent,
    };

    fn corrupt_task_item_state(paths: &LiosPaths, item_id: Uuid) {
        let connection = rusqlite::Connection::open(&paths.database).unwrap();
        connection
            .execute(
                "UPDATE task_items SET state = 'FutureState' WHERE id = ?1",
                rusqlite::params![item_id.to_string()],
            )
            .unwrap();
    }

    fn insert_malformed_task(paths: &LiosPaths, state: TaskState) -> TaskRecord {
        let store = TaskStore::open(&paths.database).unwrap();
        let task = TaskRecord::queued("malformed item task", 1);
        let item = TaskItem {
            id: Uuid::new_v4(),
            task_id: task.id,
            name: "broken.bin".to_string(),
            relative_path: None,
            source_path: None,
            source_modified_at_ns: None,
            size: 1,
            state: TaskItemState::Queued,
            phase: None,
            bytes_done: 0,
            bytes_total: 1,
            error: None,
        };
        store.insert(&task).unwrap();
        store.update_state(task.id, state, None).unwrap();
        store.upsert_item(&item).unwrap();
        corrupt_task_item_state(paths, item.id);
        task
    }

    fn summary() -> TaskSummary {
        TaskSummary {
            id: Uuid::new_v4(),
            account_id: "account-a".to_string(),
            space_id: "space-a".to_string(),
            state: TaskState::Running,
            label: "upload album".to_string(),
            phase: Some("uploading".to_string()),
            progress_total: 2,
            progress_done: 1,
            bytes_total: 20,
            bytes_done: 10,
            speed_bps: 5,
            eta_seconds: Some(2),
            attempt: 1,
            created_at: "2026-07-12T00:00:00Z".to_string(),
            updated_at: "2026-07-12T00:00:01Z".to_string(),
            error: None,
            item_count: 2,
            can_retry: false,
        }
    }

    #[test]
    fn desktop_startup_preserves_active_download_sidecars() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let sidecar = paths
            .staging
            .join("a".repeat(64))
            .join("b".repeat(64))
            .join(Uuid::new_v4().to_string())
            .join("catalog.download");
        std::fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
        std::fs::write(&sidecar, b"active download").unwrap();

        AppContext::from_paths(paths);

        assert_eq!(std::fs::read(sidecar).unwrap(), b"active download");
    }

    fn assert_summary_json(value: &serde_json::Value) {
        assert_eq!(value["item_count"], json!(2));
        assert_eq!(value["can_retry"], json!(false));
        assert!(value.get("items").is_none());
        assert!(value.get("source_path").is_none());
        assert!(value.get("source_modified_at_ns").is_none());
    }

    #[test]
    fn setup_omits_tasks_and_targeted_events_serialize_safe_summaries() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let config = LiosConfig::default();
        let task = summary();
        let setup = SetupSnapshot {
            paths: paths_dto(&paths),
            recovery_key: recovery_key_status(&config),
            config,
            has_token: false,
            spaces: Vec::new(),
            warning: None,
        };
        let event = TaskUpdateEvent::Upsert {
            task: Box::new(task),
        };

        assert!(serde_json::to_value(setup).unwrap().get("tasks").is_none());
        assert_summary_json(&serde_json::to_value(event).unwrap()["task"]);
    }

    #[tokio::test]
    async fn persisted_failed_task_events_do_not_expose_absolute_local_paths() {
        const WINDOWS_SENTINEL: &str = r"C:\Users\LIOS_WINDOWS_PRIVATE\Documents\secret.bin";
        const UNIX_SENTINEL: &str = "/home/LIOS_UNIX_PRIVATE/Documents/secret.bin";

        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let lifecycle = Mutex::new(TaskLifecycleState::default());
        let catalog_mutation_gate = CatalogMutationGate::default();
        let task = activate_new_task(&paths, &lifecycle, TaskRecord::queued("upload", 1))
            .await
            .unwrap();
        let error = CommandError::from(LiosError::Unsupported(format!(
            "source paths no longer exist: {WINDOWS_SENTINEL}; {UNIX_SENTINEL}"
        )));

        let final_state = finish_active_worker(
            &paths,
            &lifecycle,
            &catalog_mutation_gate,
            task.id,
            TaskState::Failed,
            Some(error.message),
        )
        .await
        .unwrap();
        let summary = TaskStore::open(&paths.database)
            .unwrap()
            .get_summary(task.id)
            .unwrap()
            .unwrap();
        let event = serde_json::to_string(&TaskUpdateEvent::Upsert {
            task: Box::new(summary.clone()),
        })
        .unwrap();

        assert_eq!(final_state, TaskState::Failed);
        assert_eq!(
            summary.error.as_deref(),
            Some("selected sources no longer exist")
        );
        for marker in ["LIOS_WINDOWS_PRIVATE", "LIOS_UNIX_PRIVATE"] {
            assert!(!event.contains(marker), "{event}");
        }
    }

    #[test]
    fn webview_task_reads_scrub_historical_absolute_path_errors() {
        const WINDOWS_SENTINEL: &str = r"C:\Users\LIOS_OLD_WINDOWS_PRIVATE\Documents\secret.bin";
        const UNIX_SENTINEL: &str = "/home/LIOS_OLD_UNIX_PRIVATE/Documents/secret.bin";

        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let store = TaskStore::open(&paths.database).unwrap();
        let task = TaskRecord::queued("upload", 1);
        store.insert(&task).unwrap();
        store
            .upsert_item(&TaskItem {
                id: Uuid::new_v4(),
                task_id: task.id,
                name: "secret.bin".to_string(),
                relative_path: Some(PathBuf::from("folder/secret.bin")),
                source_path: Some(PathBuf::from(WINDOWS_SENTINEL)),
                source_modified_at_ns: None,
                size: 1,
                state: TaskItemState::Queued,
                phase: None,
                bytes_done: 0,
                bytes_total: 1,
                error: None,
            })
            .unwrap();
        store
            .update_state(
                task.id,
                TaskState::Failed,
                Some(format!("source path no longer exists: {WINDOWS_SENTINEL}")),
            )
            .unwrap();
        store
            .update_items_state(
                task.id,
                TaskItemState::Failed,
                None,
                Some(format!(
                    "IO error for operation on {UNIX_SENTINEL}: permission denied"
                )),
                false,
            )
            .unwrap();
        drop(store);

        let summaries = task_summaries_for_paths(&paths).unwrap();
        let items = list_task_items_for_paths(&paths, task.id, 0, 20).unwrap();
        let serialized = serde_json::to_string(&(summaries, items)).unwrap();

        for marker in ["LIOS_OLD_WINDOWS_PRIVATE", "LIOS_OLD_UNIX_PRIVATE"] {
            assert!(!serialized.contains(marker), "{serialized}");
        }
    }

    #[test]
    fn task_submission_response_is_a_safe_summary() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let sentinel = temp.path().join("private").join("secret.bin");
        let source = SourceFileSnapshot {
            source_path: sentinel.clone(),
            relative_path: "secret.bin".into(),
            size: 7,
            modified_at_ns: Some(123_456_789),
        };
        let spec = TaskSpec::Upload {
            account_id: "account-a".to_string(),
            space_id: "space-a".to_string(),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "safe-summary".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            },
            parent_node_id: "root".to_string(),
            source_paths: vec![sentinel.clone()],
            source_snapshot: None,
            chunk_size: 1,
            conflict_resolutions: Vec::new(),
        };
        let task = persist_submission(&paths, &spec, std::slice::from_ref(&source)).unwrap();

        let summary = submission_summary(&task).unwrap();
        let value = serde_json::to_value(summary).unwrap();
        let serialized = value.to_string();

        assert_eq!(value["item_count"], json!(1));
        assert_eq!(value["can_retry"], json!(false));
        assert!(value.get("items").is_none());
        assert!(value.get("source_path").is_none());
        assert!(value.get("source_modified_at_ns").is_none());
        assert!(!serialized.contains(sentinel.to_string_lossy().as_ref()));
    }

    #[test]
    fn task_item_page_validates_limits_and_missing_tasks() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let store = TaskStore::open(&paths.database).unwrap();
        let task = TaskRecord::queued("upload album", 1);
        store.insert(&task).unwrap();
        store
            .upsert_item(&TaskItem {
                id: Uuid::new_v4(),
                task_id: task.id,
                name: "a.bin".to_string(),
                relative_path: Some("a.bin".into()),
                source_path: Some(temp.path().join("a.bin")),
                source_modified_at_ns: None,
                size: 1,
                state: TaskItemState::Queued,
                phase: None,
                bytes_done: 0,
                bytes_total: 1,
                error: None,
            })
            .unwrap();
        drop(store);

        for limit in [0, 201] {
            let error = list_task_items_for_paths(&paths, task.id, 0, limit).unwrap_err();
            assert_eq!(error.code, CommandErrorCode::InvalidInput);
        }
        let missing = list_task_items_for_paths(&paths, Uuid::new_v4(), 0, 20).unwrap_err();
        assert_eq!(missing.code, CommandErrorCode::InvalidInput);

        let page = list_task_items_for_paths(&paths, task.id, 0, 20).unwrap();
        assert_eq!(page.task_id, task.id);
        assert_eq!(page.offset, 0);
        assert_eq!(page.total, 1);
        assert_eq!(page.items.len(), 1);
    }

    #[test]
    fn task_item_page_serialization_omits_sensitive_source_fields() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let store = TaskStore::open(&paths.database).unwrap();
        let task = TaskRecord::queued("upload album", 1);
        store.insert(&task).unwrap();
        store
            .upsert_item(&TaskItem {
                id: Uuid::new_v4(),
                task_id: task.id,
                name: "a.bin".to_string(),
                relative_path: Some("folder/a.bin".into()),
                source_path: Some(temp.path().join("private/a.bin")),
                source_modified_at_ns: Some(123),
                size: 1,
                state: TaskItemState::Queued,
                phase: Some("preparing".to_string()),
                bytes_done: 0,
                bytes_total: 1,
                error: None,
            })
            .unwrap();
        drop(store);

        let page = list_task_items_for_paths(&paths, task.id, 0, 20).unwrap();
        let value = serde_json::to_value(page).unwrap();
        let item = &value["items"][0];

        assert_eq!(item["name"], json!("a.bin"));
        assert_eq!(item["relative_path"], json!("folder/a.bin"));
        assert!(item.get("source_path").is_none());
        assert!(item.get("source_modified_at_ns").is_none());
    }

    #[test]
    fn task_item_page_rejects_offsets_unsafe_for_javascript_or_sqlite() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let store = TaskStore::open(&paths.database).unwrap();
        let task = TaskRecord::queued("upload album", 0);
        store.insert(&task).unwrap();
        drop(store);

        for offset in [9_007_199_254_740_992, u64::MAX] {
            let error = list_task_items_for_paths(&paths, task.id, offset, 20).unwrap_err();
            assert_eq!(error.code, CommandErrorCode::InvalidInput);
        }
    }

    #[test]
    fn state_queries_ignore_malformed_items_on_unrelated_tasks() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        insert_malformed_task(&paths, TaskState::Completed);
        let store = TaskStore::open(&paths.database).unwrap();
        let active = TaskRecord::queued("active task", 0);
        store.insert(&active).unwrap();
        store
            .update_state(active.id, TaskState::Running, None)
            .unwrap();
        drop(store);

        assert_eq!(super::task_interrupt_core(&paths, active.id).unwrap(), None);
        assert_eq!(
            observed_task_interrupt(&paths, active.id, &CancellationToken::new()).unwrap(),
            None
        );
        assert!(!cleanup_is_safe(&paths, &TaskLifecycleState::default()).unwrap());
    }

    #[tokio::test]
    async fn control_paths_ignore_malformed_items_on_the_target_task() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let task = insert_malformed_task(&paths, TaskState::Running);
        let lifecycle = Mutex::new(TaskLifecycleState::default());

        interrupt_task_state(&paths, &lifecycle, task.id, TaskState::Canceled)
            .await
            .unwrap();

        assert_eq!(
            observed_task_interrupt(&paths, task.id, &CancellationToken::new()).unwrap(),
            Some(TaskState::Canceled)
        );
        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .get_summary(task.id)
                .unwrap()
                .unwrap()
                .state,
            TaskState::Canceled
        );
    }

    #[tokio::test]
    async fn finish_and_clear_paths_ignore_malformed_items_on_the_target_task() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let paused = insert_malformed_task(&paths, TaskState::Paused);
        let lifecycle = Mutex::new(TaskLifecycleState::default());

        let final_state = finish_active_worker(
            &paths,
            &lifecycle,
            &CatalogMutationGate::default(),
            paused.id,
            TaskState::Completed,
            None,
        )
        .await
        .unwrap();

        assert_eq!(final_state, TaskState::Paused);

        let completed = insert_malformed_task(&paths, TaskState::Completed);
        clear_task_record(&paths, &lifecycle, completed.id)
            .await
            .unwrap();
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .get_summary(completed.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn startup_recovery_lists_queued_specs_without_decoding_items() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = TaskSpec::Delete {
            account_id: "a".repeat(64),
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            },
            node_ids: vec!["node-a".to_string()],
        };
        let task = TaskRecord::queued_for_spec(&spec);
        let item = TaskItem {
            id: Uuid::new_v4(),
            task_id: task.id,
            name: "broken.bin".to_string(),
            relative_path: None,
            source_path: None,
            source_modified_at_ns: None,
            size: 1,
            state: TaskItemState::Queued,
            phase: None,
            bytes_done: 0,
            bytes_total: 1,
            error: None,
        };
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store.upsert_item(&item).unwrap();
        corrupt_task_item_state(&paths, item.id);
        drop(store);

        let recovery = recover_startup_tasks(&paths).unwrap();

        assert_eq!(recovery.queued, vec![task.id]);
        assert!(recovery.reconcile.is_empty());
        let queued = TaskStore::open(&paths.database)
            .unwrap()
            .list_queued_summaries_with_specs()
            .unwrap();
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].0.id, task.id);
        assert_eq!(queued[0].1.account_id(), spec.account_id());
        assert_eq!(queued[0].1.space_id(), spec.space_id());
    }

    #[test]
    fn startup_recovery_skips_a_space_locked_by_another_frontend() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = TaskSpec::Delete {
            account_id: "a".repeat(64),
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            },
            node_ids: vec!["node-a".to_string()],
        };
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Running, None)
            .unwrap();
        let _other_frontend = paths.try_lock_space(spec.space_id()).unwrap();

        let recovery = recover_startup_tasks(&paths).unwrap();

        assert!(recovery.queued.is_empty());
        assert!(recovery.reconcile.is_empty());
        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .get_summary(task.id)
                .unwrap()
                .unwrap()
                .state,
            TaskState::Running
        );
    }
}

#[cfg(test)]
mod catalog_transaction_integration_tests {
    use super::*;
    use tempfile::tempdir;

    fn desired_file(path: &str, local_path: PathBuf) -> CatalogSyncFile {
        CatalogSyncFile {
            path: path.to_string(),
            expected_sha256: Some(sha256_hex_file(&local_path).unwrap()),
            expected_size: Some(fs::metadata(&local_path).unwrap().len()),
            local_path: Some(local_path),
        }
    }

    #[test]
    fn baseline_keeps_the_exact_downloaded_catalog_hash_and_old_references() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let key = KeyFile::generate_to_path(temp.path().join("key.yaml")).unwrap();
        let catalog = Catalog::initialize_empty("Space", &key, paths.staging.clone()).unwrap();
        let expected_hash = sha256_hex_file(&paths.staging.join(CATALOG_FILE)).unwrap();

        let baseline = catalog_baseline_from_downloaded_catalog(
            &paths,
            &catalog,
            &key,
            vec![StorageObject {
                path: CATALOG_FILE.to_string(),
                size: 1,
                sha256: Some(expected_hash.clone()),
            }],
        )
        .unwrap();

        assert_eq!(
            baseline.catalog_sha256.as_deref(),
            Some(expected_hash.as_str())
        );
        assert!(baseline.referenced_paths.contains(CATALOG_FILE));
        assert!(baseline
            .referenced_paths
            .iter()
            .any(|path| path.starts_with("recovery/nodes/")));
    }

    #[test]
    fn only_paths_absent_from_the_old_catalog_are_prepublish_safe() {
        let temp = tempdir().unwrap();
        let catalog_path = temp.path().join("catalog.enc");
        let existing_path = temp.path().join("existing.enc");
        let orphan_path = temp.path().join("orphan.lios");
        let new_path = temp.path().join("new.lios");
        fs::write(&catalog_path, b"new catalog").unwrap();
        fs::write(&existing_path, b"updated descriptor").unwrap();
        fs::write(&orphan_path, b"changed orphan").unwrap();
        fs::write(&new_path, b"new chunk").unwrap();
        let existing_remote = "recovery/nodes/existing.enc";
        let orphan_remote = "objects/files/orphan/chunks/chunk.lios";
        let new_remote = "objects/files/new/chunks/chunk.lios";
        let desired = vec![
            desired_file(CATALOG_FILE, catalog_path),
            desired_file(existing_remote, existing_path),
            desired_file(orphan_remote, orphan_path),
            desired_file(new_remote, new_path),
        ];
        let baseline = CatalogBaseline {
            catalog_sha256: Some("a".repeat(64)),
            referenced_paths: HashSet::from([
                CATALOG_FILE.to_string(),
                existing_remote.to_string(),
            ]),
            remote_objects: vec![
                StorageObject {
                    path: CATALOG_FILE.to_string(),
                    size: 1,
                    sha256: Some("b".repeat(64)),
                },
                StorageObject {
                    path: existing_remote.to_string(),
                    size: 1,
                    sha256: Some("c".repeat(64)),
                },
                StorageObject {
                    path: orphan_remote.to_string(),
                    size: 1,
                    sha256: Some("d".repeat(64)),
                },
            ],
        };

        let work =
            plan_catalog_sync_with_baseline(desired, baseline, temp.path().to_path_buf()).unwrap();

        assert_eq!(
            work.base_catalog_sha256.as_deref(),
            Some("a".repeat(64).as_str())
        );
        assert_eq!(
            work.prepublish_safe_paths,
            HashSet::from([new_remote.to_string()])
        );
        assert!(!work.prepublish_safe_paths.contains(existing_remote));
        assert!(!work.prepublish_safe_paths.contains(orphan_remote));
        assert!(!work.prepublish_safe_paths.contains(CATALOG_FILE));
    }
}

#[cfg(test)]
mod task_checkpoint_tests {
    use std::collections::HashSet;
    use std::fs;

    use lios_core::catalog_transaction::{
        CatalogBlobCheckpoint, CatalogBlobCheckpointState, CatalogTransactionPhase,
        CatalogTransactionProgress,
    };
    use lios_core::config::LiosPaths;
    use lios_core::storage::CatalogSyncUpload;
    use lios_core::tasks::{CheckpointState, TaskRecord, TaskState, TaskStore};
    use tempfile::tempdir;

    use super::{
        persist_sync_checkpoints, persist_transaction_progress, sha256_hex_file, SyncWork,
        TransferMetrics,
    };

    #[test]
    fn sync_plan_persists_base_target_and_pending_objects_before_remote_work() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let store = TaskStore::open(&paths.database).unwrap();
        let task = TaskRecord::queued("upload", 2);
        store.insert(&task).unwrap();
        let catalog_path = paths.staging.join("catalog.enc");
        let object_path = paths.staging.join("object.lios");
        fs::write(&catalog_path, b"catalog target").unwrap();
        fs::write(&object_path, b"object target").unwrap();
        let catalog_oid = sha256_hex_file(&catalog_path).unwrap();
        let object_oid = sha256_hex_file(&object_path).unwrap();
        let work = SyncWork {
            upload: vec![
                CatalogSyncUpload {
                    path: "catalog.enc".to_string(),
                    local_path: catalog_path,
                    expected_sha256: catalog_oid.clone(),
                    expected_size: Some(14),
                },
                CatalogSyncUpload {
                    path: "objects/files/a/manifest.enc".to_string(),
                    local_path: object_path,
                    expected_sha256: object_oid.clone(),
                    expected_size: Some(13),
                },
            ],
            delete: Vec::new(),
            initial_remote_inventory: Vec::new(),
            prepublish_safe_paths: HashSet::new(),
            base_catalog_sha256: Some("a".repeat(64)),
            expected_revision: None,
            probe_directory: paths.staging.clone(),
        };

        persist_sync_checkpoints(&paths, task.id, &work).unwrap();

        let catalog = store.load_catalog_checkpoint(task.id).unwrap().unwrap();
        assert_eq!(catalog.base_catalog_sha256, Some("a".repeat(64)));
        assert_eq!(catalog.target_catalog_sha256, catalog_oid);
        let checkpoints = store.list_checkpoints(task.id).unwrap();
        assert_eq!(checkpoints.len(), 2);
        assert!(checkpoints.iter().all(|checkpoint| {
            checkpoint.state == CheckpointState::Pending
                && ((checkpoint.remote_path == "catalog.enc" && checkpoint.oid == catalog_oid)
                    || (checkpoint.remote_path == "objects/files/a/manifest.enc"
                        && checkpoint.oid == object_oid))
        }));
    }

    #[test]
    fn transaction_checkpoint_progress_is_persisted_even_when_ui_refresh_is_throttled() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let store = TaskStore::open(&paths.database).unwrap();
        let mut task = TaskRecord::queued("upload", 2);
        task.state = TaskState::Running;
        store.insert(&task).unwrap();
        let mut metrics = TransferMetrics::new();

        persist_transaction_progress(
            &paths,
            None,
            &mut task,
            &mut metrics,
            CatalogTransactionProgress {
                phase: CatalogTransactionPhase::UploadBlobs,
                completed_items: 0,
                total_items: 2,
                bytes_done: 0,
                bytes_total: 10,
                blob_checkpoint: None,
            },
        )
        .unwrap();
        persist_transaction_progress(
            &paths,
            None,
            &mut task,
            &mut metrics,
            CatalogTransactionProgress {
                phase: CatalogTransactionPhase::UploadBlobs,
                completed_items: 1,
                total_items: 2,
                bytes_done: 5,
                bytes_total: 10,
                blob_checkpoint: Some(CatalogBlobCheckpoint {
                    path: "objects/files/a/chunks/b.lios".to_string(),
                    oid: "a".repeat(64),
                    size: 5,
                    state: CatalogBlobCheckpointState::Uploaded,
                }),
            },
        )
        .unwrap();

        let checkpoints = store.list_checkpoints(task.id).unwrap();
        assert_eq!(checkpoints.len(), 1);
        assert_eq!(checkpoints[0].state, CheckpointState::Uploaded);

        persist_transaction_progress(
            &paths,
            None,
            &mut task,
            &mut metrics,
            CatalogTransactionProgress {
                phase: CatalogTransactionPhase::Publish,
                completed_items: 2,
                total_items: 2,
                bytes_done: 5,
                bytes_total: 10,
                blob_checkpoint: Some(CatalogBlobCheckpoint {
                    path: "objects/files/a/chunks/b.lios".to_string(),
                    oid: "a".repeat(64),
                    size: 5,
                    state: CatalogBlobCheckpointState::Committed,
                }),
            },
        )
        .unwrap();

        assert_eq!(
            store.list_checkpoints(task.id).unwrap()[0].state,
            CheckpointState::Committed
        );
    }
}

#[cfg(test)]
mod task_cleanup_tests {
    use std::{fs, sync::Arc, time::Duration};

    use lios_core::config::{LiosPaths, RepoConfig};
    use lios_core::tasks::{
        CheckpointState, TaskObjectCheckpoint, TaskRecord, TaskSpec, TaskState, TaskStore,
    };
    use tempfile::tempdir;
    use tokio::sync::{oneshot, Mutex};

    use super::{
        activate_existing_task, activate_new_task, cleanup_current_staging_cache, cleanup_if_idle,
        clear_task_record, finish_active_worker, finish_committed_worker, group_startup_tasks,
        reconciliation_error_should_wait, recover_startup_tasks, retry_storage_operation,
        set_task_state, startup_reconciliation_terminal_error, task_state_blocks_clear,
        task_state_is_active, CatalogMutationGate, CommandError, CommandErrorCode,
        StartupReconciliationOutcome, StartupTaskRecovery, TaskLifecycleState,
    };

    #[test]
    fn startup_requeues_replayable_specs_and_preserves_commits_for_reconciliation() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = TaskSpec::Delete {
            account_id: "a".repeat(64),
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            },
            node_ids: vec!["node-a".to_string()],
        };
        let store = TaskStore::open(&paths.database).unwrap();
        let replayable = TaskRecord::queued_for_spec(&spec);
        store.insert_with_spec(&replayable, &spec).unwrap();
        store
            .update_state(replayable.id, TaskState::Running, None)
            .unwrap();
        let committing = TaskRecord::queued_for_spec(&spec);
        store.insert_with_spec(&committing, &spec).unwrap();
        store
            .update_state(committing.id, TaskState::Committing, None)
            .unwrap();

        let recovery = recover_startup_tasks(&paths).unwrap();

        assert_eq!(recovery.queued, vec![replayable.id]);
        assert_eq!(recovery.reconcile, vec![committing.id]);
        let store = TaskStore::open(&paths.database).unwrap();
        assert_eq!(
            store.get(replayable.id).unwrap().unwrap().state,
            TaskState::Queued
        );
        let committing = store.get(committing.id).unwrap().unwrap();
        assert_eq!(committing.state, TaskState::Committing);
        assert_eq!(committing.error, None);

        let groups = group_startup_tasks(
            &paths,
            StartupTaskRecovery {
                queued: vec![replayable.id],
                reconcile: vec![committing.id],
            },
        )
        .unwrap();
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].reconcile, vec![committing.id]);
        assert_eq!(groups[0].queued, vec![replayable.id]);
    }

    #[test]
    fn startup_reconciliation_logs_only_terminal_decisions() {
        assert_eq!(
            startup_reconciliation_terminal_error(StartupReconciliationOutcome::Continue),
            Some(None)
        );
        assert_eq!(
            startup_reconciliation_terminal_error(StartupReconciliationOutcome::Stop),
            Some(Some((CommandErrorCode::RemoteConflict, false)))
        );
        assert_eq!(
            startup_reconciliation_terminal_error(StartupReconciliationOutcome::Replay),
            None
        );
    }

    #[test]
    fn every_nonterminal_task_state_is_active() {
        for state in [
            TaskState::Queued,
            TaskState::Preparing,
            TaskState::Running,
            TaskState::Paused,
            TaskState::Retrying,
            TaskState::Committing,
        ] {
            assert!(task_state_is_active(&state), "{state:?} must be active");
        }

        for state in [TaskState::Failed, TaskState::Completed, TaskState::Canceled] {
            assert!(!task_state_is_active(&state), "{state:?} must be terminal");
        }
    }

    #[test]
    fn uncertain_remote_reads_keep_committing_tasks_in_reconciliation() {
        for code in [
            CommandErrorCode::Authentication,
            CommandErrorCode::Network,
            CommandErrorCode::RateLimited,
            CommandErrorCode::RemoteServer,
            CommandErrorCode::Storage,
        ] {
            assert!(reconciliation_error_should_wait(&CommandError::new(
                code,
                "remote catalog is temporarily unavailable",
                false,
                None,
            )));
        }
        for code in [
            CommandErrorCode::RemoteConflict,
            CommandErrorCode::CorruptedData,
            CommandErrorCode::InvalidInput,
        ] {
            assert!(!reconciliation_error_should_wait(&CommandError::new(
                code,
                "reconciliation cannot continue",
                false,
                None,
            )));
        }
    }

    #[tokio::test]
    async fn reconciliation_retries_local_storage_failures_before_releasing_the_space() {
        let mut attempts = 0;

        let value = retry_storage_operation(|| {
            attempts += 1;
            if attempts < 3 {
                Err(CommandError::new(
                    CommandErrorCode::Storage,
                    "database is temporarily unavailable",
                    false,
                    None,
                ))
            } else {
                Ok("persisted")
            }
        })
        .await
        .unwrap();

        assert_eq!(attempts, 3);
        assert_eq!(value, "persisted");
    }

    #[tokio::test]
    async fn clear_rejects_every_persisted_nonterminal_state() {
        for state in [
            TaskState::Queued,
            TaskState::Preparing,
            TaskState::Running,
            TaskState::Paused,
            TaskState::Retrying,
            TaskState::Committing,
        ] {
            assert!(task_state_blocks_clear(&state));
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let task = TaskRecord::queued("persisted active", 0);
            let store = TaskStore::open(&paths.database).unwrap();
            store.insert(&task).unwrap();
            store.update_state(task.id, state.clone(), None).unwrap();
            let lifecycle = Mutex::new(TaskLifecycleState::default());

            let error = clear_task_record(&paths, &lifecycle, task.id)
                .await
                .unwrap_err();

            assert_eq!(error.code, CommandErrorCode::InvalidInput, "{state:?}");
            assert!(TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .iter()
                .any(|record| record.id == task.id));
        }
    }

    #[tokio::test]
    async fn active_task_staging_survives_another_task_finishing() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let staged = paths.staging.join("other-task.download");
        fs::write(&staged, b"still active").unwrap();
        let store = TaskStore::open(&paths.database).unwrap();
        let active = TaskRecord::queued("active", 1);
        store.insert(&active).unwrap();
        store
            .update_state(active.id, TaskState::Running, None)
            .unwrap();
        let completed = TaskRecord::queued("completed", 1);
        store.insert(&completed).unwrap();
        store
            .update_state(completed.id, TaskState::Completed, None)
            .unwrap();

        let gate = Mutex::new(TaskLifecycleState::default());
        let skipped = cleanup_if_idle(&paths, &gate, || {
            cleanup_current_staging_cache(&paths, true, false)
        })
        .await
        .unwrap();

        assert!(skipped.is_none());
        assert!(staged.exists());
    }

    #[tokio::test]
    async fn task_end_cleanup_runs_after_all_tasks_are_inactive() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let staged = paths.staging.join("finished.download");
        fs::write(&staged, b"finished").unwrap();
        let store = TaskStore::open(&paths.database).unwrap();
        let completed = TaskRecord::queued("completed", 1);
        store.insert(&completed).unwrap();
        store
            .update_state(completed.id, TaskState::Completed, None)
            .unwrap();

        let gate = Mutex::new(TaskLifecycleState::default());
        let cleaned = cleanup_if_idle(&paths, &gate, || {
            cleanup_current_staging_cache(&paths, true, false)
        })
        .await
        .unwrap();

        assert!(cleaned.is_some());
        assert!(!staged.exists());
    }

    #[tokio::test]
    async fn completed_worker_preserves_warning_text() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let lifecycle = Mutex::new(TaskLifecycleState::default());
        let catalog_mutation_gate = CatalogMutationGate::default();
        let task = activate_new_task(&paths, &lifecycle, TaskRecord::queued("verify_quick", 1))
            .await
            .unwrap();
        let warning = "1 个旧版分片缺少长度元数据";

        let final_state = finish_active_worker(
            &paths,
            &lifecycle,
            &catalog_mutation_gate,
            task.id,
            TaskState::Completed,
            Some(warning.to_string()),
        )
        .await
        .unwrap();

        let completed = TaskStore::open(&paths.database)
            .unwrap()
            .get(task.id)
            .unwrap()
            .unwrap();
        assert_eq!(final_state, TaskState::Completed);
        assert_eq!(completed.state, TaskState::Completed);
        assert_eq!(completed.error.as_deref(), Some(warning));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn activation_cannot_enter_between_cleanup_check_and_deletion() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let stale_staged = paths.staging.join("stale.download");
        let active_staged = paths.staging.join("active.download");
        fs::write(&stale_staged, b"stale").unwrap();

        let gate = Arc::new(Mutex::new(TaskLifecycleState::default()));
        let cleanup_paths = paths.clone();
        let deletion_paths = cleanup_paths.clone();
        let cleanup_gate = Arc::clone(&gate);
        let (checked_tx, checked_rx) = oneshot::channel();
        let (continue_tx, continue_rx) = std::sync::mpsc::channel();
        let cleanup = tokio::spawn(async move {
            cleanup_if_idle(&cleanup_paths, cleanup_gate.as_ref(), move || {
                checked_tx.send(()).unwrap();
                continue_rx.recv_timeout(Duration::from_secs(5)).unwrap();
                cleanup_current_staging_cache(&deletion_paths, true, false)
            })
            .await
        });
        checked_rx.await.unwrap();

        let activation_paths = paths.clone();
        let activation_gate = Arc::clone(&gate);
        let activation_staged = active_staged.clone();
        let (attempted_tx, attempted_rx) = oneshot::channel();
        let activation = tokio::spawn(async move {
            attempted_tx.send(()).unwrap();
            let task = activate_new_task(
                &activation_paths,
                activation_gate.as_ref(),
                TaskRecord::queued("active", 1),
            )
            .await
            .unwrap();
            fs::write(&activation_staged, b"active").unwrap();
            task
        });
        attempted_rx.await.unwrap();
        tokio::task::yield_now().await;

        assert!(!activation.is_finished());
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .list()
            .unwrap()
            .is_empty());

        continue_tx.send(()).unwrap();
        assert!(cleanup.await.unwrap().unwrap().is_some());
        let activated = activation.await.unwrap();
        assert_eq!(activated.state, TaskState::Running);
        assert!(!stale_staged.exists());
        assert!(active_staged.exists());

        let skipped = cleanup_if_idle(&paths, gate.as_ref(), || {
            cleanup_current_staging_cache(&paths, true, false)
        })
        .await
        .unwrap();
        assert!(skipped.is_none());
        assert!(active_staged.exists());
    }

    #[tokio::test]
    async fn existing_task_reactivation_waits_for_lifecycle_gate() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let store = TaskStore::open(&paths.database).unwrap();
        let paused = TaskRecord::queued("paused", 1);
        store.insert(&paused).unwrap();
        store
            .update_state(paused.id, TaskState::Paused, None)
            .unwrap();

        let gate = Arc::new(Mutex::new(TaskLifecycleState::default()));
        let guard = gate.lock().await;
        let activation_paths = paths.clone();
        let activation_gate = Arc::clone(&gate);
        let task_id = paused.id;
        let activation = tokio::spawn(async move {
            activate_existing_task(
                &activation_paths,
                activation_gate.as_ref(),
                task_id,
                TaskState::Queued,
            )
            .await
        });
        tokio::task::yield_now().await;

        let state_while_locked = store
            .list()
            .unwrap()
            .into_iter()
            .find(|task| task.id == paused.id)
            .unwrap()
            .state;
        assert_eq!(state_while_locked, TaskState::Paused);

        drop(guard);
        activation.await.unwrap().unwrap();
        let state_after_release = store
            .list()
            .unwrap()
            .into_iter()
            .find(|task| task.id == paused.id)
            .unwrap()
            .state;
        assert_eq!(state_after_release, TaskState::Queued);
    }

    #[tokio::test]
    async fn canceled_worker_blocks_cleanup_until_exit_is_acknowledged() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let staged = paths.staging.join("blocked-transfer.download");
        fs::write(&staged, b"worker still owns this").unwrap();
        let lifecycle = Mutex::new(TaskLifecycleState::default());
        let catalog_mutation_gate = CatalogMutationGate::default();

        let task = activate_new_task(&paths, &lifecycle, TaskRecord::queued("blocked delete", 1))
            .await
            .unwrap();
        set_task_state(&paths, &lifecycle, task.id, TaskState::Canceled, None)
            .await
            .unwrap();

        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .find(|record| record.id == task.id)
                .unwrap()
                .state,
            TaskState::Canceled
        );
        assert!(lifecycle.lock().await.active_workers.contains(&task.id));

        let skipped = cleanup_if_idle(&paths, &lifecycle, || {
            cleanup_current_staging_cache(&paths, true, false)
        })
        .await
        .unwrap();
        assert!(skipped.is_none());
        assert!(staged.exists());

        let final_state = finish_active_worker(
            &paths,
            &lifecycle,
            &catalog_mutation_gate,
            task.id,
            TaskState::Completed,
            None,
        )
        .await
        .unwrap();

        assert_eq!(final_state, TaskState::Canceled);
        assert!(!lifecycle.lock().await.active_workers.contains(&task.id));
        assert!(!staged.exists());
        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .find(|record| record.id == task.id)
                .unwrap()
                .state,
            TaskState::Canceled
        );
    }

    #[tokio::test]
    async fn clear_rejects_registered_canceled_and_paused_workers_until_exit() {
        for controlled_state in [TaskState::Canceled, TaskState::Paused] {
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let lifecycle = Mutex::new(TaskLifecycleState::default());
            let catalog_mutation_gate = CatalogMutationGate::default();
            let task = activate_new_task(
                &paths,
                &lifecycle,
                TaskRecord::queued("controlled worker", 1),
            )
            .await
            .unwrap();
            set_task_state(&paths, &lifecycle, task.id, controlled_state.clone(), None)
                .await
                .unwrap();

            let error = clear_task_record(&paths, &lifecycle, task.id)
                .await
                .unwrap_err();
            assert_eq!(error.code, CommandErrorCode::InvalidInput);
            assert!(TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .iter()
                .any(|record| record.id == task.id));

            let final_state = finish_active_worker(
                &paths,
                &lifecycle,
                &catalog_mutation_gate,
                task.id,
                TaskState::Completed,
                None,
            )
            .await
            .unwrap();
            assert_eq!(final_state, controlled_state);

            if controlled_state == TaskState::Paused {
                let error = clear_task_record(&paths, &lifecycle, task.id)
                    .await
                    .unwrap_err();
                assert_eq!(error.code, CommandErrorCode::InvalidInput);
                set_task_state(&paths, &lifecycle, task.id, TaskState::Canceled, None)
                    .await
                    .unwrap();
            }
            clear_task_record(&paths, &lifecycle, task.id)
                .await
                .unwrap();
            assert!(TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .iter()
                .all(|record| record.id != task.id));
        }
    }

    #[tokio::test]
    async fn committed_worker_ignores_a_late_cancel_state() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let lifecycle = Mutex::new(TaskLifecycleState::default());
        let catalog_mutation_gate = CatalogMutationGate::default();
        let task = activate_new_task(
            &paths,
            &lifecycle,
            TaskRecord::queued("committed upload", 1),
        )
        .await
        .unwrap();
        TaskStore::open(&paths.database)
            .unwrap()
            .upsert_checkpoint(&TaskObjectCheckpoint {
                task_id: task.id,
                remote_path: "catalog.enc".to_string(),
                oid: "a".repeat(64),
                size: 10,
                state: CheckpointState::Pending,
            })
            .unwrap();
        set_task_state(&paths, &lifecycle, task.id, TaskState::Canceled, None)
            .await
            .unwrap();

        let final_state =
            finish_committed_worker(&paths, &lifecycle, &catalog_mutation_gate, task.id)
                .await
                .unwrap();

        assert_eq!(final_state, TaskState::Completed);
        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .into_iter()
                .find(|record| record.id == task.id)
                .unwrap()
                .state,
            TaskState::Completed
        );
        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .list_checkpoints(task.id)
                .unwrap()[0]
                .state,
            CheckpointState::Committed
        );
    }

    #[tokio::test]
    async fn finalization_failure_still_deregisters_the_active_worker() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let lifecycle = Mutex::new(TaskLifecycleState::default());
        let catalog_mutation_gate = CatalogMutationGate::default();
        let task = activate_new_task(
            &paths,
            &lifecycle,
            TaskRecord::queued("committed upload", 1),
        )
        .await
        .unwrap();
        fs::remove_file(&paths.database).unwrap();
        fs::create_dir(&paths.database).unwrap();

        let result =
            finish_committed_worker(&paths, &lifecycle, &catalog_mutation_gate, task.id).await;

        assert!(result.is_err());
        assert!(!lifecycle.lock().await.active_workers.contains(&task.id));
    }
}

#[cfg(test)]
mod remote_verification_tests {
    use std::fs;
    #[cfg(windows)]
    use std::process::Command;

    use lios_core::catalog::CatalogRemoteFile;
    use lios_core::config::{LiosPaths, RepoConfig};
    use lios_core::storage::RepoRevision;
    use lios_core::tasks::{TaskRecord, TaskSpec, TaskState, TaskStore};
    use lios_core::LiosError;
    use tempfile::tempdir;
    use tokio::sync::Mutex;
    use tokio_util::sync::CancellationToken;

    #[cfg(windows)]
    use super::cleanup_terminal_task_staging_and_record;
    use super::{
        append_task_warning, cleanup_terminal_task_staging,
        cleanup_terminal_task_staging_after_restart_async, clear_task_record,
        ensure_verification_revision_unchanged, head_revision_with_cancellation,
        map_remote_integrity_error, validate_local_remote_file, verification_commit_id,
        CommandErrorCode, LocalRemoteFileValidation, TaskLifecycleState,
    };

    fn verification_spec() -> TaskSpec {
        TaskSpec::VerifySpace {
            account_id: "a".repeat(64),
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            },
            full: true,
        }
    }

    fn rebuild_spec() -> TaskSpec {
        TaskSpec::RebuildCatalog {
            account_id: "a".repeat(64),
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            },
            expected_revision: Some("preview-revision".to_string()),
        }
    }

    #[test]
    fn changed_verification_revision_is_a_remote_conflict() {
        let started = RepoRevision {
            branch: "master".to_string(),
            commit_id: Some("commit-a".to_string()),
        };
        let finished = RepoRevision {
            branch: "master".to_string(),
            commit_id: Some("commit-b".to_string()),
        };

        let error = ensure_verification_revision_unchanged(&started, &finished).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::RemoteConflict);
    }

    #[test]
    fn verification_requires_an_immutable_commit_id() {
        let revision = RepoRevision {
            branch: "master".to_string(),
            commit_id: None,
        };

        let error = verification_commit_id(&revision).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::Storage);
    }

    #[tokio::test]
    async fn revision_lookup_observes_preexisting_cancellation() {
        let adapter =
            lios_core::modelscope::ModelScopeAdapter::new("http://127.0.0.1:9", "unused-token");
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let revision = head_revision_with_cancellation(&adapter, &repo, &cancellation)
            .await
            .unwrap();

        assert_eq!(revision, None);
    }

    #[test]
    fn terminal_verification_discards_staging_but_paused_verification_keeps_it() {
        for (state, should_exist) in [
            (TaskState::Completed, false),
            (TaskState::Failed, false),
            (TaskState::Canceled, false),
            (TaskState::Paused, true),
            (TaskState::Retrying, true),
        ] {
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let spec = verification_spec();
            let task = TaskRecord::queued_for_spec(&spec);
            let store = TaskStore::open(&paths.database).unwrap();
            store.insert_with_spec(&task, &spec).unwrap();
            store.update_state(task.id, state, None).unwrap();
            let task_paths = paths
                .for_task(spec.account_id(), spec.space_id(), task.id)
                .unwrap();
            task_paths.ensure_dirs().unwrap();
            fs::write(task_paths.staging.join("cached.lios"), b"cached").unwrap();

            cleanup_terminal_task_staging(&task_paths, &spec, task.id).unwrap();

            assert_eq!(task_paths.staging.exists(), should_exist);
        }
    }

    #[test]
    fn terminal_rebuild_discards_staging_but_paused_rebuild_keeps_it() {
        for (state, should_exist) in [
            (TaskState::Completed, false),
            (TaskState::Failed, false),
            (TaskState::Canceled, false),
            (TaskState::Paused, true),
            (TaskState::Retrying, true),
        ] {
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let spec = rebuild_spec();
            let task = TaskRecord::queued_for_spec(&spec);
            let store = TaskStore::open(&paths.database).unwrap();
            store.insert_with_spec(&task, &spec).unwrap();
            store.update_state(task.id, state, None).unwrap();
            let task_paths = paths
                .for_task(spec.account_id(), spec.space_id(), task.id)
                .unwrap();
            task_paths.ensure_dirs().unwrap();
            fs::write(task_paths.staging.join("catalog.enc"), b"rebuilt").unwrap();

            cleanup_terminal_task_staging(&task_paths, &spec, task.id).unwrap();

            assert_eq!(task_paths.staging.exists(), should_exist);
        }
    }

    #[cfg(windows)]
    #[test]
    fn verification_cleanup_rejects_a_junction_target_inside_lios_home() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = verification_spec();
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Completed, None)
            .unwrap();
        let task_paths = paths
            .for_task(spec.account_id(), spec.space_id(), task.id)
            .unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::remove_dir_all(&task_paths.staging).unwrap();
        let protected = paths.home.join("protected");
        fs::create_dir_all(&protected).unwrap();
        let sentinel = protected.join("keep.txt");
        fs::write(&sentinel, b"keep").unwrap();
        let output = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(&task_paths.staging)
            .arg(&protected)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(cleanup_terminal_task_staging(&task_paths, &spec, task.id).is_err());
        assert!(sentinel.exists());
        cleanup_terminal_task_staging_and_record(&task_paths, &spec, task.id).unwrap();
        let stored = TaskStore::open(&paths.database)
            .unwrap()
            .get(task.id)
            .unwrap()
            .unwrap();
        assert!(stored
            .error
            .as_deref()
            .is_some_and(|error| error.contains("verification staging cleanup failed")));
    }

    #[tokio::test]
    async fn deferred_startup_sweep_removes_terminal_verification_staging() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = verification_spec();
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Completed, None)
            .unwrap();
        let task_paths = paths
            .for_task(spec.account_id(), spec.space_id(), task.id)
            .unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("cached.lios"), b"cached").unwrap();

        cleanup_terminal_task_staging_after_restart_async(paths.clone())
            .await
            .unwrap();

        assert!(!task_paths.staging.exists());
    }

    #[tokio::test]
    async fn deferred_startup_sweep_removes_terminal_rebuild_staging() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = rebuild_spec();
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Completed, None)
            .unwrap();
        let task_paths = paths
            .for_task(spec.account_id(), spec.space_id(), task.id)
            .unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("catalog.enc"), b"rebuilt").unwrap();

        cleanup_terminal_task_staging_after_restart_async(paths.clone())
            .await
            .unwrap();

        assert!(!task_paths.staging.exists());
    }

    #[tokio::test]
    async fn cached_file_hashing_observes_cancellation() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("cached.lios");
        fs::write(&path, vec![7u8; 2 * 1024 * 1024]).unwrap();
        let file = CatalogRemoteFile {
            path: "objects/files/a/chunks/b.lios".to_string(),
            expected_size: Some(2 * 1024 * 1024),
            sha256: Some("unused-after-cancel".to_string()),
        };
        let cancellation = CancellationToken::new();
        cancellation.cancel();

        let outcome = validate_local_remote_file(&path, &file, &cancellation)
            .await
            .unwrap();

        assert_eq!(outcome, LocalRemoteFileValidation::Canceled);
    }

    #[test]
    fn cleanup_warning_is_persisted_for_a_canceled_task() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = verification_spec();
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Canceled, None)
            .unwrap();

        append_task_warning(&paths, task.id, "cleanup failed").unwrap();

        assert_eq!(
            TaskStore::open(&paths.database)
                .unwrap()
                .get(task.id)
                .unwrap()
                .unwrap()
                .error
                .as_deref(),
            Some("cleanup failed")
        );
    }

    #[tokio::test]
    async fn clearing_terminal_verification_removes_staging_before_the_record() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = verification_spec();
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Completed, None)
            .unwrap();
        let task_paths = paths
            .for_task(spec.account_id(), spec.space_id(), task.id)
            .unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("cached.lios"), b"cached").unwrap();

        clear_task_record(&paths, &Mutex::new(TaskLifecycleState::default()), task.id)
            .await
            .unwrap();

        assert!(!task_paths.staging.exists());
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .get(task.id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn clearing_terminal_rebuild_removes_staging_before_the_record() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = rebuild_spec();
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Completed, None)
            .unwrap();
        let task_paths = paths
            .for_task(spec.account_id(), spec.space_id(), task.id)
            .unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("catalog.enc"), b"rebuilt").unwrap();

        clear_task_record(&paths, &Mutex::new(TaskLifecycleState::default()), task.id)
            .await
            .unwrap();

        assert!(!task_paths.staging.exists());
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .get(task.id)
            .unwrap()
            .is_none());
    }

    #[test]
    fn remote_integrity_error_preserves_the_affected_object() {
        let reason = "remote object LFS OID mismatch: objects/files/a/chunks/b.lios".to_string();

        let error = map_remote_integrity_error(LiosError::DataCorruption(reason.clone()));

        assert_eq!(error.code, CommandErrorCode::CorruptedData);
        assert!(error.message.contains(&reason));
        assert_eq!(
            error
                .details
                .as_ref()
                .and_then(|details| details["reason"].as_str()),
            Some(reason.as_str())
        );
    }
}

#[cfg(test)]
mod catalog_rebuild_command_tests {
    use std::fs;

    use lios_core::catalog::{Catalog, CatalogSelection, CATALOG_FILE};
    use lios_core::config::LiosPaths;
    use lios_core::crypto::KeyFile;
    use lios_core::storage::{RepoRevision, StorageObject};
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::{
        discard_staged_catalog_for_rebuild, plan_catalog_rebuild_sync, recovery_metadata_objects,
        validate_rebuild_revision, CommandErrorCode,
    };

    fn staged_object(paths: &LiosPaths, remote_path: String) -> StorageObject {
        let bytes = fs::read(paths.staging.join(&remote_path)).unwrap();
        StorageObject {
            path: remote_path,
            size: bytes.len() as u64,
            sha256: Some(hex::encode(Sha256::digest(bytes))),
        }
    }

    #[test]
    fn rebuild_downloads_only_descriptors_and_manifests() {
        let objects = vec![
            StorageObject {
                path: "recovery/nodes/a.enc".to_string(),
                size: 1,
                sha256: Some("a".repeat(64)),
            },
            StorageObject {
                path: "objects/files/b/manifest.enc".to_string(),
                size: 2,
                sha256: Some("b".repeat(64)),
            },
            StorageObject {
                path: "objects/files/b/chunks/c.lios".to_string(),
                size: 3,
                sha256: Some("c".repeat(64)),
            },
            StorageObject {
                path: "notes.txt".to_string(),
                size: 4,
                sha256: None,
            },
        ];

        let selected = recovery_metadata_objects(&objects).unwrap();

        assert_eq!(
            selected
                .into_iter()
                .map(|object| object.path)
                .collect::<Vec<_>>(),
            vec![
                "objects/files/b/manifest.enc".to_string(),
                "recovery/nodes/a.enc".to_string(),
            ]
        );
    }

    #[test]
    fn rebuild_confirmation_requires_the_preview_revision() {
        let current = RepoRevision {
            branch: "master".to_string(),
            commit_id: Some("commit-123".to_string()),
        };
        assert_eq!(
            validate_rebuild_revision(Some("commit-123"), &current).unwrap(),
            "commit-123"
        );
        assert_eq!(
            validate_rebuild_revision(None, &current).unwrap_err().code,
            CommandErrorCode::InvalidInput
        );
        assert_eq!(
            validate_rebuild_revision(Some("commit-old"), &current)
                .unwrap_err()
                .code,
            CommandErrorCode::RemoteConflict
        );
    }

    #[test]
    fn rebuild_publish_plan_never_deletes_unreferenced_remote_objects() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let key = KeyFile::generate_to_path(temp.path().join("key.yaml")).unwrap();
        let catalog = Catalog::initialize_empty("Recovered", &key, paths.staging.clone()).unwrap();
        let mut remote = catalog
            .remote_files_for_selection(&CatalogSelection::All, &key)
            .unwrap()
            .into_iter()
            .map(|file| staged_object(&paths, file.path))
            .collect::<Vec<_>>();
        remote.push(StorageObject {
            path: format!(
                "objects/files/{}/chunks/{}.lios",
                "a".repeat(32),
                "b".repeat(64)
            ),
            size: 5,
            sha256: Some("c".repeat(64)),
        });

        let expected_revision = RepoRevision {
            branch: "master".to_string(),
            commit_id: Some("preview-revision".to_string()),
        };
        let work =
            plan_catalog_rebuild_sync(&paths, &catalog, &key, remote, expected_revision.clone())
                .unwrap();

        assert!(work.delete.is_empty());
        assert_eq!(work.expected_revision, Some(expected_revision));
        assert_eq!(
            work.upload
                .iter()
                .map(|upload| upload.path.as_str())
                .collect::<Vec<_>>(),
            vec![CATALOG_FILE]
        );
    }

    #[test]
    fn rebuild_retry_discards_only_the_prior_generated_catalog() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let catalog_path = paths.staging.join(CATALOG_FILE);
        let descriptor_path = paths.staging.join("recovery/nodes/a.enc");
        fs::create_dir_all(descriptor_path.parent().unwrap()).unwrap();
        fs::write(&catalog_path, b"previous rebuild output").unwrap();
        fs::write(&descriptor_path, b"recovery metadata").unwrap();

        discard_staged_catalog_for_rebuild(&paths).unwrap();

        assert!(!catalog_path.exists());
        assert_eq!(fs::read(descriptor_path).unwrap(), b"recovery metadata");
    }
}

#[cfg(test)]
mod recovery_key_service_tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use lios_core::catalog::{Catalog, CATALOG_FILE};
    use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
    use lios_core::crypto::KeyFile;
    use lios_core::storage::{StorageAdapter, StorageObject};
    use lios_core::{LiosError, RemoteError, RemoteErrorKind};
    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::command_error::CommandErrorCode;
    use super::config_mutation_gate::ConfigMutationGate;
    use super::recovery_key_service::{
        export_recovery_key_for_paths, import_candidate_with_adapter,
        import_candidate_with_adapter_after_verification, recovery_key_status,
        verify_candidate_with_adapter,
    };

    enum DownloadResult {
        Catalog(PathBuf),
        NotFound,
    }

    struct FakeCatalogAdapter {
        result: DownloadResult,
        calls: Arc<AtomicUsize>,
        staging_dirs: Arc<Mutex<Vec<PathBuf>>>,
        on_download: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    }

    impl FakeCatalogAdapter {
        fn catalog(path: PathBuf) -> Self {
            Self {
                result: DownloadResult::Catalog(path),
                calls: Arc::new(AtomicUsize::new(0)),
                staging_dirs: Arc::new(Mutex::new(Vec::new())),
                on_download: Mutex::new(None),
            }
        }

        fn catalog_with_action(path: PathBuf, action: impl FnOnce() + Send + 'static) -> Self {
            Self {
                result: DownloadResult::Catalog(path),
                calls: Arc::new(AtomicUsize::new(0)),
                staging_dirs: Arc::new(Mutex::new(Vec::new())),
                on_download: Mutex::new(Some(Box::new(action))),
            }
        }

        fn not_found() -> Self {
            Self {
                result: DownloadResult::NotFound,
                calls: Arc::new(AtomicUsize::new(0)),
                staging_dirs: Arc::new(Mutex::new(Vec::new())),
                on_download: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl StorageAdapter for FakeCatalogAdapter {
        async fn create_repo(&self, _namespace: &str, _dataset: &str) -> lios_core::Result<()> {
            Ok(())
        }

        async fn repo_exists(&self, _namespace: &str, _dataset: &str) -> lios_core::Result<bool> {
            Ok(true)
        }

        async fn list_objects(
            &self,
            _namespace: &str,
            _dataset: &str,
            _prefix: &str,
        ) -> lios_core::Result<Vec<StorageObject>> {
            Ok(Vec::new())
        }

        async fn download_object(
            &self,
            _namespace: &str,
            _dataset: &str,
            remote_path: &str,
            local_path: &Path,
        ) -> lios_core::Result<()> {
            assert_eq!(remote_path, CATALOG_FILE);
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.staging_dirs
                .lock()
                .unwrap()
                .push(local_path.parent().unwrap().to_path_buf());
            if let Some(action) = self.on_download.lock().unwrap().take() {
                action();
            }
            match &self.result {
                DownloadResult::Catalog(source) => {
                    fs::copy(source, local_path)?;
                    Ok(())
                }
                DownloadResult::NotFound => Err(LiosError::Remote(RemoteError::new(
                    RemoteErrorKind::NotFound,
                    Some(404),
                ))),
            }
        }
    }

    fn repo() -> RepoConfig {
        RepoConfig {
            namespace: "novix".to_string(),
            dataset: "archive".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        }
    }

    fn configured_paths() -> (tempfile::TempDir, LiosPaths, PathBuf) {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let active_key = paths.home.join("recovery.key");
        KeyFile::generate_to_path(&active_key).unwrap();
        LiosConfig {
            key_file_path: Some(active_key.clone()),
            ..LiosConfig::default()
        }
        .save(&paths.config)
        .unwrap();
        (temp, paths, active_key)
    }

    #[test]
    fn export_refuses_clobber_and_status_rejects_stale_or_deleted_backup() {
        let (_temp, paths, _active_key) = configured_paths();
        let existing = paths.home.parent().unwrap().join("existing.key");
        fs::write(&existing, b"leave me alone").unwrap();
        let original_config = fs::read(&paths.config).unwrap();
        let config_gate = ConfigMutationGate::default();

        let error = export_recovery_key_for_paths(&paths, &config_gate, &existing).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::Storage);
        assert_eq!(fs::read(&existing).unwrap(), b"leave me alone");
        assert_eq!(fs::read(&paths.config).unwrap(), original_config);

        let destination = paths.home.parent().unwrap().join("backup.key");
        let status = export_recovery_key_for_paths(&paths, &config_gate, &destination).unwrap();
        assert!(status.backed_up);
        assert_eq!(status.backup_location.as_deref(), destination.to_str());
        let saved = LiosConfig::load(&paths.config).unwrap();
        assert_eq!(saved.backup_path.as_deref(), Some(destination.as_path()));

        fs::remove_file(&destination).unwrap();
        KeyFile::generate_to_path(&destination).unwrap();
        assert!(!recovery_key_status(&saved).backed_up);
        fs::remove_file(&destination).unwrap();
        assert!(!recovery_key_status(&saved).backed_up);
    }

    #[tokio::test]
    async fn matching_key_verifies_catalog_in_isolated_temporary_staging() {
        let remote = tempdir().unwrap();
        let candidate_path = remote.path().join("candidate.key");
        let candidate = KeyFile::generate_to_path(&candidate_path).unwrap();
        let remote_staging = remote.path().join("remote");
        Catalog::initialize_empty("Archive", &candidate, &remote_staging).unwrap();
        let adapter = FakeCatalogAdapter::catalog(remote_staging.join(CATALOG_FILE));
        let target = repo();

        let verification =
            verify_candidate_with_adapter(&candidate_path, Some(&target), Some(&adapter))
                .await
                .unwrap();

        assert!(verification.format_valid);
        assert!(verification.catalog_checked);
        assert_eq!(verification.checked_space, Some(target));
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        for staging in adapter.staging_dirs.lock().unwrap().iter() {
            assert!(!staging.exists(), "temporary staging was not removed");
        }
    }

    #[tokio::test]
    async fn wrong_key_catalog_decryption_is_classified_as_wrong_key() {
        let (_temp, paths, active_key_path) = configured_paths();
        let active_key = KeyFile::load_from_path(&active_key_path).unwrap();
        let remote = tempdir().unwrap();
        let remote_staging = remote.path().join("remote");
        Catalog::initialize_empty("Archive", &active_key, &remote_staging).unwrap();
        let adapter = FakeCatalogAdapter::catalog(remote_staging.join(CATALOG_FILE));
        let candidate_path = paths.home.parent().unwrap().join("external.key");
        KeyFile::generate_to_path(&candidate_path).unwrap();
        let target = repo();
        let original_config = fs::read(&paths.config).unwrap();
        let config_gate = ConfigMutationGate::default();

        let error = import_candidate_with_adapter(
            &paths,
            &config_gate,
            &candidate_path,
            Some(&target),
            Some(&adapter),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::WrongKey);
        assert_eq!(fs::read(&paths.config).unwrap(), original_config);
        assert_eq!(
            LiosConfig::load(&paths.config)
                .unwrap()
                .key_file_path
                .as_deref(),
            Some(active_key_path.as_path())
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert!(!paths.home.join("external.key").exists());
    }

    #[tokio::test]
    async fn import_rejects_candidate_replaced_during_catalog_download() {
        let (_temp, paths, active_key_path) = configured_paths();
        let active_key = KeyFile::load_from_path(&active_key_path).unwrap();
        let target = repo();
        let mut config = LiosConfig::load(&paths.config).unwrap();
        config.spaces.insert("archive".to_string(), target.clone());
        config.save(&paths.config).unwrap();
        let original_config = fs::read(&paths.config).unwrap();
        let remote = tempdir().unwrap();
        let remote_staging = remote.path().join("remote");
        Catalog::initialize_empty("Archive", &active_key, &remote_staging).unwrap();
        let candidate_path = paths.home.parent().unwrap().join("external.key");
        active_key.save_to_path(&candidate_path).unwrap();
        let candidate_to_replace = candidate_path.clone();
        let adapter =
            FakeCatalogAdapter::catalog_with_action(remote_staging.join(CATALOG_FILE), move || {
                fs::remove_file(&candidate_to_replace).unwrap();
                KeyFile::generate_to_path(&candidate_to_replace).unwrap();
            });
        let config_gate = ConfigMutationGate::default();

        let error = import_candidate_with_adapter(
            &paths,
            &config_gate,
            &candidate_path,
            Some(&target),
            Some(&adapter),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::WrongKey);
        assert_eq!(fs::read(&paths.config).unwrap(), original_config);
        assert_eq!(
            LiosConfig::load(&paths.config)
                .unwrap()
                .key_file_path
                .as_deref(),
            Some(active_key_path.as_path())
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
        assert!(!paths.home.join("external.key").exists());
    }

    #[tokio::test]
    async fn import_rejects_repo_changed_during_download_without_clobbering_concurrent_config() {
        let (_temp, paths, active_key_path) = configured_paths();
        let active_key = KeyFile::load_from_path(&active_key_path).unwrap();
        let target = repo();
        let mut config = LiosConfig::load(&paths.config).unwrap();
        config.spaces.insert("archive".to_string(), target.clone());
        config.save(&paths.config).unwrap();
        let remote = tempdir().unwrap();
        let remote_staging = remote.path().join("remote");
        Catalog::initialize_empty("Archive", &active_key, &remote_staging).unwrap();
        let candidate_path = paths.home.parent().unwrap().join("external.key");
        active_key.save_to_path(&candidate_path).unwrap();
        let concurrent_key_path = paths.home.parent().unwrap().join("concurrent.key");
        KeyFile::generate_to_path(&concurrent_key_path).unwrap();
        let concurrent_repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "other-space".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let config_path = paths.config.clone();
        let concurrent_key_for_action = concurrent_key_path.clone();
        let concurrent_repo_for_action = concurrent_repo.clone();
        let concurrent_bytes = Arc::new(Mutex::new(None));
        let concurrent_bytes_for_action = Arc::clone(&concurrent_bytes);
        let adapter =
            FakeCatalogAdapter::catalog_with_action(remote_staging.join(CATALOG_FILE), move || {
                let mut config = LiosConfig::load(&config_path).unwrap();
                config.spaces.clear();
                config
                    .spaces
                    .insert("other".to_string(), concurrent_repo_for_action);
                config.key_file_path = Some(concurrent_key_for_action);
                config.save(&config_path).unwrap();
                *concurrent_bytes_for_action.lock().unwrap() =
                    Some(fs::read(&config_path).unwrap());
            });
        let config_gate = ConfigMutationGate::default();

        let error = import_candidate_with_adapter(
            &paths,
            &config_gate,
            &candidate_path,
            Some(&target),
            Some(&adapter),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::RemoteConflict);
        assert_eq!(
            fs::read(&paths.config).unwrap(),
            concurrent_bytes.lock().unwrap().clone().unwrap()
        );
        let saved = LiosConfig::load(&paths.config).unwrap();
        assert_eq!(saved.spaces.get("other"), Some(&concurrent_repo));
        assert_eq!(
            saved.key_file_path.as_deref(),
            Some(concurrent_key_path.as_path())
        );
        assert_eq!(adapter.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_config_gate_prevents_stale_import_write_after_remote_verification() {
        let (_temp, paths, active_key_path) = configured_paths();
        let active_key = KeyFile::load_from_path(&active_key_path).unwrap();
        let target = repo();
        let mut config = LiosConfig::load(&paths.config).unwrap();
        config.spaces.insert("archive".to_string(), target.clone());
        config.save(&paths.config).unwrap();
        let remote = tempdir().unwrap();
        let remote_staging = remote.path().join("remote");
        Catalog::initialize_empty("Archive", &active_key, &remote_staging).unwrap();
        let candidate_path = paths.home.parent().unwrap().join("external.key");
        active_key.save_to_path(&candidate_path).unwrap();
        let adapter = Arc::new(FakeCatalogAdapter::catalog(
            remote_staging.join(CATALOG_FILE),
        ));
        let gate = Arc::new(ConfigMutationGate::default());
        let concurrent_key_path = paths.home.parent().unwrap().join("concurrent.key");
        KeyFile::generate_to_path(&concurrent_key_path).unwrap();
        let concurrent_repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "gate-winner".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };

        let (gate_held_tx, gate_held_rx) = std::sync::mpsc::channel();
        let (write_tx, write_rx) = std::sync::mpsc::channel();
        let (written_tx, written_rx) = oneshot::channel();
        let writer_gate = Arc::clone(&gate);
        let writer_config_path = paths.config.clone();
        let writer_key_path = concurrent_key_path.clone();
        let writer_repo = concurrent_repo.clone();
        let writer = std::thread::spawn(move || {
            let _guard = writer_gate.lock().unwrap();
            gate_held_tx.send(()).unwrap();
            write_rx.recv().unwrap();
            let mut config = LiosConfig::load(&writer_config_path).unwrap();
            config.spaces.clear();
            config.spaces.insert("winner".to_string(), writer_repo);
            config.key_file_path = Some(writer_key_path);
            config.save(&writer_config_path).unwrap();
            written_tx
                .send(fs::read(&writer_config_path).unwrap())
                .unwrap();
        });
        gate_held_rx.recv().unwrap();

        let (verified_tx, verified_rx) = oneshot::channel();
        let import_paths = paths.clone();
        let import_candidate = candidate_path.clone();
        let import_target = target.clone();
        let import_adapter = Arc::clone(&adapter);
        let import_gate = Arc::clone(&gate);
        let import = tokio::spawn(async move {
            import_candidate_with_adapter_after_verification(
                &import_paths,
                import_gate.as_ref(),
                &import_candidate,
                Some(&import_target),
                Some(import_adapter.as_ref()),
                move || verified_tx.send(()).unwrap(),
            )
            .await
        });

        verified_rx.await.unwrap();
        write_tx.send(()).unwrap();
        let concurrent_bytes = written_rx.await.unwrap();
        let error = import.await.unwrap().unwrap_err();
        writer.join().unwrap();

        assert_eq!(error.code, CommandErrorCode::RemoteConflict);
        assert_eq!(fs::read(&paths.config).unwrap(), concurrent_bytes);
        let saved = LiosConfig::load(&paths.config).unwrap();
        assert_eq!(saved.spaces.get("winner"), Some(&concurrent_repo));
        assert_eq!(
            saved.key_file_path.as_deref(),
            Some(concurrent_key_path.as_path())
        );
    }

    #[tokio::test]
    async fn uninitialized_or_unconfigured_space_accepts_format_only_verification() {
        let temp = tempdir().unwrap();
        let candidate_path = temp.path().join("candidate.key");
        KeyFile::generate_to_path(&candidate_path).unwrap();
        let target = repo();
        let adapter = FakeCatalogAdapter::not_found();

        let uninitialized =
            verify_candidate_with_adapter(&candidate_path, Some(&target), Some(&adapter))
                .await
                .unwrap();
        assert!(uninitialized.format_valid);
        assert!(!uninitialized.catalog_checked);
        assert_eq!(uninitialized.checked_space, Some(target));

        let format_only =
            verify_candidate_with_adapter::<FakeCatalogAdapter>(&candidate_path, None, None)
                .await
                .unwrap();
        assert!(format_only.format_valid);
        assert!(!format_only.catalog_checked);
        assert_eq!(format_only.checked_space, None);
    }
}
