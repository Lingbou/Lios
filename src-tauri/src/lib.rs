pub mod catalog_mutation_gate;
pub mod catalog_probe;
pub mod command_error;
pub mod command_surface;
pub mod download_service;
pub mod production_config;

#[cfg(test)]
#[path = "../build_support.rs"]
mod build_support;

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use catalog_mutation_gate::CatalogMutationGate;
use catalog_probe::{ensure_space_can_initialize, map_catalog_load_error};
use command_error::{CommandError, CommandErrorCode};
use command_surface::with_registered_commands;
use download_service::prepare_download_task;
use lios_core::cache::{cleanup_temporary_staging, prune_unreferenced_staging, CacheCleanupReport};
use lios_core::catalog::{
    Catalog, CatalogRemoteFile, CatalogSelection, CatalogTreeNode, ConflictResolution, DriveItem,
    UploadConflict, CATALOG_FILE,
};
use lios_core::catalog_transaction::{
    execute_catalog_transaction, CatalogTransactionOutcome, CatalogTransactionPhase,
    CatalogTransactionProgress, CatalogTransactionSpec,
};
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
use lios_core::credentials::{protect_to_file, unprotect_from_file};
use lios_core::crypto::KeyFile;
use lios_core::modelscope::{DatasetRepoSummary, ModelScopeAdapter, ModelScopeUserSummary};
use lios_core::pack::PackOptions;
use lios_core::restore::{RestoreConflictPolicy, RestoreOptions};
use lios_core::storage::{
    plan_catalog_sync_changes, CatalogSyncFile, CatalogSyncUpload, StorageAdapter, StorageObject,
};
use lios_core::tasks::{TaskRecord, TaskState, TaskStore};
use production_config::{
    configured_endpoint, generate_key_file_and_bind, persist_config, prepare_config_for_write,
    prepare_startup_config, validate_repo, SetupWarning,
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::Emitter;
use tokio::sync::Mutex;
use uuid::Uuid;

type CommandResult<T> = std::result::Result<T, CommandError>;

struct AppContext {
    paths: LiosPaths,
    catalog_mutation_gate: CatalogMutationGate,
    task_lifecycle_gate: Mutex<TaskLifecycleState>,
}

#[derive(Default)]
struct TaskLifecycleState {
    active_workers: HashSet<Uuid>,
}

impl AppContext {
    fn new() -> Self {
        let paths = LiosPaths::default_user();
        let _ = cleanup_temporary_staging(&paths.staging);
        if let Ok(store) = TaskStore::open(&paths.database) {
            let _ = store.mark_running_interrupted("上次应用退出，任务已中断");
        }
        Self {
            paths,
            catalog_mutation_gate: CatalogMutationGate::default(),
            task_lifecycle_gate: Mutex::new(TaskLifecycleState::default()),
        }
    }
}

#[derive(Serialize)]
struct PathsDto {
    home: String,
    config: String,
    database: String,
    staging: String,
    credentials: String,
}

#[derive(Serialize)]
struct SetupSnapshot {
    paths: PathsDto,
    config: LiosConfig,
    has_token: bool,
    tasks: Vec<TaskRecord>,
    warning: Option<SetupWarning>,
}

#[derive(Clone, Serialize)]
struct TaskUpdateEvent {
    tasks: Vec<TaskRecord>,
}

#[derive(Serialize)]
struct CatalogLoadResult {
    local_path: String,
    bytes: u64,
    tree: CatalogTreeNode,
    warnings: Vec<String>,
}

#[derive(Serialize)]
struct DatasetRepoListResult {
    user: ModelScopeUserSummary,
    repositories: Vec<DatasetRepoSummary>,
}

struct SyncWork {
    upload: Vec<CatalogSyncUpload>,
    delete: Vec<String>,
    initial_remote_inventory: Vec<StorageObject>,
    prepublish_safe_paths: HashSet<String>,
    base_catalog_sha256: Option<String>,
    probe_directory: PathBuf,
}

struct CatalogBaseline {
    catalog_sha256: Option<String>,
    referenced_paths: HashSet<String>,
    remote_objects: Vec<StorageObject>,
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
        credentials: paths.credentials.display().to_string(),
    }
}

fn load_config(paths: &LiosPaths) -> CommandResult<LiosConfig> {
    LiosConfig::load(&paths.config).map_err(to_err)
}

