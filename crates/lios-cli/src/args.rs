use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use lios_core::config::MODELSCOPE_ENDPOINT;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(
    name = "lios",
    version,
    about = "Encrypted rsync-style transfers for Lios Spaces",
    long_about = None
)]
pub struct Cli {
    /// Use DIR/.lios instead of the current user's default state directory.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,

    /// Emit one stable JSON document and disable interaction and progress.
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    Setup,
    Status,
    Auth(AuthArgs),
    Key(KeyArgs),
    Space(SpaceArgs),
    Ls(LsArgs),
    Search(SearchArgs),
    Mkdir(MkdirArgs),
    Cp(CpArgs),
    Sync(SyncArgs),
    Mv(MvArgs),
    Rm(RmArgs),
    Verify(VerifyArgs),
    Task(TaskArgs),
    Worker(WorkerArgs),
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Self::Setup => "setup",
            Self::Status => "status",
            Self::Auth(_) => "auth",
            Self::Key(_) => "key",
            Self::Space(_) => "space",
            Self::Ls(_) => "ls",
            Self::Search(_) => "search",
            Self::Mkdir(_) => "mkdir",
            Self::Cp(_) => "cp",
            Self::Sync(_) => "sync",
            Self::Mv(_) => "mv",
            Self::Rm(_) => "rm",
            Self::Verify(_) => "verify",
            Self::Task(_) => "task",
            Self::Worker(_) => "worker",
        }
    }
}

#[derive(Debug, Args)]
pub struct AuthArgs {
    #[command(subcommand)]
    pub command: AuthCommand,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    Login {
        #[arg(long)]
        token_stdin: bool,
    },
    Status,
    Logout,
}

#[derive(Debug, Args)]
pub struct KeyArgs {
    #[command(subcommand)]
    pub command: KeyCommand,
}

#[derive(Debug, Subcommand)]
pub enum KeyCommand {
    Status,
    Backup { destination: PathBuf },
    Verify { path: PathBuf },
    Import { path: PathBuf },
}

#[derive(Debug, Args)]
pub struct SpaceArgs {
    #[command(subcommand)]
    pub command: SpaceCommand,
}

#[derive(Debug, Subcommand)]
pub enum SpaceCommand {
    Create {
        name: String,
        #[arg(long)]
        namespace: Option<String>,
        #[arg(long)]
        dataset: Option<String>,
        #[arg(long, default_value = MODELSCOPE_ENDPOINT)]
        endpoint: String,
    },
    Init {
        name: String,
        repository: String,
        #[arg(long, default_value = MODELSCOPE_ENDPOINT)]
        endpoint: String,
    },
    Add {
        name: String,
        repository: String,
        #[arg(long, default_value = MODELSCOPE_ENDPOINT)]
        endpoint: String,
    },
    Discover {
        #[arg(long, default_value = MODELSCOPE_ENDPOINT)]
        endpoint: String,
    },
    List,
    Show {
        name: String,
        #[arg(long)]
        remote: bool,
    },
    Rename {
        old: String,
        new: String,
    },
    Remove {
        name: String,
        #[arg(long)]
        force: bool,
    },
}

#[derive(Debug, Args)]
pub struct LsArgs {
    pub space_path: String,
    #[arg(long)]
    pub long: bool,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    pub space_path: String,
    pub query: String,
}

#[derive(Debug, Args)]
pub struct MkdirArgs {
    #[arg(required = true)]
    pub space_paths: Vec<String>,
    #[arg(long)]
    pub parents: bool,
}

#[derive(Debug, Args)]
pub struct CpArgs {
    #[arg(required = true, num_args = 2..)]
    pub operands: Vec<String>,
    #[arg(long)]
    pub no_clobber: bool,
    #[arg(long)]
    pub interactive: bool,
    #[arg(long)]
    pub replace_type: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    pub progress: bool,
}

impl CpArgs {
    pub fn sources_and_destination(&self) -> (&[String], &String) {
        let (sources, destination) = self.operands.split_at(self.operands.len() - 1);
        (sources, &destination[0])
    }
}

#[derive(Debug, Args)]
pub struct SyncArgs {
    pub source: String,
    pub destination: String,
    #[arg(long)]
    pub delete: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub exclude: Vec<String>,
    #[arg(long, value_name = "FILE")]
    pub exclude_from: Option<PathBuf>,
    #[arg(long)]
    pub replace_type: bool,
    #[arg(long)]
    pub dry_run: bool,
    #[arg(long)]
    pub detach: bool,
    #[arg(long)]
    pub progress: bool,
}

