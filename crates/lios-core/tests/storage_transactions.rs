use std::collections::HashSet;

use lios_core::storage::{
    current_catalog_sha256, BlobCheckpoint, CommitPlan, RemoteAction, StorageObject,
    StorageTransactionError, MODELSCOPE_COMMIT_ACTION_LIMIT, MODELSCOPE_LFS_BATCH_SIZE,
};
use serde_json::json;

fn upload(path: impl Into<String>, seed: usize) -> RemoteAction {
    RemoteAction::lfs_upsert(
        path,
        BlobCheckpoint::new(format!("{seed:064x}"), seed as u64 + 1),
    )
}

fn remote(path: impl Into<String>, sha256: impl Into<String>) -> StorageObject {
    StorageObject {
        path: path.into(),
        size: 1,
        sha256: Some(sha256.into()),
    }
}

fn empty_prepublish_safe_paths() -> HashSet<String> {
    HashSet::new()
}

#[test]
fn modelscope_transaction_limits_are_explicit() {
    assert_eq!(MODELSCOPE_LFS_BATCH_SIZE, 64);
    assert_eq!(MODELSCOPE_COMMIT_ACTION_LIMIT, 256);
}

#[test]
fn blob_checkpoint_roundtrips_for_future_persistence() {
    let checkpoint = BlobCheckpoint::new("a".repeat(64), 0);
    let encoded = serde_json::to_string(&checkpoint).unwrap();
    let decoded: BlobCheckpoint = serde_json::from_str(&encoded).unwrap();

    assert_eq!(decoded, checkpoint);
}

#[test]
fn remote_actions_serialize_as_modelscope_lfs_upsert_and_delete() {
    let upsert = upload("objects/files/new/chunks/part.lios", 7);
    let delete = RemoteAction::delete("recovery/nodes/stale.enc");

    assert_eq!(
        serde_json::to_value(upsert).unwrap(),
        json!({
            "action": "create",
            "path": "objects/files/new/chunks/part.lios",
            "type": "lfs",
            "size": 8,
            "sha256": format!("{:064x}", 7),
            "content": "",
            "encoding": ""
        })
    );
    assert_eq!(
        serde_json::to_value(delete).unwrap(),
        json!({
            "action": "delete",
            "path": "recovery/nodes/stale.enc",
            "type": "normal",
            "size": 0,
            "sha256": "",
            "content": "",
            "encoding": ""
        })
    );
}

#[test]
fn at_most_256_uploads_publish_before_cleanup_with_catalog_last() {
    let remote_inventory = vec![
        remote("catalog.enc", "old-catalog"),
        remote("objects/files/stale/manifest.enc", "stale"),
    ];
    let uploads = vec![
        upload("catalog.enc", 3),
        upload("recovery/nodes/root.enc", 2),
        upload("objects/files/new/manifest.enc", 1),
    ];
    let deletes = vec!["objects/files/stale/manifest.enc".to_string()];

    let plan = CommitPlan::build(
        uploads,
        deletes,
        &remote_inventory,
        &empty_prepublish_safe_paths(),
        current_catalog_sha256(&remote_inventory).map(ToOwned::to_owned),
    )
    .unwrap();

    assert!(plan.prepublish.is_empty());
    assert_eq!(plan.publish.len(), 3);
    assert_eq!(
        plan.publish
            .iter()
            .map(RemoteAction::path)
            .collect::<Vec<_>>(),
        vec![
            "objects/files/new/manifest.enc",
            "recovery/nodes/root.enc",
            "catalog.enc"
        ]
    );
    assert!(plan.publish.iter().all(RemoteAction::is_upload));
    assert_eq!(plan.cleanup.len(), 1);
    assert_eq!(plan.cleanup[0].len(), 1);
    assert_eq!(
        plan.cleanup[0][0].path(),
        "objects/files/stale/manifest.enc"
    );
    assert!(plan.cleanup[0][0].is_delete());
    assert_eq!(plan.base_catalog_sha256.as_deref(), Some("old-catalog"));
}

