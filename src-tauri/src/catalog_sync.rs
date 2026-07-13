use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use lios_core::catalog::{Catalog, CatalogSelection, CATALOG_FILE};
use lios_core::catalog_transaction::{
    execute_catalog_transaction, CatalogTransactionOutcome, CatalogTransactionProgress,
    CatalogTransactionSpec,
};
use lios_core::config::{LiosPaths, RepoConfig};
use lios_core::crypto::KeyFile;
use lios_core::modelscope::ModelScopeAdapter;
use lios_core::storage::{
    plan_catalog_sync_changes, CatalogSyncFile, CatalogSyncUpload, RepoRevision, StorageAdapter,
    StorageObject,
};
use lios_core::tasks::{CheckpointState, TaskCatalogCheckpoint, TaskObjectCheckpoint, TaskStore};
use uuid::Uuid;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::{remote_to_staging_path, sha256_hex_file, to_err, CommandResult};

pub(crate) struct SyncWork {
    pub(crate) upload: Vec<CatalogSyncUpload>,
    pub(crate) delete: Vec<String>,
    pub(crate) initial_remote_inventory: Vec<StorageObject>,
    pub(crate) prepublish_safe_paths: HashSet<String>,
    pub(crate) base_catalog_sha256: Option<String>,
    pub(crate) expected_revision: Option<RepoRevision>,
    pub(crate) probe_directory: PathBuf,
}

pub(crate) struct CatalogBaseline {
    pub(crate) catalog_sha256: Option<String>,
    pub(crate) referenced_paths: HashSet<String>,
    pub(crate) remote_objects: Vec<StorageObject>,
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
            expected_size: file.expected_size,
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

pub(crate) fn catalog_baseline_from_downloaded_catalog(
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

pub(crate) async fn download_catalog_baseline(
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

pub(crate) fn plan_catalog_sync_with_baseline(
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
        expected_revision: None,
        probe_directory,
    })
}

pub(crate) fn plan_catalog_sync(
    paths: &LiosPaths,
    catalog: &Catalog,
    key: &KeyFile,
    baseline: CatalogBaseline,
) -> CommandResult<SyncWork> {
    let desired = desired_catalog_objects(paths, catalog, key)?;
    plan_catalog_sync_with_baseline(desired, baseline, paths.staging.clone())
}

pub(crate) fn persist_sync_checkpoints(
    paths: &LiosPaths,
    task_id: Uuid,
    work: &SyncWork,
) -> CommandResult<()> {
    if work.upload.is_empty() && work.delete.is_empty() {
        return Ok(());
    }
    let catalog = work
        .upload
        .iter()
        .find(|upload| upload.path == CATALOG_FILE)
        .ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::CorruptedData,
                "catalog transaction has no target catalog checkpoint",
                false,
                None,
            )
        })?;
    let objects = work
        .upload
        .iter()
        .map(|upload| {
            let size = fs::metadata(&upload.local_path).map_err(to_err)?.len();
            if upload
                .expected_size
                .is_some_and(|expected| expected != size)
            {
                return Err(CommandError::new(
                    CommandErrorCode::CorruptedData,
                    format!(
                        "catalog transaction object changed before checkpointing: {}",
                        upload.path
                    ),
                    false,
                    None,
                ));
            }
            Ok(TaskObjectCheckpoint {
                task_id,
                remote_path: upload.path.clone(),
                oid: upload.expected_sha256.clone(),
                size,
                state: CheckpointState::Pending,
            })
        })
        .collect::<CommandResult<Vec<_>>>()?;
    let checkpoint = TaskCatalogCheckpoint {
        task_id,
        base_catalog_sha256: work.base_catalog_sha256.clone(),
        target_catalog_sha256: catalog.expected_sha256.clone(),
    };
    TaskStore::open(&paths.database)
        .map_err(to_err)?
        .replace_transaction_checkpoints(&checkpoint, &objects)
        .map_err(to_err)
}

pub(crate) async fn execute_sync_work<A, C, P>(
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
            expected_revision: work.expected_revision,
            probe_directory: work.probe_directory,
        },
        should_cancel,
        on_progress,
    )
    .await
    .map_err(to_err)
}

pub(crate) async fn sync_current_catalog(
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
