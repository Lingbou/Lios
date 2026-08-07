//! Shared, explicit `SpaceName -> Repository Address` registration.

use std::collections::BTreeMap;

use lios_core::config::{LiosConfig, LiosPaths, RepoConfig, CONFIG_SCHEMA_VERSION};

use crate::production_config::validate_repo;
use crate::{to_err, CommandError, CommandResult};

#[derive(Debug, Clone)]
pub struct SpaceRegistry {
    paths: LiosPaths,
}

impl SpaceRegistry {
    pub fn new(paths: LiosPaths) -> Self {
        Self { paths }
    }

    pub fn list(&self) -> CommandResult<BTreeMap<String, RepoConfig>> {
        let config = self.load_v2()?;
        validate_unique_addresses(&config.spaces)?;
        Ok(config.spaces)
    }

    pub fn resolve(&self, name: &str) -> CommandResult<RepoConfig> {
        validate_space_name(name)?;
        self.list()?
            .remove(name)
            .ok_or_else(|| CommandError::invalid_input(format!("space `{name}` is not registered")))
    }

    pub fn add(&self, name: &str, repo: RepoConfig) -> CommandResult<()> {
        validate_space_name(name)?;
        let repo = validate_repo(repo)?;
        self.mutate(|spaces| {
            validate_available_registration(spaces, name, &repo)?;
            spaces.insert(name.to_string(), repo);
            Ok(())
        })
    }

    /// Preflight a registration before a remote create or initialize call.
    /// `add` repeats the validation while holding the config lock so a race
    /// still cannot publish a duplicate local registration.
    pub fn ensure_can_add(&self, name: &str, repo: &RepoConfig) -> CommandResult<()> {
        validate_space_name(name)?;
        let repo = validate_repo(repo.clone())?;
        let spaces = self.list()?;
        validate_available_registration(&spaces, name, &repo)
    }

    pub fn rename(&self, old: &str, new: &str) -> CommandResult<()> {
        validate_space_name(old)?;
        validate_space_name(new)?;
        self.mutate(|spaces| {
            if spaces.contains_key(new) {
                return Err(CommandError::invalid_input(format!(
                    "space `{new}` is already registered"
                )));
            }
            let repo = spaces.remove(old).ok_or_else(|| {
                CommandError::invalid_input(format!("space `{old}` is not registered"))
            })?;
            spaces.insert(new.to_string(), repo);
            Ok(())
        })
    }

    pub fn remove(&self, name: &str) -> CommandResult<()> {
        validate_space_name(name)?;
        self.mutate(|spaces| {
            spaces.remove(name).ok_or_else(|| {
                CommandError::invalid_input(format!("space `{name}` is not registered"))
            })?;
            Ok(())
        })
    }

    fn mutate(
        &self,
        mutation: impl FnOnce(&mut BTreeMap<String, RepoConfig>) -> CommandResult<()>,
    ) -> CommandResult<()> {
        let _lock = self.paths.try_lock_config().map_err(CommandError::from)?;
        let mut config = self.load_v2()?;
        validate_unique_addresses(&config.spaces)?;
        mutation(&mut config.spaces)?;
        validate_unique_addresses(&config.spaces)?;
        config.save(&self.paths.config).map_err(to_err)
    }

    fn load_v2(&self) -> CommandResult<LiosConfig> {
        let config = LiosConfig::load(&self.paths.config).map_err(to_err)?;
        if config.schema_version != CONFIG_SCHEMA_VERSION {
            return Err(CommandError::invalid_input(
                "configuration must be upgraded with `lios setup`",
            ));
        }
        Ok(config)
    }
}

pub fn validate_space_name(name: &str) -> CommandResult<()> {
    let bytes = name.as_bytes();
    if bytes.is_empty()
        || bytes.len() > 32
        || !bytes[0].is_ascii_lowercase()
        || bytes.iter().any(|byte| {
            !byte.is_ascii_lowercase() && !byte.is_ascii_digit() && *byte != b'_' && *byte != b'-'
        })
    {
        return Err(CommandError::invalid_input(
            "SpaceName must match [a-z][a-z0-9_-]{0,31}",
        ));
    }
    Ok(())
}

fn validate_unique_addresses(spaces: &BTreeMap<String, RepoConfig>) -> CommandResult<()> {
    for (index, (name, repo)) in spaces.iter().enumerate() {
        validate_space_name(name)?;
        validate_repo(repo.clone())?;
        if spaces.values().skip(index + 1).any(|other| other == repo) {
            return Err(CommandError::invalid_input(
                "a Repository Address cannot have multiple SpaceNames",
            ));
        }
    }
    Ok(())
}

fn validate_available_registration(
    spaces: &BTreeMap<String, RepoConfig>,
    name: &str,
    repo: &RepoConfig,
) -> CommandResult<()> {
    if spaces.contains_key(name) {
        return Err(CommandError::invalid_input(format!(
            "space `{name}` is already registered"
        )));
    }
    if spaces.values().any(|existing| existing == repo) {
        return Err(CommandError::invalid_input(
            "this Repository Address is already registered",
        ));
    }
    Ok(())
}
