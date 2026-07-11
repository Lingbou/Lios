use std::fs;

use lios_core::storage::{
    plan_catalog_sync_changes, plan_current_snapshot_changes, validate_catalog_sync_upload,
    CatalogSyncFile, LocalStorageObject, StorageObject,
};
use lios_core::LiosError;
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn write_sync_file(root: &std::path::Path, name: &str, bytes: &[u8]) -> CatalogSyncFile {
    let local_path = root.join(name.replace('/', "_"));
    fs::write(&local_path, bytes).unwrap();
    CatalogSyncFile {
        path: name.to_string(),
        local_path: Some(local_path),
        expected_sha256: Some(sha256(bytes)),
        expected_size: Some(bytes.len() as u64),
    }
}

#[test]
fn catalog_sync_rejects_corrupted_staged_file_before_planning_actions() {
    let temp = tempdir().unwrap();
    let local_path = temp.path().join("manifest.enc");
    fs::write(&local_path, b"corrupt staged bytes").unwrap();
    let expected = hex::encode(Sha256::digest(b"trusted manifest bytes"));

    let error = plan_catalog_sync_changes(
        vec![CatalogSyncFile {
            path: "objects/files/live/manifest.enc".to_string(),
            local_path: Some(local_path),
            expected_sha256: Some(expected),
            expected_size: Some(b"trusted manifest bytes".len() as u64),
        }],
        vec![StorageObject {
            path: "objects/files/stale/manifest.enc".to_string(),
            size: 12,
            sha256: Some("stale".to_string()),
        }],
    )
    .unwrap_err();

    assert!(matches!(error, LiosError::DataCorruption(_)));
}

#[test]
fn catalog_sync_uses_verified_remote_when_local_file_is_missing() {
    let temp = tempdir().unwrap();
    let expected_bytes = b"remote object";
    let path = "objects/files/live/chunks/chunk.lios";

    let plan = plan_catalog_sync_changes(
        vec![CatalogSyncFile {
            path: path.to_string(),
            local_path: Some(temp.path().join("missing.lios")),
            expected_sha256: Some(sha256(expected_bytes)),
            expected_size: Some(expected_bytes.len() as u64),
        }],
        vec![StorageObject {
            path: path.to_string(),
            size: expected_bytes.len() as u64,
            sha256: Some(sha256(expected_bytes)),
        }],
    )
    .unwrap();

    assert!(plan.upload.is_empty());
}

#[test]
fn catalog_sync_rejects_trusted_file_missing_locally_and_remotely() {
    let error = plan_catalog_sync_changes(
        vec![CatalogSyncFile {
            path: "recovery/nodes/missing.enc".to_string(),
            local_path: None,
            expected_sha256: Some(sha256(b"missing descriptor")),
            expected_size: Some(b"missing descriptor".len() as u64),
        }],
        Vec::new(),
    )
    .unwrap_err();

    assert!(matches!(error, LiosError::DataCorruption(_)));
}

#[test]
fn catalog_sync_allows_absent_untrusted_optional_file_without_uploading() {
    let temp = tempdir().unwrap();
    let plan = plan_catalog_sync_changes(
        vec![CatalogSyncFile {
            path: "objects/files/legacy/manifest.enc".to_string(),
            local_path: Some(temp.path().join("missing-legacy-manifest.enc")),
            expected_sha256: None,
            expected_size: None,
        }],
        Vec::new(),
    )
    .unwrap();

    assert!(plan.upload.is_empty());
    assert!(plan.delete.is_empty());
}

#[test]
fn catalog_sync_uploads_catalog_last() {
    let temp = tempdir().unwrap();
    let desired = vec![
        write_sync_file(temp.path(), "catalog.enc", b"catalog"),
        write_sync_file(temp.path(), "recovery/nodes/root.enc", b"descriptor"),
        write_sync_file(temp.path(), "objects/files/live/manifest.enc", b"manifest"),
    ];

    let plan = plan_catalog_sync_changes(desired, Vec::new()).unwrap();

    assert_eq!(
        plan.upload
            .iter()
            .map(|upload| upload.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "objects/files/live/manifest.enc",
            "recovery/nodes/root.enc",
            "catalog.enc"
        ]
    );
}

#[test]
fn catalog_sync_deletes_only_unreferenced_managed_paths() {
    let desired = vec![CatalogSyncFile {
        path: "objects/files/legacy/manifest.enc".to_string(),
        local_path: None,
        expected_sha256: None,
        expected_size: None,
    }];
    let remote = vec![
        StorageObject {
            path: "objects/files/legacy/manifest.enc".to_string(),
            size: 10,
            sha256: None,
        },
        StorageObject {
            path: "objects/files/stale/chunks/stale.lios".to_string(),
            size: 10,
            sha256: Some("stale".to_string()),
        },
        StorageObject {
            path: "recovery/nodes/stale.enc".to_string(),
            size: 10,
            sha256: Some("stale-node".to_string()),
        },
        StorageObject {
            path: "catalog.enc".to_string(),
            size: 10,
            sha256: Some("old-catalog".to_string()),
        },
        StorageObject {
            path: "README.md".to_string(),
            size: 10,
            sha256: Some("user-file".to_string()),
        },
    ];

    let plan = plan_catalog_sync_changes(desired, remote).unwrap();

    assert_eq!(
        plan.delete,
        vec![
            "objects/files/stale/chunks/stale.lios",
            "recovery/nodes/stale.enc"
        ]
    );
}

