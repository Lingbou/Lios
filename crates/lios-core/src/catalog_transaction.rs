use std::collections::HashSet;
use std::path::PathBuf;

use uuid::Uuid;

use crate::storage::{
    validate_catalog_sync_upload, BlobCheckpoint, BlobSpec, BlobValidation, CatalogSyncUpload,
    CommitPlan, RemoteAction, RepoRevision, StorageAdapter, StorageObject,
};
use crate::{LiosError, RemoteError, RemoteErrorKind, Result};

#[derive(Debug, Clone)]
pub struct CatalogTransactionSpec {
    pub uploads: Vec<CatalogSyncUpload>,
    pub delete_paths: Vec<String>,
    pub initial_remote_inventory: Vec<StorageObject>,
    pub prepublish_safe_paths: HashSet<String>,
    pub base_catalog_sha256: Option<String>,
    pub expected_revision: Option<RepoRevision>,
    pub probe_directory: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogTransactionPhase {
    ValidateBlobs,
    UploadBlobs,
    Prepublish,
    ProbeCatalog,
    Publish,
    Cleanup,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogTransactionProgress {
    pub phase: CatalogTransactionPhase,
    pub completed_items: u64,
    pub total_items: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub blob_checkpoint: Option<CatalogBlobCheckpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogBlobCheckpointState {
    Uploaded,
    Committed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogBlobCheckpoint {
    pub path: String,
    pub oid: String,
    pub size: u64,
    pub state: CatalogBlobCheckpointState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogTransactionOutcome {
    Completed { warnings: Vec<String> },
    Canceled,
}

pub async fn execute_catalog_transaction<A, C, P>(
    adapter: &A,
    namespace: &str,
    dataset: &str,
    spec: CatalogTransactionSpec,
    mut should_cancel: C,
    mut on_progress: P,
) -> Result<CatalogTransactionOutcome>
where
    A: StorageAdapter + ?Sized,
    C: FnMut() -> Result<bool>,
    P: FnMut(CatalogTransactionProgress) -> Result<()>,
{
    validate_initial_catalog(&spec)?;
    if should_cancel()? {
        return Ok(CatalogTransactionOutcome::Canceled);
    }

    let has_changes = !spec.uploads.is_empty() || !spec.delete_paths.is_empty();
    let planned_catalog_sha256 = spec
        .uploads
        .iter()
        .find(|upload| upload.path == "catalog.enc")
        .map(|upload| upload.expected_sha256.clone());
    let total_items =
        (spec.uploads.len() + spec.delete_paths.len()) as u64 + u64::from(has_changes);
    let mut blobs = Vec::with_capacity(spec.uploads.len());
    let mut actions = Vec::with_capacity(spec.uploads.len());
    for upload in &spec.uploads {
        validate_catalog_sync_upload(upload)?;
        let blob = BlobSpec::from_path(upload.local_path.clone()).await?;
        if blob.oid != upload.expected_sha256
            || upload.expected_size.is_some_and(|size| size != blob.size)
        {
            return Err(LiosError::DataCorruption(format!(
                "planned catalog upload changed before blob validation: {}",
                upload.path
            )));
        }
        actions.push(RemoteAction::lfs_upsert(
            upload.path.clone(),
            BlobCheckpoint::new(blob.oid.clone(), blob.size),
        ));
        blobs.push(blob);
    }

    let plan = CommitPlan::build(
        actions,
        spec.delete_paths.clone(),
        &spec.initial_remote_inventory,
        &spec.prepublish_safe_paths,
        spec.base_catalog_sha256.clone(),
    )?;
    if spec.expected_revision.is_some() && !plan.prepublish.is_empty() {
        return Err(LiosError::Unsupported(
            "revision-guarded catalog publication cannot use prepublish commits".to_string(),
        ));
    }

    let mut progress = CatalogTransactionProgress {
        phase: CatalogTransactionPhase::ValidateBlobs,
        completed_items: 0,
        total_items,
        bytes_done: 0,
        bytes_total: 0,
        blob_checkpoint: None,
    };
    on_progress(progress.clone())?;

    let validations = adapter.validate_blobs(namespace, dataset, &blobs).await?;
    if validations.len() != blobs.len() {
        return Err(RemoteError::new(RemoteErrorKind::InvalidResponse, None).into());
    }
    progress.bytes_total = blobs
        .iter()
        .zip(&validations)
        .filter(|(_blob, validation)| matches!(validation, BlobValidation::UploadRequired(_)))
        .map(|(blob, _validation)| blob.size)
        .fold(0u64, u64::saturating_add);

    progress.phase = CatalogTransactionPhase::UploadBlobs;
    on_progress(progress.clone())?;
    for ((upload, blob), validation) in spec.uploads.iter().zip(&blobs).zip(validations) {
        if should_cancel()? {
            return Ok(CatalogTransactionOutcome::Canceled);
        }
        let checkpoint = match validation {
            BlobValidation::Reusable(checkpoint) => checkpoint,
            BlobValidation::UploadRequired(validated) => {
                let completed_before_blob = progress.bytes_done;
                let (progress_tx, mut progress_rx) = tokio::sync::mpsc::unbounded_channel();
                let upload = adapter.upload_blob_with_progress(blob, validated, Some(progress_tx));
                tokio::pin!(upload);
                let mut progress_open = true;
                let checkpoint = loop {
                    tokio::select! {
                        result = &mut upload => {
                            let checkpoint = result?;
                            while let Ok(streamed) = progress_rx.try_recv() {
                                if streamed > blob.size {
                                    return Err(LiosError::DataCorruption(
                                        "blob upload reported more bytes than expected".to_string(),
                                    ));
                                }
                                let bytes_done = completed_before_blob.saturating_add(streamed);
                                if bytes_done > progress.bytes_done {
                                    progress.bytes_done = bytes_done;
                                    on_progress(progress.clone())?;
                                }
                                if should_cancel()? {
                                    return Ok(CatalogTransactionOutcome::Canceled);
                                }
                            }
                            break checkpoint;
                        },
                        streamed = progress_rx.recv(), if progress_open => {
                            let Some(streamed) = streamed else {
                                progress_open = false;
                                continue;
                            };
                            if streamed > blob.size {
                                return Err(LiosError::DataCorruption(
                                    "blob upload reported more bytes than expected".to_string(),
                                ));
                            }
                            let bytes_done = completed_before_blob.saturating_add(streamed);
                            if bytes_done > progress.bytes_done {
                                progress.bytes_done = bytes_done;
                                on_progress(progress.clone())?;
                            }
                            if should_cancel()? {
                                return Ok(CatalogTransactionOutcome::Canceled);
                            }
                        }
                    }
                };
                progress.bytes_done = completed_before_blob.saturating_add(blob.size);
                checkpoint
            }
        };
        if checkpoint.oid != blob.oid || checkpoint.size != blob.size {
            return Err(LiosError::DataCorruption(
                "storage adapter returned a mismatched blob checkpoint".to_string(),
            ));
        }
        progress.completed_items += 1;
        progress.blob_checkpoint = Some(CatalogBlobCheckpoint {
            path: upload.path.clone(),
            oid: checkpoint.oid,
            size: checkpoint.size,
            state: CatalogBlobCheckpointState::Uploaded,
        });
        on_progress(progress.clone())?;
        progress.blob_checkpoint = None;
    }

    for batch in &plan.prepublish {
        if should_cancel()? {
            return Ok(CatalogTransactionOutcome::Canceled);
        }
        progress.phase = CatalogTransactionPhase::Prepublish;
        on_progress(progress.clone())?;
        adapter
            .commit_actions(
                namespace,
                dataset,
                "Prepublish encrypted catalog objects",
                batch,
            )
            .await?;
    }

    if plan.publish.is_empty() {
        return Ok(CatalogTransactionOutcome::Completed {
            warnings: Vec::new(),
        });
    }
    if should_cancel()? {
        return Ok(CatalogTransactionOutcome::Canceled);
    }

    progress.phase = CatalogTransactionPhase::ProbeCatalog;
    on_progress(progress.clone())?;
    let remote_catalog_sha256 = {
        let probe = ProbeGuard::new(&spec.probe_directory)?;
        probe_catalog_sha256(adapter, namespace, dataset, probe.path()).await?
    };
    if remote_catalog_sha256 != spec.base_catalog_sha256 {
        return Err(remote_conflict());
    }
    if should_cancel()? {
        return Ok(CatalogTransactionOutcome::Canceled);
    }
    if let Some(expected_revision) = &spec.expected_revision {
        let current_revision = adapter.head_revision(namespace, dataset).await?;
        if &current_revision != expected_revision {
            return Err(remote_conflict());
        }
        if should_cancel()? {
            return Ok(CatalogTransactionOutcome::Canceled);
        }
    }

    progress.phase = CatalogTransactionPhase::Publish;
    on_progress(progress.clone())?;
    let publish = adapter
        .commit_actions(
            namespace,
            dataset,
            "Publish encrypted catalog transaction",
            &plan.publish,
        )
        .await;
    let mut warnings = Vec::new();
    if let Err(error) = publish {
        let confirmed = if let Some(expected) = planned_catalog_sha256.as_deref() {
            let probe = ProbeGuard::new(&spec.probe_directory)?;
            matches!(
                probe_catalog_sha256(adapter, namespace, dataset, probe.path()).await,
                Ok(Some(actual)) if actual == expected
            )
        } else {
            false
        };
        if !confirmed {
            return Err(error);
        }
        warnings.push(format!(
            "publish response failed, but the remote catalog confirms publication: {error}"
        ));
    }
    progress.completed_items += 1 + plan
        .publish
        .iter()
        .filter(|action| action.is_delete())
        .count() as u64;
    if let Err(error) = on_progress(progress.clone()) {
        warnings.push(format!("publish progress could not be recorded: {error}"));
    }
    for (upload, blob) in spec.uploads.iter().zip(&blobs) {
        progress.blob_checkpoint = Some(CatalogBlobCheckpoint {
            path: upload.path.clone(),
            oid: blob.oid.clone(),
            size: blob.size,
            state: CatalogBlobCheckpointState::Committed,
        });
        if let Err(error) = on_progress(progress.clone()) {
            warnings.push(format!(
                "committed blob checkpoint could not be recorded: {error}"
            ));
        }
    }
    progress.blob_checkpoint = None;

    progress.phase = CatalogTransactionPhase::Cleanup;
    for batch in &plan.cleanup {
        let cleanup = adapter
            .commit_actions(
                namespace,
                dataset,
                "Delete unreferenced encrypted catalog objects",
                batch,
            )
            .await;
        match cleanup {
            Ok(()) => {
                progress.completed_items +=
                    batch.iter().filter(|action| action.is_delete()).count() as u64;
                if let Err(error) = on_progress(progress.clone()) {
                    warnings.push(format!("cleanup progress could not be recorded: {error}"));
                }
            }
            Err(error) => {
                warnings.push(format!(
                    "cleanup batch failed after catalog publication: {error}"
                ));
            }
        }
    }

    Ok(CatalogTransactionOutcome::Completed { warnings })
}

fn validate_initial_catalog(spec: &CatalogTransactionSpec) -> Result<()> {
    let remote_catalog = spec
        .initial_remote_inventory
        .iter()
        .find(|object| object.path == "catalog.enc");
    match (&spec.base_catalog_sha256, remote_catalog) {
        (None, None) => Ok(()),
        (None, Some(_)) | (Some(_), None) => Err(remote_conflict()),
        (Some(base), Some(remote)) => {
            if remote
                .sha256
                .as_deref()
                .is_some_and(|sha256| sha256 != base)
            {
                Err(remote_conflict())
            } else {
                Ok(())
            }
        }
    }
}

pub async fn probe_catalog_sha256<A: StorageAdapter + ?Sized>(
    adapter: &A,
    namespace: &str,
    dataset: &str,
    probe_path: &std::path::Path,
) -> Result<Option<String>> {
    match adapter
        .download_object(namespace, dataset, "catalog.enc", probe_path)
        .await
    {
        Ok(()) => BlobSpec::from_path(probe_path.to_path_buf())
            .await
            .map(|blob| Some(blob.oid)),
        Err(LiosError::Remote(error)) if error.kind == RemoteErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

struct ProbeGuard {
    path: PathBuf,
}

impl ProbeGuard {
    fn new(directory: &std::path::Path) -> Result<Self> {
        std::fs::create_dir_all(directory)?;
        for _ in 0..16 {
            let path = directory.join(format!(
                ".lios-catalog-probe-{}.enc",
                Uuid::new_v4().simple()
            ));
            if !path.exists() && !path.with_extension("download").exists() {
                return Ok(Self { path });
            }
        }
        Err(LiosError::Storage(
            "could not allocate a unique catalog probe path".to_string(),
        ))
    }

    fn path(&self) -> &std::path::Path {
        &self.path
    }
}

impl Drop for ProbeGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let _ = std::fs::remove_file(self.path.with_extension("download"));
    }
}

fn remote_conflict() -> LiosError {
    RemoteError::new(RemoteErrorKind::Conflict, None).into()
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::fs;
    use std::path::Path;
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use sha2::{Digest, Sha256};
    use tempfile::tempdir;

    use super::*;
    use crate::storage::{
        BlobCheckpoint, BlobSpec, BlobValidation, RemoteAction, RepoRevision, ValidatedBlobUpload,
    };
    use crate::{RemoteError, RemoteErrorKind};

    const NAMESPACE: &str = "novix";
    const DATASET: &str = "cold";

    #[derive(Debug, Clone)]
    enum ProbeResult {
        Bytes(Vec<u8>),
        NotFound,
        Error(RemoteErrorKind),
        Sequence(VecDeque<ProbeResult>),
    }

    #[derive(Debug)]
    struct ScriptState {
        events: Vec<String>,
        probe: ProbeResult,
        head_revision: RepoRevision,
        upload_required: bool,
        cleanup_failures_remaining: usize,
        publish_failures_remaining: usize,
    }

    #[derive(Clone)]
    struct ScriptedAdapter {
        state: Arc<Mutex<ScriptState>>,
    }

    impl ScriptedAdapter {
        fn new(probe: ProbeResult, upload_required: bool) -> Self {
            Self {
                state: Arc::new(Mutex::new(ScriptState {
                    events: Vec::new(),
                    probe,
                    head_revision: RepoRevision {
                        branch: "master".to_string(),
                        commit_id: Some("head".to_string()),
                    },
                    upload_required,
                    cleanup_failures_remaining: 0,
                    publish_failures_remaining: 0,
                })),
            }
        }

        fn events(&self) -> Vec<String> {
            self.state.lock().unwrap().events.clone()
        }

        fn record(&self, event: impl Into<String>) {
            self.state.lock().unwrap().events.push(event.into());
        }

        fn fail_cleanup_times(&self, failures: usize) {
            self.state.lock().unwrap().cleanup_failures_remaining = failures;
        }

        fn fail_publish_times(&self, failures: usize) {
            self.state.lock().unwrap().publish_failures_remaining = failures;
        }

        fn set_head_revision(&self, commit_id: &str) {
            self.state.lock().unwrap().head_revision = RepoRevision {
                branch: "master".to_string(),
                commit_id: Some(commit_id.to_string()),
            };
        }
    }

    #[async_trait]
    impl StorageAdapter for ScriptedAdapter {
        async fn create_repo(&self, _namespace: &str, _dataset: &str) -> Result<()> {
            Ok(())
        }

        async fn repo_exists(&self, _namespace: &str, _dataset: &str) -> Result<bool> {
            Ok(true)
        }

        async fn head_revision(&self, _namespace: &str, _dataset: &str) -> Result<RepoRevision> {
            self.record("revision");
            Ok(self.state.lock().unwrap().head_revision.clone())
        }

        async fn list_objects(
            &self,
            _namespace: &str,
            _dataset: &str,
            _prefix: &str,
        ) -> Result<Vec<StorageObject>> {
            Ok(Vec::new())
        }

        async fn validate_blobs(
            &self,
            _namespace: &str,
            _dataset: &str,
            blobs: &[BlobSpec],
        ) -> Result<Vec<BlobValidation>> {
            self.record("validate");
            let upload_required = self.state.lock().unwrap().upload_required;
            Ok(blobs
                .iter()
                .map(|blob| {
                    let checkpoint = BlobCheckpoint::new(blob.oid.clone(), blob.size);
                    if upload_required {
                        BlobValidation::UploadRequired(ValidatedBlobUpload::new(
                            checkpoint,
                            "https://uploads.modelscope.cn/blob".to_string(),
                        ))
                    } else {
                        BlobValidation::Reusable(checkpoint)
                    }
                })
                .collect())
        }

        async fn upload_blob(
            &self,
            blob: &BlobSpec,
            validated: ValidatedBlobUpload,
        ) -> Result<BlobCheckpoint> {
            let (checkpoint, _url) = validated.into_parts();
            assert_eq!(checkpoint.oid, blob.oid);
            assert_eq!(checkpoint.size, blob.size);
            self.record(format!("upload:{}", blob.oid));
            Ok(checkpoint)
        }

        async fn upload_blob_with_progress(
            &self,
            blob: &BlobSpec,
            validated: ValidatedBlobUpload,
            progress: Option<tokio::sync::mpsc::UnboundedSender<u64>>,
        ) -> Result<BlobCheckpoint> {
            if let Some(progress) = progress {
                let _ = progress.send(blob.size / 2);
                tokio::task::yield_now().await;
                let _ = progress.send(blob.size);
            }
            self.upload_blob(blob, validated).await
        }

        async fn commit_actions(
            &self,
            _namespace: &str,
            _dataset: &str,
            _commit_message: &str,
            actions: &[RemoteAction],
        ) -> Result<()> {
            let event = if actions.iter().any(|action| action.path() == "catalog.enc") {
                "publish"
            } else if actions.iter().all(RemoteAction::is_delete) {
                "cleanup"
            } else {
                "prepublish"
            };
            self.record(event);
            if event == "publish" {
                let mut state = self.state.lock().unwrap();
                if state.publish_failures_remaining > 0 {
                    state.publish_failures_remaining -= 1;
                    return Err(RemoteError::new(RemoteErrorKind::Network, None).into());
                }
            }
            if event == "cleanup" {
                let mut state = self.state.lock().unwrap();
                if state.cleanup_failures_remaining > 0 {
                    state.cleanup_failures_remaining -= 1;
                    return Err(RemoteError::new(RemoteErrorKind::Server, Some(500)).into());
                }
            }
            Ok(())
        }

        async fn download_object(
            &self,
            _namespace: &str,
            _dataset: &str,
            remote_path: &str,
            local_path: &Path,
        ) -> Result<()> {
            assert_eq!(remote_path, "catalog.enc");
            self.record("probe");
            let probe = {
                let mut state = self.state.lock().unwrap();
                match &mut state.probe {
                    ProbeResult::Sequence(results) => results.pop_front().unwrap(),
                    probe => probe.clone(),
                }
            };
            match probe {
                ProbeResult::Bytes(bytes) => {
                    if let Some(parent) = local_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(local_path, bytes)?;
                    Ok(())
                }
                ProbeResult::NotFound => {
                    Err(RemoteError::new(RemoteErrorKind::NotFound, Some(404)).into())
                }
                ProbeResult::Error(kind) => Err(RemoteError::new(kind, None).into()),
                ProbeResult::Sequence(_) => unreachable!("nested probe sequence"),
            }
        }
    }

    fn sha256(bytes: &[u8]) -> String {
        hex::encode(Sha256::digest(bytes))
    }

    fn upload(path: &str, local_path: PathBuf) -> CatalogSyncUpload {
        let bytes = fs::read(&local_path).unwrap();
        CatalogSyncUpload {
            path: path.to_string(),
            local_path,
            expected_sha256: sha256(&bytes),
            expected_size: Some(bytes.len() as u64),
        }
    }

    fn remote_catalog(base: &[u8]) -> StorageObject {
        StorageObject {
            path: "catalog.enc".to_string(),
            size: base.len() as u64,
            sha256: Some(sha256(base)),
        }
    }

    fn spec(
        root: &Path,
        base: Option<&[u8]>,
        include_object: bool,
        delete_count: usize,
    ) -> CatalogTransactionSpec {
        let catalog_path = root.join("catalog-next.enc");
        fs::write(&catalog_path, b"new encrypted catalog").unwrap();
        let mut uploads = vec![upload("catalog.enc", catalog_path)];
        let mut safe = HashSet::new();
        if include_object {
            let object_path = root.join("object.lios");
            fs::write(&object_path, b"encrypted object").unwrap();
            let remote_path = "objects/files/aa/chunks/bb.lios";
            uploads.push(upload(remote_path, object_path));
            safe.insert(remote_path.to_string());
        }
        CatalogTransactionSpec {
            uploads,
            delete_paths: (0..delete_count)
                .map(|index| format!("objects/stale/{index:03}.lios"))
                .collect(),
            initial_remote_inventory: base.map(remote_catalog).into_iter().collect(),
            prepublish_safe_paths: safe,
            base_catalog_sha256: base.map(sha256),
            expected_revision: None,
            probe_directory: root.to_path_buf(),
        }
    }

    fn assert_completed(outcome: CatalogTransactionOutcome) -> Vec<String> {
        let CatalogTransactionOutcome::Completed { warnings } = outcome else {
            panic!("expected completed transaction");
        };
        warnings
    }

    async fn run(
        adapter: &ScriptedAdapter,
        spec: CatalogTransactionSpec,
        should_cancel: impl FnMut() -> Result<bool>,
    ) -> Result<CatalogTransactionOutcome> {
        execute_catalog_transaction(
            adapter,
            NAMESPACE,
            DATASET,
            spec,
            should_cancel,
            |_progress| Ok(()),
        )
        .await
    }

    #[tokio::test]
    async fn required_blob_uploads_finish_before_the_first_commit() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), true);

        let outcome = run(&adapter, spec(tmp.path(), Some(base), true, 0), || {
            Ok(false)
        })
        .await
        .unwrap();

        assert!(assert_completed(outcome).is_empty());
        let events = adapter.events();
        let first_commit = events
            .iter()
            .position(|event| matches!(event.as_str(), "prepublish" | "publish"))
            .unwrap();
        let last_upload = events
            .iter()
            .rposition(|event| event.starts_with("upload:"))
            .unwrap();
        assert!(last_upload < first_commit, "{events:?}");
    }

    #[tokio::test]
    async fn required_blob_uploads_report_intermediate_streamed_bytes() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), true);
        let mut observed = Vec::new();

