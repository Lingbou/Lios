use lios_core::storage::{plan_current_snapshot_changes, LocalStorageObject, StorageObject};

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
