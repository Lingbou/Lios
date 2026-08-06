mod args;
mod error;
mod output;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;
use std::process::{Command as ProcessCommand, Stdio};

use args::{
    parse_repository_address, AuthCommand, Cli, Command, CpArgs, KeyCommand, MkdirArgs, MvArgs,
    RmArgs, SpaceCommand, SyncArgs, TaskCommand, WorkerCommand,
};
use clap::Parser;
use error::{CliError, CliResult};
use lios_application::location::{Location, LocationParser, RemoteLocation};
use lios_application::service::Application;
use lios_application::space_registry::{validate_space_name, SpaceRegistry};
use lios_application::transfer_planner::{PlanOptions, TreeEntry};
use lios_application::transfer_request::{
    prepare_pull, prepare_push, PreparedPull, PreparedPush, RemoteSource,
};
use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind, DriveItem, DriveItemKind};
use lios_core::config::{LiosPaths, RepoConfig};
use lios_core::tasks::{TaskState, TaskSummary};
use output::{render_error, render_success, CommandOutput};
use serde_json::json;

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args = std::env::args_os().collect::<Vec<_>>();
    let requested_json = raw_args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(raw_args) {
        Ok(cli) => cli,
        Err(error) => {
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
                "warning": setup.warning,
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
                match fs::remove_file(&paths.credentials) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
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

async fn run_mkdir(
    application: &Application,
    registry: &SpaceRegistry,
    args: MkdirArgs,
) -> CliResult<CommandOutput> {
    let mut created = Vec::new();
    for operand in args.space_paths {
        let remote = parse_remote(registry, &operand)?;
        if remote.catalog_path == "/" {
            return Err(CliError::invalid_input("the Space root already exists"));
        }
        let repo = registry.resolve(&remote.space_name)?;
        let mut catalog = application.open_space(repo.clone()).await?;
        let segments = catalog_segments(&remote.catalog_path)?;
        let mut current_id = catalog.tree.id.clone();
        let mut current_path = String::new();
        for (index, segment) in segments.iter().enumerate() {
            current_path.push('/');
            current_path.push_str(segment);
            let existing = find_child(&catalog.tree, &current_id, segment)?;
            if let Some(node) = existing {
                if !matches!(node.kind, CatalogTreeNodeKind::Directory { .. }) {
                    return Err(CliError::invalid_input(format!(
                        "Catalog path component is a file: {current_path}"
                    )));
                }
                if index + 1 == segments.len() && !args.parents {
                    return Err(CliError::invalid_input(format!(
                        "Catalog directory already exists: {operand}"
                    )));
                }
                current_id = node.id.clone();
                continue;
            }
            if index + 1 != segments.len() && !args.parents {
                return Err(CliError::invalid_input(format!(
                    "parent Catalog directory does not exist: {current_path}"
                )));
            }
            catalog = application
                .create_folder_in(repo.clone(), &current_id, segment)
                .await?;
            current_id = resolve_catalog_path(&catalog.tree, &current_path)?
                .id
                .clone();
            created.push(format!("{}:{current_path}", remote.space_name));
        }
    }
    let mut output = CommandOutput::new(json!({"created": created}));
    if created.is_empty() {
        output = output.human("No directories created");
    } else {
        for path in &created {
            output = output.human(format!("Created {path}"));
        }
    }
    Ok(output)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MovePlan {
    source_id: String,
    new_parent_id: String,
    new_name: String,
    replace_id: Option<String>,
    source_kind: DriveItemKind,
    target_kind: Option<DriveItemKind>,
    rendered_destination: String,
}

async fn run_mv(
    application: &Application,
    registry: &SpaceRegistry,
    args: MvArgs,
) -> CliResult<CommandOutput> {
    if args.replace && !args.yes {
        return Err(CliError::invalid_input("--replace requires --yes"));
    }
    if args.replace_type && !args.yes {
        return Err(CliError::invalid_input("--replace-type requires --yes"));
    }
    let source = parse_remote(registry, &args.source)?;
    let destination = parse_remote(registry, &args.destination)?;
    if source.space_name != destination.space_name {
        return Err(CliError::invalid_input(
            "mv only supports paths within one Space",
        ));
    }
    if source.catalog_path == "/" {
        return Err(CliError::invalid_input("the Space root cannot be moved"));
    }
    let repo = registry.resolve(&source.space_name)?;
    let catalog = application.open_space(repo.clone()).await?;
    let plan = plan_remote_move(&catalog.tree, &source, &destination)?;
    if plan.replace_id.as_deref() == Some(plan.source_id.as_str()) {
        return Ok(CommandOutput::new(json!({
            "dry_run": args.dry_run,
            "changed": false,
            "destination": plan.rendered_destination,
        }))
        .human("Source and destination refer to the same Catalog node"));
    }
    if let Some(target_kind) = &plan.target_kind {
        if target_kind == &plan.source_kind {
            if !(args.replace && args.yes) {
                return Err(CliError::invalid_input(
                    "destination exists; replacing it requires --replace --yes",
                ));
            }
        } else if !(args.replace_type && args.yes) {
            return Err(CliError::invalid_input(
                "destination has a different type; replacement requires --replace-type --yes",
            ));
        }
    }
    if args.dry_run {
        return Ok(CommandOutput::new(json!({
            "dry_run": true,
            "source": args.source,
            "destination": plan.rendered_destination,
            "replace": plan.replace_id.is_some(),
        }))
        .human(format!(
            "Move {} -> {}",
            args.source, plan.rendered_destination
        )));
    }
    let snapshot = application
        .move_node_in(
            repo,
            &plan.source_id,
            &plan.new_parent_id,
            &plan.new_name,
            plan.replace_id.as_deref(),
        )
        .await?;
    Ok(CommandOutput::new(json!({
        "source": args.source,
        "destination": plan.rendered_destination,
        "warnings": snapshot.warnings,
    }))
    .human(format!(
        "Moved {} -> {}",
        args.source, plan.rendered_destination
    )))
}

async fn run_rm(
    application: &Application,
    registry: &SpaceRegistry,
    args: RmArgs,
) -> CliResult<CommandOutput> {
    let mut grouped = BTreeMap::<String, Vec<RemoteLocation>>::new();
    for operand in &args.space_paths {
        let remote = parse_remote(registry, operand)?;
        if remote.catalog_path == "/" {
            return Err(CliError::invalid_input(
                "rm cannot remove an entire Space; use `space remove` for the local alias",
            ));
        }
        grouped
            .entry(remote.space_name.clone())
            .or_default()
            .push(remote);
    }

    let mut plans = Vec::new();
    for (space_name, locations) in grouped {
        let repo = registry.resolve(&space_name)?;
        let catalog = application.open_space(repo.clone()).await?;
        let mut seen = HashSet::new();
        let mut node_ids = Vec::new();
        let mut paths = Vec::new();
        for location in locations {
            let node = resolve_catalog_path(&catalog.tree, &location.catalog_path)?;
            if seen.insert(node.id.clone()) {
                node_ids.push(node.id.clone());
                paths.push(format!("{space_name}:{}", location.catalog_path));
            }
        }
        plans.push((repo, node_ids, paths));
    }

    let rendered_paths = plans
        .iter()
        .flat_map(|(_, _, paths)| paths.iter().cloned())
        .collect::<Vec<_>>();
    if args.dry_run {
        let mut output = CommandOutput::new(json!({
            "dry_run": true,
            "delete": rendered_paths,
        }));
        for path in &rendered_paths {
            output = output.human(format!("Delete {path}"));
        }
        return Ok(output);
    }
    confirm_removal(&rendered_paths, args.yes)?;

    let mut task_ids = Vec::new();
    for (repo, node_ids, _) in plans {
        let task = application.queue_delete_for(repo, node_ids)?;
        task_ids.push(task.id);
        wait_for_worker_task(application, task.id).await?;
    }
    Ok(CommandOutput::new(json!({
        "tasks": task_ids,
        "deleted": rendered_paths,
    }))
    .human(format!("Removed {} Catalog path(s)", rendered_paths.len()))
    .human("Remote deletion removes Catalog references and may not reclaim ModelScope capacity"))
}

fn confirm_removal(paths: &[String], yes: bool) -> CliResult<()> {
    if yes {
        return Ok(());
    }
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(CliError::invalid_input(
            "rm requires --yes when standard input or output is not a TTY",
        ));
    }
    println!("The following Catalog paths will be removed:");
    for path in paths {
        println!("  {path}");
    }
    print!("Continue? [y/N]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    if io::stdin().read_line(&mut answer)? == 0 {
        return Err(CliError::invalid_input(
            "delete confirmation input ended before a choice was made",
        ));
    }
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        Ok(())
    } else {
        Err(CliError::invalid_input("delete was not confirmed"))
    }
}

async fn run_cp(
    application: &Application,
    registry: &SpaceRegistry,
    args: CpArgs,
) -> CliResult<CommandOutput> {
    let (source_operands, destination_operand) = args.sources_and_destination();
    let mut prepared = prepare_transfer_operands(
        application,
        registry,
        source_operands,
        destination_operand,
        PlanOptions {
            no_clobber: args.no_clobber,
            replace_type: args.replace_type,
            yes: args.yes,
            ..PlanOptions::default()
        },
    )
    .await?;
    if args.interactive {
        apply_interactive_choices(prepared.plan_mut())?;
    }
    if args.dry_run {
        return plan_output("copy", prepared.plan());
    }
    let (repo, plan) = prepared.into_persisted(
        source_operands.join("\0"),
        destination_operand.clone(),
        Vec::new(),
        None,
    )?;
    let task = application.queue_copy(repo, plan)?;
    if args.detach {
        start_worker(application.paths())?;
        return Ok(
            CommandOutput::new(json!({"task_id": task.id, "detached": true}))
                .human(format!("Queued copy task {}", task.id)),
        );
    }
    let result = wait_for_worker_task_with_progress(application, task.id, args.progress).await?;
    Ok(CommandOutput::new(json!({
        "task": result,
    }))
    .human(format!("Copy task {} completed", task.id)))
}

async fn run_sync(
    application: &Application,
    registry: &SpaceRegistry,
    args: SyncArgs,
) -> CliResult<CommandOutput> {
    if args.delete && !args.yes {
        return Err(CliError::invalid_input("--delete requires --yes"));
    }
    let mut excludes = args.exclude;
    if let Some(path) = args.exclude_from {
        let contents = fs::read_to_string(path)?;
        excludes.extend(
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_string),
        );
    }
    let prepared = prepare_transfer_operands(
        application,
        registry,
        std::slice::from_ref(&args.source),
        &args.destination,
        PlanOptions {
            delete: args.delete,
            exclude: excludes.clone(),
            exclude_root: None,
            replace_type: args.replace_type,
            yes: args.yes,
            no_clobber: false,
        },
    )
    .await?;
    if args.dry_run {
        return plan_output("sync", prepared.plan());
    }
    let delete_scope = args.delete.then(|| args.destination.clone());
    let (repo, plan) = prepared.into_persisted(
        args.source.clone(),
        args.destination.clone(),
        excludes,
        delete_scope,
    )?;
    let task = application.queue_sync(repo, plan)?;
    if args.detach {
        start_worker(application.paths())?;
        return Ok(
            CommandOutput::new(json!({"task_id": task.id, "detached": true}))
                .human(format!("Queued sync task {}", task.id)),
        );
    }
    let result = wait_for_worker_task_with_progress(application, task.id, args.progress).await?;
    Ok(CommandOutput::new(json!({
        "task": result,
    }))
    .human(format!("Sync task {} completed", task.id)))
}

