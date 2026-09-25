//! Validation and persistence for user-facing application configuration.

use crate::command_error::CommandError;
use lios_core::config::{
    ensure_default_key_binding, validate_modelscope_production_endpoint, LiosConfig, LiosPaths,
    RepoConfig, MODELSCOPE_ENDPOINT,
};

pub fn configured_endpoint(
    config: &LiosConfig,
    endpoint: Option<String>,
) -> Result<String, CommandError> {
    let endpoint = endpoint
        .and_then(|value| {
            let trimmed = value.trim().to_string();
            (!trimmed.is_empty()).then_some(trimmed)
        })
        .or_else(|| config.endpoint.clone())
        .unwrap_or_else(|| MODELSCOPE_ENDPOINT.to_string());
    validate_modelscope_production_endpoint(&endpoint).map_err(Into::into)
}

pub fn validate_repo_identifier(name: &str, field: &'static str) -> Result<(), CommandError> {
    if name.is_empty() {
        return Err(CommandError::invalid_input(format!(
            "{field} cannot be empty"
        )));
    }
    if matches!(name, "." | "..")
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(CommandError::invalid_input(format!(
            "{field} `{name}` contains invalid or non-ASCII characters; ModelScope datasets must use ASCII letters, numbers, hyphens, and underscores (Chinese characters are not supported by ModelScope Git endpoints)"
        )));
    }
    Ok(())
}

pub fn validate_repo(repo: RepoConfig) -> Result<RepoConfig, CommandError> {
    let namespace = repo.namespace.trim();
    let dataset = repo.dataset.trim();
    if namespace.is_empty() || dataset.is_empty() {
        return Err(CommandError::invalid_input("dataset repo is incomplete"));
    }
    validate_repo_identifier(namespace, "namespace")?;
    validate_repo_identifier(dataset, "dataset")?;
    let title = repo
        .title
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty());
    Ok(RepoConfig {
        namespace: namespace.to_string(),
        dataset: dataset.to_string(),
        endpoint: validate_modelscope_production_endpoint(&repo.endpoint)?,
        title,
    })
}

fn validated_config(config: &LiosConfig) -> Result<LiosConfig, CommandError> {
    let mut validated = config.clone();
    if let Some(endpoint) = &validated.endpoint {
        validated.endpoint = Some(validate_modelscope_production_endpoint(endpoint)?);
    }
    for repo in validated.spaces.values_mut() {
        *repo = validate_repo(repo.clone())?;
    }
    Ok(validated)
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
) -> Result<(), CommandError> {
    let key_bound = ensure_default_key_binding(paths, config)?;
    if key_bound {
        persist_config(paths, config)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use lios_core::config::{LiosConfig, LiosPaths, RepoConfig};
    use tempfile::tempdir;

    use super::{configured_endpoint, persist_config, prepare_startup_config, validate_repo};
    use crate::command_error::CommandErrorCode;

    #[test]
    fn rejects_an_explicit_custom_endpoint() {
        let error = configured_endpoint(
            &LiosConfig::default(),
            Some("http://127.0.0.1:12345".to_string()),
        )
        .unwrap_err();
        assert_eq!(error.code, CommandErrorCode::InvalidInput);
    }

    #[test]
    fn rejects_non_ascii_dataset_name() {
        let error = validate_repo(RepoConfig {
            namespace: "novix".to_string(),
            dataset: "测试上传".to_string(),
            endpoint: "https://www.modelscope.cn/".to_string(),
            title: None,
        })
        .unwrap_err();
        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        assert!(error.message.contains("non-ASCII"));
    }

    #[test]
    fn rejects_repository_identifiers_that_change_url_segments() {
        for name in [".", "..", "name?query", "name#fragment", "name%2fescape"] {
            let error = validate_repo(RepoConfig {
                namespace: "novix".to_string(),
                dataset: name.to_string(),
                endpoint: "https://modelscope.cn".to_string(),
                title: None,
            })
            .unwrap_err();

            assert_eq!(error.code, CommandErrorCode::InvalidInput, "{name}");
        }
    }

    #[test]
    fn validates_and_normalizes_repo_before_save_or_use() {
        let repo = validate_repo(RepoConfig {
            namespace: " novix ".to_string(),
            dataset: " cold ".to_string(),
            endpoint: "https://www.modelscope.cn/".to_string(),
            title: None,
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
            key_file_path: None,
            backup_path: None,
            chunk_size: Some(1024),
            ..LiosConfig::default()
        };
        config.spaces.insert(
            "cold".to_string(),
            RepoConfig {
                namespace: "novix".to_string(),
                dataset: "cold".to_string(),
                endpoint: "http://127.0.0.1:12345".to_string(),
                title: None,
            },
        );
        config.save(&paths.config).unwrap();
        let original = fs::read(&paths.config).unwrap();
        config.key_file_path = Some(paths.home.join("imported.key"));

        let error = persist_config(&paths, &mut config).unwrap_err();

        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        assert_eq!(fs::read(&paths.config).unwrap(), original);
    }

    #[test]
    fn startup_binds_default_key_file() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let mut config = LiosConfig::default();
        prepare_startup_config(&paths, &mut config).unwrap();
        assert_eq!(config.key_file_path, Some(paths.home.join("recovery.key")));
        assert!(paths.home.join("recovery.key").exists());
    }
    #[test]
    fn configured_endpoint_falls_back_to_saved_config_endpoint() {
        let config = LiosConfig {
            endpoint: Some("https://www.modelscope.cn".to_string()),
            ..LiosConfig::default()
        };
        let endpoint = configured_endpoint(&config, None).unwrap();
        assert_eq!(endpoint, "https://www.modelscope.cn");

        let explicit =
            configured_endpoint(&config, Some("https://modelscope.cn/".to_string())).unwrap();
        assert_eq!(explicit, "https://modelscope.cn");
    }

    #[test]
    fn validates_and_normalizes_config_endpoint_on_persist() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        paths.ensure_dirs().unwrap();
        let mut config = LiosConfig {
            endpoint: Some("https://www.modelscope.cn/".to_string()),
            ..LiosConfig::default()
        };
        persist_config(&paths, &mut config).unwrap();
        assert_eq!(
            config.endpoint.as_deref(),
            Some("https://www.modelscope.cn")
        );

        let mut invalid_config = LiosConfig {
            endpoint: Some("http://invalid.endpoint".to_string()),
            ..LiosConfig::default()
        };
        let error = persist_config(&paths, &mut invalid_config).unwrap_err();
        assert_eq!(error.code, CommandErrorCode::InvalidInput);
    }
}
