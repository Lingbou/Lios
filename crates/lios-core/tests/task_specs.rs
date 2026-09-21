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
        title: None,
    }
}

#[test]
fn transfer_spec_persists_the_confirmed_plan() {
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
    let transfer = TaskSpec::Transfer {
        space_id: "b".repeat(64),
        repo: repo(),
        plan: plan.clone(),
    };
    let mut pull_plan = plan;
    pull_plan.direction = TransferDirection::Pull;
    let pull = TaskSpec::Transfer {
        space_id: "b".repeat(64),
        repo: repo(),
        plan: pull_plan,
    };

    let transfer_roundtrip: TaskSpec =
        serde_json::from_str(&serde_json::to_string(&transfer).unwrap()).unwrap();
    let pull_roundtrip: TaskSpec =
        serde_json::from_str(&serde_json::to_string(&pull).unwrap()).unwrap();
    assert_eq!(transfer_roundtrip.label(), "upload");
    assert_eq!(pull_roundtrip.label(), "download");
}