fn start_worker(paths: &LiosPaths) -> CliResult<()> {
    if paths
        .worker_running()
        .map_err(lios_application::CommandError::from)?
    {
        return Ok(());
    }
    let _ = fs::remove_file(paths.worker_stop_path());
    let current = std::env::current_exe()?;
    let extension = current.extension().map(|value| value.to_os_string());
    let mut worker = current.with_file_name("lios-worker");
    if let Some(extension) = extension {
        worker.set_extension(extension);
    }
    if !worker.is_file() {
        return Err(CliError::new(format!(
            "lios-worker was not found beside {}",
            current.display()
        )));
    }
    let home_root = paths
        .home
        .parent()
        .ok_or_else(|| CliError::new("Lios Home has no parent directory for worker startup"))?;
    ProcessCommand::new(worker)
        .arg("--home")
        .arg(home_root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    Ok(())
}

async fn wait_for_worker_task(
    application: &Application,
    task_id: uuid::Uuid,
) -> CliResult<TaskSummary> {
    wait_for_worker_task_with_progress(application, task_id, false).await
}

async fn wait_for_worker_task_with_progress(
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

async fn wait_for_pause_after_interrupt(
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

async fn request_worker_interruption(
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

enum PreparedCliTransfer {
    Push {
        repo: RepoConfig,
        catalog: lios_application::service::CatalogSnapshot,
        source_trailing_slash: bool,
        prepared: PreparedPush,
    },
    Pull {
        repo: RepoConfig,
        catalog: lios_application::service::CatalogSnapshot,
        source_trailing_slash: bool,
        prepared: PreparedPull,
    },
}

impl PreparedCliTransfer {
    fn plan(&self) -> &lios_application::transfer_planner::TransferPlan {
        match self {
            Self::Push { prepared, .. } => &prepared.plan,
            Self::Pull { prepared, .. } => &prepared.plan,
        }
    }

    fn plan_mut(&mut self) -> &mut lios_application::transfer_planner::TransferPlan {
        match self {
            Self::Push { prepared, .. } => &mut prepared.plan,
            Self::Pull { prepared, .. } => &mut prepared.plan,
        }
    }

    fn into_persisted(
        self,
        source_operand: String,
        destination_operand: String,
        excludes: Vec<String>,
        delete_scope: Option<String>,
    ) -> CliResult<(RepoConfig, lios_core::tasks::PersistedTransferPlan)> {
        match self {
            Self::Push {
                repo,
                catalog,
                source_trailing_slash,
                prepared,
            } => {
                let baseline = lios_application::sha256_hex_file(&catalog.local_path)?;
                Ok((
                    repo,
                    prepared.into_persisted(
                        source_operand,
                        destination_operand,
                        source_trailing_slash,
                        excludes,
                        Some(baseline),
                        delete_scope,
                    ),
                ))
            }
            Self::Pull {
                repo,
                catalog,
                source_trailing_slash,
                prepared,
            } => {
                let baseline = lios_application::sha256_hex_file(&catalog.local_path)?;
                Ok((
                    repo,
                    prepared.into_persisted(
                        source_operand,
                        destination_operand,
                        source_trailing_slash,
                        excludes,
                        Some(baseline),
                        delete_scope,
                    ),
                ))
            }
        }
    }
}

async fn prepare_transfer_operands(
    application: &Application,
    registry: &SpaceRegistry,
    source_operands: &[String],
    destination_operand: &str,
    options: PlanOptions,
) -> CliResult<PreparedCliTransfer> {
    let names = registry.list()?.into_keys().collect::<Vec<_>>();
    let parser = LocationParser::new(names);
    let sources = source_operands
        .iter()
        .map(|source| parser.parse(source))
        .collect::<Result<Vec<_>, _>>()?;
    let destination = parser.parse(destination_operand)?;
    match destination {
        Location::Remote(remote) => {
            let local_sources = sources
                .into_iter()
                .map(|source| match source {
                    Location::Local(local) => Ok(local),
                    Location::Remote(_) => Err(CliError::invalid_input(
                        "Space-to-Space transfers are not supported",
                    )),
                })
                .collect::<CliResult<Vec<_>>>()?;
            let repo = registry.resolve(&remote.space_name)?;
            let catalog = application.open_space(repo.clone()).await?;
            let mut remote_entries = Vec::new();
            flatten_catalog_entries(&catalog.tree, "", &mut remote_entries);
            let prepared = prepare_push(
                &local_sources,
                &remote.catalog_path,
                &remote_entries,
                &options,
            )?;
            Ok(PreparedCliTransfer::Push {
                repo,
                catalog,
                source_trailing_slash: local_sources.iter().any(|source| source.trailing_slash),
                prepared,
            })
        }
        Location::Local(local_destination) => {
            let remote_sources = sources
                .into_iter()
                .map(|source| match source {
                    Location::Remote(remote) => Ok(remote),
                    Location::Local(_) => Err(CliError::invalid_input(
                        "a transfer must have exactly one local side and one Space side",
                    )),
                })
                .collect::<CliResult<Vec<_>>>()?;
            let space_name = remote_sources
                .first()
                .map(|source| source.space_name.clone())
                .ok_or_else(|| CliError::invalid_input("copy source cannot be empty"))?;
            if remote_sources
                .iter()
                .any(|source| source.space_name != space_name)
            {
                return Err(CliError::invalid_input(
                    "all remote copy sources must belong to the same Space",
                ));
            }
            let repo = registry.resolve(&space_name)?;
            let catalog = application.open_space(repo.clone()).await?;
            let sources = remote_sources
                .iter()
                .map(|source| {
                    Ok(RemoteSource {
                        node: resolve_catalog_path(&catalog.tree, &source.catalog_path)?.clone(),
                        trailing_slash: source.trailing_slash,
                    })
                })
                .collect::<CliResult<Vec<_>>>()?;
            let prepared = prepare_pull(&sources, &local_destination, &options)?;
            Ok(PreparedCliTransfer::Pull {
                repo,
                catalog,
                source_trailing_slash: remote_sources.iter().any(|source| source.trailing_slash),
                prepared,
            })
        }
    }
}

fn plan_output(
    label: &str,
    plan: &lios_application::transfer_planner::TransferPlan,
) -> CliResult<CommandOutput> {
    let mut output = CommandOutput::new(json!({
        "dry_run": true,
        "actions": plan.actions,
    }));
    for action in &plan.actions {
        output = output.human(format!("{:?}  {}", action.kind, action.path));
    }
    if plan.actions.is_empty() {
        output = output.human(format!("{label}: no changes"));
    }
    Ok(output)
}

fn apply_interactive_choices(
    plan: &mut lios_application::transfer_planner::TransferPlan,
) -> CliResult<()> {
    use lios_application::transfer_planner::PlanActionKind;
    use std::io::Write;

    for action in &mut plan.actions {
        if action.kind != PlanActionKind::Update {
            continue;
        }
        print!("Replace {}? [y/N]: ", action.path);
        io::stdout().flush()?;
        let mut answer = String::new();
        if io::stdin().read_line(&mut answer)? == 0 {
            return Err(CliError::invalid_input(
                "interactive input ended before a choice was made",
            ));
        }
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            action.kind = PlanActionKind::Skip;
        }
    }
    Ok(())
}

fn parse_remote(registry: &SpaceRegistry, operand: &str) -> CliResult<RemoteLocation> {
    let names = registry.list()?.into_keys().collect::<Vec<_>>();
    match LocationParser::new(names).parse(operand)? {
        Location::Remote(remote) => Ok(remote),
        Location::Local(_) => Err(CliError::invalid_input(
            "this command requires a Space path such as `photos:/docs`",
        )),
    }
}

fn resolve_catalog_path<'a>(
    root: &'a CatalogTreeNode,
    path: &str,
) -> CliResult<&'a CatalogTreeNode> {
    if !path.starts_with('/') {
        return Err(CliError::invalid_input(
            "Catalog paths must be absolute and start with /",
        ));
    }
    let mut current = root;
    for segment in path.split('/').filter(|segment| !segment.is_empty()) {
        current = match &current.kind {
            CatalogTreeNodeKind::Directory { children } => children
                .iter()
                .find(|child| child.name.eq_ignore_ascii_case(segment))
                .ok_or_else(|| {
                    CliError::invalid_input(format!("Catalog path was not found: {path}"))
                })?,
            CatalogTreeNodeKind::File { .. } => {
                return Err(CliError::invalid_input(format!(
                    "Catalog path traverses through a file: {path}"
                )))
            }
        };
    }
    Ok(current)
}

fn catalog_segments(path: &str) -> CliResult<Vec<&str>> {
    if !path.starts_with('/') {
        return Err(CliError::invalid_input(
            "Catalog paths must be absolute and start with /",
        ));
    }
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    if segments
        .iter()
        .any(|segment| matches!(*segment, "." | ".."))
    {
        return Err(CliError::invalid_input(
            "Catalog paths cannot contain . or .. components",
        ));
    }
    Ok(segments)
}

fn find_node_by_id<'a>(root: &'a CatalogTreeNode, id: &str) -> Option<&'a CatalogTreeNode> {
    if root.id == id {
        return Some(root);
    }
    let CatalogTreeNodeKind::Directory { children } = &root.kind else {
        return None;
    };
    children.iter().find_map(|child| find_node_by_id(child, id))
}