#[derive(Debug, Args)]
pub struct MvArgs {
    pub source: String,
    pub destination: String,
    #[arg(long)]
    pub replace: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub replace_type: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct RmArgs {
    #[arg(required = true)]
    pub space_paths: Vec<String>,
    #[arg(long, required = true)]
    pub recursive: bool,
    #[arg(long)]
    pub yes: bool,
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    pub space: String,
    #[arg(long)]
    pub full: bool,
}

#[derive(Debug, Args)]
pub struct TaskArgs {
    #[command(subcommand)]
    pub command: TaskCommand,
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    List,
    Show { id: Uuid },
    Wait { id: Uuid },
    Pause { id: Uuid },
    Resume { id: Uuid },
    Retry { id: Uuid },
    Cancel { id: Uuid },
    Clear(TaskClearArgs),
}

#[derive(Debug, Args)]
pub struct TaskClearArgs {
    pub id: Option<Uuid>,
    #[arg(long)]
    pub completed: bool,
    #[arg(long)]
    pub failed: bool,
    #[arg(long)]
    pub all_terminal: bool,
}

#[derive(Debug, Args)]
pub struct WorkerArgs {
    #[command(subcommand)]
    pub command: WorkerCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkerCommand {
    Status,
    Stop,
}

pub fn parse_repository_address(
    repository: &str,
    endpoint: String,
) -> Result<lios_core::config::RepoConfig, lios_application::CommandError> {
    let (namespace, dataset) = repository.split_once('/').ok_or_else(|| {
        lios_application::CommandError::invalid_input("repository must be written as OWNER/DATASET")
    })?;
    if dataset.contains('/') {
        return Err(lios_application::CommandError::invalid_input(
            "repository must be written as OWNER/DATASET",
        ));
    }
    lios_application::production_config::validate_repo(lios_core::config::RepoConfig {
        namespace: namespace.to_string(),
        dataset: dataset.to_string(),
        endpoint,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{AuthCommand, Cli, Command, SpaceCommand, TaskCommand};

    #[test]
    fn public_cli_contract_parses() {
        let cases = [
            vec!["lios", "setup"],
            vec!["lios", "--json", "status"],
            vec!["lios", "auth", "login", "--token-stdin"],
            vec!["lios", "key", "backup", "recovery.key"],
            vec!["lios", "space", "add", "photos", "allen/photos"],
            vec!["lios", "space", "list"],
            vec!["lios", "ls", "photos:/docs", "--long"],
            vec!["lios", "search", "photos:/docs", "paper"],
            vec!["lios", "mkdir", "photos:/docs", "--parents"],
            vec!["lios", "cp", "local/", "photos:/backup", "--dry-run"],
            vec!["lios", "sync", "photos:/backup/", ".", "--delete", "--yes"],
            vec!["lios", "mv", "photos:/old", "photos:/new"],
            vec!["lios", "rm", "photos:/old", "--recursive", "--yes"],
            vec!["lios", "verify", "photos:", "--full"],
            vec![
                "lios",
                "task",
                "wait",
                "00000000-0000-0000-0000-000000000001",
            ],
            vec!["lios", "worker", "status"],
        ];
        for case in cases {
            Cli::try_parse_from(&case).unwrap_or_else(|error| panic!("{case:?}: {error}"));
        }
    }

    #[test]
    fn nested_commands_have_stable_shapes() {
        let auth = Cli::try_parse_from(["lios", "auth", "status"]).unwrap();
        assert!(matches!(
            auth.command,
            Command::Auth(super::AuthArgs {
                command: AuthCommand::Status
            })
        ));

        let space = Cli::try_parse_from(["lios", "space", "list"]).unwrap();
        assert!(matches!(
            space.command,
            Command::Space(super::SpaceArgs {
                command: SpaceCommand::List
            })
        ));

        let task = Cli::try_parse_from([
            "lios",
            "task",
            "retry",
            "00000000-0000-0000-0000-000000000001",
        ])
        .unwrap();
        assert!(matches!(
            task.command,
            Command::Task(super::TaskArgs {
                command: TaskCommand::Retry { .. }
            })
        ));
    }

    #[test]
    fn removed_legacy_commands_are_rejected() {
        for command in ["upload", "download", "delete", "rename", "repos"] {
            assert!(Cli::try_parse_from(["lios", command]).is_err());
        }
        assert!(Cli::try_parse_from(["lios", "space", "open"]).is_err());
    }
}
