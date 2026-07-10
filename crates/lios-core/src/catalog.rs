use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::atomic::{publish_staged_new, write_atomic, write_atomic_immutable, SiblingTempFile};
use crate::crypto::KeyFile;
use crate::pack::{PackOptions, PackProgress, PackSource};
use crate::restore::{RestoreConflictPolicy, RestoreOptions};
use crate::{LiosError, Result};

pub const CATALOG_FILE: &str = "catalog.enc";
const OBJECTS_DIR: &str = "objects";
const FILES_DIR: &str = "objects/files";
const FILE_CHUNKS_DIR: &str = "chunks";
const FILE_MANIFEST: &str = "manifest.enc";
const TMP_DIR: &str = ".tmp";

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
}

impl PackReport {
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

#[derive(Debug, Serialize, Deserialize)]
struct PlainCatalog {
    version: u8,
    root: CatalogNode,
}

#[derive(Debug, Serialize, Deserialize)]
struct CatalogNode {
    id: String,
    name: String,
    #[serde(default)]
    updated_at: String,
    kind: CatalogNodeKind,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
enum CatalogNodeKind {
    Directory {
        children: Vec<CatalogNode>,
    },
    File {
        original_size: u64,
        sha256: String,
        object_id: String,
        chunks: Vec<ChunkRecord>,
    },
}

#[derive(Debug, Serialize, Deserialize)]
struct ChunkRecord {
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

        fs::create_dir_all(options.staging_dir.join(OBJECTS_DIR))?;
        let mut tracker = PackProgressTracker::new(on_progress);
        tracker.add_total(pack_stats(&source_path, options.chunk_size)?);
        let root = match source_kind {
            Some(PackablePathKind::Directory) => pack_directory(
                &source_path,
                &source_path,
                name,
                key,
                &options,
                &mut tracker,
                &mut report,
            )?,
            Some(PackablePathKind::File) => {
                pack_file(&source_path, name, key, &options, &mut tracker)?
            }
            None => unreachable!(),
        };

        let plain = PlainCatalog { version: 1, root };
        let serialized = serde_json::to_vec(&plain)?;
        let encrypted = key.encrypt(&serialized)?;
        let encrypted_catalog_path = options.staging_dir.join(CATALOG_FILE);
        write_atomic(&encrypted_catalog_path, &encrypted)?;

        Ok(PackOutcome::Packed {
            catalog: Self {
                encrypted_catalog_path,
                staging_dir: options.staging_dir,
            },
            report,
        })
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
        let plain = PlainCatalog {
            version: 1,
            root: CatalogNode {
                id: random_id(),
                name,
                updated_at: timestamp(),
                kind: CatalogNodeKind::Directory {
                    children: Vec::new(),
                },
            },
        };
        catalog.save_plain(&plain, key)?;
        Ok(catalog)
    }

    pub fn encrypted_catalog_path(&self) -> &Path {
        &self.encrypted_catalog_path
    }

    pub fn decrypt_tree(&self, key: &KeyFile) -> Result<CatalogTreeNode> {
        let catalog = self.load_plain(key)?;
        Ok(tree_node(&catalog.root))
    }

    pub fn list_children(&self, parent_id: &str, key: &KeyFile) -> Result<Vec<DriveItem>> {
        let catalog = self.load_plain(key)?;
        let parent = find_node(&catalog.root, parent_id).ok_or_else(|| {
            LiosError::Unsupported(format!("catalog node not found: {parent_id}"))
        })?;
        match &parent.kind {
            CatalogNodeKind::Directory { children } => {
                Ok(children.iter().map(drive_item).collect())
            }
            CatalogNodeKind::File { .. } => Err(LiosError::Unsupported(
                "cannot list children for a file".to_string(),
            )),
        }
    }

    pub fn search(&self, query: &str, key: &KeyFile) -> Result<Vec<DriveItem>> {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return Ok(Vec::new());
        }
        let catalog = self.load_plain(key)?;
        let mut matches = Vec::new();
        collect_search_matches(&catalog.root, &query, &mut matches);
        Ok(matches)
    }

