use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use base64::{engine::general_purpose::STANDARD, Engine};
use chacha20poly1305::{
    aead::{Aead, AeadCore},
    ChaCha20Poly1305, Key, KeyInit,
};
use lios_core::{
    catalog::{
        Catalog, CatalogNodeV2, CatalogSelection, CatalogTreeNodeKind, CatalogV2, ContentObject,
        LegacyChunkRef, LegacyStorageRef, NodeDescriptorKindV2, NodeDescriptorV2, ObjectManifestV2,
        StorageRef, V2ChunkRef, V2StorageRef,
    },
    crypto::KeyFile,
    format_v2::{decrypt_envelope_v2, encrypt_envelope_v2, parse_envelope_v2, EnvelopeKindV2},
    pack::{PackOptions, PackSource},
    restore::{RestoreConflictPolicy, RestoreOptions},
    storage::StorageObject,
};
use serde::{de::DeserializeOwned, Serialize};
use sha2::Digest;
use tempfile::tempdir;

#[derive(serde::Deserialize)]
struct TestKeyFile {
    key: Option<String>,
    master_key: Option<String>,
}

fn assert_public_serde_type<T: Serialize + DeserializeOwned>() {}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

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

fn encrypt_v1_catalog(key_path: &Path, value: &serde_json::Value) -> Vec<u8> {
    let key_file: TestKeyFile =
        serde_yaml::from_str(&fs::read_to_string(key_path).unwrap()).unwrap();
    let encoded = key_file.key.or(key_file.master_key).unwrap();
    let key: [u8; 32] = STANDARD.decode(encoded).unwrap().try_into().unwrap();
    let cipher = ChaCha20Poly1305::new(Key::from_slice(&key));
    let nonce = ChaCha20Poly1305::generate_nonce(&mut rand::rngs::OsRng);
    let plaintext = serde_json::to_vec(value).unwrap();
    let ciphertext = cipher.encrypt(&nonce, plaintext.as_slice()).unwrap();
    let mut encrypted = nonce.to_vec();
    encrypted.extend_from_slice(&ciphertext);
    encrypted
}

fn descriptor_hashes(catalog: &CatalogV2) -> BTreeMap<String, String> {
    catalog
        .nodes
        .iter()
        .map(|(id, node)| {
            (
                id.clone(),
                node.descriptor_encrypted_sha256.clone().unwrap(),
            )
        })
        .collect()
}

fn insert_test_directory(
    catalog: &mut CatalogV2,
    id: String,
    parent_id: Option<String>,
    name: impl Into<String>,
) {
    catalog.nodes.insert(
        id.clone(),
        CatalogNodeV2 {
            descriptor: NodeDescriptorV2 {
                version: 2,
                node_id: id,
                parent_id,
                name: name.into(),
                updated_at: String::new(),
                kind: NodeDescriptorKindV2::Directory,
            },
            descriptor_encrypted_sha256: None,
        },
    );
}

fn deterministic_bytes(len: usize) -> Vec<u8> {
    let mut state = 0x1234_5678_9abc_def0u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect()
}

fn file_object_id(catalog: &Catalog, key: &KeyFile, name: &str) -> String {
    let tree = catalog.decrypt_tree(key).unwrap();
    if tree.name == name {
        let CatalogTreeNodeKind::File { object_id, .. } = tree.kind else {
            panic!("expected file root");
        };
        return object_id;
    }
    let CatalogTreeNodeKind::Directory { children } = tree.kind else {
        panic!("expected directory root or named file root");
    };
    let file = children.iter().find(|node| node.name == name).unwrap();
    let CatalogTreeNodeKind::File { object_id, .. } = &file.kind else {
        panic!("expected file node");
    };
    object_id.clone()
}

fn content_remote_inventory(
    catalog: &Catalog,
    key: &KeyFile,
    staging: &Path,
) -> Vec<StorageObject> {
    catalog
        .remote_files_for_selection(&CatalogSelection::All, key)
        .unwrap()
        .into_iter()
        .filter(|file| file.path.starts_with("objects/files/"))
        .map(|file| {
            let bytes = fs::read(staging.join(&file.path)).unwrap();
            StorageObject {
                path: file.path,
                size: bytes.len() as u64,
                sha256: Some(hex::encode(sha2::Sha256::digest(bytes))),
            }
        })
        .collect()
}

#[test]
fn public_v2_catalog_model_types_are_serde_capable() {
    assert_public_serde_type::<CatalogV2>();
    assert_public_serde_type::<ContentObject>();
    assert_public_serde_type::<StorageRef>();
    assert_public_serde_type::<LegacyStorageRef>();
    assert_public_serde_type::<LegacyChunkRef>();
    assert_public_serde_type::<V2StorageRef>();
    assert_public_serde_type::<V2ChunkRef>();
    assert_public_serde_type::<NodeDescriptorV2>();
    assert_public_serde_type::<ObjectManifestV2>();
}

