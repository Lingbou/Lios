use std::fs;
use std::path::Path;

use lios_core::{
    catalog::{
        Catalog, CatalogSelection, CatalogTreeNode, CatalogTreeNodeKind, ConflictAction,
        ConflictResolution,
    },
    crypto::KeyFile,
    pack::{PackOptions, PackProgress},
};
use tempfile::tempdir;

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn child<'a>(node: &'a CatalogTreeNode, name: &str) -> &'a CatalogTreeNode {
    match &node.kind {
        CatalogTreeNodeKind::Directory { children } => children
            .iter()
            .find(|child| child.name == name)
            .unwrap_or_else(|| panic!("missing child {name}")),
        CatalogTreeNodeKind::File { .. } => panic!("expected directory"),
    }
}

fn child_names(node: &CatalogTreeNode) -> Vec<String> {
    match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            children.iter().map(|child| child.name.clone()).collect()
        }
        CatalogTreeNodeKind::File { .. } => panic!("expected directory"),
    }
}

#[test]
fn empty_space_initializes_encryptable_catalog() {
    let tmp = tempdir().unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog =
        Catalog::initialize_empty("Lios Space", &key, tmp.path().join("staging")).unwrap();

    let tree = catalog.decrypt_tree(&key).unwrap();
    assert_eq!(tree.name, "Lios Space");
    assert!(matches!(
        tree.kind,
        CatalogTreeNodeKind::Directory { ref children } if children.is_empty()
    ));

    let encrypted = fs::read(catalog.encrypted_catalog_path()).unwrap();
    assert!(!String::from_utf8_lossy(&encrypted).contains("Lios Space"));
}

#[test]
fn uploads_into_existing_directory_without_replacing_siblings() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog.create_folder(&root_id, "docs", &key).unwrap();
    let docs_id = child(&catalog.decrypt_tree(&key).unwrap(), "docs")
        .id
        .clone();

    let first = tmp.path().join("a.txt");
    let second = tmp.path().join("b.txt");
    write_file(&first, b"A");
    write_file(&second, b"B");

    catalog
        .add_paths_to_folder(
            &docs_id,
            &[first, second],
            &[],
            &key,
            PackOptions {
                chunk_size: 2,
                staging_dir: staging,
            },
        )
        .unwrap();

    let tree = catalog.decrypt_tree(&key).unwrap();
    let docs = child(&tree, "docs");
    assert_eq!(child_names(docs), vec!["a.txt", "b.txt"]);
}

#[test]
fn upload_progress_total_is_stable_for_multiple_paths() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let first = tmp.path().join("a.bin");
    let second = tmp.path().join("b.bin");
    write_file(&first, b"1234");
    write_file(&second, b"567");

    let mut events = Vec::<PackProgress>::new();
    catalog
        .add_paths_to_folder_with_progress(
            &root_id,
            &[first, second],
            &[],
            &key,
            PackOptions {
                chunk_size: 2,
                staging_dir: staging,
            },
            |progress| events.push(progress),
        )
        .unwrap();

    assert_eq!(events.first().unwrap().total_chunks, 4);
    assert!(events.iter().all(|event| event.total_chunks == 4));
    assert_eq!(events.last().unwrap().completed_chunks, 4);
}

#[test]
fn uploaded_file_chunks_live_under_the_file_object_directory() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let source = tmp.path().join("video.bin");
    write_file(&source, b"abcdef");

    catalog
        .add_paths_to_folder(
            &root_id,
            &[source],
            &[],
            &key,
            PackOptions {
                chunk_size: 2,
                staging_dir: staging,
            },
        )
        .unwrap();

    let tree = catalog.decrypt_tree(&key).unwrap();
    let file = child(&tree, "video.bin");
    let object_id = match &file.kind {
        CatalogTreeNodeKind::File { object_id, .. } => object_id,
        CatalogTreeNodeKind::Directory { .. } => panic!("expected file"),
    };
    let remote_files = catalog
        .remote_files_for_selection(&CatalogSelection::Node(file.id.clone()), &key)
        .unwrap();
    let object_prefix = format!("objects/files/{object_id}/");

    assert!(remote_files
        .iter()
        .any(|file| file.path == format!("{object_prefix}manifest.enc")));
    assert!(remote_files
        .iter()
        .filter(|file| file.path.ends_with(".lios"))
        .all(|file| file.path.starts_with(&format!("{object_prefix}chunks/"))));
    assert!(remote_files
        .iter()
        .all(|file| !file.path.starts_with("objects/chunks/")));
}

