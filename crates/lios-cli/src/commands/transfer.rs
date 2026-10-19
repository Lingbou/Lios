use crate::args::{CpArgs, MkdirArgs, MvArgs, RmArgs, SyncArgs};
use crate::commands::catalog::*;
use crate::commands::worker::*;
use crate::error::{CliError, CliResult};
use crate::output::CommandOutput;
use lios_application::location::{Location, LocationParser, RemoteLocation};
use lios_application::service::Application;
use lios_application::space_registry::SpaceRegistry;
use lios_application::transfer_planner::PlanOptions;
use lios_application::transfer_request::{
    prepare_pull, prepare_push, PreparedPull, PreparedPush, RemoteSource,
};
use lios_core::catalog::{CatalogTreeNode, CatalogTreeNodeKind, DriveItemKind};
use lios_core::config::RepoConfig;
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::{self, IsTerminal, Write};

pub async fn run_mkdir(
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
pub struct MovePlan {
    source_id: String,
    new_parent_id: String,
    new_name: String,
    replace_id: Option<String>,
    source_kind: DriveItemKind,
    target_kind: Option<DriveItemKind>,
    rendered_destination: String,
}

pub async fn run_mv(
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

pub async fn run_rm(
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

pub fn confirm_removal(paths: &[String], yes: bool) -> CliResult<()> {
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

pub async fn run_cp(
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
    let task = application.queue_transfer(repo, plan)?;
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

pub async fn run_sync(
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
    let task = application.queue_transfer(repo, plan)?;
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

pub enum PreparedCliTransfer {
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

pub async fn prepare_transfer_operands(
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

pub fn plan_output(
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

pub fn apply_interactive_choices(
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

pub fn plan_remote_move(
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

#[cfg(test)]
mod tests {
    use super::*;
    use lios_core::catalog::CatalogTreeNodeKind;

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
}
