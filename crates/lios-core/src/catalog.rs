use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::atomic::{write_atomic, write_atomic_immutable, SiblingTempFile};
use crate::crypto::KeyFile;
use crate::format_v2::{
    decrypt_compatible_v1_or_v2, encrypt_envelope_v2, envelope_encoded_len_v2, EnvelopeKindV2,
};
use crate::framed_v2::{
    decode_chunk_stream_v2, encode_chunk_stream_v2, ChunkDecodeLimitsV2, ChunkIdV2,
};
use crate::pack::{PackOptions, PackProgress, PackSource};
use crate::restore::{RestoreConflictPolicy, RestoreOptions};
use crate::storage::StorageObject;
use crate::{LiosError, Result};

pub const CATALOG_FILE: &str = "catalog.enc";
const FILES_DIR: &str = "objects/files";
const FILE_CHUNKS_DIR: &str = "chunks";
const FILE_MANIFEST: &str = "manifest.enc";
const NODE_DESCRIPTORS_DIR: &str = "recovery/nodes";
/// Recursive catalog consumers only run after validation. Capping the tree at
/// 256 parent-child edges keeps their stack usage conservative and predictable.
const MAX_CATALOG_DEPTH: usize = 256;
const LEGACY_RESTORE_BUFFER_SIZE: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PackablePathKind {
    Directory,
    File,
}

#[derive(Clone, Debug)]
pub enum CatalogSelection {
    All,
    Node(String),
    Nodes(Vec<String>),
}

