use lios_core::{
    catalog::{ConflictAction, ConflictResolution, SourceSnapshotReport},
    config::{ensure_default_key_configured, LiosConfig, LiosPaths, RepoConfig},
    credentials::{protect_to_file, unprotect_from_file},
    crypto::KeyFile,
    tasks::{
        CheckpointState, FileContentIndexEntry, TaskCatalogCheckpoint, TaskItem, TaskItemState,
        TaskObjectCheckpoint, TaskRecord, TaskSpec, TaskState, TaskStore,
    },
    LiosError,
};
use serde::Deserialize;
use tempfile::tempdir;
use uuid::Uuid;

#[derive(Deserialize)]
struct EmittedV2KeyFile {
    version: u8,
    algorithm: String,
    kdf: String,
    master_key: String,
}

#[test]
fn local_paths_follow_lios_layout() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());

    assert_eq!(paths.home, tmp.path().join(".lios"));
    assert_eq!(paths.config, tmp.path().join(".lios/config.yaml"));
    assert_eq!(paths.database, tmp.path().join(".lios/lios.db"));
    assert_eq!(paths.staging, tmp.path().join(".lios/staging"));
    assert_eq!(paths.credentials, tmp.path().join(".lios/credentials.enc"));
}

#[test]
fn task_scoped_paths_are_nested_under_staging_and_reject_unsafe_ids() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    let task_id = Uuid::new_v4();
    let account_id = "a".repeat(64);
    let space_id = "b".repeat(64);

    let scoped = paths.for_task(&account_id, &space_id, task_id).unwrap();
    assert_eq!(
        scoped.staging,
        paths
            .staging
            .join(&account_id)
            .join(&space_id)
            .join(task_id.to_string())
    );
    assert_eq!(scoped.database, paths.database);
    assert_eq!(scoped.config, paths.config);
    assert!(paths.for_task("../account", &space_id, task_id).is_err());
    assert!(paths
        .for_task(&account_id.to_uppercase(), &space_id, task_id)
        .is_err());
    assert!(paths.for_task(&account_id, "short", task_id).is_err());
}

#[test]
fn config_roundtrips_as_yaml_without_token_material() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("config.yaml");
    let config = LiosConfig {
        active_repo: Some(RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold-backup".to_string(),
            endpoint: "https://www.modelscope.cn".to_string(),
        }),
        key_file_path: Some(tmp.path().join("recovery.key")),
        chunk_size: Some(128 * 1024 * 1024),
    };

    config.save(&path).unwrap();
    let raw_yaml = std::fs::read_to_string(&path).unwrap();
    let loaded = LiosConfig::load(&path).unwrap();

    assert_eq!(loaded.active_repo.unwrap().dataset, "cold-backup");
    assert!(!raw_yaml.contains("token"));
    assert!(!raw_yaml.contains("secret"));
}

#[test]
fn first_run_generates_default_key_and_persists_config() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    paths.ensure_dirs().unwrap();
    let mut config = LiosConfig::default();

    ensure_default_key_configured(&paths, &mut config).unwrap();

    let key_path = paths.home.join("recovery.key");
    assert_eq!(config.key_file_path.as_deref(), Some(key_path.as_path()));
    assert!(key_path.exists());

    let saved = LiosConfig::load(&paths.config).unwrap();
    assert_eq!(saved.key_file_path.as_deref(), Some(key_path.as_path()));
    KeyFile::load_from_path(key_path).unwrap();
}

#[test]
fn missing_key_path_rebinds_existing_valid_default_key_without_overwriting() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    paths.ensure_dirs().unwrap();
    let key_path = paths.home.join("recovery.key");
    KeyFile::generate_to_path(&key_path).unwrap();
    let original_key_file = std::fs::read(&key_path).unwrap();
    let mut config = LiosConfig::default();

    ensure_default_key_configured(&paths, &mut config).unwrap();

    assert_eq!(std::fs::read(&key_path).unwrap(), original_key_file);
    assert_eq!(config.key_file_path.as_deref(), Some(key_path.as_path()));
    let saved = LiosConfig::load(&paths.config).unwrap();
    assert_eq!(saved.key_file_path.as_deref(), Some(key_path.as_path()));
}

#[test]
fn invalid_existing_default_key_is_not_overwritten() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    paths.ensure_dirs().unwrap();
    let key_path = paths.home.join("recovery.key");
    let invalid_key = b"version: 1\nalgorithm: wrong\nkey: invalid\n";
    std::fs::write(&key_path, invalid_key).unwrap();
    let mut config = LiosConfig::default();

    let result = ensure_default_key_configured(&paths, &mut config);

    let error = result.unwrap_err();
    assert!(matches!(&error, LiosError::InvalidKeyFile));
    assert_eq!(error.to_string(), "invalid key file");
    assert_eq!(std::fs::read(&key_path).unwrap(), invalid_key);
    assert!(config.key_file_path.is_none());
    assert!(!paths.config.exists());
}

#[test]
fn key_generation_refuses_to_overwrite_existing_destination() {
    let tmp = tempdir().unwrap();
    let key_path = tmp.path().join("recovery.key");
    KeyFile::generate_to_path(&key_path).unwrap();
    let original_key_file = std::fs::read(&key_path).unwrap();

    let result = KeyFile::generate_to_path(&key_path);

    assert!(result.is_err());
    assert_eq!(std::fs::read(&key_path).unwrap(), original_key_file);
}

