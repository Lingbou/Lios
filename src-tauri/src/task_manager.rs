use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use lios_core::catalog::{snapshot_source_files, SourceFileSnapshot, SourceSnapshotReport};
use lios_core::config::{LiosPaths, RepoConfig};
use lios_core::tasks::{
    TaskCatalogCheckpoint, TaskItem, TaskItemState, TaskRecord, TaskSpec, TaskStore,
};
use lios_core::{LiosError, Result as CoreResult};
use sha2::{Digest, Sha256};
use tokio::sync::{AcquireError, Mutex, Notify, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const MAX_CONCURRENT_TRANSFERS: usize = 2;
pub const MAX_AUTOMATIC_RETRIES: u32 = 5;
const TRANSFER_SPEED_WINDOW: Duration = Duration::from_secs(5);
const TRANSFER_PUBLISH_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransferObservation {
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
    pub should_publish: bool,
}

pub struct TransferMetrics {
    started: Instant,
    samples: VecDeque<(Duration, u64)>,
    last_publish: Option<Duration>,
}

impl TransferMetrics {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            samples: VecDeque::new(),
            last_publish: None,
        }
    }

    pub fn observe(
        &mut self,
        bytes_done: u64,
        bytes_total: u64,
        force_publish: bool,
    ) -> TransferObservation {
        self.observe_at(
            self.started.elapsed(),
            bytes_done,
            bytes_total,
            force_publish,
        )
    }

    fn observe_at(
        &mut self,
        elapsed: Duration,
        bytes_done: u64,
        bytes_total: u64,
        force_publish: bool,
    ) -> TransferObservation {
        if self
            .samples
            .back()
            .is_some_and(|(_, previous_bytes)| bytes_done < *previous_bytes)
        {
            self.samples.clear();
        }
        self.samples.push_back((elapsed, bytes_done));
        let cutoff = elapsed.saturating_sub(TRANSFER_SPEED_WINDOW);
        while self.samples.len() > 1
            && self
                .samples
                .front()
                .is_some_and(|(sample_time, _)| *sample_time < cutoff)
        {
            self.samples.pop_front();
        }
        let speed_bps = self
            .samples
            .front()
            .and_then(|(sample_time, sample_bytes)| {
                let seconds = elapsed.saturating_sub(*sample_time).as_secs_f64();
                (seconds > 0.0).then(|| {
                    (bytes_done.saturating_sub(*sample_bytes) as f64 / seconds).round() as u64
                })
            })
            .unwrap_or(0);
        let eta_seconds = (speed_bps > 0 && bytes_total > bytes_done)
            .then(|| bytes_total.saturating_sub(bytes_done).div_ceil(speed_bps));
        let should_publish = force_publish
            || self.last_publish.is_none_or(|last_publish| {
                elapsed.saturating_sub(last_publish) >= TRANSFER_PUBLISH_INTERVAL
            });
        if should_publish {
            self.last_publish = Some(elapsed);
        }
        TransferObservation {
            speed_bps,
            eta_seconds,
            should_publish,
        }
    }
}

impl Default for TransferMetrics {
    fn default() -> Self {
        Self::new()
    }
}

pub fn retry_backoff(attempt: u32) -> Duration {
    let exponent = attempt.saturating_sub(1).min(31);
    Duration::from_secs((1u64 << exponent).min(30))
}

pub fn next_retry_attempt(current_attempt: u32, retryable: bool) -> Option<u32> {
    retryable
        .then(|| current_attempt.checked_add(1))
        .flatten()
        .filter(|attempt| *attempt <= MAX_AUTOMATIC_RETRIES)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CatalogReconcileDecision {
    Committed,
    Replay,
    Conflict,
}

pub fn reconcile_catalog_hash(
    checkpoint: &TaskCatalogCheckpoint,
    remote_catalog_sha256: Option<&str>,
) -> CatalogReconcileDecision {
    if remote_catalog_sha256 == Some(checkpoint.target_catalog_sha256.as_str()) {
        return CatalogReconcileDecision::Committed;
    }
    if remote_catalog_sha256 == checkpoint.base_catalog_sha256.as_deref() {
        return CatalogReconcileDecision::Replay;
    }
    CatalogReconcileDecision::Conflict
}

pub fn persist_submission(
    paths: &LiosPaths,
    spec: &TaskSpec,
    source_files: &[SourceFileSnapshot],
) -> CoreResult<TaskRecord> {
    let mut task = TaskRecord::queued_for_spec(spec);
    task.items = source_files
        .iter()
        .map(|source| TaskItem {
            id: Uuid::new_v4(),
            task_id: task.id,
            name: source
                .relative_path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| source.relative_path.to_string_lossy().into_owned()),
            relative_path: Some(source.relative_path.clone()),
            source_path: Some(source.source_path.clone()),
            source_modified_at_ns: source.modified_at_ns,
            size: source.size,
            state: TaskItemState::Queued,
            phase: None,
            bytes_done: 0,
            bytes_total: source.size,
            error: None,
        })
        .collect();
    task.progress_total = u64::try_from(task.items.len()).map_err(|_| {
        LiosError::DataCorruption("task item count exceeds the supported range".to_string())
    })?;
    task.bytes_total = task.items.iter().try_fold(0u64, |total, item| {
        total.checked_add(item.size).ok_or_else(|| {
            LiosError::DataCorruption("task source byte total overflowed".to_string())
        })
    })?;
    let mut store = TaskStore::open(&paths.database)?;
    store.insert_with_spec_and_items(&task, spec, &task.items)?;
    Ok(task)
}