#[test]
fn planned_catalog_upload_rejects_file_mutation_before_upload() {
    let temp = tempdir().unwrap();
    let desired = write_sync_file(
        temp.path(),
        "objects/files/live/chunks/chunk.lios",
        b"trusted bytes",
    );
    let expected_sha256 = desired.expected_sha256.clone().unwrap();
    let expected_size = desired.expected_size;
    let local_path = desired.local_path.clone().unwrap();
    let plan = plan_catalog_sync_changes(vec![desired], Vec::new()).unwrap();
    let upload = plan.upload.first().unwrap();
    assert_eq!(upload.expected_sha256, expected_sha256);
    assert_eq!(upload.expected_size, expected_size);

    fs::write(local_path, b"mutated after planning").unwrap();

    let error = validate_catalog_sync_upload(upload).unwrap_err();
    assert!(matches!(error, LiosError::DataCorruption(_)));
}

#[test]
fn snapshot_plan_uploads_changed_files_and_deletes_unreferenced_objects() {
    let local = vec![
        LocalStorageObject {
            path: "catalog.enc".to_string(),
            sha256: "new-catalog".to_string(),
        },
        LocalStorageObject {
            path: "objects/chunks/keep.lios".to_string(),
            sha256: "same".to_string(),
        },
        LocalStorageObject {
            path: "objects/chunks/update.lios".to_string(),
            sha256: "new".to_string(),
        },
        LocalStorageObject {
            path: "objects/files/live/manifest.enc".to_string(),
            sha256: "manifest".to_string(),
        },
    ];
    let remote = vec![
        StorageObject {
            path: "catalog.enc".to_string(),
            size: 10,
            sha256: Some("old-catalog".to_string()),
        },
        StorageObject {
            path: "objects/chunks/keep.lios".to_string(),
            size: 10,
            sha256: Some("same".to_string()),
        },
        StorageObject {
            path: "objects/chunks/update.lios".to_string(),
            size: 10,
            sha256: Some("old".to_string()),
        },
        StorageObject {
            path: "objects/chunks/stale.lios".to_string(),
            size: 10,
            sha256: Some("stale".to_string()),
        },
        StorageObject {
            path: "README.md".to_string(),
            size: 10,
            sha256: Some("keep-user-file".to_string()),
        },
    ];

    let plan = plan_current_snapshot_changes(local, remote);

    assert_eq!(
        plan.upload
            .iter()
            .map(|object| object.path.as_str())
            .collect::<Vec<_>>(),
        vec![
            "objects/chunks/update.lios",
            "objects/files/live/manifest.enc",
            "catalog.enc"
        ]
    );
    assert_eq!(plan.delete, vec!["objects/chunks/stale.lios"]);
}

#[test]
fn snapshot_plan_manages_node_descriptors_and_publishes_catalog_last() {
    let local = vec![
        LocalStorageObject {
            path: "catalog.enc".to_string(),
            sha256: "catalog-v2".to_string(),
        },
        LocalStorageObject {
            path: "recovery/nodes/live.enc".to_string(),
            sha256: "live-descriptor".to_string(),
        },
        LocalStorageObject {
            path: "objects/files/live/manifest.enc".to_string(),
            sha256: "manifest".to_string(),
        },
    ];
    let remote = vec![
        StorageObject {
            path: "recovery/nodes/stale.enc".to_string(),
            size: 10,
            sha256: Some("stale-descriptor".to_string()),
        },
        StorageObject {
            path: "unmanaged/user-file.txt".to_string(),
            size: 10,
            sha256: Some("keep".to_string()),
        },
    ];

    let plan = plan_current_snapshot_changes(local, remote);

    assert_eq!(plan.upload.last().unwrap().path, "catalog.enc");
    assert!(plan
        .upload
        .iter()
        .any(|object| object.path == "recovery/nodes/live.enc"));
    assert_eq!(plan.delete, vec!["recovery/nodes/stale.enc"]);
}

#[test]
fn metadata_only_v1_migration_keeps_all_legacy_object_paths_untouched() {
    let local = vec![
        LocalStorageObject {
            path: "objects/files/legacy-object/chunks/golden.lios".to_string(),
            sha256: "legacy-chunk".to_string(),
        },
        LocalStorageObject {
            path: "recovery/nodes/root.enc".to_string(),
            sha256: "root-descriptor".to_string(),
        },
        LocalStorageObject {
            path: "recovery/nodes/file.enc".to_string(),
            sha256: "file-descriptor".to_string(),
        },
        LocalStorageObject {
            path: "catalog.enc".to_string(),
            sha256: "catalog-v2".to_string(),
        },
    ];
    let remote = vec![
        StorageObject {
            path: "objects/files/legacy-object/manifest.enc".to_string(),
            size: 10,
            sha256: None,
        },
        StorageObject {
            path: "objects/files/legacy-object/chunks/golden.lios".to_string(),
            size: 10,
            sha256: Some("legacy-chunk".to_string()),
        },
        StorageObject {
            path: "catalog.enc".to_string(),
            size: 10,
            sha256: Some("catalog-v1".to_string()),
        },
    ];

    let plan = plan_current_snapshot_changes(local, remote);

    assert!(plan
        .upload
        .iter()
        .all(|object| !object.path.starts_with("objects/")));
    assert!(plan.delete.iter().all(|path| !path.starts_with("objects/")));
    assert_eq!(plan.upload.last().unwrap().path, "catalog.enc");
}
