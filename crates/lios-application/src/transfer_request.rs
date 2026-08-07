//! Filesystem snapshotting and rsync-style destination mapping for push plans.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::location::LocalLocation;
use crate::transfer_planner::{EntryKind, PlanOptions, TransferPlan, TransferPlanner, TreeEntry};
use crate::{sha256_hex_file, to_err, CommandError, CommandResult};
use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind};
use lios_core::tasks::{
    PersistedTransferAction, PersistedTransferPlan, TransferActionKind, TransferActionState,
    TransferDirection, TransferEntryKind,
};

#[derive(Debug, Clone)]
pub struct PreparedPush {
    pub plan: TransferPlan,
    pub source_paths: BTreeMap<String, PathBuf>,
    pub source_fingerprints: BTreeMap<String, String>,
}

#[derive(Debug, Clone)]
pub struct RemoteSource {
    pub node: CatalogTreeNode,
    pub trailing_slash: bool,
}

#[derive(Debug, Clone)]
pub struct PreparedPull {
    pub plan: TransferPlan,
    pub destination_root: PathBuf,
    pub remote_node_ids: BTreeMap<String, String>,
    pub destination_paths: BTreeMap<String, PathBuf>,
    pub destination_fingerprints: BTreeMap<String, String>,
}

impl PreparedPull {
    pub fn into_persisted(
        self,
        source_operand: String,
        destination_operand: String,
        source_trailing_slash: bool,
        excludes: Vec<String>,
        remote_catalog_baseline: Option<String>,
        delete_scope: Option<String>,
    ) -> PersistedTransferPlan {
        let actions = self
            .plan
            .actions
            .into_iter()
            .map(|action| PersistedTransferAction {
                relative_path: action.path.clone(),
                source_path: None,
                remote_node_id: self.remote_node_ids.get(&action.path).cloned(),
                local_destination_path: self.destination_paths.get(&action.path).cloned(),
                kind: match action.kind {
                    crate::transfer_planner::PlanActionKind::Create => TransferActionKind::Create,
                    crate::transfer_planner::PlanActionKind::Update => TransferActionKind::Update,
                    crate::transfer_planner::PlanActionKind::Skip => TransferActionKind::Skip,
                    crate::transfer_planner::PlanActionKind::Delete => TransferActionKind::Delete,
                    crate::transfer_planner::PlanActionKind::ReplaceType => {
                        TransferActionKind::ReplaceType
                    }
                },
                entry_kind: match action.entry_kind {
                    EntryKind::File => TransferEntryKind::File,
                    EntryKind::Directory => TransferEntryKind::Directory,
                },
                source_sha256: action.source_sha256,
                source_fingerprint: None,
                size: action.size,
                destination_fingerprint: self.destination_fingerprints.get(&action.path).cloned(),
                state: if action.kind == crate::transfer_planner::PlanActionKind::Skip {
                    TransferActionState::Skipped
                } else {
                    TransferActionState::Pending
                },
            })
            .collect();
        PersistedTransferPlan {
            direction: TransferDirection::Pull,
            source_operand,
            destination_operand,
            source_trailing_slash,
            excludes,
            remote_catalog_baseline,
            delete_scope,
            actions,
        }
    }
}