#[cfg(unix)]
#[test]
fn generated_recovery_key_is_owner_read_write_only() {
    use std::os::unix::fs::PermissionsExt;

    let tmp = tempdir().unwrap();
    let key_path = tmp.path().join("recovery.key");

    KeyFile::generate_to_path(&key_path).unwrap();

    let mode = std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn key_export_refuses_to_overwrite_existing_destination() {
    let tmp = tempdir().unwrap();
    let destination = tmp.path().join("export.key");
    let source = tmp.path().join("source.key");
    KeyFile::generate_to_path(&destination).unwrap();
    let key = KeyFile::generate_to_path(&source).unwrap();
    let original_destination = std::fs::read(&destination).unwrap();

    let result = key.save_to_path(&destination);

    assert!(result.is_err());
    assert_eq!(std::fs::read(&destination).unwrap(), original_destination);
}

#[test]
fn current_v1_key_yaml_remains_loadable() {
    let tmp = tempdir().unwrap();
    let key_path = tmp.path().join("recovery.key");
    std::fs::write(
        &key_path,
        "version: 1\nalgorithm: XChaCha20Poly1305-compatible-32-byte-key\nkey: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
    )
    .unwrap();

    KeyFile::load_from_path(&key_path).unwrap();
}

#[test]
fn generated_and_exported_keys_switch_to_v2_with_full_v2_production_writes() {
    let tmp = tempdir().unwrap();
    let generated_path = tmp.path().join("generated.key");
    let v2_source_path = tmp.path().join("v2-source.key");
    let exported_path = tmp.path().join("exported.key");

    KeyFile::generate_to_path(&generated_path).unwrap();
    std::fs::write(
        &v2_source_path,
        "version: 2\nkdf: HKDF-SHA256\nalgorithm: XChaCha20-Poly1305\nmaster_key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
    )
    .unwrap();
    KeyFile::load_from_path(&v2_source_path)
        .unwrap()
        .save_to_path(&exported_path)
        .unwrap();

    for path in [&generated_path, &exported_path] {
        let parsed: EmittedV2KeyFile =
            serde_yaml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(parsed.version, 2);
        assert_eq!(parsed.kdf, "HKDF-SHA256");
        assert_eq!(parsed.algorithm, "XChaCha20-Poly1305");
        assert_eq!(
            base64::Engine::decode(
                &base64::engine::general_purpose::STANDARD,
                parsed.master_key
            )
            .unwrap()
            .len(),
            32
        );
    }
}

#[test]
fn v2_key_yaml_loads() {
    let tmp = tempdir().unwrap();
    let v2_path = tmp.path().join("v2.key");
    std::fs::write(
        &v2_path,
        "version: 2\nkdf: HKDF-SHA256\nalgorithm: XChaCha20-Poly1305\nmaster_key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
    )
    .unwrap();

    KeyFile::load_from_path(&v2_path).unwrap();
}

#[test]
fn unknown_key_versions_and_algorithms_are_rejected() {
    let tmp = tempdir().unwrap();
    let cases = [
        "version: 9\nalgorithm: XChaCha20Poly1305-compatible-32-byte-key\nkey: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        "version: 1\nalgorithm: unknown\nkey: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        "version: 2\nkdf: HKDF-SHA256\nalgorithm: unknown\nmaster_key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        "version: 2\nkdf: unknown\nalgorithm: XChaCha20-Poly1305\nmaster_key: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
    ];

    for (index, contents) in cases.into_iter().enumerate() {
        let path = tmp.path().join(format!("invalid-{index}.key"));
        std::fs::write(&path, contents).unwrap();
        assert!(matches!(
            KeyFile::load_from_path(path),
            Err(LiosError::InvalidKeyFile)
        ));
    }
}

#[test]
fn credentials_roundtrip_through_platform_protection() {
    let tmp = tempdir().unwrap();
    let path = tmp.path().join("credentials.enc");
    let token = "ms-token-example";

    protect_to_file(token, &path).unwrap();
    let loaded = unprotect_from_file(&path).unwrap();

    assert_eq!(loaded, token);
    #[cfg(windows)]
    assert_ne!(std::fs::read(&path).unwrap(), token.as_bytes());
}

#[test]
fn task_store_persists_progress_and_terminal_state() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let task = TaskRecord::queued("upload album", 4096);

    store.insert(&task).unwrap();
    store.update_progress(task.id, 1024, 4096).unwrap();
    store
        .update_state(
            task.id,
            TaskState::Failed,
            Some("network timeout".to_string()),
        )
        .unwrap();
    drop(store);

    let reopened = TaskStore::open(&db_path).unwrap();
    let tasks = reopened.list().unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].label, "upload album");
    assert_eq!(tasks[0].progress_done, 1024);
    assert_eq!(tasks[0].progress_total, 4096);
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert_eq!(tasks[0].error.as_deref(), Some("network timeout"));
}

#[test]
fn task_store_persists_phase() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let task = TaskRecord::queued("upload album", 0);

    store.insert(&task).unwrap();
    store
        .update_phase(task.id, Some("preparing".to_string()))
        .unwrap();
    drop(store);

    let reopened = TaskStore::open(&db_path).unwrap();
    let tasks = reopened.list().unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].phase.as_deref(), Some("preparing"));
}

#[test]
fn task_store_persists_transfer_bytes_and_speed() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let task = TaskRecord::queued("upload album", 0);

    store.insert(&task).unwrap();
    store
        .update_transfer(task.id, 2, 8, 256, 1024, 128)
        .unwrap();

    let tasks = store.list().unwrap();
    assert_eq!(tasks[0].progress_done, 2);
    assert_eq!(tasks[0].progress_total, 8);
    assert_eq!(tasks[0].bytes_done, 256);
    assert_eq!(tasks[0].bytes_total, 1024);
    assert_eq!(tasks[0].speed_bps, 128);
}

#[test]
fn task_store_marks_running_tasks_interrupted() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let running = TaskRecord::queued("upload", 0);
    let completed = TaskRecord::queued("download", 1);

    store.insert(&running).unwrap();
    store.insert(&completed).unwrap();
    store
        .update_state(running.id, TaskState::Running, None)
        .unwrap();
    store
        .update_phase(running.id, Some("preparing".to_string()))
        .unwrap();
    store
        .update_state(completed.id, TaskState::Completed, None)
        .unwrap();

    store.mark_running_interrupted("app exited").unwrap();

    let tasks = store.list().unwrap();
    let running = tasks.iter().find(|task| task.id == running.id).unwrap();
    let completed = tasks.iter().find(|task| task.id == completed.id).unwrap();
    assert_eq!(running.state, TaskState::Failed);
    assert_eq!(running.phase, None);
    assert_eq!(running.error.as_deref(), Some("app exited"));
    assert_eq!(completed.state, TaskState::Completed);
}

#[test]
fn task_store_can_delete_a_task_record() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let first = TaskRecord::queued("upload album", 2);
    let second = TaskRecord::queued("download album", 1);

    store.insert(&first).unwrap();
    store.insert(&second).unwrap();
    store.delete(first.id).unwrap();

    let tasks = store.list().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, second.id);
    assert_eq!(tasks[0].label, "download album");
}