#[test]
fn initialize_empty_writes_v2_catalog_and_root_descriptor() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("recovery.key")).unwrap();

    let catalog = Catalog::initialize_empty("Secret Space", &key, staging.clone()).unwrap();
    let encrypted_catalog = fs::read(catalog.encrypted_catalog_path()).unwrap();
    let catalog_metadata = parse_envelope_v2(&encrypted_catalog).unwrap();
    assert_eq!(catalog_metadata.kind, EnvelopeKindV2::Catalog);

    let plaintext = decrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, &encrypted_catalog).unwrap();
    let catalog_v2: CatalogV2 = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(catalog_v2.version, 2);
    assert_eq!(catalog_v2.nodes.len(), 1);
    let root = catalog_v2.nodes.get(&catalog_v2.root_id).unwrap();
    assert_eq!(root.descriptor.name, "Secret Space");
    let descriptor_sha256 = root.descriptor_encrypted_sha256.as_ref().unwrap();

    let descriptor_path = staging
        .join("recovery/nodes")
        .join(format!("{}.enc", catalog_v2.root_id));
    let encrypted_descriptor = fs::read(descriptor_path).unwrap();
    assert_eq!(
        hex::encode(sha2::Sha256::digest(&encrypted_descriptor)),
        *descriptor_sha256
    );
    assert_eq!(
        parse_envelope_v2(&encrypted_descriptor).unwrap().kind,
        EnvelopeKindV2::NodeDescriptor
    );
    let descriptor_plaintext =
        decrypt_envelope_v2(&key, EnvelopeKindV2::NodeDescriptor, &encrypted_descriptor).unwrap();
    let descriptor: NodeDescriptorV2 = serde_json::from_slice(&descriptor_plaintext).unwrap();
    assert_eq!(descriptor.node_id, catalog_v2.root_id);
    assert_eq!(descriptor.parent_id, None);
    assert_eq!(descriptor.name, "Secret Space");

    assert!(!String::from_utf8_lossy(&encrypted_catalog).contains("Secret Space"));
    assert!(!String::from_utf8_lossy(&encrypted_descriptor).contains("Secret Space"));
}

#[test]
fn golden_v1_read_is_non_mutating_and_first_metadata_change_upgrades_to_v2() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    let catalog_path = staging.join("catalog.enc");
    write_file(
        &catalog_path,
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(
        &staging.join("objects/files/legacy-object/chunks/golden.lios"),
        &fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap(),
    );
    let original_catalog = fs::read(&catalog_path).unwrap();
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging.clone());

    let tree = catalog.decrypt_tree(&key).unwrap();
    assert_eq!(tree.id, "legacy-root");
    assert_eq!(tree.name, "legacy-archive");
    assert_eq!(fs::read(&catalog_path).unwrap(), original_catalog);
    assert!(!staging.join("recovery/nodes").exists());

    catalog
        .create_folder("legacy-root", "new-empty", &key)
        .unwrap();

    assert_eq!(
        parse_envelope_v2(&fs::read(&catalog_path).unwrap())
            .unwrap()
            .kind,
        EnvelopeKindV2::Catalog
    );
    let upgraded = read_catalog_v2(&catalog, &key);
    assert_eq!(upgraded.version, 2);
    assert_eq!(upgraded.nodes.len(), 3);
    assert_ne!(upgraded.root_id, "legacy-root");
    assert!(upgraded
        .nodes
        .keys()
        .all(|id| id.len() == 32 && id.chars().all(|character| character.is_ascii_hexdigit())));
    assert!(upgraded.nodes.values().all(|node| node
        .descriptor_encrypted_sha256
        .as_ref()
        .is_some_and(|hash| hash.len() == 64)));

    let legacy = upgraded
        .content_objects
        .get("legacy-object")
        .expect("legacy content object");
    let StorageRef::Legacy(storage) = &legacy.storage else {
        panic!("expected legacy storage reference");
    };
    assert_eq!(
        storage.manifest_path,
        "objects/files/legacy-object/manifest.enc"
    );
    assert_eq!(
        storage.chunks[0].path,
        "objects/files/legacy-object/chunks/golden.lios"
    );
    assert_eq!(
        storage.chunks[0].encrypted_sha256,
        "41e43900b34eba5f8a06c54c8e3eed0a1b231bbfe600f0b3810b3045ea01ac4c"
    );

    let remote = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();
    assert!(remote
        .iter()
        .any(|file| file.path == "objects/files/legacy-object/manifest.enc"));
    assert!(remote
        .iter()
        .any(|file| file.path == "objects/files/legacy-object/chunks/golden.lios"));
    assert_eq!(
        remote
            .iter()
            .filter(|file| file.path.starts_with("recovery/nodes/"))
            .count(),
        3
    );
}

#[test]
fn unknown_plaintext_catalog_version_fails_explicitly() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    fs::create_dir_all(&staging).unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let encrypted = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Catalog,
        br#"{"version":99,"root_id":"root","nodes":{}}"#,
    )
    .unwrap();
    fs::write(staging.join("catalog.enc"), encrypted).unwrap();

    let error = Catalog::from_staging(staging)
        .decrypt_tree(&key)
        .unwrap_err();
    assert!(error.to_string().contains("unknown catalog version"));
}

#[test]
fn nested_and_empty_directories_get_descriptors_and_rename_only_rewrites_target() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog.create_folder(&root_id, "nested", &key).unwrap();
    let nested_id = match catalog.decrypt_tree(&key).unwrap().kind {
        CatalogTreeNodeKind::Directory { children } => children[0].id.clone(),
        CatalogTreeNodeKind::File { .. } => panic!("expected root directory"),
    };
    catalog.create_folder(&nested_id, "empty", &key).unwrap();

    let before = read_catalog_v2(&catalog, &key);
    let before_hashes = descriptor_hashes(&before);
    assert_eq!(before.nodes.len(), 3);
    assert!(before.nodes.values().any(|node| {
        node.descriptor.name == "empty"
            && matches!(
                node.descriptor.kind,
                lios_core::catalog::NodeDescriptorKindV2::Directory
            )
    }));

    catalog
        .rename_node(&nested_id, "renamed-nested", &key)
        .unwrap();
    let after = read_catalog_v2(&catalog, &key);
    let after_hashes = descriptor_hashes(&after);

    assert_ne!(before_hashes[&nested_id], after_hashes[&nested_id]);
    for node_id in before.nodes.keys().filter(|id| *id != &nested_id) {
        assert_eq!(before_hashes[node_id], after_hashes[node_id]);
    }
}