#[derive(Clone, Debug)]
pub struct Catalog {
    encrypted_catalog_path: PathBuf,
    staging_dir: PathBuf,
}

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

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyContentOptimizationSummary {
    pub object_id: String,
    pub content_sha256: String,
    pub original_size: u64,
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

impl PackOutcome {
    pub fn into_catalog(self) -> Result<Catalog> {
        match self {
            Self::Packed { catalog, report } => {
                report.ensure_no_skipped_paths()?;
                Ok(catalog)
            }
            Self::Skipped { report } => {
                report.ensure_no_skipped_paths()?;
                Err(LiosError::Unsupported(
                    "packing produced no catalog".to_string(),
                ))
            }
        }
    }
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
pub struct CatalogV2 {
    pub version: u8,
    pub root_id: String,
    pub nodes: BTreeMap<String, CatalogNodeV2>,
    pub content_index: BTreeMap<String, String>,
    pub content_objects: BTreeMap<String, ContentObject>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CatalogNodeV2 {
    pub descriptor: NodeDescriptorV2,
    pub descriptor_encrypted_sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct NodeDescriptorV2 {
    pub version: u8,
    pub node_id: String,
    pub parent_id: Option<String>,
    pub name: String,
    #[serde(default)]
    pub updated_at: String,
    pub kind: NodeDescriptorKindV2,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type")]
pub enum NodeDescriptorKindV2 {
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
    Legacy(LegacyStorageRef),
    V2(V2StorageRef),
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyStorageRef {
    pub manifest_path: String,
    pub manifest_encrypted_sha256: Option<String>,
    pub chunks: Vec<LegacyChunkRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LegacyChunkRef {
    pub index: usize,
    pub path: String,
    pub original_size: u64,
    pub original_sha256: String,
    pub encrypted_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct V2StorageRef {
    pub manifest_path: String,
    pub manifest_encrypted_sha256: String,
    pub chunks: Vec<V2ChunkRef>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct V2ChunkRef {
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
pub struct ObjectManifestV2 {
    pub version: u8,
    pub format_version: u8,
    pub object_id: String,
    pub content_sha256: String,
    pub original_size: u64,
    pub chunks: Vec<V2ChunkRef>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct ObjectManifestV1 {
    version: u8,
    object_id: String,
    chunks: Vec<LegacyChunkRef>,
}

struct LoadedCatalogV2 {
    catalog: CatalogV2,
    migrated_from_v1: bool,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogV1 {
    version: u8,
    root: CatalogNodeV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CatalogNodeV1 {
    id: String,
    name: String,
    #[serde(default)]
    updated_at: String,
    kind: CatalogNodeKindV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum CatalogNodeKindV1 {
    Directory {
        children: Vec<CatalogNodeV1>,
    },
    File {
        original_size: u64,
        sha256: String,
        object_id: String,
        chunks: Vec<ChunkRecordV1>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChunkRecordV1 {
    index: usize,
    path: String,
    original_size: u64,
    original_sha256: String,
    encrypted_sha256: String,
}

impl Catalog {
    pub fn from_staging(staging_dir: impl Into<PathBuf>) -> Self {
        let staging_dir = staging_dir.into();
        Self {
            encrypted_catalog_path: staging_dir.join(CATALOG_FILE),
            staging_dir,
        }
    }

    pub fn pack(source: PackSource, key: &KeyFile, options: PackOptions) -> Result<Self> {
        Self::pack_with_report(source, key, options)?.into_catalog()
    }

    pub fn pack_with_report(
        source: PackSource,
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<PackOutcome> {
        Self::pack_with_optional_progress(source, key, options, None)
    }

    pub fn pack_with_progress(
        source: PackSource,
        key: &KeyFile,
        options: PackOptions,
        mut on_progress: impl FnMut(PackProgress),
    ) -> Result<Self> {
        Self::pack_with_progress_and_report(source, key, options, &mut on_progress)?.into_catalog()
    }

    pub fn pack_with_progress_and_report(
        source: PackSource,
        key: &KeyFile,
        options: PackOptions,
        mut on_progress: impl FnMut(PackProgress),
    ) -> Result<PackOutcome> {
        Self::pack_with_optional_progress(source, key, options, Some(&mut on_progress))
    }

    fn pack_with_optional_progress(
        source: PackSource,
        key: &KeyFile,
        options: PackOptions,
        on_progress: Option<&mut dyn FnMut(PackProgress)>,
    ) -> Result<PackOutcome> {
        if options.chunk_size == 0 {
            return Err(LiosError::Unsupported(
                "chunk size must be greater than zero".to_string(),
            ));
        }

        let PackSource::Path(source_path) = source;
        let source_kind = packable_path_kind(&source_path)?;
        let mut report = PackReport::default();
        if source_kind.is_none() {
            report.skipped_paths.push(skipped_link(&source_path));
            return Ok(PackOutcome::Skipped { report });
        }
        let name = file_name(&source_path)?;

        fs::create_dir_all(options.staging_dir.join(FILES_DIR))?;
        let mut tracker = PackProgressTracker::new(on_progress);
        tracker.add_total(pack_stats(&source_path, options.chunk_size)?);
        let mut plain = CatalogV2 {
            version: 2,
            root_id: String::new(),
            nodes: BTreeMap::new(),
            content_index: BTreeMap::new(),
            content_objects: BTreeMap::new(),
        };
        let root_id = pack_path_v2(
            &mut plain,
            &source_path,
            name.clone(),
            PathBuf::from(name),
            None,
            key,
            &options,
            &[],
            &mut tracker,
            &mut report,
        )?;
        plain.root_id = root_id;
        let encrypted_catalog_path = options.staging_dir.join(CATALOG_FILE);
        let catalog = Self {
            encrypted_catalog_path,
            staging_dir: options.staging_dir,
        };
        catalog.save_v2(&mut plain, key)?;

        Ok(PackOutcome::Packed { catalog, report })
    }

    pub fn initialize_empty(
        name: impl Into<String>,
        key: &KeyFile,
        staging_dir: impl Into<PathBuf>,
    ) -> Result<Self> {
        let staging_dir = staging_dir.into();
        fs::create_dir_all(&staging_dir)?;
        let catalog = Self::from_staging(staging_dir);
        let name = normalize_name(&name.into())?;
        let root_id = random_id();
        let descriptor = NodeDescriptorV2 {
            version: 2,
            node_id: root_id.clone(),
            parent_id: None,
            name,
            updated_at: timestamp(),
            kind: NodeDescriptorKindV2::Directory,
        };
        let mut plain = CatalogV2 {
            version: 2,
            root_id: root_id.clone(),
            nodes: BTreeMap::from([(
                root_id,
                CatalogNodeV2 {
                    descriptor,
                    descriptor_encrypted_sha256: None,
                },
            )]),
            content_index: BTreeMap::new(),
            content_objects: BTreeMap::new(),
        };
        catalog.save_v2(&mut plain, key)?;
        Ok(catalog)
    }

    pub fn encrypted_catalog_path(&self) -> &Path {
        &self.encrypted_catalog_path
    }

    pub fn decrypt_tree(&self, key: &KeyFile) -> Result<CatalogTreeNode> {
        let catalog = self.load_catalog_v2(key)?;
        tree_node_v2(&catalog.catalog, &catalog.catalog.root_id)
    }

    pub fn list_children(&self, parent_id: &str, key: &KeyFile) -> Result<Vec<DriveItem>> {
        let loaded = self.load_catalog_v2(key)?;
        let parent = catalog_node(&loaded.catalog, parent_id)?;
        match parent.descriptor.kind {
            NodeDescriptorKindV2::Directory => child_ids(&loaded.catalog, parent_id)
                .into_iter()
                .map(|id| drive_item_v2(&loaded.catalog, id))
                .collect(),
            NodeDescriptorKindV2::File { .. } => Err(LiosError::Unsupported(
                "cannot list children for a file".to_string(),
            )),
        }
    }

    pub fn search(&self, query: &str, key: &KeyFile) -> Result<Vec<DriveItem>> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let loaded = self.load_catalog_v2(key)?;
        let mut matches = Vec::new();
        collect_search_matches_v2(
            &loaded.catalog,
            &loaded.catalog.root_id,
            &query,
            &mut matches,
        )?;
        Ok(matches)
    }

    pub fn create_folder(&self, parent_id: &str, name: &str, key: &KeyFile) -> Result<()> {
        let name = normalize_name(name)?;
        let mut loaded = self.load_catalog_v2(key)?;
        ensure_directory_v2(&loaded.catalog, parent_id)?;
        if child_ids(&loaded.catalog, parent_id)
            .into_iter()
            .any(|id| windows_names_equal(&loaded.catalog.nodes[id].descriptor.name, &name))
        {
            return Err(LiosError::Unsupported(format!(
                "folder already contains {name}"
            )));
        }
        let node_id = random_id();
        loaded.catalog.nodes.insert(
            node_id.clone(),
            CatalogNodeV2 {
                descriptor: NodeDescriptorV2 {
                    version: 2,
                    node_id,
                    parent_id: Some(parent_id.to_string()),
                    name,
                    updated_at: timestamp(),
                    kind: NodeDescriptorKindV2::Directory,
                },
                descriptor_encrypted_sha256: None,
            },
        );
        mark_node_updated(&mut loaded.catalog, parent_id)?;
        prepare_catalog_for_v2_write(&mut loaded)?;
        self.save_v2(&mut loaded.catalog, key)
    }

    pub fn rename_node(&self, node_id: &str, new_name: &str, key: &KeyFile) -> Result<()> {
        let new_name = normalize_name(new_name)?;
        let mut loaded = self.load_catalog_v2(key)?;
        let parent_id = catalog_node(&loaded.catalog, node_id)?
            .descriptor
            .parent_id
            .clone();
        if let Some(parent_id) = parent_id {
            if child_ids(&loaded.catalog, &parent_id)
                .into_iter()
                .any(|id| {
                    id != node_id
                        && windows_names_equal(&loaded.catalog.nodes[id].descriptor.name, &new_name)
                })
            {
                return Err(LiosError::Unsupported(format!(
                    "folder already contains {new_name}"
                )));
            }
        }
        let node =
            loaded.catalog.nodes.get_mut(node_id).ok_or_else(|| {
                LiosError::Unsupported(format!("catalog node not found: {node_id}"))
            })?;
        node.descriptor.name = new_name;
        node.descriptor.updated_at = timestamp();
        node.descriptor_encrypted_sha256 = None;
        prepare_catalog_for_v2_write(&mut loaded)?;
        self.save_v2(&mut loaded.catalog, key)
    }

    pub fn delete_nodes(&self, node_ids: &[String], key: &KeyFile) -> Result<()> {
        let mut loaded = self.load_catalog_v2(key)?;
        let ids = node_ids
            .iter()
            .filter(|id| id.as_str() != loaded.catalog.root_id)
            .cloned()
            .collect::<HashSet<_>>();
        let mut remove = HashSet::new();
        for id in ids {
            collect_descendant_ids(&loaded.catalog, &id, &mut remove);
        }
        loaded.catalog.nodes.retain(|id, _| !remove.contains(id));
        prune_unreferenced_content(&mut loaded.catalog);
        prepare_catalog_for_v2_write(&mut loaded)?;
        self.save_v2(&mut loaded.catalog, key)
    }

    pub fn preview_upload_conflicts(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        key: &KeyFile,
    ) -> Result<Vec<UploadConflict>> {
        let loaded = self.load_catalog_v2(key)?;
        ensure_directory_v2(&loaded.catalog, parent_id)?;
        let mut conflicts = Vec::new();
        for path in paths {
            let target_name = file_name(path)?;
            if should_skip_link(path)? {
                continue;
            }
            if let Some(existing_id) =
                child_ids(&loaded.catalog, parent_id)
                    .into_iter()
                    .find(|id| {
                        windows_names_equal(
                            &loaded.catalog.nodes[*id].descriptor.name,
                            &target_name,
                        )
                    })
            {
                let existing = &loaded.catalog.nodes[existing_id].descriptor;
                conflicts.push(UploadConflict {
                    source_path: path.display().to_string(),
                    target_name,
                    existing_node_id: existing.node_id.clone(),
                    kind: if path.is_dir() {
                        DriveItemKind::Directory
                    } else {
                        DriveItemKind::File
                    },
                });
            }
        }
        Ok(conflicts)
    }

    pub fn add_paths_to_folder(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<()> {
        let report =
            self.add_paths_to_folder_with_report(parent_id, paths, resolutions, key, options)?;
        report.ensure_no_skipped_paths()
    }

    pub fn add_paths_to_folder_with_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
    ) -> Result<PackReport> {
        self.add_paths_to_folder_with_optional_progress(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            &[],
            None,
        )
    }

    pub fn add_paths_to_folder_with_remote_inventory(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        remote_objects: &[StorageObject],
    ) -> Result<()> {
        let report = self.add_paths_to_folder_with_optional_progress(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            remote_objects,
            None,
        )?;
        report.ensure_no_skipped_paths()
    }

    pub fn add_paths_to_folder_with_progress(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        mut on_progress: impl FnMut(PackProgress),
    ) -> Result<()> {
        let report = self.add_paths_to_folder_with_progress_and_report(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            &mut on_progress,
        )?;
        report.ensure_no_skipped_paths()
    }

    pub fn add_paths_to_folder_with_progress_and_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        mut on_progress: impl FnMut(PackProgress),
    ) -> Result<PackReport> {
        self.add_paths_to_folder_with_optional_progress(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            &[],
            Some(&mut on_progress),
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn add_paths_to_folder_with_remote_inventory_and_progress_and_report(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        remote_objects: &[StorageObject],
        mut on_progress: impl FnMut(PackProgress),
    ) -> Result<PackReport> {
        self.add_paths_to_folder_with_optional_progress(
            parent_id,
            paths,
            resolutions,
            key,
            options,
            remote_objects,
            Some(&mut on_progress),
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn add_paths_to_folder_with_optional_progress(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        remote_objects: &[StorageObject],
        on_progress: Option<&mut dyn FnMut(PackProgress)>,
    ) -> Result<PackReport> {
        if options.chunk_size == 0 {
            return Err(LiosError::Unsupported(
                "chunk size must be greater than zero".to_string(),
            ));
        }
        fs::create_dir_all(options.staging_dir.join(FILES_DIR))?;
        let mut loaded = self.load_catalog_v2(key)?;
        ensure_directory_v2(&loaded.catalog, parent_id)?;
        let mut tracker = PackProgressTracker::new(on_progress);
        let mut report = PackReport::default();
        let resolution_by_source = resolutions
            .iter()
            .map(|resolution| (resolution.source_path.as_str(), resolution.action.clone()))
            .collect::<HashMap<_, _>>();

        let mut total_stats = PackStats {
            chunks: 0,
            bytes: 0,
        };
        for path in paths {
            let target_name = file_name(path)?;
            let Some(_) = packable_path_kind(path)? else {
                continue;
            };
            let source_key = path.display().to_string();
            let conflict_action = if child_ids(&loaded.catalog, parent_id).into_iter().any(|id| {
                windows_names_equal(&loaded.catalog.nodes[id].descriptor.name, &target_name)
            }) {
                Some(
                    resolution_by_source
                        .get(source_key.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            LiosError::Unsupported(format!(
                                "upload conflict was not resolved: {target_name}"
                            ))
                        })?,
                )
            } else {
                None
            };
            if matches!(conflict_action, Some(ConflictAction::Skip)) {
                continue;
            }
            let stats = pack_stats(path, options.chunk_size)?;
            total_stats.chunks += stats.chunks;
            total_stats.bytes += stats.bytes;
        }
        if total_stats.chunks > 0 || total_stats.bytes > 0 {
            tracker.add_total(total_stats);
        }

        for path in paths {
            let source_name = file_name(path)?;
            let source_relative_path = PathBuf::from(&source_name);
            let mut target_name = source_name;
            let Some(_) = packable_path_kind(path)? else {
                report.skipped_paths.push(skipped_link(path));
                continue;
            };
            let source_key = path.display().to_string();
            let existing_id = child_ids(&loaded.catalog, parent_id)
                .into_iter()
                .find(|id| {
                    windows_names_equal(&loaded.catalog.nodes[*id].descriptor.name, &target_name)
                })
                .cloned();
            let conflict_action = if existing_id.is_some() {
                Some(
                    resolution_by_source
                        .get(source_key.as_str())
                        .cloned()
                        .ok_or_else(|| {
                            LiosError::Unsupported(format!(
                                "upload conflict was not resolved: {target_name}"
                            ))
                        })?,
                )
            } else {
                None
            };

            match conflict_action {
                Some(ConflictAction::Skip) => continue,
                Some(ConflictAction::KeepBoth) => {
                    target_name = available_name(
                        &child_ids(&loaded.catalog, parent_id)
                            .into_iter()
                            .map(|id| loaded.catalog.nodes[id].descriptor.name.as_str())
                            .collect::<Vec<_>>(),
                        &target_name,
                    );
                }
                Some(ConflictAction::Replace) => {
                    if let Some(existing_id) = existing_id {
                        let mut remove = HashSet::new();
                        collect_descendant_ids(&loaded.catalog, &existing_id, &mut remove);
                        loaded.catalog.nodes.retain(|id, _| !remove.contains(id));
                    }
                }
                None => {}
            }

            pack_path_v2(
                &mut loaded.catalog,
                path,
                target_name,
                source_relative_path,
                Some(parent_id.to_string()),
                key,
                &options,
                remote_objects,
                &mut tracker,
                &mut report,
            )?;
            mark_node_updated(&mut loaded.catalog, parent_id)?;
        }
        prune_unreferenced_content(&mut loaded.catalog);
        prepare_catalog_for_v2_write(&mut loaded)?;
        self.save_v2(&mut loaded.catalog, key)?;
        Ok(report)
    }

    pub fn remote_files_for_selection(
        &self,
        selection: &CatalogSelection,
        key: &KeyFile,
    ) -> Result<Vec<CatalogRemoteFile>> {
        let loaded = self.load_catalog_v2(key)?;
        let selected_ids = match selection {
            CatalogSelection::All => vec![loaded.catalog.root_id.clone()],
            CatalogSelection::Node(id) => {
                catalog_node(&loaded.catalog, id)?;
                vec![id.clone()]
            }
            CatalogSelection::Nodes(ids) => {
                for id in ids {
                    catalog_node(&loaded.catalog, id)?;
                }
                ids.clone()
            }
        };
        let mut files = Vec::new();
        let mut object_ids = HashSet::new();
        let mut node_ids = HashSet::new();
        for id in selected_ids {
            collect_descendant_ids(&loaded.catalog, &id, &mut node_ids);
        }
        for (id, node) in &loaded.catalog.nodes {
            if let Some(sha256) = &node.descriptor_encrypted_sha256 {
                files.push(CatalogRemoteFile {
                    path: format!("{NODE_DESCRIPTORS_DIR}/{id}.enc"),
                    sha256: Some(sha256.clone()),
                });
            }
        }
        for id in &node_ids {
            let node = catalog_node(&loaded.catalog, id)?;
            if let NodeDescriptorKindV2::File { object_id, .. } = &node.descriptor.kind {
                object_ids.insert(object_id.clone());
            }
        }
        for object_id in object_ids {
            let object = loaded
                .catalog
                .content_objects
                .get(&object_id)
                .ok_or_else(|| {
                    LiosError::DataCorruption(format!("missing content object: {object_id}"))
                })?;
            collect_content_remote_files(object, &mut files);
        }
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Ok(files)
    }

    pub fn verify_staged_integrity(&self, key: &KeyFile) -> Result<CatalogIntegrityReport> {
        let loaded = self.load_catalog_v2(key)?;
        let mut report = CatalogIntegrityReport::default();
        for (node_id, node) in &loaded.catalog.nodes {
            let expected_sha256 = match &node.descriptor_encrypted_sha256 {
                Some(expected_sha256) => expected_sha256,
                None if loaded.migrated_from_v1 => continue,
                None => {
                    return Err(LiosError::DataCorruption(format!(
                        "native v2 node descriptor hash is missing: {node_id}"
                    )))
                }
            };
            let relative_path = format!("{NODE_DESCRIPTORS_DIR}/{node_id}.enc");
            let encrypted = read_verified_encrypted_file(
                &self.staging_dir,
                &relative_path,
                Some(expected_sha256),
                None,
            )?;
            let plaintext =
                decrypt_compatible_v1_or_v2(key, EnvelopeKindV2::NodeDescriptor, &encrypted)?;
            let descriptor: NodeDescriptorV2 = serde_json::from_slice(&plaintext)?;
            if descriptor != node.descriptor {
                return Err(LiosError::DataCorruption(format!(
                    "node descriptor mismatch: {node_id}"
                )));
            }
            report.nodes_verified = report.nodes_verified.checked_add(1).ok_or_else(|| {
                LiosError::DataCorruption("verified node count overflowed".to_string())
            })?;
            add_verified_encoded_bytes(&mut report, encrypted.len())?;
        }

        for object in loaded.catalog.content_objects.values() {
            verify_content_object_integrity(object, key, &self.staging_dir, &mut report)?;
            report.objects_verified = report.objects_verified.checked_add(1).ok_or_else(|| {
                LiosError::DataCorruption("verified object count overflowed".to_string())
            })?;
            report.original_bytes_verified = report
                .original_bytes_verified
                .checked_add(object.original_size)
                .ok_or_else(|| {
                    LiosError::DataCorruption("verified original byte count overflowed".to_string())
                })?;
        }
        Ok(report)
    }

    pub fn legacy_content_objects_needing_optimization(
        &self,
        key: &KeyFile,
    ) -> Result<Vec<LegacyContentOptimizationSummary>> {
        let loaded = self.load_catalog_v2(key)?;
        Ok(loaded
            .catalog
            .content_objects
            .values()
            .filter(|object| matches!(object.storage, StorageRef::Legacy(_)))
            .map(|object| LegacyContentOptimizationSummary {
                object_id: object.object_id.clone(),
                content_sha256: object.content_sha256.clone(),
                original_size: object.original_size,
            })
            .collect())
    }

    pub fn restore(
        &self,
        selection: CatalogSelection,
        key: &KeyFile,
        options: RestoreOptions,
    ) -> Result<()> {
        let loaded = self.load_catalog_v2(key)?;
        fs::create_dir_all(&options.output_dir)?;
        match selection {
            CatalogSelection::All => {
                restore_node_v2(
                    &loaded.catalog,
                    &loaded.catalog.root_id,
                    &options.output_dir,
                    key,
                    &self.staging_dir,
                    &options,
                )?;
            }
            CatalogSelection::Node(id) => {
                restore_node_v2(
                    &loaded.catalog,
                    &id,
                    &options.output_dir,
                    key,
                    &self.staging_dir,
                    &options,
                )?;
            }
            CatalogSelection::Nodes(ids) => {
                for id in ids {
                    restore_node_v2(
                        &loaded.catalog,
                        &id,
                        &options.output_dir,
                        key,
                        &self.staging_dir,
                        &options,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn load_catalog_v2(&self, key: &KeyFile) -> Result<LoadedCatalogV2> {
        let encrypted = read_verified_encrypted_file(&self.staging_dir, CATALOG_FILE, None, None)?;
        let decrypted = decrypt_compatible_v1_or_v2(key, EnvelopeKindV2::Catalog, &encrypted)?;
        let value: serde_json::Value = serde_json::from_slice(&decrypted)?;
        let version = value
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| LiosError::Unsupported("catalog version is missing".to_string()))?;
        match version {
            1 => {
                let catalog: CatalogV1 = serde_json::from_value(value)?;
                Ok(LoadedCatalogV2 {
                    catalog: normalize_v1_catalog(catalog)?,
                    migrated_from_v1: true,
                })
            }
            2 => {
                let catalog: CatalogV2 = serde_json::from_value(value)?;
                validate_catalog_v2(&catalog, true)?;
                Ok(LoadedCatalogV2 {
                    catalog,
                    migrated_from_v1: false,
                })
            }
            _ => Err(LiosError::Unsupported(format!(
                "unknown catalog version: {version}"
            ))),
        }
    }

    fn save_v2(&self, catalog: &mut CatalogV2, key: &KeyFile) -> Result<()> {
        validate_catalog_v2(catalog, true)?;
        fs::create_dir_all(self.staging_dir.join(NODE_DESCRIPTORS_DIR))?;
        for node in catalog.nodes.values_mut() {
            if node.descriptor_encrypted_sha256.is_some() {
                continue;
            }
            let plaintext = serde_json::to_vec(&node.descriptor)?;
            let encrypted = encrypt_envelope_v2(key, EnvelopeKindV2::NodeDescriptor, &plaintext)?;
            let path = self
                .staging_dir
                .join(NODE_DESCRIPTORS_DIR)
                .join(format!("{}.enc", node.descriptor.node_id));
            write_atomic(&path, &encrypted)?;
            node.descriptor_encrypted_sha256 = Some(sha256_hex(&encrypted));
        }
        let serialized = serde_json::to_vec(catalog)?;
        let encrypted = encrypt_envelope_v2(key, EnvelopeKindV2::Catalog, &serialized)?;
        write_atomic(&self.encrypted_catalog_path, &encrypted)?;
        Ok(())
    }
}

fn normalize_v1_catalog(catalog: CatalogV1) -> Result<CatalogV2> {
    if catalog.version != 1 {
        return Err(LiosError::Unsupported(format!(
            "unknown catalog version: {}",
            catalog.version
        )));
    }
    let root_id = catalog.root.id.clone();
    let mut normalized = CatalogV2 {
        version: 2,
        root_id,
        nodes: BTreeMap::new(),
        content_index: BTreeMap::new(),
        content_objects: BTreeMap::new(),
    };
    normalize_v1_node(catalog.root, None, &mut normalized)?;
    rebuild_content_index(&mut normalized);
    validate_catalog_v2(&normalized, false)?;
    Ok(normalized)
}

fn normalize_v1_node(
    node: CatalogNodeV1,
    parent_id: Option<String>,
    catalog: &mut CatalogV2,
) -> Result<()> {
    let mut pending = vec![(node, parent_id, 0usize)];
    while let Some((node, parent_id, depth)) = pending.pop() {
        let CatalogNodeV1 {
            id,
            name,
            updated_at,
            kind,
        } = node;
        let descriptor_kind = match kind {
            CatalogNodeKindV1::Directory { children } => {
                if !children.is_empty() && depth >= MAX_CATALOG_DEPTH {
                    return Err(catalog_depth_error());
                }
                let child_depth = depth.checked_add(1).ok_or_else(catalog_depth_error)?;
                for child in children.into_iter().rev() {
                    pending.push((child, Some(id.clone()), child_depth));
                }
                NodeDescriptorKindV2::Directory
            }
            CatalogNodeKindV1::File {
                original_size,
                sha256,
                object_id,
                chunks,
            } => {
                let storage = LegacyStorageRef {
                    manifest_path: format!("{FILES_DIR}/{object_id}/{FILE_MANIFEST}"),
                    manifest_encrypted_sha256: None,
                    chunks: chunks
                        .into_iter()
                        .map(|chunk| LegacyChunkRef {
                            index: chunk.index,
                            path: chunk.path,
                            original_size: chunk.original_size,
                            original_sha256: chunk.original_sha256,
                            encrypted_sha256: chunk.encrypted_sha256,
                        })
                        .collect(),
                };
                let content = ContentObject {
                    object_id: object_id.clone(),
                    content_sha256: sha256.clone(),
                    original_size,
                    storage: StorageRef::Legacy(storage),
                };
                if let Some(existing) = catalog.content_objects.get(&object_id) {
                    if existing != &content {
                        return Err(LiosError::DataCorruption(format!(
                            "conflicting legacy content object: {object_id}"
                        )));
                    }
                } else {
                    catalog.content_objects.insert(object_id.clone(), content);
                }
                catalog
                    .content_index
                    .entry(sha256.clone())
                    .or_insert_with(|| object_id.clone());
                NodeDescriptorKindV2::File {
                    object_id,
                    content_sha256: sha256,
                    original_size,
                }
            }
        };
        let descriptor = NodeDescriptorV2 {
            version: 2,
            node_id: id.clone(),
            parent_id,
            name,
            updated_at,
            kind: descriptor_kind,
        };
        if catalog
            .nodes
            .insert(
                id.clone(),
                CatalogNodeV2 {
                    descriptor,
                    descriptor_encrypted_sha256: None,
                },
            )
            .is_some()
        {
            return Err(LiosError::DataCorruption(format!(
                "duplicate catalog node id: {id}"
            )));
        }
    }
    Ok(())
}

fn validate_catalog_v2(catalog: &CatalogV2, require_canonical_node_ids: bool) -> Result<()> {
    if catalog.version != 2 {
        return Err(LiosError::Unsupported(format!(
            "unknown catalog version: {}",
            catalog.version
        )));
    }
    if require_canonical_node_ids {
        validate_opaque_id_v2(&catalog.root_id, "node")?;
    }
    let root = catalog
        .nodes
        .get(&catalog.root_id)
        .ok_or_else(|| LiosError::DataCorruption("catalog root node is missing".to_string()))?;
    if root.descriptor.parent_id.is_some() {
        return Err(LiosError::DataCorruption(
            "catalog root has a parent".to_string(),
        ));
    }
    let mut children_by_parent = HashMap::<&str, Vec<&str>>::new();
    let mut sibling_names = HashMap::<&str, HashSet<String>>::new();
    for (id, node) in &catalog.nodes {
        if require_canonical_node_ids {
            validate_opaque_id_v2(id, "node")?;
        }
        if node.descriptor.version != 2 || node.descriptor.node_id != *id {
            return Err(LiosError::DataCorruption(format!(
                "invalid catalog node descriptor: {id}"
            )));
        }
        if id != &catalog.root_id {
            let parent_id = node.descriptor.parent_id.as_deref().ok_or_else(|| {
                LiosError::DataCorruption(format!("non-root catalog node has no parent: {id}"))
            })?;
            let parent = catalog.nodes.get(parent_id).ok_or_else(|| {
                LiosError::DataCorruption(format!("missing catalog parent: {parent_id}"))
            })?;
            if !matches!(parent.descriptor.kind, NodeDescriptorKindV2::Directory) {
                return Err(LiosError::DataCorruption(format!(
                    "catalog parent is not a directory: {parent_id}"
                )));
            }
            let name_key = windows_name_key(&node.descriptor.name);
            if !sibling_names.entry(parent_id).or_default().insert(name_key) {
                return Err(LiosError::DataCorruption(format!(
                    "duplicate sibling name under Windows semantics: {}",
                    node.descriptor.name
                )));
            }
            children_by_parent
                .entry(parent_id)
                .or_default()
                .push(id.as_str());
        }
        if let NodeDescriptorKindV2::File {
            object_id,
            content_sha256,
            original_size,
        } = &node.descriptor.kind
        {
            let object = catalog.content_objects.get(object_id).ok_or_else(|| {
                LiosError::DataCorruption(format!("missing content object: {object_id}"))
            })?;
            if object.content_sha256 != *content_sha256 || object.original_size != *original_size {
                return Err(LiosError::DataCorruption(format!(
                    "file node content metadata mismatch: {id}"
                )));
            }
        }
    }
    let mut visited = HashSet::with_capacity(catalog.nodes.len());
    let mut pending = vec![(catalog.root_id.as_str(), 0usize)];
    while let Some((node_id, depth)) = pending.pop() {
        if depth > MAX_CATALOG_DEPTH {
            return Err(catalog_depth_error());
        }
        if !visited.insert(node_id) {
            return Err(LiosError::DataCorruption(format!(
                "catalog node is reachable more than once: {node_id}"
            )));
        }
        if let Some(children) = children_by_parent.get(node_id) {
            let child_depth = depth.checked_add(1).ok_or_else(catalog_depth_error)?;
            if child_depth > MAX_CATALOG_DEPTH {
                return Err(catalog_depth_error());
            }
            pending.extend(children.iter().map(|child_id| (*child_id, child_depth)));
        }
    }
    if visited.len() != catalog.nodes.len() {
        return Err(LiosError::DataCorruption(
            "catalog contains a cycle or disconnected node".to_string(),
        ));
    }
    let mut remote_paths = HashSet::new();
    for (object_id, object) in &catalog.content_objects {
        if object.object_id != *object_id {
            return Err(LiosError::DataCorruption(format!(
                "content object id mismatch: {object_id}"
            )));
        }
        validate_storage_ref(object, &mut remote_paths)?;
    }
    for (sha256, object_id) in &catalog.content_index {
        let object = catalog.content_objects.get(object_id).ok_or_else(|| {
            LiosError::DataCorruption(format!("content index object is missing: {object_id}"))
        })?;
        if object.content_sha256 != *sha256 {
            return Err(LiosError::DataCorruption(format!(
                "content index hash mismatch: {sha256}"
            )));
        }
    }
    for object in catalog.content_objects.values() {
        if !catalog.content_index.contains_key(&object.content_sha256) {
            return Err(LiosError::DataCorruption(format!(
                "content index hash is missing: {}",
                object.content_sha256
            )));
        }
    }
    Ok(())
}

fn catalog_depth_error() -> LiosError {
    LiosError::DataCorruption(format!(
        "catalog depth exceeds conservative limit of {MAX_CATALOG_DEPTH}"
    ))
}

fn validate_storage_ref(object: &ContentObject, remote_paths: &mut HashSet<String>) -> Result<()> {
    match &object.storage {
        StorageRef::Legacy(storage) => {
            validate_legacy_object_path(&storage.manifest_path)?;
            if !remote_paths.insert(windows_name_key(&storage.manifest_path)) {
                return Err(LiosError::DataCorruption(format!(
                    "duplicate legacy object path: {}",
                    storage.manifest_path
                )));
            }
            for chunk in &storage.chunks {
                validate_legacy_object_path(&chunk.path)?;
                if !remote_paths.insert(windows_name_key(&chunk.path)) {
                    return Err(LiosError::DataCorruption(format!(
                        "duplicate legacy object path: {}",
                        chunk.path
                    )));
                }
            }
        }
        StorageRef::V2(storage) => {
            validate_opaque_id_v2(&object.object_id, "object")?;
            let expected_manifest = format!("{FILES_DIR}/{}/{}", object.object_id, FILE_MANIFEST);
            if storage.manifest_path != expected_manifest {
                return Err(LiosError::DataCorruption(format!(
                    "invalid v2 manifest path: {}",
                    storage.manifest_path
                )));
            }
            validate_remote_object_path(&storage.manifest_path)?;
            if !remote_paths.insert(windows_name_key(&storage.manifest_path)) {
                return Err(LiosError::DataCorruption(format!(
                    "duplicate v2 object path: {}",
                    storage.manifest_path
                )));
            }
            for (index, chunk) in storage.chunks.iter().enumerate() {
                validate_remote_object_path(&chunk.path)?;
                if !remote_paths.insert(windows_name_key(&chunk.path)) {
                    return Err(LiosError::DataCorruption(format!(
                        "duplicate v2 object path: {}",
                        chunk.path
                    )));
                }
                if chunk.index != index || chunk.format_version != 2 {
                    return Err(LiosError::DataCorruption(format!(
                        "invalid v2 chunk reference: {}",
                        chunk.path
                    )));
                }
                validate_lower_hex_id(&chunk.chunk_id, 64, "chunk")?;
                parse_chunk_id_v2(&chunk.chunk_id)?;
                let expected_path = format!(
                    "{FILES_DIR}/{}/{FILE_CHUNKS_DIR}/{}.lios",
                    object.object_id, chunk.chunk_id
                );
                if chunk.path != expected_path {
                    return Err(LiosError::DataCorruption(format!(
                        "invalid v2 chunk path: {}",
                        chunk.path
                    )));
                }
            }
        }
    }
    Ok(())
}

fn validate_remote_object_path(path: &str) -> Result<()> {
    let relative = Path::new(path);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(LiosError::Unsupported(format!(
            "invalid remote object path in catalog: {path}"
        )));
    }
    Ok(())
}

fn validate_legacy_object_path(path: &str) -> Result<()> {
    validate_remote_object_path(path)?;
    let mut components = Path::new(path).components();
    let inside_objects = matches!(
        components.next(),
        Some(std::path::Component::Normal(component)) if component == "objects"
    ) && components.next().is_some();
    if !inside_objects {
        return Err(LiosError::DataCorruption(format!(
            "legacy path is outside the managed objects prefix: {path}"
        )));
    }
    Ok(())
}

fn validate_opaque_id_v2(value: &str, kind: &'static str) -> Result<()> {
    validate_lower_hex_id(value, 32, kind)
}

fn validate_lower_hex_id(value: &str, expected_len: usize, kind: &'static str) -> Result<()> {
    if value.len() != expected_len
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LiosError::DataCorruption(format!(
            "invalid v2 {kind} id: {value}"
        )));
    }
    Ok(())
}

fn prepare_catalog_for_v2_write(loaded: &mut LoadedCatalogV2) -> Result<()> {
    loaded.catalog.version = 2;
    rebuild_content_index(&mut loaded.catalog);
    if !loaded.migrated_from_v1 {
        validate_catalog_v2(&loaded.catalog, true)?;
        return Ok(());
    }

    let id_map = loaded
        .catalog
        .nodes
        .keys()
        .map(|id| (id.clone(), random_id()))
        .collect::<BTreeMap<_, _>>();
    let old_root_id = loaded.catalog.root_id.clone();
    let mut remapped = BTreeMap::new();
    for (old_id, mut node) in std::mem::take(&mut loaded.catalog.nodes) {
        let new_id = id_map[&old_id].clone();
        node.descriptor.node_id = new_id.clone();
        node.descriptor.parent_id = node
            .descriptor
            .parent_id
            .as_ref()
            .map(|parent_id| id_map[parent_id].clone());
        node.descriptor_encrypted_sha256 = None;
        remapped.insert(new_id, node);
    }
    loaded.catalog.root_id = id_map[&old_root_id].clone();
    loaded.catalog.nodes = remapped;
    loaded.migrated_from_v1 = false;
    validate_catalog_v2(&loaded.catalog, true)
}

fn catalog_node<'a>(catalog: &'a CatalogV2, id: &str) -> Result<&'a CatalogNodeV2> {
    catalog
        .nodes
        .get(id)
        .ok_or_else(|| LiosError::Unsupported(format!("catalog node not found: {id}")))
}

fn ensure_directory_v2(catalog: &CatalogV2, id: &str) -> Result<()> {
    match catalog_node(catalog, id)?.descriptor.kind {
        NodeDescriptorKindV2::Directory => Ok(()),
        NodeDescriptorKindV2::File { .. } => Err(LiosError::Unsupported(
            "catalog node is not a directory".to_string(),
        )),
    }
}

fn child_ids<'a>(catalog: &'a CatalogV2, parent_id: &str) -> Vec<&'a String> {
    let mut children = catalog
        .nodes
        .iter()
        .filter(|(_, node)| node.descriptor.parent_id.as_deref() == Some(parent_id))
        .map(|(id, _)| id)
        .collect::<Vec<_>>();
    children.sort_by(|left, right| {
        let left = &catalog.nodes[*left].descriptor;
        let right = &catalog.nodes[*right].descriptor;
        let left_dir = matches!(left.kind, NodeDescriptorKindV2::Directory);
        let right_dir = matches!(right.kind, NodeDescriptorKindV2::Directory);
        right_dir
            .cmp(&left_dir)
            .then_with(|| sort_name_key(&left.name).cmp(&sort_name_key(&right.name)))
    });
    children
}

fn tree_node_v2(catalog: &CatalogV2, node_id: &str) -> Result<CatalogTreeNode> {
    let node = catalog_node(catalog, node_id)?;
    let kind = match &node.descriptor.kind {
        NodeDescriptorKindV2::Directory => CatalogTreeNodeKind::Directory {
            children: child_ids(catalog, node_id)
                .into_iter()
                .map(|child_id| tree_node_v2(catalog, child_id))
                .collect::<Result<Vec<_>>>()?,
        },
        NodeDescriptorKindV2::File {
            original_size,
            content_sha256,
            object_id,
        } => {
            let object = catalog.content_objects.get(object_id).ok_or_else(|| {
                LiosError::DataCorruption(format!("missing content object: {object_id}"))
            })?;
            CatalogTreeNodeKind::File {
                original_size: *original_size,
                sha256: content_sha256.clone(),
                object_id: object_id.clone(),
                chunk_count: storage_chunk_count(&object.storage),
            }
        }
    };
    Ok(CatalogTreeNode {
        id: node_id.to_string(),
        name: node.descriptor.name.clone(),
        updated_at: node.descriptor.updated_at.clone(),
        kind,
    })
}

fn drive_item_v2(catalog: &CatalogV2, node_id: &str) -> Result<DriveItem> {
    let node = catalog_node(catalog, node_id)?;
    match &node.descriptor.kind {
        NodeDescriptorKindV2::Directory => Ok(DriveItem {
            id: node_id.to_string(),
            name: node.descriptor.name.clone(),
            kind: DriveItemKind::Directory,
            size: 0,
            updated_at: node.descriptor.updated_at.clone(),
            children_count: child_ids(catalog, node_id).len(),
        }),
        NodeDescriptorKindV2::File { original_size, .. } => Ok(DriveItem {
            id: node_id.to_string(),
            name: node.descriptor.name.clone(),
            kind: DriveItemKind::File,
            size: *original_size,
            updated_at: node.descriptor.updated_at.clone(),
            children_count: 0,
        }),
    }
}

fn collect_search_matches_v2(
    catalog: &CatalogV2,
    node_id: &str,
    query: &str,
    matches: &mut Vec<DriveItem>,
) -> Result<()> {
    let node = catalog_node(catalog, node_id)?;
    if node.descriptor.name.to_lowercase().contains(query) {
        matches.push(drive_item_v2(catalog, node_id)?);
    }
    for child_id in child_ids(catalog, node_id) {
        collect_search_matches_v2(catalog, child_id, query, matches)?;
    }
    Ok(())
}

fn mark_node_updated(catalog: &mut CatalogV2, node_id: &str) -> Result<()> {
    let node = catalog
        .nodes
        .get_mut(node_id)
        .ok_or_else(|| LiosError::Unsupported(format!("catalog node not found: {node_id}")))?;
    node.descriptor.updated_at = timestamp();
    node.descriptor_encrypted_sha256 = None;
    Ok(())
}

fn collect_descendant_ids(catalog: &CatalogV2, node_id: &str, ids: &mut HashSet<String>) {
    if !catalog.nodes.contains_key(node_id) || !ids.insert(node_id.to_string()) {
        return;
    }
    for child_id in child_ids(catalog, node_id) {
        collect_descendant_ids(catalog, child_id, ids);
    }
}

fn prune_unreferenced_content(catalog: &mut CatalogV2) {
    let referenced = catalog
        .nodes
        .values()
        .filter_map(|node| match &node.descriptor.kind {
            NodeDescriptorKindV2::File { object_id, .. } => Some(object_id.clone()),
            NodeDescriptorKindV2::Directory => None,
        })
        .collect::<HashSet<_>>();
    catalog
        .content_objects
        .retain(|object_id, _| referenced.contains(object_id));
    rebuild_content_index(catalog);
}

fn rebuild_content_index(catalog: &mut CatalogV2) {
    catalog.content_index.clear();
    for (object_id, object) in &catalog.content_objects {
        catalog
            .content_index
            .entry(object.content_sha256.clone())
            .or_insert_with(|| object_id.clone());
    }
}

fn storage_chunk_count(storage: &StorageRef) -> usize {
    match storage {
        StorageRef::Legacy(storage) => storage.chunks.len(),
        StorageRef::V2(storage) => storage.chunks.len(),
    }
}

fn collect_content_remote_files(object: &ContentObject, files: &mut Vec<CatalogRemoteFile>) {
    match &object.storage {
        StorageRef::Legacy(storage) => {
            files.push(CatalogRemoteFile {
                path: storage.manifest_path.clone(),
                sha256: storage.manifest_encrypted_sha256.clone(),
            });
            files.extend(storage.chunks.iter().map(|chunk| CatalogRemoteFile {
                path: chunk.path.clone(),
                sha256: Some(chunk.encrypted_sha256.clone()),
            }));
        }
        StorageRef::V2(storage) => {
            files.push(CatalogRemoteFile {
                path: storage.manifest_path.clone(),
                sha256: Some(storage.manifest_encrypted_sha256.clone()),
            });
            files.extend(storage.chunks.iter().map(|chunk| CatalogRemoteFile {
                path: chunk.path.clone(),
                sha256: Some(chunk.encoded_sha256.clone()),
            }));
        }
    }
}

struct PackProgressTracker<'a> {
    completed_chunks: u64,
    total_chunks: u64,
    completed_bytes: u64,
    total_bytes: u64,
    on_progress: Option<&'a mut dyn FnMut(PackProgress)>,
}

impl<'a> PackProgressTracker<'a> {
    fn new(on_progress: Option<&'a mut dyn FnMut(PackProgress)>) -> Self {
        Self {
            completed_chunks: 0,
            total_chunks: 0,
            completed_bytes: 0,
            total_bytes: 0,
            on_progress,
        }
    }

    fn add_total(&mut self, stats: PackStats) {
        self.total_chunks += stats.chunks;
        self.total_bytes += stats.bytes;
        self.emit();
    }

    fn complete_chunk(&mut self, bytes: u64) {
        self.completed_chunks += 1;
        self.completed_bytes += bytes;
        self.emit();
    }

    fn emit(&mut self) {
        if let Some(callback) = self.on_progress.as_mut() {
            callback(PackProgress {
                completed_chunks: self.completed_chunks,
                total_chunks: self.total_chunks,
                completed_bytes: self.completed_bytes,
                total_bytes: self.total_bytes,
            });
        }
    }
}

#[derive(Clone, Copy)]
struct PackStats {
    chunks: u64,
    bytes: u64,
}

fn pack_stats(path: &Path, chunk_size: usize) -> Result<PackStats> {
    let Some(path_kind) = packable_path_kind(path)? else {
        return Ok(PackStats {
            chunks: 0,
            bytes: 0,
        });
    };
    if path_kind == PackablePathKind::File {
        let len = source_metadata(path)?.len();
        if len == 0 {
            return Ok(PackStats {
                chunks: 1,
                bytes: 0,
            });
        }
        let chunk_size = chunk_size as u64;
        return Ok(PackStats {
            chunks: len.div_ceil(chunk_size),
            bytes: len,
        });
    }
    if path_kind == PackablePathKind::Directory {
        let mut total = PackStats {
            chunks: 0,
            bytes: 0,
        };
        for entry in source_directory_entries(path)? {
            let child = pack_stats(&entry.path(), chunk_size)?;
            total.chunks += child.chunks;
            total.bytes += child.bytes;
        }
        return Ok(total);
    }
    Ok(PackStats {
        chunks: 0,
        bytes: 0,
    })
}

#[allow(clippy::too_many_arguments)]
fn pack_path_v2(
    catalog: &mut CatalogV2,
    path: &Path,
    name: String,
    relative_path: PathBuf,
    parent_id: Option<String>,
    key: &KeyFile,
    options: &PackOptions,
    remote_objects: &[StorageObject],
    progress: &mut PackProgressTracker<'_>,
    report: &mut PackReport,
) -> Result<String> {
    let path_kind = ensure_packable_path(path)?;
    let node_id = random_id();
    match path_kind {
        PackablePathKind::Directory => {
            report.packed_directories.push(SourceDirectorySnapshot {
                source_path: path.to_path_buf(),
                relative_path: relative_path.clone(),
            });
            catalog.nodes.insert(
                node_id.clone(),
                CatalogNodeV2 {
                    descriptor: NodeDescriptorV2 {
                        version: 2,
                        node_id: node_id.clone(),
                        parent_id,
                        name,
                        updated_at: timestamp(),
                        kind: NodeDescriptorKindV2::Directory,
                    },
                    descriptor_encrypted_sha256: None,
                },
            );
            let entries = source_directory_entries(path)?;
            for entry in entries {
                let child_path = entry.path();
                let Some(_) = packable_path_kind(&child_path)? else {
                    report.skipped_paths.push(skipped_link(&child_path));
                    continue;
                };
                let child_name = file_name(&child_path)?;
                pack_path_v2(
                    catalog,
                    &child_path,
                    child_name.clone(),
                    relative_path.join(child_name),
                    Some(node_id.clone()),
                    key,
                    options,
                    remote_objects,
                    progress,
                    report,
                )?;
            }
        }
        PackablePathKind::File => {
            let (object_id, source_snapshot) = pack_content_object_v2(
                catalog,
                path,
                &relative_path,
                key,
                options,
                remote_objects,
                progress,
            )?;
            report.packed_files.push(source_snapshot);
            let object = catalog.content_objects.get(&object_id).ok_or_else(|| {
                LiosError::DataCorruption(format!("missing packed content object: {object_id}"))
            })?;
            catalog.nodes.insert(
                node_id.clone(),
                CatalogNodeV2 {
                    descriptor: NodeDescriptorV2 {
                        version: 2,
                        node_id: node_id.clone(),
                        parent_id,
                        name,
                        updated_at: timestamp(),
                        kind: NodeDescriptorKindV2::File {
                            object_id,
                            content_sha256: object.content_sha256.clone(),
                            original_size: object.original_size,
                        },
                    },
                    descriptor_encrypted_sha256: None,
                },
            );
        }
    }
    Ok(node_id)
}

fn ensure_packable_path(path: &Path) -> Result<PackablePathKind> {
    packable_path_kind(path)?.ok_or_else(|| {
        LiosError::Unsupported(format!(
            "source path changed before packing: {}",
            path.display()
        ))
    })
}

fn pack_content_object_v2(
    catalog: &mut CatalogV2,
    path: &Path,
    relative_path: &Path,
    key: &KeyFile,
    options: &PackOptions,
    remote_objects: &[StorageObject],
    progress: &mut PackProgressTracker<'_>,
) -> Result<(String, SourceFileSnapshot)> {
    ensure_packable_path_kind(path, PackablePathKind::File)?;
    let source_snapshot = snapshot_regular_file(path, relative_path)?;
    let object_id = random_id();
    let object_dir = options.staging_dir.join(FILES_DIR).join(&object_id);
    let chunks_dir = object_dir.join(FILE_CHUNKS_DIR);
    fs::create_dir_all(&chunks_dir)?;

    let result = (|| {
        let source = fs::File::open(path).map_err(|error| map_source_io_error(error, path))?;
        let mut source = BufReader::new(source);
        let mut file_hasher = Sha256::new();
        let mut chunks = Vec::new();
        let mut total_size = 0u64;

        loop {
            let at_eof = source.fill_buf()?.is_empty();
            if at_eof && !chunks.is_empty() {
                break;
            }

            let chunk_id = ChunkIdV2::random();
            let chunk_id_hex = hex::encode(chunk_id.as_bytes());
            let relative_path =
                format!("{FILES_DIR}/{object_id}/{FILE_CHUNKS_DIR}/{chunk_id_hex}.lios");
            let chunk_path = options.staging_dir.join(&relative_path);
            let mut temp = SiblingTempFile::create(&chunk_path, ".lios-tmp")?;
            let stats = if at_eof {
                encode_chunk_stream_v2(key, chunk_id, std::io::empty(), temp.file_mut())?
            } else {
                let limited = source.by_ref().take(options.chunk_size as u64);
                let hashing = WholeFileHashingReader::new(limited, &mut file_hasher);
                encode_chunk_stream_v2(key, chunk_id, hashing, temp.file_mut())?
            };
            temp.persist_new(&chunk_path)?;
            total_size = total_size
                .checked_add(stats.original_bytes)
                .ok_or_else(|| LiosError::Unsupported("file is too large".to_string()))?;
            chunks.push(V2ChunkRef {
                index: chunks.len(),
                chunk_id: chunk_id_hex,
                path: relative_path,
                original_size: stats.original_bytes,
                original_sha256: hex::encode(stats.original_sha256),
                encoded_size: stats.encoded_bytes,
                encoded_sha256: hex::encode(stats.encoded_sha256),
                format_version: 2,
            });
            progress.complete_chunk(stats.original_bytes);
            if at_eof {
                break;
            }
        }

        verify_source_file_unchanged(&source_snapshot)?;
        let content_sha256 = hex::encode(file_hasher.finalize());
        let candidate_ids = content_object_candidates(catalog, &content_sha256);
        let mut unavailable_object_ids = Vec::new();
        for existing_object_id in candidate_ids {
            if existing_content_is_locally_available(
                catalog,
                &existing_object_id,
                key,
                &options.staging_dir,
            )? {
                catalog
                    .content_index
                    .insert(content_sha256.clone(), existing_object_id.clone());
                fs::remove_dir_all(&object_dir)?;
                return Ok((existing_object_id, source_snapshot));
            }
            let existing_object = &catalog.content_objects[&existing_object_id];
            if existing_content_is_remotely_available(existing_object, remote_objects)?
                && clear_staged_content_files(existing_object, &options.staging_dir)?
            {
                catalog
                    .content_index
                    .insert(content_sha256.clone(), existing_object_id.clone());
                fs::remove_dir_all(&object_dir)?;
                return Ok((existing_object_id, source_snapshot));
            }
            unavailable_object_ids.push(existing_object_id);
        }

        let manifest = ObjectManifestV2 {
            version: 2,
            format_version: 2,
            object_id: object_id.clone(),
            content_sha256: content_sha256.clone(),
            original_size: total_size,
            chunks: chunks.clone(),
        };
        let manifest_plaintext = serde_json::to_vec(&manifest)?;
        let encrypted_manifest =
            encrypt_envelope_v2(key, EnvelopeKindV2::Manifest, &manifest_plaintext)?;
        let manifest_path = format!("{FILES_DIR}/{object_id}/{FILE_MANIFEST}");
        write_atomic_immutable(
            &options.staging_dir.join(&manifest_path),
            &encrypted_manifest,
        )?;
        let content = ContentObject {
            object_id: object_id.clone(),
            content_sha256: content_sha256.clone(),
            original_size: total_size,
            storage: StorageRef::V2(V2StorageRef {
                manifest_path,
                manifest_encrypted_sha256: sha256_hex(&encrypted_manifest),
                chunks,
            }),
        };
        catalog
            .content_index
            .insert(content_sha256, object_id.clone());
        catalog.content_objects.insert(object_id.clone(), content);
        for unavailable_object_id in unavailable_object_ids {
            replace_content_object_references(catalog, &unavailable_object_id, &object_id);
            catalog.content_objects.remove(&unavailable_object_id);
        }
        Ok((object_id.clone(), source_snapshot))
    })();

    if result.is_err() {
        let _ = fs::remove_dir_all(&object_dir);
    }
    result
}

fn existing_content_is_locally_available(
    catalog: &CatalogV2,
    object_id: &str,
    key: &KeyFile,
    staging_dir: &Path,
) -> Result<bool> {
    let object = catalog.content_objects.get(object_id).ok_or_else(|| {
        LiosError::DataCorruption(format!("content index object is missing: {object_id}"))
    })?;
    match &object.storage {
        StorageRef::Legacy(storage) => {
            for chunk in &storage.chunks {
                let path = staging_dir.join(&chunk.path);
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(false);
                    }
                    Err(error) => return Err(error.into()),
                };
                if is_link_or_junction(&path, &metadata)?
                    || !metadata.is_file()
                    || sha256_file(&path)? != chunk.encrypted_sha256
                {
                    return Ok(false);
                }
            }
        }
        StorageRef::V2(storage) => {
            let expected_manifest = ObjectManifestV2 {
                version: 2,
                format_version: 2,
                object_id: object.object_id.clone(),
                content_sha256: object.content_sha256.clone(),
                original_size: object.original_size,
                chunks: storage.chunks.clone(),
            };
            let manifest_path = staging_dir.join(&storage.manifest_path);
            let metadata = match fs::symlink_metadata(&manifest_path) {
                Ok(metadata) => metadata,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
                Err(error) => return Err(error.into()),
            };
            if is_link_or_junction(&manifest_path, &metadata)? || !metadata.is_file() {
                return Ok(false);
            }
            let encrypted = fs::read(&manifest_path)?;
            if sha256_hex(&encrypted) != storage.manifest_encrypted_sha256 {
                return Ok(false);
            }
            let Ok(plaintext) =
                decrypt_compatible_v1_or_v2(key, EnvelopeKindV2::Manifest, &encrypted)
            else {
                return Ok(false);
            };
            let Ok(manifest) = serde_json::from_slice::<ObjectManifestV2>(&plaintext) else {
                return Ok(false);
            };
            if manifest != expected_manifest {
                return Ok(false);
            }
            for chunk in &storage.chunks {
                let path = staging_dir.join(&chunk.path);
                let metadata = match fs::symlink_metadata(&path) {
                    Ok(metadata) => metadata,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                        return Ok(false);
                    }
                    Err(error) => return Err(error.into()),
                };
                if is_link_or_junction(&path, &metadata)?
                    || !metadata.is_file()
                    || metadata.len() != chunk.encoded_size
                    || sha256_file(&path)? != chunk.encoded_sha256
                {
                    return Ok(false);
                }
            }
        }
    }
    Ok(true)
}

fn content_object_candidates(catalog: &CatalogV2, content_sha256: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    if let Some(indexed) = catalog.content_index.get(content_sha256) {
        candidates.push(indexed.clone());
    }
    for (object_id, object) in &catalog.content_objects {
        if object.content_sha256 == content_sha256 && !candidates.contains(object_id) {
            candidates.push(object_id.clone());
        }
    }
    candidates
}

fn existing_content_is_remotely_available(
    object: &ContentObject,
    remote_objects: &[StorageObject],
) -> Result<bool> {
    match &object.storage {
        StorageRef::Legacy(storage) => Ok(storage.chunks.iter().all(|chunk| {
            remote_object_hash_matches(remote_objects, &chunk.path, &chunk.encrypted_sha256)
        })),
        StorageRef::V2(storage) => {
            let expected_manifest = ObjectManifestV2 {
                version: 2,
                format_version: 2,
                object_id: object.object_id.clone(),
                content_sha256: object.content_sha256.clone(),
                original_size: object.original_size,
                chunks: storage.chunks.clone(),
            };
            let manifest_size =
                envelope_encoded_len_v2(serde_json::to_vec(&expected_manifest)?.len())?;
            if !remote_object_matches(
                remote_objects,
                &storage.manifest_path,
                manifest_size,
                &storage.manifest_encrypted_sha256,
            ) {
                return Ok(false);
            }
            Ok(storage.chunks.iter().all(|chunk| {
                remote_object_matches(
                    remote_objects,
                    &chunk.path,
                    chunk.encoded_size,
                    &chunk.encoded_sha256,
                )
            }))
        }
    }
}

fn remote_object_hash_matches(
    remote_objects: &[StorageObject],
    path: &str,
    expected_sha256: &str,
) -> bool {
    remote_objects
        .iter()
        .any(|object| object.path == path && object.sha256.as_deref() == Some(expected_sha256))
}

fn remote_object_matches(
    remote_objects: &[StorageObject],
    path: &str,
    expected_size: u64,
    expected_sha256: &str,
) -> bool {
    remote_objects.iter().any(|object| {
        object.path == path
            && object.size == expected_size
            && object.sha256.as_deref() == Some(expected_sha256)
    })
}

fn clear_staged_content_files(object: &ContentObject, staging_dir: &Path) -> Result<bool> {
    let paths = match &object.storage {
        StorageRef::Legacy(storage) => std::iter::once(storage.manifest_path.as_str())
            .chain(storage.chunks.iter().map(|chunk| chunk.path.as_str()))
            .collect::<Vec<_>>(),
        StorageRef::V2(storage) => std::iter::once(storage.manifest_path.as_str())
            .chain(storage.chunks.iter().map(|chunk| chunk.path.as_str()))
            .collect::<Vec<_>>(),
    };
    for relative_path in paths {
        let path = staging_dir.join(relative_path);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error.into()),
        };
        if is_link_or_junction(&path, &metadata)? || !metadata.is_file() {
            return Ok(false);
        }
        fs::remove_file(path)?;
    }
    Ok(true)
}

fn replace_content_object_references(
    catalog: &mut CatalogV2,
    unavailable_object_id: &str,
    replacement_object_id: &str,
) {
    for node in catalog.nodes.values_mut() {
        let NodeDescriptorKindV2::File { object_id, .. } = &mut node.descriptor.kind else {
            continue;
        };
        if object_id == unavailable_object_id {
            *object_id = replacement_object_id.to_string();
            node.descriptor_encrypted_sha256 = None;
        }
    }
}

struct WholeFileHashingReader<'a, R> {
    inner: R,
    hasher: &'a mut Sha256,
}

impl<'a, R> WholeFileHashingReader<'a, R> {
    fn new(inner: R, hasher: &'a mut Sha256) -> Self {
        Self { inner, hasher }
    }
}

impl<R: Read> Read for WholeFileHashingReader<'_, R> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let read = self.inner.read(buffer)?;
        self.hasher.update(&buffer[..read]);
        Ok(read)
    }
}

fn restore_node_v2(
    catalog: &CatalogV2,
    node_id: &str,
    parent: &Path,
    key: &KeyFile,
    staging_dir: &Path,
    options: &RestoreOptions,
) -> Result<()> {
    let node = catalog_node(catalog, node_id)?;
    let node_name = restore_local_name(&node.descriptor.name);
    let restore_root = &options.output_dir;
    match &node.descriptor.kind {
        NodeDescriptorKindV2::Directory => {
            let dir = parent.join(node_name);
            ensure_restore_descendants_safe(restore_root, &dir)?;
            fs::create_dir_all(&dir)?;
            ensure_restore_descendants_safe(restore_root, &dir)?;
            for child_id in child_ids(catalog, node_id) {
                restore_node_v2(catalog, child_id, &dir, key, staging_dir, options)?;
            }
        }
        NodeDescriptorKindV2::File {
            object_id,
            content_sha256,
            original_size,
        } => {
            let requested_path = parent.join(node_name);
            ensure_restore_descendants_safe(restore_root, &requested_path)?;
            let output_path = resolve_restore_path(&requested_path, &options.conflict_policy);
            ensure_restore_descendants_safe(restore_root, &output_path)?;
            if let Some(output_parent) = output_path.parent() {
                ensure_restore_descendants_safe(restore_root, output_parent)?;
                fs::create_dir_all(output_parent)?;
                ensure_restore_descendants_safe(restore_root, output_parent)?;
            }
            let object = catalog.content_objects.get(object_id).ok_or_else(|| {
                LiosError::DataCorruption(format!("missing content object: {object_id}"))
            })?;
            if object.content_sha256 != *content_sha256 || object.original_size != *original_size {
                return Err(LiosError::DataCorruption(format!(
                    "file node content metadata mismatch: {node_id}"
                )));
            }
            let mut output = SiblingTempFile::create(&output_path, ".lios-part")?;
            let mut file_hasher = Sha256::new();
            let mut restored_size = 0u64;
            match &object.storage {
                StorageRef::Legacy(storage) => {
                    let mut chunks = storage.chunks.iter().collect::<Vec<_>>();
                    chunks.sort_by_key(|chunk| chunk.index);
                    for chunk in chunks {
                        let encrypted = fs::read(staging_dir.join(&chunk.path))?;
                        if sha256_hex(&encrypted) != chunk.encrypted_sha256 {
                            return Err(LiosError::Crypto);
                        }
                        let compressed = key.decrypt(&encrypted)?;
                        restore_legacy_chunk(
                            compressed.as_slice(),
                            chunk,
                            output.file_mut(),
                            &mut file_hasher,
                            &mut restored_size,
                            *original_size,
                        )?;
                    }
                }
                StorageRef::V2(storage) => {
                    validate_v2_manifest(object, storage, key, staging_dir)?;
                    let mut chunks = storage.chunks.iter().collect::<Vec<_>>();
                    chunks.sort_by_key(|chunk| chunk.index);
                    for chunk in chunks {
                        if chunk.format_version != 2 {
                            return Err(LiosError::Unsupported(format!(
                                "unknown chunk format version: {}",
                                chunk.format_version
                            )));
                        }
                        let chunk_path = staging_dir.join(&chunk.path);
                        if fs::metadata(&chunk_path)?.len() != chunk.encoded_size
                            || sha256_file(&chunk_path)? != chunk.encoded_sha256
                        {
                            return Err(LiosError::Crypto);
                        }
                        let chunk_id = parse_chunk_id_v2(&chunk.chunk_id)?;
                        let input = fs::File::open(&chunk_path)?;
                        let writer =
                            WholeFileHashingWriter::new(output.file_mut(), &mut file_hasher);
                        let stats = decode_chunk_stream_v2(
                            key,
                            chunk_id,
                            input,
                            writer,
                            &ChunkDecodeLimitsV2::for_chunk(chunk.original_size),
                        )?;
                        if stats.original_bytes != chunk.original_size
                            || hex::encode(stats.original_sha256) != chunk.original_sha256
                            || stats.encoded_bytes != chunk.encoded_size
                            || hex::encode(stats.encoded_sha256) != chunk.encoded_sha256
                        {
                            return Err(LiosError::Crypto);
                        }
                        restored_size = restored_size
                            .checked_add(stats.original_bytes)
                            .filter(|size| *size <= *original_size)
                            .ok_or_else(|| {
                                LiosError::DataCorruption(
                                    "restored file exceeds declared size".to_string(),
                                )
                            })?;
                    }
                }
            }
            if restored_size != *original_size
                || hex::encode(file_hasher.finalize()) != *content_sha256
            {
                return Err(LiosError::Crypto);
            }
            ensure_restore_descendants_safe(restore_root, &output_path)?;
            output.persist_new(&output_path)?;
        }
    }
    Ok(())
}

fn restore_legacy_chunk(
    compressed: &[u8],
    chunk: &LegacyChunkRef,
    output: &mut impl Write,
    file_hasher: &mut Sha256,
    restored_size: &mut u64,
    expected_file_size: u64,
) -> Result<()> {
    let mut decoder = zstd::stream::read::Decoder::new(compressed)?;
    let mut chunk_hasher = Sha256::new();
    let mut chunk_size = 0u64;
    let mut buffer = [0u8; LEGACY_RESTORE_BUFFER_SIZE];
    loop {
        let read = decoder.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let bytes = &buffer[..read];
        let read = u64::try_from(read).map_err(|_| {
            LiosError::DataCorruption("legacy chunk size cannot be represented".to_string())
        })?;
        let next_chunk_size = chunk_size
            .checked_add(read)
            .ok_or_else(|| LiosError::DataCorruption("legacy chunk size overflow".to_string()))?;
        let next_file_size = restored_size.checked_add(read).ok_or_else(|| {
            LiosError::DataCorruption("legacy restored size overflow".to_string())
        })?;
        if next_chunk_size > chunk.original_size || next_file_size > expected_file_size {
            return Err(LiosError::DataCorruption(
                "legacy chunk exceeds declared size".to_string(),
            ));
        }
        chunk_hasher.update(bytes);
        file_hasher.update(bytes);
        output.write_all(bytes)?;
        chunk_size = next_chunk_size;
        *restored_size = next_file_size;
    }
    if chunk_size != chunk.original_size
        || hex::encode(chunk_hasher.finalize()) != chunk.original_sha256
    {
        return Err(LiosError::Crypto);
    }
    Ok(())
}

fn read_verified_encrypted_file(
    staging_dir: &Path,
    relative_path: &str,
    expected_sha256: Option<&str>,
    expected_size: Option<u64>,
) -> Result<Vec<u8>> {
    let (mut file, opened_size) =
        open_verified_staging_file(staging_dir, relative_path, expected_size)?;
    let mut encrypted = Vec::new();
    file.read_to_end(&mut encrypted)?;
    if u64::try_from(encrypted.len()).ok() != Some(opened_size) {
        return Err(LiosError::Crypto);
    }
    if expected_sha256.is_some_and(|expected| sha256_hex(&encrypted) != expected) {
        return Err(LiosError::Crypto);
    }
    Ok(encrypted)
}

fn open_verified_staging_file(
    staging_dir: &Path,
    relative_path: &str,
    expected_size: Option<u64>,
) -> Result<(fs::File, u64)> {
    validate_remote_object_path(relative_path)?;
    let path = staging_dir.join(relative_path);
    #[cfg(windows)]
    let opened_staging_path = opened_windows_staging_path(staging_dir)?;
    ensure_staging_descendants_safe(staging_dir, &path)?;
    let file = open_staging_file(&path)?;
    let metadata = file.metadata()?;
    if opened_file_is_link_or_junction(&metadata) || !metadata.is_file() {
        return Err(LiosError::DataCorruption(format!(
            "encrypted object is not a regular file: {}",
            path.display()
        )));
    }
    #[cfg(windows)]
    ensure_windows_opened_path_matches(&file, &opened_staging_path, relative_path)?;
    if expected_size.is_some_and(|size| metadata.len() != size) {
        return Err(LiosError::Crypto);
    }
    Ok((file, metadata.len()))
}

#[cfg(windows)]
fn open_staging_file(path: &Path) -> Result<fs::File> {
    use std::os::windows::fs::OpenOptionsExt;

    use windows::Win32::Storage::FileSystem::FILE_FLAG_OPEN_REPARSE_POINT;

    Ok(fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(path)?)
}

#[cfg(not(windows))]
fn open_staging_file(path: &Path) -> Result<fs::File> {
    Ok(fs::File::open(path)?)
}

#[cfg(windows)]
fn opened_file_is_link_or_junction(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn opened_file_is_link_or_junction(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

#[cfg(windows)]
fn opened_windows_staging_path(staging_dir: &Path) -> Result<String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};

    use windows::Win32::Storage::FileSystem::{
        FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };

    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    let directory = fs::OpenOptions::new()
        .read(true)
        .custom_flags(FILE_FLAG_BACKUP_SEMANTICS.0 | FILE_FLAG_OPEN_REPARSE_POINT.0)
        .open(staging_dir)?;
    let metadata = directory.metadata()?;
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 || !metadata.is_dir() {
        return Err(LiosError::DataCorruption(format!(
            "staging root is not a regular directory: {}",
            staging_dir.display()
        )));
    }
    windows_final_path_key(&directory)
}

#[cfg(windows)]
fn ensure_windows_opened_path_matches(
    file: &fs::File,
    opened_staging_path: &str,
    relative_path: &str,
) -> Result<()> {
    let opened_file_path = windows_final_path_key(file)?;
    let expected = format!(
        "{}\\{}",
        opened_staging_path.trim_end_matches('\\'),
        relative_path.replace('/', "\\")
    )
    .to_lowercase();
    if opened_file_path != expected {
        return Err(LiosError::DataCorruption(format!(
            "staging path resolved through a symlink or junction: {relative_path}"
        )));
    }
    Ok(())
}

#[cfg(windows)]
fn windows_final_path_key(file: &fs::File) -> Result<String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;

    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED};

    let handle = HANDLE(file.as_raw_handle());
    let mut buffer = vec![0u16; 512];
    loop {
        let length =
            unsafe { GetFinalPathNameByHandleW(handle, &mut buffer, FILE_NAME_NORMALIZED) };
        if length == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let length = usize::try_from(length)
            .map_err(|_| LiosError::DataCorruption("opened path length overflowed".to_string()))?;
        if length < buffer.len() {
            return Ok(OsString::from_wide(&buffer[..length])
                .to_string_lossy()
                .replace('/', "\\")
                .trim_end_matches('\\')
                .to_lowercase());
        }
        buffer.resize(length.saturating_add(1), 0);
    }
}

fn ensure_staging_descendants_safe(staging_dir: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(staging_dir)
        .map_err(|_| LiosError::InvalidRelativePath(path.to_path_buf()))?;
    let root_metadata = fs::symlink_metadata(staging_dir)?;
    if is_link_or_junction(staging_dir, &root_metadata)? || !root_metadata.is_dir() {
        return Err(LiosError::DataCorruption(format!(
            "staging root is not a regular directory: {}",
            staging_dir.display()
        )));
    }
    let mut current = staging_dir.to_path_buf();
    for component in relative.components() {
        let std::path::Component::Normal(part) = component else {
            return Err(LiosError::InvalidRelativePath(path.to_path_buf()));
        };
        current.push(part);
        let metadata = fs::symlink_metadata(&current)?;
        if is_link_or_junction(&current, &metadata)? {
            return Err(LiosError::DataCorruption(format!(
                "staging path contains symlink or junction: {}",
                current.display()
            )));
        }
    }
    Ok(())
}

fn add_verified_encoded_bytes(
    report: &mut CatalogIntegrityReport,
    encoded_len: usize,
) -> Result<()> {
    let encoded_len = u64::try_from(encoded_len)
        .map_err(|_| LiosError::DataCorruption("encoded object is too large".to_string()))?;
    report.encoded_bytes_verified = report
        .encoded_bytes_verified
        .checked_add(encoded_len)
        .ok_or_else(|| {
            LiosError::DataCorruption("verified encoded byte count overflowed".to_string())
        })?;
    Ok(())
}

fn verify_content_object_integrity(
    object: &ContentObject,
    key: &KeyFile,
    staging_dir: &Path,
    report: &mut CatalogIntegrityReport,
) -> Result<()> {
    let mut file_hasher = Sha256::new();
    let mut restored_size = 0u64;
    match &object.storage {
        StorageRef::Legacy(storage) => {
            let encrypted_manifest = read_verified_encrypted_file(
                staging_dir,
                &storage.manifest_path,
                storage.manifest_encrypted_sha256.as_deref(),
                None,
            )?;
            let manifest_plaintext =
                decrypt_compatible_v1_or_v2(key, EnvelopeKindV2::Manifest, &encrypted_manifest)?;
            let manifest: ObjectManifestV1 = serde_json::from_slice(&manifest_plaintext)?;
            let expected_manifest = ObjectManifestV1 {
                version: 1,
                object_id: object.object_id.clone(),
                chunks: storage.chunks.clone(),
            };
            if manifest != expected_manifest {
                return Err(LiosError::DataCorruption(format!(
                    "legacy content manifest mismatch: {}",
                    object.object_id
                )));
            }
            add_verified_encoded_bytes(report, encrypted_manifest.len())?;
            let mut chunks = storage.chunks.iter().collect::<Vec<_>>();
            chunks.sort_by_key(|chunk| chunk.index);
            for chunk in chunks {
                let encrypted = read_verified_encrypted_file(
                    staging_dir,
                    &chunk.path,
                    Some(&chunk.encrypted_sha256),
                    None,
                )?;
                let compressed = key.decrypt(&encrypted)?;
                let mut sink = std::io::sink();
                restore_legacy_chunk(
                    compressed.as_slice(),
                    chunk,
                    &mut sink,
                    &mut file_hasher,
                    &mut restored_size,
                    object.original_size,
                )?;
                report.chunks_verified =
                    report.chunks_verified.checked_add(1).ok_or_else(|| {
                        LiosError::DataCorruption("verified chunk count overflowed".to_string())
                    })?;
                add_verified_encoded_bytes(report, encrypted.len())?;
            }
        }
        StorageRef::V2(storage) => {
            let expected_manifest = expected_v2_manifest(object, storage);
            let expected_manifest_size =
                envelope_encoded_len_v2(serde_json::to_vec(&expected_manifest)?.len())?;
            let encrypted_manifest = read_verified_encrypted_file(
                staging_dir,
                &storage.manifest_path,
                Some(&storage.manifest_encrypted_sha256),
                Some(expected_manifest_size),
            )?;
            validate_v2_manifest_bytes(
                object,
                storage,
                key,
                &encrypted_manifest,
                &expected_manifest,
            )?;
            add_verified_encoded_bytes(report, encrypted_manifest.len())?;
            let mut chunks = storage.chunks.iter().collect::<Vec<_>>();
            chunks.sort_by_key(|chunk| chunk.index);
            for chunk in chunks {
                if chunk.format_version != 2 {
                    return Err(LiosError::Unsupported(format!(
                        "unknown chunk format version: {}",
                        chunk.format_version
                    )));
                }
                let (input, _) =
                    open_verified_staging_file(staging_dir, &chunk.path, Some(chunk.encoded_size))?;
                let chunk_id = parse_chunk_id_v2(&chunk.chunk_id)?;
                let writer = WholeFileHashingWriter::new(std::io::sink(), &mut file_hasher);
                let stats = decode_chunk_stream_v2(
                    key,
                    chunk_id,
                    input,
                    writer,
                    &ChunkDecodeLimitsV2::for_chunk(chunk.original_size),
                )?;
                if stats.original_bytes != chunk.original_size
                    || hex::encode(stats.original_sha256) != chunk.original_sha256
                    || stats.encoded_bytes != chunk.encoded_size
                    || hex::encode(stats.encoded_sha256) != chunk.encoded_sha256
                {
                    return Err(LiosError::Crypto);
                }
                restored_size = restored_size
                    .checked_add(stats.original_bytes)
                    .filter(|size| *size <= object.original_size)
                    .ok_or_else(|| {
                        LiosError::DataCorruption("verified file exceeds declared size".to_string())
                    })?;
                report.chunks_verified =
                    report.chunks_verified.checked_add(1).ok_or_else(|| {
                        LiosError::DataCorruption("verified chunk count overflowed".to_string())
                    })?;
                report.encoded_bytes_verified = report
                    .encoded_bytes_verified
                    .checked_add(stats.encoded_bytes)
                    .ok_or_else(|| {
                        LiosError::DataCorruption(
                            "verified encoded byte count overflowed".to_string(),
                        )
                    })?;
            }
        }
    }
    if restored_size != object.original_size
        || hex::encode(file_hasher.finalize()) != object.content_sha256
    {
        return Err(LiosError::Crypto);
    }
    Ok(())
}

fn validate_v2_manifest(
    object: &ContentObject,
    storage: &V2StorageRef,
    key: &KeyFile,
    staging_dir: &Path,
) -> Result<()> {
    let expected = expected_v2_manifest(object, storage);
    let encrypted = read_verified_encrypted_file(
        staging_dir,
        &storage.manifest_path,
        Some(&storage.manifest_encrypted_sha256),
        None,
    )?;
    validate_v2_manifest_bytes(object, storage, key, &encrypted, &expected)
}

fn validate_v2_manifest_bytes(
    object: &ContentObject,
    storage: &V2StorageRef,
    key: &KeyFile,
    encrypted: &[u8],
    expected: &ObjectManifestV2,
) -> Result<()> {
    let plaintext = decrypt_compatible_v1_or_v2(key, EnvelopeKindV2::Manifest, encrypted)?;
    let manifest: ObjectManifestV2 = serde_json::from_slice(&plaintext)?;
    if manifest.version != 2 {
        return Err(LiosError::Unsupported(format!(
            "unknown manifest version: {}",
            manifest.version
        )));
    }
    if manifest.format_version != 2 {
        return Err(LiosError::Unsupported(format!(
            "unknown manifest format version: {}",
            manifest.format_version
        )));
    }
    if manifest != *expected {
        return Err(LiosError::DataCorruption(format!(
            "content manifest mismatch: {}",
            object.object_id
        )));
    }
    if storage.manifest_encrypted_sha256 != sha256_hex(encrypted) {
        return Err(LiosError::Crypto);
    }
    Ok(())
}

fn expected_v2_manifest(object: &ContentObject, storage: &V2StorageRef) -> ObjectManifestV2 {
    ObjectManifestV2 {
        version: 2,
        format_version: 2,
        object_id: object.object_id.clone(),
        content_sha256: object.content_sha256.clone(),
        original_size: object.original_size,
        chunks: storage.chunks.clone(),
    }
}

fn parse_chunk_id_v2(value: &str) -> Result<ChunkIdV2> {
    let bytes =
        hex::decode(value).map_err(|_| LiosError::InvalidV2Format("invalid chunk id encoding"))?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| LiosError::InvalidV2Format("invalid chunk id length"))?;
    Ok(ChunkIdV2::from_bytes(bytes))
}

struct WholeFileHashingWriter<'a, W> {
    inner: W,
    hasher: &'a mut Sha256,
}

impl<'a, W> WholeFileHashingWriter<'a, W> {
    fn new(inner: W, hasher: &'a mut Sha256) -> Self {
        Self { inner, hasher }
    }
}

impl<W: Write> Write for WholeFileHashingWriter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        let written = self.inner.write(bytes)?;
        self.hasher.update(&bytes[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn ensure_restore_descendants_safe(root: &Path, path: &Path) -> Result<()> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| LiosError::InvalidRelativePath(path.to_path_buf()))?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        match component {
            std::path::Component::CurDir => continue,
            std::path::Component::Normal(part) => current.push(part),
            _ => return Err(LiosError::InvalidRelativePath(path.to_path_buf())),
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) if is_link_or_junction(&current, &metadata)? => {
                return Err(LiosError::Unsupported(format!(
                    "restore path contains symlink or junction: {}",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn sort_name_key(name: &str) -> (String, String, u32) {
    let path = Path::new(name);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .map(windows_name_key)
        .unwrap_or_default();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(windows_name_key)
        .unwrap_or_else(|| windows_name_key(name));
    if let Some((base, suffix)) = stem.rsplit_once(" (") {
        if let Some(number) = suffix
            .strip_suffix(')')
            .and_then(|number| number.parse::<u32>().ok())
        {
            return (base.to_string(), extension, number);
        }
    }
    (stem, extension, 0)
}

fn windows_name_key(name: &str) -> String {
    name.to_lowercase()
}

fn windows_names_equal(left: &str, right: &str) -> bool {
    windows_name_key(left) == windows_name_key(right)
}

fn normalize_name(name: &str) -> Result<String> {
    if !is_portable_logical_name(name) {
        return Err(LiosError::Unsupported("invalid item name".to_string()));
    }
    Ok(name.to_string())
}

fn is_portable_logical_name(name: &str) -> bool {
    !(name.is_empty()
        || name.trim().is_empty()
        || name == "."
        || name == ".."
        || name
            .chars()
            .any(|character| character <= '\u{1f}' || "/\\:*?\"<>|".contains(character))
        || name.ends_with(' ')
        || name.ends_with('.')
        || is_windows_reserved_name(name))
}

fn restore_local_name(name: &str) -> String {
    if is_portable_logical_name(name) {
        return name.to_string();
    }
    let mut base = name
        .chars()
        .map(|character| {
            if character <= '\u{1f}' || "/\\:*?\"<>|".contains(character) {
                '_'
            } else {
                character
            }
        })
        .collect::<String>()
        .trim_end_matches([' ', '.'])
        .to_string();
    if base.trim().is_empty() || base == "." || base == ".." {
        base = "item".to_string();
    }
    if is_windows_reserved_name(&base) {
        base.insert(0, '_');
    }
    let digest = sha256_hex(name.as_bytes());
    format!("{base} (legacy {})", &digest[..8])
}

fn is_windows_reserved_name(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or(name)
        .trim_end_matches([' ', '.'])
        .to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || stem
            .strip_prefix("COM")
            .or_else(|| stem.strip_prefix("LPT"))
            .is_some_and(|number| {
                matches!(number, "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9")
            })
}

fn available_name(existing: &[&str], name: &str) -> String {
    let existing = existing
        .iter()
        .map(|name| windows_name_key(name))
        .collect::<HashSet<_>>();
    if !existing.contains(&windows_name_key(name)) {
        return name.to_string();
    }
    let path = Path::new(name);
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name);
    let extension = path.extension().and_then(|extension| extension.to_str());
    for index in 1.. {
        let candidate = match extension {
            Some(extension) => format!("{stem} ({index}).{extension}"),
            None => format!("{stem} ({index})"),
        };
        if !existing.contains(&windows_name_key(&candidate)) {
            return candidate;
        }
    }
    unreachable!()
}

fn resolve_restore_path(path: &Path, conflict_policy: &RestoreConflictPolicy) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    match conflict_policy {
        RestoreConflictPolicy::Rename => {
            let parent = path.parent().unwrap_or_else(|| Path::new(""));
            let stem = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("restored");
            let extension = path.extension().and_then(|e| e.to_str());
            for index in 1.. {
                let file_name = match extension {
                    Some(extension) => format!("{stem} (restored {index}).{extension}"),
                    None => format!("{stem} (restored {index})"),
                };
                let candidate = parent.join(file_name);
                if !candidate.exists() {
                    return candidate;
                }
            }
            unreachable!()
        }
    }
}

fn file_name(path: &Path) -> Result<String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| LiosError::MissingFileName(path.to_path_buf()))?;
    normalize_name(&name)
}

fn snapshot_source_path(
    path: &Path,
    relative_path: &Path,
    report: &mut SourceSnapshotReport,
) -> Result<()> {
    let Some(kind) = packable_path_kind(path)? else {
        report.skipped_paths.push(skipped_link(path));
        return Ok(());
    };
    match kind {
        PackablePathKind::File => {
            report
                .files
                .push(snapshot_regular_file(path, relative_path)?);
        }
        PackablePathKind::Directory => {
            report.directories.push(SourceDirectorySnapshot {
                source_path: path.to_path_buf(),
                relative_path: relative_path.to_path_buf(),
            });
            let entries = source_directory_entries(path)?;
            for entry in entries {
                let child_path = entry.path();
                let child_relative = relative_path.join(file_name(&child_path)?);
                snapshot_source_path(&child_path, &child_relative, report)?;
            }
        }
    }
    Ok(())
}

fn snapshot_regular_file(path: &Path, relative_path: &Path) -> Result<SourceFileSnapshot> {
    let metadata = source_metadata(path)?;
    let modified = metadata
        .modified()?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| {
            LiosError::Unsupported(format!(
                "source file modification time predates the Unix epoch: {}",
                path.display()
            ))
        })?;
    let modified_at_ns = i64::try_from(modified.as_nanos()).map_err(|_| {
        LiosError::Unsupported(format!(
            "source file modification time is out of range: {}",
            path.display()
        ))
    })?;
    Ok(SourceFileSnapshot {
        source_path: path.to_path_buf(),
        relative_path: relative_path.to_path_buf(),
        size: metadata.len(),
        modified_at_ns: Some(modified_at_ns),
    })
}

fn source_metadata(path: &Path) -> Result<fs::Metadata> {
    fs::symlink_metadata(path).map_err(|error| map_source_io_error(error, path))
}

fn source_directory_entries(path: &Path) -> Result<Vec<fs::DirEntry>> {
    let entries = fs::read_dir(path).map_err(|error| map_source_io_error(error, path))?;
    let mut collected = Vec::new();
    for entry in entries {
        collected.push(entry.map_err(|error| map_source_io_error(error, path))?);
    }
    collected.sort_by_key(|entry| entry.path());
    Ok(collected)
}

fn map_source_io_error(error: std::io::Error, path: &Path) -> LiosError {
    if error.kind() == std::io::ErrorKind::NotFound {
        return LiosError::Unsupported(format!("source path no longer exists: {}", path.display()));
    }
    error.into()
}

fn packable_path_kind(path: &Path) -> Result<Option<PackablePathKind>> {
    let metadata = source_metadata(path)?;
    if is_link_or_junction(path, &metadata)? {
        return Ok(None);
    }
    if metadata.is_dir() {
        Ok(Some(PackablePathKind::Directory))
    } else if metadata.is_file() {
        Ok(Some(PackablePathKind::File))
    } else {
        Err(LiosError::Unsupported(format!(
            "source path is not a file or directory: {}",
            path.display()
        )))
    }
}

fn ensure_packable_path_kind(path: &Path, expected: PackablePathKind) -> Result<()> {
    match packable_path_kind(path)? {
        Some(actual) if actual == expected => Ok(()),
        _ => Err(LiosError::Unsupported(format!(
            "source path changed before packing: {}",
            path.display()
        ))),
    }
}

fn should_skip_link(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => is_link_or_junction(path, &metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn is_link_or_junction(path: &Path, metadata: &fs::Metadata) -> Result<bool> {
    use std::os::windows::fs::MetadataExt;

    classify_windows_reparse_tag(metadata.file_attributes(), || {
        query_windows_reparse_tag(path)
    })
    .map_err(Into::into)
}

#[cfg(not(windows))]
fn is_link_or_junction(_path: &Path, metadata: &fs::Metadata) -> Result<bool> {
    Ok(metadata.file_type().is_symlink())
}

#[cfg(windows)]
fn classify_windows_reparse_tag(
    file_attributes: u32,
    query_tag: impl FnOnce() -> std::io::Result<u32>,
) -> std::io::Result<bool> {
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    const IO_REPARSE_TAG_MOUNT_POINT: u32 = 0xA0000003;
    const IO_REPARSE_TAG_SYMLINK: u32 = 0xA000000C;

    if file_attributes & FILE_ATTRIBUTE_REPARSE_POINT == 0 {
        return Ok(false);
    }
    let tag = query_tag()?;
    Ok(matches!(
        tag,
        IO_REPARSE_TAG_MOUNT_POINT | IO_REPARSE_TAG_SYMLINK
    ))
}

#[cfg(windows)]
fn query_windows_reparse_tag(path: &Path) -> std::io::Result<u32> {
    use std::os::windows::ffi::OsStrExt;

    use windows::core::PCWSTR;
    use windows::Win32::Storage::FileSystem::{FindClose, FindFirstFileW, WIN32_FIND_DATAW};

    let path = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut data = WIN32_FIND_DATAW::default();
    let handle = unsafe { FindFirstFileW(PCWSTR(path.as_ptr()), &mut data) }
        .map_err(|_| std::io::Error::last_os_error())?;
    unsafe { FindClose(handle) }.map_err(|_| std::io::Error::last_os_error())?;
    Ok(data.dwReserved0)
}

fn skipped_link(path: &Path) -> SkippedPath {
    SkippedPath {
        path: path.to_path_buf(),
        reason: SkippedPathReason::SymbolicLinkOrJunction,
    }
}

fn random_id() -> String {
    Uuid::new_v4().simple().to_string()
}

fn timestamp() -> String {
    Utc::now().to_rfc3339()
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    #[test]
    fn windows_reparse_tags_distinguish_links_from_other_tags() {
        use std::io;

        const REPARSE_ATTRIBUTE: u32 = 0x400;
        const SYMLINK_TAG: u32 = 0xA000000C;
        const MOUNT_POINT_TAG: u32 = 0xA0000003;
        const CLOUD_TAG: u32 = 0x9000001A;

        assert!(
            super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || Ok(SYMLINK_TAG)).unwrap()
        );
        assert!(
            super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || Ok(MOUNT_POINT_TAG)).unwrap()
        );
        assert!(!super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || Ok(CLOUD_TAG)).unwrap());
        let error = super::classify_windows_reparse_tag(REPARSE_ATTRIBUTE, || {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected tag query failure",
            ))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }

    #[test]
    fn source_kind_revalidation_rejects_path_changed_into_link() {
        use std::fs;

        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let outside = tmp.path().join("outside");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&outside).unwrap();
        let expected = super::packable_path_kind(&source).unwrap().unwrap();
        fs::remove_dir(&source).unwrap();
        create_directory_link(&outside, &source);

        let error = super::ensure_packable_path_kind(&source, expected).unwrap_err();
        assert!(error.to_string().contains("source path changed"));
    }

    #[cfg(unix)]
    fn create_directory_link(target: &std::path::Path, link: &std::path::Path) {
        std::os::unix::fs::symlink(target, link).unwrap();
    }

    #[cfg(windows)]
    fn create_directory_link(target: &std::path::Path, link: &std::path::Path) {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "failed to create junction: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
