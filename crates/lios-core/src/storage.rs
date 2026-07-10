use std::collections::{HashMap, HashSet};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::Result;

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

pub fn plan_current_snapshot_changes(
    mut local: Vec<LocalStorageObject>,
    remote: Vec<StorageObject>,
) -> SnapshotChangePlan {
    local.sort_by(|a, b| {
        let a_catalog = a.path == "catalog.enc";
        let b_catalog = b.path == "catalog.enc";
        a_catalog.cmp(&b_catalog).then_with(|| a.path.cmp(&b.path))
    });

    let desired = local
        .iter()
        .map(|object| object.path.clone())
        .collect::<HashSet<_>>();
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
        .filter(|object| object.path.starts_with("objects/") && !desired.contains(&object.path))
        .map(|object| object.path)
        .collect::<Vec<_>>();
    delete.sort();
    delete.dedup();

    SnapshotChangePlan { upload, delete }
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
