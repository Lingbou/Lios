use std::fs;
use std::path::Path;

use lios_core::{
    catalog::{
        Catalog, CatalogSelection, CatalogTreeNodeKind, CatalogV2, NodeDescriptorKindV2,
        ObjectManifestV2, StorageRef,
    },
    crypto::KeyFile,
    format_v2::{decrypt_envelope_v2, encrypt_envelope_v2, EnvelopeKindV2},
    pack::{PackOptions, PackProgress, PackSource},
    restore::{RestoreConflictPolicy, RestoreOptions},
    LiosError,
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn read_catalog_v2(catalog: &Catalog, key: &KeyFile) -> CatalogV2 {
    let encrypted = fs::read(catalog.encrypted_catalog_path()).unwrap();
    let plaintext = decrypt_envelope_v2(key, EnvelopeKindV2::Catalog, &encrypted).unwrap();
    serde_json::from_slice(&plaintext).unwrap()
}

fn write_catalog_v2(catalog: &Catalog, key: &KeyFile, value: &CatalogV2) {
    let plaintext = serde_json::to_vec(value).unwrap();
    let encrypted = encrypt_envelope_v2(key, EnvelopeKindV2::Catalog, &plaintext).unwrap();
    fs::write(catalog.encrypted_catalog_path(), encrypted).unwrap();
}

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

fn assert_restore_link_error(result: Result<(), LiosError>) {
    let Err(LiosError::Unsupported(message)) = result else {
        panic!("expected restore path link rejection");
    };
    assert!(message.contains("restore path contains symlink or junction"));
}

#[test]
fn golden_v1_catalog_and_chunk_restore_end_to_end() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    let chunk_path = staging.join("objects/files/legacy-object/chunks/golden.lios");
    let restore = tmp.path().join("restore");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(
        &chunk_path,
        &fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();

    Catalog::from_staging(staging)
        .restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: restore.clone(),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )
        .unwrap();

    assert_eq!(
        fs::read(restore.join("legacy-archive/legacy.bin")).unwrap(),
        fs::read(fixtures.join("legacy_chunk_v1.bin")).unwrap()
    );
}

#[test]
fn oversized_legacy_zstd_chunk_is_rejected_at_declared_bound() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(
        &staging.join("objects/files/legacy-object/chunks/golden.lios"),
        &fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging);
    catalog
        .create_folder("legacy-root", "migration-marker", &key)
        .unwrap();

    let mut value = read_catalog_v2(&catalog, &key);
    let (file_id, object_id) = value
        .nodes
        .iter()
        .find_map(|(node_id, node)| match &node.descriptor.kind {
            NodeDescriptorKindV2::File { object_id, .. } => {
                Some((node_id.clone(), object_id.clone()))
            }
            NodeDescriptorKindV2::Directory => None,
        })
        .unwrap();
    let declared_size = value.content_objects[&object_id]
        .original_size
        .checked_sub(1)
        .unwrap();
    let NodeDescriptorKindV2::File { original_size, .. } =
        &mut value.nodes.get_mut(&file_id).unwrap().descriptor.kind
    else {
        panic!("expected legacy file node");
    };
    *original_size = declared_size;
    let object = value.content_objects.get_mut(&object_id).unwrap();
    object.original_size = declared_size;
    let StorageRef::Legacy(storage) = &mut object.storage else {
        panic!("expected legacy storage");
    };
    assert_eq!(storage.chunks.len(), 1);
    storage.chunks[0].original_size = declared_size;
    write_catalog_v2(&catalog, &key, &value);

    let result = catalog.restore(
        CatalogSelection::Node(file_id),
        &key,
        RestoreOptions {
            output_dir: restore.clone(),
            conflict_policy: RestoreConflictPolicy::Rename,
        },
    );

    let Err(LiosError::DataCorruption(message)) = result else {
        panic!("expected declared-size rejection");
    };
    assert!(message.contains("legacy chunk exceeds declared size"));
    assert!(read_all_files(&restore).is_empty());
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
            staging_dir: staging.clone(),
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
fn corrupted_restore_leaves_no_final_or_partial_file() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    write_file(&source, b"0123456789abcdef");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let chunk = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .find(|file| file.path.ends_with(".lios"))
        .unwrap();
    let chunk_path = staging.join(chunk.path);
    let mut corrupted = fs::read(&chunk_path).unwrap();
    corrupted[0] ^= 0xff;
    fs::write(&chunk_path, corrupted).unwrap();

    let result = catalog.restore(
        CatalogSelection::All,
        &key,
        RestoreOptions {
            output_dir: restore.clone(),
            conflict_policy: RestoreConflictPolicy::Rename,
        },
    );

    assert!(result.is_err());
    assert!(!restore.join("source.bin").exists());
    let partials = fs::read_dir(&restore)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".lios-part"))
        .collect::<Vec<_>>();
    assert!(
        partials.is_empty(),
        "partial restore files remain: {partials:?}"
    );
}

