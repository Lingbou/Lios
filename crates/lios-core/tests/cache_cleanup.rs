use std::fs;
use std::path::Path;

use lios_core::cache::{
    cleanup_temporary_staging, prune_unreferenced_staging, reset_shared_staging,
};
use lios_core::config::LiosPaths;
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

#[test]
fn reset_shared_staging_preserves_task_scopes_and_removes_shared_cache() {
    let tmp = tempdir().unwrap();
    let paths = LiosPaths::from_home(tmp.path());
    paths.ensure_dirs().unwrap();
    let space_id = "b".repeat(64);
    let task_file = paths
        .staging
        .join(&space_id)
        .join(Uuid::new_v4().to_string())
        .join("chunk.lios");
    let catalog = paths.staging.join("catalog.enc");
    write_file(&task_file, b"task");
    write_file(&catalog, b"catalog");

    let report = reset_shared_staging(&paths).unwrap();

    assert!(task_file.exists());
    assert!(!catalog.exists());
    assert_eq!(report.files_removed, 1);
}

use lios_core::cache::{cleanup_all_inactive_staging, cleanup_task_staging};
use std::collections::HashSet;
use uuid::Uuid;

#[test]
fn cleanup_task_staging_removes_task_folder_and_empty_parents() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let space_id = "b".repeat(64);
    let task_id = Uuid::new_v4();

    let task_file = staging
        .join(&space_id)
        .join(task_id.to_string())
        .join("chunks/data.lios");
    write_file(&task_file, b"staged data");

    let report = cleanup_task_staging(&staging, &space_id, task_id).unwrap();

    assert!(!task_file.exists());
    assert!(!staging.join(&space_id).exists());
    assert_eq!(report.files_removed, 1);
    assert_eq!(report.bytes_removed, b"staged data".len() as u64);
}

#[test]
fn cleanup_all_inactive_staging_prunes_only_terminal_and_orphaned_tasks() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let space_id = "b".repeat(64);
    let active_task_id = Uuid::new_v4();
    let completed_task_id = Uuid::new_v4();

    let active_file = staging
        .join(&space_id)
        .join(active_task_id.to_string())
        .join("chunks/active.lios");
    let completed_file = staging
        .join(&space_id)
        .join(completed_task_id.to_string())
        .join("chunks/completed.lios");
    write_file(&active_file, b"active");
    write_file(&completed_file, b"completed chunk 12345");

    let mut active_ids = HashSet::new();
    active_ids.insert(active_task_id);

    let report = cleanup_all_inactive_staging(&staging, &active_ids).unwrap();

    assert!(active_file.exists());
    assert!(!completed_file.exists());
    assert_eq!(report.files_removed, 1);
    assert_eq!(report.bytes_removed, b"completed chunk 12345".len() as u64);
}

#[test]
fn inactive_sweep_preserves_shared_entries_and_active_downloads() {
    let tmp = tempdir().unwrap();
    let staging = tmp.path().join("staging");
    let space = "b".repeat(64);
    let active_id = Uuid::new_v4();
    let inactive_id = Uuid::new_v4();
    let active_download = staging
        .join(&space)
        .join(active_id.to_string())
        .join("chunk.download");
    let inactive_file = staging
        .join(&space)
        .join(inactive_id.to_string())
        .join("chunk.lios");
    write_file(&staging.join("catalog.enc"), b"catalog");
    write_file(&staging.join("objects/files/live/chunk.lios"), b"object");
    write_file(&active_download, b"partial");
    write_file(&inactive_file, b"stale");

    cleanup_all_inactive_staging(&staging, &HashSet::from([active_id])).unwrap();

    assert!(staging.join("catalog.enc").exists());
    assert!(staging.join("objects/files/live/chunk.lios").exists());
    assert!(active_download.exists());
    assert!(!inactive_file.exists());
}
