use super::*;
use crate::{LiosError, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogTreeNode {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub kind: CatalogTreeNodeKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum CatalogTreeNodeKind {
    Directory {
        children: Vec<CatalogTreeNode>,
    },
    File {
        original_size: u64,
        sha256: String,
        object_id: String,
        chunk_count: usize,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogRemoteFile {
    pub path: String,
    pub expected_size: Option<u64>,
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CatalogIntegrityReport {
    pub nodes_verified: u64,
    pub objects_verified: u64,
    pub chunks_verified: u64,
    pub encoded_bytes_verified: u64,
    pub original_bytes_verified: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CatalogRemoteIntegrityReport {
    pub expected_objects: u64,
    pub verified_objects: u64,
    pub metadata_limited_objects: u64,
    pub encoded_bytes_verified: u64,
    pub unreferenced_managed_objects: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct CatalogRebuildReport {
    pub nodes_rebuilt: u64,
    pub directories_rebuilt: u64,
    pub files_rebuilt: u64,
    pub content_objects_rebuilt: u64,
    pub chunks_referenced: u64,
    pub original_bytes_referenced: u64,
    pub unreferenced_managed_objects: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum DriveItemKind {
    Directory,
    File,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DriveItem {
    pub id: String,
    pub name: String,
    pub kind: DriveItemKind,
    pub size: u64,
    pub updated_at: String,
    pub children_count: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct UploadConflict {
    pub source_path: String,
    pub target_name: String,
    pub existing_node_id: String,
    pub kind: DriveItemKind,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConflictAction {
    Replace,
    KeepBoth,
    Skip,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ConflictResolution {
    pub source_path: String,
    pub action: ConflictAction,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PackReport {
    pub skipped_paths: Vec<SkippedPath>,
    #[serde(default)]
    pub packed_files: Vec<SourceFileSnapshot>,
    #[serde(default)]
    pub packed_directories: Vec<SourceDirectorySnapshot>,
}

impl PackReport {
    pub fn source_snapshot(&self) -> SourceSnapshotReport {
        SourceSnapshotReport {
            files: self.packed_files.clone(),
            directories: self.packed_directories.clone(),
            skipped_paths: self.skipped_paths.clone(),
        }
    }

    pub fn ensure_no_skipped_paths(&self) -> Result<()> {
        if self.skipped_paths.is_empty() {
            return Ok(());
        }
        let count = self.skipped_paths.len();
        let label = if count == 1 { "path" } else { "paths" };
        let paths = self
            .skipped_paths
            .iter()
            .take(3)
            .map(|skipped| skipped.path.display().to_string())
            .collect::<Vec<_>>()
            .join(", ");
        let remainder = if count > 3 {
            format!(", and {} more", count - 3)
        } else {
            String::new()
        };
        Err(LiosError::Unsupported(format!(
            "skipped {count} {label}: {paths}{remainder}"
        )))
    }
}

#[derive(Clone, Debug)]
pub enum PackOutcome {
    Packed {
        catalog: Catalog,
        report: PackReport,
    },
    Skipped {
        report: PackReport,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SkippedPath {
    pub path: PathBuf,
    pub reason: SkippedPathReason,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SkippedPathReason {
    SymbolicLinkOrJunction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceFileSnapshot {
    pub source_path: PathBuf,
    pub relative_path: PathBuf,
    pub size: u64,
    pub modified_at_ns: Option<i64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceDirectorySnapshot {
    pub source_path: PathBuf,
    pub relative_path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshotReport {
    pub files: Vec<SourceFileSnapshot>,
    #[serde(default)]
    pub directories: Vec<SourceDirectorySnapshot>,
    pub skipped_paths: Vec<SkippedPath>,
}

pub fn snapshot_source_files(paths: &[PathBuf]) -> Result<SourceSnapshotReport> {
    let mut report = SourceSnapshotReport::default();
    for path in paths {
        let relative_path = PathBuf::from(file_name(path)?);
        snapshot_source_path(path, &relative_path, &mut report)?;
    }
    Ok(report)
}

pub fn verify_source_file_unchanged(snapshot: &SourceFileSnapshot) -> Result<()> {
    let path_kind = packable_path_kind(&snapshot.source_path)
        .map_err(|error| map_missing_source_file(error, &snapshot.source_path))?;
    if path_kind != Some(PackablePathKind::File) {
        return Err(LiosError::Unsupported(format!(
            "source file changed while it was being packed: {}",
            snapshot.source_path.display()
        )));
    }
    let current = snapshot_regular_file(&snapshot.source_path, &snapshot.relative_path)
        .map_err(|error| map_missing_source_file(error, &snapshot.source_path))?;
    if current.size != snapshot.size || current.modified_at_ns != snapshot.modified_at_ns {
        return Err(LiosError::Unsupported(format!(
            "source file changed while it was being packed: {}",
            snapshot.source_path.display()
        )));
    }
    Ok(())
}

fn map_missing_source_file(error: LiosError, path: &Path) -> LiosError {
    match error {
        LiosError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            LiosError::Unsupported(format!("source file no longer exists: {}", path.display()))
        }
        LiosError::Unsupported(message) if message.starts_with("source path no longer exists:") => {
            LiosError::Unsupported(format!("source file no longer exists: {}", path.display()))
        }
        error => error,
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogV1 {
    pub version: u8,
    pub root_id: String,
    pub nodes: BTreeMap<String, CatalogNodeV1>,
    pub content_index: BTreeMap<String, String>,
    pub content_objects: BTreeMap<String, ContentObject>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogNodeV1 {
    pub descriptor: NodeDescriptorV1,
    pub descriptor_encrypted_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeDescriptorV1 {
    pub version: u8,
    pub node_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub updated_at: String,
    pub kind: NodeDescriptorKindV1,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum NodeDescriptorKindV1 {
    Directory,
    File {
        object_id: String,
        content_sha256: String,
        original_size: u64,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContentObject {
    pub object_id: String,
    pub content_sha256: String,
    pub original_size: u64,
    pub storage: StorageRef,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "value")]
pub enum StorageRef {
    V1(V1StorageRef),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct V1StorageRef {
    pub manifest_path: String,
    pub manifest_encrypted_sha256: String,
    pub chunks: Vec<V1ChunkRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct V1ChunkRef {
    pub index: usize,
    pub chunk_id: String,
    pub path: String,
    pub original_size: u64,
    pub original_sha256: String,
    pub encoded_size: u64,
    pub encoded_sha256: String,
    pub format_version: u8,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectManifestV1 {
    pub version: u8,
    pub format_version: u8,
    pub object_id: String,
    pub content_sha256: String,
    pub original_size: u64,
    pub chunks: Vec<V1ChunkRef>,
}

pub(crate) struct LoadedCatalogV1 {
    pub(crate) catalog: CatalogV1,
}

#[derive(Clone, Debug)]
pub struct PackOptions {
    pub chunk_size: usize,
    pub staging_dir: PathBuf,
}

impl PackOptions {
    pub const DEFAULT_CHUNK_SIZE: usize = 128 * 1024 * 1024;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackProgress {
    pub completed_chunks: u64,
    pub total_chunks: u64,
    pub completed_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub output_dir: PathBuf,
}