fn task_store(paths: &LiosPaths) -> CommandResult<TaskStore> {
    TaskStore::open(&paths.database).map_err(to_err)
}

fn emit_tasks(app: &tauri::AppHandle, paths: &LiosPaths) {
    if let Ok(tasks) = task_store(paths).and_then(|store| store.list().map_err(to_err)) {
        let _ = app.emit("lios-tasks-updated", TaskUpdateEvent { tasks });
    }
}

fn task_interrupt(paths: &LiosPaths, id: Uuid) -> CommandResult<Option<TaskState>> {
    task_interrupt_core(paths, id).map_err(to_err)
}

fn task_interrupt_core(paths: &LiosPaths, id: Uuid) -> lios_core::Result<Option<TaskState>> {
    let state = TaskStore::open(&paths.database)?
        .list()?
        .into_iter()
        .find(|task| task.id == id)
        .map(|task| task.state);
    match state {
        Some(TaskState::Paused) => Ok(Some(TaskState::Paused)),
        Some(TaskState::Canceled) => Ok(Some(TaskState::Canceled)),
        _ => Ok(None),
    }
}

fn insert_task(paths: &LiosPaths, task: &TaskRecord) -> CommandResult<()> {
    task_store(paths)?.insert(task).map_err(to_err)
}

fn update_task_transfer(
    paths: &LiosPaths,
    id: Uuid,
    done: u64,
    total: u64,
    bytes_done: u64,
    bytes_total: u64,
    speed_bps: u64,
) -> CommandResult<()> {
    task_store(paths)?
        .update_transfer(id, done, total, bytes_done, bytes_total, speed_bps)
        .map_err(to_err)
}

fn update_task_phase(paths: &LiosPaths, id: Uuid, phase: Option<String>) -> CommandResult<()> {
    task_store(paths)?.update_phase(id, phase).map_err(to_err)
}

fn average_speed_bps(bytes_done: u64, start: Instant) -> u64 {
    let elapsed = start.elapsed().as_secs_f64();
    if elapsed <= 0.0 {
        0
    } else {
        (bytes_done as f64 / elapsed) as u64
    }
}

fn transaction_phase_label(phase: CatalogTransactionPhase) -> &'static str {
    match phase {
        CatalogTransactionPhase::ValidateBlobs => "validating",
        CatalogTransactionPhase::UploadBlobs => "uploading",
        CatalogTransactionPhase::Prepublish
        | CatalogTransactionPhase::ProbeCatalog
        | CatalogTransactionPhase::Publish => "committing",
        CatalogTransactionPhase::Cleanup => "cleaning",
    }
}

fn persist_transaction_progress(
    paths: &LiosPaths,
    app: Option<&tauri::AppHandle>,
    task: &mut TaskRecord,
    started: Instant,
    progress: CatalogTransactionProgress,
) -> lios_core::Result<()> {
    let phase = transaction_phase_label(progress.phase).to_string();
    task.phase = Some(phase.clone());
    task.progress_done = progress.completed_items;
    task.progress_total = progress.total_items;
    task.bytes_done = progress.bytes_done;
    task.bytes_total = progress.bytes_total;
    task.speed_bps = average_speed_bps(progress.bytes_done, started);
    let store = TaskStore::open(&paths.database)?;
    store.update_phase(task.id, Some(phase))?;
    store.update_transfer(
        task.id,
        task.progress_done,
        task.progress_total,
        task.bytes_done,
        task.bytes_total,
        task.speed_bps,
    )?;
    if let Some(app) = app {
        emit_tasks(app, paths);
    }
    Ok(())
}

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

fn read_token(paths: &LiosPaths) -> CommandResult<String> {
    unprotect_from_file(&paths.credentials).map_err(to_err)
}

