use std::fs;
use std::path::Path;

use lios_core::cache::{cleanup_temporary_staging, prune_unreferenced_staging};
use tempfile::tempdir;

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

#[test]
fn cleanup_temporary_staging_removes_tmp_and_interrupted_downloads() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let keep = staging.join("objects/files/live/chunks/chunk.lios");
    let interrupted = staging.join("objects/files/live/chunks/chunk.download");
    let tmp_file = staging.join(".tmp/chunks/work/chunk.lios");
    write_file(&keep, b"keep");
    write_file(&interrupted, b"partial");
    write_file(&tmp_file, b"temporary");

    let report = cleanup_temporary_staging(&staging).unwrap();

    assert!(keep.exists());
    assert!(!interrupted.exists());
    assert!(!staging.join(".tmp").exists());
    assert_eq!(report.files_removed, 2);
    assert_eq!(
        report.bytes_removed,
        b"partial".len() as u64 + b"temporary".len() as u64
    );
}

#[test]
fn prune_unreferenced_staging_preserves_catalog_and_referenced_objects() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let catalog = staging.join("catalog.enc");
    let referenced = staging.join("objects/files/live/chunks/keep.lios");
    let stale = staging.join("objects/files/stale/chunks/drop.lios");
    write_file(&catalog, b"catalog");
    write_file(&referenced, b"keep");
    write_file(&stale, b"stale");

    let report = prune_unreferenced_staging(
        &staging,
        [
            "catalog.enc".to_string(),
            "objects/files/live/chunks/keep.lios".to_string(),
        ],
    )
    .unwrap();

    assert!(catalog.exists());
    assert!(referenced.exists());
    assert!(!stale.exists());
    assert_eq!(report.files_removed, 1);
    assert_eq!(report.bytes_removed, b"stale".len() as u64);
}
