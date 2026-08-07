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

#[tokio::test]
async fn second_frontend_gets_a_typed_busy_error_for_the_same_space() {
    let (_temp, application, paths, repo) = configured_application();
    let task = application.queue_verify_for(repo, false).unwrap();
    let space_id = task.space_id.clone();
    let _first_frontend = paths.try_lock_space(&space_id).unwrap();

    let error = application
        .run_task(task.id, |_progress| {})
        .await
        .unwrap_err();

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