fn adapter_from_config(
    paths: &LiosPaths,
    config: &LiosConfig,
) -> CommandResult<(ModelScopeAdapter, RepoConfig)> {
    let repo = config
        .active_repo
        .clone()
        .ok_or_else(|| CommandError::invalid_input("dataset repo is not configured"))?;
    let repo = validate_repo(repo)?;
    let token = read_token(paths)?;
    Ok((ModelScopeAdapter::new(repo.endpoint.clone(), token), repo))
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

fn sha256_hex_file(path: &Path) -> CommandResult<String> {
    let bytes = fs::read(path).map_err(to_err)?;
    Ok(hex::encode(Sha256::digest(bytes)))
}

fn local_remote_file_is_valid(local_path: &Path, file: &CatalogRemoteFile) -> CommandResult<bool> {
    if !local_path.exists() {
        return Ok(false);
    }
    match &file.sha256 {
        Some(expected) => Ok(sha256_hex_file(local_path)? == *expected),
        None => Ok(true),
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

async fn activate_existing_task(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    task_id: Uuid,
    state: TaskState,
) -> CommandResult<()> {
    set_task_state(paths, gate, task_id, state, None).await
}

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

fn cleanup_is_safe(paths: &LiosPaths, lifecycle: &TaskLifecycleState) -> CommandResult<bool> {
    if !lifecycle.active_workers.is_empty() {
        return Ok(false);
    }
    Ok(!task_store(paths)?
        .list()
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
            .list()
            .map_err(to_err)?
            .into_iter()
            .find(|task| task.id == task_id)
            .map(|task| task.state);
        let final_state = match (preserve_control_state, current_state) {
            (true, Some(TaskState::Canceled)) => TaskState::Canceled,
            (true, Some(TaskState::Paused)) => TaskState::Paused,
            _ => intended_state,
        };
        let final_error = if final_state == TaskState::Failed {
            error
        } else {
            None
        };
        update_task_phase(paths, task_id, None)?;
        update_task_state(paths, task_id, final_state.clone(), final_error)?;
        Ok(final_state)
    })();
    lifecycle.active_workers.remove(&task_id);
    if cleanup_is_safe(paths, &lifecycle).unwrap_or(false) {
        let _ = cleanup_current_staging_cache(paths, true, false);
    }
    result
}

async fn finish_committed_worker(
    paths: &LiosPaths,
    gate: &Mutex<TaskLifecycleState>,
    catalog_mutation_gate: &CatalogMutationGate,
    task_id: Uuid,
) -> CommandResult<TaskState> {
    finish_worker(
        paths,
        gate,
        catalog_mutation_gate,
        task_id,
        TaskState::Completed,
        None,
        false,
    )
    .await
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
    if store
        .list()
        .map_err(to_err)?
        .iter()
        .any(|task| task.id == task_id && task_state_blocks_clear(&task.state))
    {
        return Err(CommandError::invalid_input("active task cannot be cleared"));
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

fn desired_catalog_objects(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
) -> CommandResult<Vec<CatalogSyncFile>> {
    let mut desired = Vec::new();
    let catalog_path = paths.staging.join(CATALOG_FILE);
    desired.push(CatalogSyncFile {
        path: CATALOG_FILE.to_string(),
        local_path: Some(catalog_path.clone()),
        expected_sha256: Some(sha256_hex_file(&catalog_path)?),
        expected_size: Some(fs::metadata(&catalog_path).map_err(to_err)?.len()),
    });
    for file in catalog
        .remote_files_for_selection(&CatalogSelection::All, key)
        .map_err(to_err)?
    {
        let local_path = remote_to_staging_path(&paths.staging, &file.path)?;
        desired.push(CatalogSyncFile {
            path: file.path,
            local_path: local_path.exists().then_some(local_path),
            expected_sha256: file.sha256,
            expected_size: None,
        });
    }
    desired.sort_by(|a, b| {
        let a_catalog = a.path == CATALOG_FILE;
        let b_catalog = b.path == CATALOG_FILE;
        a_catalog.cmp(&b_catalog).then_with(|| a.path.cmp(&b.path))
    });
    desired.dedup_by(|a, b| a.path == b.path);
    Ok(desired)
}

fn catalog_reference_paths(catalog: &Catalog, key: &KeyFile) -> CommandResult<HashSet<String>> {
    let mut referenced = HashSet::from([CATALOG_FILE.to_string()]);
    referenced.extend(
        catalog
            .remote_files_for_selection(&CatalogSelection::All, key)
            .map_err(to_err)?
            .into_iter()
            .map(|file| file.path),
    );
    Ok(referenced)
}

fn catalog_baseline_from_downloaded_catalog(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    remote_objects: Vec<StorageObject>,
) -> CommandResult<CatalogBaseline> {
    Ok(CatalogBaseline {
        catalog_sha256: Some(sha256_hex_file(&paths.staging.join(CATALOG_FILE))?),
        referenced_paths: catalog_reference_paths(catalog, key)?,
        remote_objects,
    })
}

async fn download_catalog_baseline(
    paths: &LiosPaths,
    key: &KeyFile,
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
) -> CommandResult<(Catalog, CatalogBaseline)> {
    let catalog_path = paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
        .await
        .map_err(to_err)?;
    let catalog = Catalog::from_staging(paths.staging.clone());
    let remote_objects = adapter
        .list_objects(&repo.namespace, &repo.dataset, "")
        .await
        .map_err(to_err)?;
    let baseline = catalog_baseline_from_downloaded_catalog(paths, &catalog, key, remote_objects)?;
    Ok((catalog, baseline))
}

fn plan_catalog_sync_with_baseline(
    desired: Vec<CatalogSyncFile>,
    baseline: CatalogBaseline,
    probe_directory: PathBuf,
) -> CommandResult<SyncWork> {
    let plan =
        plan_catalog_sync_changes(desired, baseline.remote_objects.clone()).map_err(to_err)?;
    let remote_paths = baseline
        .remote_objects
        .iter()
        .map(|object| object.path.as_str())
        .collect::<HashSet<_>>();
    let prepublish_safe_paths = plan
        .upload
        .iter()
        .filter(|upload| {
            upload.path != CATALOG_FILE
                && !baseline.referenced_paths.contains(&upload.path)
                && !remote_paths.contains(upload.path.as_str())
        })
        .map(|upload| upload.path.clone())
        .collect();
    Ok(SyncWork {
        upload: plan.upload,
        delete: plan.delete,
        initial_remote_inventory: baseline.remote_objects,
        prepublish_safe_paths,
        base_catalog_sha256: baseline.catalog_sha256,
        probe_directory,
    })
}

fn plan_catalog_sync(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    baseline: CatalogBaseline,
) -> CommandResult<SyncWork> {
    let desired = desired_catalog_objects(paths, catalog, key)?;
    plan_catalog_sync_with_baseline(desired, baseline, paths.staging.clone())
}

async fn execute_sync_work<A, C, P>(
    adapter: &A,
    repo: &RepoConfig,
    work: SyncWork,
    should_cancel: C,
    on_progress: P,
) -> CommandResult<CatalogTransactionOutcome>
where
    A: StorageAdapter + ?Sized,
    C: FnMut() -> lios_core::Result<bool>,
    P: FnMut(CatalogTransactionProgress) -> lios_core::Result<()>,
{
    execute_catalog_transaction(
        adapter,
        &repo.namespace,
        &repo.dataset,
        CatalogTransactionSpec {
            uploads: work.upload,
            delete_paths: work.delete,
            initial_remote_inventory: work.initial_remote_inventory,
            prepublish_safe_paths: work.prepublish_safe_paths,
            base_catalog_sha256: work.base_catalog_sha256,
            probe_directory: work.probe_directory,
        },
        should_cancel,
        on_progress,
    )
    .await
    .map_err(to_err)
}

async fn sync_current_catalog(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
    baseline: CatalogBaseline,
) -> CommandResult<Vec<String>> {
    let work = plan_catalog_sync(paths, catalog, key, baseline)?;
    match execute_sync_work(adapter, repo, work, || Ok(false), |_progress| Ok(())).await? {
        CatalogTransactionOutcome::Completed { warnings } => Ok(warnings),
        CatalogTransactionOutcome::Canceled => Err(CommandError::new(
            CommandErrorCode::Internal,
            "catalog transaction was unexpectedly canceled",
            false,
            None,
        )),
    }
}

#[tauri::command]
fn current_setup(state: tauri::State<'_, AppContext>) -> CommandResult<SetupSnapshot> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    let warning = prepare_startup_config(&state.paths, &mut config)?;
    let tasks = task_store(&state.paths)?.list().map_err(to_err)?;
    Ok(SetupSnapshot {
        paths: paths_dto(&state.paths),
        config,
        has_token: state.paths.credentials.exists(),
        tasks,
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
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), token);
    adapter
        .create_repo(&repo.namespace, &repo.dataset)
        .await
        .map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(repo);
    persist_config(&state.paths, &mut config)
}

#[tauri::command]
async fn connect_dataset_repo(
    state: tauri::State<'_, AppContext>,
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
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), token);
    if !adapter
        .repo_exists(&repo.namespace, &repo.dataset)
        .await
        .map_err(to_err)?
    {
        return Err(CommandError::invalid_input(
            "dataset repo was not found or is not visible",
        ));
    }
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(repo);
    persist_config(&state.paths, &mut config)
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
        .map_err(to_err)?;
    Ok(DatasetRepoListResult { user, repositories })
}

#[tauri::command]
fn select_dataset_repo(state: tauri::State<'_, AppContext>, repo: RepoConfig) -> CommandResult<()> {
    let repo = validate_repo(repo)?;
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(repo);
    persist_config(&state.paths, &mut config)
}

#[tauri::command]
async fn initialize_space(
    state: tauri::State<'_, AppContext>,
    space: RepoConfig,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let repo = validate_repo(space)?;
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
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(repo.clone());
    persist_config(&state.paths, &mut config)?;
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
    space: RepoConfig,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let repo = validate_repo(space)?;
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
    let _shared_staging_guard = state.catalog_mutation_gate.lock_shared_staging().await;
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(repo.clone());
    persist_config(&state.paths, &mut config)?;
    let key = key_from_config(&config)?;
    let local_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
        .await
        .map_err(map_catalog_load_error)?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
        warnings: Vec::new(),
    })
}

