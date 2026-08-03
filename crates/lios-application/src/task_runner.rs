use std::collections::HashMap;
use std::path::PathBuf;

use lios_core::catalog::{
    Catalog, CatalogIntegrityReport, CatalogRemoteIntegrityReport, CatalogSelection,
    CatalogTreeNode, CatalogTreeNodeKind, ConflictAction, ConflictResolution, SourceFileSnapshot,
    CATALOG_FILE,
};
use lios_core::catalog_transaction::{
    probe_catalog_sha256, CatalogBlobCheckpointState, CatalogTransactionOutcome,
    CatalogTransactionPhase, CatalogTransactionProgress,
};
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
use lios_core::modelscope::ModelScopeAdapter;
use lios_core::pack::PackOptions;
use lios_core::restore::{RestoreConflictPolicy, RestoreOptions};
use lios_core::storage::StorageAdapter;
use lios_core::tasks::{
    CheckpointState, PersistedTransferAction, PersistedTransferPlan, TaskItem, TaskItemState,
    TaskObjectCheckpoint, TaskRecord, TaskSpec, TaskState, TaskStore, TaskSummary,
    TransferActionKind, TransferDirection, TransferEntryKind,
};
use uuid::Uuid;

use crate::catalog_sync::{
    download_catalog_baseline, execute_sync_work, persist_sync_checkpoints, plan_catalog_sync,
};
use crate::service::{
    existing_absolute_directory, existing_absolute_paths, key_from_config, Application,
};
use crate::task_manager::{
    apply_pack_progress, persist_submission, persist_transfer_submission, reconcile_catalog_hash,
    snapshot_upload_sources, validate_task_sources, CatalogReconcileDecision, TaskScope,
};
use crate::transfer_request::fingerprint_path;
use crate::{remote_to_staging_path, to_err, CommandError, CommandErrorCode, CommandResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundProgress {
    pub task_id: Uuid,
    pub phase: String,
    pub completed: u64,
    pub total: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
}

#[derive(Debug, Clone)]
pub struct TaskRunResult {
    pub summary: TaskSummary,
    pub notices: Vec<String>,
}

impl Application {
    pub fn list_tasks(&self) -> CommandResult<Vec<TaskSummary>> {
        TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .list_summaries()
            .map_err(to_err)
    }

    pub fn get_task(&self, task_id: Uuid) -> CommandResult<Option<TaskSummary>> {
        TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .get_summary(task_id)
            .map_err(to_err)
    }

    pub fn queue_upload_for(
        &self,
        repo: RepoConfig,
        parent_node_id: String,
        source_paths: Vec<PathBuf>,
        mut conflict_resolutions: Vec<ConflictResolution>,
    ) -> CommandResult<TaskSummary> {
        let mut source_paths = existing_absolute_paths(source_paths)?;
        self.normalize_conflict_resolutions(&mut source_paths, &mut conflict_resolutions);
        if source_paths.is_empty() {
            return Err(CommandError::invalid_input(
                "all selected upload paths were skipped",
            ));
        }
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        key_from_config(&config)?;
        let scope = TaskScope::from_repo(&repo);
        let source_snapshot = snapshot_upload_sources(&source_paths).map_err(to_err)?;
        let spec = TaskSpec::Upload {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            parent_node_id,
            source_paths,
            source_snapshot: Some(source_snapshot.clone()),
            chunk_size: config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
            conflict_resolutions,
        };
        let task =
            persist_submission(&self.paths, &spec, &source_snapshot.files).map_err(to_err)?;
        summary_for(&self.paths, task.id)
    }

    pub fn queue_delete_for(
        &self,
        repo: RepoConfig,
        node_ids: Vec<String>,
    ) -> CommandResult<TaskSummary> {
        let node_ids = clean_ids(node_ids, "delete selection cannot be empty")?;
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::Delete {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            node_ids,
        };
        let task = persist_submission(&self.paths, &spec, &[]).map_err(to_err)?;
        summary_for(&self.paths, task.id)
    }

    pub fn queue_download_for(
        &self,
        repo: RepoConfig,
        node_ids: Vec<String>,
        output_dir: PathBuf,
    ) -> CommandResult<TaskSummary> {
        let node_ids = clean_ids(node_ids, "download selection cannot be empty")?;
        let output_dir = existing_absolute_directory(&output_dir)?;
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::Download {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            node_ids,
            output_dir,
        };
        let task = persist_submission(&self.paths, &spec, &[]).map_err(to_err)?;
        summary_for(&self.paths, task.id)
    }

    pub fn queue_verify_for(&self, repo: RepoConfig, full: bool) -> CommandResult<TaskSummary> {
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        key_from_config(&config)?;
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::VerifySpace {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            full,
        };
        let task = persist_submission(&self.paths, &spec, &[]).map_err(to_err)?;
        summary_for(&self.paths, task.id)
    }

    pub fn queue_copy(
        &self,
        repo: RepoConfig,
        plan: PersistedTransferPlan,
    ) -> CommandResult<TaskSummary> {
        self.queue_transfer(repo, plan, false)
    }

    pub fn queue_sync(
        &self,
        repo: RepoConfig,
        plan: PersistedTransferPlan,
    ) -> CommandResult<TaskSummary> {
        self.queue_transfer(repo, plan, true)
    }

    fn queue_transfer(
        &self,
        repo: RepoConfig,
        plan: PersistedTransferPlan,
        sync: bool,
    ) -> CommandResult<TaskSummary> {
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        key_from_config(&config)?;
        let scope = TaskScope::from_repo(&repo);
        let actions = plan.actions.clone();
        let spec = if sync {
            TaskSpec::Sync {
                account_id: scope.account_id,
                space_id: scope.space_id,
                repo,
                plan,
            }
        } else {
            TaskSpec::Copy {
                account_id: scope.account_id,
                space_id: scope.space_id,
                repo,
                plan,
            }
        };
        let task = persist_transfer_submission(&self.paths, &spec, &actions).map_err(to_err)?;
        summary_for(&self.paths, task.id)
    }

    pub async fn run_task<F>(
        &self,
        task_id: Uuid,
        mut on_progress: F,
    ) -> CommandResult<TaskRunResult>
    where
        F: FnMut(ForegroundProgress),
    {
        let preview_spec = TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .load_spec(task_id)
            .map_err(to_err)?
            .ok_or_else(|| CommandError::invalid_input("task has no resumable specification"))?;
        let space_id = preview_spec.space_id().to_string();
        let _execution_permit = self
            .task_manager
            .acquire(space_id.clone())
            .await
            .map_err(|_| CommandError::invalid_input("task manager is shutting down"))?;
        let _process_lock = self
            .paths
            .try_lock_space(&space_id)
            .map_err(CommandError::from)?;
        let _catalog_guard = self.catalog_gate.lock_mutation().await;

        if let Some(result) = self
            .prepare_task_for_run(task_id, &preview_spec, &mut on_progress)
            .await?
        {
            return Ok(result);
        }

        let spec = TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .claim_queued(task_id)
            .map_err(to_err)?
            .ok_or_else(|| CommandError::invalid_input("task is not queued"))?;
        if spec.space_id() != space_id {
            return Err(CommandError::new(
                CommandErrorCode::CorruptedData,
                "task space changed before execution",
                false,
                None,
            ));
        }
        let task_paths = self
            .paths
            .for_task(spec.account_id(), spec.space_id(), task_id)
            .map_err(to_err)?;
        task_paths.ensure_dirs().map_err(to_err)?;
        let mut task = TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .get(task_id)
            .map_err(to_err)?
            .ok_or_else(|| CommandError::invalid_input("claimed task disappeared"))?;
        TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .set_transaction_state(task_id, TaskState::Running)
            .map_err(to_err)?;
        let result = self
            .execute_task_spec(&task_paths, &mut task, spec, &mut on_progress)
            .await;
        let store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        match result {
            Ok(notices) => {
                store
                    .update_state(
                        task_id,
                        TaskState::Completed,
                        (!notices.is_empty()).then(|| notices.join("; ")),
                    )
                    .map_err(to_err)?;
                store.complete_active_items(task_id).map_err(to_err)?;
                Ok(TaskRunResult {
                    summary: summary_for(&self.paths, task_id)?,
                    notices,
                })
            }
            Err(error) => {
                let persisted = store.get_summary(task_id).map_err(to_err)?;
                let state = if persisted
                    .as_ref()
                    .is_some_and(|summary| summary.state == TaskState::Committing)
                {
                    TaskState::Committing
                } else {
                    TaskState::Failed
                };
                store
                    .update_state(task_id, state, Some(error.message.clone()))
                    .map_err(to_err)?;
                Err(error)
            }
        }
    }

    pub async fn resume_task<F>(
        &self,
        task_id: Uuid,
        on_progress: F,
    ) -> CommandResult<TaskRunResult>
    where
        F: FnMut(ForegroundProgress),
    {
        self.requeue_paused_task(task_id)?;
        self.run_task(task_id, on_progress).await
    }

    pub fn requeue_paused_task(&self, task_id: Uuid) -> CommandResult<TaskSummary> {
        let store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        let summary = store
            .get_summary(task_id)
            .map_err(to_err)?
            .ok_or_else(|| CommandError::invalid_input("task was not found"))?;
        if summary.state != TaskState::Paused {
            return Err(CommandError::invalid_input(
                "only paused or interrupted tasks can resume",
            ));
        }
        if !store
            .transition_state(task_id, TaskState::Paused, TaskState::Queued)
            .map_err(to_err)?
        {
            return Err(CommandError::new(
                CommandErrorCode::RemoteConflict,
                "task state changed before resume",
                true,
                None,
            ));
        }
        summary_for(&self.paths, task_id)
    }

    pub async fn retry_task<F>(&self, task_id: Uuid, on_progress: F) -> CommandResult<TaskRunResult>
    where
        F: FnMut(ForegroundProgress),
    {
        self.requeue_failed_task(task_id)?;
        self.run_task(task_id, on_progress).await
    }

    pub fn requeue_failed_task(&self, task_id: Uuid) -> CommandResult<TaskSummary> {
        let mut store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        if !store.requeue_failed(task_id).map_err(to_err)? {
            return Err(CommandError::invalid_input(
                "only failed tasks with a saved specification can retry",
            ));
        }
        summary_for(&self.paths, task_id)
    }

    pub async fn pause_task(&self, task_id: Uuid) -> CommandResult<TaskSummary> {
        let store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        if !store
            .interrupt_task(task_id, TaskState::Paused)
            .map_err(to_err)?
        {
            return Err(CommandError::invalid_input(
                "only queued or running tasks can pause",
            ));
        }
        self.task_manager.cancel(task_id).await;
        summary_for(&self.paths, task_id)
    }

    pub async fn cancel_task(&self, task_id: Uuid) -> CommandResult<TaskSummary> {
        let store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        if !store
            .interrupt_task(task_id, TaskState::Canceled)
            .map_err(to_err)?
        {
            return Err(CommandError::invalid_input(
                "only non-terminal tasks can be canceled",
            ));
        }
        self.task_manager.cancel(task_id).await;
        summary_for(&self.paths, task_id)
    }

    pub fn clear_task(&self, task_id: Uuid) -> CommandResult<()> {
        let store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        let summary = store
            .get_summary(task_id)
            .map_err(to_err)?
            .ok_or_else(|| CommandError::invalid_input("task was not found"))?;
        if !matches!(
            summary.state,
            TaskState::Failed | TaskState::Completed | TaskState::Canceled
        ) {
            return Err(CommandError::invalid_input(
                "only terminal task records can be cleared",
            ));
        }
        store.delete(task_id).map_err(to_err)
    }

    async fn prepare_task_for_run<F>(
        &self,
        task_id: Uuid,
        spec: &TaskSpec,
        on_progress: &mut F,
    ) -> CommandResult<Option<TaskRunResult>>
    where
        F: FnMut(ForegroundProgress),
    {
        let summary = summary_for(&self.paths, task_id)?;
        let mut store = TaskStore::open(&self.paths.database).map_err(to_err)?;
        match summary.state {
            TaskState::Queued => Ok(None),
            TaskState::Paused => {
                if !store
                    .transition_state(task_id, TaskState::Paused, TaskState::Queued)
                    .map_err(to_err)?
                {
                    return Err(CommandError::invalid_input(
                        "paused task could not be resumed",
                    ));
                }
                Ok(None)
            }
            TaskState::Failed => {
                if !store.requeue_failed(task_id).map_err(to_err)? {
                    return Err(CommandError::invalid_input("failed task cannot be retried"));
                }
                Ok(None)
            }
            TaskState::Preparing | TaskState::Running | TaskState::Retrying => {
                if !store.requeue_interrupted(task_id).map_err(to_err)? {
                    return Err(CommandError::invalid_input(
                        "interrupted task could not be resumed",
                    ));
                }
                Ok(None)
            }
            TaskState::Committing => {
                let decision = self.reconcile_committing_task(task_id, spec).await?;
                match decision {
                    CatalogReconcileDecision::Committed => {
                        store.complete_reconciled_commit(task_id).map_err(to_err)?;
                        on_progress(progress_from_summary(summary_for(&self.paths, task_id)?));
                        Ok(Some(TaskRunResult {
                            summary: summary_for(&self.paths, task_id)?,
                            notices: vec![
                                "the remote catalog confirms the interrupted commit completed"
                                    .to_string(),
                            ],
                        }))
                    }
                    CatalogReconcileDecision::Replay => {
                        if !store.requeue_committing(task_id).map_err(to_err)? {
                            return Err(CommandError::invalid_input(
                                "interrupted commit could not be replayed",
                            ));
                        }
                        Ok(None)
                    }
                    CatalogReconcileDecision::Conflict => {
                        store
                            .fail_reconciled_commit(task_id, "remote catalog changed")
                            .map_err(to_err)?;
                        Err(CommandError::new(
                            CommandErrorCode::RemoteConflict,
                            "remote catalog changed while the task was interrupted",
                            false,
                            None,
                        ))
                    }
                }
            }
            TaskState::Completed => Ok(Some(TaskRunResult {
                summary,
                notices: Vec::new(),
            })),
            TaskState::Canceled => Err(CommandError::invalid_input(
                "canceled task cannot be resumed",
            )),
        }
    }

    async fn reconcile_committing_task(
        &self,
        task_id: Uuid,
        spec: &TaskSpec,
    ) -> CommandResult<CatalogReconcileDecision> {
        let repo = task_repo(spec);
        let checkpoint = TaskStore::open(&self.paths.database)
            .map_err(to_err)?
            .load_catalog_checkpoint(task_id)
            .map_err(to_err)?
            .ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "committing task has no catalog checkpoint",
                    false,
                    None,
                )
            })?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let task_paths = self
            .paths
            .for_task(spec.account_id(), spec.space_id(), task_id)
            .map_err(to_err)?;
        task_paths.ensure_dirs().map_err(to_err)?;
        let probe_path = task_paths.staging.join("catalog-reconcile.enc");
        let remote = probe_catalog_sha256(&adapter, &repo.namespace, &repo.dataset, &probe_path)
            .await
            .map_err(to_err)?;
        Ok(reconcile_catalog_hash(&checkpoint, remote.as_deref()))
    }

    async fn execute_task_spec<F>(
        &self,
        task_paths: &LiosPaths,
        task: &mut TaskRecord,
        spec: TaskSpec,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        match spec {
            TaskSpec::Copy { repo, plan, .. } | TaskSpec::Sync { repo, plan, .. } => {
                self.run_persisted_transfer(task_paths, task, repo, plan, on_progress)
                    .await
            }
            TaskSpec::Upload {
                repo,
                parent_node_id,
                source_paths,
                source_snapshot,
                chunk_size,
                conflict_resolutions,
                ..
            } => {
                let snapshot = source_snapshot.ok_or_else(|| {
                    CommandError::invalid_input("upload task has no saved source snapshot")
                })?;
                self.run_upload(
                    task_paths,
                    task,
                    repo,
                    parent_node_id,
                    source_paths,
                    snapshot.files,
                    chunk_size,
                    conflict_resolutions,
                    on_progress,
                )
                .await
            }
            TaskSpec::Delete { repo, node_ids, .. } => {
                self.run_delete(task_paths, task, repo, node_ids, on_progress)
                    .await
            }
            TaskSpec::Download {
                repo,
                node_ids,
                output_dir,
                ..
            } => {
                self.run_download(task_paths, task, repo, node_ids, output_dir, on_progress)
                    .await
            }
            TaskSpec::VerifySpace { repo, full, .. } => {
                self.run_verify(task_paths, task, repo, full, on_progress)
                    .await
            }
            TaskSpec::RebuildCatalog { .. } => Err(CommandError::invalid_input(
                "catalog rebuild is not available in the first CLI release",
            )),
        }
    }

    async fn run_persisted_transfer<F>(
        &self,
        paths: &LiosPaths,
        task: &mut TaskRecord,
        repo: RepoConfig,
        plan: PersistedTransferPlan,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        if plan.direction == TransferDirection::Pull {
            return self
                .run_persisted_pull(paths, task, repo, plan, on_progress)
                .await;
        }
        let config = LiosConfig::load(&paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let (catalog, baseline) = download_catalog_baseline(paths, &key, &adapter, &repo).await?;
        if baseline.catalog_sha256 != plan.remote_catalog_baseline {
            return Err(CommandError::new(
                CommandErrorCode::RemoteConflict,
                "remote Catalog changed after the transfer plan was confirmed",
                true,
                None,
            ));
        }

        let total = u64::try_from(plan.actions.len()).map_err(|_| {
            CommandError::new(
                CommandErrorCode::CorruptedData,
                "transfer action count is invalid",
                false,
                None,
            )
        })?;
        let bytes_total = plan.actions.iter().try_fold(0u64, |sum, action| {
            sum.checked_add(action.size).ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "transfer byte count overflowed",
                    false,
                    None,
                )
            })
        })?;
        TaskStore::open(&paths.database)
            .map_err(to_err)?
            .update_transfer(task.id, 0, total, 0, bytes_total, 0)
            .map_err(to_err)?;

        let mut journal = transfer_journal(paths, task.id)?;
        let mut bytes_done = 0u64;
        for (index, action) in plan.actions.iter().enumerate() {
            let skipped = action.kind == TransferActionKind::Skip;
            mark_transfer_item(
                paths,
                &mut journal,
                action,
                if skipped {
                    TaskItemState::Skipped
                } else {
                    TaskItemState::Running
                },
                Some("applying_plan"),
                skipped,
                None,
            )?;
            let applied = (|| -> CommandResult<()> {
                if let (Some(source), Some(expected)) =
                    (&action.source_path, &action.source_fingerprint)
                {
                    if fingerprint_path(source)? != *expected {
                        return Err(CommandError::new(
                            CommandErrorCode::RemoteConflict,
                            format!(
                                "local source changed after planning: {}",
                                action.relative_path
                            ),
                            true,
                            None,
                        ));
                    }
                }

                match action.kind {
                    TransferActionKind::Skip => {}
                    TransferActionKind::Delete => {
                        let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
                        if let Some(node) = resolve_relative_node(&tree, &action.relative_path) {
                            catalog
                                .delete_nodes(std::slice::from_ref(&node.id), &key)
                                .map_err(to_err)?;
                        }
                    }
                    TransferActionKind::ReplaceType => {
                        let tree = catalog.decrypt_tree(&key).map_err(to_err)?;
                        if let Some(node) = resolve_relative_node(&tree, &action.relative_path) {
                            catalog
                                .delete_nodes(std::slice::from_ref(&node.id), &key)
                                .map_err(to_err)?;
                        }
                        apply_push_create_or_update(
                            &catalog,
                            paths,
                            &key,
                            &baseline.remote_objects,
                            action,
                            config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
                        )?;
                    }
                    TransferActionKind::Create | TransferActionKind::Update => {
                        apply_push_create_or_update(
                            &catalog,
                            paths,
                            &key,
                            &baseline.remote_objects,
                            action,
                            config.chunk_size.unwrap_or(PackOptions::DEFAULT_CHUNK_SIZE),
                        )?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = applied {
                mark_transfer_item(
                    paths,
                    &mut journal,
                    action,
                    TaskItemState::Failed,
                    Some("applying_plan"),
                    false,
                    Some(error.message.clone()),
                )?;
                return Err(error);
            }
            if !skipped {
                mark_transfer_item(
                    paths,
                    &mut journal,
                    action,
                    TaskItemState::Completed,
                    Some("catalog_pending"),
                    true,
                    None,
                )?;
            }
            bytes_done = bytes_done.checked_add(action.size).ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "transfer byte progress overflowed",
                    false,
                    None,
                )
            })?;
            let done = u64::try_from(index + 1).unwrap_or(total);
            TaskStore::open(&paths.database)
                .map_err(to_err)?
                .update_transfer(task.id, done, total, bytes_done, bytes_total, 0)
                .map_err(to_err)?;
            on_progress(ForegroundProgress {
                task_id: task.id,
                phase: "applying_plan".to_string(),
                completed: done,
                total,
                bytes_done,
                bytes_total,
            });
        }

        let work = plan_catalog_sync(paths, &catalog, &key, baseline)?;
        persist_sync_checkpoints(paths, task.id, &work)?;
        self.publish_sync(paths, task.id, &adapter, &repo, work, on_progress)
            .await
    }

    async fn run_persisted_pull<F>(
        &self,
        paths: &LiosPaths,
        task: &mut TaskRecord,
        repo: RepoConfig,
        plan: PersistedTransferPlan,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        let config = LiosConfig::load(&paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let catalog_path = paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let current_baseline = crate::sha256_hex_file(&catalog_path)?;
        if plan.remote_catalog_baseline.as_deref() != Some(current_baseline.as_str()) {
            return Err(CommandError::new(
                CommandErrorCode::RemoteConflict,
                "remote Catalog changed after the transfer plan was confirmed",
                true,
                None,
            ));
        }
        let catalog = Catalog::from_staging(paths.staging.clone());
        let mut journal = transfer_journal(paths, task.id)?;
        let node_ids = plan
            .actions
            .iter()
            .filter(|action| !transfer_action_finished(&journal, action))
            .filter(|action| action.entry_kind == TransferEntryKind::File)
            .filter(|action| {
                !matches!(
                    action.kind,
                    TransferActionKind::Skip | TransferActionKind::Delete
                )
            })
            .filter_map(|action| action.remote_node_id.clone())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        let selection = CatalogSelection::Nodes(node_ids);
        let remote_files = catalog
            .remote_files_for_selection(&selection, &key)
            .map_err(to_err)?;
        for file in &remote_files {
            let local_path = remote_to_staging_path(&paths.staging, &file.path)?;
            adapter
                .download_object(&repo.namespace, &repo.dataset, &file.path, &local_path)
                .await
                .map_err(to_err)?;
        }

        let total = u64::try_from(plan.actions.len()).map_err(|_| {
            CommandError::new(
                CommandErrorCode::CorruptedData,
                "transfer action count is invalid",
                false,
                None,
            )
        })?;
        let bytes_total = plan.actions.iter().try_fold(0u64, |sum, action| {
            sum.checked_add(action.size).ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "transfer byte count overflowed",
                    false,
                    None,
                )
            })
        })?;
        let ordered_actions = plan
            .actions
            .iter()
            .filter(|action| action.kind != TransferActionKind::Delete)
            .chain(
                plan.actions
                    .iter()
                    .filter(|action| action.kind == TransferActionKind::Delete),
            )
            .collect::<Vec<_>>();
        let mut bytes_done = ordered_actions
            .iter()
            .filter(|action| transfer_action_finished(&journal, action))
            .try_fold(0u64, |sum, action| {
                sum.checked_add(action.size).ok_or_else(|| {
                    CommandError::new(
                        CommandErrorCode::CorruptedData,
                        "transfer byte progress overflowed",
                        false,
                        None,
                    )
                })
            })?;
        let mut completed = ordered_actions
            .iter()
            .filter(|action| transfer_action_finished(&journal, action))
            .count();
        for action in ordered_actions {
            if transfer_action_finished(&journal, action) {
                continue;
            }
            let skipped = action.kind == TransferActionKind::Skip;
            mark_transfer_item(
                paths,
                &mut journal,
                action,
                if skipped {
                    TaskItemState::Skipped
                } else {
                    TaskItemState::Running
                },
                Some(if action.kind == TransferActionKind::Delete {
                    "deleting"
                } else {
                    "applying_plan"
                }),
                skipped,
                None,
            )?;
            let applied = (|| -> CommandResult<()> {
                validate_local_destination_fingerprint(action)?;
                match action.kind {
                    TransferActionKind::Skip => {}
                    TransferActionKind::Delete => delete_local_destination(action)?,
                    TransferActionKind::Create
                    | TransferActionKind::Update
                    | TransferActionKind::ReplaceType => {
                        apply_pull_action(&catalog, paths, &key, action)?;
                    }
                }
                Ok(())
            })();
            if let Err(error) = applied {
                mark_transfer_item(
                    paths,
                    &mut journal,
                    action,
                    TaskItemState::Failed,
                    Some("applying_plan"),
                    false,
                    Some(error.message.clone()),
                )?;
                return Err(error);
            }
            if !skipped {
                mark_transfer_item(
                    paths,
                    &mut journal,
                    action,
                    TaskItemState::Completed,
                    Some("applied"),
                    true,
                    None,
                )?;
            }
            bytes_done = bytes_done.checked_add(action.size).ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "transfer byte progress overflowed",
                    false,
                    None,
                )
            })?;
            completed += 1;
            let done = u64::try_from(completed).unwrap_or(total);
            TaskStore::open(&paths.database)
                .map_err(to_err)?
                .update_transfer(task.id, done, total, bytes_done, bytes_total, 0)
                .map_err(to_err)?;
            on_progress(ForegroundProgress {
                task_id: task.id,
                phase: "applying_plan".to_string(),
                completed: done,
                total,
                bytes_done,
                bytes_total,
            });
        }
        Ok(Vec::new())
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_upload<F>(
        &self,
        paths: &LiosPaths,
        task: &mut TaskRecord,
        repo: RepoConfig,
        parent_node_id: String,
        source_paths: Vec<PathBuf>,
        _source_files: Vec<SourceFileSnapshot>,
        chunk_size: usize,
        conflict_resolutions: Vec<ConflictResolution>,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        let config = LiosConfig::load(&paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let expected_snapshot = match TaskStore::open(&paths.database)
            .map_err(to_err)?
            .load_spec(task.id)
            .map_err(to_err)?
        {
            Some(TaskSpec::Upload {
                source_snapshot: Some(snapshot),
                ..
            }) => snapshot,
            _ => {
                return Err(CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "upload source snapshot is missing",
                    false,
                    None,
                ))
            }
        };
        validate_task_sources(&source_paths, &expected_snapshot, &task.items).map_err(to_err)?;
        let (catalog, baseline) = download_catalog_baseline(paths, &key, &adapter, &repo).await?;
        let remote_inventory = baseline.remote_objects.clone();
        let report = catalog
            .add_paths_to_folder_with_remote_inventory_and_progress_and_report(
                &parent_node_id,
                &source_paths,
                &conflict_resolutions,
                &key,
                PackOptions {
                    chunk_size,
                    staging_dir: paths.staging.clone(),
                },
                &remote_inventory,
                |pack| {
                    if let Ok(changed) = apply_pack_progress(
                        &mut task.items,
                        pack.completed_chunks,
                        pack.completed_bytes,
                        chunk_size,
                    ) {
                        if let Ok(store) = TaskStore::open(&paths.database) {
                            for item in changed {
                                let _ = store.upsert_item(&item);
                            }
                            let _ = store.update_transfer(
                                task.id,
                                pack.completed_chunks,
                                pack.total_chunks,
                                pack.completed_bytes,
                                pack.total_bytes,
                                0,
                            );
                        }
                    }
                    on_progress(ForegroundProgress {
                        task_id: task.id,
                        phase: "preparing".to_string(),
                        completed: pack.completed_chunks,
                        total: pack.total_chunks,
                        bytes_done: pack.completed_bytes,
                        bytes_total: pack.total_bytes,
                    });
                },
            )
            .map_err(to_err)?;
        report.ensure_no_skipped_paths().map_err(to_err)?;
        let work = plan_catalog_sync(paths, &catalog, &key, baseline)?;
        persist_sync_checkpoints(paths, task.id, &work)?;
        self.publish_sync(paths, task.id, &adapter, &repo, work, on_progress)
            .await
    }

    async fn run_delete<F>(
        &self,
        paths: &LiosPaths,
        task: &TaskRecord,
        repo: RepoConfig,
        node_ids: Vec<String>,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        let config = LiosConfig::load(&paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let (catalog, baseline) = download_catalog_baseline(paths, &key, &adapter, &repo).await?;
        catalog.delete_nodes(&node_ids, &key).map_err(to_err)?;
        let work = plan_catalog_sync(paths, &catalog, &key, baseline)?;
        persist_sync_checkpoints(paths, task.id, &work)?;
        self.publish_sync(paths, task.id, &adapter, &repo, work, on_progress)
            .await
    }

    async fn publish_sync<F>(
        &self,
        paths: &LiosPaths,
        task_id: Uuid,
        adapter: &ModelScopeAdapter,
        repo: &RepoConfig,
        work: crate::catalog_sync::SyncWork,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        let store = TaskStore::open(&paths.database).map_err(to_err)?;
        if !store
            .set_transaction_state(task_id, TaskState::Committing)
            .map_err(to_err)?
        {
            return Err(CommandError::new(
                CommandErrorCode::CorruptedData,
                "task could not enter the committing state",
                false,
                None,
            ));
        }
        let outcome = execute_sync_work(
            adapter,
            repo,
            work,
            || Ok(false),
            |progress| {
                persist_transaction_progress(paths, task_id, &progress)?;
                on_progress(progress_from_transaction(task_id, &progress));
                Ok(())
            },
        )
        .await?;
        match outcome {
            CatalogTransactionOutcome::Completed { warnings } => Ok(warnings),
            CatalogTransactionOutcome::Canceled => Err(CommandError::new(
                CommandErrorCode::Internal,
                "foreground catalog transaction was unexpectedly canceled",
                false,
                None,
            )),
        }
    }

    async fn run_download<F>(
        &self,
        paths: &LiosPaths,
        task: &mut TaskRecord,
        repo: RepoConfig,
        node_ids: Vec<String>,
        output_dir: PathBuf,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        let config = LiosConfig::load(&paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
        let catalog_path = paths.staging.join(CATALOG_FILE);
        adapter
            .download_object(&repo.namespace, &repo.dataset, CATALOG_FILE, &catalog_path)
            .await
            .map_err(to_err)?;
        let catalog = Catalog::from_staging(paths.staging.clone());
        let selection = CatalogSelection::Nodes(node_ids);
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
        let bytes_total = remote_files.iter().fold(0u64, |total, file| {
            total.saturating_add(remote_sizes.get(&file.path).copied().unwrap_or(0))
        });
        let store = TaskStore::open(&paths.database).map_err(to_err)?;
        let total = remote_files.len() as u64 + 1;
        store
            .update_transfer(task.id, 0, total, 0, bytes_total, 0)
            .map_err(to_err)?;
        let mut bytes_done = 0u64;
        for (index, file) in remote_files.iter().enumerate() {
            let local_path = remote_to_staging_path(&paths.staging, &file.path)?;
            adapter
                .download_object(&repo.namespace, &repo.dataset, &file.path, &local_path)
                .await
                .map_err(to_err)?;
            bytes_done = bytes_done.saturating_add(
                std::fs::metadata(&local_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            );
            let completed = index as u64 + 1;
            store
                .update_transfer(task.id, completed, total, bytes_done, bytes_total, 0)
                .map_err(to_err)?;
            on_progress(ForegroundProgress {
                task_id: task.id,
                phase: "downloading".to_string(),
                completed,
                total,
                bytes_done,
                bytes_total,
            });
        }
        store
            .update_phase(task.id, Some("restoring".to_string()))
            .map_err(to_err)?;
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
        store
            .update_transfer(task.id, total, total, bytes_done, bytes_total, 0)
            .map_err(to_err)?;
        on_progress(ForegroundProgress {
            task_id: task.id,
            phase: "completed".to_string(),
            completed: total,
            total,
            bytes_done,
            bytes_total,
        });
        Ok(Vec::new())
    }

    async fn run_verify<F>(
        &self,
        paths: &LiosPaths,
        task: &TaskRecord,
        repo: RepoConfig,
        full: bool,
        on_progress: &mut F,
    ) -> CommandResult<Vec<String>>
    where
        F: FnMut(ForegroundProgress),
    {
        let config = LiosConfig::load(&paths.config).map_err(to_err)?;
        let key = key_from_config(&config)?;
        let adapter = ModelScopeAdapter::new(repo.endpoint.clone(), self.read_token()?);
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
        let remote = catalog
            .verify_remote_inventory(&key, &remote_objects)
            .map_err(to_err)?;
        if !full {
            on_progress(ForegroundProgress {
                task_id: task.id,
                phase: "verified".to_string(),
                completed: remote.verified_objects,
                total: remote.expected_objects,
                bytes_done: remote.encoded_bytes_verified,
                bytes_total: remote.encoded_bytes_verified,
            });
            return Ok(vec![format_remote_report(&remote)]);
        }
        let files = catalog
            .remote_files_for_selection(&CatalogSelection::All, &key)
            .map_err(to_err)?;
        let bytes_total = files.iter().fold(0u64, |total, file| {
            total.saturating_add(file.expected_size.unwrap_or(0))
        });
        let mut bytes_done = 0u64;
        for (index, file) in files.iter().enumerate() {
            let local_path = remote_to_staging_path(&paths.staging, &file.path)?;
            adapter
                .download_object(&repo.namespace, &repo.dataset, &file.path, &local_path)
                .await
                .map_err(to_err)?;
            bytes_done = bytes_done.saturating_add(
                std::fs::metadata(&local_path)
                    .map(|metadata| metadata.len())
                    .unwrap_or(0),
            );
            on_progress(ForegroundProgress {
                task_id: task.id,
                phase: "verifying".to_string(),
                completed: index as u64 + 1,
                total: files.len() as u64,
                bytes_done,
                bytes_total,
            });
        }
        let local = catalog.verify_staged_integrity(&key).map_err(to_err)?;
        Ok(vec![
            format_remote_report(&remote),
            format_local_report(&local),
        ])
    }
}

fn transfer_journal(paths: &LiosPaths, task_id: Uuid) -> CommandResult<HashMap<String, TaskItem>> {
    TaskStore::open(&paths.database)
        .map_err(to_err)?
        .list_items(task_id)
        .map_err(to_err)
        .map(|items| {
            items
                .into_iter()
                .filter_map(|item| {
                    item.relative_path
                        .as_ref()
                        .map(|path| (path.to_string_lossy().into_owned(), item.clone()))
                })
                .collect()
        })
}

fn transfer_action_finished(
    journal: &HashMap<String, TaskItem>,
    action: &PersistedTransferAction,
) -> bool {
    journal.get(&action.relative_path).is_some_and(|item| {
        matches!(
            item.state,
            TaskItemState::Completed | TaskItemState::Skipped
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn mark_transfer_item(
    paths: &LiosPaths,
    journal: &mut HashMap<String, TaskItem>,
    action: &PersistedTransferAction,
    state: TaskItemState,
    phase: Option<&str>,
    complete_bytes: bool,
    error: Option<String>,
) -> CommandResult<()> {
    let item = journal.get_mut(&action.relative_path).ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::CorruptedData,
            format!("transfer journal item is missing: {}", action.relative_path),
            false,
            None,
        )
    })?;
    item.state = state;
    item.phase = phase.map(str::to_string);
    item.error = error;
    if complete_bytes {
        item.bytes_done = item.bytes_total;
    }
    TaskStore::open(&paths.database)
        .map_err(to_err)?
        .upsert_item(item)
        .map_err(to_err)
}

fn apply_push_create_or_update(
    catalog: &Catalog,
    paths: &LiosPaths,
    key: &lios_core::crypto::KeyFile,
    remote_objects: &[lios_core::storage::StorageObject],
    action: &PersistedTransferAction,
    chunk_size: usize,
) -> CommandResult<()> {
    let (parent_path, name) = action
        .relative_path
        .rsplit_once('/')
        .map_or(("", action.relative_path.as_str()), |(parent, name)| {
            (parent, name)
        });
    let tree = catalog.decrypt_tree(key).map_err(to_err)?;
    let parent = if parent_path.is_empty() {
        &tree
    } else {
        resolve_relative_node(&tree, parent_path).ok_or_else(|| {
            CommandError::new(
                CommandErrorCode::RemoteConflict,
                format!("planned parent directory is missing: {parent_path}"),
                true,
                None,
            )
        })?
    };
    if !matches!(parent.kind, CatalogTreeNodeKind::Directory { .. }) {
        return Err(CommandError::new(
            CommandErrorCode::RemoteConflict,
            format!("planned parent is no longer a directory: {parent_path}"),
            true,
            None,
        ));
    }

    match action.entry_kind {
        TransferEntryKind::Directory => {
            if resolve_relative_node(&tree, &action.relative_path).is_none() {
                catalog
                    .create_folder(&parent.id, name, key)
                    .map_err(to_err)?;
            }
            Ok(())
        }
        TransferEntryKind::File => {
            let source = action.source_path.as_ref().ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "planned file action has no local source",
                    false,
                    None,
                )
            })?;
            let input_dir = paths
                .staging
                .join("transfer-input")
                .join(Uuid::new_v4().simple().to_string());
            std::fs::create_dir_all(&input_dir).map_err(to_err)?;
            let staged_source = input_dir.join(name);
            if std::fs::hard_link(source, &staged_source).is_err() {
                std::fs::copy(source, &staged_source).map_err(to_err)?;
            }
            if action.source_sha256.as_deref().is_some_and(|expected| {
                crate::sha256_hex_file(&staged_source).ok().as_deref() != Some(expected)
            }) {
                return Err(CommandError::new(
                    CommandErrorCode::RemoteConflict,
                    format!("local source content changed: {}", action.relative_path),
                    true,
                    None,
                ));
            }
            let resolutions = if resolve_relative_node(&tree, &action.relative_path).is_some() {
                vec![ConflictResolution {
                    source_path: staged_source.display().to_string(),
                    action: ConflictAction::Replace,
                }]
            } else {
                Vec::new()
            };
            catalog
                .add_paths_to_folder_with_remote_inventory(
                    &parent.id,
                    std::slice::from_ref(&staged_source),
                    &resolutions,
                    key,
                    PackOptions {
                        chunk_size,
                        staging_dir: paths.staging.clone(),
                    },
                    remote_objects,
                )
                .map_err(to_err)
        }
    }
}