#[test]
fn task_store_persists_specs_items_checkpoints_and_content_index() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let spec = TaskSpec::Upload {
        account_id: "account-a".to_string(),
        space_id: "novix/cold".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        parent_node_id: "root".to_string(),
        source_paths: vec![tmp.path().join("album.bin")],
        source_snapshot: None,
        chunk_size: 128 * 1024 * 1024,
        conflict_resolutions: vec![ConflictResolution {
            source_path: tmp.path().join("album.bin").to_string_lossy().into_owned(),
            action: ConflictAction::KeepBoth,
        }],
    };
    let task = TaskRecord::queued_for_spec(&spec);
    let item = TaskItem {
        id: Uuid::new_v4(),
        task_id: task.id,
        name: "album.bin".to_string(),
        relative_path: Some("photos/album.bin".into()),
        source_path: Some(tmp.path().join("album.bin")),
        source_modified_at_ns: Some(123456789),
        size: 4096,
        state: TaskItemState::Running,
        phase: Some("uploading".to_string()),
        bytes_done: 1024,
        bytes_total: 4096,
        error: None,
    };
    let checkpoint = TaskObjectCheckpoint {
        task_id: task.id,
        remote_path: "objects/files/a/chunks/b.lios".to_string(),
        oid: "a".repeat(64),
        size: 1024,
        state: CheckpointState::Uploaded,
    };
    let content = FileContentIndexEntry {
        account_id: "account-a".to_string(),
        space_id: "novix/cold".to_string(),
        content_sha256: "b".repeat(64),
        object_id: "object-a".to_string(),
        size: 4096,
        updated_at: "2026-07-11T00:00:00Z".to_string(),
    };

    store.insert_with_spec(&task, &spec).unwrap();
    store.upsert_item(&item).unwrap();
    store.upsert_checkpoint(&checkpoint).unwrap();
    store.upsert_content_index(&content).unwrap();
    let mut updated_task = task.clone();
    updated_task.state = TaskState::Preparing;
    updated_task.progress_total = 4;
    store.insert(&updated_task).unwrap();
    drop(store);

    let reopened = TaskStore::open(&db_path).unwrap();
    let loaded_spec = reopened.load_spec(task.id).unwrap().unwrap();
    match loaded_spec {
        TaskSpec::Upload {
            account_id,
            space_id,
            repo,
            parent_node_id,
            source_paths,
            chunk_size,
            conflict_resolutions,
            ..
        } => {
            assert_eq!(account_id, "account-a");
            assert_eq!(space_id, "novix/cold");
            assert_eq!(repo.namespace, "novix");
            assert_eq!(repo.dataset, "cold");
            assert_eq!(repo.endpoint, "https://modelscope.cn");
            assert_eq!(parent_node_id, "root");
            assert_eq!(source_paths, vec![tmp.path().join("album.bin")]);
            assert_eq!(chunk_size, 128 * 1024 * 1024);
            assert_eq!(conflict_resolutions.len(), 1);
            assert_eq!(conflict_resolutions[0].action, ConflictAction::KeepBoth);
        }
        other => panic!("expected upload task spec, got {other:?}"),
    }
    let tasks = reopened.list().unwrap();
    assert_eq!(tasks[0].account_id, "account-a");
    assert_eq!(tasks[0].space_id, "novix/cold");
    assert_eq!(tasks[0].state, TaskState::Preparing);
    assert_eq!(tasks[0].progress_total, 4);
    assert_eq!(tasks[0].items, vec![item.clone()]);
    assert_eq!(
        reopened.list_checkpoints(task.id).unwrap(),
        vec![checkpoint]
    );
    assert_eq!(
        reopened
            .find_content_index("account-a", "novix/cold", &"b".repeat(64))
            .unwrap(),
        Some(content)
    );

    reopened.delete(task.id).unwrap();
    assert!(reopened.list_items(task.id).unwrap().is_empty());
    assert!(reopened.list_checkpoints(task.id).unwrap().is_empty());
}

#[test]
fn task_store_persists_catalog_commit_checkpoint_hashes() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let task = TaskRecord::queued("upload", 1);
    store.insert(&task).unwrap();
    let checkpoint = TaskCatalogCheckpoint {
        task_id: task.id,
        base_catalog_sha256: Some("a".repeat(64)),
        target_catalog_sha256: "b".repeat(64),
    };

    store.upsert_catalog_checkpoint(&checkpoint).unwrap();
    drop(store);

    let reopened = TaskStore::open(&db_path).unwrap();
    assert_eq!(
        reopened.load_catalog_checkpoint(task.id).unwrap(),
        Some(checkpoint)
    );
}

#[test]
fn task_store_replaces_catalog_transaction_checkpoints_atomically() {
    let tmp = tempdir().unwrap();
    let mut store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
    let task = TaskRecord::queued("upload", 2);
    store.insert(&task).unwrap();
    let first_catalog = TaskCatalogCheckpoint {
        task_id: task.id,
        base_catalog_sha256: Some("a".repeat(64)),
        target_catalog_sha256: "b".repeat(64),
    };
    let first_objects = vec![
        TaskObjectCheckpoint {
            task_id: task.id,
            remote_path: "catalog.enc".to_string(),
            oid: "b".repeat(64),
            size: 10,
            state: CheckpointState::Pending,
        },
        TaskObjectCheckpoint {
            task_id: task.id,
            remote_path: "objects/files/old/manifest.enc".to_string(),
            oid: "c".repeat(64),
            size: 20,
            state: CheckpointState::Pending,
        },
    ];

    store
        .replace_transaction_checkpoints(&first_catalog, &first_objects)
        .unwrap();

    let second_catalog = TaskCatalogCheckpoint {
        task_id: task.id,
        base_catalog_sha256: Some("a".repeat(64)),
        target_catalog_sha256: "d".repeat(64),
    };
    let second_objects = vec![TaskObjectCheckpoint {
        task_id: task.id,
        remote_path: "catalog.enc".to_string(),
        oid: "d".repeat(64),
        size: 11,
        state: CheckpointState::Pending,
    }];
    store
        .replace_transaction_checkpoints(&second_catalog, &second_objects)
        .unwrap();

    assert_eq!(
        store.load_catalog_checkpoint(task.id).unwrap(),
        Some(second_catalog)
    );
    assert_eq!(store.list_checkpoints(task.id).unwrap(), second_objects);
}