#[tauri::command]
async fn preview_upload_conflicts(
    state: tauri::State<'_, AppContext>,
    parent_node_id: String,
    paths: Vec<String>,
) -> CommandResult<Vec<UploadConflict>> {
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
    let _shared_staging_guard = state.catalog_mutation_gate.lock_shared_staging().await;
    let catalog_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
        .await
        .map_err(to_err)?;
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    let paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
    catalog
        .preview_upload_conflicts(&parent_node_id, &paths, &key)
        .map_err(to_err)
}

#[tauri::command]
async fn create_folder(
    state: tauri::State<'_, AppContext>,
    parent_node_id: String,
    name: String,
) -> CommandResult<CatalogLoadResult> {
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
    let _catalog_mutation_guard = state.catalog_mutation_gate.lock_mutation().await;
    let local_path = state.paths.staging.join(CATALOG_FILE);
    let (catalog, baseline) =
        download_catalog_baseline(&state.paths, &key, &adapter, &repo).await?;
    catalog
        .create_folder(&parent_node_id, &name, &key)
        .map_err(to_err)?;
    let warnings =
        sync_current_catalog(&state.paths, &catalog, &key, &adapter, &repo, baseline).await?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
        warnings,
    })
}

#[tauri::command]
async fn rename_node(
    state: tauri::State<'_, AppContext>,
    node_id: String,
    new_name: String,
) -> CommandResult<CatalogLoadResult> {
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
    let _catalog_mutation_guard = state.catalog_mutation_gate.lock_mutation().await;
    let local_path = state.paths.staging.join(CATALOG_FILE);
    let (catalog, baseline) =
        download_catalog_baseline(&state.paths, &key, &adapter, &repo).await?;
    catalog
        .rename_node(&node_id, &new_name, &key)
        .map_err(to_err)?;
    let warnings =
        sync_current_catalog(&state.paths, &catalog, &key, &adapter, &repo, baseline).await?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
        warnings,
    })
}

