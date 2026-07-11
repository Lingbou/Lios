use std::fs;
use std::path::Path;

use lios_core::{
    catalog::{
        Catalog, CatalogIntegrityOutcome, CatalogSelection, CatalogTreeNodeKind, CatalogV2,
        NodeDescriptorKindV2, ObjectManifestV2, StorageRef,
    },
    crypto::KeyFile,
    format_v2::{decrypt_envelope_v2, encrypt_envelope_v2, EnvelopeKindV2},
    pack::{PackOptions, PackProgress, PackSource},
    restore::{RestoreConflictPolicy, RestoreOptions},
    storage::StorageObject,
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

fn tamper_file(path: &Path) {
    let mut encoded = fs::read(path).unwrap();
    let midpoint = encoded.len() / 2;
    encoded[midpoint] ^= 0x40;
    fs::write(path, encoded).unwrap();
}

fn staged_remote_inventory(catalog: &Catalog, key: &KeyFile) -> Vec<StorageObject> {
    let staging = catalog.encrypted_catalog_path().parent().unwrap();
    let mut remote = catalog
        .remote_files_for_selection(&CatalogSelection::All, key)
        .unwrap()
        .into_iter()
        .map(|file| {
            let local_path = staging.join(&file.path);
            let bytes = fs::read(&local_path).unwrap();
            assert_eq!(file.expected_size, Some(bytes.len() as u64));
            StorageObject {
                path: file.path,
                size: bytes.len() as u64,
                sha256: Some(hex::encode(Sha256::digest(bytes))),
            }
        })
        .collect::<Vec<_>>();
    let catalog_bytes = fs::read(catalog.encrypted_catalog_path()).unwrap();
    remote.push(StorageObject {
        path: "catalog.enc".to_string(),
        size: catalog_bytes.len() as u64,
        sha256: Some(hex::encode(Sha256::digest(catalog_bytes))),
    });
    remote
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
fn full_integrity_check_authenticates_descriptors_manifests_chunks_and_whole_files() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    let data = (0..4096).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    write_file(&source, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 257,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();

    let report = catalog.verify_staged_integrity(&key).unwrap();
    let expected_encoded_bytes = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .map(|file| fs::metadata(staging.join(file.path)).unwrap().len())
        .sum::<u64>();

    assert_eq!(report.nodes_verified, 1);
    assert_eq!(report.objects_verified, 1);
    assert_eq!(report.chunks_verified, 16);
    assert_eq!(report.original_bytes_verified, data.len() as u64);
    assert_eq!(report.encoded_bytes_verified, expected_encoded_bytes);
}

#[test]
fn full_integrity_check_rejects_authenticated_object_corruption() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let value = read_catalog_v2(&catalog, &key);
    let chunk_path = value
        .content_objects
        .values()
        .find_map(|object| match &object.storage {
            StorageRef::V2(storage) => storage.chunks.first().map(|chunk| chunk.path.clone()),
            StorageRef::Legacy(_) => None,
        })
        .unwrap();
    tamper_file(&staging.join(chunk_path));

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn full_integrity_check_rejects_authenticated_manifest_corruption() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let value = read_catalog_v2(&catalog, &key);
    let manifest_path = value
        .content_objects
        .values()
        .find_map(|object| match &object.storage {
            StorageRef::V2(storage) => Some(storage.manifest_path.clone()),
            StorageRef::Legacy(_) => None,
        })
        .unwrap();
    tamper_file(&staging.join(manifest_path));

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn full_integrity_check_rejects_authenticated_descriptor_corruption() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let value = read_catalog_v2(&catalog, &key);
    let descriptor_path = staging
        .join("recovery/nodes")
        .join(format!("{}.enc", value.root_id));
    tamper_file(&descriptor_path);

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn full_integrity_check_rejects_missing_native_v2_descriptor_hash() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    value
        .nodes
        .get_mut(&value.root_id.clone())
        .unwrap()
        .descriptor_encrypted_sha256 = None;
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn quick_inventory_enumeration_rejects_missing_native_v2_descriptor_hash() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    value
        .nodes
        .get_mut(&value.root_id.clone())
        .unwrap()
        .descriptor_encrypted_sha256 = None;
    write_catalog_v2(&catalog, &key, &value);

    assert!(matches!(
        catalog.remote_files_for_selection(&CatalogSelection::All, &key),
        Err(LiosError::DataCorruption(_))
    ));
}

#[test]
fn full_integrity_check_authenticates_migrated_legacy_manifest() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    let chunk_path = staging.join("objects/files/legacy-object/chunks/golden.lios");
    let manifest_path = staging.join("objects/files/legacy-object/manifest.enc");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(
        &chunk_path,
        &fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let manifest = serde_json::json!({
        "version": 1,
        "object_id": "legacy-object",
        "chunks": [{
            "index": 0,
            "path": "objects/files/legacy-object/chunks/golden.lios",
            "original_size": 78,
            "original_sha256": "c79c31459aceec53b672359d1fbb98381afee6e0d72c9c142e807bad6d07b0cb",
            "encrypted_sha256": "41e43900b34eba5f8a06c54c8e3eed0a1b231bbfe600f0b3810b3045ea01ac4c"
        }]
    });
    let encrypted_manifest = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Manifest,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    write_file(&manifest_path, &encrypted_manifest);
    let catalog = Catalog::from_staging(staging);

    let report = catalog.verify_staged_integrity(&key).unwrap();

    assert_eq!(report.nodes_verified, 0);
    assert_eq!(report.objects_verified, 1);
    assert_eq!(report.chunks_verified, 1);
    assert_eq!(
        report.encoded_bytes_verified,
        encrypted_manifest.len() as u64 + fs::metadata(chunk_path).unwrap().len()
    );
}

#[test]
fn full_integrity_check_rejects_migrated_legacy_manifest_mismatch() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(
        &staging.join("objects/files/legacy-object/chunks/golden.lios"),
        &fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let manifest = serde_json::json!({
        "version": 1,
        "object_id": "wrong-object",
        "chunks": []
    });
    let encrypted_manifest = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Manifest,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    write_file(
        &staging.join("objects/files/legacy-object/manifest.enc"),
        &encrypted_manifest,
    );
    let catalog = Catalog::from_staging(staging);

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn full_integrity_check_rejects_linked_staging_ancestor() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let linked_target = tmp.path().join("linked-recovery");
    fs::rename(staging.join("recovery"), &linked_target).unwrap();
    create_directory_link(&linked_target, &staging.join("recovery"));

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn full_integrity_check_can_cancel_before_scanning_objects() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"cancel integrity payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();

    let outcome = catalog
        .verify_staged_integrity_with_cancel(&key, || true)
        .unwrap();

    assert_eq!(
        outcome,
        CatalogIntegrityOutcome::Canceled(Default::default())
    );
}

#[test]
fn remote_inventory_check_authenticates_current_references_and_reports_stale_objects() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"remote inventory payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();
    let mut remote = staged_remote_inventory(&catalog, &key);
    remote.push(StorageObject {
        path: "objects/files/stale/chunks/old.lios".to_string(),
        size: 9,
        sha256: Some("stale-oid".to_string()),
    });

    let report = catalog.verify_remote_inventory(&key, &remote).unwrap();

    assert_eq!(report.expected_objects, remote.len() as u64 - 1);
    assert_eq!(report.verified_objects, report.expected_objects);
    assert_eq!(report.unreferenced_managed_objects, 1);
    assert_eq!(
        report.encoded_bytes_verified,
        remote[..remote.len() - 1]
            .iter()
            .map(|object| object.size)
            .sum::<u64>()
    );
}