fn find_child<'a>(
    root: &'a CatalogTreeNode,
    parent_id: &str,
    name: &str,
) -> CliResult<Option<&'a CatalogTreeNode>> {
    let parent = find_node_by_id(root, parent_id)
        .ok_or_else(|| CliError::invalid_input("Catalog parent node was not found"))?;
    let CatalogTreeNodeKind::Directory { children } = &parent.kind else {
        return Err(CliError::invalid_input("Catalog parent is not a directory"));
    };
    Ok(children
        .iter()
        .find(|child| child.name.eq_ignore_ascii_case(name)))
}

fn node_kind(node: &CatalogTreeNode) -> DriveItemKind {
    match node.kind {
        CatalogTreeNodeKind::Directory { .. } => DriveItemKind::Directory,
        CatalogTreeNodeKind::File { .. } => DriveItemKind::File,
    }
}

fn split_catalog_parent(path: &str) -> CliResult<(String, String)> {
    let segments = catalog_segments(path)?;
    let (name, parents) = segments
        .split_last()
        .ok_or_else(|| CliError::invalid_input("the Space root has no parent"))?;
    let parent = if parents.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", parents.join("/"))
    };
    Ok((parent, (*name).to_string()))
}

fn join_catalog_path(parent: &str, name: &str) -> String {
    if parent == "/" {
        format!("/{name}")
    } else {
        format!("{}/{name}", parent.trim_end_matches('/'))
    }
}

