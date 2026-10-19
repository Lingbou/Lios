use crate::error::{CliError, CliResult};
use lios_application::service::Application;
use lios_core::config::LiosPaths;
use lios_core::tasks::{TaskState, TaskSummary};
use std::fs;
use std::io::{self, IsTerminal, Write};

pub fn start_worker(paths: &LiosPaths) -> CliResult<()> {
    lios_application::worker_supervisor::spawn_shared_worker(paths, false).map_err(CliError::from)
}

pub async fn wait_for_worker_task(
    application: &Application,
    task_id: uuid::Uuid,
) -> CliResult<TaskSummary> {
    wait_for_worker_task_with_progress(application, task_id, false).await
}

pub async fn wait_for_worker_task_with_progress(
    application: &Application,
    task_id: uuid::Uuid,
    force_progress: bool,
) -> CliResult<TaskSummary> {
    start_worker(application.paths())?;
    let dynamic_progress = io::stderr().is_terminal();
    let mut last_progress = None;
    loop {
        let task = application
            .get_task(task_id)?
            .ok_or_else(|| CliError::invalid_input("task was not found"))?;
        match task.state {
            TaskState::Completed => {
                if dynamic_progress && last_progress.is_some() {
                    eprintln!();
                }
                return Ok(task);
            }
            TaskState::Failed => {
                if dynamic_progress && last_progress.is_some() {
                    eprintln!();
                }
                return Err(CliError::task_failure(
                    task.error.unwrap_or_else(|| "task failed".to_string()),
                ));
            }
            TaskState::Canceled => {
                if dynamic_progress && last_progress.is_some() {
                    eprintln!();
                }
                return Err(CliError::task_failure("task was canceled"));
            }
            TaskState::Paused => {
                if dynamic_progress && last_progress.is_some() {
                    eprintln!();
                }
                return Ok(task);
            }
            _ => {}
        }
        let progress = (
            task.state.clone(),
            task.progress_done,
            task.progress_total,
            task.bytes_done,
        );
        if (dynamic_progress || force_progress) && last_progress.as_ref() != Some(&progress) {
            if dynamic_progress {
                eprint!(
                    "\r{:?} {}/{} ({} bytes)",
                    task.state, task.progress_done, task.progress_total, task.bytes_done
                );
                io::stderr().flush()?;
            } else {
                eprintln!(
                    "{:?} {}/{} ({} bytes)",
                    task.state, task.progress_done, task.progress_total, task.bytes_done
                );
            }
            last_progress = Some(progress);
        }

        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(CliError::from)?;
                application.paths().ensure_worker_control_dir()?;
                fs::write(application.paths().worker_pause_path(task_id), b"pause\n")?;
                return wait_for_pause_after_interrupt(application, task_id).await;
            }
        }
    }
}

pub async fn wait_for_pause_after_interrupt(
    application: &Application,
    task_id: uuid::Uuid,
) -> CliResult<TaskSummary> {
    loop {
        let task = application
            .get_task(task_id)?
            .ok_or_else(|| CliError::invalid_input("task was not found"))?;
        if matches!(
            task.state,
            TaskState::Paused | TaskState::Completed | TaskState::Failed | TaskState::Canceled
        ) {
            return Err(CliError::interrupted(format!(
                "task {task_id} is now {:?}",
                task.state
            )));
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
            signal = tokio::signal::ctrl_c() => {
                signal.map_err(CliError::from)?;
                return Err(CliError::interrupted(format!(
                    "client stopped while task {task_id} remains in {:?}",
                    task.state
                )));
            }
        }
    }
}

pub async fn request_worker_interruption(
    application: &Application,
    task_id: uuid::Uuid,
    cancel: bool,
) -> CliResult<TaskSummary> {
    let current = application
        .get_task(task_id)?
        .ok_or_else(|| CliError::invalid_input("task was not found"))?;
    if matches!(current.state, TaskState::Queued | TaskState::Paused) {
        return if cancel {
            Ok(application.cancel_task(task_id).await?)
        } else {
            Ok(application.pause_task(task_id).await?)
        };
    }
    if matches!(
        current.state,
        TaskState::Completed | TaskState::Failed | TaskState::Canceled
    ) {
        return Err(CliError::invalid_input("task is already terminal"));
    }
    // A worker may have idle-exited or crashed while the task was still
    // marked active. Start one so the request is actually consumed instead
    // of polling a control file forever.
    start_worker(application.paths())?;
    application.paths().ensure_worker_control_dir()?;
    let request = if cancel {
        application.paths().worker_cancel_path(task_id)
    } else {
        application.paths().worker_pause_path(task_id)
    };
    let request_body: &[u8] = if cancel { b"cancel\n" } else { b"pause\n" };
    fs::write(request, request_body)?;
    loop {
        let task = application
            .get_task(task_id)?
            .ok_or_else(|| CliError::invalid_input("task was not found"))?;
        if cancel && task.state == TaskState::Canceled
            || !cancel && task.state == TaskState::Paused
            || matches!(task.state, TaskState::Completed | TaskState::Failed)
        {
            return Ok(task);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}