#[tauri::command]
async fn search_catalog(
    state: tauri::State<'_, AppContext>,
    query: String,
) -> CommandResult<Vec<DriveItem>> {
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
    let _shared_staging_guard = state.catalog_mutation_gate.lock_shared_staging().await;
    let catalog_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
        .await
        .map_err(to_err)?;
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    catalog.search(&query, &key).map_err(to_err)
}

#[tauri::command]
fn generate_key_file(
    state: tauri::State<'_, AppContext>,
    path: Option<String>,
) -> CommandResult<String> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let path = path
        .map(PathBuf::from)
        .unwrap_or_else(|| state.paths.home.join("recovery.key"));
    let path = generate_key_file_and_bind(&state.paths, path)?;
    Ok(path.display().to_string())
}

#[tauri::command]
fn import_key_file(state: tauri::State<'_, AppContext>, path: String) -> CommandResult<()> {
    let path = PathBuf::from(path);
    KeyFile::load_from_path(&path).map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    prepare_config_for_write(&state.paths, &mut config)?;
    config.key_file_path = Some(path);
    persist_config(&state.paths, &mut config)
}

#[tauri::command]
async fn enqueue_upload_to_folder(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    parent_node_id: String,
    paths: Vec<String>,
    conflict_resolutions: Vec<ConflictResolution>,
) -> CommandResult<TaskRecord> {
    if paths.is_empty() {
        return Err(CommandError::invalid_input("upload paths cannot be empty"));
    }
    let mut task = activate_new_task(
        &state.paths,
        &state.task_lifecycle_gate,
        TaskRecord::queued("upload", 0),
    )
    .await?;
    emit_tasks(&app, &state.paths);
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let _catalog_mutation_guard = state.catalog_mutation_gate.lock_mutation().await;
        let (catalog, baseline) =
            download_catalog_baseline(&state.paths, &key, &adapter, &repo).await?;
        let upload_paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        update_task_phase(&state.paths, task.id, Some("preparing".to_string()))?;
        emit_tasks(&app, &state.paths);
        let preparing_started = Instant::now();
        let report = catalog
            .add_paths_to_folder_with_remote_inventory_and_progress_and_report(
                &parent_node_id,
                &upload_paths,
                &conflict_resolutions,
                &key,
                PackOptions {
                    chunk_size: config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
                    staging_dir: state.paths.staging.clone(),
                },
                &baseline.remote_objects,
                |progress| {
                    task.progress_done = progress.completed_chunks;
                    task.progress_total = progress.total_chunks;
                    let speed_bps = average_speed_bps(progress.completed_bytes, preparing_started);
                    let _ = update_task_transfer(
                        &state.paths,
                        task.id,
                        progress.completed_chunks,
                        progress.total_chunks,
                        progress.completed_bytes,
                        progress.total_bytes,
                        speed_bps,
                    );
                    emit_tasks(&app, &state.paths);
                },
            )
            .map_err(to_err)?;
        report.ensure_no_skipped_paths().map_err(to_err)?;
        let work = plan_catalog_sync(&state.paths, &catalog, &key, baseline)?;
        let transaction_started = Instant::now();
        let task_id = task.id;
        let mut interrupted_state = None;
        let outcome = execute_sync_work(
            &adapter,
            &repo,
            work,
            || {
                interrupted_state = task_interrupt_core(&state.paths, task_id)?;
                Ok(interrupted_state.is_some())
            },
            |progress| {
                persist_transaction_progress(
                    &state.paths,
                    Some(&app),
                    &mut task,
                    transaction_started,
                    progress,
                )
            },
        )
        .await?;
        match outcome {
            CatalogTransactionOutcome::Completed { warnings } => {
                Ok::<(Option<TaskState>, Vec<String>), CommandError>((None, warnings))
            }
            CatalogTransactionOutcome::Canceled => Ok((
                Some(interrupted_state.unwrap_or(TaskState::Canceled)),
                Vec::new(),
            )),
        }
    }
    .await;
    match result {
        Ok((None, warnings)) => {
            task.state = finish_committed_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
            )
            .await?;
            task.error = (!warnings.is_empty()).then(|| warnings.join("; "));
            if task.error.is_some() {
                task_store(&state.paths)?
                    .update_state(task.id, task.state.clone(), task.error.clone())
                    .map_err(to_err)?;
            }
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Ok((Some(interrupt_state), _warnings)) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                interrupt_state,
                None,
            )
            .await?;
            task.error = None;
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                TaskState::Failed,
                Some(error.message.clone()),
            )
            .await?;
            task.error = if task.state == TaskState::Failed {
                Some(error.message.clone())
            } else {
                None
            };
            emit_tasks(&app, &state.paths);
            Err(error)
        }
    }
}