fn resolve_relative_node<'a>(
    root: &'a CatalogTreeNode,
    relative_path: &str,
) -> Option<&'a CatalogTreeNode> {
    if relative_path.is_empty() {
        return Some(root);
    }
    let mut current = root;
    for segment in relative_path.split('/') {
        let CatalogTreeNodeKind::Directory { children } = &current.kind else {
            return None;
        };
        current = children
            .iter()
            .find(|child| child.name.eq_ignore_ascii_case(segment))?;
    }
    Some(current)
}

fn validate_local_destination_fingerprint(action: &PersistedTransferAction) -> CommandResult<()> {
    let Some(path) = action.local_destination_path.as_deref() else {
        return Ok(());
    };
    match action.destination_fingerprint.as_deref() {
        Some(expected) => {
            let actual = fingerprint_path(path).map_err(|_| {
                CommandError::new(
                    CommandErrorCode::RemoteConflict,
                    format!(
                        "local destination changed after planning: {}",
                        action.relative_path
                    ),
                    true,
                    None,
                )
            })?;
            if actual != expected {
                return Err(CommandError::new(
                    CommandErrorCode::RemoteConflict,
                    format!(
                        "local destination changed after planning: {}",
                        action.relative_path
                    ),
                    true,
                    None,
                ));
            }
        }
        None if path.exists() => {
            return Err(CommandError::new(
                CommandErrorCode::RemoteConflict,
                format!(
                    "local destination appeared after planning: {}",
                    action.relative_path
                ),
                true,
                None,
            ));
        }
        None => {}
    }
    Ok(())
}