#[test]
fn delete_removes_descriptor_from_desired_remote_set() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog.create_folder(&root_id, "remove-me", &key).unwrap();
    let remove_id = match catalog.decrypt_tree(&key).unwrap().kind {
        CatalogTreeNodeKind::Directory { children } => children[0].id.clone(),
        CatalogTreeNodeKind::File { .. } => panic!("expected root directory"),
    };
    let descriptor_path = format!("recovery/nodes/{remove_id}.enc");
    assert!(catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .iter()
        .any(|file| file.path == descriptor_path));

    catalog
        .delete_nodes(std::slice::from_ref(&remove_id), &key)
        .unwrap();

    assert!(catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap()
        .iter()
        .all(|file| file.path != descriptor_path));
}

#[test]
fn selected_remote_files_include_all_current_node_descriptors() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog.create_folder(&root_id, "left", &key).unwrap();
    catalog.create_folder(&root_id, "right", &key).unwrap();
    let tree = catalog.decrypt_tree(&key).unwrap();
    let CatalogTreeNodeKind::Directory { children } = tree.kind else {
        panic!("expected root directory");
    };
    let left_id = children
        .iter()
        .find(|node| node.name == "left")
        .unwrap()
        .id
        .clone();

    let selected = catalog
        .remote_files_for_selection(&CatalogSelection::Node(left_id), &key)
        .unwrap();

    assert_eq!(
        selected
            .iter()
            .filter(|file| file.path.starts_with("recovery/nodes/"))
            .count(),
        3
    );
}

#[test]
fn new_file_uses_v2_object_layout_manifest_and_multiframe_streaming() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let source = tmp.path().join("classified-video.bin");
    let data = deterministic_bytes(2 * 1024 * 1024 + 333_333);
    write_file(&source, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    catalog
        .add_paths_to_folder(
            &root_id,
            std::slice::from_ref(&source),
            &[],
            &key,
            PackOptions {
                chunk_size: 3 * 1024 * 1024,
                staging_dir: staging.clone(),
            },
        )
        .unwrap();

    let object_id = file_object_id(&catalog, &key, "classified-video.bin");
    assert_eq!(object_id.len(), 32);
    let catalog_v2 = read_catalog_v2(&catalog, &key);
    let object = catalog_v2.content_objects.get(&object_id).unwrap();
    let StorageRef::V2(storage) = &object.storage else {
        panic!("expected v2 storage");
    };
    assert_eq!(
        storage.manifest_path,
        format!("objects/files/{object_id}/manifest.enc")
    );
    assert_eq!(storage.chunks.len(), 1);
    let chunk = &storage.chunks[0];
    assert_eq!(chunk.chunk_id.len(), 64);
    assert_eq!(
        chunk.path,
        format!("objects/files/{object_id}/chunks/{}.lios", chunk.chunk_id)
    );
    assert!(chunk.encoded_size > 0);

    let manifest_bytes = fs::read(staging.join(&storage.manifest_path)).unwrap();
    assert_eq!(
        hex::encode(sha2::Sha256::digest(&manifest_bytes)),
        storage.manifest_encrypted_sha256
    );
    assert_eq!(
        parse_envelope_v2(&manifest_bytes).unwrap().kind,
        EnvelopeKindV2::Manifest
    );
    let manifest_plaintext =
        decrypt_envelope_v2(&key, EnvelopeKindV2::Manifest, &manifest_bytes).unwrap();
    let manifest: ObjectManifestV2 = serde_json::from_slice(&manifest_plaintext).unwrap();
    assert_eq!(manifest.version, 2);
    assert_eq!(manifest.format_version, 2);
    assert_eq!(manifest.object_id, object_id);
    assert_eq!(manifest.content_sha256, object.content_sha256);
    assert_eq!(manifest.original_size, data.len() as u64);
    assert_eq!(manifest.chunks, storage.chunks);

    let chunk_bytes = fs::read(staging.join(&chunk.path)).unwrap();
    assert!(!String::from_utf8_lossy(&manifest_bytes).contains("classified-video.bin"));
    assert!(!String::from_utf8_lossy(&chunk_bytes).contains("classified-video.bin"));
    assert!(!chunk_bytes.windows(32).any(|window| window == &data[..32]));
    let frame_header_len = lios_core::framed_v2::CHUNK_FRAME_HEADER_LEN_V2;
    let stream_header_len = lios_core::framed_v2::CHUNK_STREAM_HEADER_LEN_V2;
    assert!(chunk_bytes.len() > stream_header_len + frame_header_len * 2);

    let remote = catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .unwrap();
    assert!(remote.iter().any(|file| file.path == storage.manifest_path));
    assert!(remote.iter().any(|file| file.path == chunk.path));
    assert!(remote
        .iter()
        .filter(|file| file.path.ends_with(".lios"))
        .all(|file| file
            .path
            .starts_with(&format!("objects/files/{object_id}/chunks/"))));
}