#[test]
fn task_store_requeues_a_committing_task_for_safe_replay() {
    let tmp = tempdir().unwrap();
    let mut store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
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
    let mut task = TaskRecord::queued_for_spec(&spec);
    task.state = TaskState::Committing;
    task.phase = Some("publishing".to_string());
    task.progress_total = 3;
    task.progress_done = 2;
    task.bytes_total = 100;
    task.bytes_done = 100;
    task.speed_bps = 50;
    task.eta_seconds = Some(1);
    let mut item = TaskItem {
        id: Uuid::new_v4(),
        task_id: task.id,
        name: "file.bin".to_string(),
        relative_path: Some("file.bin".into()),
        source_path: None,
        source_modified_at_ns: None,
        size: 100,
        state: TaskItemState::Running,
        phase: Some("committing".to_string()),
        bytes_done: 100,
        bytes_total: 100,
        error: Some("old warning".to_string()),
    };
    task.items = vec![item.clone()];
    store
        .insert_with_spec_and_items(&task, &spec, &task.items)
        .unwrap();

    assert!(store.requeue_committing(task.id).unwrap());

    let replay = store.get(task.id).unwrap().unwrap();
    assert_eq!(replay.state, TaskState::Queued);
    assert_eq!(replay.phase, None);
    assert_eq!(replay.progress_done, 0);
    assert_eq!(replay.bytes_done, 0);
    assert_eq!(replay.speed_bps, 0);
    assert_eq!(replay.eta_seconds, None);
    assert_eq!(replay.attempt, 1);
    item.state = TaskItemState::Queued;
    item.phase = None;
    item.bytes_done = 0;
    item.error = None;
    assert_eq!(replay.items, vec![item]);
}

#[test]
fn task_store_completes_a_reconciled_commit_and_its_checkpoints() {
    let tmp = tempdir().unwrap();
    let mut store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
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
    let mut task = TaskRecord::queued_for_spec(&spec);
    task.state = TaskState::Committing;
    task.phase = Some("reconciling".to_string());
    task.progress_total = 2;
    task.progress_done = 1;
    task.bytes_total = 10;
    task.bytes_done = 10;
    let item = TaskItem {
        id: Uuid::new_v4(),
        task_id: task.id,
        name: "file.bin".to_string(),
        relative_path: Some("file.bin".into()),
        source_path: None,
        source_modified_at_ns: None,
        size: 10,
        state: TaskItemState::Running,
        phase: Some("committing".to_string()),
        bytes_done: 10,
        bytes_total: 10,
        error: None,
    };
    task.items = vec![item];
    store
        .insert_with_spec_and_items(&task, &spec, &task.items)
        .unwrap();
    store
        .upsert_checkpoint(&TaskObjectCheckpoint {
            task_id: task.id,
            remote_path: "catalog.enc".to_string(),
            oid: "c".repeat(64),
            size: 10,
            state: CheckpointState::Uploaded,
        })
        .unwrap();

    assert!(store.complete_reconciled_commit(task.id).unwrap());

    let completed = store.get(task.id).unwrap().unwrap();
    assert_eq!(completed.state, TaskState::Completed);
    assert_eq!(completed.phase, None);
    assert_eq!(completed.progress_done, completed.progress_total);
    assert_eq!(completed.bytes_done, completed.bytes_total);
    assert_eq!(completed.items[0].state, TaskItemState::Completed);
    assert_eq!(
        completed.items[0].bytes_done,
        completed.items[0].bytes_total
    );
    assert_eq!(
        store.list_checkpoints(task.id).unwrap()[0].state,
        CheckpointState::Committed
    );
    assert!(!store.complete_reconciled_commit(task.id).unwrap());
}

#[test]
fn task_store_fails_a_reconciled_conflict_atomically_without_discarding_checkpoints() {
    let tmp = tempdir().unwrap();
    let mut store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
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
    let mut task = TaskRecord::queued_for_spec(&spec);
    task.state = TaskState::Committing;
    task.phase = Some("reconciling".to_string());
    let item = TaskItem {
        id: Uuid::new_v4(),
        task_id: task.id,
        name: "file.bin".to_string(),
        relative_path: Some("file.bin".into()),
        source_path: None,
        source_modified_at_ns: None,
        size: 10,
        state: TaskItemState::Running,
        phase: Some("committing".to_string()),
        bytes_done: 10,
        bytes_total: 10,
        error: None,
    };
    task.items = vec![item];
    store
        .insert_with_spec_and_items(&task, &spec, &task.items)
        .unwrap();
    let later = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&later, &spec).unwrap();
    let checkpoint = TaskObjectCheckpoint {
        task_id: task.id,
        remote_path: "catalog.enc".to_string(),
        oid: "c".repeat(64),
        size: 10,
        state: CheckpointState::Uploaded,
    };
    store.upsert_checkpoint(&checkpoint).unwrap();

    assert!(store
        .fail_reconciled_commit(task.id, "remote catalog changed")
        .unwrap());

    let failed = store.get(task.id).unwrap().unwrap();
    assert_eq!(failed.state, TaskState::Failed);
    assert_eq!(failed.phase, None);
    assert_eq!(failed.error.as_deref(), Some("remote catalog changed"));
    assert_eq!(failed.items[0].state, TaskItemState::Failed);
    assert_eq!(
        failed.items[0].error.as_deref(),
        Some("remote catalog changed")
    );
    assert_eq!(store.list_checkpoints(task.id).unwrap(), vec![checkpoint]);
    let later = store.get(later.id).unwrap().unwrap();
    assert_eq!(later.state, TaskState::Failed);
    assert_eq!(later.error.as_deref(), Some("remote catalog changed"));
    assert!(!store
        .fail_reconciled_commit(task.id, "second conflict")
        .unwrap());
}

#[test]
fn task_store_makes_committing_monotonic_against_pause_cancel_and_progress() {
    let tmp = tempdir().unwrap();
    let store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
    let running = TaskRecord::queued("upload", 1);
    store.insert(&running).unwrap();
    store
        .update_state(running.id, TaskState::Running, None)
        .unwrap();

    assert!(store
        .set_transaction_state(running.id, TaskState::Committing)
        .unwrap());
    assert!(!store.interrupt_task(running.id, TaskState::Paused).unwrap());
    assert!(!store
        .interrupt_task(running.id, TaskState::Canceled)
        .unwrap());
    assert!(!store
        .set_transaction_state(running.id, TaskState::Running)
        .unwrap());
    assert!(!store
        .schedule_retry(running.id, 1, "network unavailable")
        .unwrap());
    assert_eq!(
        store.get(running.id).unwrap().unwrap().state,
        TaskState::Committing
    );

    let canceled = TaskRecord::queued("upload", 1);
    store.insert(&canceled).unwrap();
    store
        .update_state(canceled.id, TaskState::Running, None)
        .unwrap();
    assert!(store
        .interrupt_task(canceled.id, TaskState::Canceled)
        .unwrap());
    assert!(!store
        .set_transaction_state(canceled.id, TaskState::Committing)
        .unwrap());
    assert!(!store
        .schedule_retry(canceled.id, 1, "network unavailable")
        .unwrap());
    assert_eq!(
        store.get(canceled.id).unwrap().unwrap().state,
        TaskState::Canceled
    );
}