fn apply_pull_action(
    catalog: &Catalog,
    paths: &LiosPaths,
    key: &lios_core::crypto::KeyFile,
    action: &PersistedTransferAction,
) -> CommandResult<()> {
    let destination = action.local_destination_path.as_deref().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::CorruptedData,
            "pull action has no local destination",
            false,
            None,
        )
    })?;
    ensure_safe_local_parents(destination)?;
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).map_err(to_err)?;
        ensure_safe_local_parents(destination)?;
    }

    match action.entry_kind {
        TransferEntryKind::Directory => {
            if action.kind == TransferActionKind::ReplaceType {
                replace_local_type(destination, || {
                    std::fs::create_dir(destination).map_err(to_err)
                })?;
            } else if !destination.exists() {
                std::fs::create_dir(destination).map_err(to_err)?;
            }
            Ok(())
        }
        TransferEntryKind::File => {
            let node_id = action.remote_node_id.as_ref().ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::CorruptedData,
                    "pull file action has no remote node ID",
                    false,
                    None,
                )
            })?;
            let tree = catalog.decrypt_tree(key).map_err(to_err)?;
            let node = find_node_by_id(&tree, node_id).ok_or_else(|| {
                CommandError::new(
                    CommandErrorCode::RemoteConflict,
                    "planned remote file no longer exists",
                    true,
                    None,
                )
            })?;
            let restore_dir = paths
                .staging
                .join("pull-ready")
                .join(Uuid::new_v4().simple().to_string());
            std::fs::create_dir_all(&restore_dir).map_err(to_err)?;
            catalog
                .restore(
                    CatalogSelection::Node(node_id.clone()),
                    key,
                    RestoreOptions {
                        output_dir: restore_dir.clone(),
                        conflict_policy: RestoreConflictPolicy::Rename,
                    },
                )
                .map_err(to_err)?;
            let restored = restore_dir.join(&node.name);
            if action.kind == TransferActionKind::ReplaceType {
                replace_local_type(destination, || {
                    lios_core::copy_file_atomic(&restored, destination, false).map_err(to_err)
                })?;
            } else {
                lios_core::copy_file_atomic(
                    &restored,
                    destination,
                    action.kind == TransferActionKind::Update,
                )
                .map_err(to_err)?;
            }
            Ok(())
        }
    }
}

