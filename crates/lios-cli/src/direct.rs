use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use lios_application::service::{Application, CatalogSnapshot, DatasetRepoList, SetupSnapshot};
use lios_application::task_runner::{ForegroundProgress, TaskRunResult};
use lios_core::catalog::{
    CatalogTreeNode, CatalogTreeNodeKind, ConflictAction, ConflictResolution, DriveItem,
    DriveItemKind,
};
use lios_core::config::{LiosPaths, RepoConfig};
use lios_core::tasks::TaskSummary;
use uuid::Uuid;

use crate::error::{CliError, CliResult};

pub struct StatusReport {
    pub setup: SetupSnapshot,
    pub remote_user: Option<String>,
}

pub struct SpaceReport {
    pub repo: RepoConfig,
    pub catalog: CatalogSnapshot,
}

pub struct CliContext {
    application: Application,
}

impl CliContext {
    pub fn new(home: Option<PathBuf>) -> CliResult<Self> {
        let paths = home
            .map(LiosPaths::from_home)
            .unwrap_or_else(LiosPaths::default_user);
        Ok(Self {
            application: Application::new(paths)?,
        })
    }

    pub fn new_for_status(home: Option<PathBuf>) -> CliResult<Self> {
        let paths = home
            .map(LiosPaths::from_home)
            .unwrap_or_else(LiosPaths::default_user);
        Ok(Self {
            application: Application::new_without_initializing(paths)?,
        })
    }

    pub fn setup(&self) -> CliResult<SetupSnapshot> {
        let snapshot = self.application.setup()?;
        if !snapshot.has_token {
            let token = prompt_token("ModelScope token: ")?;
            self.application.set_token(&token)?;
            return Ok(self.application.setup()?);
        }
        Ok(snapshot)
    }

    pub fn auth(&self) -> CliResult<()> {
        let token = prompt_token("New ModelScope token: ")?;
        self.application.set_token(&token)?;
        Ok(())
    }

    pub async fn status(&self, remote: bool) -> CliResult<StatusReport> {
        let setup = self.application.inspect_setup()?;
        let remote_user = if remote {
            Some(
                self.application
                    .list_dataset_repos(None)
                    .await?
                    .user
                    .username,
            )
        } else {
            None
        };
        Ok(StatusReport { setup, remote_user })
    }

    pub async fn list_repositories(&self, endpoint: String) -> CliResult<DatasetRepoList> {
        Ok(self.application.list_dataset_repos(Some(endpoint)).await?)
    }

    pub async fn create_repository(&self, repo: RepoConfig) -> CliResult<()> {
        Ok(self.application.create_dataset_repo(repo).await?)
    }

    pub async fn initialize_space(&self, repo: RepoConfig) -> CliResult<SpaceReport> {
        let catalog = self.application.initialize_space(repo.clone()).await?;
        Ok(SpaceReport { repo, catalog })
    }

    pub async fn open_space(&self, repo: RepoConfig) -> CliResult<SpaceReport> {
        let catalog = self.application.open_space(repo.clone()).await?;
        Ok(SpaceReport { repo, catalog })
    }

    pub async fn list_path(&self, path: &str) -> CliResult<Vec<DriveItem>> {
        let repo = self.application.active_repo()?;
        let snapshot = self.application.open_space(repo).await?;
        list_tree_path(&snapshot.tree, path)
    }

    pub async fn search(&self, query: &str) -> CliResult<Vec<DriveItem>> {
        Ok(self.application.search(query).await?)
    }

    pub async fn create_folder(
        &self,
        parent_node_id: &str,
        name: &str,
    ) -> CliResult<CatalogSnapshot> {
        Ok(self.application.create_folder(parent_node_id, name).await?)
    }

    pub async fn rename_node(&self, node_id: &str, new_name: &str) -> CliResult<CatalogSnapshot> {
        Ok(self.application.rename_node(node_id, new_name).await?)
    }

