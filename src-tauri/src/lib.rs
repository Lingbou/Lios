use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;

use lios_core::cache::{cleanup_temporary_staging, prune_unreferenced_staging, CacheCleanupReport};
use lios_core::catalog::{
    Catalog, CatalogRemoteFile, CatalogSelection, CatalogTreeNode, ConflictResolution, DriveItem,
    UploadConflict, CATALOG_FILE,
};
use lios_core::config::{ensure_default_key_configured, LiosConfig, LiosPaths, RepoConfig};
use lios_core::credentials::{protect_to_file, unprotect_from_file};
use lios_core::crypto::KeyFile;
use lios_core::modelscope::{DatasetRepoSummary, ModelScopeAdapter, ModelScopeUserSummary};
use lios_core::pack::{PackOptions, PackSource};
use lios_core::restore::{RestoreConflictPolicy, RestoreOptions};
use lios_core::storage::{plan_current_snapshot_changes, LocalStorageObject, StorageAdapter};
use lios_core::tasks::{TaskRecord, TaskState, TaskStore};
use serde::Serialize;
use sha2::{Digest, Sha256};
use tauri::Emitter;
use uuid::Uuid;

type CommandResult<T> = std::result::Result<T, String>;

const DEFAULT_MODELSCOPE_ENDPOINT: &str = "https://modelscope.cn";

struct AppContext {
    paths: LiosPaths,
}