fn replace_local_type(
    destination: &std::path::Path,
    apply: impl FnOnce() -> CommandResult<()>,
) -> CommandResult<()> {
    let backup = destination.with_file_name(format!(
        ".{}.lios-backup-{}",
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("item"),
        Uuid::new_v4().simple()
    ));
    std::fs::rename(destination, &backup).map_err(to_err)?;
    match apply() {
        Ok(()) => {
            if backup.is_dir() {
                std::fs::remove_dir_all(backup).map_err(to_err)?;
            } else {
                std::fs::remove_file(backup).map_err(to_err)?;
            }
            Ok(())
        }
        Err(error) => {
            let _ = if destination.is_dir() {
                std::fs::remove_dir_all(destination)
            } else {
                std::fs::remove_file(destination)
            };
            let _ = std::fs::rename(&backup, destination);
            Err(error)
        }
    }
}

fn delete_local_destination(action: &PersistedTransferAction) -> CommandResult<()> {
    let destination = action.local_destination_path.as_deref().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::CorruptedData,
            "delete action has no local destination",
            false,
            None,
        )
    })?;
    if !destination.exists() {
        return Ok(());
    }
    let metadata = std::fs::symlink_metadata(destination).map_err(to_err)?;
    if metadata.file_type().is_symlink() {
        return Err(CommandError::invalid_input(
            "restore destination contains unsupported symbolic links or junctions",
        ));
    }
    if metadata.is_dir() {
        std::fs::remove_dir(destination).map_err(to_err)
    } else {
        std::fs::remove_file(destination).map_err(to_err)
    }
}