fn plan_remote_move(
    root: &CatalogTreeNode,
    source: &RemoteLocation,
    destination: &RemoteLocation,
) -> CliResult<MovePlan> {
    let source_node = resolve_catalog_path(root, &source.catalog_path)?;
    if source
        .catalog_path
        .eq_ignore_ascii_case(&destination.catalog_path)
    {
        let (parent_path, new_name) = split_catalog_parent(&source.catalog_path)?;
        let parent = resolve_catalog_path(root, &parent_path)?;
        return Ok(MovePlan {
            source_id: source_node.id.clone(),
            new_parent_id: parent.id.clone(),
            new_name,
            replace_id: Some(source_node.id.clone()),
            source_kind: node_kind(source_node),
            target_kind: Some(node_kind(source_node)),
            rendered_destination: format!(
                "{}:{}",
                destination.space_name, destination.catalog_path
            ),
        });
    }

    let source_kind = node_kind(source_node);
    let (new_parent, new_name, replacement, target_path) =
        match resolve_catalog_path(root, &destination.catalog_path) {
            Ok(node) if matches!(node.kind, CatalogTreeNodeKind::Directory { .. }) => {
                let source_name = source_node.name.clone();
                let replacement = find_child(root, &node.id, &source_name)?;
                (
                    node,
                    source_name.clone(),
                    replacement,
                    join_catalog_path(&destination.catalog_path, &source_name),
                )
            }
            Ok(node) => {
                let (parent_path, name) = split_catalog_parent(&destination.catalog_path)?;
                let parent = resolve_catalog_path(root, &parent_path)?;
                if !matches!(parent.kind, CatalogTreeNodeKind::Directory { .. }) {
                    return Err(CliError::invalid_input(
                        "mv destination parent is not a directory",
                    ));
                }
                (parent, name, Some(node), destination.catalog_path.clone())
            }
            Err(_) => {
                let (parent_path, name) = split_catalog_parent(&destination.catalog_path)?;
                let parent = resolve_catalog_path(root, &parent_path)?;
                if !matches!(parent.kind, CatalogTreeNodeKind::Directory { .. }) {
                    return Err(CliError::invalid_input(
                        "mv destination parent is not a directory",
                    ));
                }
                (parent, name, None, destination.catalog_path.clone())
            }
        };

    Ok(MovePlan {
        source_id: source_node.id.clone(),
        new_parent_id: new_parent.id.clone(),
        new_name,
        replace_id: replacement.map(|node| node.id.clone()),
        source_kind,
        target_kind: replacement.map(node_kind),
        rendered_destination: format!("{}:{target_path}", destination.space_name),
    })
}

