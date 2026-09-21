use lios_application::location::LocalLocation;
use lios_application::transfer_planner::{PlanActionKind, PlanOptions};
use lios_application::transfer_request::{prepare_download, prepare_pull, RemoteSource};
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

fn remote_file(name: &str, sha256: &str, size: u64) -> CatalogTreeNode {
    CatalogTreeNode {
        id: format!("{name}-id"),
        name: name.to_string(),
        updated_at: "now".to_string(),
        kind: CatalogTreeNodeKind::File {
            original_size: size,
            sha256: sha256.to_string(),
            object_id: format!("{name}-object"),
            chunk_count: 1,
        },
    }
}

#[test]
fn download_preserves_existing_conflicting_file() {
    let temp = tempdir().unwrap();
    let destination = temp.path().join("restore");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("report.txt"), b"local").unwrap();

    let prepared = prepare_download(
        &[RemoteSource {
            node: remote_file("report.txt", &"a".repeat(64), 6),
            trailing_slash: false,
        }],
        &LocalLocation {
            path: destination.clone(),
            trailing_slash: true,
        },
    )
    .unwrap();

    let action = prepared.plan.action("report.txt").unwrap();
    assert_eq!(action.kind, PlanActionKind::Create);
    assert_eq!(
        prepared.destination_paths.get("report.txt"),
        Some(&destination.join("report (restored 1).txt"))
    );
    assert!(!prepared.destination_fingerprints.contains_key("report.txt"));
    assert_eq!(
        std::fs::read(destination.join("report.txt")).unwrap(),
        b"local"
    );
}

#[cfg(unix)]
#[test]
fn download_ignores_unrelated_destination_entries() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let destination = temp.path().join("restore");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("unrelated.bin"), b"data").unwrap();
    symlink("missing-target", destination.join("unrelated-link")).unwrap();

    let prepared = prepare_download(
        &[RemoteSource {
            node: remote_file("report.txt", &"a".repeat(64), 6),
            trailing_slash: false,
        }],
        &LocalLocation {
            path: destination.clone(),
            trailing_slash: true,
        },
    )
    .unwrap();

    assert_eq!(
        prepared.plan.action("report.txt").unwrap().kind,
        PlanActionKind::Create
    );
    assert!(destination.join("unrelated-link").is_symlink());
}

#[test]
fn download_uses_next_restored_name_when_first_is_taken() {
    let temp = tempdir().unwrap();
    let destination = temp.path().join("restore");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("report.txt"), b"local").unwrap();
    std::fs::write(destination.join("report (restored 1).txt"), b"older").unwrap();

    let prepared = prepare_download(
        &[RemoteSource {
            node: remote_file("report.txt", &"a".repeat(64), 6),
            trailing_slash: false,
        }],
        &LocalLocation {
            path: destination.clone(),
            trailing_slash: true,
        },
    )
    .unwrap();

    assert_eq!(
        prepared.destination_paths.get("report.txt"),
        Some(&destination.join("report (restored 2).txt"))
    );
}

#[test]
fn download_skips_identical_existing_file_without_rename() {
    let temp = tempdir().unwrap();
    let destination = temp.path().join("restore");
    std::fs::create_dir(&destination).unwrap();
    std::fs::write(destination.join("report.txt"), b"remote").unwrap();
    let local_sha256 = lios_application::sha256_hex_file(&destination.join("report.txt")).unwrap();

    let prepared = prepare_download(
        &[RemoteSource {
            node: remote_file("report.txt", &local_sha256, 6),
            trailing_slash: false,
        }],
        &LocalLocation {
            path: destination.clone(),
            trailing_slash: true,
        },
    )
    .unwrap();

    assert_eq!(
        prepared.plan.action("report.txt").unwrap().kind,
        PlanActionKind::Skip
    );
    assert_eq!(
        prepared.destination_paths.get("report.txt"),
        Some(&destination.join("report.txt"))
    );
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
