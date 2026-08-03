use std::path::PathBuf;

use lios_core::config::{RepoConfig, MODELSCOPE_ENDPOINT};
use lios_core::tasks::{
    PersistedTransferAction, PersistedTransferPlan, TaskSpec, TransferActionKind,
    TransferActionState, TransferDirection, TransferEntryKind,
};

fn repo() -> RepoConfig {
    RepoConfig {
        namespace: "allen".to_string(),
        dataset: "photos".to_string(),
        endpoint: MODELSCOPE_ENDPOINT.to_string(),
    }
}

#[test]
fn copy_and_sync_specs_persist_the_confirmed_plan() {
    let plan = PersistedTransferPlan {
        direction: TransferDirection::Push,
        source_operand: "photos/".to_string(),
        destination_operand: "photos:/backup".to_string(),
        source_trailing_slash: true,
        excludes: vec!["cache/**".to_string()],
        remote_catalog_baseline: Some("abc".to_string()),
        delete_scope: None,
        actions: vec![PersistedTransferAction {
            relative_path: "2026/image.jpg".to_string(),
            source_path: Some(PathBuf::from("/tmp/photos/2026/image.jpg")),
            remote_node_id: None,
            local_destination_path: None,
            kind: TransferActionKind::Create,
            entry_kind: TransferEntryKind::File,
            source_sha256: Some("a".repeat(64)),
            source_fingerprint: Some("dev:ino:size:mtime".to_string()),
            size: 10,
            destination_fingerprint: None,
            state: TransferActionState::Pending,
        }],
    };
    let copy = TaskSpec::Copy {
        account_id: "a".repeat(64),
        space_id: "b".repeat(64),
        repo: repo(),
        plan: plan.clone(),
    };
    let sync = TaskSpec::Sync {
        account_id: "a".repeat(64),
        space_id: "b".repeat(64),
        repo: repo(),
        plan,
    };

    let copy_roundtrip: TaskSpec =
        serde_json::from_str(&serde_json::to_string(&copy).unwrap()).unwrap();
    let sync_roundtrip: TaskSpec =
        serde_json::from_str(&serde_json::to_string(&sync).unwrap()).unwrap();
    assert_eq!(copy_roundtrip.label(), "copy");
    assert_eq!(sync_roundtrip.label(), "sync");
}

#[test]
fn legacy_delete_spec_remains_deserializable() {
    let json = format!(
        r#"{{"kind":"delete","account_id":"{}","space_id":"{}","repo":{{"namespace":"allen","dataset":"photos","endpoint":"https://modelscope.cn"}},"node_ids":["node"]}}"#,
        "a".repeat(64),
        "b".repeat(64)
    );

    let spec: TaskSpec = serde_json::from_str(&json).unwrap();

    assert_eq!(spec.label(), "delete");
}
