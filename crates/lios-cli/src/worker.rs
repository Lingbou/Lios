use std::fs;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{Duration, Instant};

use clap::Parser;
use lios_application::service::Application;
use lios_core::config::LiosPaths;
use lios_core::tasks::{TaskState, TaskStore};
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "lios-worker", version, about = "Background Lios task worker")]
struct WorkerArgs {
    #[arg(long, value_name = "DIR")]
    home: Option<PathBuf>,
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(WorkerArgs::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lios-worker: {}", error.message);
            ExitCode::from(7)
        }
    }
}

async fn run(args: WorkerArgs) -> lios_application::CommandResult<()> {
    let paths = args
        .home
        .map(LiosPaths::from_home)
        .unwrap_or_else(LiosPaths::default_user);
    paths.ensure_dirs()?;
    paths.ensure_worker_control_dir()?;
    let _worker_lock = match paths.try_lock_worker() {
        Ok(lock) => lock,
        Err(lios_core::config::ConfigLockError::Busy) => return Ok(()),
        Err(error) => return Err(error.into()),
    };
    let _ = fs::remove_file(paths.worker_stop_path());
    fs::write(paths.worker_pid_path(), std::process::id().to_string())?;
    let application = Application::new(paths.clone())?;
    TaskStore::open(&paths.database)?.recover_after_restart("worker restarted")?;
    let mut idle_since = Instant::now();

    loop {
        if paths.worker_stop_path().exists() {
            let _ = fs::remove_file(paths.worker_stop_path());
            break;
        }
        let next = application
            .list_tasks()?
            .into_iter()
            .rev()
            .find(|task| task.state == TaskState::Queued);
        if let Some(task) = next {
            idle_since = Instant::now();
            if run_one_task(&application, &paths, task.id).await? {
                break;
            }
            continue;
        }
        if idle_since.elapsed() >= Duration::from_secs(5 * 60) {
            break;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let _ = fs::remove_file(paths.worker_stop_path());
    let _ = fs::remove_file(paths.worker_pid_path());
    Ok(())
}

async fn run_one_task(
    application: &Application,
    paths: &LiosPaths,
    task_id: Uuid,
) -> lios_application::CommandResult<bool> {
    let runner = application.clone();
    let mut execution = Box::pin(runner.run_task(task_id, |_| {}));
    loop {
        tokio::select! {
            result = &mut execution => {
                let _ = result;
                let _ = fs::remove_file(paths.worker_pause_path(task_id));
                let _ = fs::remove_file(paths.worker_cancel_path(task_id));
                return Ok(paths.worker_stop_path().exists());
            }
            _ = tokio::time::sleep(Duration::from_millis(100)) => {
                let stop_requested = paths.worker_stop_path().exists();
                let pause_path = paths.worker_pause_path(task_id);
                let cancel_path = paths.worker_cancel_path(task_id);
                if !stop_requested && !pause_path.exists() && !cancel_path.exists() {
                    continue;
                }
                let summary = application.get_task(task_id)?.ok_or_else(|| {
                    lios_application::CommandError::invalid_input("running task disappeared")
                })?;
                if summary.state == TaskState::Committing {
                    continue;
                }
                if matches!(
                    summary.state,
                    TaskState::Completed | TaskState::Failed | TaskState::Canceled | TaskState::Paused
                ) {
                    let _ = fs::remove_file(&pause_path);
                    let _ = fs::remove_file(&cancel_path);
                    return Ok(stop_requested);
                }
                if cancel_path.exists() {
                    application.cancel_task(task_id).await?;
                } else {
                    application.pause_task(task_id).await?;
                }
                let _ = fs::remove_file(&pause_path);
                let _ = fs::remove_file(&cancel_path);
                return Ok(stop_requested);
            }
        }
    }
}