#[test]
fn task_store_fails_later_queued_tasks_when_a_space_conflicts() {
    let tmp = tempdir().unwrap();
    let mut store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
    let make_spec = |space_id: &str| TaskSpec::Delete {
        account_id: "a".repeat(64),
        space_id: space_id.to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        node_ids: vec!["node-a".to_string()],
    };
    let blocked_spec = make_spec(&"b".repeat(64));
    let other_spec = make_spec(&"c".repeat(64));
    let blocked_a = TaskRecord::queued_for_spec(&blocked_spec);
    let blocked_b = TaskRecord::queued_for_spec(&blocked_spec);
    let other = TaskRecord::queued_for_spec(&other_spec);
    store.insert_with_spec(&blocked_a, &blocked_spec).unwrap();
    store.insert_with_spec(&blocked_b, &blocked_spec).unwrap();
    store.insert_with_spec(&other, &other_spec).unwrap();

    assert_eq!(
        store
            .fail_queued_tasks_in_space(&"b".repeat(64), "remote catalog conflict")
            .unwrap(),
        2
    );

    assert!(store.list().unwrap().into_iter().all(|task| {
        if task.space_id == "b".repeat(64) {
            task.state == TaskState::Failed
                && task.error.as_deref() == Some("remote catalog conflict")
        } else {
            task.state == TaskState::Queued
        }
    }));
}

#[test]
fn task_store_rolls_back_submission_when_any_item_is_invalid() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let mut store = TaskStore::open(&db_path).unwrap();
    let spec = TaskSpec::Upload {
        account_id: "account-a".to_string(),
        space_id: "space-a".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        parent_node_id: "root".to_string(),
        source_paths: vec![tmp.path().join("album.bin")],
        source_snapshot: None,
        chunk_size: 128 * 1024 * 1024,
        conflict_resolutions: Vec::new(),
    };
    let task = TaskRecord::queued_for_spec(&spec);
    let invalid_item = TaskItem {
        id: Uuid::new_v4(),
        task_id: task.id,
        name: "album.bin".to_string(),
        relative_path: Some("album.bin".into()),
        source_path: Some(tmp.path().join("album.bin")),
        source_modified_at_ns: Some(123456789),
        size: u64::MAX,
        state: TaskItemState::Queued,
        phase: None,
        bytes_done: 0,
        bytes_total: u64::MAX,
        error: None,
    };

    assert!(matches!(
        store
            .insert_with_spec_and_items(&task, &spec, &[invalid_item])
            .unwrap_err(),
        LiosError::DataCorruption(_)
    ));
    assert!(store.get(task.id).unwrap().is_none());
    assert!(store.list_items(task.id).unwrap().is_empty());
}

#[test]
fn task_store_updates_all_file_items_to_a_terminal_state() {
    let tmp = tempdir().unwrap();
    let store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
    let task = TaskRecord::queued("upload", 1);
    store.insert(&task).unwrap();
    store
        .upsert_item(&TaskItem {
            id: Uuid::new_v4(),
            task_id: task.id,
            name: "album.bin".to_string(),
            relative_path: Some("album.bin".into()),
            source_path: Some(tmp.path().join("album.bin")),
            source_modified_at_ns: Some(123456789),
            size: 4096,
            state: TaskItemState::Running,
            phase: Some("uploading".to_string()),
            bytes_done: 1024,
            bytes_total: 4096,
            error: None,
        })
        .unwrap();

    store
        .update_items_state(
            task.id,
            TaskItemState::Canceled,
            None,
            Some("task canceled".to_string()),
            false,
        )
        .unwrap();

    let item = store.list_items(task.id).unwrap().remove(0);
    assert_eq!(item.state, TaskItemState::Canceled);
    assert_eq!(item.phase, None);
    assert_eq!(item.bytes_done, 1024);
    assert_eq!(item.error.as_deref(), Some("task canceled"));
}

#[test]
fn task_store_schedules_automatic_retry_and_requeues_manual_retry() {
    let tmp = tempdir().unwrap();
    let mut store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
    let spec = TaskSpec::Upload {
        account_id: "account-a".to_string(),
        space_id: "space-a".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        parent_node_id: "root".to_string(),
        source_paths: vec![tmp.path().join("album.bin")],
        source_snapshot: None,
        chunk_size: 128 * 1024 * 1024,
        conflict_resolutions: Vec::new(),
    };
    let task = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&task, &spec).unwrap();
    store
        .update_state(task.id, TaskState::Running, None)
        .unwrap();
    store
        .upsert_item(&TaskItem {
            id: Uuid::new_v4(),
            task_id: task.id,
            name: "album.bin".to_string(),
            relative_path: Some("album.bin".into()),
            source_path: Some(tmp.path().join("album.bin")),
            source_modified_at_ns: Some(123456789),
            size: 4096,
            state: TaskItemState::Running,
            phase: Some("uploading".to_string()),
            bytes_done: 2048,
            bytes_total: 4096,
            error: None,
        })
        .unwrap();

    assert!(store
        .schedule_retry(task.id, 1, "network unavailable")
        .unwrap());
    let retrying = store.get(task.id).unwrap().unwrap();
    assert_eq!(retrying.state, TaskState::Retrying);
    assert_eq!(retrying.phase.as_deref(), Some("retrying"));
    assert_eq!(retrying.attempt, 1);
    assert_eq!(retrying.error.as_deref(), Some("network unavailable"));

    store
        .update_state(
            task.id,
            TaskState::Failed,
            Some("network unavailable".to_string()),
        )
        .unwrap();
    assert!(store.requeue_failed(task.id).unwrap());
    let queued = store.get(task.id).unwrap().unwrap();
    assert_eq!(queued.state, TaskState::Queued);
    assert_eq!(queued.phase, None);
    assert_eq!(queued.attempt, 0);
    assert_eq!(queued.error, None);
    let item = store.list_items(task.id).unwrap().remove(0);
    assert_eq!(item.state, TaskItemState::Queued);
    assert_eq!(item.phase, None);
    assert_eq!(item.bytes_done, 0);
    assert_eq!(item.error, None);
}

#[test]
fn task_store_rejects_unknown_persisted_states_instead_of_requeueing_them() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let task = TaskRecord::queued("unknown state", 0);
    store.insert(&task).unwrap();
    drop(store);

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute(
            "UPDATE tasks SET state = 'FutureState' WHERE id = ?1",
            rusqlite::params![task.id.to_string()],
        )
        .unwrap();
    drop(connection);

    let error = TaskStore::open(&db_path).unwrap().list().unwrap_err();
    assert!(matches!(error, LiosError::DataCorruption(_)));
}