fn list_tree_path(root: &CatalogTreeNode, path: &str) -> CliResult<Vec<DriveItem>> {
    let node = resolve_catalog_path(root, path)?;
    let CatalogTreeNodeKind::Directory { children } = &node.kind else {
        return Err(CliError::invalid_input(format!(
            "Catalog path is not a directory: {path}"
        )));
    };
    Ok(children.iter().map(tree_node_to_drive_item).collect())
}

fn tree_node_to_drive_item(node: &CatalogTreeNode) -> DriveItem {
    let (kind, size, children_count) = match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            (DriveItemKind::Directory, 0, children.len())
        }
        CatalogTreeNodeKind::File { original_size, .. } => (DriveItemKind::File, *original_size, 0),
    };
    DriveItem {
        id: node.id.clone(),
        name: node.name.clone(),
        kind,
        size,
        updated_at: node.updated_at.clone(),
        children_count,
    }
}

fn collect_matches(node: &CatalogTreeNode, query: &str, matches: &mut Vec<DriveItem>) {
    if node.name.to_ascii_lowercase().contains(query) {
        matches.push(tree_node_to_drive_item(node));
    }
    if let CatalogTreeNodeKind::Directory { children } = &node.kind {
        for child in children {
            collect_matches(child, query, matches);
        }
    }
}

