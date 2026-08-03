use lios_application::service::Application;
use lios_core::config::{LiosPaths, RepoConfig, MODELSCOPE_ENDPOINT};
use lios_core::tasks::{
    PersistedTransferAction, PersistedTransferPlan, TaskItemState, TaskStore, TransferActionKind,
    TransferActionState, TransferDirection, TransferEntryKind,
};
use tempfile::tempdir;

fn action(path: &str, kind: TransferActionKind) -> PersistedTransferAction {
    PersistedTransferAction {
        relative_path: path.to_string(),
        source_path: None,
        remote_node_id: None,
        local_destination_path: None,
        kind,
        entry_kind: TransferEntryKind::Directory,
        source_sha256: None,
        source_fingerprint: None,
        size: 0,
        destination_fingerprint: None,
        state: if kind == TransferActionKind::Skip {
            TransferActionState::Skipped
        } else {
            TransferActionState::Pending
        },
    }
}

#[test]
fn copy_submission_persists_every_planned_action_as_a_journal_item() {
    let temp = tempdir().unwrap();
    let paths = LiosPaths::from_home(temp.path());
    let application = Application::new(paths.clone()).unwrap();
    application.setup().unwrap();
    let actions = vec![
        action("created", TransferActionKind::Create),
        action("unchanged", TransferActionKind::Skip),
        action("removed", TransferActionKind::Delete),
    ];
    let plan = PersistedTransferPlan {
        direction: TransferDirection::Push,
        source_operand: "local/".to_string(),
        destination_operand: "photos:/".to_string(),
        source_trailing_slash: true,
        excludes: Vec::new(),
        remote_catalog_baseline: Some("a".repeat(64)),
        delete_scope: Some("photos:/".to_string()),
        actions,
    };
    let task = application
        .queue_copy(
            RepoConfig {
                namespace: "allen".to_string(),
                dataset: "photos".to_string(),
                endpoint: MODELSCOPE_ENDPOINT.to_string(),
            },
            plan,
        )
        .unwrap();

    let items = TaskStore::open(&paths.database)
        .unwrap()
        .list_items(task.id)
        .unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items[0].relative_path.as_deref(),
        Some(std::path::Path::new("created"))
    );
    assert_eq!(items[0].state, TaskItemState::Queued);
    assert_eq!(items[1].state, TaskItemState::Skipped);
    assert_eq!(items[2].state, TaskItemState::Queued);
}