#[test]
fn identical_content_reuses_one_content_object_and_prunes_provisional_object() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let second = tmp.path().join("second.bin");
    let data = deterministic_bytes(65_537);
    write_file(&first, &data);
    write_file(&second, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    catalog
        .add_paths_to_folder(
            &root_id,
            &[first, second],
            &[],
            &key,
            PackOptions {
                chunk_size: 32 * 1024,
                staging_dir: staging.clone(),
            },
        )
        .unwrap();

    let first_object = file_object_id(&catalog, &key, "first.bin");
    let second_object = file_object_id(&catalog, &key, "second.bin");
    assert_eq!(first_object, second_object);
    let catalog_v2 = read_catalog_v2(&catalog, &key);
    assert_eq!(catalog_v2.content_objects.len(), 1);
    assert_eq!(catalog_v2.content_index.len(), 1);
    assert_eq!(
        fs::read_dir(staging.join("objects/files")).unwrap().count(),
        1
    );
}

#[test]
fn identical_upload_replaces_unavailable_indexed_object_with_provisional_v2_object() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let data = deterministic_bytes(65_537);
    write_file(&first, &data);
    write_file(&duplicate, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let options = PackOptions {
        chunk_size: 32 * 1024,
        staging_dir: staging.clone(),
    };

    catalog
        .add_paths_to_folder(
            &root_id,
            std::slice::from_ref(&first),
            &[],
            &key,
            options.clone(),
        )
        .unwrap();
    let unavailable_object_id = file_object_id(&catalog, &key, "first.bin");
    fs::remove_dir_all(staging.join("objects/files").join(&unavailable_object_id)).unwrap();

    catalog
        .add_paths_to_folder(&root_id, &[duplicate], &[], &key, options)
        .unwrap();

    let replacement_object_id = file_object_id(&catalog, &key, "duplicate.bin");
    assert_ne!(replacement_object_id, unavailable_object_id);
    assert_eq!(
        file_object_id(&catalog, &key, "first.bin"),
        replacement_object_id
    );
    let value = read_catalog_v2(&catalog, &key);
    assert_eq!(value.content_objects.len(), 1);
    assert!(!value.content_objects.contains_key(&unavailable_object_id));
    let StorageRef::V2(storage) = &value.content_objects[&replacement_object_id].storage else {
        panic!("expected replacement v2 storage");
    };
    assert!(staging.join(&storage.manifest_path).is_file());
    assert!(storage
        .chunks
        .iter()
        .all(|chunk| staging.join(&chunk.path).is_file()));
}

#[test]
fn verified_remote_inventory_preserves_cross_session_dedup() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let data = deterministic_bytes(65_537);
    write_file(&first, &data);
    write_file(&duplicate, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let options = PackOptions {
        chunk_size: 32 * 1024,
        staging_dir: staging.clone(),
    };
    catalog
        .add_paths_to_folder(
            &root_id,
            std::slice::from_ref(&first),
            &[],
            &key,
            options.clone(),
        )
        .unwrap();
    let object_id = file_object_id(&catalog, &key, "first.bin");
    let remote = content_remote_inventory(&catalog, &key, &staging);
    fs::remove_dir_all(staging.join("objects/files").join(&object_id)).unwrap();

    catalog
        .add_paths_to_folder_with_remote_inventory(
            &root_id,
            &[duplicate],
            &[],
            &key,
            options,
            &remote,
        )
        .unwrap();

    assert_eq!(file_object_id(&catalog, &key, "duplicate.bin"), object_id);
    assert_eq!(read_catalog_v2(&catalog, &key).content_objects.len(), 1);
}

#[test]
fn corrupt_remote_inventory_does_not_authorize_existing_object_reuse() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let data = deterministic_bytes(65_537);
    write_file(&first, &data);
    write_file(&duplicate, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let options = PackOptions {
        chunk_size: 32 * 1024,
        staging_dir: staging.clone(),
    };
    catalog
        .add_paths_to_folder(
            &root_id,
            std::slice::from_ref(&first),
            &[],
            &key,
            options.clone(),
        )
        .unwrap();
    let old_object_id = file_object_id(&catalog, &key, "first.bin");
    let mut remote = content_remote_inventory(&catalog, &key, &staging);
    remote
        .iter_mut()
        .find(|object| object.path.ends_with(".lios"))
        .unwrap()
        .size += 1;
    fs::remove_dir_all(staging.join("objects/files").join(&old_object_id)).unwrap();

    catalog
        .add_paths_to_folder_with_remote_inventory(
            &root_id,
            &[duplicate],
            &[],
            &key,
            options,
            &remote,
        )
        .unwrap();

    let replacement_id = file_object_id(&catalog, &key, "duplicate.bin");
    assert_ne!(replacement_id, old_object_id);
    assert_eq!(file_object_id(&catalog, &key, "first.bin"), replacement_id);
}

#[test]
fn corrupt_staged_object_without_verified_remote_copy_uses_provisional_object() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let data = deterministic_bytes(65_537);
    write_file(&first, &data);
    write_file(&duplicate, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let options = PackOptions {
        chunk_size: 32 * 1024,
        staging_dir: staging.clone(),
    };
    catalog
        .add_paths_to_folder(
            &root_id,
            std::slice::from_ref(&first),
            &[],
            &key,
            options.clone(),
        )
        .unwrap();
    let old_object_id = file_object_id(&catalog, &key, "first.bin");
    let value = read_catalog_v2(&catalog, &key);
    let StorageRef::V2(storage) = &value.content_objects[&old_object_id].storage else {
        panic!("expected v2 storage");
    };
    fs::write(staging.join(&storage.chunks[0].path), b"corrupt").unwrap();

    catalog
        .add_paths_to_folder_with_remote_inventory(&root_id, &[duplicate], &[], &key, options, &[])
        .unwrap();

    let replacement_id = file_object_id(&catalog, &key, "duplicate.bin");
    assert_ne!(replacement_id, old_object_id);
    assert_eq!(file_object_id(&catalog, &key, "first.bin"), replacement_id);
}

