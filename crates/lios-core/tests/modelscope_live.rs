use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use lios_core::{
    catalog::{Catalog, CatalogSelection, CATALOG_FILE},
    crypto::KeyFile,
    modelscope::ModelScopeAdapter,
    pack::{PackOptions, PackSource},
    restore::{RestoreConflictPolicy, RestoreOptions},
    storage::{
        current_catalog_sha256, BlobSpec, BlobValidation, CommitPlan, RemoteAction, StorageAdapter,
    },
};
use sha2::{Digest, Sha256};
use tempfile::tempdir;

fn required_env(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("{name} must be set for the live test"))
}

fn endpoint() -> String {
    std::env::var("LIOS_MODELSCOPE_ENDPOINT")
        .unwrap_or_else(|_| "https://modelscope.cn".to_string())
}

fn dataset_name() -> String {
    std::env::var("LIOS_MODELSCOPE_DATASET").unwrap_or_else(|_| "lios-e2e-smoke".to_string())
}

fn write_file(path: &Path, contents: &[u8]) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, contents).unwrap();
}

fn staged_files(staging: &Path) -> Vec<(PathBuf, String)> {
    let mut files = Vec::new();
    for entry in walkdir::WalkDir::new(staging) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let relative = entry.path().strip_prefix(staging).unwrap();
            let remote = relative.to_string_lossy().replace('\\', "/");
            files.push((entry.path().to_path_buf(), remote));
        }
    }
    files.sort_by(|a, b| {
        let a_catalog = a.1 == CATALOG_FILE;
        let b_catalog = b.1 == CATALOG_FILE;
        a_catalog.cmp(&b_catalog).then_with(|| a.1.cmp(&b.1))
    });
    files
}

