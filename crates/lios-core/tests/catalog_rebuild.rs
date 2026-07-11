use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use lios_core::{
    catalog::{
        Catalog, CatalogRebuildOutcome, CatalogRebuildReport, CatalogSelection, CatalogTreeNode,
        CatalogTreeNodeKind, NodeDescriptorV2, ObjectManifestV2,
    },
    crypto::KeyFile,
    format_v2::{decrypt_envelope_v2, encrypt_envelope_v2, parse_envelope_v2, EnvelopeKindV2},
    pack::{PackOptions, PackSource},
    storage::StorageObject,
};
use serde::{de::DeserializeOwned, Serialize};
use sha2::{Digest, Sha256};
use tempfile::{tempdir, TempDir};

const FILE_BYTES: &[u8] = b"catalog rebuild payload";

struct RecoveryFixture {
    _tmp: TempDir,
    staging: PathBuf,
    key: KeyFile,
    remote: Vec<StorageObject>,
    catalog_object: StorageObject,
    descriptor_paths: Vec<String>,
    manifest_paths: Vec<String>,
    chunk_paths: Vec<String>,
}

fn assert_public_serde_type<T: Serialize + DeserializeOwned>() {}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn storage_object(staging: &Path, path: String) -> StorageObject {
    let bytes = fs::read(staging.join(&path)).unwrap();
    StorageObject {
        path,
        size: bytes.len() as u64,
        sha256: Some(hex::encode(Sha256::digest(bytes))),
    }
}

