use lios_application::space_registry::SpaceRegistry;
use lios_application::CommandErrorCode;
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig, MODELSCOPE_ENDPOINT};
use tempfile::tempdir;

fn repo(namespace: &str, dataset: &str) -> RepoConfig {
    RepoConfig {
        namespace: namespace.to_string(),
        dataset: dataset.to_string(),
        endpoint: MODELSCOPE_ENDPOINT.to_string(),
    }
}

#[test]
fn registry_validates_aliases_and_forbids_duplicate_repository_addresses() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    paths.ensure_dirs().unwrap();
    LiosConfig::default().save(&paths.config).unwrap();
    let registry = SpaceRegistry::new(paths.clone());

    registry.add("photos", repo("allen", "photos")).unwrap();
    assert_eq!(registry.resolve("photos").unwrap(), repo("allen", "photos"));

    let invalid = registry.add("Photos", repo("allen", "other")).unwrap_err();
    assert_eq!(invalid.code, CommandErrorCode::InvalidInput);

    let duplicate = registry
        .add("archive", repo("allen", "photos"))
        .unwrap_err();
    assert_eq!(duplicate.code, CommandErrorCode::InvalidInput);

    registry.rename("photos", "family_photos").unwrap();
    assert!(registry.resolve("photos").is_err());
    assert_eq!(
        registry.resolve("family_photos").unwrap(),
        repo("allen", "photos")
    );
    registry.remove("family_photos").unwrap();
    assert!(registry.list().unwrap().is_empty());
}

#[test]
fn startup_backs_up_v1_and_does_not_auto_register_the_active_repository() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    paths.ensure_dirs().unwrap();
    std::fs::write(
        &paths.config,
        "active_repo:\n  namespace: allen\n  dataset: photos\n  endpoint: https://modelscope.cn\nchunk_size: 1024\n",
    )
    .unwrap();
    let application = lios_application::service::Application::new(paths.clone()).unwrap();

    let snapshot = application.setup().unwrap();

    assert!(snapshot.config.spaces.is_empty());
    assert!(!std::fs::read_to_string(&paths.config)
        .unwrap()
        .contains("active_repo"));
    assert!(paths.home.join("config.yaml.v1.bak").is_file());
    let warning = snapshot.warning.expect("migration warning");
    assert!(warning.message.contains("space add"));
    assert!(warning.message.contains("allen/photos"));
}

#[test]
fn registry_mutations_respect_the_cross_process_config_lock() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    paths.ensure_dirs().unwrap();
    LiosConfig::default().save(&paths.config).unwrap();
    let registry = SpaceRegistry::new(paths.clone());
    let _other_process = paths.try_lock_config().unwrap();

    let error = registry.add("photos", repo("allen", "photos")).unwrap_err();

    assert_eq!(error.code, CommandErrorCode::Busy);
    assert!(registry.list().unwrap().is_empty());
}
