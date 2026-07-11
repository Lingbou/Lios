use std::fs;
use std::path::Path;

use lios_core::{
    catalog::{
        snapshot_source_files, verify_source_file_unchanged, Catalog, CatalogTreeNodeKind,
        PackOutcome, SkippedPathReason,
    },
    crypto::KeyFile,
    pack::{PackOptions, PackSource},
    LiosError,
};
use tempfile::tempdir;

fn child_names(catalog: &Catalog, key: &KeyFile) -> Vec<String> {
    let tree = catalog.decrypt_tree(key).unwrap();
    match tree.kind {
        CatalogTreeNodeKind::Directory { children } => {
            children.into_iter().map(|child| child.name).collect()
        }
        CatalogTreeNodeKind::File { .. } => panic!("expected directory root"),
    }
}

fn assert_skipped_error<T>(result: Result<T, LiosError>, skipped: &Path) {
    let Err(LiosError::Unsupported(message)) = result else {
        panic!("expected skipped-path error");
    };
    assert_eq!(message, format!("skipped 1 path: {}", skipped.display()));
}

#[test]
fn source_file_snapshots_preserve_relative_names_sizes_and_mtime() {
    let tmp = tempdir().unwrap();
    let album = tmp.path().join("album");
    let nested = album.join("nested");
    let empty = album.join("empty");
    fs::create_dir_all(&nested).unwrap();
    fs::create_dir_all(&empty).unwrap();
    fs::write(album.join("cover.bin"), b"cover").unwrap();
    fs::write(nested.join("track.bin"), b"track-data").unwrap();

    let report = snapshot_source_files(std::slice::from_ref(&album)).unwrap();

    assert!(report.skipped_paths.is_empty());
    assert_eq!(report.directories.len(), 3);
    assert_eq!(report.directories[0].relative_path, Path::new("album"));
    assert_eq!(
        report.directories[1].relative_path,
        Path::new("album/empty")
    );
    assert_eq!(
        report.directories[2].relative_path,
        Path::new("album/nested")
    );
    assert_eq!(report.files.len(), 2);
    assert_eq!(report.files[0].source_path, album.join("cover.bin"));
    assert_eq!(report.files[0].relative_path, Path::new("album/cover.bin"));
    assert_eq!(report.files[0].size, 5);
    assert!(report.files[0].modified_at_ns.is_some());
    assert_eq!(report.files[1].source_path, nested.join("track.bin"));
    assert_eq!(
        report.files[1].relative_path,
        Path::new("album/nested/track.bin")
    );
    assert_eq!(report.files[1].size, 10);
}

#[test]
fn source_file_verification_rejects_changes_during_packing() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    fs::write(&source, [1u8; 4]).unwrap();
    let snapshot = snapshot_source_files(std::slice::from_ref(&source))
        .unwrap()
        .files
        .remove(0);
    fs::write(&source, [2u8; 8]).unwrap();

    let error = verify_source_file_unchanged(&snapshot).unwrap_err();

    assert!(matches!(
        error,
        LiosError::Unsupported(message)
            if message.contains("source file changed while it was being packed")
    ));
}

#[test]
fn source_file_verification_reports_a_file_removed_during_packing() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("source.bin");
    fs::write(&source, [1u8; 4]).unwrap();
    let snapshot = snapshot_source_files(std::slice::from_ref(&source))
        .unwrap()
        .files
        .remove(0);
    fs::remove_file(&source).unwrap();

    let error = verify_source_file_unchanged(&snapshot).unwrap_err();

    assert!(matches!(
        error,
        LiosError::Unsupported(message)
            if message.contains("source file no longer exists")
                && message.contains("source.bin")
    ));
}

#[test]
fn folder_upload_report_records_the_exact_packed_source_snapshot() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("album");
    fs::create_dir_all(source.join("empty")).unwrap();
    fs::write(source.join("track.bin"), b"track").unwrap();
    let expected = snapshot_source_files(std::slice::from_ref(&source)).unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let report = catalog
        .add_paths_to_folder_with_progress_and_report(
            &root_id,
            std::slice::from_ref(&source),
            &[],
            &key,
            PackOptions {
                chunk_size: 4,
                staging_dir: staging,
            },
            |_| {},
        )
        .unwrap();

    assert_eq!(report.source_snapshot(), expected);
}