fn ensure_safe_local_parents(destination: &std::path::Path) -> CommandResult<()> {
    let mut current = destination.parent();
    while let Some(path) = current {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(CommandError::invalid_input(
                    "restore destination contains unsupported symbolic links or junctions",
                ));
            }
            Ok(metadata) if !metadata.is_dir() => {
                return Err(CommandError::new(
                    CommandErrorCode::RemoteConflict,
                    "restore destination parent is not a directory",
                    true,
                    None,
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(to_err(error)),
        }
        current = path.parent();
    }
    Ok(())
}

fn find_node_by_id<'a>(root: &'a CatalogTreeNode, id: &str) -> Option<&'a CatalogTreeNode> {
    if root.id == id {
        return Some(root);
    }
    let CatalogTreeNodeKind::Directory { children } = &root.kind else {
        return None;
    };
    children.iter().find_map(|child| find_node_by_id(child, id))
}

fn clean_ids(values: Vec<String>, empty_message: &'static str) -> CommandResult<Vec<String>> {
    let values = values
        .into_iter()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    if values.is_empty() {
        return Err(CommandError::invalid_input(empty_message));
    }
    Ok(values)
}

fn summary_for(paths: &LiosPaths, task_id: Uuid) -> CommandResult<TaskSummary> {
    TaskStore::open(&paths.database)
        .map_err(to_err)?
        .get_summary(task_id)
        .map_err(to_err)?
        .ok_or_else(|| CommandError::invalid_input("task was not found"))
}

