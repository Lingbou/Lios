use crate::error::{CliError, CliResult};
use lios_application::location::{Location, LocationParser, RemoteLocation};
use lios_application::space_registry::SpaceRegistry;
use lios_application::transfer_planner::TreeEntry;
use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind, DriveItem, DriveItemKind};

pub fn parse_remote(registry: &SpaceRegistry, operand: &str) -> CliResult<RemoteLocation> {
    let names = registry.list()?.into_keys().collect::<Vec<_>>();
    match LocationParser::new(names).parse(operand)? {
        Location::Remote(remote) => Ok(remote),
        Location::Local(_) => Err(CliError::invalid_input(
            "this command requires a Space path such as `photos:/docs`",
        )),
    }
}

pub fn resolve_catalog_path<'a>(
    root: &'a CatalogTreeNode,
    path: &str,
) -> CliResult<&'a CatalogTreeNode> {
    if !path.starts_with('/') {
        return Err(CliError::invalid_input(
            "Catalog paths must be absolute and start with /",
        ));
    }
    let mut current = root;
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        current = match &current.kind {
            CatalogTreeNodeKind::Directory { children } => children
                .iter()
                .find(|child| child.name.eq_ignore_ascii_case(segment))
                .ok_or_else(|| {
                    CliError::invalid_input(format!("Catalog path was not found: {path}"))
                })?,
            CatalogTreeNodeKind::File { .. } => {
                return Err(CliError::invalid_input(format!(
                    "Catalog path traverses through a file: {path}"
                )))
            }
        };
    }
    Ok(current)
}

pub fn catalog_segments(path: &str) -> CliResult<Vec<&str>> {
    if !path.starts_with('/') {
        return Err(CliError::invalid_input(
            "Catalog paths must be absolute and start with /",
        ));
    }
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err(CliError::invalid_input(
            "Catalog paths cannot contain . or .. components",
        ));
    }
    Ok(segments)
}

pub fn find_node_by_id<'a>(root: &'a CatalogTreeNode, id: &str) -> Option<&'a CatalogTreeNode> {
    if root.id == id {
        return Some(root);
    }
    let CatalogTreeNodeKind::Directory { children } = &root.kind else {
        return None;
    };
    children.iter().find_map(|child| find_node_by_id(child, id))
}

pub fn find_child<'a>(
    root: &'a CatalogTreeNode,
    parent_id: &str,
    name: &str,
) -> CliResult<Option<&'a CatalogTreeNode>> {
    let parent = find_node_by_id(root, parent_id)
        .ok_or_else(|| CliError::invalid_input("Catalog parent node was not found"))?;
    let CatalogTreeNodeKind::Directory { children } = &parent.kind else {
        return Err(CliError::invalid_input("Catalog parent is not a directory"));
    };
    Ok(children
        .iter()
        .find(|child| child.name.eq_ignore_ascii_case(name)))
}

pub fn node_kind(node: &CatalogTreeNode) -> DriveItemKind {
    match node.kind {
        CatalogTreeNodeKind::Directory { .. } => DriveItemKind::Directory,
        CatalogTreeNodeKind::File { .. } => DriveItemKind::File,
    }
}

pub fn split_catalog_parent(path: &str) -> CliResult<(String, String)> {
    let segments = catalog_segments(path)?;
    let (name, parents) = segments
        .split_last()
        .ok_or_else(|| CliError::invalid_input("the Space root has no parent"))?;
    let parent = if parents.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parents.join("/"))
    };
    Ok((parent, (*name).to_string()))
}

pub fn join_catalog_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

pub fn list_tree_path(root: &CatalogTreeNode, path: &str) -> CliResult<Vec<DriveItem>> {
    let node = resolve_catalog_path(root, path)?;
    let CatalogTreeNodeKind::Directory { children } = &node.kind else {
        return Err(CliError::invalid_input(format!(
            "Catalog path is not a directory: {path}"
        )));
    };
    Ok(children.iter().map(tree_node_to_drive_item).collect())
}

pub fn tree_node_to_drive_item(node: &CatalogTreeNode) -> DriveItem {
    let (kind, size, children_count) = match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            (DriveItemKind::Directory, 0, children.len())
        }
        CatalogTreeNodeKind::File { original_size, .. } => (DriveItemKind::File, *original_size, 0),
    };
    DriveItem {
        id: node.id.clone(),
        name: node.name.clone(),
        kind,
        size,
        updated_at: node.updated_at.clone(),
        children_count,
    }
}

pub fn collect_matches(node: &CatalogTreeNode, query: &str, matches: &mut Vec<DriveItem>) {
    if node.name.to_ascii_lowercase().contains(query) {
        matches.push(tree_node_to_drive_item(node));
    }
    if let CatalogTreeNodeKind::Directory { children } = &node.kind {
        for child in children {
            collect_matches(child, query, matches);
        }
    }
}

pub fn flatten_catalog_entries(node: &CatalogTreeNode, parent: &str, entries: &mut Vec<TreeEntry>) {
    let path = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/{}", node.name)
    };
    match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            if !path.is_empty() {
                entries.push(TreeEntry::directory(path.clone()));
            }
            let child_parent = if path.is_empty() { "" } else { path.as_str() };
            for child in children {
                let child_path = if child_parent.is_empty() {
                    child.name.clone()
                } else {
                    format!("{child_parent}/{}", child.name)
                };
                match &child.kind {
                    CatalogTreeNodeKind::Directory { .. } => {
                        entries.push(TreeEntry::directory(child_path.clone()));
                        flatten_catalog_children(child, &child_path, entries);
                    }
                    CatalogTreeNodeKind::File {
                        original_size,
                        sha256,
                        ..
                    } => entries.push(TreeEntry::file(child_path, sha256.clone(), *original_size)),
                }
            }
        }
        CatalogTreeNodeKind::File {
            original_size,
            sha256,
            ..
        } if !path.is_empty() => {
            entries.push(TreeEntry::file(path, sha256.clone(), *original_size))
        }
        CatalogTreeNodeKind::File { .. } => {}
    }
}

pub fn flatten_catalog_children(
    node: &CatalogTreeNode,
    node_path: &str,
    entries: &mut Vec<TreeEntry>,
) {
    let CatalogTreeNodeKind::Directory { children } = &node.kind else {
        return;
    };
    for child in children {
        let path = format!("{node_path}/{}", child.name);
        match &child.kind {
            CatalogTreeNodeKind::Directory { .. } => {
                entries.push(TreeEntry::directory(path.clone()));
                flatten_catalog_children(child, &path, entries);
            }
            CatalogTreeNodeKind::File {
                original_size,
                sha256,
                ..
            } => entries.push(TreeEntry::file(path, sha256.clone(), *original_size)),
        }
    }
}

pub fn item_kind(item: &DriveItem) -> &'static str {
    match item.kind {
        DriveItemKind::Directory => "dir ",
        DriveItemKind::File => "file",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_parent_split_is_root_aware() {
        assert_eq!(
            split_catalog_parent("/docs").unwrap(),
            ("/".to_string(), "docs".to_string())
        );
        assert_eq!(
            split_catalog_parent("/a/b").unwrap(),
            ("/a".to_string(), "b".to_string())
        );
    }
}