#[test]
fn packed_snapshot_exposes_a_temporary_file_that_disappears_before_post_validation() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("album");
    fs::create_dir_all(&source).unwrap();
    fs::write(source.join("original.bin"), b"original").unwrap();
    let queued_snapshot = snapshot_source_files(std::slice::from_ref(&source)).unwrap();
    let temporary = source.join("temporary.bin");
    fs::write(&temporary, b"temporary").unwrap();
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let report = catalog
        .add_paths_to_folder_with_progress_and_report(
            &root_id,
            std::slice::from_ref(&source),
            &[],
            &key,
            PackOptions {
                chunk_size: 4,
                staging_dir: staging,
            },
            |_| {},
        )
        .unwrap();
    fs::remove_file(temporary).unwrap();

    assert_ne!(report.source_snapshot(), queued_snapshot);
    assert_eq!(
        snapshot_source_files(std::slice::from_ref(&source)).unwrap(),
        queued_snapshot
    );
}

#[test]
fn logical_item_names_reject_unsafe_path_components() {
    let tmp = tempdir().unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, tmp.path().join("staging")).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    for name in [
        "",
        " ",
        ".",
        "..",
        "nested/name",
        "nested\\name",
        "trailing ",
        "trailing.",
    ] {
        assert!(
            catalog.create_folder(&root_id, name, &key).is_err(),
            "unsafe logical name was accepted: {name:?}"
        );
    }
}

#[test]
fn logical_item_names_reject_windows_reserved_devices_with_extensions() {
    let tmp = tempdir().unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, tmp.path().join("staging")).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    for name in [
        "CON",
        "con.txt",
        "PRN.log",
        "aux",
        "NUL.bin",
        "COM1",
        "com9.archive",
        "LPT1",
        "lpt9.txt",
    ] {
        assert!(
            catalog.create_folder(&root_id, name, &key).is_err(),
            "reserved logical name was accepted: {name:?}"
        );
    }
}

#[test]
fn new_logical_names_reject_windows_illegal_characters_and_controls() {
    let tmp = tempdir().unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, tmp.path().join("staging")).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    for name in [
        "bad:name",
        "bad*name",
        "bad?name",
        "bad\"name",
        "bad<name",
        "bad>name",
        "bad|name",
        "bad\u{1f}name",
    ] {
        assert!(
            catalog.create_folder(&root_id, name, &key).is_err(),
            "Windows-illegal logical name was accepted: {name:?}"
        );
    }

    catalog.create_folder(&root_id, "valid", &key).unwrap();
    let tree = catalog.decrypt_tree(&key).unwrap();
    let CatalogTreeNodeKind::Directory { children } = tree.kind else {
        panic!("expected directory root");
    };
    let valid_id = children
        .iter()
        .find(|child| child.name == "valid")
        .unwrap()
        .id
        .clone();
    assert!(catalog.rename_node(&valid_id, "bad:name", &key).is_err());
    assert!(catalog
        .preview_upload_conflicts(&root_id, &[tmp.path().join("bad:name.txt")], &key)
        .is_err());
}

#[test]
fn upload_name_validation_rejects_reserved_devices_before_file_access() {
    let tmp = tempdir().unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, tmp.path().join("staging")).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;
    let reserved_path = tmp.path().join("CON.txt");

    let result = catalog.preview_upload_conflicts(&root_id, &[reserved_path], &key);

    assert!(result.is_err());
}

#[test]
fn empty_catalog_root_rejects_unsafe_logical_name() {
    let tmp = tempdir().unwrap();
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();

    let result = Catalog::initialize_empty("..", &key, tmp.path().join("staging"));

    assert!(result.is_err());
}

#[test]
fn directory_pack_skips_linked_directory_subtrees() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("album");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(source.join("included.txt"), b"included").unwrap();
    fs::write(outside.join("excluded.txt"), b"excluded").unwrap();
    create_directory_link(&outside, &source.join("linked"));

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let outcome = Catalog::pack_with_report(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("staging"),
        },
    )
    .unwrap();
    let PackOutcome::Packed { catalog, report } = outcome else {
        panic!("directory with ordinary files should produce a catalog");
    };

    assert_eq!(child_names(&catalog, &key), vec!["included.txt"]);
    assert_eq!(report.skipped_paths.len(), 1);
    assert_eq!(
        report.skipped_paths[0].path,
        tmp.path().join("album/linked")
    );
    assert_eq!(
        report.skipped_paths[0].reason,
        SkippedPathReason::SymbolicLinkOrJunction
    );
}

#[test]
fn root_source_link_returns_local_report_without_catalog() {
    let tmp = tempdir().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("excluded.txt"), b"excluded").unwrap();
    let linked = tmp.path().join("linked");
    create_directory_link(&outside, &linked);
    let staging = tmp.path().join("staging");
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();

    let outcome = Catalog::pack_with_report(
        PackSource::Path(linked.clone()),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging.clone(),
        },
    )
    .unwrap();

    let PackOutcome::Skipped { report } = outcome else {
        panic!("root link should be reported without a catalog");
    };
    assert_eq!(report.skipped_paths.len(), 1);
    assert_eq!(report.skipped_paths[0].path, linked);
    assert_eq!(
        report.skipped_paths[0].reason,
        SkippedPathReason::SymbolicLinkOrJunction
    );
    let serialized = serde_json::to_value(&report).unwrap();
    assert_eq!(
        serialized["skipped_paths"][0]["reason"],
        "symbolic_link_or_junction"
    );
    assert!(!staging.join("catalog.enc").exists());
}

