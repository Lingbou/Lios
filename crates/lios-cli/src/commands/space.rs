use crate::args::{parse_repository_address, SpaceCommand};
use crate::error::{CliError, CliResult};
use crate::output::CommandOutput;
use lios_application::service::Application;
use lios_application::space_registry::{validate_space_name, SpaceRegistry};
use lios_core::config::RepoConfig;
use serde_json::json;

pub async fn run_space(
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
                title: None,
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

pub fn registered_space_output(name: String, repo: RepoConfig) -> CliResult<CommandOutput> {
    Ok(
        CommandOutput::new(json!({"name": name, "repository": repo})).human(format!(
            "Registered {name}: as {}/{}",
            repo.namespace, repo.dataset
        )),
    )
}
