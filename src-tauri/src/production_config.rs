use std::path::PathBuf;

use lios_core::config::{
    ensure_default_key_binding, validate_modelscope_production_endpoint, LiosConfig, LiosPaths,
    RepoConfig, MODELSCOPE_ENDPOINT,
};
use lios_core::crypto::KeyFile;
use serde::Serialize;

use crate::command_error::CommandError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub enum SetupWarningCode {
    ReconnectRequired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SetupWarning {
    pub code: SetupWarningCode,
    pub message: String,
}

pub fn configured_endpoint(
    config: &LiosConfig,
    endpoint: Option<String>,
) -> Result<String, CommandError> {
    let endpoint = endpoint
        .and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .or_else(|| {
            config
                .active_repo
                .as_ref()
                .map(|repo| repo.endpoint.clone())
        })
        .unwrap_or_else(|| MODELSCOPE_ENDPOINT.to_string());
    validate_modelscope_production_endpoint(&endpoint).map_err(Into::into)
}

pub fn validate_repo(repo: RepoConfig) -> Result<RepoConfig, CommandError> {
    let namespace = repo.namespace.trim();
    let dataset = repo.dataset.trim();
    if namespace.is_empty() || dataset.is_empty() {
        return Err(CommandError::invalid_input("dataset repo is incomplete"));
    }
    Ok(RepoConfig {
        namespace: namespace.to_string(),
        dataset: dataset.to_string(),
        endpoint: validate_modelscope_production_endpoint(&repo.endpoint)?,
    })
}

fn validated_config(config: &LiosConfig) -> Result<LiosConfig, CommandError> {
    let mut validated = config.clone();
    if let Some(repo) = validated.active_repo.take() {
        validated.active_repo = Some(validate_repo(repo)?);
    }
    Ok(validated)
}

fn migrate_legacy_config(
    config: &LiosConfig,
) -> Result<(LiosConfig, Option<SetupWarning>, bool), CommandError> {
    let mut migrated = config.clone();
    let Some(repo) = migrated.active_repo.take() else {
        return Ok((migrated, None, false));
    };
    if validate_modelscope_production_endpoint(&repo.endpoint).is_err() {
        return Ok((
            migrated,
            Some(SetupWarning {
                code: SetupWarningCode::ReconnectRequired,
                message: "The saved ModelScope endpoint is no longer supported; reconnect a space."
                    .to_string(),
            }),
            true,
        ));
    }

    let validated = validate_repo(repo.clone())?;
    let changed = validated.namespace != repo.namespace
        || validated.dataset != repo.dataset
        || validated.endpoint != repo.endpoint;
    migrated.active_repo = Some(validated);
    Ok((migrated, None, changed))
}

pub fn persist_config(paths: &LiosPaths, config: &mut LiosConfig) -> Result<(), CommandError> {
    let validated = validated_config(config)?;
    validated.save(&paths.config)?;
    *config = validated;
    Ok(())
}

pub fn prepare_startup_config(
    paths: &LiosPaths,
    config: &mut LiosConfig,
) -> Result<Option<SetupWarning>, CommandError> {
    let (mut prepared, warning, migrated) = migrate_legacy_config(config)?;
    let key_bound = ensure_default_key_binding(paths, &mut prepared)?;
    if migrated || key_bound {
        persist_config(paths, &mut prepared)?;
    }
    *config = prepared;
    Ok(warning)
}

pub fn prepare_config_for_write(
    paths: &LiosPaths,
    config: &mut LiosConfig,
) -> Result<Option<SetupWarning>, CommandError> {
    let (mut prepared, warning, changed) = migrate_legacy_config(config)?;
    if changed {
        persist_config(paths, &mut prepared)?;
    }
    *config = prepared;
    Ok(warning)
}

pub fn generate_key_file_and_bind(
    paths: &LiosPaths,
    path: PathBuf,
) -> Result<PathBuf, CommandError> {
    paths.ensure_dirs()?;
    let mut config = LiosConfig::load(&paths.config)?;
    prepare_config_for_write(paths, &mut config)?;
    KeyFile::generate_to_path(&path)?;
    config.key_file_path = Some(path.clone());
    if let Err(error) = persist_config(paths, &mut config) {
        let _ = std::fs::remove_file(&path);
        return Err(error);
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
    use tempfile::tempdir;

    use super::{
        configured_endpoint, generate_key_file_and_bind, persist_config, prepare_startup_config,
        validate_repo, SetupWarningCode,
    };
    use crate::command_error::CommandErrorCode;

    #[test]
    fn rejects_custom_endpoint_loaded_from_old_config() {
        let config = LiosConfig {
            active_repo: Some(RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "http://127.0.0.1:12345".to_string(),
            }),
            ..LiosConfig::default()
        };

        let error = configured_endpoint(&config, None).unwrap_err();
        assert_eq!(error.code, CommandErrorCode::InvalidInput);
    }

    #[test]
    fn validates_and_normalizes_repo_before_save_or_use() {
        let repo = validate_repo(RepoConfig {
            namespace: " novix ".to_string(),
            dataset: " cold ".to_string(),
            endpoint: "https://www.modelscope.cn/".to_string(),
        })
        .unwrap();

        assert_eq!(repo.namespace, "novix");
        assert_eq!(repo.dataset, "cold");
        assert_eq!(repo.endpoint, "https://www.modelscope.cn");
    }

    #[test]
    fn validated_persistence_rejects_old_custom_endpoint_without_rewriting() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let mut config = LiosConfig {
            active_repo: Some(RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "http://127.0.0.1:12345".to_string(),
            }),
            key_file_path: None,
            chunk_size: Some(1024),
        };
        config.save(&paths.config).unwrap();
        let original = fs::read(&paths.config).unwrap();
        config.key_file_path = Some(paths.home.join("imported.key"));

        let error = persist_config(&paths, &mut config).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        assert_eq!(fs::read(&paths.config).unwrap(), original);
    }

    #[test]
    fn startup_migrates_old_endpoint_and_persists_default_key_binding() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let mut config = LiosConfig {
            active_repo: Some(RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "http://127.0.0.1:12345".to_string(),
            }),
            key_file_path: None,
            chunk_size: None,
        };
        config.save(&paths.config).unwrap();

        let warning = prepare_startup_config(&paths, &mut config)
            .unwrap()
            .unwrap();
        let saved = LiosConfig::load(&paths.config).unwrap();

        assert_eq!(warning.code, SetupWarningCode::ReconnectRequired);
        assert!(warning.message.contains("reconnect"));
        assert!(config.active_repo.is_none());
        assert!(saved.active_repo.is_none());
        assert_eq!(saved.key_file_path, config.key_file_path);
        assert!(paths.home.join("recovery.key").exists());
    }

    #[test]
    fn invalid_config_is_rejected_before_explicit_key_file_creation() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let config = LiosConfig {
            active_repo: Some(RepoConfig {
                namespace: " ".to_string(),
                dataset: "cold".to_string(),
                endpoint: "https://modelscope.cn".to_string(),
            }),
            key_file_path: None,
            chunk_size: None,
        };
        config.save(&paths.config).unwrap();
        let target = paths.home.join("explicit.key");

        let error = generate_key_file_and_bind(&paths, target.clone()).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        assert!(!target.exists());
    }
}
