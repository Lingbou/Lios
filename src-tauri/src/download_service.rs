use lios_core::catalog::CatalogSelection;
use lios_core::tasks::TaskRecord;
use std::path::PathBuf;

use crate::command_error::CommandError;

#[derive(Debug)]
pub struct PreparedDownload {
    pub task: TaskRecord,
    pub selection: CatalogSelection,
    pub output_dir: PathBuf,
}

pub fn prepare_download_task(
    node_ids: Vec<String>,
    output_dir: String,
) -> Result<PreparedDownload, CommandError> {
    let ids = node_ids
        .into_iter()
        .map(|id| id.trim().to_string())
        .filter(|id| !id.is_empty())
        .collect::<Vec<_>>();
    if ids.is_empty() {
        return Err(CommandError::invalid_input(
            "download selection cannot be empty",
        ));
    }
    let output_dir = PathBuf::from(output_dir.trim());
    if !output_dir.is_absolute() || !output_dir.is_dir() {
        return Err(CommandError::invalid_input(
            "download output must be an existing absolute directory",
        ));
    }
    let output_dir = output_dir.canonicalize().map_err(|_| {
        CommandError::invalid_input("download output must be an existing absolute directory")
    })?;

    let task = TaskRecord::queued("download", 1);
    Ok(PreparedDownload {
        task,
        selection: CatalogSelection::Nodes(ids),
        output_dir,
    })
}

#[cfg(test)]
mod tests {
    use lios_core::catalog::CatalogSelection;
    use lios_core::config::LiosPaths;
    use lios_core::tasks::TaskStore;
    use tempfile::tempdir;

    use super::prepare_download_task;
    use crate::command_error::CommandErrorCode;

    #[test]
    fn empty_selection_is_rejected_before_task_insertion() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());

        let error = prepare_download_task(
            vec!["".to_string(), "   ".to_string()],
            temp.path().display().to_string(),
        )
        .unwrap_err();

        assert_eq!(error.code, CommandErrorCode::InvalidInput);
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .list()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn download_preparation_is_node_scoped_without_inserting_task() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());

        let prepared = prepare_download_task(
            vec![" node-a ".to_string(), "".to_string(), "node-b".to_string()],
            temp.path().display().to_string(),
        )
        .unwrap();

        let CatalogSelection::Nodes(ids) = prepared.selection else {
            panic!("download selection must remain node-scoped");
        };
        assert_eq!(ids, ["node-a", "node-b"]);
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .list()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn invalid_output_is_rejected_before_task_insertion() {
        let temp = tempdir().unwrap();
        let paths = LiosPaths::from_home(temp.path());
        let file = temp.path().join("not-a-directory");
        std::fs::write(&file, b"file").unwrap();

        for output in [
            "".to_string(),
            "relative/output".to_string(),
            file.display().to_string(),
        ] {
            let error = prepare_download_task(vec!["node-a".to_string()], output).unwrap_err();
            assert_eq!(error.code, CommandErrorCode::InvalidInput);
        }
        assert!(TaskStore::open(&paths.database)
            .unwrap()
            .list()
            .unwrap()
            .is_empty());
    }

    #[test]
    fn prepared_download_carries_canonical_output_directory() {
        let temp = tempdir().unwrap();
        let output = temp.path().join("output");
        std::fs::create_dir(&output).unwrap();

        let prepared =
            prepare_download_task(vec!["node-a".to_string()], output.display().to_string())
                .unwrap();

        assert_eq!(prepared.output_dir, output.canonicalize().unwrap());
    }
}
