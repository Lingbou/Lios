use std::fs;
use std::path::Path;

use lios_core::{
    catalog::{Catalog, CatalogSelection, CatalogTreeNodeKind},
    crypto::KeyFile,
    pack::{PackOptions, PackProgress, PackSource},
    restore::{RestoreConflictPolicy, RestoreOptions},
};
use tempfile::tempdir;

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn read_all_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            entries.push((relative, fs::read(entry.path()).unwrap()));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

#[test]
fn packed_file_restores_to_identical_bytes() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    let key_path = tmp.path().join("lios.key");
    let data = (0..4096).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    write_file(&source, &data);

    let key = KeyFile::generate_to_path(&key_path).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source.clone()),
        &key,
        PackOptions {
            chunk_size: 257,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();

    catalog
        .restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: restore.clone(),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )
        .unwrap();

    assert_eq!(fs::read(restore.join("source.bin")).unwrap(), data);
}

#[test]
fn pack_reports_progress_for_each_chunk() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"0123456789");

    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let mut events = Vec::<PackProgress>::new();
    Catalog::pack_with_progress(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
        |progress| events.push(progress),
    )
    .unwrap();

    assert_eq!(
        events,
        vec![
            PackProgress {
                completed_chunks: 0,
                total_chunks: 3,
                completed_bytes: 0,
                total_bytes: 10,
            },
            PackProgress {
                completed_chunks: 1,
                total_chunks: 3,
                completed_bytes: 4,
                total_bytes: 10,
            },
            PackProgress {
                completed_chunks: 2,
                total_chunks: 3,
                completed_bytes: 8,
                total_bytes: 10,
            },
            PackProgress {
                completed_chunks: 3,
                total_chunks: 3,
                completed_bytes: 10,
                total_bytes: 10,
            },
        ]
    );
}

#[test]
fn wrong_key_cannot_decrypt_catalog_or_chunks() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("secret.txt");
    let staging = tmp.path().join("staging");
    write_file(&source, b"private data");

    let good_key = KeyFile::generate_to_path(tmp.path().join("good.key")).unwrap();
    let wrong_key = KeyFile::generate_to_path(tmp.path().join("wrong.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &good_key,
        PackOptions {
            chunk_size: 5,
            staging_dir: staging,
        },
    )
    .unwrap();

    let result = catalog.restore(
        CatalogSelection::All,
        &wrong_key,
        RestoreOptions {
            output_dir: tmp.path().join("restore"),
            conflict_policy: RestoreConflictPolicy::Rename,
        },
    );

    assert!(result.is_err());
}

#[test]
fn encrypted_manifest_does_not_contain_plaintext_names() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("very-sensitive-name.txt");
    let staging = tmp.path().join("staging");
    write_file(&source, b"classified");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();

    let bytes = fs::read(catalog.encrypted_catalog_path()).unwrap();
    let as_text = String::from_utf8_lossy(&bytes);

    assert!(!as_text.contains("very-sensitive-name.txt"));
    assert!(!as_text.contains("classified"));
}

#[test]
fn directory_restore_preserves_tree_and_renames_conflicts() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("album");
    write_file(&source_dir.join("a.txt"), b"A");
    write_file(&source_dir.join("nested/b.txt"), b"B");
    write_file(&source_dir.join("nested/deep/c.txt"), b"C");

    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    fs::create_dir_all(&restore).unwrap();
    write_file(&restore.join("album/a.txt"), b"existing");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source_dir),
        &key,
        PackOptions {
            chunk_size: 2,
            staging_dir: staging,
        },
    )
    .unwrap();

    catalog
        .restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: restore.clone(),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )
        .unwrap();

    let files = read_all_files(&restore);
    assert!(files.contains(&("album/a.txt".to_string(), b"existing".to_vec())));
    assert!(files
        .iter()
        .any(|(name, bytes)| name.starts_with("album/a (restored") && bytes == b"A"));
    assert!(files.contains(&("album/nested/b.txt".to_string(), b"B".to_vec())));
    assert!(files.contains(&("album/nested/deep/c.txt".to_string(), b"C".to_vec())));
}

#[test]
fn catalog_exposes_decrypted_tree_and_remote_files_for_selected_node() {
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("project");
    write_file(&source_dir.join("docs/readme.md"), b"readme");
    write_file(&source_dir.join("src/main.rs"), b"fn main() {}");

    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source_dir),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();

    let tree = catalog.decrypt_tree(&key).unwrap();
    assert_eq!(tree.name, "project");
    let docs_id = match &tree.kind {
        CatalogTreeNodeKind::Directory { children } => children
            .iter()
            .find(|child| child.name == "docs")
            .map(|child| child.id.clone())
            .unwrap(),
        CatalogTreeNodeKind::File { .. } => panic!("root should be a directory"),
    };

    let selected = catalog
        .remote_files_for_selection(&CatalogSelection::Node(docs_id), &key)
        .unwrap();
    let all = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();

    assert!(selected
        .iter()
        .any(|file| file.path.ends_with("/manifest.enc")));
    assert!(selected.iter().any(|file| file.path.ends_with(".lios")));
    assert!(all.len() > selected.len());
}

#[test]
fn unchanged_chunks_keep_encrypted_hashes_but_live_under_file_object_directories() {
    let tmp = tempdir().unwrap();
    let first = tmp.path().join("first.bin");
    let second = tmp.path().join("second.bin");
    write_file(&first, b"aaaabbbb");
    write_file(&second, b"aaaacccc");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let first_catalog = Catalog::pack(
        PackSource::Path(first),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("first-staging"),
        },
    )
    .unwrap();
    let second_catalog = Catalog::pack(
        PackSource::Path(second),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("second-staging"),
        },
    )
    .unwrap();

    let first_chunks = first_catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .filter(|file| file.path.ends_with(".lios"))
        .collect::<Vec<_>>();
    let second_chunks = second_catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .filter(|file| file.path.ends_with(".lios"))
        .collect::<Vec<_>>();

    assert_eq!(first_chunks.len(), 2);
    assert_eq!(second_chunks.len(), 2);
    assert!(first_chunks.iter().any(|first| {
        second_chunks
            .iter()
            .any(|second| first.path != second.path && first.sha256 == second.sha256)
    }));
    assert!(first_chunks
        .iter()
        .chain(second_chunks.iter())
        .all(|file| file.path.starts_with("objects/files/") && file.path.contains("/chunks/")));
}