fn flatten_catalog_entries(node: &CatalogTreeNode, parent: &str, entries: &mut Vec<TreeEntry>) {
    let path = if parent.is_empty() {
        String::new()
    } else {
        format!("{parent}/{}", node.name)
    };
    match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            if !path.is_empty() {
                entries.push(TreeEntry::directory(path.clone()));
            }
            let child_parent = if path.is_empty() { "" } else { path.as_str() };
            for child in children {
                let child_path = if child_parent.is_empty() {
                    child.name.clone()
                } else {
                    format!("{child_parent}/{}", child.name)
                };
                match &child.kind {
                    CatalogTreeNodeKind::Directory { .. } => {
                        entries.push(TreeEntry::directory(child_path.clone()));
                        flatten_catalog_children(child, &child_path, entries);
                    }
                    CatalogTreeNodeKind::File {
                        original_size,
                        sha256,
                        ..
                    } => entries.push(TreeEntry::file(child_path, sha256.clone(), *original_size)),
                }
            }
        }
        CatalogTreeNodeKind::File {
            original_size,
            sha256,
            ..
        } if !path.is_empty() => {
            entries.push(TreeEntry::file(path, sha256.clone(), *original_size))
        }
        CatalogTreeNodeKind::File { .. } => {}
    }
}

fn flatten_catalog_children(node: &CatalogTreeNode, node_path: &str, entries: &mut Vec<TreeEntry>) {
    let CatalogTreeNodeKind::Directory { children } = &node.kind else {
        return;
    };
    for child in children {
        let path = format!("{node_path}/{}", child.name);
        match &child.kind {
            CatalogTreeNodeKind::Directory { .. } => {
                entries.push(TreeEntry::directory(path.clone()));
                flatten_catalog_children(child, &path, entries);
            }
            CatalogTreeNodeKind::File {
                original_size,
                sha256,
                ..
            } => entries.push(TreeEntry::file(path, sha256.clone(), *original_size)),
        }
    }
}

