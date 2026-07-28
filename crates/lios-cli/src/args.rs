use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};
use lios_core::config::MODELSCOPE_ENDPOINT;

#[derive(Debug, Parser)]
#[command(
    name = "lios",
    version,
    about = "Use a Lios encrypted ModelScope drive from the terminal",
    long_about = None
)]
pub struct Cli {
    /// Use DIR/.lios instead of the current user's default state directory.
    #[arg(long, global = true, value_name = "DIR")]
    pub home: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize local state and configure ModelScope authentication.
    Setup,
    /// Replace the saved ModelScope token (input is hidden).
    Auth,
    /// Show local configuration and optionally validate remote authentication.
    Status(StatusArgs),
    /// List or create ModelScope dataset repositories.
    Repos(ReposArgs),
    /// Open or initialize an encrypted Lios space.
    Space(SpaceArgs),
    /// List the children of a catalog path.
    Ls(LsArgs),
    /// Search the active encrypted catalog.
    Search(SearchArgs),
    /// Create a directory below a catalog node.
    Mkdir(MkdirArgs),
    /// Rename a catalog node.
    Rename(RenameArgs),
    /// Upload local files or directories and wait for completion.
    Upload(UploadArgs),
    /// Download catalog nodes and wait for completion.
    Download(DownloadArgs),
    /// Delete catalog nodes and wait for completion.
    Delete(DeleteArgs),
    /// Verify the active space and wait for completion.
    Verify(VerifyArgs),
    /// Inspect or resume durable transfer tasks.
    Task(TaskArgs),
}

#[derive(Debug, Args)]
pub struct StatusArgs {
    /// Also contact ModelScope to validate the saved token.
    #[arg(long)]
    pub remote: bool,
}

#[derive(Debug, Args)]
pub struct ReposArgs {
    #[command(subcommand)]
    pub command: ReposCommand,
}

#[derive(Debug, Subcommand)]
pub enum ReposCommand {
    /// List repositories owned by the authenticated ModelScope user.
    List(EndpointArgs),
    /// Create a private ModelScope dataset repository.
    Create(RepoArgs),
}

#[derive(Debug, Args)]
pub struct SpaceArgs {
    #[command(subcommand)]
    pub command: SpaceCommand,
}

#[derive(Debug, Subcommand)]
pub enum SpaceCommand {
    /// Download, decrypt, and select an existing Lios space.
    Open(RepoArgs),
    /// Initialize an empty encrypted catalog in an existing repository.
    Init(RepoArgs),
}

#[derive(Debug, Args)]
pub struct EndpointArgs {
    #[arg(long, default_value = MODELSCOPE_ENDPOINT)]
    pub endpoint: String,
}

#[derive(Debug, Args)]
pub struct RepoArgs {
    #[arg(long)]
    pub namespace: String,

    #[arg(long)]
    pub dataset: String,

    #[arg(long, default_value = MODELSCOPE_ENDPOINT)]
    pub endpoint: String,
}

#[derive(Debug, Args)]
pub struct LsArgs {
    /// Absolute catalog path, starting at /. Defaults to the catalog root.
    #[arg(default_value = "/")]
    pub path: String,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
    pub query: String,
}

#[derive(Debug, Args)]
pub struct MkdirArgs {
    /// Parent catalog node ID (shown by `lios ls`).
    #[arg(long)]
    pub parent: String,

    pub name: String,
}

#[derive(Debug, Args)]
pub struct RenameArgs {
    /// Catalog node ID (shown by `lios ls`).
    pub node: String,

    pub new_name: String,
}

#[derive(Debug, Args)]
pub struct UploadArgs {
    /// Destination directory node ID.
    #[arg(long)]
    pub parent: String,

    /// Existing local files or directories. Relative paths are accepted.
    #[arg(required = true)]
    pub paths: Vec<PathBuf>,
}

#[derive(Debug, Args)]
pub struct DownloadArgs {
    /// Existing local output directory. Relative paths are accepted.
    #[arg(long, value_name = "DIR")]
    pub output: PathBuf,

    /// Catalog node IDs.
    #[arg(required = true)]
    pub nodes: Vec<String>,
}

#[derive(Debug, Args)]
pub struct DeleteArgs {
    /// Catalog node IDs.
    #[arg(required = true)]
    pub nodes: Vec<String>,
}

#[derive(Debug, Args)]
pub struct VerifyArgs {
    /// Download, decrypt, and cryptographically verify every referenced object.
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
    /// List durable tasks from the local SQLite task store.
    List,
    /// Resume or retry a durable task and wait for completion.
    Resume { task_id: uuid::Uuid },
}

impl RepoArgs {
    pub fn into_config(
        self,
    ) -> Result<lios_core::config::RepoConfig, lios_application::CommandError> {
        lios_application::production_config::validate_repo(lios_core::config::RepoConfig {
            namespace: self.namespace,
            dataset: self.dataset,
            endpoint: self.endpoint,
        })
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command, ReposCommand, SpaceCommand, TaskCommand};

    #[test]
    fn representative_manual_command_tree_parses() {
        let cases = [
            vec!["lios", "setup"],
            vec!["lios", "auth"],
            vec!["lios", "status", "--remote"],
            vec!["lios", "repos", "list"],
            vec![
                "lios",
                "space",
                "open",
                "--namespace",
                "novix",
                "--dataset",
                "cold",
            ],
            vec!["lios", "ls", "/docs"],
            vec!["lios", "search", "paper"],
            vec!["lios", "mkdir", "--parent", "root", "docs"],
            vec!["lios", "rename", "node-1", "new-name"],
            vec!["lios", "upload", "--parent", "root", "README.md"],
            vec!["lios", "download", "--output", ".", "node-1"],
            vec!["lios", "delete", "node-1"],
            vec!["lios", "verify", "--full"],
            vec![
                "lios",
                "task",
                "resume",
                "00000000-0000-0000-0000-000000000001",
            ],
        ];
        for case in cases {
            Cli::try_parse_from(&case).unwrap_or_else(|error| panic!("{case:?}: {error}"));
        }
    }

    #[test]
    fn nested_commands_have_the_expected_shape() {
        let repos = Cli::try_parse_from(["lios", "repos", "list"]).unwrap();
        assert!(matches!(
            repos.command,
            Command::Repos(super::ReposArgs {
                command: ReposCommand::List(_)
            })
        ));

        let space = Cli::try_parse_from([
            "lios",
            "space",
            "init",
            "--namespace",
            "novix",
            "--dataset",
            "cold",
        ])
        .unwrap();
        assert!(matches!(
            space.command,
            Command::Space(super::SpaceArgs {
                command: SpaceCommand::Init(_)
            })
        ));

        let task = Cli::try_parse_from(["lios", "task", "list"]).unwrap();
        assert!(matches!(
            task.command,
            Command::Task(super::TaskArgs {
                command: TaskCommand::List
            })
        ));
    }

    #[test]
    fn json_mode_is_not_part_of_the_manual_cli() {
        let removed_flag = ["--", "json"].concat();
        assert!(Cli::try_parse_from(["lios", &removed_flag, "status"]).is_err());
    }
}