#[test]
fn task_store_migrates_the_existing_v1_tasks_table_in_place() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let task_id = Uuid::new_v4();
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                label TEXT NOT NULL,
                phase TEXT,
                progress_total INTEGER NOT NULL,
                progress_done INTEGER NOT NULL,
                bytes_total INTEGER NOT NULL DEFAULT 0,
                bytes_done INTEGER NOT NULL DEFAULT 0,
                speed_bps INTEGER NOT NULL DEFAULT 0,
                error TEXT
            );
            "#,
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO tasks
                (id, state, label, phase, progress_total, progress_done,
                 bytes_total, bytes_done, speed_bps, error)
            VALUES (?1, 'Completed', 'legacy upload', NULL, 2, 2, 4096, 4096, 0, NULL)
            "#,
            rusqlite::params![task_id.to_string()],
        )
        .unwrap();
    drop(connection);

    let store = TaskStore::open(&db_path).unwrap();
    let tasks = store.list().unwrap();

    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].id, task_id);
    assert_eq!(tasks[0].state, TaskState::Completed);
    assert_eq!(tasks[0].label, "legacy upload");
    assert_eq!(tasks[0].account_id, "");
    assert_eq!(tasks[0].space_id, "");
    assert_eq!(tasks[0].progress_done, 2);
    assert_eq!(tasks[0].bytes_done, 4096);
    assert!(!tasks[0].created_at.is_empty());
    assert!(!tasks[0].updated_at.is_empty());
    assert!(tasks[0].items.is_empty());
    drop(store);

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let schema_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema_version, 4);
}

#[test]
fn task_store_serializes_concurrent_first_open_migrations() {
    use std::sync::{Arc, Barrier};

    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            r#"
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                label TEXT NOT NULL,
                progress_total INTEGER NOT NULL,
                progress_done INTEGER NOT NULL,
                error TEXT
            );
            "#,
        )
        .unwrap();
    drop(connection);

    let barrier = Arc::new(Barrier::new(4));
    let handles = (0..4)
        .map(|_| {
            let barrier = Arc::clone(&barrier);
            let db_path = db_path.clone();
            std::thread::spawn(move || {
                barrier.wait();
                TaskStore::open(db_path).unwrap();
            })
        })
        .collect::<Vec<_>>();
    for handle in handles {
        handle.join().unwrap();
    }

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let schema_version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema_version, 4);
}

#[test]
fn task_store_migrates_v2_items_with_a_nullable_relative_path() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let task_id = Uuid::new_v4();
    let item_id = Uuid::new_v4();
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            r#"
            PRAGMA user_version = 2;
            CREATE TABLE tasks (
                id TEXT PRIMARY KEY NOT NULL,
                account_id TEXT NOT NULL DEFAULT '',
                space_id TEXT NOT NULL DEFAULT '',
                state TEXT NOT NULL,
                label TEXT NOT NULL,
                phase TEXT,
                progress_total INTEGER NOT NULL,
                progress_done INTEGER NOT NULL,
                bytes_total INTEGER NOT NULL DEFAULT 0,
                bytes_done INTEGER NOT NULL DEFAULT 0,
                speed_bps INTEGER NOT NULL DEFAULT 0,
                eta_seconds INTEGER,
                attempt INTEGER NOT NULL DEFAULT 0,
                spec_json TEXT,
                created_at TEXT NOT NULL DEFAULT '',
                updated_at TEXT NOT NULL DEFAULT '',
                error TEXT
            );
            CREATE TABLE task_items (
                id TEXT PRIMARY KEY NOT NULL,
                task_id TEXT NOT NULL,
                name TEXT NOT NULL,
                source_path TEXT,
                source_modified_at_ns INTEGER,
                size INTEGER NOT NULL,
                state TEXT NOT NULL,
                phase TEXT,
                bytes_done INTEGER NOT NULL DEFAULT 0,
                bytes_total INTEGER NOT NULL DEFAULT 0,
                error TEXT,
                FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE
            );
            "#,
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO tasks (id, state, label, progress_total, progress_done) VALUES (?1, 'Queued', 'upload', 1, 0)",
            rusqlite::params![task_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO task_items (id, task_id, name, size, state) VALUES (?1, ?2, 'legacy.bin', 4, 'Queued')",
            rusqlite::params![item_id.to_string(), task_id.to_string()],
        )
        .unwrap();
    drop(connection);

    let store = TaskStore::open(&db_path).unwrap();
    let item = store.list_items(task_id).unwrap().remove(0);

    assert_eq!(item.id, item_id);
    assert_eq!(item.relative_path, None);
}

#[test]
fn task_store_current_schema_open_does_not_take_the_writer_lock() {
    use std::sync::mpsc;
    use std::time::Duration;

    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    drop(TaskStore::open(&db_path).unwrap());
    let blocker = rusqlite::Connection::open(&db_path).unwrap();
    blocker.execute_batch("BEGIN IMMEDIATE;").unwrap();

    let (result_tx, result_rx) = mpsc::channel();
    let open_path = db_path.clone();
    let handle = std::thread::spawn(move || {
        result_tx
            .send(TaskStore::open(open_path).map(drop))
            .unwrap();
    });
    let result = result_rx.recv_timeout(Duration::from_millis(500));
    blocker.execute_batch("ROLLBACK;").unwrap();
    handle.join().unwrap();

    result
        .expect("opening the current schema must not wait for the writer lock")
        .unwrap();
}