#[test]
fn more_than_256_uploads_use_bounded_phases_with_catalog_publish_before_cleanup() {
    let mut uploads = (0..256)
        .map(|index| upload(format!("objects/new/{index:03}.enc"), index + 1))
        .collect::<Vec<_>>();
    uploads.push(upload("catalog.enc", 999));
    let remote_inventory = vec![
        remote("catalog.enc", "old-catalog"),
        remote("objects/stale.enc", "stale"),
    ];
    let prepublish_safe_paths = uploads
        .iter()
        .filter(|action| action.path() != "catalog.enc")
        .map(|action| action.path().to_string())
        .collect::<HashSet<_>>();

    let plan = CommitPlan::build(
        uploads,
        vec!["objects/stale.enc".to_string()],
        &remote_inventory,
        &prepublish_safe_paths,
        Some("old-catalog".to_string()),
    )
    .unwrap();

    assert_eq!(plan.prepublish.len(), 1);
    assert_eq!(plan.prepublish[0].len(), 256);
    assert!(plan.prepublish[0]
        .iter()
        .all(|action| action.is_upload() && action.path() != "catalog.enc"));
    assert_eq!(plan.publish.len(), 1);
    assert_eq!(plan.publish[0].path(), "catalog.enc");
    assert_eq!(plan.cleanup.len(), 1);
    assert_eq!(plan.cleanup[0].len(), 1);
    assert_eq!(plan.cleanup[0][0].path(), "objects/stale.enc");
    assert!(plan
        .all_batches()
        .all(|batch| batch.len() <= MODELSCOPE_COMMIT_ACTION_LIMIT));
}

#[test]
fn absent_uploads_are_not_prepublish_safe_without_explicit_approval() {
    let mut uploads = (0..255)
        .map(|index| upload(format!("objects/new/{index:03}.enc"), index + 1))
        .collect::<Vec<_>>();
    uploads.push(upload("catalog.enc", 999));
    let remote_inventory = vec![
        remote("catalog.enc", "old-catalog"),
        remote("objects/stale.enc", "stale"),
    ];

    let plan = CommitPlan::build(
        uploads,
        vec!["objects/stale.enc".to_string()],
        &remote_inventory,
        &empty_prepublish_safe_paths(),
        Some("old-catalog".to_string()),
    )
    .unwrap();

    assert!(plan.prepublish.is_empty());
    assert_eq!(plan.publish.len(), MODELSCOPE_COMMIT_ACTION_LIMIT);
    assert_eq!(plan.publish.last().unwrap().path(), "catalog.enc");
    assert_eq!(plan.cleanup.len(), 1);
}

#[test]
fn existing_path_descriptor_update_is_never_prepublished() {
    let descriptor_path = "recovery/nodes/root.enc";
    let mut uploads = (0..255)
        .map(|index| upload(format!("objects/new/{index:03}.enc"), index + 1))
        .collect::<Vec<_>>();
    uploads.push(upload(descriptor_path, 900));
    uploads.push(upload("catalog.enc", 901));
    let remote_inventory = vec![
        remote("catalog.enc", "old-catalog"),
        remote(descriptor_path, "old-descriptor"),
    ];
    let prepublish_safe_paths = uploads
        .iter()
        .filter(|action| action.path() != "catalog.enc")
        .map(|action| action.path().to_string())
        .collect::<HashSet<_>>();

    let plan = CommitPlan::build(
        uploads,
        Vec::new(),
        &remote_inventory,
        &prepublish_safe_paths,
        Some("old-catalog".to_string()),
    )
    .unwrap();

    assert!(plan
        .prepublish
        .iter()
        .flatten()
        .all(|action| action.path() != descriptor_path));
    assert_eq!(
        plan.publish
            .iter()
            .map(RemoteAction::path)
            .collect::<Vec<_>>(),
        vec![descriptor_path, "catalog.enc"]
    );
}