#[test]
fn upload_conflict_supports_replace_keep_both_and_skip() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let old = tmp.path().join("old/same.txt");
    let new = tmp.path().join("new/same.txt");
    write_file(&old, b"old");
    write_file(&new, b"new");
    let options = PackOptions {
        chunk_size: 2,
        staging_dir: staging.clone(),
    };
    catalog
        .add_paths_to_folder(&root_id, &[old.clone()], &[], &key, options.clone())
        .unwrap();

    let conflicts = catalog
        .preview_upload_conflicts(&root_id, &[new.clone()], &key)
        .unwrap();
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].target_name, "same.txt");

    catalog
        .add_paths_to_folder(
            &root_id,
            &[new.clone()],
            &[ConflictResolution {
                source_path: new.display().to_string(),
                action: ConflictAction::KeepBoth,
            }],
            &key,
            options.clone(),
        )
        .unwrap();
    let names = child_names(&catalog.decrypt_tree(&key).unwrap());
    assert_eq!(names, vec!["same.txt", "same (1).txt"]);

    catalog
        .add_paths_to_folder(
            &root_id,
            &[new.clone()],
            &[ConflictResolution {
                source_path: new.display().to_string(),
                action: ConflictAction::Skip,
            }],
            &key,
            options.clone(),
        )
        .unwrap();
    let names = child_names(&catalog.decrypt_tree(&key).unwrap());
    assert_eq!(names, vec!["same.txt", "same (1).txt"]);

    catalog
        .add_paths_to_folder(
            &root_id,
            &[new],
            &[ConflictResolution {
                source_path: tmp.path().join("new/same.txt").display().to_string(),
                action: ConflictAction::Replace,
            }],
            &key,
            options,
        )
        .unwrap();
    let names = child_names(&catalog.decrypt_tree(&key).unwrap());
    assert_eq!(names, vec!["same.txt", "same (1).txt"]);
}

#[test]
fn delete_removes_catalog_references_to_remote_objects() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let file = tmp.path().join("delete-me.txt");
    write_file(&file, b"gone");
    catalog
        .add_paths_to_folder(
            &root_id,
            &[file],
            &[],
            &key,
            PackOptions {
                chunk_size: 2,
                staging_dir: staging,
            },
        )
        .unwrap();
    let file_id = child(&catalog.decrypt_tree(&key).unwrap(), "delete-me.txt")
        .id
        .clone();
    let before = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();

    catalog.delete_nodes(&[file_id], &key).unwrap();
    let after = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();

    assert!(before.iter().any(|file| file.path.ends_with(".lios")));
    assert!(after.iter().all(|file| !file.path.ends_with(".lios")));
}

#[test]
fn rename_and_create_folder_only_change_catalog_references() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let file = tmp.path().join("note.txt");
    write_file(&file, b"note");
    catalog
        .add_paths_to_folder(
            &root_id,
            &[file],
            &[],
            &key,
            PackOptions {
                chunk_size: 2,
                staging_dir: staging,
            },
        )
        .unwrap();
    let file_id = child(&catalog.decrypt_tree(&key).unwrap(), "note.txt")
        .id
        .clone();
    let before = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();

    catalog.rename_node(&file_id, "renamed.txt", &key).unwrap();
    catalog.create_folder(&root_id, "empty", &key).unwrap();
    let after = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();
    let tree = catalog.decrypt_tree(&key).unwrap();

    assert_eq!(before, after);
    assert!(child_names(&tree).contains(&"renamed.txt".to_string()));
    assert!(child_names(&tree).contains(&"empty".to_string()));
}

#[test]
fn encrypted_catalog_after_drive_edits_does_not_contain_plaintext_names() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let file = tmp.path().join("very-secret.txt");
    write_file(&file, b"classified");

    catalog
        .add_paths_to_folder(
            &root_id,
            &[file],
            &[],
            &key,
            PackOptions {
                chunk_size: 4,
                staging_dir: staging,
            },
        )
        .unwrap();
    let encrypted = fs::read(catalog.encrypted_catalog_path()).unwrap();
    let text = String::from_utf8_lossy(&encrypted);

    assert!(!text.contains("very-secret.txt"));
    assert!(!text.contains("classified"));
}