#[test]
fn remote_inventory_check_rejects_missing_referenced_object() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"remote inventory payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();
    let mut remote = staged_remote_inventory(&catalog, &key);
    remote.retain(|object| !object.path.ends_with(".lios"));

    assert!(matches!(
        catalog.verify_remote_inventory(&key, &remote),
        Err(LiosError::DataCorruption(_))
    ));
}

#[test]
fn remote_inventory_check_rejects_size_and_lfs_oid_mismatch() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    let staging = tmp.path().join("staging");
    write_file(&source, b"remote inventory payload");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    )
    .unwrap();
    let remote = staged_remote_inventory(&catalog, &key);
    let mut wrong_size = remote.clone();
    wrong_size[0].size = wrong_size[0].size.saturating_add(1);
    let mut wrong_oid = remote.clone();
    wrong_oid[0].sha256 = Some("wrong-oid".to_string());

    assert!(matches!(
        catalog.verify_remote_inventory(&key, &wrong_size),
        Err(LiosError::DataCorruption(_))
    ));
    assert!(matches!(
        catalog.verify_remote_inventory(&key, &wrong_oid),
        Err(LiosError::DataCorruption(_))
    ));
}

#[test]
fn legacy_remote_inventory_uses_historical_manifest_encoding() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging.clone());
    let expected = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();
    let manifest = expected
        .iter()
        .find(|file| file.path.ends_with("/manifest.enc"))
        .unwrap();
    let chunk = expected
        .iter()
        .find(|file| file.path.ends_with(".lios"))
        .unwrap();
    assert_eq!(manifest.expected_size, Some(338));
    assert_eq!(
        manifest.sha256.as_deref(),
        Some("e323494296cd2dfee9c0c684f876712cde09473d4d8703805f95c9e689b303b8")
    );
    assert_eq!(chunk.expected_size, None);

    let mut remote = expected
        .into_iter()
        .map(|file| StorageObject {
            path: file.path,
            size: file.expected_size.unwrap_or(106),
            sha256: file.sha256,
        })
        .collect::<Vec<_>>();
    let catalog_bytes = fs::read(staging.join("catalog.enc")).unwrap();
    remote.push(StorageObject {
        path: "catalog.enc".to_string(),
        size: catalog_bytes.len() as u64,
        sha256: Some(hex::encode(Sha256::digest(catalog_bytes))),
    });

    let report = catalog.verify_remote_inventory(&key, &remote).unwrap();

    assert_eq!(report.metadata_limited_objects, 1);
    assert_eq!(
        report.verified_objects + report.metadata_limited_objects,
        report.expected_objects
    );
}

