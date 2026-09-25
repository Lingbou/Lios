use std::fs;

use lios_application::service::Application;
use lios_application::CommandErrorCode;
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig, MODELSCOPE_ENDPOINT};
use lios_core::tasks::{TaskState, TaskStore};
use tempfile::tempdir;

fn configured_application() -> (tempfile::TempDir, Application, LiosPaths, RepoConfig) {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    let application = Application::new(paths.clone()).unwrap();
    application.setup().unwrap();
    application.set_token("local-test-token").unwrap();
    let mut config = LiosConfig::load(&paths.config).unwrap();
    let repo = RepoConfig {
        namespace: "novix".to_string(),
        dataset: "cold".to_string(),
        endpoint: MODELSCOPE_ENDPOINT.to_string(),
        title: None,
    };
    config.spaces.insert("cold".to_string(), repo.clone());
    config.save(&paths.config).unwrap();
    (temp, application, paths, repo)
}

#[test]
fn setup_creates_shared_state_and_persists_a_token() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    let application = Application::new(paths.clone()).unwrap();

    let initial = application.setup().unwrap();
    assert!(!initial.has_token);
    assert!(initial.recovery_key.key_location.is_some());
    assert!(paths.home.is_dir());

    application.set_token("  model-scope-token  ").unwrap();
    let configured = application.setup().unwrap();
    assert!(configured.has_token);
    assert!(paths.credentials.is_file());
    assert!(fs::metadata(&paths.credentials).unwrap().len() > 0);

    application.clear_token().unwrap();
    let cleared = application.setup().unwrap();
    assert!(!cleared.has_token);
    assert!(!paths.credentials.exists());
    application.clear_token().unwrap();
}

#[test]
fn application_startup_does_not_delete_another_process_download_sidecar() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    let sidecar = paths
        .staging
        .join("a".repeat(64))
        .join("b".repeat(64))
        .join(uuid::Uuid::new_v4().to_string())
        .join("catalog.download");
    fs::create_dir_all(sidecar.parent().unwrap()).unwrap();
    fs::write(&sidecar, b"active download").unwrap();

    let _application = Application::new(paths).unwrap();

    assert_eq!(fs::read(sidecar).unwrap(), b"active download");
}

#[test]
fn foreground_task_is_durable_before_network_execution() {
    let (_temp, application, paths, repo) = configured_application();

    let task = application.queue_verify_for(repo, false).unwrap();
    let persisted = TaskStore::open(&paths.database)
        .unwrap()
        .get_summary(task.id)
        .unwrap()
        .unwrap();

    assert_eq!(persisted.state, TaskState::Queued);
    assert_eq!(persisted.label, "verify_quick");
    assert!(!persisted.can_retry);
}

#[test]
fn resuming_a_paused_task_clears_stale_worker_controls() {
    let (_temp, application, paths, repo) = configured_application();
    let task = application.queue_verify_for(repo, false).unwrap();
    TaskStore::open(&paths.database)
        .unwrap()
        .update_state(task.id, TaskState::Paused, None)
        .unwrap();
    paths.ensure_worker_control_dir().unwrap();
    fs::write(paths.worker_pause_path(task.id), b"pause\n").unwrap();
    fs::write(paths.worker_cancel_path(task.id), b"cancel\n").unwrap();

    let resumed = application.requeue_paused_task(task.id).unwrap();

    assert_eq!(resumed.state, TaskState::Queued);
    assert!(!paths.worker_pause_path(task.id).exists());
    assert!(!paths.worker_cancel_path(task.id).exists());
}

#[test]
fn retrying_a_failed_task_clears_stale_worker_controls() {
    let (_temp, application, paths, repo) = configured_application();
    let task = application.queue_verify_for(repo, false).unwrap();
    TaskStore::open(&paths.database)
        .unwrap()
        .update_state(task.id, TaskState::Failed, Some("failed".to_string()))
        .unwrap();
    paths.ensure_worker_control_dir().unwrap();
    fs::write(paths.worker_pause_path(task.id), b"pause\n").unwrap();
    fs::write(paths.worker_cancel_path(task.id), b"cancel\n").unwrap();

    let retried = application.requeue_failed_task(task.id).unwrap();

    assert_eq!(retried.state, TaskState::Queued);
    assert!(!paths.worker_pause_path(task.id).exists());
    assert!(!paths.worker_cancel_path(task.id).exists());
}

#[test]
fn clearing_a_terminal_task_removes_worker_controls() {
    let (_temp, application, paths, repo) = configured_application();
    let task = application.queue_verify_for(repo, false).unwrap();
    TaskStore::open(&paths.database)
        .unwrap()
        .update_state(task.id, TaskState::Completed, None)
        .unwrap();
    paths.ensure_worker_control_dir().unwrap();
    fs::write(paths.worker_pause_path(task.id), b"pause\n").unwrap();
    fs::write(paths.worker_cancel_path(task.id), b"cancel\n").unwrap();

    application.clear_task(task.id).unwrap();

    assert!(!paths.worker_pause_path(task.id).exists());
    assert!(!paths.worker_cancel_path(task.id).exists());
    assert!(TaskStore::open(&paths.database)
        .unwrap()
        .get_summary(task.id)
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn paused_task_is_not_executed_by_a_worker_claim_race() {
    let (_temp, application, paths, repo) = configured_application();
    let task = application.queue_verify_for(repo, false).unwrap();
    TaskStore::open(&paths.database)
        .unwrap()
        .update_state(task.id, TaskState::Paused, None)
        .unwrap();

    application.run_task(task.id).await.unwrap();

    let persisted = TaskStore::open(&paths.database)
        .unwrap()
        .get_summary(task.id)
        .unwrap()
        .unwrap();
    assert_eq!(persisted.state, TaskState::Paused);
}

#[tokio::test]
async fn second_frontend_gets_a_typed_busy_error_for_the_same_space() {
    let (_temp, application, paths, repo) = configured_application();
    let task = application.queue_verify_for(repo, false).unwrap();
    let space_id = task.space_id.clone();
    let _first_frontend = paths.try_lock_space(&space_id).unwrap();

    let error = application.run_task(task.id).await.unwrap_err();

    assert_eq!(error.code, CommandErrorCode::Busy);
    assert!(error.retryable);
    assert_eq!(
        TaskStore::open(&paths.database)
            .unwrap()
            .get_summary(task.id)
            .unwrap()
            .unwrap()
            .state,
        TaskState::Queued
    );
}