#[test]
fn task_store_recovers_only_replayable_tasks_and_resets_running_items() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let mut store = TaskStore::open(&db_path).unwrap();
    let spec = TaskSpec::Upload {
        account_id: "account-a".to_string(),
        space_id: "novix/cold".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        parent_node_id: "root".to_string(),
        source_paths: vec![tmp.path().join("source.bin")],
        source_snapshot: None,
        chunk_size: 128 * 1024 * 1024,
        conflict_resolutions: Vec::new(),
    };
    let replayable_states = [
        TaskState::Preparing,
        TaskState::Running,
        TaskState::Retrying,
    ];
    let mut replayable_ids = Vec::new();
    for state in replayable_states {
        let task = TaskRecord::queued_for_spec(&spec);
        store.insert_with_spec(&task, &spec).unwrap();
        store.update_state(task.id, state, None).unwrap();
        replayable_ids.push(task.id);
    }
    let running_item = TaskItem {
        id: Uuid::new_v4(),
        task_id: replayable_ids[1],
        name: "source.bin".to_string(),
        relative_path: Some("source.bin".into()),
        source_path: Some(tmp.path().join("source.bin")),
        source_modified_at_ns: Some(123456789),
        size: 64,
        state: TaskItemState::Running,
        phase: Some("uploading".to_string()),
        bytes_done: 32,
        bytes_total: 64,
        error: Some("stale".to_string()),
    };
    store.upsert_item(&running_item).unwrap();

    let paused = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&paused, &spec).unwrap();
    store
        .update_state(paused.id, TaskState::Paused, None)
        .unwrap();
    let committing = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&committing, &spec).unwrap();
    store
        .update_state(committing.id, TaskState::Committing, None)
        .unwrap();
    let already_queued = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&already_queued, &spec).unwrap();
    let legacy_running = TaskRecord::queued("legacy", 0);
    store.insert(&legacy_running).unwrap();
    store
        .update_state(legacy_running.id, TaskState::Running, None)
        .unwrap();
    let invalid_spec = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&invalid_spec, &spec).unwrap();
    store
        .update_state(invalid_spec.id, TaskState::Running, None)
        .unwrap();
    store
        .upsert_item(&TaskItem {
            id: Uuid::new_v4(),
            task_id: invalid_spec.id,
            name: "invalid.bin".to_string(),
            relative_path: None,
            source_path: None,
            source_modified_at_ns: None,
            size: 1,
            state: TaskItemState::Running,
            phase: Some("uploading".to_string()),
            bytes_done: 0,
            bytes_total: 1,
            error: None,
        })
        .unwrap();
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute(
            "UPDATE tasks SET spec_json = '{broken' WHERE id = ?1",
            rusqlite::params![invalid_spec.id.to_string()],
        )
        .unwrap();
    drop(connection);
    for state in [TaskState::Completed, TaskState::Failed, TaskState::Canceled] {
        let task = TaskRecord::queued_for_spec(&spec);
        store.insert_with_spec(&task, &spec).unwrap();
        store.update_state(task.id, state, None).unwrap();
    }

    let report = store
        .recover_after_restart("legacy task cannot be resumed")
        .unwrap();
    assert_eq!(report.requeued, replayable_ids.len());
    assert_eq!(report.failed_unrecoverable, 1);
    assert_eq!(report.failed_invalid_spec, 1);
    assert_eq!(report.needs_reconciliation, 1);

    let tasks = store.list().unwrap();
    for task in tasks
        .iter()
        .filter(|task| replayable_ids.contains(&task.id))
    {
        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(task.attempt, 1);
        assert_eq!(task.phase, None);
    }
    let recovered_item = store.list_items(replayable_ids[1]).unwrap().remove(0);
    assert_eq!(recovered_item.state, TaskItemState::Queued);
    assert_eq!(recovered_item.phase, None);
    assert_eq!(recovered_item.error, None);
    let queued = tasks
        .iter()
        .find(|task| task.id == already_queued.id)
        .unwrap();
    assert_eq!(queued.state, TaskState::Queued);
    assert_eq!(queued.attempt, 0);
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == paused.id)
            .unwrap()
            .state,
        TaskState::Paused
    );
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.id == committing.id)
            .unwrap()
            .state,
        TaskState::Committing
    );
    let legacy = tasks
        .iter()
        .find(|task| task.id == legacy_running.id)
        .unwrap();
    assert_eq!(legacy.state, TaskState::Failed);
    assert_eq!(
        legacy.error.as_deref(),
        Some("legacy task cannot be resumed")
    );
    let invalid = tasks
        .iter()
        .find(|task| task.id == invalid_spec.id)
        .unwrap();
    assert_eq!(invalid.state, TaskState::Failed);
    assert_eq!(
        invalid.error.as_deref(),
        Some("persisted task specification is invalid")
    );
    let invalid_item = store.list_items(invalid_spec.id).unwrap().remove(0);
    assert_eq!(invalid_item.state, TaskItemState::Failed);
    assert_eq!(
        invalid_item.error.as_deref(),
        Some("persisted task specification is invalid")
    );
    assert_eq!(
        tasks
            .iter()
            .filter(|task| matches!(
                task.state,
                TaskState::Completed | TaskState::Failed | TaskState::Canceled
            ))
            .count(),
        5
    );
}

#[test]
fn task_store_rejects_out_of_range_writes_and_negative_persisted_numbers() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let store = TaskStore::open(&db_path).unwrap();
    let mut oversized = TaskRecord::queued("oversized", 0);
    oversized.bytes_total = u64::MAX;
    assert!(matches!(
        store.insert(&oversized).unwrap_err(),
        LiosError::DataCorruption(_)
    ));

    let task = TaskRecord::queued("negative", 0);
    store.insert(&task).unwrap();
    let oversized_item = TaskItem {
        id: Uuid::new_v4(),
        task_id: task.id,
        name: "oversized.bin".to_string(),
        relative_path: None,
        source_path: None,
        source_modified_at_ns: None,
        size: u64::MAX,
        state: TaskItemState::Queued,
        phase: None,
        bytes_done: 0,
        bytes_total: u64::MAX,
        error: None,
    };
    assert!(matches!(
        store.upsert_item(&oversized_item).unwrap_err(),
        LiosError::DataCorruption(_)
    ));
    assert!(matches!(
        store
            .upsert_checkpoint(&TaskObjectCheckpoint {
                task_id: task.id,
                remote_path: "objects/oversized".to_string(),
                oid: "a".repeat(64),
                size: u64::MAX,
                state: CheckpointState::Pending,
            })
            .unwrap_err(),
        LiosError::DataCorruption(_)
    ));
    assert!(matches!(
        store
            .upsert_content_index(&FileContentIndexEntry {
                account_id: "account-a".to_string(),
                space_id: "novix/cold".to_string(),
                content_sha256: "b".repeat(64),
                object_id: "object-a".to_string(),
                size: u64::MAX,
                updated_at: "2026-07-11T00:00:00Z".to_string(),
            })
            .unwrap_err(),
        LiosError::DataCorruption(_)
    ));
    drop(store);
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute(
            "UPDATE tasks SET bytes_total = -1 WHERE id = ?1",
            rusqlite::params![task.id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            r#"
            INSERT INTO task_items
                (id, task_id, name, size, state, bytes_done, bytes_total)
            VALUES (?1, ?2, 'negative.bin', -1, 'Queued', 0, 0)
            "#,
            rusqlite::params![Uuid::new_v4().to_string(), task.id.to_string()],
        )
        .unwrap();
    drop(connection);

    let reopened = TaskStore::open(&db_path).unwrap();
    assert!(matches!(
        reopened.list().unwrap_err(),
        LiosError::DataCorruption(_)
    ));
    assert!(matches!(
        reopened.list_items(task.id).unwrap_err(),
        LiosError::DataCorruption(_)
    ));
}

