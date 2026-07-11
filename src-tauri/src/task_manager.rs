use std::collections::HashMap;
use std::sync::{Arc, Mutex as StdMutex, Weak};

use lios_core::config::RepoConfig;
use sha2::{Digest, Sha256};
use tokio::sync::{AcquireError, Mutex, OwnedMutexGuard, OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

pub const MAX_CONCURRENT_TRANSFERS: usize = 2;

pub struct TaskExecutionGate {
    transfers: Arc<Semaphore>,
    spaces: StdMutex<HashMap<String, Weak<Mutex<()>>>>,
}

pub struct TaskExecutionPermit {
    _space: OwnedMutexGuard<()>,
    _transfer: OwnedSemaphorePermit,
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

    pub async fn register(&self, task_id: Uuid) -> TaskControl {
        self.controls.register(task_id).await
    }

    pub async fn cancel(&self, task_id: Uuid) -> bool {
        self.controls.cancel(task_id).await
    }

    pub async fn remove(&self, control: &TaskControl) {
        self.controls.remove(control).await;
    }
}

#[derive(Default)]
pub struct TaskControlRegistry {
    tokens: Mutex<HashMap<Uuid, TaskControlEntry>>,
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
            _transfer: transfer,
        })
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

    use lios_core::config::RepoConfig;

    use super::{TaskControlRegistry, TaskExecutionGate, TaskScope};

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
}