#[tauri::command]
async fn enqueue_delete_nodes(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    node_ids: Vec<String>,
) -> CommandResult<TaskRecord> {
    if node_ids.is_empty() {
        return Err(CommandError::invalid_input(
            "delete selection cannot be empty",
        ));
    }
    let mut task = activate_new_task(
        &state.paths,
        &state.task_lifecycle_gate,
        TaskRecord::queued("delete", 0),
    )
    .await?;
    emit_tasks(&app, &state.paths);
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let _catalog_mutation_guard = state.catalog_mutation_gate.lock_mutation().await;
        let (catalog, baseline) =
            download_catalog_baseline(&state.paths, &key, &adapter, &repo).await?;
        catalog.delete_nodes(&node_ids, &key).map_err(to_err)?;
        let work = plan_catalog_sync(&state.paths, &catalog, &key, baseline)?;
        let transaction_started = Instant::now();
        let task_id = task.id;
        let mut interrupted_state = None;
        let outcome = execute_sync_work(
            &adapter,
            &repo,
            work,
            || {
                interrupted_state = task_interrupt_core(&state.paths, task_id)?;
                Ok(interrupted_state.is_some())
            },
            |progress| {
                persist_transaction_progress(
                    &state.paths,
                    Some(&app),
                    &mut task,
                    transaction_started,
                    progress,
                )
            },
        )
        .await?;
        match outcome {
            CatalogTransactionOutcome::Completed { warnings } => {
                Ok::<(Option<TaskState>, Vec<String>), CommandError>((None, warnings))
            }
            CatalogTransactionOutcome::Canceled => Ok((
                Some(interrupted_state.unwrap_or(TaskState::Canceled)),
                Vec::new(),
            )),
        }
    }
    .await;
    match result {
        Ok((None, warnings)) => {
            task.state = finish_committed_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
            )
            .await?;
            task.error = (!warnings.is_empty()).then(|| warnings.join("; "));
            if task.error.is_some() {
                task_store(&state.paths)?
                    .update_state(task.id, task.state.clone(), task.error.clone())
                    .map_err(to_err)?;
            }
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Ok((Some(interrupt_state), _warnings)) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                interrupt_state,
                None,
            )
            .await?;
            task.error = None;
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                TaskState::Failed,
                Some(error.message.clone()),
            )
            .await?;
            task.error = if task.state == TaskState::Failed {
                Some(error.message.clone())
            } else {
                None
            };
            emit_tasks(&app, &state.paths);
            Err(error)
        }
    }
}