fn item_kind(item: &DriveItem) -> &'static str {
    match item.kind {
        DriveItemKind::Directory => "dir ",
        DriveItemKind::File => "file",
    }
}

async fn run_space(
    application: Application,
    registry: SpaceRegistry,
    command: SpaceCommand,
) -> CliResult<CommandOutput> {
    match command {
        SpaceCommand::List => {
            let spaces = registry.list()?;
            let mut output = CommandOutput::new(&spaces);
            if spaces.is_empty() {
                output = output.human("No registered Spaces");
            } else {
                for (name, repo) in spaces {
                    output = output.human(format!(
                        "{name}:  {}/{}  {}",
                        repo.namespace, repo.dataset, repo.endpoint
                    ));
                }
            }
            Ok(output)
        }
        SpaceCommand::Show { name, remote } => {
            let repo = registry.resolve(&name)?;
            let mut result = json!({"name": name, "repository": repo});
            if remote {
                let catalog = application.open_space(repo.clone()).await?;
                result["catalog_root"] = serde_json::to_value(catalog.tree)?;
            }
            Ok(CommandOutput::new(result).human(format!(
                "{}: {}/{} ({})",
                name, repo.namespace, repo.dataset, repo.endpoint
            )))
        }
        SpaceCommand::Rename { old, new } => {
            registry.rename(&old, &new)?;
            Ok(CommandOutput::new(json!({"old": old, "new": new}))
                .human(format!("Renamed Space {old}: to {new}:")))
        }
        SpaceCommand::Remove { name, force: _ } => {
            registry.remove(&name)?;
            Ok(CommandOutput::new(json!({"removed": name}))
                .human(format!("Removed local Space alias {name}:")))
        }
        SpaceCommand::Discover { endpoint } => {
            let repositories = application.list_dataset_repos(Some(endpoint)).await?;
            Ok(
                CommandOutput::new(&repositories.repositories).human(format!(
                    "Discovered {} Dataset Repository candidate(s)",
                    repositories.repositories.len()
                )),
            )
        }
        SpaceCommand::Add {
            name,
            repository,
            endpoint,
        } => {
            let repo = parse_repository_address(&repository, endpoint)?;
            registry.ensure_can_add(&name, &repo)?;
            application.open_space(repo.clone()).await?;
            registry.add(&name, repo.clone())?;
            registered_space_output(name, repo)
        }
        SpaceCommand::Init {
            name,
            repository,
            endpoint,
        } => {
            let repo = parse_repository_address(&repository, endpoint)?;
            registry.ensure_can_add(&name, &repo)?;
            application.initialize_space(repo.clone()).await?;
            registry.add(&name, repo.clone())?;
            registered_space_output(name, repo)
        }
        SpaceCommand::Create {
            name,
            namespace,
            dataset,
            endpoint,
        } => {
            validate_space_name(&name)?;
            let namespace = match namespace {
                Some(namespace) => namespace,
                None => {
                    application
                        .list_dataset_repos(Some(endpoint.clone()))
                        .await?
                        .user
                        .username
                }
            };
            let repo = lios_application::production_config::validate_repo(RepoConfig {
                namespace,
                dataset: dataset.unwrap_or_else(|| name.clone()),
                endpoint,
            })?;
            registry.ensure_can_add(&name, &repo)?;
            application.create_dataset_repo(repo.clone()).await?;
            if let Err(error) = application.initialize_space(repo.clone()).await {
                let mut cli_error = CliError::from(error);
                cli_error.message = format!(
                    "repository {}/{} was created but not registered; retry with `lios space init {} {}/{}`: {}",
                    repo.namespace, repo.dataset, name, repo.namespace, repo.dataset, cli_error.message
                );
                return Err(cli_error);
            }
            registry.add(&name, repo.clone())?;
            registered_space_output(name, repo)
        }
    }
}

