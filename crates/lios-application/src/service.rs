use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use lios_core::catalog::{
    Catalog, CatalogTreeNode, ConflictResolution, DriveItem, UploadConflict, CATALOG_FILE,
};
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
use lios_core::credentials::{protect_to_file, unprotect_from_file};
use lios_core::crypto::KeyFile;
use lios_core::modelscope::{DatasetRepoSummary, ModelScopeAdapter, ModelScopeUserSummary};
use lios_core::storage::StorageAdapter;
use uuid::Uuid;

use crate::catalog_mutation_gate::CatalogMutationGate;
use crate::catalog_probe::{ensure_space_can_initialize, map_catalog_load_error};
use crate::catalog_sync::{download_catalog_baseline, sync_current_catalog, CatalogBaseline};
use crate::config_mutation_gate::ConfigMutationGate;
use crate::production_config::{
    configured_endpoint, prepare_startup_config, validate_repo, SetupWarning,
};
use crate::recovery_key_service::{
    export_recovery_key_for_paths, import_recovery_key_for_paths, verify_recovery_key_for_paths,
    RecoveryKeyVerification,
};
use crate::recovery_key_service::{recovery_key_status, RecoveryKeyStatus};
use crate::task_manager::{TaskManager, TaskScope};
use crate::{to_err, CommandError, CommandResult};

#[derive(Debug, Clone)]
pub struct SetupSnapshot {
    pub paths: LiosPaths,
    pub initialized: bool,
    pub config: LiosConfig,
    pub recovery_key: RecoveryKeyStatus,
    pub has_token: bool,
    pub warning: Option<SetupWarning>,
}

#[derive(Debug, Clone)]
pub struct CatalogSnapshot {
    pub local_path: PathBuf,
    pub bytes: u64,
    pub tree: CatalogTreeNode,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct DatasetRepoList {
    pub user: ModelScopeUserSummary,
    pub repositories: Vec<DatasetRepoSummary>,
}

#[derive(Clone)]
pub struct Application {
    pub(crate) paths: LiosPaths,
    read_staging: Arc<tempfile::TempDir>,
    pub(crate) config_gate: Arc<ConfigMutationGate>,
    pub(crate) catalog_gate: Arc<CatalogMutationGate>,
    pub(crate) task_manager: TaskManager,
}

impl Application {
    pub fn new(paths: LiosPaths) -> CommandResult<Self> {
        paths.ensure_dirs().map_err(to_err)?;
        Self::new_without_initializing(paths)
    }

    pub fn new_without_initializing(paths: LiosPaths) -> CommandResult<Self> {
        let read_staging = tempfile::Builder::new()
            .prefix("lios-catalog-read-")
            .tempdir()
            .map_err(to_err)?;
        Ok(Self {
            paths,
            read_staging: Arc::new(read_staging),
            config_gate: Arc::new(ConfigMutationGate::default()),
            catalog_gate: Arc::new(CatalogMutationGate::default()),
            task_manager: TaskManager::default(),
        })
    }

    pub fn default_user() -> CommandResult<Self> {
        Self::new(LiosPaths::default_user())
    }

    pub fn paths(&self) -> &LiosPaths {
        &self.paths
    }

    pub fn task_manager(&self) -> &TaskManager {
        &self.task_manager
    }

    pub fn setup(&self) -> CommandResult<SetupSnapshot> {
        self.paths.ensure_dirs().map_err(to_err)?;
        let (config, warning) = {
            let _guard = self.config_gate.lock()?;
            let _process_guard = self.paths.try_lock_config().map_err(CommandError::from)?;
            let mut config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
            let warning = prepare_startup_config(&self.paths, &mut config)?;
            (config, warning)
        };
        Ok(self.setup_snapshot(config, true, warning))
    }

    pub fn inspect_setup(&self) -> CommandResult<SetupSnapshot> {
        let initialized = self.paths.config.is_file();
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        Ok(self.setup_snapshot(config, initialized, None))
    }

    fn setup_snapshot(
        &self,
        config: LiosConfig,
        initialized: bool,
        warning: Option<SetupWarning>,
    ) -> SetupSnapshot {
        SetupSnapshot {
            paths: self.paths.clone(),
            initialized,
            recovery_key: recovery_key_status(&config),
            config,
            has_token: self.paths.credentials.is_file(),
            warning,
        }
    }

