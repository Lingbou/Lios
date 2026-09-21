//! Foreground task scheduling and resumability primitives.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use lios_core::config::{LiosPaths, RepoConfig};
use lios_core::tasks::{
    PersistedTransferAction, TaskCatalogCheckpoint, TaskItem, TaskItemState, TaskRecord, TaskSpec,
    TaskStore, TransferActionKind,
};
use lios_core::{LiosError, Result as CoreResult};
use sha2::{Digest, Sha256};
use uuid::Uuid;

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

pub fn persist_submission(paths: &LiosPaths, spec: &TaskSpec) -> CoreResult<TaskRecord> {
    let task = TaskRecord::queued_for_spec(spec);
    let mut store = TaskStore::open(&paths.database)?;
    store.insert_with_spec_and_items(&task, spec, &task.items)?;
    Ok(task)
}

pub fn persist_transfer_submission(
    paths: &LiosPaths,
    spec: &TaskSpec,
    actions: &[PersistedTransferAction],
) -> CoreResult<TaskRecord> {
    let mut task = TaskRecord::queued_for_spec(spec);
    task.items = actions
        .iter()
        .map(|action| TaskItem {
            id: Uuid::new_v4(),
            task_id: task.id,
            name: action.relative_path.clone(),
            relative_path: Some(action.relative_path.clone().into()),
            source_path: action.source_path.clone(),
            source_modified_at_ns: None,
            size: action.size,
            state: if action.kind == TransferActionKind::Skip {
                TaskItemState::Skipped
            } else {
                TaskItemState::Queued
            },
            phase: None,
            bytes_done: if action.kind == TransferActionKind::Skip {
                action.size
            } else {
                0
            },
            bytes_total: action.size,
            error: None,
        })
        .collect();
    task.progress_total = u64::try_from(task.items.len()).map_err(|_| {
        LiosError::DataCorruption("transfer action count exceeds the supported range".to_string())
    })?;
    task.bytes_total = task.items.iter().try_fold(0u64, |total, item| {
        total.checked_add(item.size).ok_or_else(|| {
            LiosError::DataCorruption("transfer action byte total overflowed".to_string())
        })
    })?;
    let mut store = TaskStore::open(&paths.database)?;
    store.insert_with_spec_and_items(&task, spec, &task.items)?;
    Ok(task)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskScope {
    pub space_id: String,
}

impl TaskScope {
    pub fn from_repo(repo: &RepoConfig) -> Self {
        Self {
            space_id: hash_scope(
                "lios:modelscope:space:v1",
                [&repo.endpoint, &repo.namespace, &repo.dataset],
            ),
        }
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
    use uuid::Uuid;

    use lios_core::config::RepoConfig;

    use super::TaskScope;

    #[test]
    fn task_scope_is_stable_and_repository_scoped() {
        let repo = RepoConfig {
            namespace: "novix".to_string(),
            dataset: "cold-backup".to_string(),
            endpoint: "https://modelscope.cn".to_string(),
            title: None,
        };
        let scope = TaskScope::from_repo(&repo);
        assert_eq!(scope, TaskScope::from_repo(&repo));
        assert_eq!(scope.space_id.len(), 64);
        assert!(scope
            .space_id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));

        let other_dataset = TaskScope::from_repo(&RepoConfig {
            dataset: "other".to_string(),
            ..repo.clone()
        });
        assert_ne!(scope.space_id, other_dataset.space_id);

        let other_account = TaskScope::from_repo(&RepoConfig {
            namespace: "someone-else".to_string(),
            ..repo
        });
        assert_ne!(scope.space_id, other_account.space_id);
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
