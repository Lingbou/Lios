//! Tauri-independent Lios application services shared by Desktop and CLI.

use std::fs;
use std::io::Read;
use std::path::{Component, Path, PathBuf};

use sha2::{Digest, Sha256};

pub mod app_log;
pub mod catalog_mutation_gate;
pub mod catalog_probe;
pub mod catalog_sync;
pub mod command_error;
pub mod config_mutation_gate;
pub mod download_service;
pub mod production_config;
pub mod recovery_key_service;
pub mod service;
pub mod task_manager;
pub mod task_runner;

pub use command_error::{CommandError, CommandErrorCode};

pub type CommandResult<T> = std::result::Result<T, CommandError>;

pub fn to_err<E>(error: E) -> CommandError
where
    CommandError: From<E>,
{
    error.into()
}

pub fn remote_to_staging_path(staging: &Path, remote_path: &str) -> CommandResult<PathBuf> {
    let relative = Path::new(remote_path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(CommandError::invalid_input(format!(
            "invalid remote object path in catalog: {remote_path}"
        )));
    }
    Ok(staging.join(relative))
}

pub fn sha256_hex_file(path: &Path) -> CommandResult<String> {
    let mut file = fs::File::open(path).map_err(to_err)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer).map_err(to_err)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}