fn task_repo(spec: &TaskSpec) -> &RepoConfig {
    match spec {
        TaskSpec::Copy { repo, .. }
        | TaskSpec::Sync { repo, .. }
        | TaskSpec::Upload { repo, .. }
        | TaskSpec::Delete { repo, .. }
        | TaskSpec::Download { repo, .. }
        | TaskSpec::VerifySpace { repo, .. }
        | TaskSpec::RebuildCatalog { repo, .. } => repo,
    }
}

fn phase_label(phase: CatalogTransactionPhase) -> &'static str {
    match phase {
        CatalogTransactionPhase::ValidateBlobs => "validating",
        CatalogTransactionPhase::UploadBlobs => "uploading",
        CatalogTransactionPhase::Prepublish => "prepublishing",
        CatalogTransactionPhase::ProbeCatalog => "checking_remote",
        CatalogTransactionPhase::Publish => "publishing",
        CatalogTransactionPhase::Cleanup => "cleaning",
    }
}

fn persist_transaction_progress(
    paths: &LiosPaths,
    task_id: Uuid,
    progress: &CatalogTransactionProgress,
) -> lios_core::Result<()> {
    let store = TaskStore::open(&paths.database)?;
    if let Some(checkpoint) = &progress.blob_checkpoint {
        store.upsert_checkpoint(&TaskObjectCheckpoint {
            task_id,
            remote_path: checkpoint.path.clone(),
            oid: checkpoint.oid.clone(),
            size: checkpoint.size,
            state: match checkpoint.state {
                CatalogBlobCheckpointState::Uploaded => CheckpointState::Uploaded,
                CatalogBlobCheckpointState::Committed => CheckpointState::Committed,
            },
        })?;
    }
    store.update_phase(task_id, Some(phase_label(progress.phase).to_string()))?;
    store.update_transfer(
        task_id,
        progress.completed_items,
        progress.total_items,
        progress.bytes_done,
        progress.bytes_total,
        0,
    )
}

