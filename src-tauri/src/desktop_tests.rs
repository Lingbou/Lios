use super::*;
#[cfg(test)]
mod task_center_backend_tests {
    use std::path::PathBuf;

    use lios_application::space_registry::SpaceRegistry;
    use lios_application::task_support::TaskScope;
    use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
    use lios_core::tasks::{
        TaskItem, TaskItemState, TaskRecord, TaskSpec, TaskState, TaskStore, TaskSummary,
    };
    use lios_core::LiosError;
    use serde_json::json;
    use tempfile::tempdir;
    use uuid::Uuid;

    use super::{
        cleanup_is_safe, clear_task_record, list_task_items_for_paths, paths_dto,
        persist_transfer_submission, recover_startup_tasks, recovery_key_status,
        remove_space_for_paths, submission_summary, task_summaries_for_paths, AppContext,
        CommandError, CommandErrorCode, SetupSnapshot, TaskUpdateEvent,
    };

    fn configured_space(paths: &LiosPaths) -> RepoConfig {
        paths.ensure_dirs().unwrap();
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
            title: None,
        };
        let mut config = LiosConfig::load(&paths.config).unwrap_or_default();
        config.spaces.insert("cold".to_string(), repo.clone());
        config.save(&paths.config).unwrap();
        repo
    }

    #[test]
    fn remove_space_refuses_while_the_space_lock_is_held() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let repo = configured_space(&paths);
        let scope = TaskScope::from_repo(&repo);
        let guard = paths.try_lock_space(&scope.space_id).unwrap();

        let error = remove_space_for_paths(&paths, "cold").unwrap_err();

        assert_eq!(error.code, CommandErrorCode::Busy);
        assert!(SpaceRegistry::new(paths.clone()).resolve("cold").is_ok());

        drop(guard);
        remove_space_for_paths(&paths, "cold").unwrap();
        assert!(SpaceRegistry::new(paths).resolve("cold").is_err());
    }

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
            started_at: Some("2026-07-12T00:00:00Z".to_string()),
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
        let task = TaskRecord::queued("upload", 1);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert(&task).unwrap();
        let error = CommandError::from(LiosError::Unsupported(format!(
            "source paths no longer exist: {WINDOWS_SENTINEL}; {UNIX_SENTINEL}"
        )));
        store
            .update_state(task.id, TaskState::Failed, Some(error.message))
            .unwrap();
        drop(store);
        let summary = TaskStore::open(&paths.database)
            .unwrap()
            .get_summary(task.id)
            .unwrap()
            .unwrap();
        let event = serde_json::to_string(&TaskUpdateEvent::Upsert {
            task: Box::new(summary.clone()),
        })
        .unwrap();

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
        let spec = TaskSpec::Transfer {
            space_id: "space-a".to_string(),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "safe-summary".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
            },
            plan: lios_core::tasks::PersistedTransferPlan {
                direction: lios_core::tasks::TransferDirection::Push,
                source_operand: sentinel.to_string_lossy().into_owned(),
                destination_operand: "/".to_string(),
                source_trailing_slash: false,
                excludes: Vec::new(),
                remote_catalog_baseline: None,
                delete_scope: None,
                actions: vec![lios_core::tasks::PersistedTransferAction {
                    relative_path: "secret.bin".to_string(),
                    source_path: Some(sentinel.clone()),
                    remote_node_id: None,
                    local_destination_path: None,
                    kind: lios_core::tasks::TransferActionKind::Create,
                    entry_kind: lios_core::tasks::TransferEntryKind::File,
                    source_sha256: Some("a".repeat(64)),
                    source_fingerprint: Some("fingerprint".to_string()),
                    size: 7,
                    destination_fingerprint: None,
                    state: lios_core::tasks::TransferActionState::Pending,
                }],
            },
        };
        let actions = match &spec {
            TaskSpec::Transfer { plan, .. } => plan.actions.clone(),
            _ => Vec::new(),
        };
        let task = persist_transfer_submission(&paths, &spec, &actions).unwrap();

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

        assert!(!cleanup_is_safe(&paths).unwrap());
    }

    #[tokio::test]
    async fn control_paths_ignore_malformed_items_on_the_target_task() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let task = insert_malformed_task(&paths, TaskState::Running);
        TaskStore::open(&paths.database)
            .unwrap()
            .interrupt_task(task.id, TaskState::Canceled)
            .unwrap();

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
        assert!(clear_task_record(&paths, paused.id).await.is_err());

        let completed = insert_malformed_task(&paths, TaskState::Completed);
        clear_task_record(&paths, completed.id).await.unwrap();
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
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
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
        assert_eq!(queued[0].1.space_id(), spec.space_id());
    }

    #[test]
    fn startup_recovery_skips_a_space_locked_by_another_frontend() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = TaskSpec::Delete {
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
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
    use lios_core::config::{LiosPaths, RepoConfig};
    use lios_core::tasks::{TaskRecord, TaskSpec, TaskState, TaskStore};
    use tempfile::tempdir;

    use super::{clear_task_record, recover_startup_tasks, task_state_is_active, CommandErrorCode};

    #[test]
    fn startup_requeues_replayable_specs_and_preserves_commits_for_reconciliation() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = TaskSpec::Delete {
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
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
    }

    #[test]
    fn every_nonterminal_task_state_is_active() {
        for state in [
            TaskState::Queued,
            TaskState::Running,
            TaskState::Paused,
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
            TaskState::Running,
            TaskState::Paused,
            TaskState::Committing,
        ] {
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let task = TaskRecord::queued("persisted active", 0);
            let store = TaskStore::open(&paths.database).unwrap();
            store.insert(&task).unwrap();
            store.update_state(task.id, state.clone(), None).unwrap();
            let error = clear_task_record(&paths, task.id).await.unwrap_err();

            assert_eq!(error.code, CommandErrorCode::InvalidInput, "{state:?}");
            assert!(TaskStore::open(&paths.database)
                .unwrap()
                .list()
                .unwrap()
                .iter()
                .any(|record| record.id == task.id));
        }
    }
}