#[tauri::command]
async fn enqueue_download(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    node_ids: Vec<String>,
    output_dir: String,
) -> CommandResult<TaskRecord> {
    let prepared = prepare_download_task(node_ids, output_dir)?;
    let mut task =
        activate_new_task(&state.paths, &state.task_lifecycle_gate, prepared.task).await?;
    let selection = prepared.selection;
    let output_dir = prepared.output_dir;
    emit_tasks(&app, &state.paths);
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let _shared_staging_guard = state.catalog_mutation_gate.lock_shared_staging().await;
        let catalog_path = state.paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let catalog = Catalog::from_staging(state.paths.staging.clone());
        let remote_files = catalog
            .remote_files_for_selection(&selection, &key)
            .map_err(to_err)?;
        let remote_sizes = adapter
            .list_objects(&repo.namespace, &repo.dataset, "")
            .await
            .map_err(to_err)?
            .into_iter()
            .map(|object| (object.path, object.size))
            .collect::<HashMap<_, _>>();
        let mut download_plan = Vec::new();
        let mut download_total_bytes = 0u64;
        for file in &remote_files {
            let local_path = remote_to_staging_path(&state.paths.staging, &file.path)?;
            let was_cached = local_remote_file_is_valid(&local_path, file)?;
            let size = remote_sizes.get(&file.path).copied().unwrap_or(0);
            if !was_cached {
                download_total_bytes += size;
            }
            download_plan.push((file, local_path, was_cached));
        }
        update_task_phase(&state.paths, task.id, Some("downloading".to_string()))?;
        task.progress_total = remote_files.len() as u64 + 1;
        task.progress_done = 0;
        task.bytes_done = 0;
        let downloading_started = Instant::now();
        update_task_transfer(
            &state.paths,
            task.id,
            task.progress_done,
            task.progress_total,
            task.bytes_done,
            download_total_bytes,
            0,
        )?;
        emit_tasks(&app, &state.paths);
        for (index, (file, local_path, was_cached)) in download_plan.iter().enumerate() {
            if let Some(state) = task_interrupt(&state.paths, task.id)? {
                return Ok::<Option<TaskState>, CommandError>(Some(state));
            }
            if !was_cached {
                let completed_before_object = task.bytes_done;
                adapter
                    .download_object_with_progress(
                        &repo.namespace,
                        &repo.dataset,
                        &file.path,
                        local_path,
                        |object_bytes_done| {
                            let bytes_done = completed_before_object + object_bytes_done;
                            let _ = update_task_transfer(
                                &state.paths,
                                task.id,
                                index as u64,
                                task.progress_total,
                                bytes_done,
                                download_total_bytes,
                                average_speed_bps(bytes_done, downloading_started),
                            );
                            emit_tasks(&app, &state.paths);
                        },
                    )
                    .await
                    .map_err(to_err)?;
            }
            if !local_remote_file_is_valid(local_path, file)? {
                return Err(CommandError::new(
                    CommandErrorCode::CorruptedData,
                    format!("downloaded object failed hash verification: {}", file.path),
                    false,
                    Some(serde_json::json!({ "path": file.path })),
                ));
            }
            task.progress_done = (index + 1) as u64;
            if !was_cached {
                task.bytes_done += std::fs::metadata(local_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0);
            }
            update_task_transfer(
                &state.paths,
                task.id,
                task.progress_done,
                task.progress_total,
                task.bytes_done,
                download_total_bytes,
                average_speed_bps(task.bytes_done, downloading_started),
            )?;
            emit_tasks(&app, &state.paths);
        }
        if let Some(state) = task_interrupt(&state.paths, task.id)? {
            return Ok::<Option<TaskState>, CommandError>(Some(state));
        }
        update_task_phase(&state.paths, task.id, Some("restoring".to_string()))?;
        emit_tasks(&app, &state.paths);
        catalog
            .restore(
                selection,
                &key,
                RestoreOptions {
                    output_dir,
                    conflict_policy: RestoreConflictPolicy::Rename,
                },
            )
            .map_err(to_err)?;
        task.progress_done = task.progress_total;
        update_task_transfer(
            &state.paths,
            task.id,
            task.progress_done,
            task.progress_total,
            task.bytes_done,
            download_total_bytes,
            average_speed_bps(task.bytes_done, downloading_started),
        )?;
        emit_tasks(&app, &state.paths);
        Ok::<Option<TaskState>, CommandError>(None)
    }
    .await;
    match result {
        Ok(None) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                TaskState::Completed,
                None,
            )
            .await?;
            task.error = None;
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Ok(Some(interrupt_state)) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                interrupt_state,
                None,
            )
            .await?;
            task.error = None;
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = finish_active_worker(
                &state.paths,
                &state.task_lifecycle_gate,
                &state.catalog_mutation_gate,
                task.id,
                TaskState::Failed,
                Some(error.message.clone()),
            )
            .await?;
            task.error = if task.state == TaskState::Failed {
                Some(error.message.clone())
            } else {
                None
            };
            emit_tasks(&app, &state.paths);
            Err(error)
        }
    }
}