impl PreparedPush {
    pub fn into_persisted(
        self,
        source_operand: String,
        destination_operand: String,
        source_trailing_slash: bool,
        excludes: Vec<String>,
        remote_catalog_baseline: Option<String>,
        delete_scope: Option<String>,
    ) -> PersistedTransferPlan {
        let actions = self
            .plan
            .actions
            .into_iter()
            .map(|action| PersistedTransferAction {
                source_path: self.source_paths.get(&action.path).cloned(),
                remote_node_id: None,
                local_destination_path: None,
                source_fingerprint: self.source_fingerprints.get(&action.path).cloned(),
                destination_fingerprint: None,
                source_sha256: action.source_sha256,
                size: action.size,
                relative_path: action.path.clone(),
                entry_kind: match action.entry_kind {
                    EntryKind::File => TransferEntryKind::File,
                    EntryKind::Directory => TransferEntryKind::Directory,
                },
                kind: match action.kind {
                    crate::transfer_planner::PlanActionKind::Create => TransferActionKind::Create,
                    crate::transfer_planner::PlanActionKind::Update => TransferActionKind::Update,
                    crate::transfer_planner::PlanActionKind::Skip => TransferActionKind::Skip,
                    crate::transfer_planner::PlanActionKind::Delete => TransferActionKind::Delete,
                    crate::transfer_planner::PlanActionKind::ReplaceType => {
                        TransferActionKind::ReplaceType
                    }
                },
                state: if action.kind == crate::transfer_planner::PlanActionKind::Skip {
                    TransferActionState::Skipped
                } else {
                    TransferActionState::Pending
                },
            })
            .collect();
        PersistedTransferPlan {
            direction: TransferDirection::Push,
            source_operand,
            destination_operand,
            source_trailing_slash,
            excludes,
            remote_catalog_baseline,
            delete_scope,
            actions,
        }
    }
}

pub fn fingerprint_path(path: &Path) -> CommandResult<String> {
    let metadata = safe_metadata(path)?;
    local_fingerprint(&metadata)
}

pub fn prepare_push(
    sources: &[LocalLocation],
    remote_destination: &str,
    remote_entries: &[TreeEntry],
    options: &PlanOptions,
) -> CommandResult<PreparedPush> {
    if sources.is_empty() {
        return Err(CommandError::invalid_input("copy source cannot be empty"));
    }
    let destination = catalog_relative(remote_destination)?;
    let destination_kind = remote_entries
        .iter()
        .find(|entry| entry.path.eq_ignore_ascii_case(&destination))
        .map(|entry| entry.kind);
    if sources.len() > 1 && destination_kind == Some(EntryKind::File) {
        return Err(CommandError::invalid_input(
            "multiple copy sources require a directory destination",
        ));
    }

    let mut source_entries = BTreeMap::<String, TreeEntry>::new();
    let mut source_paths = BTreeMap::new();
    let mut source_fingerprints = BTreeMap::new();
    let mut delete_destination_root = false;
    let mut exclude_root = None;
    if sources.len() > 1 && !destination.is_empty() {
        insert_directory_with_ancestors(&mut source_entries, &destination);
    }

    for source in sources {
        let metadata = safe_metadata(&source.path)?;
        let source_name = source
            .path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .ok_or_else(|| CommandError::invalid_input("copy source has no file name"))?;
        if metadata.is_file() {
            if source.trailing_slash {
                return Err(CommandError::invalid_input(
                    "a trailing slash is only valid for a directory source",
                ));
            }
            let target = if sources.len() > 1 || destination_kind == Some(EntryKind::Directory) {
                join_catalog(&destination, &source_name)
            } else {
                destination.clone()
            };
            if target.is_empty() {
                return Err(CommandError::invalid_input(
                    "a file cannot replace the Space root",
                ));
            }
            insert_file(
                &mut source_entries,
                &mut source_paths,
                &mut source_fingerprints,
                target,
                source.path.clone(),
                &metadata,
            )?;
            continue;
        }
        if !metadata.is_dir() {
            return Err(CommandError::invalid_input(
                "copy sources must be regular files or directories",
            ));
        }
        delete_destination_root = delete_destination_root
            || (sources.len() == 1 && source.trailing_slash && destination.is_empty());

        let base = if source.trailing_slash {
            destination.clone()
        } else {
            join_catalog(&destination, &source_name)
        };
        if sources.len() == 1 {
            exclude_root = Some(base.clone());
        }
        if !base.is_empty() {
            insert_directory_with_ancestors(&mut source_entries, &base);
        }
        snapshot_directory(
            &source.path,
            &base,
            &mut source_entries,
            &mut source_paths,
            &mut source_fingerprints,
        )?;
    }

    let source_entries = source_entries.into_values().collect::<Vec<_>>();
    let destination_entries = transfer_scope_entries(
        &source_entries,
        remote_entries,
        options.delete,
        delete_destination_root,
    );
    let mut planner_options = options.clone();
    planner_options.exclude_root = exclude_root;
    let plan = TransferPlanner::plan(&source_entries, &destination_entries, &planner_options)?;
    Ok(PreparedPush {
        plan,
        source_paths,
        source_fingerprints,
    })
}