#[cfg(test)]
mod remote_verification_tests {
    use std::fs;
    #[cfg(windows)]
    use std::process::Command;

    #[cfg(windows)]
    use super::cleanup_terminal_task_staging_and_record;
    use super::{
        append_task_warning, cleanup_current_staging_cache, cleanup_terminal_task_staging,
        cleanup_terminal_task_staging_after_restart_async, clear_task_record,
        ensure_verification_revision_unchanged, map_remote_integrity_error,
        validate_local_remote_file, verification_commit_id, CommandErrorCode,
        LocalRemoteFileValidation,
    };
    use lios_core::catalog::CatalogRemoteFile;
    use lios_core::config::{LiosPaths, RepoConfig};
    use lios_core::storage::RepoRevision;
    use lios_core::tasks::{TaskRecord, TaskSpec, TaskState, TaskStore};
    use lios_core::LiosError;
    use tempfile::tempdir;
    use uuid::Uuid;

    fn verification_spec() -> TaskSpec {
        TaskSpec::VerifySpace {
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
            },
            full: true,
        }
    }

    fn rebuild_spec() -> TaskSpec {
        TaskSpec::RebuildCatalog {
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
            },
            expected_revision: "preview-revision".to_string(),
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

    #[test]
    fn terminal_verification_discards_staging_but_paused_verification_keeps_it() {
        for (state, should_exist) in [
            (TaskState::Completed, false),
            (TaskState::Failed, false),
            (TaskState::Canceled, false),
            (TaskState::Paused, true),
            (TaskState::Running, true),
        ] {
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let spec = verification_spec();
            let task = TaskRecord::queued_for_spec(&spec);
            let store = TaskStore::open(&paths.database).unwrap();
            store.insert_with_spec(&task, &spec).unwrap();
            store.update_state(task.id, state, None).unwrap();
            let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
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
            (TaskState::Running, true),
        ] {
            let temp = tempdir().unwrap();
            let paths = LiosPaths::from_home(temp.path());
            let spec = rebuild_spec();
            let task = TaskRecord::queued_for_spec(&spec);
            let store = TaskStore::open(&paths.database).unwrap();
            store.insert_with_spec(&task, &spec).unwrap();
            store.update_state(task.id, state, None).unwrap();
            let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
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
        let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
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
        let expected_warning = format!("{} staging cleanup failed", spec.label());
        assert!(stored
            .error
            .as_deref()
            .is_some_and(|error| error.contains(&expected_warning)));
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
        let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
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
        let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("catalog.enc"), b"rebuilt").unwrap();

        cleanup_terminal_task_staging_after_restart_async(paths.clone())
            .await
            .unwrap();

        assert!(!task_paths.staging.exists());
    }

    #[tokio::test]
    async fn cached_file_hashing_detects_mismatched_content() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("cached.lios");
        fs::write(&path, vec![7u8; 2 * 1024 * 1024]).unwrap();
        let file = CatalogRemoteFile {
            path: "objects/files/a/chunks/b.lios".to_string(),
            expected_size: Some(2 * 1024 * 1024),
            sha256: Some("0".repeat(64)),
        };

        let outcome = validate_local_remote_file(&path, &file).await.unwrap();

        assert_eq!(outcome, LocalRemoteFileValidation::Invalid);
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
        let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("cached.lios"), b"cached").unwrap();

        clear_task_record(&paths, task.id).await.unwrap();

        assert!(!task_paths.staging.exists());
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .get(task.id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn clearing_terminal_download_removes_staging_before_the_record() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let spec = TaskSpec::Delete {
            space_id: "b".repeat(64),
            repo: RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
            },
            node_ids: vec!["node1".to_string()],
        };
        let task = TaskRecord::queued_for_spec(&spec);
        let store = TaskStore::open(&paths.database).unwrap();
        store.insert_with_spec(&task, &spec).unwrap();
        store
            .update_state(task.id, TaskState::Completed, None)
            .unwrap();
        let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("chunk.lios"), b"downloaded").unwrap();

        clear_task_record(&paths, task.id).await.unwrap();

        assert!(!task_paths.staging.exists());
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .get(task.id)
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn cleanup_current_staging_cache_removes_terminal_and_orphaned_task_staging() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let space_id = "b".repeat(64);

        let completed_id = Uuid::new_v4();
        let orphaned_id = Uuid::new_v4();

        let store = TaskStore::open(&paths.database).unwrap();
        let mut completed_task = TaskRecord::queued("completed", 1);
        completed_task.id = completed_id;
        store.insert(&completed_task).unwrap();
        store
            .update_state(completed_id, TaskState::Completed, None)
            .unwrap();

        let completed_staged = paths
            .staging
            .join(&space_id)
            .join(completed_id.to_string())
            .join("chunk.lios");
        let orphaned_staged = paths
            .staging
            .join(&space_id)
            .join(orphaned_id.to_string())
            .join("chunk.lios");

        fs::create_dir_all(completed_staged.parent().unwrap()).unwrap();
        fs::create_dir_all(orphaned_staged.parent().unwrap()).unwrap();
        fs::write(&completed_staged, b"completed 123").unwrap();
        fs::write(&orphaned_staged, b"orphaned 456").unwrap();

        let report = cleanup_current_staging_cache(&paths, false, false).unwrap();

        assert!(!completed_staged.exists());
        assert!(!orphaned_staged.exists());
        assert_eq!(report.files_removed, 2);
        assert_eq!(
            report.bytes_removed,
            b"completed 123".len() as u64 + b"orphaned 456".len() as u64
        );
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
        let task_paths = paths.for_task(spec.space_id(), task.id).unwrap();
        task_paths.ensure_dirs().unwrap();
        fs::write(task_paths.staging.join("catalog.enc"), b"rebuilt").unwrap();

        clear_task_record(&paths, task.id).await.unwrap();

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
    use super::recovery_key_service::{
        export_recovery_key_for_paths, import_candidate_with_adapter,
        import_candidate_with_adapter_after_verification, recovery_key_status,
        verify_candidate_with_adapter,
    };
    use lios_application::ConfigMutationGate;

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
        async fn create_repo(
            &self,
            _namespace: &str,
            _dataset: &str,
            _title: Option<&str>,
        ) -> lios_core::Result<()> {
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
            title: None,
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
            title: None,
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
            title: None,
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

#[cfg(test)]
mod settings_command_tests {
    use lios_application::service::Application;
    use lios_core::config::{
        validate_modelscope_production_endpoint, LiosConfig, LiosPaths, MODELSCOPE_WWW_ENDPOINT,
    };
    use tempfile::tempdir;

    use super::production_config::persist_config;

    #[test]
    fn clear_token_removes_saved_credentials_file() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let app = Application::new(paths.clone()).unwrap();
        app.set_token("test-token").unwrap();
        assert!(paths.credentials.is_file());

        app.clear_token().unwrap();
        assert!(!paths.credentials.exists());

        // Idempotent clear
        app.clear_token().unwrap();
        assert!(!paths.credentials.exists());
    }

    #[test]
    fn set_endpoint_logic_validates_and_persists() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let mut config = LiosConfig::default();

        let validated =
            validate_modelscope_production_endpoint("https://www.modelscope.cn/").unwrap();
        assert_eq!(validated, MODELSCOPE_WWW_ENDPOINT);
        config.endpoint = Some(validated);
        persist_config(&paths, &mut config).unwrap();

        let loaded = LiosConfig::load(&paths.config).unwrap();
        assert_eq!(loaded.endpoint.as_deref(), Some(MODELSCOPE_WWW_ENDPOINT));

        // Setting to empty clears endpoint
        config.endpoint = None;
        persist_config(&paths, &mut config).unwrap();
        let loaded = LiosConfig::load(&paths.config).unwrap();
        assert_eq!(loaded.endpoint, None);

        // Invalid endpoint is rejected
        let invalid = validate_modelscope_production_endpoint("https://evil.com");
        assert!(invalid.is_err());
    }
}