#[test]
fn more_than_255_critical_updates_plus_catalog_is_rejected() {
    let mut uploads = Vec::new();
    let mut remote_inventory = vec![remote("catalog.enc", "old-catalog")];
    for index in 0..256 {
        let path = format!("recovery/nodes/{index:03}.enc");
        uploads.push(upload(path.clone(), index + 1));
        remote_inventory.push(remote(path, format!("old-{index}")));
    }
    uploads.push(upload("catalog.enc", 999));

    let error = CommitPlan::build(
        uploads,
        Vec::new(),
        &remote_inventory,
        &empty_prepublish_safe_paths(),
        Some("old-catalog".to_string()),
    )
    .unwrap_err();

    assert_eq!(
        error,
        StorageTransactionError::PublishBatchTooLarge {
            actions: 257,
            limit: MODELSCOPE_COMMIT_ACTION_LIMIT,
        }
    );
}

#[test]
fn planner_rejects_unmanaged_delete_paths() {
    let error = CommitPlan::build(
        vec![upload("catalog.enc", 1)],
        vec!["README.md".to_string()],
        &[remote("catalog.enc", "old-catalog")],
        &empty_prepublish_safe_paths(),
        Some("old-catalog".to_string()),
    )
    .unwrap_err();

    assert_eq!(
        error,
        StorageTransactionError::UnmanagedDeletePath("README.md".to_string())
    );
}

#[test]
fn planner_rejects_malformed_blob_oids_but_accepts_zero_size() {
    let malformed_oid = "A".repeat(64);
    let error = CommitPlan::build(
        vec![
            upload("catalog.enc", 1),
            RemoteAction::lfs_upsert(
                "objects/files/new/chunks/zero.lios",
                BlobCheckpoint::new(malformed_oid.clone(), 0),
            ),
        ],
        Vec::new(),
        &[],
        &empty_prepublish_safe_paths(),
        None,
    )
    .unwrap_err();

    assert_eq!(
        error,
        StorageTransactionError::InvalidBlobOid(malformed_oid)
    );

    let zero_size = CommitPlan::build(
        vec![RemoteAction::lfs_upsert(
            "catalog.enc",
            BlobCheckpoint::new("0".repeat(64), 0),
        )],
        Vec::new(),
        &[],
        &empty_prepublish_safe_paths(),
        None,
    )
    .unwrap();
    assert_eq!(zero_size.publish.len(), 1);
}

#[test]
fn planner_rejects_unmanaged_or_non_normal_upload_paths() {
    for invalid_path in [
        "",
        "/objects/file.enc",
        "C:/objects/file.enc",
        "objects\\file.enc",
        "objects/./file.enc",
        "objects/../file.enc",
        "objects//file.enc",
        "recovery/nodes/../../file.enc",
        "README.md",
    ] {
        let error = CommitPlan::build(
            vec![upload("catalog.enc", 1), upload(invalid_path, 2)],
            Vec::new(),
            &[],
            &empty_prepublish_safe_paths(),
            None,
        )
        .unwrap_err();

        assert_eq!(
            error,
            StorageTransactionError::UnmanagedUploadPath(invalid_path.to_string()),
            "accepted invalid upload path {invalid_path}"
        );
    }
}

#[test]
fn duplicate_catalog_actions_return_the_dedicated_error() {
    let error = CommitPlan::build(
        vec![upload("catalog.enc", 1), upload("catalog.enc", 2)],
        Vec::new(),
        &[],
        &empty_prepublish_safe_paths(),
        None,
    )
    .unwrap_err();

    assert_eq!(
        error,
        StorageTransactionError::DuplicateCatalogAction { count: 2 }
    );
}

#[test]
fn planner_rejects_duplicate_delete_paths() {
    let path = "objects/stale.enc";
    let error = CommitPlan::build(
        vec![upload("catalog.enc", 1)],
        vec![path.to_string(), path.to_string()],
        &[remote(path, "stale")],
        &empty_prepublish_safe_paths(),
        Some("old-catalog".to_string()),
    )
    .unwrap_err();

    assert_eq!(
        error,
        StorageTransactionError::DuplicateActionPath(path.to_string())
    );
}
