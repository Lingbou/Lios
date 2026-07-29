mod args;
mod direct;
mod error;

use std::io::{self, Write};
use std::process::ExitCode;
use std::time::{Duration, Instant};

use args::{Cli, Command, ReposCommand, SpaceCommand, TaskCommand};
use clap::Parser;
use direct::{CliContext, SpaceReport};
use error::CliResult;
use lios_application::service::CatalogSnapshot;
use lios_application::task_runner::{ForegroundProgress, TaskRunResult};
use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind, DriveItem, DriveItemKind};
use lios_core::tasks::{TaskState, TaskSummary};

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("lios: {error}");
            ExitCode::from(error.exit_code())
        }
    }
}

async fn run(cli: Cli) -> CliResult<()> {
    let context = if matches!(&cli.command, Command::Status(_)) {
        CliContext::new_for_status(cli.home)?
    } else {
        CliContext::new(cli.home)?
    };
    match cli.command {
        Command::Setup => {
            let snapshot = context.setup()?;
            println!(
                "Lios state initialized at {}",
                snapshot.paths.home.display()
            );
            if let Some(path) = snapshot.recovery_key.key_location {
                println!("Recovery key: {path}");
            }
            println!("ModelScope token: saved");
            if let Some(repo) = snapshot.config.active_repo {
                println!(
                    "Active space: {}/{} ({})",
                    repo.namespace, repo.dataset, repo.endpoint
                );
            }
            if let Some(warning) = snapshot.warning {
                eprintln!("warning: {}", warning.message);
            }
        }
        Command::Auth => {
            context.auth()?;
            println!("ModelScope token updated");
        }
        Command::Status(args) => {
            let report = context.status(args.remote).await?;
            println!("Lios {}", env!("CARGO_PKG_VERSION"));
            if report.setup.paths.config.is_file() {
                println!("State: {}", report.setup.paths.home.display());
            } else {
                println!(
                    "State: not initialized ({})",
                    report.setup.paths.home.display()
                );
            }
            match report.setup.recovery_key.key_location {
                Some(path) => println!("Recovery key: configured at {path}"),
                None => println!("Recovery key: missing"),
            }
            println!(
                "Recovery-key backup: {}",
                if report.setup.recovery_key.backed_up {
                    "verified"
                } else {
                    "not verified"
                }
            );
            println!(
                "ModelScope token: {}",
                if report.setup.has_token {
                    "saved"
                } else {
                    "missing (run `lios auth`)"
                }
            );
            if let Some(repo) = report.setup.config.active_repo {
                println!(
                    "Active space: {}/{} ({})",
                    repo.namespace, repo.dataset, repo.endpoint
                );
            } else {
                println!("Active space: none");
            }
            if let Some(user) = report.remote_user {
                println!("ModelScope user: {user}");
            }
            if let Some(warning) = report.setup.warning {
                eprintln!("warning: {}", warning.message);
            }
        }
        Command::Repos(args) => match args.command {
            ReposCommand::List(args) => {
                let report = context.list_repositories(args.endpoint).await?;
                println!("ModelScope repositories for {}:", report.user.username);
                if report.repositories.is_empty() {
                    println!("  (none)");
                }
                for repo in report.repositories {
                    println!(
                        "  {}/{}  {}",
                        repo.namespace,
                        repo.dataset,
                        repo.visibility.as_deref().unwrap_or("unknown")
                    );
                }
            }
            ReposCommand::Create(args) => {
                let repo = args.into_config()?;
                context.create_repository(repo.clone()).await?;
                println!(
                    "Created private repository {}/{}",
                    repo.namespace, repo.dataset
                );
                println!(
                    "It is now the active repository; run `lios space init` to initialize it."
                );
            }
        },
        Command::Space(args) => match args.command {
            SpaceCommand::Open(args) => {
                let report = context.open_space(args.into_config()?).await?;
                print_space_report("Opened", &report);
            }
            SpaceCommand::Init(args) => {
                let report = context.initialize_space(args.into_config()?).await?;
                print_space_report("Initialized", &report);
            }
        },
        Command::Ls(args) => {
            let items = context.list_path(&args.path).await?;
            print_items(&items);
        }
        Command::Search(args) => {
            let items = context.search(&args.query).await?;
            print_items(&items);
        }
        Command::Mkdir(args) => {
            let snapshot = context.create_folder(&args.parent, &args.name).await?;
            println!("Created directory `{}`", args.name);
            print_catalog_warnings(&snapshot);
        }
        Command::Rename(args) => {
            let snapshot = context.rename_node(&args.node, &args.new_name).await?;
            println!("Renamed {} to `{}`", args.node, args.new_name);
            print_catalog_warnings(&snapshot);
        }
        Command::Upload(args) => {
            let task = context.queue_upload(args.parent, args.paths).await?;
            run_queued_task(&context, task).await?;
        }
        Command::Download(args) => {
            let task = context.queue_download(args.nodes, args.output)?;
            run_queued_task(&context, task).await?;
        }
        Command::Delete(args) => {
            if !confirm_delete(&args.nodes)? {
                println!("Delete canceled");
                return Ok(());
            }
            let task = context.queue_delete(args.nodes)?;
            run_queued_task(&context, task).await?;
        }
        Command::Verify(args) => {
            let task = context.queue_verify(args.full)?;
            run_queued_task(&context, task).await?;
        }
        Command::Task(args) => match args.command {
            TaskCommand::List => {
                let tasks = context.list_tasks()?;
                print_tasks(&tasks);
            }
            TaskCommand::Resume { task_id } => {
                println!("Resuming task {task_id}");
                let mut progress = ProgressPrinter::default();
                let result = context
                    .resume_task(task_id, |update| progress.update(update))
                    .await?;
                print_task_result(&result);
            }
        },
    }
    Ok(())
}

