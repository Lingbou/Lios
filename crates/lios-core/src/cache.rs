use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use uuid::Uuid;
use walkdir::WalkDir;

use crate::{LiosError, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheCleanupReport {
    pub files_removed: u64,
    pub dirs_removed: u64,
    pub bytes_removed: u64,
}

impl CacheCleanupReport {
    pub fn add(&mut self, other: CacheCleanupReport) {
        self.files_removed += other.files_removed;
        self.dirs_removed += other.dirs_removed;
        self.bytes_removed += other.bytes_removed;
    }
}

pub fn cleanup_temporary_staging(staging: impl AsRef<Path>) -> Result<CacheCleanupReport> {
    let staging = staging.as_ref();
    let mut report = CacheCleanupReport::default();
    if !staging.exists() {
        return Ok(report);
    }

    let tmp_dir = staging.join(".tmp");
    if tmp_dir.exists() {
        report.add(remove_path_counting(&tmp_dir)?);
    }

    let mut interrupted = Vec::new();
    for entry in WalkDir::new(staging) {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("download")
        {
            interrupted.push(entry.path().to_path_buf());
        }
    }
    for path in interrupted {
        report.add(remove_file_counting(&path)?);
    }

    Ok(report)
}

pub fn cleanup_task_staging(
    staging_root: impl AsRef<Path>,
    account_id: &str,
    space_id: &str,
    task_id: Uuid,
) -> Result<CacheCleanupReport> {
    let staging_root = staging_root.as_ref();
    let task_staging = staging_root
        .join(account_id)
        .join(space_id)
        .join(task_id.to_string());
    let mut report = CacheCleanupReport::default();
    if !task_staging.exists() {
        return Ok(report);
    }
    report.add(remove_path_counting(&task_staging)?);
    let space_dir = staging_root.join(account_id).join(space_id);
    if space_dir.exists()
        && fs::read_dir(&space_dir)
            .map(|mut iter| iter.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(&space_dir);
        report.dirs_removed += 1;
    }
    let account_dir = staging_root.join(account_id);
    if account_dir.exists()
        && fs::read_dir(&account_dir)
            .map(|mut iter| iter.next().is_none())
            .unwrap_or(false)
    {
        let _ = fs::remove_dir(&account_dir);
        report.dirs_removed += 1;
    }
    Ok(report)
}

pub fn cleanup_all_inactive_staging(
    staging_root: impl AsRef<Path>,
    active_task_ids: &HashSet<Uuid>,
) -> Result<CacheCleanupReport> {
    let staging_root = staging_root.as_ref();
    let mut report = cleanup_temporary_staging_except_active(staging_root, active_task_ids)?;
    if !staging_root.exists() {
        return Ok(report);
    }

    let entries = match fs::read_dir(staging_root) {
        Ok(entries) => entries,
        Err(_) => return Ok(report),
    };

    for entry in entries.flatten() {
        let entry_path = entry.path();
        if !entry_path.is_dir() {
            continue;
        }

        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        if name == ".tmp" {
            continue;
        }

        if !is_scope_hex(&name) {
            continue;
        }

        let space_entries = match fs::read_dir(&entry_path) {
            Ok(entries) => entries,
            Err(_) => continue,
        };

        for space_entry in space_entries.flatten() {
            let space_path = space_entry.path();
            if !space_path.is_dir() {
                if let Ok(file_report) = remove_file_counting(&space_path) {
                    report.add(file_report);
                }
                continue;
            }

            let space_file_name = space_entry.file_name();
            let space_name = space_file_name.to_string_lossy();
            if !is_scope_hex(&space_name) {
                continue;
            }

            let task_entries = match fs::read_dir(&space_path) {
                Ok(entries) => entries,
                Err(_) => continue,
            };

            for task_entry in task_entries.flatten() {
                let task_path = task_entry.path();
                let task_name = task_entry.file_name().to_string_lossy().into_owned();
                if let Ok(uuid) = Uuid::parse_str(&task_name) {
                    if !active_task_ids.contains(&uuid) {
                        if let Ok(task_report) = remove_path_counting(&task_path) {
                            report.add(task_report);
                        }
                    }
                }
            }

            if fs::read_dir(&space_path)
                .map(|mut iter| iter.next().is_none())
                .unwrap_or(false)
            {
                if fs::remove_dir(&space_path).is_ok() {
                    report.dirs_removed += 1;
                }
            }
        }

        if fs::read_dir(&entry_path)
            .map(|mut iter| iter.next().is_none())
            .unwrap_or(false)
        {
            if fs::remove_dir(&entry_path).is_ok() {
                report.dirs_removed += 1;
            }
        }
    }

    Ok(report)
}

fn cleanup_temporary_staging_except_active(
    staging: &Path,
    active_task_ids: &HashSet<Uuid>,
) -> Result<CacheCleanupReport> {
    let mut report = CacheCleanupReport::default();
    if !staging.exists() {
        return Ok(report);
    }

    let tmp_dir = staging.join(".tmp");
    if tmp_dir.exists() {
        report.add(remove_path_counting(&tmp_dir)?);
    }

    let mut interrupted = Vec::new();
    for entry in WalkDir::new(staging) {
        let entry = entry?;
        if entry.file_type().is_file()
            && entry.path().extension().and_then(|ext| ext.to_str()) == Some("download")
            && !path_is_in_active_task(staging, entry.path(), active_task_ids)
        {
            interrupted.push(entry.path().to_path_buf());
        }
    }
    for path in interrupted {
        report.add(remove_file_counting(&path)?);
    }

    Ok(report)
}

fn path_is_in_active_task(staging: &Path, path: &Path, active_task_ids: &HashSet<Uuid>) -> bool {
    let Ok(relative) = path.strip_prefix(staging) else {
        return false;
    };
    let mut components = relative.components();
    let (
        Some(Component::Normal(account)),
        Some(Component::Normal(space)),
        Some(Component::Normal(task)),
    ) = (components.next(), components.next(), components.next())
    else {
        return false;
    };
    if !is_scope_hex(&account.to_string_lossy()) || !is_scope_hex(&space.to_string_lossy()) {
        return false;
    }
    task.to_str()
        .and_then(|value| Uuid::parse_str(value).ok())
        .is_some_and(|task_id| active_task_ids.contains(&task_id))
}

fn is_scope_hex(name: &str) -> bool {
    name.len() == 64
        && name
            .chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
}

pub fn prune_unreferenced_staging(
    staging: impl AsRef<Path>,
    referenced_remote_paths: impl IntoIterator<Item = String>,
) -> Result<CacheCleanupReport> {
    let staging = staging.as_ref();
    let mut report = cleanup_temporary_staging(staging)?;
    if !staging.exists() {
        return Ok(report);
    }

    let keep = referenced_remote_paths
        .into_iter()
        .map(|path| safe_relative_path(&path))
        .collect::<Result<HashSet<_>>>()?;

    let mut stale_files = Vec::new();
    for entry in WalkDir::new(staging) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(staging)
            .map_err(|_| LiosError::InvalidRelativePath(entry.path().to_path_buf()))?
            .to_path_buf();
        if !keep.contains(&relative) {
            stale_files.push(entry.path().to_path_buf());
        }
    }
    for path in stale_files {
        report.add(remove_file_counting(&path)?);
    }

    let mut dirs = WalkDir::new(staging)
        .min_depth(1)
        .into_iter()
        .collect::<std::result::Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|entry| entry.file_type().is_dir())
        .map(|entry| entry.path().to_path_buf())
        .collect::<Vec<_>>();
    dirs.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for dir in dirs {
        if fs::read_dir(&dir)?.next().is_none() {
            fs::remove_dir(&dir)?;
            report.dirs_removed += 1;
        }
    }

    Ok(report)
}