pub fn prepare_pull(
    sources: &[RemoteSource],
    local_destination: &LocalLocation,
    options: &PlanOptions,
) -> CommandResult<PreparedPull> {
    if sources.is_empty() {
        return Err(CommandError::invalid_input("copy source cannot be empty"));
    }
    let destination = absolute_local_path(&local_destination.path)?;
    let destination_metadata = match fs::symlink_metadata(&destination) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) {
                return Err(CommandError::invalid_input(
                    "restore destination contains unsupported symbolic links or junctions",
                ));
            }
            Some(metadata)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(to_err(error)),
    };
    if sources.len() > 1
        && destination_metadata
            .as_ref()
            .is_some_and(fs::Metadata::is_file)
    {
        return Err(CommandError::invalid_input(
            "multiple copy sources require a directory destination",
        ));
    }

    let exact_file = sources.len() == 1
        && matches!(sources[0].node.kind, CatalogTreeNodeKind::File { .. })
        && !destination_metadata
            .as_ref()
            .is_some_and(fs::Metadata::is_dir)
        && !local_destination.trailing_slash;
    let (destination_root, exact_name) = if exact_file {
        let parent = destination
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        let name = destination
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| CommandError::invalid_input("local destination has no file name"))?
            .to_string();
        (parent, Some(name))
    } else {
        (destination, None)
    };

    let mut source_entries = BTreeMap::new();
    let mut remote_node_ids = BTreeMap::new();
    let mut exclude_root = None;
    let delete_destination_root = sources.len() == 1
        && sources[0].trailing_slash
        && matches!(sources[0].node.kind, CatalogTreeNodeKind::Directory { .. });
    for (index, source) in sources.iter().enumerate() {
        let target = if let Some(exact_name) = exact_name.as_ref() {
            exact_name.clone()
        } else if matches!(source.node.kind, CatalogTreeNodeKind::Directory { .. })
            && source.trailing_slash
        {
            String::new()
        } else {
            source.node.name.clone()
        };
        if sources.len() == 1 && matches!(source.node.kind, CatalogTreeNodeKind::Directory { .. }) {
            exclude_root = Some(target.clone());
        }
        if sources.len() > 1 && exact_name.is_some() {
            return Err(CommandError::invalid_input(
                "multiple remote sources require a directory destination",
            ));
        }
        flatten_remote_source(
            &source.node,
            &target,
            source.trailing_slash,
            &mut source_entries,
            &mut remote_node_ids,
        )?;
        if index > 0 && target.is_empty() {
            return Err(CommandError::invalid_input(
                "multiple directory-content sources cannot share one destination root",
            ));
        }
    }

    let (destination_entries, destination_fingerprints) =
        snapshot_local_destination(&destination_root)?;
    let source_entries = source_entries.into_values().collect::<Vec<_>>();
    let scoped_destination = transfer_scope_entries(
        &source_entries,
        &destination_entries,
        options.delete,
        delete_destination_root,
    );
    let mut planner_options = options.clone();
    planner_options.exclude_root = exclude_root;
    let plan = TransferPlanner::plan(&source_entries, &scoped_destination, &planner_options)?;
    let destination_paths = plan
        .actions
        .iter()
        .map(|action| {
            (
                action.path.clone(),
                destination_root.join(Path::new(&action.path)),
            )
        })
        .collect();
    Ok(PreparedPull {
        plan,
        destination_root,
        remote_node_ids,
        destination_paths,
        destination_fingerprints,
    })
}

fn flatten_remote_source(
    node: &CatalogTreeNode,
    target: &str,
    contents_only: bool,
    entries: &mut BTreeMap<String, TreeEntry>,
    node_ids: &mut BTreeMap<String, String>,
) -> CommandResult<()> {
    if contents_only {
        let CatalogTreeNodeKind::Directory { children } = &node.kind else {
            return Err(CommandError::invalid_input(
                "a trailing slash is only valid for a directory source",
            ));
        };
        for child in children {
            flatten_remote_node(child, &child.name, entries, node_ids)?;
        }
        return Ok(());
    }
    flatten_remote_node(node, target, entries, node_ids)
}