    pub async fn queue_upload(
        &self,
        parent_node_id: String,
        paths: Vec<PathBuf>,
    ) -> CliResult<TaskSummary> {
        let paths = canonicalize_upload_paths(paths)?;
        let conflicts = self
            .application
            .preview_upload_conflicts(&parent_node_id, &paths)
            .await?;
        let mut resolutions = Vec::with_capacity(conflicts.len());
        for conflict in conflicts {
            let action = prompt_conflict_action(&conflict.target_name, &conflict.source_path)?;
            resolutions.push(ConflictResolution {
                source_path: conflict.source_path,
                action,
            });
        }
        Ok(self
            .application
            .queue_upload(parent_node_id, paths, resolutions)?)
    }

    pub fn queue_download(
        &self,
        node_ids: Vec<String>,
        output_dir: PathBuf,
    ) -> CliResult<TaskSummary> {
        let output_dir = fs::canonicalize(&output_dir).map_err(|error| {
            CliError::invalid_input(format!(
                "download output could not be resolved ({}): {error}",
                output_dir.display()
            ))
        })?;
        Ok(self.application.queue_download(node_ids, output_dir)?)
    }

    pub fn queue_delete(&self, node_ids: Vec<String>) -> CliResult<TaskSummary> {
        Ok(self.application.queue_delete(node_ids)?)
    }

    pub fn queue_verify(&self, full: bool) -> CliResult<TaskSummary> {
        Ok(self.application.queue_verify(full)?)
    }

    pub async fn run_task<F>(&self, task_id: Uuid, on_progress: F) -> CliResult<TaskRunResult>
    where
        F: FnMut(ForegroundProgress),
    {
        Ok(self.application.run_task(task_id, on_progress).await?)
    }

    pub async fn resume_task<F>(&self, task_id: Uuid, on_progress: F) -> CliResult<TaskRunResult>
    where
        F: FnMut(ForegroundProgress),
    {
        Ok(self.application.resume_task(task_id, on_progress).await?)
    }

    pub fn list_tasks(&self) -> CliResult<Vec<TaskSummary>> {
        Ok(self.application.list_tasks()?)
    }
}

fn prompt_token(prompt: &str) -> CliResult<String> {
    let token = rpassword::prompt_password(prompt)?;
    let token = token.trim();
    if token.is_empty() {
        return Err(CliError::invalid_input("ModelScope token cannot be empty"));
    }
    Ok(token.to_string())
}

fn canonicalize_upload_paths(paths: Vec<PathBuf>) -> CliResult<Vec<PathBuf>> {
    paths
        .into_iter()
        .map(|path| {
            fs::canonicalize(&path).map_err(|error| {
                CliError::invalid_input(format!(
                    "upload path could not be resolved ({}): {error}",
                    path.display()
                ))
            })
        })
        .collect()
}

fn prompt_conflict_action(target_name: &str, source_path: &str) -> CliResult<ConflictAction> {
    let stdin = io::stdin();
    let stdout = io::stdout();
    let stderr = io::stderr();
    prompt_conflict_action_with_io(
        target_name,
        source_path,
        &mut stdin.lock(),
        &mut stdout.lock(),
        &mut stderr.lock(),
    )
}

fn prompt_conflict_action_with_io(
    target_name: &str,
    source_path: &str,
    input: &mut impl BufRead,
    output: &mut impl Write,
    error_output: &mut impl Write,
) -> CliResult<ConflictAction> {
    loop {
        write!(
            output,
            "Upload conflict for {target_name} ({source_path}): [k]eep both, [r]eplace, [s]kip (default k): "
        )?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            return Err(CliError::invalid_input(
                "upload conflict input ended before a choice was made",
            ));
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" | "k" | "keep" | "keep-both" => return Ok(ConflictAction::KeepBoth),
            "r" | "replace" => return Ok(ConflictAction::Replace),
            "s" | "skip" => return Ok(ConflictAction::Skip),
            _ => {
                let _ = writeln!(error_output, "Please enter k, r, or s.");
            }
        }
    }
}

fn list_tree_path(root: &CatalogTreeNode, path: &str) -> CliResult<Vec<DriveItem>> {
    let node = resolve_catalog_path(root, path)?;
    let CatalogTreeNodeKind::Directory { children } = &node.kind else {
        return Err(CliError::invalid_input(format!(
            "catalog path is not a directory: {path}"
        )));
    };
    Ok(children.iter().map(tree_node_to_drive_item).collect())
}

