use std::sync::{Mutex, MutexGuard};

use crate::command_error::{CommandError, CommandErrorCode};

#[derive(Default)]
pub struct ConfigMutationGate {
    inner: Mutex<()>,
}

impl ConfigMutationGate {
    pub fn lock(&self) -> Result<MutexGuard<'_, ()>, CommandError> {
        self.inner.lock().map_err(|_| {
            CommandError::new(
                CommandErrorCode::Internal,
                "config mutation gate is unavailable",
                false,
                None,
            )
        })
    }
}
