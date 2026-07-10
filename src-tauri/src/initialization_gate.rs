use tokio::sync::{Mutex, MutexGuard};

/// Serializes catalog initialization inside one running app.
///
/// ModelScope does not expose parent-commit compare-and-swap for these writes. Phase v1 therefore
/// remains single-writer: concurrent initialization from different devices is unsupported.
#[derive(Default)]
pub struct InitializationGate {
    mutex: Mutex<()>,
}

impl InitializationGate {
    pub async fn lock(&self) -> MutexGuard<'_, ()> {
        self.mutex.lock().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tokio::sync::oneshot;

    use super::InitializationGate;

    #[tokio::test]
    async fn concurrent_initializers_are_serialized_inside_one_app() {
        let gate = Arc::new(InitializationGate::default());
        let first_guard = gate.lock().await;
        let second_gate = Arc::clone(&gate);
        let (entered_tx, mut entered_rx) = oneshot::channel();
        let second = tokio::spawn(async move {
            let _guard = second_gate.lock().await;
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
}