#[test]
fn catalog_pack_fails_when_nested_path_is_skipped() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("album");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(source.join("included.txt"), b"included").unwrap();
    let linked = source.join("linked");
    create_directory_link(&outside, &linked);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();

    let result = Catalog::pack(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("staging"),
        },
    );

    assert_skipped_error(result, &linked);
}

#[test]
fn catalog_pack_with_progress_fails_when_nested_path_is_skipped() {
    let tmp = tempdir().unwrap();
    let source = tmp.path().join("album");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&outside).unwrap();
    fs::write(source.join("included.txt"), b"included").unwrap();
    let linked = source.join("linked");
    create_directory_link(&outside, &linked);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();

    let result = Catalog::pack_with_progress(
        PackSource::Path(source),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("staging"),
        },
        |_| {},
    );

    assert_skipped_error(result, &linked);
}

#[test]
fn catalog_only_pack_rejects_root_source_link() {
    let tmp = tempdir().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let linked = tmp.path().join("linked");
    create_directory_link(&outside, &linked);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();

    let result = Catalog::pack(
        PackSource::Path(linked.clone()),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("staging"),
        },
    );

    assert_skipped_error(result, &linked);
}

#[cfg(unix)]
#[test]
fn root_link_is_classified_before_reserved_name_validation() {
    let tmp = tempdir().unwrap();
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let linked = tmp.path().join("CON");
    create_directory_link(&outside, &linked);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();

    let outcome = Catalog::pack_with_report(
        PackSource::Path(linked.clone()),
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: tmp.path().join("staging"),
        },
    )
    .unwrap();

    let PackOutcome::Skipped { report } = outcome else {
        panic!("reserved root link should be skipped before name validation");
    };
    assert_eq!(report.skipped_paths[0].path, linked);
}

#[test]
fn folder_upload_skips_linked_directory_and_continues() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("excluded.txt"), b"excluded").unwrap();
    let linked = tmp.path().join("linked");
    create_directory_link(&outside, &linked);
    let included = tmp.path().join("included.txt");
    fs::write(&included, b"included").unwrap();

    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let report = catalog
        .add_paths_to_folder_with_report(
            &root_id,
            &[linked.clone(), included],
            &[],
            &key,
            PackOptions {
                chunk_size: 4,
                staging_dir: staging,
            },
        )
        .unwrap();

    assert_eq!(child_names(&catalog, &key), vec!["included.txt"]);
    assert_eq!(report.skipped_paths.len(), 1);
    assert_eq!(report.skipped_paths[0].path, linked);
    assert_eq!(
        report.skipped_paths[0].reason,
        SkippedPathReason::SymbolicLinkOrJunction
    );
    let serialized = serde_json::to_value(&report.skipped_paths[0]).unwrap();
    assert_eq!(serialized["reason"], "symbolic_link_or_junction");
}

#[test]
fn folder_upload_compatibility_api_fails_when_path_is_skipped() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let linked = tmp.path().join("linked");
    create_directory_link(&outside, &linked);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let result = catalog.add_paths_to_folder(
        &root_id,
        std::slice::from_ref(&linked),
        &[],
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
    );

    assert_skipped_error(result, &linked);
}

#[test]
fn folder_upload_progress_compatibility_api_fails_when_path_is_skipped() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let outside = tmp.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let linked = tmp.path().join("linked");
    create_directory_link(&outside, &linked);
    let key = KeyFile::generate_to_path(tmp.path().join("key")).unwrap();
    let catalog = Catalog::initialize_empty("Space", &key, staging.clone()).unwrap();
    let root_id = catalog.decrypt_tree(&key).unwrap().id;

    let result = catalog.add_paths_to_folder_with_progress(
        &root_id,
        std::slice::from_ref(&linked),
        &[],
        &key,
        PackOptions {
            chunk_size: 4,
            staging_dir: staging,
        },
        |_| {},
    );

    assert_skipped_error(result, &linked);
}

#[cfg(unix)]
fn create_directory_link(target: &Path, link: &Path) {
    std::os::unix::fs::symlink(target, link).unwrap();
}

#[cfg(windows)]
fn create_directory_link(target: &Path, link: &Path) {
    let output = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "failed to create junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}