#[test]
fn whole_file_hash_mismatch_leaves_no_final_or_partial_file() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    write_file(&source, b"0123456789abcdef");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();

    let mut value = read_catalog_v2(&catalog, &key);
    let root_id = value.root_id.clone();
    let NodeDescriptorKindV2::File {
        object_id,
        content_sha256,
        ..
    } = &mut value.nodes.get_mut(&root_id).unwrap().descriptor.kind
    else {
        panic!("expected file root");
    };
    let object_id = object_id.clone();
    let original_sha256 = content_sha256.clone();
    let wrong_sha256 = "00".repeat(32);
    *content_sha256 = wrong_sha256.clone();
    let object = value.content_objects.get_mut(&object_id).unwrap();
    object.content_sha256 = wrong_sha256.clone();
    value.content_index.remove(&original_sha256);
    value
        .content_index
        .insert(wrong_sha256.clone(), object_id.clone());
    let StorageRef::V2(storage) = &mut object.storage else {
        panic!("expected v2 storage");
    };
    let manifest_path = staging.join(&storage.manifest_path);
    let manifest_encrypted = fs::read(&manifest_path).unwrap();
    let manifest_plaintext =
        decrypt_envelope_v2(&key, EnvelopeKindV2::Manifest, &manifest_encrypted).unwrap();
    let mut manifest: ObjectManifestV2 = serde_json::from_slice(&manifest_plaintext).unwrap();
    manifest.content_sha256 = wrong_sha256;
    let manifest_encrypted = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Manifest,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    storage.manifest_encrypted_sha256 = hex::encode(Sha256::digest(&manifest_encrypted));
    fs::write(&manifest_path, manifest_encrypted).unwrap();
    write_catalog_v2(&catalog, &key, &value);

    let result = catalog.restore(
        CatalogSelection::All,
        &key,
        RestoreOptions {
            output_dir: restore.clone(),
            conflict_policy: RestoreConflictPolicy::Rename,
        },
    );

    assert!(matches!(result, Err(LiosError::Crypto)));
    assert!(!restore.join("source.bin").exists());
    let partials = fs::read_dir(&restore)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| name.ends_with(".lios-part"))
        .collect::<Vec<_>>();
    assert!(
        partials.is_empty(),
        "partial restore files remain: {partials:?}"
    );
}

#[test]
fn legacy_invalid_logical_name_restores_to_deterministic_safe_local_name() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.txt");
    let staging = tmp.path().join("staging");
    let first_restore = tmp.path().join("first-restore");
    let second_restore = tmp.path().join("second-restore");
    write_file(&source, b"legacy contents");

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
    let legacy_name = "CON:legacy?.txt";
    let mut value = read_catalog_v2(&catalog, &key);
    value
        .nodes
        .get_mut(&value.root_id.clone())
        .unwrap()
        .descriptor
        .name = legacy_name.to_string();
    write_catalog_v2(&catalog, &key, &value);

    assert_eq!(catalog.decrypt_tree(&key).unwrap().name, legacy_name);
    for output_dir in [&first_restore, &second_restore] {
        catalog
            .restore(
                CatalogSelection::All,
                &key,
                RestoreOptions {
                    output_dir: output_dir.clone(),
                    conflict_policy: RestoreConflictPolicy::Rename,
                },
            )
            .unwrap();
    }

    let first = read_all_files(&first_restore);
    let second = read_all_files(&second_restore);
    assert_eq!(first, second);
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].1, b"legacy contents");
    let local_name = &first[0].0;
    assert_ne!(local_name, legacy_name);
    assert!(local_name.contains("legacy"));
    assert!(!local_name
        .chars()
        .any(|character| character <= '\u{1f}' || ":*?\"<>|/\\".contains(character)));
    assert!(!local_name.ends_with([' ', '.']));
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
fn immutable_manifest_is_reused_or_abandoned_but_never_replaced() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let third = tmp.path().join("third.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"same bytes");
    write_file(&duplicate, b"same bytes");
    write_file(&third, b"same bytes");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let options = PackOptions {
        chunk_size: 4,
        staging_dir: staging.clone(),
    };
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog
        .add_paths_to_folder(&root_id, &[source], &[], &key, options.clone())
        .unwrap();
    let manifest = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .find(|file| file.path.ends_with("/manifest.enc"))
        .unwrap();
    let original_manifest_path = manifest.path;
    let manifest_path = staging.join(&original_manifest_path);
    let original_manifest = fs::read(&manifest_path).unwrap();

    catalog
        .add_paths_to_folder(&root_id, &[duplicate], &[], &key, options.clone())
        .unwrap();
    assert_eq!(fs::read(&manifest_path).unwrap(), original_manifest);

    fs::write(&manifest_path, b"conflicting manifest").unwrap();
    catalog
        .add_paths_to_folder(&root_id, &[third], &[], &key, options)
        .unwrap();

    assert_eq!(fs::read(&manifest_path).unwrap(), b"conflicting manifest");
    let repaired = read_catalog_v2(&catalog, &key);
    assert_eq!(repaired.content_objects.len(), 1);
    let replacement = repaired.content_objects.values().next().unwrap();
    let StorageRef::V2(storage) = &replacement.storage else {
        panic!("expected v2 replacement storage");
    };
    assert_ne!(storage.manifest_path, original_manifest_path);
    assert!(staging.join(&storage.manifest_path).is_file());
    for node in repaired.nodes.values() {
        if let NodeDescriptorKindV2::File { object_id, .. } = &node.descriptor.kind {
            assert_eq!(object_id, &replacement.object_id);
        }
    }
}