    pub fn create_folder(&self, parent_id: &str, name: &str, key: &KeyFile) -> Result<()> {
        let name = normalize_name(name)?;
        let mut catalog = self.load_plain(key)?;
        let parent = find_directory_mut(&mut catalog.root, parent_id)?;
        let CatalogNodeKind::Directory { children } = &mut parent.kind else {
            unreachable!();
        };
        if children.iter().any(|child| child.name == name) {
            return Err(LiosError::Unsupported(format!(
                "folder already contains {name}"
            )));
        }
        children.push(CatalogNode {
            id: random_id(),
            name,
            updated_at: timestamp(),
            kind: CatalogNodeKind::Directory {
                children: Vec::new(),
            },
        });
        parent.updated_at = timestamp();
        sort_children(children);
        self.save_plain(&catalog, key)
    }

    pub fn rename_node(&self, node_id: &str, new_name: &str, key: &KeyFile) -> Result<()> {
        let new_name = normalize_name(new_name)?;
        let mut catalog = self.load_plain(key)?;
        if catalog.root.id == node_id {
            catalog.root.name = new_name;
            catalog.root.updated_at = timestamp();
            return self.save_plain(&catalog, key);
        }
        if !rename_child(&mut catalog.root, node_id, &new_name)? {
            return Err(LiosError::Unsupported(format!(
                "catalog node not found: {node_id}"
            )));
        }
        self.save_plain(&catalog, key)
    }

    pub fn delete_nodes(&self, node_ids: &[String], key: &KeyFile) -> Result<()> {
        let mut catalog = self.load_plain(key)?;
        let ids = node_ids
            .iter()
            .filter(|id| id.as_str() != catalog.root.id)
            .cloned()
            .collect::<HashSet<_>>();
        delete_children(&mut catalog.root, &ids);
        self.save_plain(&catalog, key)
    }

