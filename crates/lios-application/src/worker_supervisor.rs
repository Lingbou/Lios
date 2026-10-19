use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command as ProcessCommand, Stdio};

use lios_core::config::LiosPaths;

use crate::command_error::{CommandError, CommandErrorCode};
use crate::{to_err, CommandResult};

pub fn locate_worker_binary() -> CommandResult<PathBuf> {
    let current = std::env::current_exe().map_err(to_err)?;
    let extension = current.extension().map(|value| value.to_os_string());
    let mut worker = current.with_file_name("lios-worker");
    if let Some(extension) = extension {
        worker.set_extension(extension);
    }
    if !worker.is_file() {
        return Err(CommandError::new(
            CommandErrorCode::Storage,
            format!("lios-worker was not found beside {}", current.display()),
            false,
            None,
        ));
    }
    Ok(worker)
}

pub fn spawn_shared_worker(paths: &LiosPaths, capture_logs: bool) -> CommandResult<()> {
    if paths.worker_running().map_err(CommandError::from)? {
        return Ok(());
    }
    let _ = fs::remove_file(paths.worker_stop_path());
    paths.ensure_worker_control_dir()?;
    let worker = locate_worker_binary()?;
    let home_root = paths.home.parent().ok_or_else(|| {
        CommandError::new(
            CommandErrorCode::Storage,
            "Lios Home has no parent directory for worker startup",
            false,
            None,
        )
    })?;

    let mut command = ProcessCommand::new(worker);
    command.arg("--home").arg(home_root).stdin(Stdio::null());

    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }

    if capture_logs {
        fs::create_dir_all(&paths.logs).map_err(to_err)?;
        let log_file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(paths.logs.join("lios-worker.log"))
            .map_err(to_err)?;
        let stderr = log_file.try_clone().map_err(to_err)?;
        command.stdout(Stdio::from(log_file));
        command.stderr(Stdio::from(stderr));

        let mut child = command.spawn().map_err(to_err)?;
        let log_path = paths.logs.join("lios-worker.log");
        std::thread::spawn(move || {
            let message = match child.wait() {
                Ok(status) => format!("lios-worker exited: {status}"),
                Err(error) => format!("failed to wait for lios-worker: {error}"),
            };
            if let Ok(mut log) = OpenOptions::new().create(true).append(true).open(log_path) {
                let _ = writeln!(log, "{message}");
            }
        });
    } else {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
        command.spawn().map_err(to_err)?;
    }

    Ok(())
}