    pub fn set_token(&self, token: &str) -> CommandResult<()> {
        let token = token.trim();
        if token.is_empty() {
            return Err(CommandError::invalid_input(
                "ModelScope token cannot be empty",
            ));
        }
        self.paths.ensure_dirs().map_err(to_err)?;
        protect_to_file(token, &self.paths.credentials).map_err(to_err)
    }

    pub async fn list_dataset_repos(
        &self,
        endpoint: Option<String>,
    ) -> CommandResult<DatasetRepoList> {
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        let endpoint = configured_endpoint(&config, endpoint)?;
        let adapter = ModelScopeAdapter::new(endpoint, self.read_token()?);
        let user = adapter.whoami().await.map_err(to_err)?;
        let repositories = adapter
            .list_dataset_repos_for_owner(Some(&user.username))
            .await
            .map_err(to_err)?;
        Ok(DatasetRepoList { user, repositories })
    }

    pub async fn create_dataset_repo(&self, repo: RepoConfig) -> CommandResult<()> {
        let repo = validate_repo(repo)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        adapter
            .create_repo(&repo.namespace, &repo.dataset)
            .await
            .map_err(to_err)?;
        Ok(())
    }

    pub async fn initialize_space(&self, repo: RepoConfig) -> CommandResult<CatalogSnapshot> {
        let repo = validate_repo(repo)?;
        let scope = TaskScope::from_repo(&repo);
        let _process_space_lock = self
            .paths
            .try_lock_space(&scope.space_id)
            .map_err(CommandError::from)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        if !adapter
            .repo_exists(&repo.namespace, &repo.dataset)
            .await
            .map_err(to_err)?
        {
            return Err(CommandError::invalid_input(
                "space was not found or is not visible",
            ));
        }
        let _space_guard = self.task_manager.acquire_space(scope.space_id).await;
        let _catalog_guard = self.catalog_gate.lock_mutation().await;
        ensure_space_can_initialize(
            &adapter,
            &repo.namespace,
            &repo.dataset,
            &self.paths.staging,
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
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        reset_staging(&self.paths)?;
        let catalog = Catalog::initialize_empty(&repo.dataset, &key, self.paths.staging.clone())
            .map_err(to_err)?;
        let warnings =
            sync_current_catalog(&self.paths, &catalog, &key, &adapter, &repo, baseline).await?;
        snapshot_from_catalog(&catalog, &key, warnings)
    }

    pub async fn open_space(&self, repo: RepoConfig) -> CommandResult<CatalogSnapshot> {
        let repo = validate_repo(repo)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        self.open_space_with_adapter(repo, &adapter).await
    }

    async fn open_space_with_adapter(
        &self,
        repo: RepoConfig,
        adapter: &(impl StorageAdapter + ?Sized),
    ) -> CommandResult<CatalogSnapshot> {
        if !adapter
            .repo_exists(&repo.namespace, &repo.dataset)
            .await
            .map_err(to_err)?
        {
            return Err(CommandError::invalid_input(
                "space was not found or is not visible",
            ));
        }
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let staging = self.fresh_read_staging()?;
        let local_path = staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
            .await
            .map_err(map_catalog_load_error)?;
        let snapshot = snapshot_from_catalog(&Catalog::from_staging(staging), &key, Vec::new())?;
        Ok(snapshot)
    }

    pub fn recovery_key_status(&self) -> CommandResult<RecoveryKeyStatus> {
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        Ok(recovery_key_status(&config))
    }

    pub fn backup_recovery_key(&self, destination: &Path) -> CommandResult<RecoveryKeyStatus> {
        export_recovery_key_for_paths(&self.paths, &self.config_gate, destination)
    }

    pub async fn verify_recovery_key(
        &self,
        candidate: &Path,
    ) -> CommandResult<RecoveryKeyVerification> {
        verify_recovery_key_for_paths(&self.paths, candidate).await
    }

    pub async fn import_recovery_key(
        &self,
        candidate: &Path,
    ) -> CommandResult<RecoveryKeyVerification> {
        import_recovery_key_for_paths(&self.paths, &self.config_gate, candidate).await
    }

    pub async fn list_children_in(
        &self,
        repo: RepoConfig,
        parent_node_id: &str,
    ) -> CommandResult<Vec<DriveItem>> {
        let (catalog, key) = self.download_catalog_for(repo).await?;
        catalog.list_children(parent_node_id, &key).map_err(to_err)
    }

    pub async fn search_in(&self, repo: RepoConfig, query: &str) -> CommandResult<Vec<DriveItem>> {
        let (catalog, key) = self.download_catalog_for(repo).await?;
        catalog.search(query, &key).map_err(to_err)
    }

    pub async fn preview_upload_conflicts_in(
        &self,
        repo: RepoConfig,
        parent_node_id: &str,
        paths: &[PathBuf],
    ) -> CommandResult<Vec<UploadConflict>> {
        let (catalog, key) = self.download_catalog_for(repo).await?;
        catalog
            .preview_upload_conflicts(parent_node_id, paths, &key)
            .map_err(to_err)
    }

    pub async fn create_folder_in(
        &self,
        repo: RepoConfig,
        parent_node_id: &str,
        name: &str,
    ) -> CommandResult<CatalogSnapshot> {
        self.mutate_space_catalog(repo, |catalog, key| {
            catalog
                .create_folder(parent_node_id, name, key)
                .map_err(to_err)
        })
        .await
    }

    pub async fn move_node_in(
        &self,
        repo: RepoConfig,
        node_id: &str,
        new_parent_node_id: &str,
        new_name: &str,
        replace_node_id: Option<&str>,
    ) -> CommandResult<CatalogSnapshot> {
        self.mutate_space_catalog(repo, |catalog, key| {
            if let Some(replace_node_id) = replace_node_id {
                catalog
                    .delete_nodes(&[replace_node_id.to_string()], key)
                    .map_err(to_err)?;
            }
            catalog
                .move_node(node_id, new_parent_node_id, new_name, key)
                .map_err(to_err)
        })
        .await
    }

    async fn mutate_space_catalog(
        &self,
        repo: RepoConfig,
        mutation: impl FnOnce(&Catalog, &KeyFile) -> CommandResult<()>,
    ) -> CommandResult<CatalogSnapshot> {
        let repo = validate_repo(repo)?;
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let scope = TaskScope::from_repo(&repo);
        let _process_space_lock = self
            .paths
            .try_lock_space(&scope.space_id)
            .map_err(CommandError::from)?;
        let _space_guard = self.task_manager.acquire_space(scope.space_id).await;
        let _catalog_guard = self.catalog_gate.lock_mutation().await;
        let (catalog, baseline) =
            download_catalog_baseline(&self.paths, &key, &adapter, &repo).await?;
        mutation(&catalog, &key)?;
        let warnings =
            sync_current_catalog(&self.paths, &catalog, &key, &adapter, &repo, baseline).await?;
        snapshot_from_catalog(&catalog, &key, warnings)
    }

    pub async fn rename_node_in(
        &self,
        repo: RepoConfig,
        node_id: &str,
        new_name: &str,
    ) -> CommandResult<CatalogSnapshot> {
        self.mutate_space_catalog(repo, |catalog, key| {
            catalog.rename_node(node_id, new_name, key).map_err(to_err)
        })
        .await
    }

    pub fn normalize_conflict_resolutions(
        &self,
        paths: &mut Vec<PathBuf>,
        resolutions: &mut Vec<ConflictResolution>,
    ) {
        use lios_core::catalog::ConflictAction;

        let skipped = resolutions
            .iter()
            .filter(|resolution| resolution.action == ConflictAction::Skip)
            .map(|resolution| resolution.source_path.clone())
            .collect::<HashSet<_>>();
        paths.retain(|path| !skipped.contains(path.to_string_lossy().as_ref()));
        resolutions.retain(|resolution| resolution.action != ConflictAction::Skip);
    }

    pub(crate) fn read_token(&self) -> CommandResult<String> {
        unprotect_from_file(&self.paths.credentials).map_err(|_| {
            CommandError::invalid_input("ModelScope token is not configured or cannot be read")
        })
    }

    async fn download_catalog_for(&self, repo: RepoConfig) -> CommandResult<(Catalog, KeyFile)> {
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        let repo = validate_repo(repo)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let staging = self.fresh_read_staging()?;
        let local_path = staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &local_path)
            .await
            .map_err(map_catalog_load_error)?;
        Ok((Catalog::from_staging(staging), key))
    }