#[test]
fn catalog_rejects_legacy_manifest_chunk_path_alias() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging);
    catalog
        .create_folder("legacy-root", "migration-marker", &key)
        .unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    let object = value.content_objects.get_mut("legacy-object").unwrap();
    let StorageRef::Legacy(storage) = &mut object.storage else {
        panic!("expected legacy storage");
    };
    storage.chunks[0].path = storage.manifest_path.clone();
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog.decrypt_tree(&key).is_err());
}

#[test]
fn full_integrity_check_rejects_catalog_read_through_linked_staging_root() {
    let tmp = tempdir().unwrap();
    let real_staging = tmp.path().join("real-staging");
    let linked_staging = tmp.path().join("linked-staging");
    fs::create_dir_all(&real_staging).unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let legacy_empty = serde_json::json!({
        "version": 1,
        "root": {
            "id": "legacy-root",
            "name": "legacy-root",
            "updated_at": "2026-01-01T00:00:00Z",
            "kind": { "type": "Directory", "children": [] }
        }
    });
    let encrypted_catalog = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Catalog,
        &serde_json::to_vec(&legacy_empty).unwrap(),
    )
    .unwrap();
    write_file(&real_staging.join("catalog.enc"), &encrypted_catalog);
    create_directory_link(&real_staging, &linked_staging);
    let catalog = Catalog::from_staging(linked_staging);

    assert!(catalog.verify_staged_integrity(&key).is_err());
}

#[test]
fn catalog_rejects_windows_equivalent_paths_shared_by_distinct_legacy_objects() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("lios.key")).unwrap();
    let shared_chunk = serde_json::json!({
        "index": 0,
        "path": "objects/files/shared/chunks/chunk.lios",
        "original_size": 1,
        "original_sha256": "source-sha",
        "encrypted_sha256": "encoded-sha"
    });
    let shared_chunk_case_variant = serde_json::json!({
        "index": 0,
        "path": "objects/files/shared/chunks/CHUNK.lios",
        "original_size": 1,
        "original_sha256": "source-sha",
        "encrypted_sha256": "encoded-sha"
    });
    let legacy_catalog = serde_json::json!({
        "version": 1,
        "root": {
            "id": "legacy-root",
            "name": "legacy-root",
            "updated_at": "2026-01-01T00:00:00Z",
            "kind": {
                "type": "Directory",
                "children": [
                    {
                        "id": "file-a",
                        "name": "a.bin",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "kind": {
                            "type": "File",
                            "original_size": 1,
                            "sha256": "file-a-sha",
                            "object_id": "object-a",
                            "chunks": [shared_chunk.clone()]
                        }
                    },
                    {
                        "id": "file-b",
                        "name": "b.bin",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "kind": {
                            "type": "File",
                            "original_size": 1,
                            "sha256": "file-b-sha",
                            "object_id": "object-b",
                            "chunks": [shared_chunk_case_variant]
                        }
                    }
                ]
            }
        }
    });
    let encrypted_catalog = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Catalog,
        &serde_json::to_vec(&legacy_catalog).unwrap(),
    )
    .unwrap();
    write_file(&staging.join("catalog.enc"), &encrypted_catalog);
    let catalog = Catalog::from_staging(staging);

    assert!(catalog.decrypt_tree(&key).is_err());
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