fn progress_from_transaction(
    task_id: Uuid,
    progress: &CatalogTransactionProgress,
) -> ForegroundProgress {
    ForegroundProgress {
        task_id,
        phase: phase_label(progress.phase).to_string(),
        completed: progress.completed_items,
        total: progress.total_items,
        bytes_done: progress.bytes_done,
        bytes_total: progress.bytes_total,
    }
}

fn progress_from_summary(summary: TaskSummary) -> ForegroundProgress {
    ForegroundProgress {
        task_id: summary.id,
        phase: summary.phase.unwrap_or_else(|| "completed".to_string()),
        completed: summary.progress_done,
        total: summary.progress_total,
        bytes_done: summary.bytes_done,
        bytes_total: summary.bytes_total,
    }
}

fn format_remote_report(report: &CatalogRemoteIntegrityReport) -> String {
    format!(
        "remote verification: {}/{} objects, {} encoded bytes, {} unreferenced objects",
        report.verified_objects,
        report.expected_objects,
        report.encoded_bytes_verified,
        report.unreferenced_managed_objects
    )
}

fn format_local_report(report: &CatalogIntegrityReport) -> String {
    format!(
        "content verification: {} nodes, {} objects, {} chunks, {} original bytes",
        report.nodes_verified,
        report.objects_verified,
        report.chunks_verified,
        report.original_bytes_verified
    )
}