#[tauri::command]
fn list_tasks(state: tauri::State<'_, AppContext>) -> CommandResult<Vec<TaskRecord>> {
    task_store(&state.paths)?.list().map_err(to_err)
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
) -> CommandResult<Vec<TaskRecord>> {
    set_task_state(
        &state.paths,
        &state.task_lifecycle_gate,
        task_id,
        TaskState::Paused,
        None,
    )
    .await?;
    let store = task_store(&state.paths)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[tauri::command]
async fn resume_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    activate_existing_task(
        &state.paths,
        &state.task_lifecycle_gate,
        task_id,
        TaskState::Queued,
    )
    .await?;
    let store = task_store(&state.paths)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[tauri::command]
async fn cancel_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    set_task_state(
        &state.paths,
        &state.task_lifecycle_gate,
        task_id,
        TaskState::Canceled,
        None,
    )
    .await?;
    let store = task_store(&state.paths)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[tauri::command]
async fn clear_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    clear_task_record(&state.paths, &state.task_lifecycle_gate, task_id).await?;
    let store = task_store(&state.paths)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
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
        .invoke_handler(with_registered_commands!(generate_tauri_handler))
        .run(tauri::generate_context!())
        .expect("failed to run Lios desktop app");
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
mod task_cleanup_tests {
    use std::{fs, sync::Arc, time::Duration};

    use lios_core::config::LiosPaths;
    use lios_core::tasks::{TaskRecord, TaskState, TaskStore};
    use tempfile::tempdir;
    use tokio::sync::{oneshot, Mutex};

    use super::{
        activate_existing_task, activate_new_task, cleanup_current_staging_cache, cleanup_if_idle,
        clear_task_record, finish_active_worker, finish_committed_worker, set_task_state,
        task_state_blocks_clear, task_state_is_active, CatalogMutationGate, CommandErrorCode,
        TaskLifecycleState,
    };

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