#[test]
fn independent_fresh_spaces_randomize_ids_and_ciphertext_for_same_content() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("same.bin");
    write_file(&source, &deterministic_bytes(128 * 1024));
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let first_staging = tmp.path().join("first-staging");
    let second_staging = tmp.path().join("second-staging");

    let first = Catalog::pack(
        PackSource::Path(source.clone()),
        &key,
        PackOptions {
            chunk_size: 64 * 1024,
            staging_dir: first_staging.clone(),
        },
    )
    .unwrap();
    let second = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 64 * 1024,
            staging_dir: second_staging.clone(),
        },
    )
    .unwrap();

    let first_object = file_object_id(&first, &key, "same.bin");
    let second_object = file_object_id(&second, &key, "same.bin");
    assert_ne!(first_object, second_object);
    let first_catalog = read_catalog_v2(&first, &key);
    let second_catalog = read_catalog_v2(&second, &key);
    let StorageRef::V2(first_storage) = &first_catalog.content_objects[&first_object].storage
    else {
        panic!("expected v2 storage");
    };
    let StorageRef::V2(second_storage) = &second_catalog.content_objects[&second_object].storage
    else {
        panic!("expected v2 storage");
    };
    assert_ne!(
        first_storage.chunks[0].chunk_id,
        second_storage.chunks[0].chunk_id
    );
    assert_ne!(
        fs::read(first_staging.join(&first_storage.chunks[0].path)).unwrap(),
        fs::read(second_staging.join(&second_storage.chunks[0].path)).unwrap()
    );
}

#[test]
fn migrated_catalog_restores_mixed_legacy_and_v2_files_exactly() {
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
    let catalog = Catalog::from_staging(staging.clone());
    let new_file = tmp.path().join("new.bin");
    let new_data = deterministic_bytes(100_003);
    write_file(&new_file, &new_data);

    catalog
        .add_paths_to_folder(
            "legacy-root",
            std::slice::from_ref(&new_file),
            &[],
            &key,
            PackOptions {
                chunk_size: 32 * 1024,
                staging_dir: staging,
            },
        )
        .unwrap();

    let migrated = read_catalog_v2(&catalog, &key);
    assert!(migrated
        .content_objects
        .values()
        .any(|object| matches!(object.storage, StorageRef::Legacy(_))));
    assert!(migrated
        .content_objects
        .values()
        .any(|object| matches!(object.storage, StorageRef::V2(_))));

    let output = tmp.path().join("restore");
    catalog
        .restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: output.clone(),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )
        .unwrap();
    assert_eq!(
        fs::read(output.join("legacy-archive/legacy.bin")).unwrap(),
        fs::read(fixtures.join("legacy_chunk_v1.bin")).unwrap()
    );
    assert_eq!(
        fs::read(output.join("legacy-archive/new.bin")).unwrap(),
        new_data
    );
}