fn flatten_remote_node(
    node: &CatalogTreeNode,
    target: &str,
    entries: &mut BTreeMap<String, TreeEntry>,
    node_ids: &mut BTreeMap<String, String>,
) -> CommandResult<()> {
    if target.is_empty() {
        return Err(CommandError::invalid_input(
            "remote source cannot map to an empty local path",
        ));
    }
    match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            entries.insert(target.to_string(), TreeEntry::directory(target));
            node_ids.insert(target.to_string(), node.id.clone());
            for child in children {
                flatten_remote_node(child, &join_catalog(target, &child.name), entries, node_ids)?;
            }
        }
        CatalogTreeNodeKind::File {
            original_size,
            sha256,
            ..
        } => {
            entries.insert(
                target.to_string(),
                TreeEntry::file(target, sha256.clone(), *original_size),
            );
            node_ids.insert(target.to_string(), node.id.clone());
        }
    }
    Ok(())
}

fn snapshot_local_destination(
    root: &Path,
) -> CommandResult<(Vec<TreeEntry>, BTreeMap<String, String>)> {
    if !root.exists() {
        return Ok((Vec::new(), BTreeMap::new()));
    }
    let metadata = safe_metadata(root)?;
    if !metadata.is_dir() {
        return Ok((Vec::new(), BTreeMap::new()));
    }
    let mut entries = Vec::new();
    let mut fingerprints = BTreeMap::new();
    for entry in WalkDir::new(root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(to_err)?;
        let metadata = safe_metadata(entry.path())?;
        let relative = path_to_catalog(
            entry
                .path()
                .strip_prefix(root)
                .map_err(|_| CommandError::invalid_input("local destination escaped its root"))?,
        )?;
        fingerprints.insert(relative.clone(), local_fingerprint(&metadata)?);
        if metadata.is_dir() {
            entries.push(TreeEntry::directory(relative));
        } else if metadata.is_file() {
            entries.push(TreeEntry::file(
                relative,
                sha256_hex_file(entry.path())?,
                metadata.len(),
            ));
        } else {
            return Err(CommandError::invalid_input(
                "restore destination contains an unsupported file type",
            ));
        }
    }
    Ok((entries, fingerprints))
}

fn absolute_local_path(path: &Path) -> CommandResult<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir().map_err(to_err)?.join(path))
    }
}

fn snapshot_directory(
    root: &Path,
    target_base: &str,
    entries: &mut BTreeMap<String, TreeEntry>,
    source_paths: &mut BTreeMap<String, PathBuf>,
    fingerprints: &mut BTreeMap<String, String>,
) -> CommandResult<()> {
    for entry in WalkDir::new(root).follow_links(false).min_depth(1) {
        let entry = entry.map_err(to_err)?;
        let metadata = safe_metadata(entry.path())?;
        let relative = entry.path().strip_prefix(root).map_err(|_| {
            CommandError::invalid_input("copy source escaped its selected directory")
        })?;
        let relative = path_to_catalog(relative)?;
        let target = join_catalog(target_base, &relative);
        if metadata.is_dir() {
            insert_directory_with_ancestors(entries, &target);
        } else if metadata.is_file() {
            insert_file(
                entries,
                source_paths,
                fingerprints,
                target,
                entry.path().to_path_buf(),
                &metadata,
            )?;
        } else {
            return Err(CommandError::invalid_input(
                "copy sources must contain only regular files and directories",
            ));
        }
    }
    Ok(())
}

