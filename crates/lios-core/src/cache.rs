use std::collections::HashSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use walkdir::WalkDir;

use crate::{LiosError, Result};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CacheCleanupReport {
    pub files_removed: u64,
    pub dirs_removed: u64,
    pub bytes_removed: u64,
}

impl CacheCleanupReport {
    fn add(&mut self, other: CacheCleanupReport) {
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

fn remove_path_counting(path: &Path) -> Result<CacheCleanupReport> {
    let mut report = CacheCleanupReport::default();
    if path.is_file() {
        return remove_file_counting(path);
    }
    if path.is_dir() {
        for entry in WalkDir::new(path).contents_first(true) {
            let entry = entry?;
            if entry.file_type().is_file() {
                report.add(remove_file_counting(entry.path())?);
            } else if entry.file_type().is_dir() {
                fs::remove_dir(entry.path())?;
                report.dirs_removed += 1;
            }
        }
    }
    Ok(report)
}

fn remove_file_counting(path: &Path) -> Result<CacheCleanupReport> {
    let bytes = fs::metadata(path)
        .map(|metadata| metadata.len())
        .unwrap_or(0);
    fs::remove_file(path)?;
    Ok(CacheCleanupReport {
        files_removed: 1,
        dirs_removed: 0,
        bytes_removed: bytes,
    })
}
