use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs;
use std::io::Read;
use std::path::PathBuf;

use async_trait::async_trait;
use serde::{Deserialize, Serialize, Serializer};
use sha2::{Digest, Sha256};
use tokio::io::AsyncReadExt;

pub use crate::catalog::StorageRef;
use crate::{LiosError, Result};

pub const MODELSCOPE_LFS_BATCH_SIZE: usize = 64;
pub const MODELSCOPE_COMMIT_ACTION_LIMIT: usize = 256;

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
pub struct RepoRevision {
    pub branch: String,
    pub commit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobSpec {
    pub local_path: PathBuf,
    pub oid: String,
    pub size: u64,
}

impl BlobSpec {
    pub async fn from_path(local_path: impl Into<PathBuf>) -> Result<Self> {
        let local_path = local_path.into();
        let mut file = tokio::fs::File::open(&local_path).await?;
        let metadata = file.metadata().await?;
        if !metadata.is_file() {
            return Err(LiosError::Storage(format!(
                "blob source is not a regular file: {}",
                local_path.display()
            )));
        }
        let mut hasher = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        loop {
            let read = file.read(&mut buffer).await?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        Ok(Self {
            local_path,
            oid: hex::encode(hasher.finalize()),
            size: metadata.len(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlobCheckpoint {
    pub oid: String,
    pub size: u64,
}

impl BlobCheckpoint {
    pub fn new(oid: impl Into<String>, size: u64) -> Self {
        Self {
            oid: oid.into(),
            size,
        }
    }
}

#[derive(PartialEq, Eq)]
pub struct ValidatedBlobUpload {
    checkpoint: BlobCheckpoint,
    // Signed targets are deliberately opaque so Debug/errors cannot expose them.
    upload_url: String,
}

impl ValidatedBlobUpload {
    pub(crate) fn new(checkpoint: BlobCheckpoint, upload_url: String) -> Self {
        Self {
            checkpoint,
            upload_url,
        }
    }

    pub(crate) fn into_parts(self) -> (BlobCheckpoint, String) {
        (self.checkpoint, self.upload_url)
    }
}

impl fmt::Debug for ValidatedBlobUpload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ValidatedBlobUpload")
            .field("checkpoint", &self.checkpoint)
            .field("upload_url", &"[REDACTED]")
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum BlobValidation {
    Reusable(BlobCheckpoint),
    UploadRequired(ValidatedBlobUpload),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteAction {
    LfsUpsert { path: String, blob: BlobCheckpoint },
    Delete { path: String },
}

impl RemoteAction {
    pub fn lfs_upsert(path: impl Into<String>, blob: BlobCheckpoint) -> Self {
        Self::LfsUpsert {
            path: path.into(),
            blob,
        }
    }

    pub fn delete(path: impl Into<String>) -> Self {
        Self::Delete { path: path.into() }
    }

    pub fn path(&self) -> &str {
        match self {
            Self::LfsUpsert { path, .. } | Self::Delete { path } => path,
        }
    }

    pub fn is_upload(&self) -> bool {
        matches!(self, Self::LfsUpsert { .. })
    }

    pub fn is_delete(&self) -> bool {
        matches!(self, Self::Delete { .. })
    }

    pub(crate) fn validate(&self) -> std::result::Result<(), StorageTransactionError> {
        match self {
            Self::LfsUpsert { path, blob } => {
                if !is_safe_upload_path(path) {
                    return Err(StorageTransactionError::UnmanagedUploadPath(path.clone()));
                }
                validate_blob_oid(&blob.oid)
            }
            Self::Delete { path } => {
                if !is_safe_managed_data_path(path) {
                    return Err(StorageTransactionError::UnmanagedDeletePath(path.clone()));
                }
                Ok(())
            }
        }
    }
}

impl Serialize for RemoteAction {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        #[derive(Serialize)]
        struct WireAction<'a> {
            action: &'static str,
            path: &'a str,
            #[serde(rename = "type")]
            kind: &'static str,
            size: u64,
            sha256: &'a str,
            content: &'static str,
            encoding: &'static str,
        }

        let wire = match self {
            Self::LfsUpsert { path, blob } => WireAction {
                action: "create",
                path,
                kind: "lfs",
                size: blob.size,
                sha256: &blob.oid,
                content: "",
                encoding: "",
            },
            Self::Delete { path } => WireAction {
                action: "delete",
                path,
                kind: "normal",
                size: 0,
                sha256: "",
                content: "",
                encoding: "",
            },
        };
        wire.serialize(serializer)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum StorageTransactionError {
    #[error("a modifying commit plan requires exactly one catalog.enc upload")]
    MissingCatalogAction,
    #[error("a modifying commit plan contains {count} catalog.enc uploads")]
    DuplicateCatalogAction { count: usize },
    #[error("the desired upload list contains a delete action for {0}")]
    DeleteInUploadActions(String),
    #[error("duplicate upload action for {0}")]
    DuplicateUploadPath(String),
    #[error("duplicate remote action for {0}")]
    DuplicateActionPath(String),
    #[error("the same path is uploaded and deleted in one plan: {0}")]
    ConflictingActionPath(String),
    #[error("blob oid must be exactly 64 lowercase hexadecimal characters: {0}")]
    InvalidBlobOid(String),
    #[error("upload path is outside Lios-managed storage: {0}")]
    UnmanagedUploadPath(String),
    #[error("delete path is outside Lios-managed storage: {0}")]
    UnmanagedDeletePath(String),
    #[error("publish batch has {actions} actions, exceeding the limit of {limit}")]
    PublishBatchTooLarge { actions: usize, limit: usize },
    #[error("commit has {actions} actions, exceeding the limit of {limit}")]
    CommitBatchTooLarge { actions: usize, limit: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitPlan {
    /// Informational catalog checkpoint only; ModelScope commits do not provide parent-commit CAS.
    pub base_catalog_sha256: Option<String>,
    pub prepublish: Vec<Vec<RemoteAction>>,
    pub publish: Vec<RemoteAction>,
    pub cleanup: Vec<Vec<RemoteAction>>,
}

impl CommitPlan {
    pub fn build(
        mut desired_uploads: Vec<RemoteAction>,
        mut delete_paths: Vec<String>,
        remote_inventory: &[StorageObject],
        prepublish_safe_paths: &HashSet<String>,
        base_catalog_sha256: Option<String>,
    ) -> std::result::Result<Self, StorageTransactionError> {
        if desired_uploads.is_empty() && delete_paths.is_empty() {
            return Ok(Self {
                base_catalog_sha256,
                prepublish: Vec::new(),
                publish: Vec::new(),
                cleanup: Vec::new(),
            });
        }

        let catalog_actions = desired_uploads
            .iter()
            .filter(|action| {
                matches!(action, RemoteAction::LfsUpsert { path, .. } if path == "catalog.enc")
            })
            .count();
        match catalog_actions {
            0 => return Err(StorageTransactionError::MissingCatalogAction),
            1 => {}
            count => return Err(StorageTransactionError::DuplicateCatalogAction { count }),
        }

        let mut upload_paths = HashSet::new();
        for action in &desired_uploads {
            if !action.is_upload() {
                return Err(StorageTransactionError::DeleteInUploadActions(
                    action.path().to_string(),
                ));
            }
            action.validate()?;
            if !upload_paths.insert(action.path().to_string()) {
                return Err(StorageTransactionError::DuplicateUploadPath(
                    action.path().to_string(),
                ));
            }
        }

        delete_paths.sort();
        if let Some(duplicate) = delete_paths
            .windows(2)
            .find(|pair| pair[0] == pair[1])
            .map(|pair| pair[0].clone())
        {
            return Err(StorageTransactionError::DuplicateActionPath(duplicate));
        }
        for path in &delete_paths {
            if !is_safe_managed_data_path(path) {
                return Err(StorageTransactionError::UnmanagedDeletePath(path.clone()));
            }
            if upload_paths.contains(path) {
                return Err(StorageTransactionError::ConflictingActionPath(path.clone()));
            }
        }

        sort_upload_actions(&mut desired_uploads);
        let total_actions = desired_uploads.len() + delete_paths.len();
        if total_actions <= MODELSCOPE_COMMIT_ACTION_LIMIT {
            let mut publish = desired_uploads;
            publish.extend(delete_paths.into_iter().map(RemoteAction::delete));
            return Ok(Self {
                base_catalog_sha256,
                prepublish: Vec::new(),
                publish,
                cleanup: Vec::new(),
            });
        }

        let remote_paths = remote_inventory
            .iter()
            .map(|object| object.path.as_str())
            .collect::<HashSet<_>>();
        let (prepublish, publish): (Vec<_>, Vec<_>) =
            desired_uploads.into_iter().partition(|action| {
                action.path() != "catalog.enc"
                    && prepublish_safe_paths.contains(action.path())
                    && !remote_paths.contains(action.path())
            });
        if publish.len() > MODELSCOPE_COMMIT_ACTION_LIMIT {
            return Err(StorageTransactionError::PublishBatchTooLarge {
                actions: publish.len(),
                limit: MODELSCOPE_COMMIT_ACTION_LIMIT,
            });
        }

        Ok(Self {
            base_catalog_sha256,
            prepublish: chunk_actions(prepublish),
            publish,
            cleanup: chunk_actions(delete_paths.into_iter().map(RemoteAction::delete).collect()),
        })
    }

    pub fn all_batches(&self) -> impl Iterator<Item = &[RemoteAction]> {
        self.prepublish
            .iter()
            .map(Vec::as_slice)
            .chain(std::iter::once(self.publish.as_slice()).filter(|batch| !batch.is_empty()))
            .chain(self.cleanup.iter().map(Vec::as_slice))
    }
}

pub fn current_catalog_sha256(objects: &[StorageObject]) -> Option<&str> {
    objects
        .iter()
        .find(|object| object.path == "catalog.enc")
        .and_then(|object| object.sha256.as_deref())
}

fn sort_upload_actions(actions: &mut [RemoteAction]) {
    actions.sort_by(|a, b| {
        let a_catalog = a.path() == "catalog.enc";
        let b_catalog = b.path() == "catalog.enc";
        a_catalog
            .cmp(&b_catalog)
            .then_with(|| a.path().cmp(b.path()))
    });
}

fn chunk_actions(actions: Vec<RemoteAction>) -> Vec<Vec<RemoteAction>> {
    let mut batches = Vec::new();
    let mut current = Vec::with_capacity(MODELSCOPE_COMMIT_ACTION_LIMIT);
    for action in actions {
        current.push(action);
        if current.len() == MODELSCOPE_COMMIT_ACTION_LIMIT {
            batches.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

pub(crate) fn validate_remote_actions(
    actions: &[RemoteAction],
) -> std::result::Result<(), StorageTransactionError> {
    let mut seen = HashMap::with_capacity(actions.len());
    for action in actions {
        action.validate()?;
        let is_upload = action.is_upload();
        if let Some(previous_is_upload) = seen.insert(action.path(), is_upload) {
            if previous_is_upload != is_upload {
                return Err(StorageTransactionError::ConflictingActionPath(
                    action.path().to_string(),
                ));
            }
            return Err(StorageTransactionError::DuplicateActionPath(
                action.path().to_string(),
            ));
        }
    }
    Ok(())
}

pub(crate) fn validate_blob_oid(oid: &str) -> std::result::Result<(), StorageTransactionError> {
    if oid.len() == 64
        && oid
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        Ok(())
    } else {
        Err(StorageTransactionError::InvalidBlobOid(oid.to_string()))
    }
}

fn is_safe_upload_path(path: &str) -> bool {
    path == "catalog.enc" || is_safe_managed_data_path(path)
}

fn is_safe_managed_data_path(path: &str) -> bool {
    let managed = path
        .strip_prefix("objects/")
        .or_else(|| path.strip_prefix("recovery/nodes/"));
    managed.is_some_and(|suffix| {
        !suffix.is_empty()
            && !path.contains('\\')
            && path
                .split('/')
                .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
    })
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
    async fn head_revision(&self, _namespace: &str, _dataset: &str) -> Result<RepoRevision> {
        Err(LiosError::Unsupported(
            "head revision lookup is not implemented by this storage adapter".to_string(),
        ))
    }
    async fn list_objects(
        &self,
        namespace: &str,
        dataset: &str,
        prefix: &str,
    ) -> Result<Vec<StorageObject>>;
    async fn validate_blobs(
        &self,
        _namespace: &str,
        _dataset: &str,
        _blobs: &[BlobSpec],
    ) -> Result<Vec<BlobValidation>> {
        Err(LiosError::Unsupported(
            "blob validation is not implemented by this storage adapter".to_string(),
        ))
    }
    async fn upload_blob(
        &self,
        _blob: &BlobSpec,
        _validated: ValidatedBlobUpload,
    ) -> Result<BlobCheckpoint> {
        Err(LiosError::Unsupported(
            "blob upload is not implemented by this storage adapter".to_string(),
        ))
    }
    async fn commit_actions(
        &self,
        _namespace: &str,
        _dataset: &str,
        _commit_message: &str,
        _actions: &[RemoteAction],
    ) -> Result<()> {
        Err(LiosError::Unsupported(
            "commit actions are not implemented by this storage adapter".to_string(),
        ))
    }
    /// Compatibility wrapper for current Tauri flows. Remove in the next integration task.
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
    /// Compatibility wrapper for current Tauri flows. Remove in the next integration task.
    async fn delete_objects(
        &self,
        namespace: &str,
        dataset: &str,
        remote_paths: &[String],
    ) -> Result<()>;
    /// Compatibility wrapper for current Tauri flows. Remove in the next integration task.
    async fn delete_prefix(&self, namespace: &str, dataset: &str, prefix: &str) -> Result<()>;
}