#[test]
fn task_store_lists_valid_queued_specs_and_claims_each_task_once() {
    let tmp = tempdir().unwrap();
    let db_path = tmp.path().join("lios.db");
    let mut store = TaskStore::open(&db_path).unwrap();
    let spec = TaskSpec::Delete {
        account_id: "account-a".to_string(),
        space_id: "novix/cold".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        node_ids: vec!["node-a".to_string()],
    };
    let queued = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&queued, &spec).unwrap();
    let completed = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&completed, &spec).unwrap();
    store
        .update_state(completed.id, TaskState::Completed, None)
        .unwrap();
    let legacy = TaskRecord::queued("legacy", 0);
    store.insert(&legacy).unwrap();
    let runnable = store.list_queued_with_specs().unwrap();
    assert_eq!(runnable.len(), 1);
    assert_eq!(runnable[0].0.id, queued.id);
    assert_eq!(runnable[0].1.space_id(), "novix/cold");

    let malformed = TaskRecord::queued_for_spec(&spec);
    store.insert_with_spec(&malformed, &spec).unwrap();
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute(
            "UPDATE tasks SET spec_json = '{broken' WHERE id = ?1",
            rusqlite::params![malformed.id.to_string()],
        )
        .unwrap();
    drop(connection);

    let claimed_spec = store.claim_queued(queued.id).unwrap().unwrap();
    assert_eq!(claimed_spec.space_id(), "novix/cold");
    let mut second_connection = TaskStore::open(&db_path).unwrap();
    assert!(second_connection.claim_queued(queued.id).unwrap().is_none());
    assert!(second_connection.claim_queued(legacy.id).unwrap().is_none());
    assert!(matches!(
        second_connection.claim_queued(malformed.id).unwrap_err(),
        LiosError::Json(_)
    ));
    assert_eq!(
        second_connection.get(malformed.id).unwrap().unwrap().state,
        TaskState::Queued
    );
    let claimed = second_connection.get(queued.id).unwrap().unwrap();
    assert_eq!(claimed.state, TaskState::Preparing);
    assert_eq!(claimed.phase.as_deref(), Some("preparing"));
}

#[test]
fn legacy_upload_task_spec_defaults_to_128mb_chunks() {
    let spec: TaskSpec = serde_json::from_value(serde_json::json!({
        "kind": "upload",
        "account_id": "account-a",
        "space_id": "novix/cold",
        "repo": {
            "namespace": "novix",
            "dataset": "cold",
            "endpoint": "https://modelscope.cn"
        },
        "parent_node_id": "root",
        "source_paths": ["C:\\source.bin"],
        "conflict_resolutions": []
    }))
    .unwrap();

    let TaskSpec::Upload {
        chunk_size,
        source_snapshot,
        ..
    } = spec
    else {
        panic!("expected upload task spec");
    };
    assert_eq!(chunk_size, 128 * 1024 * 1024);
    assert_eq!(source_snapshot, None);
}

#[test]
fn verify_task_labels_distinguish_quick_and_full_checks() {
    let repo = RepoConfig {
        namespace: "novix".to_string(),
        dataset: "archive".to_string(),
        endpoint: "https://modelscope.cn".to_string(),
    };
    let quick = TaskSpec::VerifySpace {
        account_id: "account".to_string(),
        space_id: "space".to_string(),
        repo: repo.clone(),
        full: false,
    };
    let full = TaskSpec::VerifySpace {
        account_id: "account".to_string(),
        space_id: "space".to_string(),
        repo,
        full: true,
    };

    assert_eq!(quick.label(), "verify_quick");
    assert_eq!(full.label(), "verify_full");
}

#[test]
fn rebuild_task_spec_roundtrips_the_confirmed_revision() {
    let spec = TaskSpec::RebuildCatalog {
        account_id: "account".to_string(),
        space_id: "space".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "archive".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        expected_revision: Some("commit-123".to_string()),
    };

    let encoded = serde_json::to_string(&spec).unwrap();
    let decoded: TaskSpec = serde_json::from_str(&encoded).unwrap();
    let TaskSpec::RebuildCatalog {
        expected_revision, ..
    } = decoded
    else {
        panic!("expected rebuild catalog task");
    };

    assert_eq!(expected_revision.as_deref(), Some("commit-123"));
}

#[test]
fn legacy_rebuild_task_spec_without_revision_loads_as_unconfirmed() {
    let decoded: TaskSpec = serde_json::from_value(serde_json::json!({
        "kind": "rebuild_catalog",
        "account_id": "account",
        "space_id": "space",
        "repo": {
            "namespace": "novix",
            "dataset": "archive",
            "endpoint": "https://modelscope.cn"
        }
    }))
    .unwrap();
    let TaskSpec::RebuildCatalog {
        expected_revision, ..
    } = decoded
    else {
        panic!("expected rebuild catalog task");
    };

    assert_eq!(expected_revision, None);
}

#[test]
fn upload_task_spec_roundtrips_the_persisted_source_snapshot() {
    let snapshot = SourceSnapshotReport::default();
    let spec = TaskSpec::Upload {
        account_id: "account-a".to_string(),
        space_id: "space-a".to_string(),
        repo: RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        },
        parent_node_id: "root".to_string(),
        source_paths: vec!["C:/source".into()],
        source_snapshot: Some(snapshot.clone()),
        chunk_size: 128 * 1024 * 1024,
        conflict_resolutions: Vec::new(),
    };

    let encoded = serde_json::to_string(&spec).unwrap();
    let decoded: TaskSpec = serde_json::from_str(&encoded).unwrap();
    let TaskSpec::Upload {
        source_snapshot, ..
    } = decoded
    else {
        panic!("expected upload task");
    };

    assert_eq!(source_snapshot, Some(snapshot));
}

#[test]
fn task_store_transitions_state_only_from_the_expected_value() {
    let tmp = tempdir().unwrap();
    let store = TaskStore::open(tmp.path().join("lios.db")).unwrap();
    let task = TaskRecord::queued("paused", 0);
    store.insert(&task).unwrap();
    store
        .update_state(task.id, TaskState::Paused, None)
        .unwrap();

    assert!(store
        .transition_state(task.id, TaskState::Paused, TaskState::Queued)
        .unwrap());
    assert!(!store
        .transition_state(task.id, TaskState::Paused, TaskState::Queued)
        .unwrap());
    let task = store.get(task.id).unwrap().unwrap();
    assert_eq!(task.state, TaskState::Queued);
    assert_eq!(task.phase, None);
    assert_eq!(task.error, None);
}