fn registered_space_output(name: String, repo: RepoConfig) -> CliResult<CommandOutput> {
    Ok(
        CommandOutput::new(json!({"name": name, "repository": repo})).human(format!(
            "Registered {name}: as {}/{}",
            repo.namespace, repo.dataset
        )),
    )
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

    fn directory(id: &str, name: &str, children: Vec<CatalogTreeNode>) -> CatalogTreeNode {
        CatalogTreeNode {
            id: id.to_string(),
            name: name.to_string(),
            updated_at: "2026-07-30T00:00:00Z".to_string(),
            kind: CatalogTreeNodeKind::Directory { children },
        }
    }

    fn file(id: &str, name: &str) -> CatalogTreeNode {
        CatalogTreeNode {
            id: id.to_string(),
            name: name.to_string(),
            updated_at: "2026-07-30T00:00:00Z".to_string(),
            kind: CatalogTreeNodeKind::File {
                original_size: 1,
                sha256: "00".repeat(32),
                object_id: id.to_string(),
                chunk_count: 1,
            },
        }
    }

    fn remote(path: &str) -> RemoteLocation {
        RemoteLocation {
            space_name: "photos".to_string(),
            catalog_path: path.to_string(),
            trailing_slash: false,
        }
    }

    #[test]
    fn mv_into_existing_directory_keeps_the_source_name() {
        let tree = directory(
            "root",
            "photos",
            vec![
                file("source", "a.jpg"),
                directory("archive", "archive", vec![]),
            ],
        );

        let plan = plan_remote_move(&tree, &remote("/a.jpg"), &remote("/archive")).unwrap();

        assert_eq!(plan.new_parent_id, "archive");
        assert_eq!(plan.new_name, "a.jpg");
        assert_eq!(plan.rendered_destination, "photos:/archive/a.jpg");
        assert_eq!(plan.replace_id, None);
    }

    #[test]
    fn mv_to_existing_path_reports_the_replacement_type() {
        let tree = directory(
            "root",
            "photos",
            vec![
                directory("source", "album", vec![]),
                file("target", "renamed"),
            ],
        );

        let plan = plan_remote_move(&tree, &remote("/album"), &remote("/renamed")).unwrap();

        assert_eq!(plan.replace_id.as_deref(), Some("target"));
        assert_eq!(plan.source_kind, DriveItemKind::Directory);
        assert_eq!(plan.target_kind, Some(DriveItemKind::File));
    }

    #[test]
    fn catalog_parent_split_is_root_aware() {
        assert_eq!(
            split_catalog_parent("/docs").unwrap(),
            ("/".to_string(), "docs".to_string())
        );
        assert_eq!(
            split_catalog_parent("/a/b").unwrap(),
            ("/a".to_string(), "b".to_string())
        );
    }

    #[test]
    fn json_rm_rejects_interactive_confirmation() {
        let cli =
            Cli::try_parse_from(["lios", "--json", "rm", "photos:/old", "--recursive"]).unwrap();

        let error = reject_json_interaction(&cli).unwrap_err();

        assert!(error.message.contains("--yes"));
    }
}