        execute_catalog_transaction(
            &adapter,
            NAMESPACE,
            DATASET,
            spec(tmp.path(), Some(base), true, 0),
            || Ok(false),
            |progress| {
                observed.push(progress);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert!(observed.iter().any(|progress| {
            progress.phase == CatalogTransactionPhase::UploadBlobs
                && progress.bytes_done > 0
                && progress.bytes_done < progress.bytes_total
                && progress.completed_items == 0
        }));
    }

    #[tokio::test]
    async fn blob_checkpoints_are_reported_as_uploaded_then_committed() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), true);
        let transaction = spec(tmp.path(), Some(base), true, 0);
        let expected_paths = transaction
            .uploads
            .iter()
            .map(|upload| upload.path.clone())
            .collect::<HashSet<_>>();
        let mut observed = Vec::new();

        execute_catalog_transaction(
            &adapter,
            NAMESPACE,
            DATASET,
            transaction,
            || Ok(false),
            |progress| {
                if let Some(checkpoint) = progress.blob_checkpoint {
                    observed.push(checkpoint);
                }
                Ok(())
            },
        )
        .await
        .unwrap();

        for path in expected_paths {
            let states = observed
                .iter()
                .filter(|checkpoint| checkpoint.path == path)
                .map(|checkpoint| checkpoint.state)
                .collect::<Vec<_>>();
            assert_eq!(
                states,
                vec![
                    CatalogBlobCheckpointState::Uploaded,
                    CatalogBlobCheckpointState::Committed,
                ],
                "{path}: {observed:?}"
            );
        }
    }

    #[tokio::test]
    async fn stable_large_transaction_orders_prepublish_probe_publish_cleanup() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);

        let outcome = run(&adapter, spec(tmp.path(), Some(base), true, 255), || {
            Ok(false)
        })
        .await
        .unwrap();

        assert!(assert_completed(outcome).is_empty());
        let events = adapter.events();
        let ordered = ["prepublish", "probe", "publish", "cleanup"]
            .map(|name| events.iter().position(|event| event == name).unwrap());
        assert!(
            ordered.windows(2).all(|pair| pair[0] < pair[1]),
            "{events:?}"
        );
    }

    #[tokio::test]
    async fn publish_phase_is_persisted_before_the_remote_publish_request() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        let progress_adapter = adapter.clone();

        execute_catalog_transaction(
            &adapter,
            NAMESPACE,
            DATASET,
            spec(tmp.path(), Some(base), true, 0),
            || Ok(false),
            move |progress| {
                if progress.phase == CatalogTransactionPhase::Publish
                    && progress.blob_checkpoint.is_none()
                {
                    progress_adapter.record("progress:publish");
                }
                Ok(())
            },
        )
        .await
        .unwrap();

        let events = adapter.events();
        let persisted = events
            .iter()
            .position(|event| event == "progress:publish")
            .unwrap();
        let published = events.iter().position(|event| event == "publish").unwrap();
        assert!(persisted < published, "{events:?}");
    }

    #[tokio::test]
    async fn changed_catalog_after_prepublish_conflicts_without_publish_or_cleanup() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(b"changed remotely".to_vec()), false);

        let error = run(&adapter, spec(tmp.path(), Some(base), true, 255), || {
            Ok(false)
        })
        .await
        .unwrap_err();

        assert!(matches!(
            error,
            LiosError::Remote(ref error) if error.kind == RemoteErrorKind::Conflict
        ));
        let events = adapter.events();
        assert!(events.iter().any(|event| event == "prepublish"));
        assert!(events.iter().any(|event| event == "probe"));
        assert!(!events.iter().any(|event| event == "publish"));
        assert!(!events.iter().any(|event| event == "cleanup"));
        assert!(!fs::read_dir(tmp.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".lios-catalog-probe-")
        }));
    }

    #[tokio::test]
    async fn changed_revision_after_blob_upload_conflicts_before_publish() {
        let tmp = tempdir().unwrap();
        let adapter = ScriptedAdapter::new(ProbeResult::NotFound, true);
        adapter.set_head_revision("changed-after-preview");
        let mut transaction = spec(tmp.path(), None, false, 0);
        transaction.expected_revision = Some(RepoRevision {
            branch: "master".to_string(),
            commit_id: Some("preview-revision".to_string()),
        });

        let error = run(&adapter, transaction, || Ok(false)).await.unwrap_err();

        assert!(matches!(
            error,
            LiosError::Remote(ref error) if error.kind == RemoteErrorKind::Conflict
        ));
        let events = adapter.events();
        assert!(events.iter().any(|event| event.starts_with("upload:")));
        assert!(events.iter().any(|event| event == "probe"));
        assert!(events.iter().any(|event| event == "revision"));
        assert!(!events.iter().any(|event| event == "publish"));
    }

    #[tokio::test]
    async fn cancellation_during_revision_guard_stops_before_publish() {
        let tmp = tempdir().unwrap();
        let adapter = ScriptedAdapter::new(ProbeResult::NotFound, false);
        let mut transaction = spec(tmp.path(), None, false, 0);
        transaction.expected_revision = Some(RepoRevision {
            branch: "master".to_string(),
            commit_id: Some("head".to_string()),
        });
        let observed = adapter.clone();

        let outcome = run(&adapter, transaction, move || {
            Ok(observed.events().iter().any(|event| event == "revision"))
        })
        .await
        .unwrap();

        assert_eq!(outcome, CatalogTransactionOutcome::Canceled);
        let events = adapter.events();
        assert!(events.iter().any(|event| event == "revision"));
        assert!(!events.iter().any(|event| event == "publish"));
    }

    #[tokio::test]
    async fn initial_catalog_mismatch_aborts_before_blob_work() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), true);
        let mut transaction = spec(tmp.path(), Some(base), false, 0);
        transaction.initial_remote_inventory[0].sha256 = Some("0".repeat(64));

        let error = run(&adapter, transaction, || Ok(false)).await.unwrap_err();

        assert!(matches!(
            error,
            LiosError::Remote(ref error) if error.kind == RemoteErrorKind::Conflict
        ));
        assert!(adapter.events().is_empty());
    }

    #[tokio::test]
    async fn probe_maps_only_not_found_to_absence_and_propagates_network_errors() {
        let tmp = tempdir().unwrap();
        let not_found = ScriptedAdapter::new(ProbeResult::NotFound, false);
        let outcome = run(&not_found, spec(tmp.path(), None, false, 0), || Ok(false))
            .await
            .unwrap();
        assert!(assert_completed(outcome).is_empty());
        assert!(not_found.events().iter().any(|event| event == "publish"));

        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let network = ScriptedAdapter::new(ProbeResult::Error(RemoteErrorKind::Network), false);
        let error = run(&network, spec(tmp.path(), Some(base), false, 0), || {
            Ok(false)
        })
        .await
        .unwrap_err();
        assert!(matches!(
            error,
            LiosError::Remote(ref error) if error.kind == RemoteErrorKind::Network
        ));
        assert!(!network.events().iter().any(|event| event == "publish"));
    }

    #[tokio::test]
    async fn cancellation_stops_before_publish_but_is_not_polled_after_publish() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let canceled = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        let outcome = run(&canceled, spec(tmp.path(), Some(base), false, 0), || {
            Ok(true)
        })
        .await
        .unwrap();
        assert_eq!(outcome, CatalogTransactionOutcome::Canceled);
        assert!(canceled.events().is_empty());

        let tmp = tempdir().unwrap();
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        let observed = adapter.clone();
        let outcome = run(
            &adapter,
            spec(tmp.path(), Some(base), true, 255),
            move || Ok(observed.events().iter().any(|event| event == "publish")),
        )
        .await
        .unwrap();
        assert!(assert_completed(outcome).is_empty());
        assert!(adapter.events().iter().any(|event| event == "cleanup"));
    }

    #[tokio::test]
    async fn progress_failure_after_publish_does_not_skip_cleanup() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        let observed = adapter.clone();

        let result = execute_catalog_transaction(
            &adapter,
            NAMESPACE,
            DATASET,
            spec(tmp.path(), Some(base), true, 255),
            || Ok(false),
            move |progress| {
                if progress.phase == CatalogTransactionPhase::Publish
                    && observed.events().iter().any(|event| event == "publish")
                {
                    Err(LiosError::Storage("progress store failed".to_string()))
                } else {
                    Ok(())
                }
            },
        )
        .await;

        let warnings = assert_completed(result.unwrap());
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("progress could not be recorded")));
        assert!(adapter.events().iter().any(|event| event == "publish"));
        assert!(adapter.events().iter().any(|event| event == "cleanup"));
    }

    #[tokio::test]
    async fn cleanup_failure_after_publish_does_not_skip_later_batches() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        adapter.fail_cleanup_times(1);

        let result = run(&adapter, spec(tmp.path(), Some(base), true, 512), || {
            Ok(false)
        })
        .await;

        let warnings = assert_completed(result.unwrap());
        assert!(warnings
            .iter()
            .any(|warning| warning.contains("cleanup batch failed")));
        assert_eq!(
            adapter
                .events()
                .iter()
                .filter(|event| event.as_str() == "cleanup")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn progress_does_not_reach_total_before_catalog_publish() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        let observed = Arc::new(Mutex::new(Vec::new()));
        let progress = Arc::clone(&observed);

        execute_catalog_transaction(
            &adapter,
            NAMESPACE,
            DATASET,
            spec(tmp.path(), Some(base), true, 0),
            || Ok(false),
            move |update| {
                progress.lock().unwrap().push(update);
                Ok(())
            },
        )
        .await
        .unwrap();

        let observed = observed.lock().unwrap();
        assert!(observed
            .iter()
            .filter(|progress| progress.phase != CatalogTransactionPhase::Publish)
            .all(|progress| progress.completed_items < progress.total_items));
        let final_progress = observed.last().unwrap();
        assert_eq!(final_progress.phase, CatalogTransactionPhase::Publish);
        assert_eq!(final_progress.completed_items, final_progress.total_items);
    }

    #[tokio::test]
    async fn internal_probe_allocation_does_not_overwrite_an_existing_file() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let sentinel = tmp.path().join("do-not-touch.txt");
        fs::write(&sentinel, b"sentinel").unwrap();
        let adapter = ScriptedAdapter::new(ProbeResult::Bytes(base.to_vec()), false);
        let transaction = spec(tmp.path(), Some(base), false, 0);

        run(&adapter, transaction, || Ok(false)).await.unwrap();

        assert_eq!(fs::read(&sentinel).unwrap(), b"sentinel");
    }

    #[tokio::test]
    async fn accepted_publish_with_a_lost_response_is_confirmed_and_cleaned_up() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(
            ProbeResult::Sequence(VecDeque::from([
                ProbeResult::Bytes(base.to_vec()),
                ProbeResult::Bytes(b"new encrypted catalog".to_vec()),
            ])),
            false,
        );
        adapter.fail_publish_times(1);

        let outcome = run(&adapter, spec(tmp.path(), Some(base), true, 255), || {
            Ok(false)
        })
        .await
        .unwrap();
        let warnings = assert_completed(outcome);

        assert!(warnings
            .iter()
            .any(|warning| warning.contains("publish response")));
        assert!(adapter.events().iter().any(|event| event == "publish"));
        assert!(adapter.events().iter().any(|event| event == "cleanup"));
    }

    #[tokio::test]
    async fn uncertain_publish_failure_returns_only_after_publish_phase_is_reported() {
        let tmp = tempdir().unwrap();
        let base = b"old encrypted catalog";
        let adapter = ScriptedAdapter::new(
            ProbeResult::Sequence(VecDeque::from([
                ProbeResult::Bytes(base.to_vec()),
                ProbeResult::Error(RemoteErrorKind::Network),
            ])),
            false,
        );
        adapter.fail_publish_times(1);
        let mut saw_publish_phase = false;

        let error = execute_catalog_transaction(
            &adapter,
            NAMESPACE,
            DATASET,
            spec(tmp.path(), Some(base), true, 0),
            || Ok(false),
            |progress| {
                saw_publish_phase |= progress.phase == CatalogTransactionPhase::Publish;
                Ok(())
            },
        )
        .await
        .unwrap_err();

        assert!(saw_publish_phase);
        assert!(matches!(
            error,
            LiosError::Remote(ref error) if error.kind == RemoteErrorKind::Network
        ));
        assert!(adapter.events().iter().any(|event| event == "publish"));
        assert!(!adapter.events().iter().any(|event| event == "cleanup"));
    }
}