async fn run_queued_task(context: &CliContext, task: TaskSummary) -> CliResult<()> {
    println!("Running {} task {}", task.label, task.id);
    let mut progress = ProgressPrinter::default();
    let result = context
        .run_task(task.id, |update| progress.update(update))
        .await?;
    print_task_result(&result);
    Ok(())
}

fn print_task_result(result: &TaskRunResult) {
    let summary = &result.summary;
    println!(
        "Task {} {:?}: {} items, {} transferred",
        summary.id,
        summary.state,
        summary.progress_done,
        format_bytes(summary.bytes_done)
    );
    for notice in &result.notices {
        eprintln!("notice: {notice}");
    }
}

fn print_space_report(action: &str, report: &SpaceReport) {
    println!(
        "{action} {}/{} ({})",
        report.repo.namespace, report.repo.dataset, report.repo.endpoint
    );
    println!(
        "Catalog root: {} ({}, {} nodes, {})",
        report.catalog.tree.name,
        report.catalog.tree.id,
        count_nodes(&report.catalog.tree),
        format_bytes(report.catalog.bytes)
    );
    print_catalog_warnings(&report.catalog);
}

fn print_catalog_warnings(snapshot: &CatalogSnapshot) {
    for warning in &snapshot.warnings {
        eprintln!("warning: {warning}");
    }
}

fn print_items(items: &[DriveItem]) {
    if items.is_empty() {
        println!("(empty)");
        return;
    }
    for item in items {
        let kind = match item.kind {
            DriveItemKind::Directory => "dir ",
            DriveItemKind::File => "file",
        };
        println!(
            "{kind}  {:>10}  {}  {}",
            format_bytes(item.size),
            item.id,
            item.name
        );
    }
}

fn print_tasks(tasks: &[TaskSummary]) {
    if tasks.is_empty() {
        println!("No durable tasks");
        return;
    }
    for task in tasks {
        let detail = match task.state {
            TaskState::Failed => task.error.as_deref().unwrap_or("failed"),
            _ => task.phase.as_deref().unwrap_or(""),
        };
        println!(
            "{}  {:?}  {}  {}/{}  {}/{}  {}",
            task.id,
            task.state,
            task.label,
            task.progress_done,
            task.progress_total,
            format_bytes(task.bytes_done),
            format_bytes(task.bytes_total),
            detail
        );
    }
}

fn confirm_delete(node_ids: &[String]) -> CliResult<bool> {
    println!(
        "The following {} catalog node(s) will be deleted:",
        node_ids.len()
    );
    for node_id in node_ids {
        println!("  {node_id}");
    }
    print!("Continue? [y/N]: ");
    io::stdout().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    Ok(matches!(
        answer.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

fn count_nodes(node: &CatalogTreeNode) -> usize {
    match &node.kind {
        CatalogTreeNodeKind::Directory { children } => {
            1 + children.iter().map(count_nodes).sum::<usize>()
        }
        CatalogTreeNodeKind::File { .. } => 1,
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: &[(&str, u64)] = &[
        ("GiB", 1024 * 1024 * 1024),
        ("MiB", 1024 * 1024),
        ("KiB", 1024),
    ];
    for (unit, divisor) in UNITS {
        if bytes >= *divisor {
            return format!("{:.1} {unit}", bytes as f64 / *divisor as f64);
        }
    }
    format!("{bytes} B")
}

#[derive(Default)]
struct ProgressPrinter {
    last_printed: Option<Instant>,
    last_phase: Option<String>,
}

impl ProgressPrinter {
    fn update(&mut self, progress: ForegroundProgress) {
        let now = Instant::now();
        let phase_changed = self.last_phase.as_deref() != Some(progress.phase.as_str());
        let terminal_count = progress.total > 0 && progress.completed >= progress.total;
        let terminal_bytes =
            progress.bytes_total > 0 && progress.bytes_done >= progress.bytes_total;
        let interval_elapsed = self
            .last_printed
            .is_none_or(|last| now.duration_since(last) >= Duration::from_millis(500));
        if phase_changed || terminal_count || terminal_bytes || interval_elapsed {
            eprintln!(
                "{}  {}  {}/{}  {}/{}",
                progress.task_id,
                progress.phase,
                progress.completed,
                progress.total,
                format_bytes(progress.bytes_done),
                format_bytes(progress.bytes_total)
            );
            self.last_printed = Some(now);
            self.last_phase = Some(progress.phase);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{format_bytes, ProgressPrinter};
    use lios_application::task_runner::ForegroundProgress;
    use uuid::Uuid;

    #[test]
    fn byte_counts_are_human_readable() {
        assert_eq!(format_bytes(9), "9 B");
        assert_eq!(format_bytes(1536), "1.5 KiB");
    }

    #[test]
    fn progress_printer_accepts_repeated_updates() {
        let mut printer = ProgressPrinter::default();
        let update = ForegroundProgress {
            task_id: Uuid::nil(),
            phase: "uploading".to_string(),
            completed: 1,
            total: 2,
            bytes_done: 1,
            bytes_total: 2,
        };
        printer.update(update.clone());
        printer.update(update);
    }
}