fn read_all_files(root: &Path) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    for entry in walkdir::WalkDir::new(root) {
        let entry = entry.unwrap();
        if entry.file_type().is_file() {
            let relative = entry
                .path()
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            let sha = hex::encode(Sha256::digest(fs::read(entry.path()).unwrap()));
            entries.push((relative, sha));
        }
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

async fn commit_staged_snapshot(
    adapter: &ModelScopeAdapter,
    namespace: &str,
    dataset: &str,
    staging: &Path,
    delete_paths: Vec<String>,
    message: &str,
) -> lios_core::Result<()> {
    let remote_inventory = adapter.list_objects(namespace, dataset, "").await?;
    let inventory_paths = remote_inventory
        .iter()
        .map(|object| object.path.as_str())
        .collect::<HashSet<_>>();
    let staged = staged_files(staging);
    let mut blobs = Vec::with_capacity(staged.len());
    let mut staged_paths = Vec::with_capacity(staged.len());
    for (local_path, remote_path) in staged {
        blobs.push(BlobSpec::from_path(local_path).await?);
        staged_paths.push(remote_path);
    }
    let validations = adapter.validate_blobs(namespace, dataset, &blobs).await?;
    let mut actions = Vec::with_capacity(staged_paths.len());
    for ((remote_path, blob), validation) in staged_paths.iter().zip(&blobs).zip(validations) {
        let checkpoint = match validation {
            BlobValidation::Reusable(checkpoint) => checkpoint,
            BlobValidation::UploadRequired(validated) => {
                adapter.upload_blob(blob, validated).await?
            }
        };
        actions.push(RemoteAction::lfs_upsert(remote_path.clone(), checkpoint));
    }
    let prepublish_safe_paths = actions
        .iter()
        .filter(|action| action.path() != CATALOG_FILE && !inventory_paths.contains(action.path()))
        .map(|action| action.path().to_string())
        .collect();
    let plan = CommitPlan::build(
        actions,
        delete_paths,
        &remote_inventory,
        &prepublish_safe_paths,
        current_catalog_sha256(&remote_inventory).map(ToOwned::to_owned),
    )?;
    for batch in &plan.prepublish {
        adapter
            .commit_actions(
                namespace,
                dataset,
                "Prepublish live encrypted objects",
                batch,
            )
            .await?;
    }
    adapter
        .commit_actions(namespace, dataset, message, &plan.publish)
        .await?;
    for batch in &plan.cleanup {
        adapter
            .commit_actions(namespace, dataset, "Clean live encrypted objects", batch)
            .await?;
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires LIOS_MODELSCOPE_LIVE=1, LIOS_MODELSCOPE_TOKEN, and LIOS_MODELSCOPE_NAMESPACE"]
async fn modelscope_private_dataset_roundtrip() {
    if std::env::var("LIOS_MODELSCOPE_LIVE").ok().as_deref() != Some("1") {
        panic!("set LIOS_MODELSCOPE_LIVE=1 to run the live ModelScope test");
    }

    let token = required_env("LIOS_MODELSCOPE_TOKEN");
    let namespace = required_env("LIOS_MODELSCOPE_NAMESPACE");
    let dataset = dataset_name();
    let endpoint = endpoint();
    let adapter = ModelScopeAdapter::new(endpoint.clone(), token.clone());
    let tmp = tempdir().unwrap();
    let source_dir = tmp.path().join("source-tree");
    let staging = tmp.path().join("staging");
    let remote_staging = tmp.path().join("remote-staging");
    let restore_dir = tmp.path().join("restore");

    write_file(&source_dir.join("top.txt"), b"top-level secret");
    write_file(
        &source_dir.join("nested/a.bin"),
        &(0..64).map(|i| i as u8).collect::<Vec<_>>(),
    );
    write_file(&source_dir.join("nested/deep/b.txt"), b"deep data");
    fs::create_dir_all(source_dir.join("empty-dir")).unwrap();

    adapter.create_repo(&namespace, &dataset).await.unwrap();
    assert!(adapter.repo_exists(&namespace, &dataset).await.unwrap());

    let result = async {
        let key = KeyFile::generate_to_path(tmp.path().join("recovery.key"))?;
        let catalog = Catalog::pack(
            PackSource::Path(source_dir.clone()),
            &key,
            PackOptions {
                chunk_size: 17,
                staging_dir: staging.clone(),
            },
        )?;
        let tree = catalog.decrypt_tree(&key)?;
        assert_eq!(tree.name, "source-tree");

        commit_staged_snapshot(
            &adapter,
            &namespace,
            &dataset,
            &staging,
            Vec::new(),
            "Publish live encrypted snapshot",
        )
        .await?;

        let remote_listing = adapter.list_objects(&namespace, &dataset, "").await?;
        assert!(remote_listing
            .iter()
            .any(|object| object.path == CATALOG_FILE));
        assert!(remote_listing
            .iter()
            .any(|object| object.path.starts_with("objects/")));

        let remote_catalog = remote_staging.join(CATALOG_FILE);
        adapter
            .download_object(&namespace, &dataset, CATALOG_FILE, &remote_catalog)
            .await?;
        let catalog = Catalog::from_staging(remote_staging.clone());
        let remote_files = catalog.remote_files_for_selection(&CatalogSelection::All, &key)?;
        for file in &remote_files {
            adapter
                .download_object(
                    &namespace,
                    &dataset,
                    &file.path,
                    &remote_staging.join(&file.path),
                )
                .await?;
        }
        catalog.restore(
            CatalogSelection::All,
            &key,
            RestoreOptions {
                output_dir: restore_dir.clone(),
                conflict_policy: RestoreConflictPolicy::Rename,
            },
        )?;

        assert_eq!(
            read_all_files(&source_dir),
            read_all_files(&restore_dir.join("source-tree"))
        );

        let cleanup_staging = tmp.path().join("cleanup-staging");
        Catalog::initialize_empty("empty", &key, cleanup_staging.clone())?;
        let stale_paths = remote_listing
            .into_iter()
            .filter(|object| {
                object.path.starts_with("objects/") || object.path.starts_with("recovery/nodes/")
            })
            .map(|object| object.path)
            .collect();
        commit_staged_snapshot(
            &adapter,
            &namespace,
            &dataset,
            &cleanup_staging,
            stale_paths,
            "Reset live encrypted snapshot",
        )
        .await?;

        Ok::<(), lios_core::LiosError>(())
    }
    .await;

    result.unwrap();
}
