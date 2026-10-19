mod args;
mod commands;
mod error;
mod output;

use std::fs;
use std::io::{self, Read};
use std::process::ExitCode;

use args::{AuthCommand, Cli, Command, KeyCommand, TaskCommand, WorkerCommand};
use clap::Parser;
use commands::*;
use error::{CliError, CliResult};
use lios_application::service::Application;
use lios_application::space_registry::SpaceRegistry;
use lios_core::config::LiosPaths;
use lios_core::tasks::TaskState;
use output::{render_error, render_success, CommandOutput};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args = std::env::args_os().collect::<Vec<_>>();
    let requested_json = raw_args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(raw_args) {
        Ok(cli) => cli,
        Err(error) => {
            if error.exit_code() == 0 {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            if requested_json {
                let cli_error = CliError::invalid_input(error.to_string());
                render_error(true, "parse", &cli_error);
                return ExitCode::from(cli_error.exit_code());
            }
            let _ = error.print();
            return ExitCode::from(2);
        }
    };
    let command = cli.command.name();
    match run(cli).await {
        Ok((json_mode, output)) => {
            render_success(json_mode, command, output);
            ExitCode::SUCCESS
        }
        Err((json_mode, error)) => {
            render_error(json_mode, command, &error);
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> Result<(bool, CommandOutput), (bool, CliError)> {
    let json_mode = cli.json;
    run_inner(cli)
        .await
        .map(|output| (json_mode, output))
        .map_err(|error| (json_mode, error))
}

async fn run_inner(cli: Cli) -> CliResult<CommandOutput> {
    reject_json_interaction(&cli)?;
    let paths = cli
        .home
        .map(LiosPaths::from_home)
        .unwrap_or_else(LiosPaths::default_user);
    let registry = SpaceRegistry::new(paths.clone());
    let read_only = matches!(
        cli.command,
        Command::Status
            | Command::Auth(args::AuthArgs {
                command: AuthCommand::Status
            })
            | Command::Worker(_)
    );
    let application = if read_only {
        Application::new_without_initializing(paths.clone())?
    } else {
        Application::new(paths.clone())?
    };

    match cli.command {
        Command::Setup => {
            let setup = application.setup()?;
            Ok(CommandOutput::new(json!({
                "home": setup.paths.home,
                "recovery_key": setup.recovery_key,
                "spaces": setup.config.spaces,
                "has_token": setup.has_token,
            }))
            .human(format!(
                "Lios state initialized at {}",
                setup.paths.home.display()
            ))
            .human("Setup did not log in or replace an existing Recovery Key"))
        }
        Command::Status => {
            let setup = application.inspect_setup()?;
            Ok(CommandOutput::new(json!({
                "version": env!("CARGO_PKG_VERSION"),
                "initialized": setup.initialized,
                "home": setup.paths.home,
                "spaces": setup.config.spaces,
                "has_token": setup.has_token,
                "recovery_key": setup.recovery_key,
            }))
            .human(format!("Lios {}", env!("CARGO_PKG_VERSION")))
            .human(if setup.initialized {
                format!("State: {}", setup.paths.home.display())
            } else {
                format!("State: not initialized ({})", setup.paths.home.display())
            })
            .human(format!(
                "Recovery key: {}",
                setup
                    .recovery_key
                    .key_location
                    .as_deref()
                    .unwrap_or("missing")
            ))
            .human(format!("Spaces: {}", setup.config.spaces.len()))
            .human(format!(
                "ModelScope token: {}",
                if setup.has_token { "saved" } else { "missing" }
            )))
        }
        Command::Auth(args) => match args.command {
            AuthCommand::Login { token_stdin } => {
                let token = if token_stdin {
                    let mut token = String::new();
                    io::stdin().read_to_string(&mut token)?;
                    token
                } else {
                    rpassword::prompt_password("ModelScope token: ")?
                };
                application.set_token(&token)?;
                Ok(CommandOutput::new(json!({"authenticated": true}))
                    .human("ModelScope token saved"))
            }
            AuthCommand::Status => {
                let saved = paths.credentials.is_file();
                Ok(CommandOutput::new(json!({"saved": saved})).human(if saved {
                    "ModelScope token: saved"
                } else {
                    "ModelScope token: missing"
                }))
            }
            AuthCommand::Logout => {
                application.clear_token()?;
                Ok(CommandOutput::new(json!({"authenticated": false}))
                    .human("ModelScope token removed"))
            }
        },
        Command::Key(args) => match args.command {
            KeyCommand::Status => {
                let status = application.recovery_key_status()?;
                Ok(CommandOutput::new(&status).human(format!(
                    "Recovery Key: {}",
                    status.key_location.as_deref().unwrap_or("missing")
                )))
            }
            KeyCommand::Backup { destination } => {
                let status = application.backup_recovery_key(&destination)?;
                Ok(CommandOutput::new(&status).human(format!(
                    "Recovery Key backed up to {}",
                    destination.display()
                )))
            }
            KeyCommand::Verify { path } => {
                let verification = application.verify_recovery_key(&path).await?;
                Ok(CommandOutput::new(&verification).human("Recovery Key verified"))
            }
            KeyCommand::Import { path } => {
                let verification = application.import_recovery_key(&path).await?;
                Ok(CommandOutput::new(&verification).human("Recovery Key imported"))
            }
        },
        Command::Space(args) => run_space(application, registry, args.command).await,
        Command::Ls(args) => {
            let remote = parse_remote(&registry, &args.space_path)?;
            let repo = registry.resolve(&remote.space_name)?;
            let catalog = application.open_space(repo).await?;
            let items = list_tree_path(&catalog.tree, &remote.catalog_path)?;
            let mut output = CommandOutput::new(&items);
            if items.is_empty() {
                output = output.human("(empty)");
            }
            for item in items {
                output = if args.long {
                    output.human(format!("{}  {}  {}", item_kind(&item), item.id, item.name))
                } else {
                    output.human(format!("{}  {}", item_kind(&item), item.name))
                };
            }
            Ok(output)
        }
        Command::Search(args) => {
            let remote = parse_remote(&registry, &args.space_path)?;
            let repo = registry.resolve(&remote.space_name)?;
            let catalog = application.open_space(repo).await?;
            let root = resolve_catalog_path(&catalog.tree, &remote.catalog_path)?;
            let mut items = Vec::new();
            collect_matches(root, &args.query.to_ascii_lowercase(), &mut items);
            let mut output = CommandOutput::new(&items);
            for item in items {
                output = output.human(format!("{}  {}", item_kind(&item), item.name));
            }
            Ok(output)
        }
        Command::Verify(args) => {
            let remote = parse_remote(&registry, &args.space)?;
            if remote.catalog_path != "/" {
                return Err(CliError::invalid_input(
                    "verify accepts an entire Space such as `photos:`",
                ));
            }
            let repo = registry.resolve(&remote.space_name)?;
            let task = application.queue_verify_for(repo, args.full)?;
            let result = wait_for_worker_task(&application, task.id).await?;
            Ok(CommandOutput::new(json!({
                "task": result,
            }))
            .human(format!("Verification task {} completed", task.id)))
        }
        Command::Mkdir(args) => run_mkdir(&application, &registry, args).await,
        Command::Cp(args) => run_cp(&application, &registry, args).await,
        Command::Sync(args) => run_sync(&application, &registry, args).await,
        Command::Mv(args) => run_mv(&application, &registry, args).await,
        Command::Rm(args) => run_rm(&application, &registry, args).await,
        Command::Task(args) => match args.command {
            TaskCommand::List => {
                let tasks = application.list_tasks()?;
                Ok(CommandOutput::new(&tasks).human(format!("{} task(s)", tasks.len())))
            }
            TaskCommand::Show { id } => {
                let task = application
                    .get_task(id)?
                    .ok_or_else(|| CliError::invalid_input("task was not found"))?;
                Ok(CommandOutput::new(&task).human(format!("{}  {:?}", task.id, task.state)))
            }
            TaskCommand::Wait { id } => {
                let result = wait_for_worker_task(&application, id).await?;
                Ok(CommandOutput::new(&result).human(format!("{}  {:?}", result.id, result.state)))
            }
            TaskCommand::Pause { id } => {
                let task = request_worker_interruption(&application, id, false).await?;
                Ok(CommandOutput::new(&task).human(format!("Paused task {id}")))
            }
            TaskCommand::Resume { id } => {
                application.requeue_paused_task(id)?;
                let result = wait_for_worker_task(&application, id).await?;
                Ok(CommandOutput::new(&result).human(format!("Resumed task {id}")))
            }
            TaskCommand::Retry { id } => {
                application.requeue_failed_task(id)?;
                let result = wait_for_worker_task(&application, id).await?;
                Ok(CommandOutput::new(&result).human(format!("Retried task {id}")))
            }
            TaskCommand::Cancel { id } => {
                let task = request_worker_interruption(&application, id, true).await?;
                Ok(CommandOutput::new(&task).human(format!("Canceled task {id}")))
            }
            TaskCommand::Clear(clear) => {
                let tasks = application.list_tasks()?;
                let ids = if let Some(id) = clear.id {
                    vec![id]
                } else {
                    if !clear.completed && !clear.failed && !clear.all_terminal {
                        return Err(CliError::invalid_input(
                            "task clear requires ID, --completed, --failed, or --all-terminal",
                        ));
                    }
                    tasks
                        .into_iter()
                        .filter(|task| {
                            clear.all_terminal
                                && matches!(
                                    task.state,
                                    TaskState::Completed | TaskState::Failed | TaskState::Canceled
                                )
                                || clear.completed && task.state == TaskState::Completed
                                || clear.failed && task.state == TaskState::Failed
                        })
                        .map(|task| task.id)
                        .collect()
                };
                for id in &ids {
                    application.clear_task(*id)?;
                }
                Ok(CommandOutput::new(json!({"cleared": ids}))
                    .human(format!("Cleared {} task record(s)", ids.len())))
            }
        },
        Command::Worker(args) => match args.command {
            WorkerCommand::Status => {
                let running = paths
                    .worker_running()
                    .map_err(lios_application::CommandError::from)?;
                Ok(
                    CommandOutput::new(json!({"running": running})).human(if running {
                        "Worker: running"
                    } else {
                        "Worker: stopped"
                    }),
                )
            }
            WorkerCommand::Stop => {
                let running = paths
                    .worker_running()
                    .map_err(lios_application::CommandError::from)?;
                if running {
                    fs::write(paths.worker_stop_path(), b"stop\n")?;
                }
                Ok(
                    CommandOutput::new(json!({"stop_requested": running})).human(if running {
                        "Worker stop requested"
                    } else {
                        "Worker is not running"
                    }),
                )
            }
        },
    }
}

fn reject_json_interaction(cli: &Cli) -> CliResult<()> {
    if !cli.json {
        return Ok(());
    }
    match &cli.command {
        Command::Auth(args::AuthArgs {
            command: AuthCommand::Login { token_stdin: false },
        }) => Err(CliError::invalid_input(
            "--json requires `auth login --token-stdin`",
        )),
        Command::Cp(args) if args.interactive || args.progress => Err(CliError::invalid_input(
            "--json cannot be combined with interaction or progress",
        )),
        Command::Sync(args) if args.progress => Err(CliError::invalid_input(
            "--json cannot be combined with progress",
        )),
        Command::Rm(args) if !args.yes && !args.dry_run => Err(CliError::invalid_input(
            "--json requires `rm --yes` unless --dry-run is used",
        )),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_rm_rejects_interactive_confirmation() {
        let cli =
            Cli::try_parse_from(["lios", "--json", "rm", "photos:/old", "--recursive"]).unwrap();

        let error = reject_json_interaction(&cli).unwrap_err();

        assert!(error.message.contains("--yes"));
    }
}