impl AppContext {
    fn new() -> Self {
        let paths = LiosPaths::default_user();
        let _ = cleanup_temporary_staging(&paths.staging);
        if let Ok(store) = TaskStore::open(&paths.database) {
            let _ = store.mark_running_interrupted("上次应用退出，任务已中断");
        }
        Self { paths }
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
}

#[derive(Serialize)]
struct DatasetRepoListResult {
    user: ModelScopeUserSummary,
    repositories: Vec<DatasetRepoSummary>,
}

#[derive(Serialize)]
struct SyncWork {
    upload: Vec<SyncUpload>,
    delete: Vec<String>,
}

#[derive(Serialize)]
struct SyncUpload {
    path: String,
    local_path: PathBuf,
}

fn to_err(error: impl std::fmt::Display) -> String {
    error.to_string()
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

fn save_config(paths: &LiosPaths, config: &LiosConfig) -> CommandResult<()> {
    config.save(&paths.config).map_err(to_err)
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
    let state = task_store(paths)?
        .list()
        .map_err(to_err)?
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

fn update_task_progress(paths: &LiosPaths, id: Uuid, done: u64, total: u64) -> CommandResult<()> {
    task_store(paths)?
        .update_progress(id, done, total)
        .map_err(to_err)
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

fn configured_endpoint(config: &LiosConfig, endpoint: Option<String>) -> String {
    endpoint
        .and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .or_else(|| {
            config
                .active_repo
                .as_ref()
                .map(|repo| repo.endpoint.clone())
        })
        .unwrap_or_else(|| DEFAULT_MODELSCOPE_ENDPOINT.to_string())
}

fn adapter_from_config(
    paths: &LiosPaths,
    config: &LiosConfig,
) -> CommandResult<(ModelScopeAdapter, RepoConfig)> {
    let repo = config
        .active_repo
        .clone()
        .ok_or_else(|| "dataset repo is not configured".to_string())?;
    let token = read_token(paths)?;
    Ok((ModelScopeAdapter::new(repo.endpoint.clone(), token), repo))
}

fn key_from_config(config: &LiosConfig) -> CommandResult<KeyFile> {
    let path = config
        .key_file_path
        .clone()
        .ok_or_else(|| "key file is not configured".to_string())?;
    KeyFile::load_from_path(path).map_err(to_err)
}

fn reset_staging(paths: &LiosPaths) -> CommandResult<()> {
    paths.ensure_dirs().map_err(to_err)?;
    if paths.staging.exists() {
        let staging = paths.staging.canonicalize().map_err(to_err)?;
        let home = paths.home.canonicalize().map_err(to_err)?;
        if !staging.starts_with(home) {
            return Err("refusing to clear staging outside ~/.lios".to_string());
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
        return Err(format!(
            "invalid remote object path in catalog: {remote_path}"
        ));
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

fn cleanup_after_task(paths: &LiosPaths) {
    let _ = cleanup_current_staging_cache(paths, true, false);
}

fn desired_catalog_objects(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
) -> CommandResult<Vec<(String, Option<PathBuf>, String)>> {
    let mut desired = Vec::new();
    let catalog_path = paths.staging.join(CATALOG_FILE);
    desired.push((
        CATALOG_FILE.to_string(),
        Some(catalog_path.clone()),
        sha256_hex_file(&catalog_path)?,
    ));
    for file in catalog
        .remote_files_for_selection(&CatalogSelection::All, key)
        .map_err(to_err)?
    {
        let local_path = remote_to_staging_path(&paths.staging, &file.path)?;
        if local_path.exists() {
            desired.push((
                file.path,
                Some(local_path.clone()),
                sha256_hex_file(&local_path)?,
            ));
        } else if let Some(sha256) = file.sha256 {
            desired.push((file.path, None, sha256));
        } else {
            desired.push((file.path, None, String::new()));
        }
    }
    desired.sort_by(|a, b| {
        let a_catalog = a.0 == CATALOG_FILE;
        let b_catalog = b.0 == CATALOG_FILE;
        a_catalog.cmp(&b_catalog).then_with(|| a.0.cmp(&b.0))
    });
    desired.dedup_by(|a, b| a.0 == b.0);
    Ok(desired)
}

async fn plan_catalog_sync(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
) -> CommandResult<SyncWork> {
    let desired = desired_catalog_objects(paths, catalog, key)?;
    let desired_paths = desired
        .iter()
        .map(|(path, _, _)| path.clone())
        .collect::<HashSet<_>>();
    let remote_objects = adapter
        .list_objects(&repo.namespace, &repo.dataset, "")
        .await
        .map_err(to_err)?;
    let remote_by_path = remote_objects
        .iter()
        .map(|object| (object.path.as_str(), object.sha256.as_deref()))
        .collect::<HashMap<_, _>>();
    let upload = desired
        .into_iter()
        .filter_map(|(path, local_path, sha256)| {
            let local_path = local_path?;
            (remote_by_path.get(path.as_str()).copied().flatten() != Some(sha256.as_str()))
                .then_some(SyncUpload { path, local_path })
        })
        .collect::<Vec<_>>();
    let mut delete = remote_objects
        .into_iter()
        .filter(|object| {
            object.path.starts_with("objects/") && !desired_paths.contains(&object.path)
        })
        .map(|object| object.path)
        .collect::<Vec<_>>();
    delete.sort();
    delete.dedup();
    Ok(SyncWork { upload, delete })
}

async fn execute_sync_work(
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
    work: &SyncWork,
) -> CommandResult<()> {
    for object in &work.upload {
        adapter
            .upload_object(
                &repo.namespace,
                &repo.dataset,
                &object.path,
                &object.local_path,
            )
            .await
            .map_err(to_err)?;
    }
    if !work.delete.is_empty() {
        adapter
            .delete_objects(&repo.namespace, &repo.dataset, &work.delete)
            .await
            .map_err(to_err)?;
    }
    Ok(())
}

async fn sync_current_catalog(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    adapter: &ModelScopeAdapter,
    repo: &RepoConfig,
) -> CommandResult<()> {
    let work = plan_catalog_sync(paths, catalog, key, adapter, repo).await?;
    execute_sync_work(adapter, repo, &work).await
}

fn selection_from_node_id(node_id: Option<String>) -> CatalogSelection {
    node_id
        .and_then(|id| {
            let trimmed = id.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .map(CatalogSelection::Node)
        .unwrap_or(CatalogSelection::All)
}

fn selection_from_node_ids(node_ids: Vec<String>) -> CatalogSelection {
    let ids = node_ids
        .into_iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        CatalogSelection::All
    } else {
        CatalogSelection::Nodes(ids)
    }
}

fn staged_files(staging: &Path) -> CommandResult<Vec<(PathBuf, LocalStorageObject)>> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(staging) {
        let entry = entry.map_err(to_err)?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry.path().strip_prefix(staging).map_err(to_err)?;
        let remote = relative.to_string_lossy().replace('\\', "/");
        files.push((
            entry.path().to_path_buf(),
            LocalStorageObject {
                path: remote,
                sha256: sha256_hex_file(entry.path())?,
            },
        ));
    }
    files.sort_by(|a, b| {
        let a_catalog = a.1.path == "catalog.enc";
        let b_catalog = b.1.path == "catalog.enc";
        a_catalog
            .cmp(&b_catalog)
            .then_with(|| a.1.path.cmp(&b.1.path))
    });
    Ok(files)
}

async fn run_upload(
    app: tauri::AppHandle,
    paths: &LiosPaths,
    source_path: String,
    label: String,
) -> CommandResult<TaskRecord> {
    let mut task = TaskRecord::queued(label, 0);
    insert_task(paths, &task)?;
    update_task_state(paths, task.id, TaskState::Running, None)?;
    task.state = TaskState::Running;
    emit_tasks(&app, paths);

    let result = async {
        let config = load_config(paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(paths, &config)?;
        reset_staging(paths)?;
        update_task_phase(paths, task.id, Some("preparing".to_string()))?;
        emit_tasks(&app, paths);
        let preparing_started = Instant::now();

        let outcome = Catalog::pack_with_progress_and_report(
            PackSource::Path(PathBuf::from(source_path)),
            &key,
            PackOptions {
                chunk_size: config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
                staging_dir: paths.staging.clone(),
            },
            |progress| {
                task.progress_done = progress.completed_chunks;
                task.progress_total = progress.total_chunks;
                let speed_bps = average_speed_bps(progress.completed_bytes, preparing_started);
                let _ = update_task_transfer(
                    paths,
                    task.id,
                    progress.completed_chunks,
                    progress.total_chunks,
                    progress.completed_bytes,
                    progress.total_bytes,
                    speed_bps,
                );
                emit_tasks(&app, paths);
            },
        )
        .map_err(to_err)?;
        outcome.into_catalog().map_err(to_err)?;

        let staged = staged_files(&paths.staging)?;
        let local_objects = staged
            .iter()
            .map(|(_, object)| object.clone())
            .collect::<Vec<_>>();
        let local_paths = staged
            .into_iter()
            .map(|(path, object)| (object.path, path))
            .collect::<HashMap<_, _>>();
        let remote_objects = adapter
            .list_objects(&repo.namespace, &repo.dataset, "")
            .await
            .map_err(to_err)?;
        let sync_plan = plan_current_snapshot_changes(local_objects, remote_objects);
        let upload_objects = sync_plan.upload;
        let delete_paths = sync_plan.delete;

        update_task_phase(paths, task.id, Some("uploading".to_string()))?;
        task.progress_done = 0;
        task.bytes_done = 0;
        task.progress_total = (upload_objects.len() + delete_paths.len()) as u64;
        let upload_total_bytes = upload_objects
            .iter()
            .filter_map(|object| local_paths.get(&object.path))
            .filter_map(|local| std::fs::metadata(local).ok())
            .map(|metadata| metadata.len())
            .sum::<u64>();
        let uploading_started = Instant::now();
        update_task_transfer(
            paths,
            task.id,
            task.progress_done,
            task.progress_total,
            task.bytes_done,
            upload_total_bytes,
            0,
        )?;
        emit_tasks(&app, paths);
        for object in upload_objects {
            if let Some(state) = task_interrupt(paths, task.id)? {
                return Ok::<Option<TaskState>, String>(Some(state));
            }
            let local = local_paths
                .get(&object.path)
                .ok_or_else(|| format!("staged object disappeared: {}", object.path))?;
            adapter
                .upload_object(&repo.namespace, &repo.dataset, &object.path, local)
                .await
                .map_err(to_err)?;
            task.progress_done += 1;
            task.bytes_done += std::fs::metadata(local)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            update_task_transfer(
                paths,
                task.id,
                task.progress_done,
                task.progress_total,
                task.bytes_done,
                upload_total_bytes,
                average_speed_bps(task.bytes_done, uploading_started),
            )?;
            emit_tasks(&app, paths);
        }
        if !delete_paths.is_empty() {
            if let Some(state) = task_interrupt(paths, task.id)? {
                return Ok::<Option<TaskState>, String>(Some(state));
            }
            adapter
                .delete_objects(&repo.namespace, &repo.dataset, &delete_paths)
                .await
                .map_err(to_err)?;
            task.progress_done += delete_paths.len() as u64;
            update_task_transfer(
                paths,
                task.id,
                task.progress_done,
                task.progress_total,
                task.bytes_done,
                upload_total_bytes,
                average_speed_bps(task.bytes_done, uploading_started),
            )?;
            emit_tasks(&app, paths);
        }
        Ok::<Option<TaskState>, String>(None)
    }
    .await;

    match result {
        Ok(None) => {
            task.state = TaskState::Completed;
            task.error = None;
            update_task_phase(paths, task.id, None)?;
            update_task_state(paths, task.id, TaskState::Completed, None)?;
            cleanup_after_task(paths);
            emit_tasks(&app, paths);
            Ok(task)
        }
        Ok(Some(interrupt_state)) => {
            task.state = interrupt_state.clone();
            update_task_phase(paths, task.id, None)?;
            update_task_state(paths, task.id, interrupt_state, None)?;
            cleanup_after_task(paths);
            emit_tasks(&app, paths);
            Ok(task)
        }
        Err(error) => {
            task.state = TaskState::Failed;
            task.error = Some(error.clone());
            update_task_phase(paths, task.id, None)?;
            update_task_state(paths, task.id, TaskState::Failed, Some(error.clone()))?;
            cleanup_after_task(paths);
            emit_tasks(&app, paths);
            Err(error)
        }
    }
}

#[tauri::command]
fn current_setup(state: tauri::State<'_, AppContext>) -> CommandResult<SetupSnapshot> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    ensure_default_key_configured(&state.paths, &mut config).map_err(to_err)?;
    let tasks = task_store(&state.paths)?.list().map_err(to_err)?;
    Ok(SetupSnapshot {
        paths: paths_dto(&state.paths),
        config,
        has_token: state.paths.credentials.exists(),
        tasks,
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
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(endpoint.clone(), token);
    adapter
        .create_repo(&namespace, &dataset)
        .await
        .map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(RepoConfig {
        namespace,
        dataset,
        endpoint,
    });
    save_config(&state.paths, &config)
}

#[tauri::command]
async fn connect_dataset_repo(
    state: tauri::State<'_, AppContext>,
    namespace: String,
    dataset: String,
    endpoint: String,
) -> CommandResult<()> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(endpoint.clone(), token);
    if !adapter
        .repo_exists(&namespace, &dataset)
        .await
        .map_err(to_err)?
    {
        return Err("dataset repo was not found or is not visible".to_string());
    }
    let mut config = load_config(&state.paths)?;
    config.active_repo = Some(RepoConfig {
        namespace,
        dataset,
        endpoint,
    });
    save_config(&state.paths, &config)
}

#[tauri::command]
async fn list_dataset_repos(
    state: tauri::State<'_, AppContext>,
    endpoint: Option<String>,
) -> CommandResult<DatasetRepoListResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let config = load_config(&state.paths)?;
    let endpoint = configured_endpoint(&config, endpoint);
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
    if repo.namespace.trim().is_empty() || repo.dataset.trim().is_empty() {
        return Err("dataset repo is incomplete".to_string());
    }
    let mut config = load_config(&state.paths)?;
    let endpoint = configured_endpoint(&config, Some(repo.endpoint));
    config.active_repo = Some(RepoConfig {
        namespace: repo.namespace.trim().to_string(),
        dataset: repo.dataset.trim().to_string(),
        endpoint,
    });
    save_config(&state.paths, &config)
}

#[tauri::command]
async fn initialize_space(
    state: tauri::State<'_, AppContext>,
    space: RepoConfig,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(space.endpoint.clone(), token);
    if !adapter
        .repo_exists(&space.namespace, &space.dataset)
        .await
        .map_err(to_err)?
    {
        return Err("space was not found or is not visible".to_string());
    }
    let mut config = load_config(&state.paths)?;
    let endpoint = configured_endpoint(&config, Some(space.endpoint));
    let repo = RepoConfig {
        namespace: space.namespace.trim().to_string(),
        dataset: space.dataset.trim().to_string(),
        endpoint,
    };
    config.active_repo = Some(repo.clone());
    save_config(&state.paths, &config)?;
    let key = key_from_config(&config)?;
    reset_staging(&state.paths)?;
    let catalog = Catalog::initialize_empty(&repo.dataset, &key, state.paths.staging.clone())
        .map_err(to_err)?;
    sync_current_catalog(&state.paths, &catalog, &key, &adapter, &repo).await?;
    let bytes = fs::metadata(catalog.encrypted_catalog_path())
        .map_err(to_err)?
        .len();
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: catalog.encrypted_catalog_path().display().to_string(),
        bytes,
        tree,
    })
}

#[tauri::command]
async fn load_space_catalog(
    state: tauri::State<'_, AppContext>,
    space: RepoConfig,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let token = read_token(&state.paths)?;
    let adapter = ModelScopeAdapter::new(space.endpoint.clone(), token);
    if !adapter
        .repo_exists(&space.namespace, &space.dataset)
        .await
        .map_err(to_err)?
    {
        return Err("space was not found or is not visible".to_string());
    }
    let mut config = load_config(&state.paths)?;
    let endpoint = configured_endpoint(&config, Some(space.endpoint));
    let repo = RepoConfig {
        namespace: space.namespace.trim().to_string(),
        dataset: space.dataset.trim().to_string(),
        endpoint,
    };
    config.active_repo = Some(repo.clone());
    save_config(&state.paths, &config)?;
    let key = key_from_config(&config)?;
    let local_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
        .await
        .map_err(|error| format!("space is not initialized: {error}"))?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
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
    let local_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
        .await
        .map_err(to_err)?;
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    catalog
        .create_folder(&parent_node_id, &name, &key)
        .map_err(to_err)?;
    sync_current_catalog(&state.paths, &catalog, &key, &adapter, &repo).await?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
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
    let local_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
        .await
        .map_err(to_err)?;
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    catalog
        .rename_node(&node_id, &new_name, &key)
        .map_err(to_err)?;
    sync_current_catalog(&state.paths, &catalog, &key, &adapter, &repo).await?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
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
    KeyFile::generate_to_path(&path).map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    config.key_file_path = Some(path.clone());
    save_config(&state.paths, &config)?;
    Ok(path.display().to_string())
}

#[tauri::command]
fn import_key_file(state: tauri::State<'_, AppContext>, path: String) -> CommandResult<()> {
    let path = PathBuf::from(path);
    KeyFile::load_from_path(&path).map_err(to_err)?;
    let mut config = load_config(&state.paths)?;
    config.key_file_path = Some(path);
    save_config(&state.paths, &config)
}

#[tauri::command]
async fn load_remote_catalog(
    state: tauri::State<'_, AppContext>,
) -> CommandResult<CatalogLoadResult> {
    state.paths.ensure_dirs().map_err(to_err)?;
    let config = load_config(&state.paths)?;
    let key = key_from_config(&config)?;
    let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
    let local_path = state.paths.staging.join(CATALOG_FILE);
    adapter
        .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
        .await
        .map_err(to_err)?;
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let catalog = Catalog::from_staging(state.paths.staging.clone());
    let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
    Ok(CatalogLoadResult {
        local_path: local_path.display().to_string(),
        bytes,
        tree,
    })
}

#[tauri::command]
async fn enqueue_upload(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    path: String,
) -> CommandResult<TaskRecord> {
    run_upload(app, &state.paths, path, "upload".to_string()).await
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
        return Err("upload paths cannot be empty".to_string());
    }
    let mut task = TaskRecord::queued("upload", 0);
    insert_task(&state.paths, &task)?;
    update_task_state(&state.paths, task.id, TaskState::Running, None)?;
    task.state = TaskState::Running;
    emit_tasks(&app, &state.paths);
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let catalog_path = state.paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let catalog = Catalog::from_staging(state.paths.staging.clone());
        let upload_paths = paths.into_iter().map(PathBuf::from).collect::<Vec<_>>();
        update_task_phase(&state.paths, task.id, Some("preparing".to_string()))?;
        emit_tasks(&app, &state.paths);
        let preparing_started = Instant::now();
        let report = catalog
            .add_paths_to_folder_with_progress_and_report(
                &parent_node_id,
                &upload_paths,
                &conflict_resolutions,
                &key,
                PackOptions {
                    chunk_size: config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
                    staging_dir: state.paths.staging.clone(),
                },
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
        let work = plan_catalog_sync(&state.paths, &catalog, &key, &adapter, &repo).await?;
        update_task_phase(&state.paths, task.id, Some("uploading".to_string()))?;
        task.progress_done = 0;
        task.bytes_done = 0;
        task.progress_total = (work.upload.len() + work.delete.len()) as u64;
        let upload_total_bytes = work
            .upload
            .iter()
            .filter_map(|object| std::fs::metadata(&object.local_path).ok())
            .map(|metadata| metadata.len())
            .sum::<u64>();
        let uploading_started = Instant::now();
        update_task_transfer(
            &state.paths,
            task.id,
            task.progress_done,
            task.progress_total,
            task.bytes_done,
            upload_total_bytes,
            0,
        )?;
        emit_tasks(&app, &state.paths);
        for object in &work.upload {
            if let Some(state) = task_interrupt(&state.paths, task.id)? {
                return Ok::<Option<TaskState>, String>(Some(state));
            }
            adapter
                .upload_object(
                    &repo.namespace,
                    &repo.dataset,
                    &object.path,
                    &object.local_path,
                )
                .await
                .map_err(to_err)?;
            task.progress_done += 1;
            task.bytes_done += std::fs::metadata(&object.local_path)
                .map(|metadata| metadata.len())
                .unwrap_or(0);
            update_task_transfer(
                &state.paths,
                task.id,
                task.progress_done,
                task.progress_total,
                task.bytes_done,
                upload_total_bytes,
                average_speed_bps(task.bytes_done, uploading_started),
            )?;
            emit_tasks(&app, &state.paths);
        }
        if !work.delete.is_empty() {
            if let Some(state) = task_interrupt(&state.paths, task.id)? {
                return Ok::<Option<TaskState>, String>(Some(state));
            }
            adapter
                .delete_objects(&repo.namespace, &repo.dataset, &work.delete)
                .await
                .map_err(to_err)?;
            task.progress_done += work.delete.len() as u64;
            update_task_transfer(
                &state.paths,
                task.id,
                task.progress_done,
                task.progress_total,
                task.bytes_done,
                upload_total_bytes,
                average_speed_bps(task.bytes_done, uploading_started),
            )?;
            emit_tasks(&app, &state.paths);
        }
        Ok::<Option<TaskState>, String>(None)
    }
    .await;
    match result {
        Ok(None) => {
            task.state = TaskState::Completed;
            update_task_phase(&state.paths, task.id, None)?;
            update_task_state(&state.paths, task.id, TaskState::Completed, None)?;
            cleanup_after_task(&state.paths);
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Ok(Some(interrupt_state)) => {
            task.state = interrupt_state.clone();
            update_task_phase(&state.paths, task.id, None)?;
            update_task_state(&state.paths, task.id, interrupt_state, None)?;
            cleanup_after_task(&state.paths);
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = TaskState::Failed;
            task.error = Some(error.clone());
            update_task_phase(&state.paths, task.id, None)?;
            update_task_state(
                &state.paths,
                task.id,
                TaskState::Failed,
                Some(error.clone()),
            )?;
            cleanup_after_task(&state.paths);
            emit_tasks(&app, &state.paths);
            Err(error)
        }
    }
}

#[tauri::command]
async fn enqueue_replace(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    path: String,
) -> CommandResult<TaskRecord> {
    run_upload(app, &state.paths, path, "replace".to_string()).await
}

#[tauri::command]
async fn enqueue_delete(
    state: tauri::State<'_, AppContext>,
    prefix: String,
) -> CommandResult<TaskRecord> {
    if prefix.trim().is_empty() {
        return Err("delete prefix cannot be empty".to_string());
    }
    let mut task = TaskRecord::queued(format!("delete {prefix}"), 1);
    insert_task(&state.paths, &task)?;
    update_task_state(&state.paths, task.id, TaskState::Running, None)?;
    let result = async {
        let config = load_config(&state.paths)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        adapter
            .delete_prefix(&repo.namespace, &repo.dataset, &prefix)
            .await
            .map_err(to_err)?;
        Ok::<(), String>(())
    }
    .await;
    match result {
        Ok(()) => {
            task.state = TaskState::Completed;
            task.progress_done = 1;
            update_task_progress(&state.paths, task.id, 1, 1)?;
            update_task_state(&state.paths, task.id, TaskState::Completed, None)?;
            cleanup_after_task(&state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = TaskState::Failed;
            task.error = Some(error.clone());
            update_task_state(
                &state.paths,
                task.id,
                TaskState::Failed,
                Some(error.clone()),
            )?;
            cleanup_after_task(&state.paths);
            Err(error)
        }
    }
}

#[tauri::command]
async fn enqueue_delete_nodes(
    state: tauri::State<'_, AppContext>,
    node_ids: Vec<String>,
) -> CommandResult<TaskRecord> {
    if node_ids.is_empty() {
        return Err("delete selection cannot be empty".to_string());
    }
    let mut task = TaskRecord::queued("delete", 0);
    insert_task(&state.paths, &task)?;
    update_task_state(&state.paths, task.id, TaskState::Running, None)?;
    task.state = TaskState::Running;
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let catalog_path = state.paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let catalog = Catalog::from_staging(state.paths.staging.clone());
        catalog.delete_nodes(&node_ids, &key).map_err(to_err)?;
        let work = plan_catalog_sync(&state.paths, &catalog, &key, &adapter, &repo).await?;
        task.progress_total = (work.upload.len() + work.delete.len()) as u64;
        update_task_progress(
            &state.paths,
            task.id,
            task.progress_done,
            task.progress_total,
        )?;
        for object in &work.upload {
            adapter
                .upload_object(
                    &repo.namespace,
                    &repo.dataset,
                    &object.path,
                    &object.local_path,
                )
                .await
                .map_err(to_err)?;
            task.progress_done += 1;
            update_task_progress(
                &state.paths,
                task.id,
                task.progress_done,
                task.progress_total,
            )?;
        }
        if !work.delete.is_empty() {
            adapter
                .delete_objects(&repo.namespace, &repo.dataset, &work.delete)
                .await
                .map_err(to_err)?;
            task.progress_done += work.delete.len() as u64;
            update_task_progress(
                &state.paths,
                task.id,
                task.progress_done,
                task.progress_total,
            )?;
        }
        Ok::<(), String>(())
    }
    .await;
    match result {
        Ok(()) => {
            task.state = TaskState::Completed;
            update_task_state(&state.paths, task.id, TaskState::Completed, None)?;
            cleanup_after_task(&state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = TaskState::Failed;
            task.error = Some(error.clone());
            update_task_state(
                &state.paths,
                task.id,
                TaskState::Failed,
                Some(error.clone()),
            )?;
            cleanup_after_task(&state.paths);
            Err(error)
        }
    }
}

#[tauri::command]
async fn enqueue_restore(
    state: tauri::State<'_, AppContext>,
    output_dir: String,
    node_id: Option<String>,
) -> CommandResult<TaskRecord> {
    let mut task = TaskRecord::queued("restore", 1);
    insert_task(&state.paths, &task)?;
    update_task_state(&state.paths, task.id, TaskState::Running, None)?;
    task.state = TaskState::Running;
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let catalog_path = state.paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let catalog = Catalog::from_staging(state.paths.staging.clone());
        let selection = selection_from_node_id(node_id);
        let remote_files = catalog
            .remote_files_for_selection(&selection, &key)
            .map_err(to_err)?;
        task.progress_total = remote_files.len() as u64 + 1;
        update_task_progress(
            &state.paths,
            task.id,
            task.progress_done,
            task.progress_total,
        )?;
        for (index, file) in remote_files.iter().enumerate() {
            if let Some(state) = task_interrupt(&state.paths, task.id)? {
                return Ok::<Option<TaskState>, String>(Some(state));
            }
            let local_path = remote_to_staging_path(&state.paths.staging, &file.path)?;
            if !local_remote_file_is_valid(&local_path, file)? {
                adapter
                    .download_object(&repo.namespace, &repo.dataset, &file.path, &local_path)
                    .await
                    .map_err(to_err)?;
            }
            if !local_remote_file_is_valid(&local_path, file)? {
                return Err(format!(
                    "downloaded object failed hash verification: {}",
                    file.path
                ));
            }
            task.progress_done = (index + 1) as u64;
            update_task_progress(
                &state.paths,
                task.id,
                task.progress_done,
                task.progress_total,
            )?;
        }
        if let Some(state) = task_interrupt(&state.paths, task.id)? {
            return Ok::<Option<TaskState>, String>(Some(state));
        }
        catalog
            .restore(
                selection,
                &key,
                RestoreOptions {
                    output_dir: PathBuf::from(output_dir),
                    conflict_policy: RestoreConflictPolicy::Rename,
                },
            )
            .map_err(to_err)?;
        task.progress_done = task.progress_total;
        update_task_progress(
            &state.paths,
            task.id,
            task.progress_done,
            task.progress_total,
        )?;
        Ok::<Option<TaskState>, String>(None)
    }
    .await;
    match result {
        Ok(None) => {
            task.state = TaskState::Completed;
            update_task_state(&state.paths, task.id, TaskState::Completed, None)?;
            cleanup_after_task(&state.paths);
            Ok(task)
        }
        Ok(Some(interrupt_state)) => {
            task.state = interrupt_state.clone();
            update_task_state(&state.paths, task.id, interrupt_state, None)?;
            cleanup_after_task(&state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = TaskState::Failed;
            task.error = Some(error.clone());
            update_task_state(
                &state.paths,
                task.id,
                TaskState::Failed,
                Some(error.clone()),
            )?;
            cleanup_after_task(&state.paths);
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
    let mut task = TaskRecord::queued("download", 1);
    insert_task(&state.paths, &task)?;
    update_task_state(&state.paths, task.id, TaskState::Running, None)?;
    task.state = TaskState::Running;
    emit_tasks(&app, &state.paths);
    let result = async {
        let config = load_config(&state.paths)?;
        let key = key_from_config(&config)?;
        let (adapter, repo) = adapter_from_config(&state.paths, &config)?;
        let catalog_path = state.paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let catalog = Catalog::from_staging(state.paths.staging.clone());
        let selection = selection_from_node_ids(node_ids);
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
                return Ok::<Option<TaskState>, String>(Some(state));
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
                return Err(format!(
                    "downloaded object failed hash verification: {}",
                    file.path
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
            return Ok::<Option<TaskState>, String>(Some(state));
        }
        update_task_phase(&state.paths, task.id, Some("restoring".to_string()))?;
        emit_tasks(&app, &state.paths);
        catalog
            .restore(
                selection,
                &key,
                RestoreOptions {
                    output_dir: PathBuf::from(output_dir),
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
        Ok::<Option<TaskState>, String>(None)
    }
    .await;
    match result {
        Ok(None) => {
            task.state = TaskState::Completed;
            update_task_phase(&state.paths, task.id, None)?;
            update_task_state(&state.paths, task.id, TaskState::Completed, None)?;
            cleanup_after_task(&state.paths);
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Ok(Some(interrupt_state)) => {
            task.state = interrupt_state.clone();
            update_task_phase(&state.paths, task.id, None)?;
            update_task_state(&state.paths, task.id, interrupt_state, None)?;
            cleanup_after_task(&state.paths);
            emit_tasks(&app, &state.paths);
            Ok(task)
        }
        Err(error) => {
            task.state = TaskState::Failed;
            task.error = Some(error.clone());
            update_task_phase(&state.paths, task.id, None)?;
            update_task_state(
                &state.paths,
                task.id,
                TaskState::Failed,
                Some(error.clone()),
            )?;
            cleanup_after_task(&state.paths);
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
fn cleanup_local_cache(state: tauri::State<'_, AppContext>) -> CommandResult<CacheCleanupReport> {
    if task_store(&state.paths)?
        .list()
        .map_err(to_err)?
        .iter()
        .any(|task| {
            matches!(
                task.state,
                TaskState::Queued | TaskState::Running | TaskState::Paused
            )
        })
    {
        return Err("active tasks must finish before cleaning local cache".to_string());
    }
    cleanup_current_staging_cache(&state.paths, true, false)
}

#[tauri::command]
fn pause_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    let store = task_store(&state.paths)?;
    store
        .update_state(task_id, TaskState::Paused, None)
        .map_err(to_err)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[tauri::command]
fn resume_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    let store = task_store(&state.paths)?;
    store
        .update_state(task_id, TaskState::Queued, None)
        .map_err(to_err)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[tauri::command]
fn cancel_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    let store = task_store(&state.paths)?;
    store
        .update_state(task_id, TaskState::Canceled, None)
        .map_err(to_err)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[tauri::command]
fn clear_task(
    app: tauri::AppHandle,
    state: tauri::State<'_, AppContext>,
    task_id: Uuid,
) -> CommandResult<Vec<TaskRecord>> {
    let store = task_store(&state.paths)?;
    if store
        .list()
        .map_err(to_err)?
        .iter()
        .any(|task| task.id == task_id && task.state == TaskState::Running)
    {
        return Err("running task cannot be cleared".to_string());
    }
    store.delete(task_id).map_err(to_err)?;
    emit_tasks(&app, &state.paths);
    store.list().map_err(to_err)
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(AppContext::new())
        .invoke_handler(tauri::generate_handler![
            current_setup,
            setup_token,
            create_dataset_repo,
            connect_dataset_repo,
            list_dataset_repos,
            select_dataset_repo,
            initialize_space,
            load_space_catalog,
            preview_upload_conflicts,
            enqueue_upload_to_folder,
            enqueue_download,
            enqueue_delete_nodes,
            rename_node,
            create_folder,
            search_catalog,
            generate_key_file,
            import_key_file,
            load_remote_catalog,
            enqueue_upload,
            enqueue_replace,
            enqueue_delete,
            enqueue_restore,
            list_tasks,
            cleanup_local_cache,
            pause_task,
            resume_task,
            cancel_task,
            clear_task
        ])
        .run(tauri::generate_context!())
        .expect("failed to run Lios desktop app");
}