fn safe_relative_path(path: &str) -> Result<PathBuf> {
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LiosError::InvalidRelativePath(relative.to_path_buf()));
    }
    Ok(relative.to_path_buf())
}

pub fn remove_path_counting(path: &Path) -> Result<CacheCleanupReport> {
    let mut report = CacheCleanupReport::default();
    if !path.exists() {
        return Ok(report);
    }
    if path.is_file() {
        return remove_file_counting(path);
    }
    if path.is_dir() {
        for entry in WalkDir::new(path).contents_first(true) {
            let entry = entry?;
            if entry.file_type().is_dir() {
                match fs::remove_dir(entry.path()) {
                    Ok(()) => report.dirs_removed += 1,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(LiosError::Io(error)),
                }
            } else {
                report.add(remove_file_counting(entry.path())?);
            }
        }
    }
    Ok(report)
}

pub fn remove_file_counting(path: &Path) -> Result<CacheCleanupReport> {
    let bytes = fs::symlink_metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    match fs::remove_file(path) {
        Ok(()) => Ok(CacheCleanupReport {
            files_removed: 1,
            dirs_removed: 0,
            bytes_removed: bytes,
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Ok(CacheCleanupReport::default())
        }
        Err(error) => Err(LiosError::Io(error)),
    }
}