fn recovery_fixture() -> RecoveryFixture {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("Secret Root");
    let nested = source.join("Nested Secret");
    fs::create_dir_all(nested.join("Empty Secret")).unwrap();
    write_file(&nested.join("hidden.txt"), FILE_BYTES);

    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("recovery.key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 7,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let catalog_object = storage_object(&staging, "catalog.enc".to_string());
    let remote = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .into_iter()
        .map(|file| storage_object(&staging, file.path))
        .collect::<Vec<_>>();
    let descriptor_paths = remote
        .iter()
        .filter(|object| object.path.starts_with("recovery/nodes/"))
        .map(|object| object.path.clone())
        .collect();
    let manifest_paths = remote
        .iter()
        .filter(|object| object.path.ends_with("/manifest.enc"))
        .map(|object| object.path.clone())
        .collect();
    let chunk_paths = remote
        .iter()
        .filter(|object| object.path.ends_with(".lios"))
        .map(|object| object.path.clone())
        .collect();
    fs::remove_file(catalog.encrypted_catalog_path()).unwrap();

    RecoveryFixture {
        _tmp: tmp,
        staging,
        key,
        remote,
        catalog_object,
        descriptor_paths,
        manifest_paths,
        chunk_paths,
    }
}

fn descriptor_path_named(fixture: &RecoveryFixture, name: &str) -> String {
    fixture
        .descriptor_paths
        .iter()
        .find(|path| {
            let encrypted = fs::read(fixture.staging.join(path)).unwrap();
            let plaintext =
                decrypt_envelope_v2(&fixture.key, EnvelopeKindV2::NodeDescriptor, &encrypted)
                    .unwrap();
            serde_json::from_slice::<NodeDescriptorV2>(&plaintext)
                .unwrap()
                .name
                == name
        })
        .unwrap()
        .clone()
}

fn refresh_remote_object(fixture: &mut RecoveryFixture, path: &str) {
    let refreshed = storage_object(&fixture.staging, path.to_string());
    let remote = fixture
        .remote
        .iter_mut()
        .find(|object| object.path == path)
        .unwrap();
    *remote = refreshed;
}

fn rewrite_descriptor(
    fixture: &mut RecoveryFixture,
    path: &str,
    mutate: impl FnOnce(&mut NodeDescriptorV2),
) {
    let encrypted = fs::read(fixture.staging.join(path)).unwrap();
    let plaintext =
        decrypt_envelope_v2(&fixture.key, EnvelopeKindV2::NodeDescriptor, &encrypted).unwrap();
    let mut descriptor: NodeDescriptorV2 = serde_json::from_slice(&plaintext).unwrap();
    mutate(&mut descriptor);
    let encrypted = encrypt_envelope_v2(
        &fixture.key,
        EnvelopeKindV2::NodeDescriptor,
        &serde_json::to_vec(&descriptor).unwrap(),
    )
    .unwrap();
    fs::write(fixture.staging.join(path), encrypted).unwrap();
    refresh_remote_object(fixture, path);
}

fn rewrite_manifest(
    fixture: &mut RecoveryFixture,
    path: &str,
    mutate: impl FnOnce(&mut ObjectManifestV2),
) {
    let encrypted = fs::read(fixture.staging.join(path)).unwrap();
    let plaintext =
        decrypt_envelope_v2(&fixture.key, EnvelopeKindV2::Manifest, &encrypted).unwrap();
    let mut manifest: ObjectManifestV2 = serde_json::from_slice(&plaintext).unwrap();
    mutate(&mut manifest);
    let encrypted = encrypt_envelope_v2(
        &fixture.key,
        EnvelopeKindV2::Manifest,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    fs::write(fixture.staging.join(path), encrypted).unwrap();
    refresh_remote_object(fixture, path);
}

fn tamper_file(path: &Path) {
    let mut bytes = fs::read(path).unwrap();
    let last = bytes.last_mut().unwrap();
    *last ^= 0x80;
    fs::write(path, bytes).unwrap();
}

fn metadata_bytes(fixture: &RecoveryFixture) -> BTreeMap<String, Vec<u8>> {
    fixture
        .descriptor_paths
        .iter()
        .chain(&fixture.manifest_paths)
        .map(|path| (path.clone(), fs::read(fixture.staging.join(path)).unwrap()))
        .collect()
}

fn collect_names(node: &CatalogTreeNode, names: &mut Vec<String>) {
    names.push(node.name.clone());
    if let CatalogTreeNodeKind::Directory { children } = &node.kind {
        for child in children {
            collect_names(child, names);
        }
    }
}

#[test]
fn rebuilds_nested_catalog_reports_counts_and_preserves_recovered_metadata() {
    assert_public_serde_type::<CatalogRebuildReport>();
    let mut fixture = recovery_fixture();
    let metadata_before = metadata_bytes(&fixture);
    fixture.remote.push(StorageObject {
        path: format!(
            "objects/files/{}/chunks/{}.lios",
            "f".repeat(32),
            "e".repeat(64)
        ),
        size: 11,
        sha256: Some("d".repeat(64)),
    });

    let (catalog, report) =
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).unwrap();

    let tree = catalog.decrypt_tree(&fixture.key).unwrap();
    let mut names = Vec::new();
    collect_names(&tree, &mut names);
    names.sort();
    assert_eq!(
        names,
        ["Empty Secret", "Nested Secret", "Secret Root", "hidden.txt"]
    );
    assert_eq!(report.nodes_rebuilt, 4);
    assert_eq!(report.directories_rebuilt, 3);
    assert_eq!(report.files_rebuilt, 1);
    assert_eq!(report.content_objects_rebuilt, 1);
    assert_eq!(report.chunks_referenced, fixture.chunk_paths.len() as u64);
    assert_eq!(report.original_bytes_referenced, FILE_BYTES.len() as u64);
    assert_eq!(report.unreferenced_managed_objects, 1);
    assert_eq!(metadata_bytes(&fixture), metadata_before);

    let encrypted_catalog = fs::read(catalog.encrypted_catalog_path()).unwrap();
    assert_eq!(
        parse_envelope_v2(&encrypted_catalog).unwrap().kind,
        EnvelopeKindV2::Catalog
    );
    let ciphertext = String::from_utf8_lossy(&encrypted_catalog);
    for name in ["Secret Root", "Nested Secret", "Empty Secret", "hidden.txt"] {
        assert!(!ciphertext.contains(name));
    }
}

#[test]
fn refuses_catalog_inventory_and_requires_descriptors() {
    let mut fixture = recovery_fixture();
    fixture.remote.push(fixture.catalog_object.clone());
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let fixture = recovery_fixture();
    let without_descriptors = fixture
        .remote
        .iter()
        .filter(|object| !object.path.starts_with("recovery/nodes/"))
        .cloned()
        .collect::<Vec<_>>();
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &without_descriptors)
            .is_err()
    );
}

#[test]
fn refuses_to_overwrite_an_existing_local_catalog() {
    let fixture = recovery_fixture();
    let catalog_path = fixture.staging.join("catalog.enc");
    let existing = b"existing local catalog";
    fs::write(&catalog_path, existing).unwrap();

    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );
    assert_eq!(fs::read(catalog_path).unwrap(), existing);
}