pub fn ensure_source_snapshot_complete(
    report: SourceSnapshotReport,
) -> CoreResult<SourceSnapshotReport> {
    if report.skipped_paths.is_empty() {
        return Ok(report);
    }
    let skipped = report
        .skipped_paths
        .iter()
        .take(3)
        .map(|entry| entry.path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    Err(LiosError::Unsupported(format!(
        "upload source contains unsupported symbolic links or junctions: {skipped}"
    )))
}

pub fn snapshot_upload_sources(
    source_paths: &[std::path::PathBuf],
) -> CoreResult<SourceSnapshotReport> {
    ensure_source_paths_exist(source_paths)?;
    ensure_source_snapshot_complete(snapshot_source_files(source_paths)?)
}

fn ensure_source_paths_exist(source_paths: &[std::path::PathBuf]) -> CoreResult<()> {
    for path in source_paths {
        if let Err(error) = std::fs::symlink_metadata(path) {
            if error.kind() == std::io::ErrorKind::NotFound {
                return Err(LiosError::Unsupported(format!(
                    "source path no longer exists: {}",
                    path.display()
                )));
            }
            return Err(error.into());
        }
    }
    Ok(())
}

pub fn validate_task_sources(
    source_paths: &[std::path::PathBuf],
    expected: &SourceSnapshotReport,
    items: &[TaskItem],
) -> CoreResult<()> {
    if !expected.skipped_paths.is_empty() || expected.files.len() != items.len() {
        return Err(LiosError::Unsupported(
            "persisted upload source snapshot is incomplete".to_string(),
        ));
    }
    let mut persisted_items = items
        .iter()
        .filter_map(|item| item.source_path.as_ref().map(|path| (path, item)))
        .collect::<HashMap<_, _>>();
    if persisted_items.len() != items.len() {
        return Err(LiosError::DataCorruption(
            "upload task item has no source path".to_string(),
        ));
    }
    for source in &expected.files {
        let item = persisted_items.remove(&source.source_path).ok_or_else(|| {
            LiosError::Unsupported("persisted upload source snapshot is incomplete".to_string())
        })?;
        if item.relative_path.as_ref() != Some(&source.relative_path)
            || item.source_modified_at_ns != source.modified_at_ns
            || item.size != source.size
        {
            return Err(LiosError::Unsupported(format!(
                "persisted source file snapshot is incomplete: {}",
                item.name
            )));
        }
    }
    if !persisted_items.is_empty() {
        return Err(LiosError::Unsupported(
            "persisted upload source snapshot is incomplete".to_string(),
        ));
    }
    ensure_source_paths_exist(source_paths)?;
    let current = ensure_source_snapshot_complete(snapshot_source_files(source_paths)?)?;
    if current.directories != expected.directories || current.files.len() != expected.files.len() {
        return Err(LiosError::Unsupported(
            "source directory changed after the task was queued".to_string(),
        ));
    }
    let mut current_files = current
        .files
        .into_iter()
        .map(|source| (source.source_path.clone(), source))
        .collect::<HashMap<_, _>>();
    for source in &expected.files {
        let current = current_files.remove(&source.source_path).ok_or_else(|| {
            LiosError::Unsupported("source directory changed after the task was queued".to_string())
        })?;
        if current != *source {
            return Err(LiosError::Unsupported(format!(
                "source file changed before upload: {}",
                source.relative_path.display()
            )));
        }
    }
    if !current_files.is_empty() {
        return Err(LiosError::Unsupported(
            "source directory changed after the task was queued".to_string(),
        ));
    }
    Ok(())
}

pub fn apply_pack_progress(
    items: &mut [TaskItem],
    completed_chunks: u64,
    completed_bytes: u64,
    chunk_size: usize,
) -> CoreResult<Vec<TaskItem>> {
    let chunk_size = u64::try_from(chunk_size)
        .ok()
        .filter(|size| *size > 0)
        .ok_or_else(|| {
            LiosError::Unsupported("chunk size must be greater than zero".to_string())
        })?;
    let mut remaining_chunks = completed_chunks;
    let mut remaining_bytes = completed_bytes;
    let mut changed = Vec::new();
    for item in items {
        let previous = item.clone();
        let total_chunks = if item.size == 0 {
            1
        } else {
            item.size.div_ceil(chunk_size)
        };
        let item_completed_chunks = remaining_chunks.min(total_chunks);
        let item_completed_bytes = remaining_bytes.min(item.size);
        if item_completed_chunks == 0 {
            item.state = TaskItemState::Queued;
            item.phase = None;
            item.bytes_done = 0;
        } else if item_completed_chunks == total_chunks {
            item.state = TaskItemState::Running;
            item.phase = Some("prepared".to_string());
            item.bytes_done = item.size;
        } else {
            item.state = TaskItemState::Running;
            item.phase = Some("preparing".to_string());
            item.bytes_done = item_completed_bytes;
        }
        item.error = None;
        remaining_chunks = remaining_chunks.saturating_sub(item_completed_chunks);
        remaining_bytes = remaining_bytes.saturating_sub(item_completed_bytes);
        if *item != previous {
            changed.push(item.clone());
        }
    }
    Ok(changed)
}

pub struct TaskExecutionGate {
    transfers: Arc<Semaphore>,
    spaces: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
}

pub struct TaskExecutionPermit {
    _space: OwnedMutexGuard<()>,
    transfer: Option<OwnedSemaphorePermit>,
}

impl TaskExecutionPermit {
    pub fn release_transfer(&mut self) {
        self.transfer.take();
    }
}

pub struct SpaceMutationPermit {
    _space: OwnedMutexGuard<()>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskScope {
    pub account_id: String,
    pub space_id: String,
}

impl TaskScope {
    pub fn from_repo(repo: &RepoConfig) -> Self {
        Self {
            account_id: hash_scope(
                "lios:modelscope:account:v1",
                [&repo.endpoint, &repo.namespace],
            ),
            space_id: hash_scope(
                "lios:modelscope:space:v1",
                [&repo.endpoint, &repo.namespace, &repo.dataset],
            ),
        }
    }
}

#[derive(Clone)]
pub struct TaskManager {
    gate: Arc<TaskExecutionGate>,
    controls: Arc<TaskControlRegistry>,
}

impl Default for TaskManager {
    fn default() -> Self {
        Self::new()
    }
}

impl TaskManager {
    pub fn new() -> Self {
        Self {
            gate: Arc::new(TaskExecutionGate::new()),
            controls: Arc::new(TaskControlRegistry::default()),
        }
    }

    pub async fn acquire(
        &self,
        space_id: impl Into<String>,
    ) -> Result<TaskExecutionPermit, AcquireError> {
        self.gate.acquire(space_id).await
    }

    pub async fn acquire_space(&self, space_id: impl Into<String>) -> SpaceMutationPermit {
        self.gate.acquire_space(space_id).await
    }

    pub async fn promote_space(
        &self,
        permit: SpaceMutationPermit,
    ) -> Result<TaskExecutionPermit, AcquireError> {
        self.gate.promote_space(permit).await
    }

    pub async fn restore_transfer(
        &self,
        permit: &mut TaskExecutionPermit,
    ) -> Result<(), AcquireError> {
        self.gate.restore_transfer(permit).await
    }

    pub async fn register(&self, task_id: Uuid) -> TaskControl {
        self.controls.register(task_id).await
    }

    pub async fn cancel(&self, task_id: Uuid) -> bool {
        self.controls.cancel(task_id).await
    }

    pub async fn remove(&self, control: &TaskControl) {
        self.controls.remove(control).await;
    }

    pub async fn is_running(&self, task_id: Uuid) -> bool {
        self.controls.is_registered(task_id).await
    }

    pub async fn wait_until_stopped(&self, task_id: Uuid) {
        self.controls.wait_until_stopped(task_id).await;
    }
}

#[derive(Default)]
pub struct TaskControlRegistry {
    tokens: Mutex<HashMap<Uuid, TaskControlEntry>>,
    stopped: Notify,
}

struct TaskControlEntry {
    generation: Uuid,
    token: CancellationToken,
}

pub struct TaskControl {
    task_id: Uuid,
    generation: Uuid,
    token: CancellationToken,
}

impl TaskControl {
    pub fn token(&self) -> &CancellationToken {
        &self.token
    }
}

impl TaskControlRegistry {
    pub async fn register(&self, task_id: Uuid) -> TaskControl {
        let generation = Uuid::new_v4();
        let token = CancellationToken::new();
        if let Some(previous) = self.tokens.lock().await.insert(
            task_id,
            TaskControlEntry {
                generation,
                token: token.clone(),
            },
        ) {
            previous.token.cancel();
        }
        TaskControl {
            task_id,
            generation,
            token,
        }
    }

    pub async fn cancel(&self, task_id: Uuid) -> bool {
        let token = self
            .tokens
            .lock()
            .await
            .get(&task_id)
            .map(|entry| entry.token.clone());
        if let Some(token) = token {
            token.cancel();
            true
        } else {
            false
        }
    }

    pub async fn remove(&self, control: &TaskControl) {
        let mut tokens = self.tokens.lock().await;
        if tokens
            .get(&control.task_id)
            .is_some_and(|entry| entry.generation == control.generation)
        {
            tokens.remove(&control.task_id);
            self.stopped.notify_waiters();
        }
    }

    pub async fn is_registered(&self, task_id: Uuid) -> bool {
        self.tokens.lock().await.contains_key(&task_id)
    }

    pub async fn wait_until_stopped(&self, task_id: Uuid) {
        loop {
            let stopped = self.stopped.notified();
            if !self.is_registered(task_id).await {
                return;
            }
            stopped.await;
        }
    }
}

impl TaskExecutionGate {
    pub fn new() -> Self {
        Self {
            transfers: Arc::new(Semaphore::new(MAX_CONCURRENT_TRANSFERS)),
            spaces: StdMutex::new(HashMap::new()),
        }
    }

    pub async fn acquire(
        &self,
        space_id: impl Into<String>,
    ) -> Result<TaskExecutionPermit, AcquireError> {
        let space = self.space_lock(space_id.into());
        let space = space.lock_owned().await;
        let transfer = Arc::clone(&self.transfers).acquire_owned().await?;
        Ok(TaskExecutionPermit {
            _space: space,
            transfer: Some(transfer),
        })
    }

    pub async fn acquire_space(&self, space_id: impl Into<String>) -> SpaceMutationPermit {
        SpaceMutationPermit {
            _space: self.space_lock(space_id.into()).lock_owned().await,
        }
    }

    pub async fn promote_space(
        &self,
        permit: SpaceMutationPermit,
    ) -> Result<TaskExecutionPermit, AcquireError> {
        let transfer = Arc::clone(&self.transfers).acquire_owned().await?;
        Ok(TaskExecutionPermit {
            _space: permit._space,
            transfer: Some(transfer),
        })
    }

    pub async fn restore_transfer(
        &self,
        permit: &mut TaskExecutionPermit,
    ) -> Result<(), AcquireError> {
        if permit.transfer.is_none() {
            permit.transfer = Some(Arc::clone(&self.transfers).acquire_owned().await?);
        }
        Ok(())
    }

    fn space_lock(&self, space_id: String) -> Arc<Mutex<()>> {
        let mut spaces = self
            .spaces
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        spaces.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = spaces.get(&space_id).and_then(Weak::upgrade) {
            return lock;
        }
        let lock = Arc::new(Mutex::new(()));
        spaces.insert(space_id, Arc::downgrade(&lock));
        lock
    }
}

impl Default for TaskExecutionGate {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_scope<'a>(domain: &str, parts: impl IntoIterator<Item = &'a String>) -> String {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    for part in parts {
        hasher.update((part.len() as u64).to_be_bytes());
        hasher.update(part.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::oneshot;

    use uuid::Uuid;

    use lios_core::catalog::{
        snapshot_source_files, SkippedPath, SkippedPathReason, SourceFileSnapshot,
        SourceSnapshotReport,
    };
    use lios_core::config::{LiosPaths, RepoConfig};
    use lios_core::tasks::{TaskItem, TaskItemState, TaskSpec, TaskState, TaskStore};
    use tempfile::tempdir;

    use super::{
        apply_pack_progress, ensure_source_snapshot_complete, persist_submission,
        validate_task_sources, TaskControlRegistry, TaskExecutionGate, TaskScope,
    };

    #[tokio::test]
    async fn different_spaces_can_use_both_global_transfer_slots() {
        let gate = Arc::new(TaskExecutionGate::new());
        let first = gate.acquire("space-a").await.unwrap();
        let second = gate.acquire("space-b").await.unwrap();
        let third_gate = Arc::clone(&gate);
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let third = tokio::spawn(async move {
            let _permit = third_gate.acquire("space-c").await.unwrap();
            entered_tx.send(()).unwrap();
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            entered_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        drop(first);
        entered_rx.await.unwrap();
        drop(second);
        third.await.unwrap();
    }

    #[tokio::test]
    async fn tasks_in_the_same_space_are_serialized_without_using_an_extra_slot() {
        let gate = Arc::new(TaskExecutionGate::new());
        let first = gate.acquire("space-a").await.unwrap();
        let same_space_gate = Arc::clone(&gate);
        let (same_tx, mut same_rx) = oneshot::channel();
        let same_space = tokio::spawn(async move {
            let _permit = same_space_gate.acquire("space-a").await.unwrap();
            same_tx.send(()).unwrap();
        });

        let other = gate.acquire("space-b").await.unwrap();
        tokio::task::yield_now().await;
        assert!(matches!(
            same_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        drop(first);
        same_rx.await.unwrap();
        drop(other);
        same_space.await.unwrap();
    }

    #[tokio::test]
    async fn a_space_only_writer_blocks_transfers_in_the_same_space() {
        let gate = Arc::new(TaskExecutionGate::new());
        let writer = gate.acquire_space("space-a").await;
        let transfer_gate = Arc::clone(&gate);
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let transfer = tokio::spawn(async move {
            let _permit = transfer_gate.acquire("space-a").await.unwrap();
            entered_tx.send(()).unwrap();
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            entered_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        drop(writer);
        entered_rx.await.unwrap();
        transfer.await.unwrap();
    }

    #[tokio::test]
    async fn a_transfer_can_release_its_global_slot_while_retaining_the_space_lock() {
        let gate = Arc::new(TaskExecutionGate::new());
        let mut first = gate.acquire("space-a").await.unwrap();
        let second = gate.acquire("space-b").await.unwrap();
        let third_gate = Arc::clone(&gate);
        let (third_tx, mut third_rx) = oneshot::channel();
        let third = tokio::spawn(async move {
            let _permit = third_gate.acquire("space-c").await.unwrap();
            third_tx.send(()).unwrap();
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            third_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        first.release_transfer();
        third_rx.await.unwrap();

        let same_gate = Arc::clone(&gate);
        let (same_tx, mut same_rx) = oneshot::channel();
        let same = tokio::spawn(async move {
            let _permit = same_gate.acquire("space-a").await.unwrap();
            same_tx.send(()).unwrap();
        });
        tokio::task::yield_now().await;
        assert!(matches!(
            same_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        drop(first);
        same_rx.await.unwrap();
        drop(second);
        third.await.unwrap();
        same.await.unwrap();
    }

    #[tokio::test]
    async fn task_control_cancels_the_registered_worker_and_is_removed_on_finish() {
        let controls = TaskControlRegistry::default();
        let task_id = Uuid::new_v4();
        let control = controls.register(task_id).await;

        assert!(controls.cancel(task_id).await);
        control.token().cancelled().await;
        controls.remove(&control).await;
        assert!(!controls.cancel(task_id).await);
    }

    #[tokio::test]
    async fn an_old_worker_cannot_remove_a_replacement_control_registration() {
        let controls = TaskControlRegistry::default();
        let task_id = Uuid::new_v4();
        let first = controls.register(task_id).await;
        let replacement = controls.register(task_id).await;
        assert!(first.token().is_cancelled());

        controls.remove(&first).await;

        assert!(controls.cancel(task_id).await);
        replacement.token().cancelled().await;
    }

    #[tokio::test]
    async fn wait_until_stopped_unblocks_only_after_the_current_control_is_removed() {
        let controls = Arc::new(TaskControlRegistry::default());
        let task_id = Uuid::new_v4();
        let control = controls.register(task_id).await;
        let waiting_controls = Arc::clone(&controls);
        let (done_tx, mut done_rx) = oneshot::channel();
        let waiter = tokio::spawn(async move {
            waiting_controls.wait_until_stopped(task_id).await;
            done_tx.send(()).unwrap();
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            done_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        controls.remove(&control).await;
        done_rx.await.unwrap();
        waiter.await.unwrap();
    }

    #[test]
    fn task_scope_is_stable_domain_separated_and_path_safe() {
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold-backup".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let scope = TaskScope::from_repo(&repo);
        assert_eq!(scope, TaskScope::from_repo(&repo));
        assert_eq!(scope.account_id.len(), 64);
        assert_eq!(scope.space_id.len(), 64);
        assert!(scope
            .account_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
        assert!(scope
            .space_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));

        let other_dataset = TaskScope::from_repo(&RepoConfig {
            dataset: "other".to_string(),
            ..repo.clone()
        });
        assert_eq!(scope.account_id, other_dataset.account_id);
        assert_ne!(scope.space_id, other_dataset.space_id);

        let other_account = TaskScope::from_repo(&RepoConfig {
            namespace: "someone-else".to_string(),
            ..repo
        });
        assert_ne!(scope.account_id, other_account.account_id);
        assert_ne!(scope.space_id, other_account.space_id);
    }

    #[test]
    fn submission_persists_a_queued_spec_and_source_file_items() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let scope = TaskScope::from_repo(&repo);
        let source_path = temp.path().join("album.bin");
        let spec = TaskSpec::Upload {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            parent_node_id: "root".to_string(),
            source_paths: vec![source_path.clone()],
            source_snapshot: None,
            chunk_size: 128 * 1024 * 1024,
            conflict_resolutions: Vec::new(),
        };

        let snapshots = vec![SourceFileSnapshot {
            source_path: source_path.clone(),
            relative_path: "photos/album.bin".into(),
            size: 4096,
            modified_at_ns: Some(123456789),
        }];
        let task = persist_submission(&paths, &spec, &snapshots).unwrap();

        assert_eq!(task.state, TaskState::Queued);
        assert_eq!(task.progress_total, 1);
        assert_eq!(task.bytes_total, 4096);
        assert_eq!(task.items.len(), 1);
        assert_eq!(task.items[0].name, "album.bin");
        assert_eq!(
            task.items[0].relative_path.as_deref(),
            Some(std::path::Path::new("photos/album.bin"))
        );
        assert_eq!(task.items[0].source_path.as_ref(), Some(&source_path));
        assert_eq!(task.items[0].source_modified_at_ns, Some(123456789));
        let store = TaskStore::open(&paths.database).unwrap();
        assert_eq!(
            store.get(task.id).unwrap().unwrap().state,
            TaskState::Queued
        );
        assert_eq!(
            store.load_spec(task.id).unwrap().unwrap().space_id(),
            spec.space_id()
        );
        assert_eq!(store.list_items(task.id).unwrap(), task.items);
    }

    #[test]
    fn persisted_source_snapshot_rejects_a_changed_file_before_execution() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let source_path = temp.path().join("album.bin");
        std::fs::write(&source_path, [1u8; 4]).unwrap();
        let snapshot = snapshot_source_files(std::slice::from_ref(&source_path)).unwrap();
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::Upload {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            parent_node_id: "root".to_string(),
            source_paths: vec![source_path.clone()],
            source_snapshot: Some(snapshot.clone()),
            chunk_size: 128 * 1024 * 1024,
            conflict_resolutions: Vec::new(),
        };
        let task = persist_submission(&paths, &spec, &snapshot.files).unwrap();
        std::fs::write(&source_path, [2u8; 8]).unwrap();

        let error =
            validate_task_sources(std::slice::from_ref(&source_path), &snapshot, &task.items)
                .unwrap_err();
        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("source file changed")
                    && message.contains("album.bin")
        ));
    }

    #[test]
    fn persisted_directory_snapshot_rejects_a_file_added_after_enqueue() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let source_dir = temp.path().join("album");
        std::fs::create_dir(&source_dir).unwrap();
        std::fs::write(source_dir.join("first.bin"), [1u8; 4]).unwrap();
        let snapshot = snapshot_source_files(std::slice::from_ref(&source_dir)).unwrap();
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::Upload {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            parent_node_id: "root".to_string(),
            source_paths: vec![source_dir.clone()],
            source_snapshot: Some(snapshot.clone()),
            chunk_size: 128 * 1024 * 1024,
            conflict_resolutions: Vec::new(),
        };
        let task = persist_submission(&paths, &spec, &snapshot.files).unwrap();
        std::fs::write(source_dir.join("second.bin"), [2u8; 4]).unwrap();

        let error =
            validate_task_sources(std::slice::from_ref(&source_dir), &snapshot, &task.items)
                .unwrap_err();
        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("source directory changed")
        ));
    }

    #[test]
    fn persisted_directory_snapshot_rejects_an_added_empty_directory() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("album");
        std::fs::create_dir(&source_dir).unwrap();
        let snapshot = snapshot_source_files(std::slice::from_ref(&source_dir)).unwrap();
        std::fs::create_dir(source_dir.join("new-empty-folder")).unwrap();

        let error =
            validate_task_sources(std::slice::from_ref(&source_dir), &snapshot, &[]).unwrap_err();

        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("source directory changed")
        ));
    }

    #[test]
    fn missing_top_level_source_reports_the_original_path() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("album.bin");
        std::fs::write(&source_path, [1u8; 4]).unwrap();
        let snapshot = snapshot_source_files(std::slice::from_ref(&source_path)).unwrap();
        let items = snapshot
            .files
            .iter()
            .map(|source| TaskItem {
                id: Uuid::new_v4(),
                task_id: Uuid::new_v4(),
                name: "album.bin".to_string(),
                relative_path: Some(source.relative_path.clone()),
                source_path: Some(source.source_path.clone()),
                source_modified_at_ns: source.modified_at_ns,
                size: source.size,
                state: TaskItemState::Queued,
                phase: None,
                bytes_done: 0,
                bytes_total: source.size,
                error: None,
            })
            .collect::<Vec<_>>();
        std::fs::remove_file(&source_path).unwrap();

        let error = validate_task_sources(std::slice::from_ref(&source_path), &snapshot, &items)
            .unwrap_err();

        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("source path no longer exists")
                    && message.contains("album.bin")
        ));
    }

    #[test]
    fn upload_snapshot_reports_a_missing_top_level_source() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("missing.bin");

        let error = super::snapshot_upload_sources(std::slice::from_ref(&source_path)).unwrap_err();

        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("source path no longer exists")
                    && message.contains("missing.bin")
        ));
    }

    #[test]
    fn persisted_source_snapshot_requires_a_modification_time() {
        let temp = tempdir().unwrap();
        let source_path = temp.path().join("album.bin");
        std::fs::write(&source_path, [1u8; 4]).unwrap();
        let snapshot = snapshot_source_files(std::slice::from_ref(&source_path)).unwrap();
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::Upload {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            parent_node_id: "root".to_string(),
            source_paths: vec![source_path.clone()],
            source_snapshot: Some(snapshot.clone()),
            chunk_size: 128 * 1024 * 1024,
            conflict_resolutions: Vec::new(),
        };
        let paths = LiosPaths::from_home(temp.path());
        let mut task = persist_submission(&paths, &spec, &snapshot.files).unwrap();
        task.items[0].source_modified_at_ns = None;

        let error =
            validate_task_sources(std::slice::from_ref(&source_path), &snapshot, &task.items)
                .unwrap_err();

        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("snapshot is incomplete")
        ));
    }

    #[test]
    fn unchanged_empty_directory_is_a_valid_zero_item_snapshot() {
        let temp = tempdir().unwrap();
        let source_dir = temp.path().join("empty");
        std::fs::create_dir(&source_dir).unwrap();
        let snapshot = snapshot_source_files(std::slice::from_ref(&source_dir)).unwrap();

        validate_task_sources(std::slice::from_ref(&source_dir), &snapshot, &[]).unwrap();
    }

    #[test]
    fn pack_progress_is_distributed_to_the_matching_source_files() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let first_path = temp.path().join("first.bin");
        let second_path = temp.path().join("second.bin");
        let snapshots = vec![
            SourceFileSnapshot {
                source_path: first_path.clone(),
                relative_path: "first.bin".into(),
                size: 4,
                modified_at_ns: Some(1),
            },
            SourceFileSnapshot {
                source_path: second_path.clone(),
                relative_path: "second.bin".into(),
                size: 8,
                modified_at_ns: Some(2),
            },
        ];
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
        };
        let scope = TaskScope::from_repo(&repo);
        let spec = TaskSpec::Upload {
            account_id: scope.account_id,
            space_id: scope.space_id,
            repo,
            parent_node_id: "root".to_string(),
            source_paths: vec![first_path, second_path],
            source_snapshot: None,
            chunk_size: 4,
            conflict_resolutions: Vec::new(),
        };
        let mut task = persist_submission(&paths, &spec, &snapshots).unwrap();

        let changed = apply_pack_progress(&mut task.items, 2, 8, 4).unwrap();

        assert_eq!(changed.len(), 2);
        assert_eq!(task.items[0].state, TaskItemState::Running);
        assert_eq!(task.items[0].phase.as_deref(), Some("prepared"));
        assert_eq!(task.items[0].bytes_done, 4);
        assert_eq!(task.items[1].state, TaskItemState::Running);
        assert_eq!(task.items[1].phase.as_deref(), Some("preparing"));
        assert_eq!(task.items[1].bytes_done, 4);
    }

    #[test]
    fn upload_submission_rejects_a_snapshot_with_skipped_links() {
        let linked = std::path::PathBuf::from("C:/source/linked");
        let report = SourceSnapshotReport {
            files: Vec::new(),
            directories: Vec::new(),
            skipped_paths: vec![SkippedPath {
                path: linked.clone(),
                reason: SkippedPathReason::SymbolicLinkOrJunction,
            }],
        };

        let error = ensure_source_snapshot_complete(report).unwrap_err();

        assert!(matches!(
            error,
            lios_core::LiosError::Unsupported(message)
                if message.contains("symbolic links or junctions")
                    && message.contains("linked")
        ));
    }

    #[test]
    fn transfer_metrics_use_a_five_second_window_and_throttle_publishing() {
        let mut metrics = super::TransferMetrics::new();
        let total = 10 * 1024 * 1024;

        let initial = metrics.observe_at(std::time::Duration::ZERO, 0, total, false);
        let early = metrics.observe_at(std::time::Duration::from_millis(100), 128, total, false);
        let middle = metrics.observe_at(
            std::time::Duration::from_secs(2),
            2 * 1024 * 1024,
            total,
            false,
        );
        let latest = metrics.observe_at(
            std::time::Duration::from_secs(6),
            6 * 1024 * 1024,
            total,
            false,
        );

        assert!(initial.should_publish);
        assert!(!early.should_publish);
        assert!(middle.should_publish);
        assert_eq!(latest.speed_bps, 1024 * 1024);
        assert_eq!(latest.eta_seconds, Some(4));
    }

    #[test]
    fn transfer_metrics_reset_when_a_new_phase_restarts_byte_progress() {
        let mut metrics = super::TransferMetrics::new();
        metrics.observe_at(std::time::Duration::from_secs(1), 1024, 2048, false);
        let restarted = metrics.observe_at(std::time::Duration::from_secs(2), 0, 4096, true);

        assert_eq!(restarted.speed_bps, 0);
        assert_eq!(restarted.eta_seconds, None);
        assert!(restarted.should_publish);
    }

    #[test]
    fn retry_backoff_uses_bounded_exponential_delays() {
        assert_eq!(super::retry_backoff(1), std::time::Duration::from_secs(1));
        assert_eq!(super::retry_backoff(2), std::time::Duration::from_secs(2));
        assert_eq!(super::retry_backoff(3), std::time::Duration::from_secs(4));
        assert_eq!(super::retry_backoff(4), std::time::Duration::from_secs(8));
        assert_eq!(super::retry_backoff(5), std::time::Duration::from_secs(16));
        assert_eq!(super::retry_backoff(7), std::time::Duration::from_secs(30));
    }

    #[test]
    fn automatic_retry_stops_after_five_retry_attempts() {
        assert_eq!(super::next_retry_attempt(0, true), Some(1));
        assert_eq!(super::next_retry_attempt(4, true), Some(5));
        assert_eq!(super::next_retry_attempt(5, true), None);
        assert_eq!(super::next_retry_attempt(0, false), None);
    }

    #[test]
    fn catalog_reconciliation_distinguishes_committed_replay_and_conflict() {
        use lios_core::tasks::TaskCatalogCheckpoint;

        let checkpoint = TaskCatalogCheckpoint {
            task_id: Uuid::new_v4(),
            base_catalog_sha256: Some("a".repeat(64)),
            target_catalog_sha256: "b".repeat(64),
        };

        assert_eq!(
            super::reconcile_catalog_hash(&checkpoint, Some(&"b".repeat(64))),
            super::CatalogReconcileDecision::Committed
        );
        assert_eq!(
            super::reconcile_catalog_hash(&checkpoint, Some(&"a".repeat(64))),
            super::CatalogReconcileDecision::Replay
        );
        assert_eq!(
            super::reconcile_catalog_hash(&checkpoint, Some(&"c".repeat(64))),
            super::CatalogReconcileDecision::Conflict
        );

        let initial = TaskCatalogCheckpoint {
            task_id: Uuid::new_v4(),
            base_catalog_sha256: None,
            target_catalog_sha256: "d".repeat(64),
        };
        assert_eq!(
            super::reconcile_catalog_hash(&initial, None),
            super::CatalogReconcileDecision::Replay
        );
        assert_eq!(
            super::reconcile_catalog_hash(&initial, Some(&"d".repeat(64))),
            super::CatalogReconcileDecision::Committed
        );
        assert_eq!(
            super::reconcile_catalog_hash(&initial, Some(&"e".repeat(64))),
            super::CatalogReconcileDecision::Conflict
        );
    }
}
