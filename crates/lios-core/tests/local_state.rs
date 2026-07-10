use lios_core::{
    config::{ensure_default_key_configured, LiosConfig, LiosPaths, RepoConfig},
    credentials::{protect_to_file, unprotect_from_file},
    crypto::KeyFile,
    tasks::{TaskRecord, TaskState, TaskStore},
};
use tempfile::tempdir;

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