#[test]
fn rejects_missing_or_tampered_descriptor() {
    let fixture = recovery_fixture();
    fs::remove_file(fixture.staging.join(&fixture.descriptor_paths[0])).unwrap();
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let fixture = recovery_fixture();
    tamper_file(&fixture.staging.join(&fixture.descriptor_paths[0]));
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );
}

#[test]
fn rejects_invalid_descriptor_path_id_hash_size_envelope_and_version() {
    let mut fixture = recovery_fixture();
    fixture.remote[0].path = "recovery/nodes/not-a-node.enc".to_string();
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = fixture.descriptor_paths[0].clone();
    rewrite_descriptor(&mut fixture, &path, |descriptor| {
        descriptor.node_id = "a".repeat(32);
    });
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let descriptor = fixture
        .remote
        .iter_mut()
        .find(|object| object.path.starts_with("recovery/nodes/"))
        .unwrap();
    descriptor.sha256 = Some("0".repeat(64));
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let descriptor = fixture
        .remote
        .iter_mut()
        .find(|object| object.path.starts_with("recovery/nodes/"))
        .unwrap();
    descriptor.size += 1;
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = fixture.descriptor_paths[0].clone();
    let encrypted = fs::read(fixture.staging.join(&path)).unwrap();
    let plaintext =
        decrypt_envelope_v2(&fixture.key, EnvelopeKindV2::NodeDescriptor, &encrypted).unwrap();
    let wrong_envelope =
        encrypt_envelope_v2(&fixture.key, EnvelopeKindV2::Manifest, &plaintext).unwrap();
    fs::write(fixture.staging.join(&path), wrong_envelope).unwrap();
    refresh_remote_object(&mut fixture, &path);
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = fixture.descriptor_paths[0].clone();
    rewrite_descriptor(&mut fixture, &path, |descriptor| descriptor.version = 1);
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );
}

#[test]
fn rejects_orphan_second_root_and_windows_sibling_collision() {
    let mut fixture = recovery_fixture();
    let path = descriptor_path_named(&fixture, "Nested Secret");
    rewrite_descriptor(&mut fixture, &path, |descriptor| {
        descriptor.parent_id = Some("a".repeat(32));
    });
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = descriptor_path_named(&fixture, "Empty Secret");
    rewrite_descriptor(&mut fixture, &path, |descriptor| {
        descriptor.parent_id = None
    });
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = descriptor_path_named(&fixture, "Empty Secret");
    rewrite_descriptor(&mut fixture, &path, |descriptor| {
        descriptor.name = "HIDDEN.TXT".to_string();
    });
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );
}

#[test]
fn rejects_missing_tampered_or_mismatched_manifest() {
    let fixture = recovery_fixture();
    fs::remove_file(fixture.staging.join(&fixture.manifest_paths[0])).unwrap();
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let fixture = recovery_fixture();
    tamper_file(&fixture.staging.join(&fixture.manifest_paths[0]));
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = fixture.manifest_paths[0].clone();
    rewrite_manifest(&mut fixture, &path, |manifest| {
        manifest.original_size += 1;
    });
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );
}

#[test]
fn rejects_missing_or_tampered_chunk_inventory() {
    let mut fixture = recovery_fixture();
    let missing = fixture.chunk_paths[0].clone();
    fixture.remote.retain(|object| object.path != missing);
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = fixture.chunk_paths[0].clone();
    let chunk = fixture
        .remote
        .iter_mut()
        .find(|object| object.path == path)
        .unwrap();
    chunk.sha256 = Some("0".repeat(64));
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );

    let mut fixture = recovery_fixture();
    let path = fixture.chunk_paths[0].clone();
    let chunk = fixture
        .remote
        .iter_mut()
        .find(|object| object.path == path)
        .unwrap();
    chunk.size += 1;
    assert!(
        Catalog::rebuild_from_recovery(&fixture.key, &fixture.staging, &fixture.remote).is_err()
    );
}

#[test]
fn canceled_rebuild_does_not_publish_a_local_catalog() {
    let fixture = recovery_fixture();
    let mut checks = 0usize;

    let outcome = Catalog::rebuild_from_recovery_with_cancel(
        &fixture.key,
        &fixture.staging,
        &fixture.remote,
        || {
            checks += 1;
            checks > 1
        },
    )
    .unwrap();

    assert!(matches!(outcome, CatalogRebuildOutcome::Canceled));
    assert!(!fixture.staging.join("catalog.enc").exists());
}