fn insert_file(
    entries: &mut BTreeMap<String, TreeEntry>,
    source_paths: &mut BTreeMap<String, PathBuf>,
    fingerprints: &mut BTreeMap<String, String>,
    target: String,
    source: PathBuf,
    metadata: &fs::Metadata,
) -> CommandResult<()> {
    if let Some(parent) = target.rsplit_once('/').map(|(parent, _)| parent) {
        if !parent.is_empty() {
            insert_directory_with_ancestors(entries, parent);
        }
    }
    let sha256 = sha256_hex_file(&source)?;
    entries.insert(
        target.clone(),
        TreeEntry::file(&target, sha256, metadata.len()),
    );
    fingerprints.insert(target.clone(), local_fingerprint(metadata)?);
    source_paths.insert(target, source);
    Ok(())
}

fn insert_directory_with_ancestors(entries: &mut BTreeMap<String, TreeEntry>, path: &str) {
    let mut current = String::new();
    for component in path.split('/').filter(|component| !component.is_empty()) {
        current = join_catalog(&current, component);
        entries
            .entry(current.clone())
            .or_insert_with(|| TreeEntry::directory(current.clone()));
    }
}

fn transfer_scope_entries(
    source: &[TreeEntry],
    destination: &[TreeEntry],
    delete: bool,
    delete_destination_root: bool,
) -> Vec<TreeEntry> {
    if !delete {
        return destination.to_vec();
    }
    if delete_destination_root {
        return destination.to_vec();
    }
    let source_paths = source
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<BTreeSet<_>>();
    let roots = source
        .iter()
        .filter(|candidate| {
            !source.iter().any(|other| {
                candidate.path != other.path
                    && candidate.path.starts_with(&format!("{}/", other.path))
            })
        })
        .map(|entry| entry.path.as_str())
        .collect::<Vec<_>>();
    destination
        .iter()
        .filter(|entry| {
            source_paths.contains(entry.path.as_str())
                || roots
                    .iter()
                    .any(|root| entry.path == *root || entry.path.starts_with(&format!("{root}/")))
        })
        .cloned()
        .collect()
}

fn safe_metadata(path: &Path) -> CommandResult<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(to_err)?;
    if metadata.file_type().is_symlink() || is_windows_reparse_point(&metadata) {
        return Err(CommandError::invalid_input(
            "copy source contains unsupported symbolic links or junctions",
        ));
    }
    Ok(metadata)
}

#[cfg(windows)]
fn is_windows_reparse_point(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn is_windows_reparse_point(_metadata: &fs::Metadata) -> bool {
    false
}

fn local_fingerprint(metadata: &fs::Metadata) -> CommandResult<String> {
    let modified = metadata
        .modified()
        .map_err(to_err)?
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CommandError::invalid_input("source modification time predates Unix epoch"))?
        .as_nanos();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(format!(
            "{}:{}:{}:{}",
            metadata.dev(),
            metadata.ino(),
            metadata.len(),
            modified
        ))
    }
    #[cfg(not(unix))]
    Ok(format!("{}:{modified}", metadata.len()))
}

fn catalog_relative(path: &str) -> CommandResult<String> {
    if !path.starts_with('/') || path.contains('\\') {
        return Err(CommandError::invalid_input(
            "remote Catalog paths must be absolute",
        ));
    }
    let components = path
        .split('/')
        .filter(|component| !component.is_empty())
        .collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| *component == "." || *component == "..")
    {
        return Err(CommandError::invalid_input(
            "remote Catalog path contains an unsafe component",
        ));
    }
    Ok(components.join("/"))
}

fn path_to_catalog(path: &Path) -> CommandResult<String> {
    let mut components = Vec::new();
    for component in path.components() {
        let std::path::Component::Normal(value) = component else {
            return Err(CommandError::invalid_input(
                "local source contains an unsupported path component",
            ));
        };
        let value = value
            .to_str()
            .ok_or_else(|| CommandError::invalid_input("local source name is not valid UTF-8"))?;
        components.push(value);
    }
    Ok(components.join("/"))
}

fn join_catalog(parent: &str, child: &str) -> String {
    match (parent.is_empty(), child.is_empty()) {
        (true, _) => child.trim_matches('/').to_string(),
        (_, true) => parent.trim_matches('/').to_string(),
        (false, false) => format!("{}/{}", parent.trim_matches('/'), child.trim_matches('/')),
    }
}