#[test]
fn identical_new_file_reuses_legacy_object_without_creating_v2_content() {
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
    let duplicate = tmp.path().join("duplicate.bin");
    write_file(
        &duplicate,
        &fs::read(fixtures.join("legacy_chunk_v1.bin")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging.clone());

    catalog
        .add_paths_to_folder(
            "legacy-root",
            &[duplicate],
            &[],
            &key,
            PackOptions {
                chunk_size: 32,
                staging_dir: staging.clone(),
            },
        )
        .unwrap();

    let catalog_v2 = read_catalog_v2(&catalog, &key);
    assert_eq!(catalog_v2.content_objects.len(), 1);
    assert!(matches!(
        catalog_v2.content_objects["legacy-object"].storage,
        StorageRef::Legacy(_)
    ));
    assert_eq!(
        file_object_id(&catalog, &key, "duplicate.bin"),
        "legacy-object"
    );
    assert_eq!(
        fs::read_dir(staging.join("objects/files")).unwrap().count(),
        1
    );
}

#[test]
fn verified_legacy_remote_inventory_preserves_cross_session_dedup() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    let chunk_path = "objects/files/legacy-object/chunks/golden.lios";
    let encrypted_chunk = fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap();
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(&staging.join(chunk_path), &encrypted_chunk);
    let duplicate = tmp.path().join("duplicate.bin");
    write_file(
        &duplicate,
        &fs::read(fixtures.join("legacy_chunk_v1.bin")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging.clone());
    let remote = vec![StorageObject {
        path: chunk_path.to_string(),
        size: 0,
        sha256: Some(hex::encode(sha2::Sha256::digest(&encrypted_chunk))),
    }];
    fs::remove_file(staging.join(chunk_path)).unwrap();

    catalog
        .add_paths_to_folder_with_remote_inventory(
            "legacy-root",
            &[duplicate],
            &[],
            &key,
            PackOptions {
                chunk_size: 32,
                staging_dir: staging.clone(),
            },
            &remote,
        )
        .unwrap();

    let catalog_v2 = read_catalog_v2(&catalog, &key);
    assert_eq!(catalog_v2.content_objects.len(), 1);
    assert!(matches!(
        catalog_v2.content_objects["legacy-object"].storage,
        StorageRef::Legacy(_)
    ));
    assert_eq!(
        file_object_id(&catalog, &key, "duplicate.bin"),
        "legacy-object"
    );
}

#[test]
fn unverified_legacy_remote_inventory_does_not_authorize_dedup() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    let chunk_path = "objects/files/legacy-object/chunks/golden.lios";
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    write_file(
        &staging.join(chunk_path),
        &fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap(),
    );
    let duplicate = tmp.path().join("duplicate.bin");
    write_file(
        &duplicate,
        &fs::read(fixtures.join("legacy_chunk_v1.bin")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging.clone());
    fs::remove_file(staging.join(chunk_path)).unwrap();

    catalog
        .add_paths_to_folder_with_remote_inventory(
            "legacy-root",
            &[duplicate],
            &[],
            &key,
            PackOptions {
                chunk_size: 32,
                staging_dir: staging,
            },
            &[StorageObject {
                path: chunk_path.to_string(),
                size: 0,
                sha256: None,
            }],
        )
        .unwrap();

    let catalog_v2 = read_catalog_v2(&catalog, &key);
    assert_eq!(catalog_v2.content_objects.len(), 1);
    assert!(catalog_v2
        .content_objects
        .values()
        .all(|object| matches!(object.storage, StorageRef::V2(_))));
}

#[test]
fn deleting_indexed_legacy_duplicate_repoints_dedup_to_surviving_object() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let key_path = fixtures.join("legacy_v1.key");
    let plaintext = fs::read(fixtures.join("legacy_chunk_v1.bin")).unwrap();
    let encrypted_chunk = fs::read(fixtures.join("legacy_chunk_v1.enc")).unwrap();
    let content_sha256 = hex::encode(sha2::Sha256::digest(&plaintext));
    let encrypted_sha256 = hex::encode(sha2::Sha256::digest(&encrypted_chunk));
    let staging = tmp.path().join("staging");
    let first_chunk_path = "objects/files/legacy-object-a/chunks/shared.lios";
    let surviving_chunk_path = "objects/files/legacy-object-b/chunks/shared.lios";
    write_file(&staging.join(first_chunk_path), &encrypted_chunk);
    write_file(&staging.join(surviving_chunk_path), &encrypted_chunk);
    let v1_catalog = serde_json::json!({
        "version": 1,
        "root": {
            "id": "legacy-root",
            "name": "legacy-archive",
            "updated_at": "2026-01-01T00:00:00Z",
            "kind": {
                "type": "Directory",
                "children": [
                    {
                        "id": "legacy-file-a",
                        "name": "first.bin",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "kind": {
                            "type": "File",
                            "original_size": plaintext.len(),
                            "sha256": content_sha256,
                            "object_id": "legacy-object-a",
                            "chunks": [{
                                "index": 0,
                                "path": first_chunk_path,
                                "original_size": plaintext.len(),
                                "original_sha256": content_sha256,
                                "encrypted_sha256": encrypted_sha256
                            }]
                        }
                    },
                    {
                        "id": "legacy-file-b",
                        "name": "survivor.bin",
                        "updated_at": "2026-01-01T00:00:00Z",
                        "kind": {
                            "type": "File",
                            "original_size": plaintext.len(),
                            "sha256": content_sha256,
                            "object_id": "legacy-object-b",
                            "chunks": [{
                                "index": 0,
                                "path": surviving_chunk_path,
                                "original_size": plaintext.len(),
                                "original_sha256": content_sha256,
                                "encrypted_sha256": encrypted_sha256
                            }]
                        }
                    }
                ]
            }
        }
    });
    write_file(
        &staging.join("catalog.enc"),
        &encrypt_v1_catalog(&key_path, &v1_catalog),
    );
    let key = KeyFile::load_from_path(&key_path).unwrap();
    let catalog = Catalog::from_staging(staging.clone());

    catalog
        .delete_nodes(&["legacy-file-a".to_string()], &key)
        .unwrap();

    let migrated = read_catalog_v2(&catalog, &key);
    assert_eq!(
        migrated
            .content_index
            .get(&content_sha256)
            .map(String::as_str),
        Some("legacy-object-b")
    );
    assert_eq!(migrated.content_objects.len(), 1);
    assert!(matches!(
        migrated.content_objects["legacy-object-b"].storage,
        StorageRef::Legacy(_)
    ));

    let duplicate = tmp.path().join("duplicate.bin");
    write_file(&duplicate, &plaintext);
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let object_dir_count_before = fs::read_dir(staging.join("objects/files")).unwrap().count();
    catalog
        .add_paths_to_folder(
            &root_id,
            &[duplicate],
            &[],
            &key,
            PackOptions {
                chunk_size: 32,
                staging_dir: staging.clone(),
            },
        )
        .unwrap();

    let after_upload = read_catalog_v2(&catalog, &key);
    assert_eq!(
        file_object_id(&catalog, &key, "duplicate.bin"),
        "legacy-object-b"
    );
    assert_eq!(after_upload.content_objects.len(), 1);
    assert!(after_upload
        .content_objects
        .values()
        .all(|object| matches!(object.storage, StorageRef::Legacy(_))));
    assert_eq!(
        fs::read_dir(staging.join("objects/files")).unwrap().count(),
        object_dir_count_before
    );
}

#[test]
fn content_object_is_pruned_only_after_its_last_node_reference_is_deleted() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let second = tmp.path().join("second.bin");
    write_file(&first, b"same content");
    write_file(&second, b"same content");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    catalog
        .add_paths_to_folder(
            &root_id,
            &[first, second],
            &[],
            &key,
            PackOptions {
                chunk_size: 4,
                staging_dir: staging,
            },
        )
        .unwrap();
    let tree = catalog.decrypt_tree(&key).unwrap();
    let CatalogTreeNodeKind::Directory { children } = tree.kind else {
        panic!("expected root directory");
    };
    let first_id = children
        .iter()
        .find(|node| node.name == "first.bin")
        .unwrap()
        .id
        .clone();
    let second_id = children
        .iter()
        .find(|node| node.name == "second.bin")
        .unwrap()
        .id
        .clone();

    catalog.delete_nodes(&[first_id], &key).unwrap();
    assert_eq!(read_catalog_v2(&catalog, &key).content_objects.len(), 1);

    catalog.delete_nodes(&[second_id], &key).unwrap();
    let empty = read_catalog_v2(&catalog, &key);
    assert!(empty.content_objects.is_empty());
    assert!(empty.content_index.is_empty());
}