#[cfg(test)]
mod tests {
    use lios_core::catalog_transaction::{
        CatalogBlobCheckpoint, CatalogBlobCheckpointState, CatalogTransactionPhase,
        CatalogTransactionProgress,
    };
    use lios_core::config::LiosPaths;
    use lios_core::tasks::{CheckpointState, TaskRecord, TaskStore};
    use tempfile::tempdir;

    use super::persist_transaction_progress;

    #[test]
    fn transaction_progress_persists_uploaded_and_committed_checkpoints() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let task = TaskRecord::queued("upload", 1);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert(&task).unwrap();

        let progress = |state| CatalogTransactionProgress {
            phase: CatalogTransactionPhase::UploadBlobs,
            completed_items: 1,
            total_items: 2,
            bytes_done: 5,
            bytes_total: 10,
            blob_checkpoint: Some(CatalogBlobCheckpoint {
                path: "objects/files/a/chunks/b.lios".to_string(),
                oid: "a".repeat(64),
                size: 5,
                state,
            }),
        };

        persist_transaction_progress(
            &paths,
            task.id,
            &progress(CatalogBlobCheckpointState::Uploaded),
        )
        .unwrap();
        assert_eq!(
            store.list_checkpoints(task.id).unwrap()[0].state,
            CheckpointState::Uploaded
        );

        persist_transaction_progress(
            &paths,
            task.id,
            &progress(CatalogBlobCheckpointState::Committed),
        )
        .unwrap();
        assert_eq!(
            store.list_checkpoints(task.id).unwrap()[0].state,
            CheckpointState::Committed
        );
    }
}