    pub fn preview_upload_conflicts(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        key: &KeyFile,
    ) -> Result<Vec<UploadConflict>> {
        let catalog = self.load_plain(key)?;
        let parent = find_node(&catalog.root, parent_id).ok_or_else(|| {
            LiosError::Unsupported(format!("catalog node not found: {parent_id}"))
        })?;
        let CatalogNodeKind::Directory { children } = &parent.kind else {
            return Err(LiosError::Unsupported(
                "cannot upload into a file".to_string(),
            ));
        };
        let mut conflicts = Vec::new();
        for path in paths {
            let target_name = file_name(path)?;
            if should_skip_link(path)? {
                continue;
            }
            if let Some(existing) = children.iter().find(|child| child.name == target_name) {
                conflicts.push(UploadConflict {
                    source_path: path.display().to_string(),
                    target_name,
                    existing_node_id: existing.id.clone(),
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
            None,
        )
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
            Some(&mut on_progress),
        )
    }

    fn add_paths_to_folder_with_optional_progress(
        &self,
        parent_id: &str,
        paths: &[PathBuf],
        resolutions: &[ConflictResolution],
        key: &KeyFile,
        options: PackOptions,
        on_progress: Option<&mut dyn FnMut(PackProgress)>,
    ) -> Result<PackReport> {
        if options.chunk_size == 0 {
            return Err(LiosError::Unsupported(
                "chunk size must be greater than zero".to_string(),
            ));
        }
        fs::create_dir_all(options.staging_dir.join(OBJECTS_DIR))?;
        let mut catalog = self.load_plain(key)?;
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
            let parent = find_node(&catalog.root, parent_id).ok_or_else(|| {
                LiosError::Unsupported(format!("catalog node not found: {parent_id}"))
            })?;
            let CatalogNodeKind::Directory { children } = &parent.kind else {
                unreachable!();
            };
            let conflict_action = if children.iter().any(|child| child.name == target_name) {
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
            let mut target_name = file_name(path)?;
            let Some(path_kind) = packable_path_kind(path)? else {
                report.skipped_paths.push(skipped_link(path));
                continue;
            };
            let source_key = path.display().to_string();
            let conflict_action = {
                let parent = find_directory_mut(&mut catalog.root, parent_id)?;
                let CatalogNodeKind::Directory { children } = &mut parent.kind else {
                    unreachable!();
                };
                if children.iter().any(|child| child.name == target_name) {
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
                }
            };

            match conflict_action {
                Some(ConflictAction::Skip) => continue,
                Some(ConflictAction::KeepBoth) => {
                    let parent = find_directory_mut(&mut catalog.root, parent_id)?;
                    let CatalogNodeKind::Directory { children } = &mut parent.kind else {
                        unreachable!();
                    };
                    target_name = available_name(
                        &children
                            .iter()
                            .map(|child| child.name.as_str())
                            .collect::<Vec<_>>(),
                        &target_name,
                    );
                }
                Some(ConflictAction::Replace) => {
                    let parent = find_directory_mut(&mut catalog.root, parent_id)?;
                    let CatalogNodeKind::Directory { children } = &mut parent.kind else {
                        unreachable!();
                    };
                    children.retain(|child| child.name != target_name);
                }
                None => {}
            }

            let node = match path_kind {
                PackablePathKind::Directory => pack_directory(
                    path,
                    path,
                    target_name,
                    key,
                    &options,
                    &mut tracker,
                    &mut report,
                )?,
                PackablePathKind::File => {
                    pack_file(path, target_name, key, &options, &mut tracker)?
                }
            };
            let parent = find_directory_mut(&mut catalog.root, parent_id)?;
            let CatalogNodeKind::Directory { children } = &mut parent.kind else {
                unreachable!();
            };
            children.push(node);
            parent.updated_at = timestamp();
            sort_children(children);
        }
        self.save_plain(&catalog, key)?;
        Ok(report)
    }

    pub fn remote_files_for_selection(
        &self,
        selection: &CatalogSelection,
        key: &KeyFile,
    ) -> Result<Vec<CatalogRemoteFile>> {
        let catalog = self.load_plain(key)?;
        let node = match selection {
            CatalogSelection::All => &catalog.root,
            CatalogSelection::Node(id) => find_node(&catalog.root, id)
                .ok_or_else(|| LiosError::Unsupported(format!("catalog node not found: {id}")))?,
            CatalogSelection::Nodes(ids) => {
                let mut files = Vec::new();
                for id in ids {
                    let node = find_node(&catalog.root, id).ok_or_else(|| {
                        LiosError::Unsupported(format!("catalog node not found: {id}"))
                    })?;
                    collect_remote_files(node, &mut files);
                }
                files.sort_by(|a, b| a.path.cmp(&b.path));
                files.dedup_by(|a, b| a.path == b.path);
                return Ok(files);
            }
        };
        let mut files = Vec::new();
        collect_remote_files(node, &mut files);
        files.sort_by(|a, b| a.path.cmp(&b.path));
        files.dedup_by(|a, b| a.path == b.path);
        Ok(files)
    }

    pub fn restore(
        &self,
        selection: CatalogSelection,
        key: &KeyFile,
        options: RestoreOptions,
    ) -> Result<()> {
        let catalog = self.load_plain(key)?;
        fs::create_dir_all(&options.output_dir)?;
        match selection {
            CatalogSelection::All => {
                restore_node(
                    &catalog.root,
                    &options.output_dir,
                    key,
                    &self.staging_dir,
                    &options,
                )?;
            }
            CatalogSelection::Node(id) => {
                let node = find_node(&catalog.root, &id).ok_or_else(|| {
                    LiosError::Unsupported(format!("catalog node not found: {id}"))
                })?;
                restore_node(node, &options.output_dir, key, &self.staging_dir, &options)?;
            }
            CatalogSelection::Nodes(ids) => {
                for id in ids {
                    let node = find_node(&catalog.root, &id).ok_or_else(|| {
                        LiosError::Unsupported(format!("catalog node not found: {id}"))
                    })?;
                    restore_node(node, &options.output_dir, key, &self.staging_dir, &options)?;
                }
            }
        }
        Ok(())
    }

    fn load_plain(&self, key: &KeyFile) -> Result<PlainCatalog> {
        let encrypted = fs::read(&self.encrypted_catalog_path)?;
        let decrypted = key.decrypt(&encrypted)?;
        Ok(serde_json::from_slice(&decrypted)?)
    }

    fn save_plain(&self, catalog: &PlainCatalog, key: &KeyFile) -> Result<()> {
        if let Some(parent) = self.encrypted_catalog_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let serialized = serde_json::to_vec(catalog)?;
        let encrypted = key.encrypt(&serialized)?;
        write_atomic(&self.encrypted_catalog_path, &encrypted)?;
        Ok(())
    }
}

fn tree_node(node: &CatalogNode) -> CatalogTreeNode {
    CatalogTreeNode {
        id: node.id.clone(),
        name: node.name.clone(),
        updated_at: node.updated_at.clone(),
        kind: match &node.kind {
            CatalogNodeKind::Directory { children } => CatalogTreeNodeKind::Directory {
                children: children.iter().map(tree_node).collect(),
            },
            CatalogNodeKind::File {
                original_size,
                sha256,
                object_id,
                chunks,
            } => CatalogTreeNodeKind::File {
                original_size: *original_size,
                sha256: sha256.clone(),
                object_id: object_id.clone(),
                chunk_count: chunks.len(),
            },
        },
    }
}

fn drive_item(node: &CatalogNode) -> DriveItem {
    match &node.kind {
        CatalogNodeKind::Directory { children } => DriveItem {
            id: node.id.clone(),
            name: node.name.clone(),
            kind: DriveItemKind::Directory,
            size: 0,
            updated_at: node.updated_at.clone(),
            children_count: children.len(),
        },
        CatalogNodeKind::File { original_size, .. } => DriveItem {
            id: node.id.clone(),
            name: node.name.clone(),
            kind: DriveItemKind::File,
            size: *original_size,
            updated_at: node.updated_at.clone(),
            children_count: 0,
        },
    }
}

fn collect_search_matches(node: &CatalogNode, query: &str, matches: &mut Vec<DriveItem>) {
    if node.name.to_lowercase().contains(query) {
        matches.push(drive_item(node));
    }
    if let CatalogNodeKind::Directory { children } = &node.kind {
        for child in children {
            collect_search_matches(child, query, matches);
        }
    }
}

fn collect_remote_files(node: &CatalogNode, files: &mut Vec<CatalogRemoteFile>) {
    match &node.kind {
        CatalogNodeKind::Directory { children } => {
            for child in children {
                collect_remote_files(child, files);
            }
        }
        CatalogNodeKind::File {
            object_id, chunks, ..
        } => {
            files.push(CatalogRemoteFile {
                path: format!("{FILES_DIR}/{object_id}/{FILE_MANIFEST}"),
                sha256: None,
            });
            files.extend(chunks.iter().map(|chunk| CatalogRemoteFile {
                path: chunk.path.clone(),
                sha256: Some(chunk.encrypted_sha256.clone()),
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
        let len = fs::symlink_metadata(path)?.len();
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
        for entry in fs::read_dir(path)? {
            let entry = entry?;
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

fn pack_directory(
    root: &Path,
    dir: &Path,
    name: String,
    key: &KeyFile,
    options: &PackOptions,
    progress: &mut PackProgressTracker<'_>,
    report: &mut PackReport,
) -> Result<CatalogNode> {
    ensure_packable_path_kind(dir, PackablePathKind::Directory)?;
    let mut children = Vec::new();
    let mut entries = fs::read_dir(dir)?.collect::<std::result::Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let Some(path_kind) = packable_path_kind(&path)? else {
            report.skipped_paths.push(skipped_link(&path));
            continue;
        };
        let child_name = file_name(&path)?;
        match path_kind {
            PackablePathKind::Directory => children.push(pack_directory(
                root, &path, child_name, key, options, progress, report,
            )?),
            PackablePathKind::File => {
                children.push(pack_file(&path, child_name, key, options, progress)?)
            }
        }
    }

    let _ = root;
    Ok(CatalogNode {
        id: random_id(),
        name,
        updated_at: timestamp(),
        kind: CatalogNodeKind::Directory { children },
    })
}

fn pack_file(
    path: &Path,
    name: String,
    key: &KeyFile,
    options: &PackOptions,
    progress: &mut PackProgressTracker<'_>,
) -> Result<CatalogNode> {
    ensure_packable_path_kind(path, PackablePathKind::File)?;
    let temp_chunk_dir = options
        .staging_dir
        .join(TMP_DIR)
        .join("chunks")
        .join(random_id());
    fs::create_dir_all(&temp_chunk_dir)?;

    let mut source = fs::File::open(path)?;
    let mut chunks = Vec::new();
    let mut file_hasher = Sha256::new();
    let mut index = 0usize;
    let mut total_size = 0u64;

    loop {
        let mut buffer = vec![0u8; options.chunk_size];
        let read = source.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        buffer.truncate(read);
        total_size += read as u64;
        file_hasher.update(&buffer);

        let original_sha256 = sha256_hex(&buffer);
        let compressed = zstd::stream::encode_all(buffer.as_slice(), 0)?;
        let encrypted = key.encrypt_deterministic("chunk", &compressed)?;
        let encrypted_sha256 = sha256_hex(&encrypted);
        let chunk_name = format!("{}.lios", key.stable_id("chunk", &buffer)?);
        let chunk_path = temp_chunk_dir.join(&chunk_name);
        fs::write(&chunk_path, encrypted)?;
        chunks.push(ChunkRecord {
            index,
            path: chunk_name,
            original_size: read as u64,
            original_sha256,
            encrypted_sha256,
        });
        index += 1;
        progress.complete_chunk(read as u64);
    }

    if chunks.is_empty() {
        let compressed = zstd::stream::encode_all([].as_slice(), 0)?;
        let encrypted = key.encrypt_deterministic("chunk", &compressed)?;
        let encrypted_sha256 = sha256_hex(&encrypted);
        let chunk_name = format!("{}.lios", key.stable_id("chunk", &[])?);
        fs::write(temp_chunk_dir.join(&chunk_name), encrypted)?;
        chunks.push(ChunkRecord {
            index: 0,
            path: chunk_name,
            original_size: 0,
            original_sha256: sha256_hex(&[]),
            encrypted_sha256,
        });
        progress.complete_chunk(0);
    }

    let file_sha256 = hex::encode(file_hasher.finalize());
    let object_id = key.stable_id("file", file_sha256.as_bytes())?;
    let object_dir = options.staging_dir.join(FILES_DIR).join(&object_id);
    let object_chunks_dir = object_dir.join(FILE_CHUNKS_DIR);
    fs::create_dir_all(&object_dir)?;
    fs::create_dir_all(&object_chunks_dir)?;
    for chunk in &mut chunks {
        let chunk_name = chunk.path.clone();
        let from = temp_chunk_dir.join(&chunk_name);
        let to = object_chunks_dir.join(&chunk_name);
        match fs::symlink_metadata(&to) {
            Ok(metadata) => {
                if is_link_or_junction(&to, &metadata)?
                    || !metadata.is_file()
                    || sha256_file(&to)? != chunk.encrypted_sha256
                {
                    return Err(LiosError::Crypto);
                }
                let _ = fs::remove_file(&from);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                publish_staged_new(&from, &to)?;
            }
            Err(error) => return Err(error.into()),
        }
        chunk.path = format!("{FILES_DIR}/{object_id}/{FILE_CHUNKS_DIR}/{chunk_name}");
    }
    let _ = fs::remove_dir_all(&temp_chunk_dir);
    let file_manifest = serde_json::to_vec(&serde_json::json!({
        "version": 1,
        "object_id": object_id,
        "chunks": &chunks,
    }))?;
    let encrypted_manifest = key.encrypt_deterministic("file-manifest", &file_manifest)?;
    write_atomic_immutable(
        &object_dir.join(FILE_MANIFEST),
        encrypted_manifest.as_slice(),
    )?;

    Ok(CatalogNode {
        id: random_id(),
        name,
        updated_at: timestamp(),
        kind: CatalogNodeKind::File {
            original_size: total_size,
            sha256: file_sha256,
            object_id,
            chunks,
        },
    })
}

fn restore_node(
    node: &CatalogNode,
    parent: &Path,
    key: &KeyFile,
    staging_dir: &Path,
    options: &RestoreOptions,
) -> Result<()> {
    let node_name = restore_local_name(&node.name);
    let restore_root = &options.output_dir;
    match &node.kind {
        CatalogNodeKind::Directory { children } => {
            let dir = parent.join(node_name);
            ensure_restore_descendants_safe(restore_root, &dir)?;
            fs::create_dir_all(&dir)?;
            ensure_restore_descendants_safe(restore_root, &dir)?;
            for child in children {
                restore_node(child, &dir, key, staging_dir, options)?;
            }
        }
        CatalogNodeKind::File { chunks, sha256, .. } => {
            let requested_path = parent.join(node_name);
            ensure_restore_descendants_safe(restore_root, &requested_path)?;
            let output_path = resolve_restore_path(&requested_path, &options.conflict_policy);
            ensure_restore_descendants_safe(restore_root, &output_path)?;
            if let Some(output_parent) = output_path.parent() {
                ensure_restore_descendants_safe(restore_root, output_parent)?;
                fs::create_dir_all(output_parent)?;
                ensure_restore_descendants_safe(restore_root, output_parent)?;
            }
            let mut output = SiblingTempFile::create(&output_path, ".lios-part")?;
            let mut file_hasher = Sha256::new();
            let mut ordered = chunks.iter().collect::<Vec<_>>();
            ordered.sort_by_key(|chunk| chunk.index);
            for chunk in ordered {
                let encrypted = fs::read(staging_dir.join(&chunk.path))?;
                if sha256_hex(&encrypted) != chunk.encrypted_sha256 {
                    return Err(LiosError::Crypto);
                }
                let compressed = key.decrypt(&encrypted)?;
                let data = zstd::stream::decode_all(compressed.as_slice())?;
                if sha256_hex(&data) != chunk.original_sha256 {
                    return Err(LiosError::Crypto);
                }
                file_hasher.update(&data);
                output.file_mut().write_all(&data)?;
            }
            let restored_sha = hex::encode(file_hasher.finalize());
            if restored_sha != *sha256 {
                return Err(LiosError::Crypto);
            }
            ensure_restore_descendants_safe(restore_root, &output_path)?;
            output.persist_new(&output_path)?;
        }
    }
    Ok(())
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

fn find_node<'a>(node: &'a CatalogNode, id: &str) -> Option<&'a CatalogNode> {
    if node.id == id {
        return Some(node);
    }
    match &node.kind {
        CatalogNodeKind::Directory { children } => {
            children.iter().find_map(|child| find_node(child, id))
        }
        CatalogNodeKind::File { .. } => None,
    }
}

fn find_directory_mut<'a>(node: &'a mut CatalogNode, id: &str) -> Result<&'a mut CatalogNode> {
    if node.id == id {
        return match &mut node.kind {
            CatalogNodeKind::Directory { .. } => Ok(node),
            CatalogNodeKind::File { .. } => Err(LiosError::Unsupported(
                "catalog node is not a directory".to_string(),
            )),
        };
    }
    if let CatalogNodeKind::Directory { children } = &mut node.kind {
        for child in children {
            if let Ok(found) = find_directory_mut(child, id) {
                return Ok(found);
            }
        }
    }
    Err(LiosError::Unsupported(format!(
        "catalog node not found: {id}"
    )))
}

fn rename_child(parent: &mut CatalogNode, node_id: &str, new_name: &str) -> Result<bool> {
    let CatalogNodeKind::Directory { children } = &mut parent.kind else {
        return Ok(false);
    };
    if let Some(index) = children.iter().position(|child| child.id == node_id) {
        if children
            .iter()
            .enumerate()
            .any(|(sibling_index, child)| sibling_index != index && child.name == new_name)
        {
            return Err(LiosError::Unsupported(format!(
                "folder already contains {new_name}"
            )));
        }
        children[index].name = new_name.to_string();
        children[index].updated_at = timestamp();
        sort_children(children);
        return Ok(true);
    }
    for child in children {
        if rename_child(child, node_id, new_name)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn delete_children(node: &mut CatalogNode, ids: &HashSet<String>) {
    if let CatalogNodeKind::Directory { children } = &mut node.kind {
        children.retain(|child| !ids.contains(&child.id));
        for child in children {
            delete_children(child, ids);
        }
    }
}

fn sort_children(children: &mut [CatalogNode]) {
    children.sort_by(|a, b| {
        let a_dir = matches!(a.kind, CatalogNodeKind::Directory { .. });
        let b_dir = matches!(b.kind, CatalogNodeKind::Directory { .. });
        b_dir
            .cmp(&a_dir)
            .then_with(|| sort_name_key(&a.name).cmp(&sort_name_key(&b.name)))
    });
}

fn sort_name_key(name: &str) -> (String, String, u32) {
    let path = Path::new(name);
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_lowercase();
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(name)
        .to_lowercase();
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
    if !existing.contains(&name) {
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
        if !existing.contains(&candidate.as_str()) {
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

fn packable_path_kind(path: &Path) -> Result<Option<PackablePathKind>> {
    let metadata = fs::symlink_metadata(path)?;
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