    fn fresh_read_staging(&self) -> CommandResult<PathBuf> {
        let path = self
            .read_staging
            .path()
            .join(Uuid::new_v4().simple().to_string());
        fs::create_dir(&path).map_err(to_err)?;
        Ok(path)
    }
}

pub(crate) fn key_from_config(config: &LiosConfig) -> CommandResult<KeyFile> {
    let path = config
        .key_file_path
        .as_ref()
        .ok_or_else(|| CommandError::invalid_input("recovery key is not configured"))?;
    KeyFile::load_from_path(path).map_err(to_err)
}

fn reset_staging(paths: &LiosPaths) -> CommandResult<()> {
    paths.ensure_dirs().map_err(to_err)?;
    if paths.staging.exists() {
        let staging = paths.staging.canonicalize().map_err(to_err)?;
        let home = paths.home.canonicalize().map_err(to_err)?;
        if !staging.starts_with(home) {
            return Err(CommandError::invalid_input(
                "refusing to clear staging outside the Lios state directory",
            ));
        }
        fs::remove_dir_all(&paths.staging).map_err(to_err)?;
    }
    fs::create_dir_all(&paths.staging).map_err(to_err)
}

fn snapshot_from_catalog(
    catalog: &Catalog,
    key: &KeyFile,
    warnings: Vec<String>,
) -> CommandResult<CatalogSnapshot> {
    let local_path = catalog.encrypted_catalog_path().to_path_buf();
    let bytes = fs::metadata(&local_path).map_err(to_err)?.len();
    let tree = catalog.decrypt_tree(key).map_err(to_err)?;
    Ok(CatalogSnapshot {
        local_path,
        bytes,
        tree,
        warnings,
    })
}

pub fn existing_absolute_paths(
    values: impl IntoIterator<Item = PathBuf>,
) -> CommandResult<Vec<PathBuf>> {
    let values = values.into_iter().collect::<Vec<_>>();
    if values.is_empty()
        || values
            .iter()
            .any(|path| !path.is_absolute() || !path.exists())
    {
        return Err(CommandError::invalid_input(
            "upload paths must be existing absolute paths",
        ));
    }
    Ok(values)
}

pub fn existing_absolute_directory(path: &Path) -> CommandResult<PathBuf> {
    if !path.is_absolute() || !path.is_dir() {
        return Err(CommandError::invalid_input(
            "download output must be an existing absolute directory",
        ));
    }
    path.canonicalize().map_err(to_err)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use async_trait::async_trait;
    use lios_core::catalog::Catalog;
    use lios_core::config::{LiosConfig, LiosPaths, RepoConfig, MODELSCOPE_ENDPOINT};
    use lios_core::crypto::KeyFile;
    use lios_core::storage::{StorageAdapter, StorageObject};
    use lios_core::Result;
    use tempfile::tempdir;

    use super::Application;

    struct CatalogFromDifferentKeyAdapter {
        catalog: Vec<u8>,
    }

    #[async_trait]
    impl StorageAdapter for CatalogFromDifferentKeyAdapter {
        async fn create_repo(&self, _namespace: &str, _dataset: &str) -> Result<()> {
            unreachable!()
        }

        async fn repo_exists(&self, _namespace: &str, _dataset: &str) -> Result<bool> {
            Ok(true)
        }

        async fn list_objects(
            &self,
            _namespace: &str,
            _dataset: &str,
            _prefix: &str,
        ) -> Result<Vec<StorageObject>> {
            unreachable!()
        }

        async fn download_object(
            &self,
            _namespace: &str,
            _dataset: &str,
            _remote_path: &str,
            local_path: &Path,
        ) -> Result<()> {
            tokio::fs::write(local_path, &self.catalog).await?;
            Ok(())
        }
    }

    fn repo(namespace: &str, dataset: &str) -> RepoConfig {
        RepoConfig {
            namespace: namespace.to_string(),
            dataset: dataset.to_string(),
            endpoint: MODELSCOPE_ENDPOINT.to_string(),
        }
    }

    #[tokio::test]
    async fn failed_space_open_does_not_mutate_the_space_registry() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let application = Application::new(paths.clone()).unwrap();
        application.setup().unwrap();
        let mut config = LiosConfig::load(&paths.config).unwrap();
        config
            .spaces
            .insert("previous".to_string(), repo("owner", "previous"));
        config.save(&paths.config).unwrap();
        let before = fs::read(&paths.config).unwrap();
        let other_home = tempdir().unwrap();
        let other_key = KeyFile::generate_to_path(other_home.path().join("recovery.key")).unwrap();
        let other_catalog =
            Catalog::initialize_empty("target", &other_key, other_home.path().join("staging"))
                .unwrap();
        let adapter = CatalogFromDifferentKeyAdapter {
            catalog: fs::read(other_catalog.encrypted_catalog_path()).unwrap(),
        };

        application
            .open_space_with_adapter(repo("owner", "target"), &adapter)
            .await
            .unwrap_err();

        assert_eq!(fs::read(&paths.config).unwrap(), before);
    }
}
