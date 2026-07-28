use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{ensure_private_state_directory, is_internal_scope_id, LiosPaths};

/// A non-blocking, advisory, cross-process lock failure for one remote space.
#[derive(Debug, thiserror::Error)]
pub enum SpaceLockError {
    #[error("remote space is busy in another Lios process: {space_id}")]
    Busy { space_id: String },
    #[error("invalid remote space identifier")]
    InvalidSpaceId,
    #[error("failed to access the remote space lock: {0}")]
    Io(#[from] io::Error),
}

/// Holds an operating-system exclusive lock for one remote space.
///
/// The lock is advisory: every process that mutates a space must use this API.
/// Dropping the guard, or terminating the process, releases the OS lock. The
/// empty lock file intentionally remains on disk so unlink/recreate races cannot
/// split one logical lock into multiple file objects.
#[derive(Debug)]
#[must_use = "dropping the guard releases the remote space lock"]
pub struct SpaceLock {
    file: File,
    path: PathBuf,
}

impl SpaceLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SpaceLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

impl LiosPaths {
    /// Tries to acquire the exclusive cross-process lock for `space_id`.
    ///
    /// This call never waits. A contending Desktop or CLI process receives
    /// [`SpaceLockError::Busy`] and can decide whether to retry or exit.
    pub fn try_lock_space(&self, space_id: &str) -> Result<SpaceLock, SpaceLockError> {
        if !is_internal_scope_id(space_id) {
            return Err(SpaceLockError::InvalidSpaceId);
        }

        ensure_private_state_directory(&self.home)?;
        let locks_dir = self.home.join("locks");
        ensure_private_state_directory(&locks_dir)?;
        let spaces_dir = locks_dir.join("spaces");
        ensure_private_state_directory(&spaces_dir)?;
        let path = spaces_dir.join(format!("{space_id}.lock"));
        let file = open_lock_file(&path)?;

        match file.try_lock() {
            Ok(()) => Ok(SpaceLock { file, path }),
            Err(TryLockError::WouldBlock) => Err(SpaceLockError::Busy {
                space_id: space_id.to_string(),
            }),
            Err(TryLockError::Error(error)) => Err(error.into()),
        }
    }
}

fn open_lock_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(file)
}
