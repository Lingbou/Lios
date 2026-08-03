use lios_application::location::LocalLocation;
use lios_application::transfer_planner::{PlanActionKind, PlanOptions, TreeEntry};
use lios_application::transfer_request::prepare_push;
use tempfile::tempdir;

#[test]
fn directory_trailing_slash_changes_only_the_destination_mapping() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("dir");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("file.txt"), b"hello").unwrap();

    let with_directory = prepare_push(
        &[LocalLocation {
            path: source.clone(),
            trailing_slash: false,
        }],
        "/backup",
        &[],
        &PlanOptions::default(),
    )
    .unwrap();
    let contents_only = prepare_push(
        &[LocalLocation {
            path: source,
            trailing_slash: true,
        }],
        "/backup",
        &[],
        &PlanOptions::default(),
    )
    .unwrap();

    assert!(with_directory.plan.action("backup/dir/file.txt").is_some());
    assert!(contents_only.plan.action("backup/file.txt").is_some());
    assert!(contents_only.plan.action("backup/dir/file.txt").is_none());
}

#[test]
fn push_uses_sha256_to_skip_identical_remote_files() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("file.txt");
    std::fs::write(&source, b"hello").unwrap();
    let sha = lios_application::sha256_hex_file(&source).unwrap();

    let prepared = prepare_push(
        &[LocalLocation {
            path: source,
            trailing_slash: false,
        }],
        "/backup",
        &[
            TreeEntry::directory("backup"),
            TreeEntry::file("backup/file.txt", sha, 5),
        ],
        &PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        prepared.plan.action("backup/file.txt").unwrap().kind,
        PlanActionKind::Skip
    );
}

#[cfg(unix)]
#[test]
fn push_rejects_symbolic_links_instead_of_following_them() {
    use std::os::unix::fs::symlink;

    let temp = tempdir().unwrap();
    let source = temp.path().join("dir");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("real.txt"), b"hello").unwrap();
    symlink(source.join("real.txt"), source.join("link.txt")).unwrap();

    let error = prepare_push(
        &[LocalLocation {
            path: source,
            trailing_slash: true,
        }],
        "/backup",
        &[],
        &PlanOptions::default(),
    )
    .unwrap_err();

    assert!(error.message.contains("symbolic links or junctions"));
}