#[test]
fn corrupted_reused_chunk_falls_back_without_overwriting_staged_bytes() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"same bytes across packs");
    write_file(&duplicate, b"same bytes across packs");

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let options = PackOptions {
        chunk_size: 4,
        staging_dir: staging.clone(),
    };
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog
        .add_paths_to_folder(&root_id, &[source], &[], &key, options.clone())
        .unwrap();
    let original_object_id = read_catalog_v2(&catalog, &key)
        .content_objects
        .keys()
        .next()
        .unwrap()
        .clone();
    let catalog_path = catalog.encrypted_catalog_path().to_path_buf();
    let original_catalog = fs::read(&catalog_path).unwrap();
    let chunk = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .find(|file| file.path.ends_with(".lios"))
        .unwrap();
    let chunk_path = staging.join(chunk.path);
    let mut corrupted_chunk = fs::read(&chunk_path).unwrap();
    corrupted_chunk[0] ^= 0xff;
    fs::write(&chunk_path, &corrupted_chunk).unwrap();

    catalog
        .add_paths_to_folder(&root_id, &[duplicate], &[], &key, options)
        .unwrap();

    assert_ne!(fs::read(&catalog_path).unwrap(), original_catalog);
    assert_eq!(fs::read(&chunk_path).unwrap(), corrupted_chunk);
    let repaired = read_catalog_v2(&catalog, &key);
    assert_eq!(repaired.content_objects.len(), 1);
    let replacement = repaired.content_objects.values().next().unwrap();
    assert_ne!(replacement.object_id, original_object_id);
    for node in repaired.nodes.values() {
        if let NodeDescriptorKindV2::File { object_id, .. } = &node.descriptor.kind {
            assert_eq!(object_id, &replacement.object_id);
        }
    }
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
fn file_restore_rejects_link_at_final_output_path() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    let outside = tmp.path().join("outside");
    write_file(&source, b"secret");
    fs::create_dir_all(&restore).unwrap();

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
    create_broken_file_redirection(&outside, &restore.join("source.bin"));

    let result = catalog.restore(
        CatalogSelection::All,
        &key,
        RestoreOptions {
            output_dir: restore,
            conflict_policy: RestoreConflictPolicy::Rename,
        },
    );

    assert_restore_link_error(result);
    assert!(!outside.join("source.bin").exists());
}

#[test]
fn directory_restore_rejects_linked_descendant_without_writing_outside_root() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("album");
    let staging = tmp.path().join("staging");
    let restore = tmp.path().join("restore");
    let outside = tmp.path().join("outside");
    write_file(&source.join("nested/secret.txt"), b"secret");
    fs::create_dir_all(restore.join("album")).unwrap();
    fs::create_dir_all(&outside).unwrap();

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
    create_directory_link(&outside, &restore.join("album").join("nested"));

    let result = catalog.restore(
        CatalogSelection::All,
        &key,
        RestoreOptions {
            output_dir: restore,
            conflict_policy: RestoreConflictPolicy::Rename,
        },
    );

    assert_restore_link_error(result);
    assert!(!outside.join("secret.txt").exists());
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
fn fresh_spaces_randomize_chunk_ciphertext_under_file_object_directories() {
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
    assert!(first_chunks.iter().all(|first| {
        second_chunks
            .iter()
            .all(|second| first.path != second.path && first.sha256 != second.sha256)
    }));
    assert!(first_chunks
        .iter()
        .chain(second_chunks.iter())
        .all(|file| file.path.starts_with("objects/files/") && file.path.contains("/chunks/")));
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) {
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

#[cfg(unix)]
fn create_broken_file_redirection(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn create_broken_file_redirection(target: &Path, link: &Path) {
    fs::create_dir_all(target).unwrap();
    create_directory_link(target, link);
    fs::remove_dir_all(target).unwrap();
}