#[test]
fn corrupted_existing_v2_object_is_replaced_before_dedup_reuse() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let first = tmp.path().join("first.bin");
    let duplicate = tmp.path().join("duplicate.bin");
    let data = deterministic_bytes(70_000);
    write_file(&first, &data);
    write_file(&duplicate, &data);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let options = PackOptions {
        chunk_size: 32 * 1024,
        staging_dir: staging.clone(),
    };
    catalog
        .add_paths_to_folder(&root_id, &[first], &[], &key, options.clone())
        .unwrap();
    let object_id = file_object_id(&catalog, &key, "first.bin");
    let value = read_catalog_v2(&catalog, &key);
    let StorageRef::V2(storage) = &value.content_objects[&object_id].storage else {
        panic!("expected v2 storage");
    };
    let chunk_path = staging.join(&storage.chunks[0].path);
    let mut corrupted = fs::read(&chunk_path).unwrap();
    *corrupted.last_mut().unwrap() ^= 0x80;
    fs::write(&chunk_path, corrupted).unwrap();

    catalog
        .add_paths_to_folder(&root_id, &[duplicate], &[], &key, options)
        .unwrap();

    let replacement_id = file_object_id(&catalog, &key, "duplicate.bin");
    assert_ne!(replacement_id, object_id);
    assert_eq!(file_object_id(&catalog, &key, "first.bin"), replacement_id);
    let repaired = read_catalog_v2(&catalog, &key);
    assert_eq!(repaired.content_objects.len(), 1);
    assert!(!repaired.content_objects.contains_key(&object_id));
}

#[test]
fn restore_rejects_unknown_manifest_version_and_wrong_envelope_kind() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let source = tmp.path().join("source.bin");
    write_file(&source, b"manifest integrity");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 8,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();
    let object_id = file_object_id(&catalog, &key, "source.bin");
    let mut value = read_catalog_v2(&catalog, &key);
    let StorageRef::V2(storage) = &mut value.content_objects.get_mut(&object_id).unwrap().storage
    else {
        panic!("expected v2 storage");
    };
    let manifest_path = staging.join(&storage.manifest_path);
    let encrypted = fs::read(&manifest_path).unwrap();
    let plaintext = decrypt_envelope_v2(&key, EnvelopeKindV2::Manifest, &encrypted).unwrap();
    let mut manifest: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
    manifest["version"] = serde_json::Value::from(99);
    let unknown = encrypt_envelope_v2(
        &key,
        EnvelopeKindV2::Manifest,
        &serde_json::to_vec(&manifest).unwrap(),
    )
    .unwrap();
    storage.manifest_encrypted_sha256 = hex::encode(sha2::Sha256::digest(&unknown));
    fs::write(&manifest_path, unknown).unwrap();
    write_catalog_v2(&catalog, &key, &value);

    let error = catalog
        .restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: tmp.path().join("unknown-version"),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )
        .unwrap_err();
    assert!(error.to_string().contains("unknown manifest version"));

    let mut value = read_catalog_v2(&catalog, &key);
    let StorageRef::V2(storage) = &mut value.content_objects.get_mut(&object_id).unwrap().storage
    else {
        panic!("expected v2 storage");
    };
    let wrong_kind = encrypt_envelope_v2(&key, EnvelopeKindV2::Catalog, b"wrong kind").unwrap();
    storage.manifest_encrypted_sha256 = hex::encode(sha2::Sha256::digest(&wrong_kind));
    fs::write(&manifest_path, wrong_kind).unwrap();
    write_catalog_v2(&catalog, &key, &value);
    let error = catalog
        .restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: tmp.path().join("wrong-kind"),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )
        .unwrap_err();
    assert!(matches!(
        error,
        lios_core::LiosError::UnexpectedV2Kind { .. }
    ));
}

#[test]
fn v2_catalog_rejects_non_normal_remote_paths_before_restore() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let source = tmp.path().join("source.bin");
    write_file(&source, b"safe path");
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
    let object_id = file_object_id(&catalog, &key, "source.bin");
    let mut value = read_catalog_v2(&catalog, &key);
    let StorageRef::V2(storage) = &mut value.content_objects.get_mut(&object_id).unwrap().storage
    else {
        panic!("expected v2 storage");
    };
    storage.chunks[0].path = "../outside.lios".to_string();
    write_catalog_v2(&catalog, &key, &value);

    let error = catalog.decrypt_tree(&key).unwrap_err();
    assert!(error.to_string().contains("invalid remote object path"));
}

#[test]
fn v2_node_id_path_escape_is_rejected_before_descriptor_write() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    let old_root_id = value.root_id.clone();
    let mut root = value.nodes.remove(&old_root_id).unwrap();
    let escaping_id = "../../../outside-node".to_string();
    root.descriptor.node_id = escaping_id.clone();
    root.descriptor_encrypted_sha256 = None;
    value.root_id = escaping_id.clone();
    value.nodes.insert(escaping_id.clone(), root);
    write_catalog_v2(&catalog, &key, &value);
    let outside = tmp.path().join("outside-node.enc");

    let result = catalog.create_folder(&escaping_id, "child", &key);

    assert!(result.is_err());
    assert!(!outside.exists());
}

