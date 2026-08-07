use lios_application::location::LocalLocation;
use lios_application::transfer_planner::{PlanActionKind, PlanOptions};
use lios_application::transfer_request::{prepare_pull, RemoteSource};
use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind};
use tempfile::tempdir;

fn remote_dir() -> CatalogTreeNode {
    CatalogTreeNode {
        id: "dir-id".to_string(),
        name: "photos".to_string(),
        updated_at: "now".to_string(),
        kind: CatalogTreeNodeKind::Directory {
            children: vec![CatalogTreeNode {
                id: "file-id".to_string(),
                name: "image.jpg".to_string(),
                updated_at: "now".to_string(),
                kind: CatalogTreeNodeKind::File {
                    original_size: 5,
                    sha256: "a".repeat(64),
                    object_id: "object".to_string(),
                    chunk_count: 1,
                },
            }],
        },
    }
}

#[test]
fn remote_directory_trailing_slash_maps_contents_symmetrically() {
    let temp = tempdir().unwrap();
    let destination = LocalLocation {
        path: temp.path().join("restore"),
        trailing_slash: false,
    };

    let directory = prepare_pull(
        &[RemoteSource {
            node: remote_dir(),
            trailing_slash: false,
        }],
        &destination,
        &PlanOptions::default(),
    )
    .unwrap();
    let contents = prepare_pull(
        &[RemoteSource {
            node: remote_dir(),
            trailing_slash: true,
        }],
        &destination,
        &PlanOptions::default(),
    )
    .unwrap();

    assert!(directory.plan.action("photos/image.jpg").is_some());
    assert!(contents.plan.action("image.jpg").is_some());
    assert_eq!(
        contents.plan.action("image.jpg").unwrap().kind,
        PlanActionKind::Create
    );
}

#[test]
fn sync_contents_to_local_root_deletes_destination_only_root_entries() {
    let temp = tempdir().unwrap();
    let destination_path = temp.path().join("restore");
    std::fs::create_dir(&destination_path).unwrap();
    std::fs::write(destination_path.join("remove.txt"), b"stale").unwrap();
    let destination = LocalLocation {
        path: destination_path,
        trailing_slash: false,
    };

    let prepared = prepare_pull(
        &[RemoteSource {
            node: remote_dir(),
            trailing_slash: true,
        }],
        &destination,
        &PlanOptions {
            delete: true,
            yes: true,
            ..PlanOptions::default()
        },
    )
    .unwrap();

    assert_eq!(
        prepared.plan.action("remove.txt").unwrap().kind,
        PlanActionKind::Delete
    );
}

#[test]
fn sync_excludes_are_relative_to_the_selected_remote_root() {
    let temp = tempdir().unwrap();
    let destination = LocalLocation {
        path: temp.path().join("restore"),
        trailing_slash: false,
    };

    let prepared = prepare_pull(
        &[RemoteSource {
            node: remote_dir(),
            trailing_slash: false,
        }],
        &destination,
        &PlanOptions {
            exclude: vec!["image.jpg".to_string()],
            ..PlanOptions::default()
        },
    )
    .unwrap();

    assert!(prepared.plan.action("photos").is_some());
    assert!(prepared.plan.action("photos/image.jpg").is_none());
}
