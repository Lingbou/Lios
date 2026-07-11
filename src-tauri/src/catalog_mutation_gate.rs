use tokio::sync::{Mutex, MutexGuard};

/// Serializes catalog mutation transactions inside one running app process.
///
/// Phase v1 has one account and one writer, so a single global gate protects the shared staging
/// directory and remote-inventory snapshot. This does not provide multi-device safety; the next
/// ModelScope transaction phase must add remote revision conflict detection. TaskManager will move
/// transfers to task-private staging later, allowing this global shared-staging gate to narrow.
#[derive(Default)]
pub struct CatalogMutationGate {
    mutex: Mutex<()>,
}

impl CatalogMutationGate {
    pub async fn lock_mutation(&self) -> MutexGuard<'_, ()> {
        self.mutex.lock().await
    }

    pub async fn lock_shared_staging(&self) -> MutexGuard<'_, ()> {
        self.mutex.lock().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::oneshot;

    use super::CatalogMutationGate;

    #[tokio::test]
    async fn catalog_mutation_sections_do_not_overlap() {
        let gate = Arc::new(CatalogMutationGate::default());
        let first_guard = gate.lock_mutation().await;
        let second_gate = Arc::clone(&gate);
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let second = tokio::spawn(async move {
            let _guard = second_gate.lock_mutation().await;
            entered_tx.send(()).unwrap();
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            entered_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        drop(first_guard);
        entered_rx.await.unwrap();
        second.await.unwrap();
    }

    #[tokio::test]
    async fn mutation_blocks_shared_staging_read_or_download_section() {
        let gate = Arc::new(CatalogMutationGate::default());
        let mutation_guard = gate.lock_mutation().await;
        let read_gate = Arc::clone(&gate);
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let read = tokio::spawn(async move {
            let _guard = read_gate.lock_shared_staging().await;
            entered_tx.send(()).unwrap();
        });

        tokio::task::yield_now().await;
        assert!(matches!(
            entered_rx.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));

        drop(mutation_guard);
        entered_rx.await.unwrap();
        read.await.unwrap();
    }
}