fn resolve_catalog_path<'a>(
    root: &'a CatalogTreeNode,
    path: &str,
) -> CliResult<&'a CatalogTreeNode> {
    if !path.starts_with('/') {
        return Err(CliError::invalid_input(
            "catalog paths must be absolute and start with /",
        ));
    }
    let mut segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .peekable();
    if segments
        .peek()
        .is_some_and(|segment| segment.eq_ignore_ascii_case(&root.name))
    {
        segments.next();
    }
    let mut current = root;
    for segment in segments {
        current = match &current.kind {
            CatalogTreeNodeKind::Directory { children } => children
                .iter()
                .find(|child| child.name.eq_ignore_ascii_case(segment))
                .ok_or_else(|| {
                    CliError::invalid_input(format!("catalog path was not found: {path}"))
                })?,
            CatalogTreeNodeKind::File { .. } => {
                return Err(CliError::invalid_input(format!(
                    "catalog path traverses through a file: {path}"
                )))
            }
        };
    }
    Ok(current)
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

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::path::PathBuf;

    use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind, DriveItemKind};

    use super::{
        canonicalize_upload_paths, list_tree_path, prompt_conflict_action_with_io,
        resolve_catalog_path,
    };

    fn tree() -> CatalogTreeNode {
        CatalogTreeNode {
            id: "root".to_string(),
            name: "space".to_string(),
            updated_at: "now".to_string(),
            kind: CatalogTreeNodeKind::Directory {
                children: vec![CatalogTreeNode {
                    id: "docs".to_string(),
                    name: "Docs".to_string(),
                    updated_at: "now".to_string(),
                    kind: CatalogTreeNodeKind::Directory {
                        children: vec![CatalogTreeNode {
                            id: "readme".to_string(),
                            name: "README.md".to_string(),
                            updated_at: "now".to_string(),
                            kind: CatalogTreeNodeKind::File {
                                original_size: 4,
                                sha256: "a".repeat(64),
                                object_id: "object".to_string(),
                                chunk_count: 1,
                            },
                        }],
                    },
                }],
            },
        }
    }

    #[test]
    fn catalog_paths_are_rooted_and_case_insensitive() {
        let tree = tree();
        assert_eq!(resolve_catalog_path(&tree, "/").unwrap().id, "root");
        assert_eq!(resolve_catalog_path(&tree, "/docs").unwrap().id, "docs");
        assert_eq!(
            resolve_catalog_path(&tree, "/SPACE/Docs").unwrap().id,
            "docs"
        );
        assert!(resolve_catalog_path(&tree, "docs").is_err());
        assert!(resolve_catalog_path(&tree, "/missing").is_err());
    }

    #[test]
    fn list_path_converts_tree_children_to_drive_items() {
        let items = list_tree_path(&tree(), "/docs").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].name, "README.md");
        assert_eq!(items[0].size, 4);
        assert_eq!(items[0].kind, DriveItemKind::File);
    }

    #[test]
    fn relative_upload_paths_are_canonicalized() {
        let current = std::env::current_dir().unwrap();
        let result = canonicalize_upload_paths(vec![PathBuf::from("Cargo.toml")]).unwrap();
        assert_eq!(
            result,
            vec![current.join("Cargo.toml").canonicalize().unwrap()]
        );
    }

    #[test]
    fn upload_conflict_distinguishes_eof_from_an_empty_choice() {
        let mut input = Cursor::new(Vec::<u8>::new());
        let mut output = Vec::new();
        let mut error_output = Vec::new();

        let error = prompt_conflict_action_with_io(
            "report.pdf",
            "/tmp/report.pdf",
            &mut input,
            &mut output,
            &mut error_output,
        )
        .unwrap_err();

        assert_eq!(
            error.to_string(),
            "upload conflict input ended before a choice was made"
        );

        let mut input = Cursor::new(b"\n");
        let action = prompt_conflict_action_with_io(
            "report.pdf",
            "/tmp/report.pdf",
            &mut input,
            &mut output,
            &mut error_output,
        )
        .unwrap();

        assert_eq!(action, lios_core::catalog::ConflictAction::KeepBoth);
    }
}
