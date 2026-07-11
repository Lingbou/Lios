use std::fs;
use std::path::{Path, PathBuf};

use directories::UserDirs;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::atomic::write_atomic;
use crate::crypto::KeyFile;
use crate::{LiosError, Result};

pub const MODELSCOPE_ENDPOINT: &str = "https://modelscope.cn";
pub const MODELSCOPE_WWW_ENDPOINT: &str = "https://www.modelscope.cn";

pub fn validate_modelscope_production_endpoint(endpoint: &str) -> Result<String> {
    match endpoint.trim() {
        "https://modelscope.cn" | "https://modelscope.cn/" => Ok(MODELSCOPE_ENDPOINT.to_string()),
        "https://www.modelscope.cn" | "https://www.modelscope.cn/" => {
            Ok(MODELSCOPE_WWW_ENDPOINT.to_string())
        }
        _ => Err(LiosError::Unsupported(
            "ModelScope endpoint must be https://modelscope.cn or https://www.modelscope.cn"
                .to_string(),
        )),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiosPaths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub database: PathBuf,
    pub staging: PathBuf,
    pub credentials: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct LiosConfig {
    pub active_repo: Option<RepoConfig>,
    pub key_file_path: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
    pub chunk_size: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepoConfig {
    pub namespace: String,
    pub dataset: String,
    pub endpoint: String,
}

pub fn lios_home() -> PathBuf {
    UserDirs::new()
        .and_then(|dirs| dirs.home_dir().canonicalize().ok())
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".lios")
}

pub fn config_path() -> PathBuf {
    lios_home().join("config.yaml")
}

impl LiosPaths {
    pub fn from_home(home: impl AsRef<Path>) -> Self {
        let home = home.as_ref().join(".lios");
        Self {
            config: home.join("config.yaml"),
            database: home.join("lios.db"),
            staging: home.join("staging"),
            credentials: home.join("credentials.enc"),
            home,
        }
    }

    pub fn default_user() -> Self {
        let home = UserDirs::new()
            .and_then(|dirs| dirs.home_dir().canonicalize().ok())
            .unwrap_or_else(|| PathBuf::from("."));
        Self::from_home(home)
    }

    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.home)?;
        fs::create_dir_all(&self.staging)?;
        Ok(())
    }

    pub fn for_task(&self, account_id: &str, space_id: &str, task_id: Uuid) -> Result<Self> {
        if !is_internal_scope_id(account_id) || !is_internal_scope_id(space_id) {
            return Err(LiosError::InvalidTaskScopeId);
        }
        let mut paths = self.clone();
        paths.staging = self
            .staging
            .join(account_id)
            .join(space_id)
            .join(task_id.to_string());
        Ok(paths)
    }
}

fn is_internal_scope_id(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

impl LiosConfig {
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        if !path.as_ref().exists() {
            return Ok(Self::default());
        }
        Ok(serde_yaml::from_str(&fs::read_to_string(path)?)?)
    }

    pub fn save(&self, path: impl AsRef<Path>) -> Result<()> {
        let serialized = serde_yaml::to_string(self)?;
        write_atomic(path.as_ref(), serialized.as_bytes())?;
        Ok(())
    }
}

pub fn ensure_default_key_binding(paths: &LiosPaths, config: &mut LiosConfig) -> Result<bool> {
    if config.key_file_path.is_some() {
        return Ok(false);
    }
    let key_path = paths.home.join("recovery.key");
    match KeyFile::load_from_path(&key_path) {
        Ok(_) => {}
        Err(LiosError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            match KeyFile::generate_to_path(&key_path) {
                Ok(_) => {}
                Err(LiosError::Io(error)) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    KeyFile::load_from_path(&key_path)?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(error) => return Err(error),
    }

    config.key_file_path = Some(key_path);
    Ok(true)
}

pub fn ensure_default_key_configured(paths: &LiosPaths, config: &mut LiosConfig) -> Result<()> {
    let mut updated = config.clone();
    if !ensure_default_key_binding(paths, &mut updated)? {
        return Ok(());
    }

    updated.save(&paths.config)?;
    *config = updated;
    Ok(())
}
