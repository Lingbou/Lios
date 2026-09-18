use lios_application::space_registry::SpaceRegistry;
use lios_application::CommandErrorCode;
use lios_core::config::{LiosConfig, LiosPaths, RepoConfig, MODELSCOPE_ENDPOINT};
use tempfile::tempdir;

fn repo(namespace: &str, dataset: &str) -> RepoConfig {
    RepoConfig {
        namespace: namespace.to_string(),
        dataset: dataset.to_string(),
        endpoint: MODELSCOPE_ENDPOINT.to_string(),
        title: None,
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

    let duplicate_name = registry
        .ensure_can_add("photos", &repo("allen", "other"))
        .unwrap_err();
    assert_eq!(duplicate_name.code, CommandErrorCode::InvalidInput);

    let invalid = registry.add("Photos", repo("allen", "other")).unwrap_err();
    assert_eq!(invalid.code, CommandErrorCode::InvalidInput);

    let duplicate = registry
        .add("archive", repo("allen", "photos"))
        .unwrap_err();
    assert_eq!(duplicate.code, CommandErrorCode::InvalidInput);
    let duplicate_address = registry
        .ensure_can_add("archive", &repo("allen", "photos"))
        .unwrap_err();
    assert_eq!(duplicate_address.code, CommandErrorCode::InvalidInput);

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
fn startup_initializes_clean_spaces_configuration() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    paths.ensure_dirs().unwrap();
    let application = lios_application::service::Application::new(paths.clone()).unwrap();
    let snapshot = application.setup().unwrap();
    assert!(snapshot.config.spaces.is_empty());
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
