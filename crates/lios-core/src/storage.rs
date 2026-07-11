use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{LiosError, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalStorageObject {
    pub path: String,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotChangePlan {
    pub upload: Vec<LocalStorageObject>,
    pub delete: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageObject {
    pub path: String,
    pub size: u64,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSyncFile {
    pub path: String,
    pub local_path: Option<PathBuf>,
    pub expected_sha256: Option<String>,
    pub expected_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CatalogSyncUpload {
    pub path: String,
    pub local_path: PathBuf,
    pub expected_sha256: String,
    pub expected_size: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogSyncPlan {
    pub upload: Vec<CatalogSyncUpload>,
    pub delete: Vec<String>,
}

pub fn plan_catalog_sync_changes(
    mut desired: Vec<CatalogSyncFile>,
    remote: Vec<StorageObject>,
) -> Result<CatalogSyncPlan> {
    desired.sort_by(|a, b| {
        let a_catalog = a.path == "catalog.enc";
        let b_catalog = b.path == "catalog.enc";
        a_catalog.cmp(&b_catalog).then_with(|| a.path.cmp(&b.path))
    });
    let desired_paths = desired
        .iter()
        .map(|file| file.path.clone())
        .collect::<HashSet<_>>();
    let remote_by_path = remote
        .iter()
        .map(|object| (object.path.as_str(), object))
        .collect::<HashMap<_, _>>();

    let mut upload = Vec::new();
    for file in desired {
        let Some(expected_sha256) = file.expected_sha256.as_deref() else {
            continue;
        };
        let remote_matches = remote_by_path
            .get(file.path.as_str())
            .is_some_and(|remote| {
                remote.sha256.as_deref() == Some(expected_sha256)
                    && file
                        .expected_size
                        .is_none_or(|expected_size| remote.size == expected_size)
            });
        if remote_matches {
            continue;
        }
        let Some(local_path) = file.local_path else {
            return Err(missing_catalog_sync_file(&file.path));
        };
        if !local_catalog_sync_file_matches(&local_path, expected_sha256, file.expected_size) {
            return Err(missing_catalog_sync_file(&file.path));
        }
        upload.push(CatalogSyncUpload {
            path: file.path,
            local_path,
            expected_sha256: expected_sha256.to_string(),
            expected_size: file.expected_size,
        });
    }

    let mut delete = remote
        .into_iter()
        .filter(|object| {
            is_managed_snapshot_path(&object.path) && !desired_paths.contains(&object.path)
        })
        .map(|object| object.path)
        .collect::<Vec<_>>();
    delete.sort();
    delete.dedup();

    Ok(CatalogSyncPlan { upload, delete })
}

fn missing_catalog_sync_file(path: &str) -> LiosError {
    LiosError::DataCorruption(format!(
        "catalog object is unavailable locally and remotely: {path}"
    ))
}

fn local_catalog_sync_file_matches(
    local_path: &std::path::Path,
    expected_sha256: &str,
    expected_size: Option<u64>,
) -> bool {
    let Ok(metadata) = fs::metadata(local_path) else {
        return false;
    };
    metadata.is_file()
        && expected_size.is_none_or(|size| metadata.len() == size)
        && sha256_file(local_path).is_ok_and(|sha256| sha256 == expected_sha256)
}

pub fn validate_catalog_sync_upload(upload: &CatalogSyncUpload) -> Result<()> {
    if !local_catalog_sync_file_matches(
        &upload.local_path,
        &upload.expected_sha256,
        upload.expected_size,
    ) {
        return Err(LiosError::DataCorruption(format!(
            "planned catalog upload changed before publication: {}",
            upload.path
        )));
    }
    Ok(())
}

pub fn plan_current_snapshot_changes(
    mut local: Vec<LocalStorageObject>,
    remote: Vec<StorageObject>,
) -> SnapshotChangePlan {
    local.sort_by(|a, b| {
        let a_catalog = a.path == "catalog.enc";
        let b_catalog = b.path == "catalog.enc";
        a_catalog.cmp(&b_catalog).then_with(|| a.path.cmp(&b.path))
    });

    let mut desired = local
        .iter()
        .map(|object| object.path.clone())
        .collect::<HashSet<_>>();
    for object in &local {
        if let Some(object_dir) = per_file_object_dir(&object.path) {
            desired.insert(format!("{object_dir}/manifest.enc"));
        }
    }
    let remote_by_path = remote
        .iter()
        .map(|object| (object.path.as_str(), object.sha256.as_deref()))
        .collect::<HashMap<_, _>>();

    let upload = local
        .into_iter()
        .filter(|object| {
            remote_by_path.get(object.path.as_str()).copied().flatten()
                != Some(object.sha256.as_str())
        })
        .collect::<Vec<_>>();

    let mut delete = remote
        .into_iter()
        .filter(|object| is_managed_snapshot_path(&object.path) && !desired.contains(&object.path))
        .map(|object| object.path)
        .collect::<Vec<_>>();
    delete.sort();
    delete.dedup();

    SnapshotChangePlan { upload, delete }
}

fn is_managed_snapshot_path(path: &str) -> bool {
    path.starts_with("objects/") || path.starts_with("recovery/nodes/")
}

fn per_file_object_dir(path: &str) -> Option<String> {
    let mut parts = path.split('/');
    if parts.next() != Some("objects") || parts.next() != Some("files") {
        return None;
    }
    let object_id = parts.next()?;
    if object_id.is_empty() || parts.next().is_none() {
        return None;
    }
    Some(format!("objects/files/{object_id}"))
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[async_trait]
pub trait StorageAdapter: Send + Sync {
    async fn create_repo(&self, namespace: &str, dataset: &str) -> Result<()>;
    async fn repo_exists(&self, namespace: &str, dataset: &str) -> Result<bool>;
    async fn list_objects(
        &self,
        namespace: &str,
        dataset: &str,
        prefix: &str,
    ) -> Result<Vec<StorageObject>>;
    async fn upload_object(
        &self,
        namespace: &str,
        dataset: &str,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> Result<()>;
    async fn download_object(
        &self,
        namespace: &str,
        dataset: &str,
        remote_path: &str,
        local_path: &std::path::Path,
    ) -> Result<()>;
    async fn delete_objects(
        &self,
        namespace: &str,
        dataset: &str,
        remote_paths: &[String],
    ) -> Result<()>;
    async fn delete_prefix(&self, namespace: &str, dataset: &str, prefix: &str) -> Result<()>;
}