#[test]
fn v2_storage_object_id_requires_canonical_lowercase_hex() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let source = tmp.path().join("source.bin");
    write_file(&source, b"canonical object id");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 8,
            staging_dir: staging,
        },
    )
    .unwrap();
    let old_object_id = file_object_id(&catalog, &key, "source.bin");
    let invalid_object_id = "A".repeat(32);
    let mut value = read_catalog_v2(&catalog, &key);
    let mut object = value.content_objects.remove(&old_object_id).unwrap();
    object.object_id = invalid_object_id.clone();
    let StorageRef::V2(storage) = &mut object.storage else {
        panic!("expected v2 storage");
    };
    storage.manifest_path = format!("objects/files/{invalid_object_id}/manifest.enc");
    for chunk in &mut storage.chunks {
        chunk.path = format!(
            "objects/files/{invalid_object_id}/chunks/{}.lios",
            chunk.chunk_id
        );
    }
    for node in value.nodes.values_mut() {
        if let NodeDescriptorKindV2::File { object_id, .. } = &mut node.descriptor.kind {
            *object_id = invalid_object_id.clone();
        }
    }
    value
        .content_index
        .values_mut()
        .for_each(|object_id| *object_id = invalid_object_id.clone());
    value.content_objects.insert(invalid_object_id, object);
    write_catalog_v2(&catalog, &key, &value);

    let error = catalog.decrypt_tree(&key).unwrap_err();
    assert!(error.to_string().contains("object id"));
}

#[test]
fn legacy_storage_paths_must_remain_inside_objects_prefix() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let key_path = fixtures.join("legacy_v1.key");
    let staging = tmp.path().join("staging");
    let value = serde_json::json!({
        "version": 1,
        "root": {
            "id": "legacy root id",
            "name": "legacy",
            "kind": {
                "type": "File",
                "original_size": 0,
                "sha256": hex::encode(sha2::Sha256::digest([])),
                "object_id": "legacy object id",
                "chunks": [{
                    "index": 0,
                    "path": "unmanaged/chunk.lios",
                    "original_size": 0,
                    "original_sha256": hex::encode(sha2::Sha256::digest([])),
                    "encrypted_sha256": hex::encode(sha2::Sha256::digest([]))
                }]
            }
        }
    });
    write_file(
        &staging.join("catalog.enc"),
        &encrypt_v1_catalog(&key_path, &value),
    );
    let key = KeyFile::load_from_path(key_path).unwrap();
    let catalog = Catalog::from_staging(staging);

    let error = catalog.decrypt_tree(&key).unwrap_err();
    assert!(error.to_string().contains("managed objects prefix"));
}

#[test]
fn v2_catalog_rejects_disconnected_cycle() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    let first = format!("{:032x}", 1);
    let second = format!("{:032x}", 2);
    insert_test_directory(&mut value, first.clone(), Some(second.clone()), "first");
    insert_test_directory(&mut value, second, Some(first), "second");
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog.decrypt_tree(&key).is_err());
}

#[test]
fn v2_catalog_rejects_non_root_node_without_parent_before_remote_enumeration() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    insert_test_directory(&mut value, format!("{:032x}", 3), None, "orphan");
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog
        .remote_files_for_selection(&CatalogSelection::All, &key)
        .is_err());
}

#[test]
fn v2_catalog_rejects_child_of_file() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let source = tmp.path().join("root.bin");
    write_file(&source, b"file parent");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 8,
            staging_dir: staging,
        },
    )
    .unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    let root_id = value.root_id.clone();
    insert_test_directory(&mut value, format!("{:032x}", 4), Some(root_id), "child");
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog.decrypt_tree(&key).is_err());
}

#[test]
fn v2_catalog_rejects_duplicate_sibling_names_under_windows_semantics() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    let root_id = value.root_id.clone();
    insert_test_directory(
        &mut value,
        format!("{:032x}", 5),
        Some(root_id.clone()),
        "Report",
    );
    insert_test_directory(&mut value, format!("{:032x}", 6), Some(root_id), "report");
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog.decrypt_tree(&key).is_err());
}

#[test]
fn v2_catalog_rejects_excessive_depth() {
    const EXCESSIVE_DEPTH: usize = 257;

    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging).unwrap();
    let mut value = read_catalog_v2(&catalog, &key);
    let mut parent_id = value.root_id.clone();
    for depth in 1..=EXCESSIVE_DEPTH {
        let id = format!("{depth:032x}");
        insert_test_directory(
            &mut value,
            id.clone(),
            Some(parent_id),
            format!("level-{depth}"),
        );
        parent_id = id;
    }
    write_catalog_v2(&catalog, &key, &value);

    assert!(catalog.decrypt_tree(&key).is_err());
}

#[test]
fn legacy_optimization_summary_identifies_only_legacy_content_objects() {
    let tmp = tempdir().unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/crypto_v1");
    let staging = tmp.path().join("staging");
    write_file(
        &staging.join("catalog.enc"),
        &fs::read(fixtures.join("legacy_catalog_v1.enc")).unwrap(),
    );
    let key = KeyFile::load_from_path(fixtures.join("legacy_v1.key")).unwrap();
    let catalog = Catalog::from_staging(staging);

    let summary = catalog
        .legacy_content_objects_needing_optimization(&key)
        .unwrap();
    assert_eq!(summary.len(), 1);
    assert_eq!(summary[0].object_id, "legacy-object");
    assert_eq!(summary[0].original_size, 78);
    assert_eq!(
        summary[0].content_sha256,
        "c79c31459aceec53b672359d1fbb98381afee6e0d72c9c142e807bad6d07b0cb"
    );
}
