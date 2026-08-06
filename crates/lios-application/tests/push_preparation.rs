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

#[test]
fn sync_contents_to_space_root_deletes_destination_only_root_entries() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("dir");
    std::fs::create_dir(&source).unwrap();
    std::fs::write(source.join("keep.txt"), b"new").unwrap();

    let prepared = prepare_push(
        &[LocalLocation {
            path: source,
            trailing_slash: true,
        }],
        "/",
        &[
            TreeEntry::file("keep.txt", "old", 3),
            TreeEntry::file("remove.txt", "stale", 5),
        ],
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
fn local_file_with_trailing_slash_is_rejected() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("file.txt");
    std::fs::write(&source, b"hello").unwrap();

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

    assert!(error.message.contains("trailing slash"));
}

#[test]
fn sync_excludes_are_relative_to_the_selected_source_root() {
    let temp = tempdir().unwrap();
    let source = temp.path().join("dir");
    std::fs::create_dir_all(source.join("cache")).unwrap();
    std::fs::write(source.join("keep.txt"), b"new").unwrap();
    std::fs::write(source.join("cache/new.bin"), b"new").unwrap();

    let prepared = prepare_push(
        &[LocalLocation {
            path: source,
            trailing_slash: true,
        }],
        "/backup",
        &[
            TreeEntry::directory("backup"),
            TreeEntry::directory("backup/cache"),
            TreeEntry::file("backup/cache/old.bin", "stale", 5),
        ],
        &PlanOptions {
            delete: true,
            exclude: vec!["cache/**".to_string()],
            yes: true,
            ..PlanOptions::default()
        },
    )
    .unwrap();

    assert!(prepared.plan.action("backup/keep.txt").is_some());
    assert!(prepared.plan.action("backup/cache").is_none());
    assert!(prepared.plan.action("backup/cache/new.bin").is_none());
    assert!(prepared.plan.action("backup/cache/old.bin").is_none());
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
