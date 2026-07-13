use std::path::PathBuf;

use lios_core::config::LiosPaths;
use lios_core::tasks::{TaskItem, TaskItemState, TaskStore, TaskSummary};
use serde::Serialize;
use tauri::Emitter;
use uuid::Uuid;

use crate::command_error::{sanitize_persisted_message, CommandError};

type CommandResult<T> = std::result::Result<T, CommandError>;

#[derive(Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum TaskUpdateEvent {
    Upsert { task: Box<TaskSummary> },
    Remove { task_ids: Vec<Uuid> },
}

#[derive(Debug, Serialize)]
pub(crate) struct TaskItemDto {
    id: Uuid,
    task_id: Uuid,
    name: String,
    relative_path: Option<PathBuf>,
    size: u64,
    state: TaskItemState,
    phase: Option<String>,
    bytes_done: u64,
    bytes_total: u64,
    error: Option<String>,
}

impl From<TaskItem> for TaskItemDto {
    fn from(item: TaskItem) -> Self {
        Self {
            id: item.id,
            task_id: item.task_id,
            name: item.name,
            relative_path: item.relative_path,
            size: item.size,
            state: item.state,
            phase: item.phase,
            bytes_done: item.bytes_done,
            bytes_total: item.bytes_total,
            error: item.error.map(sanitize_persisted_message),
        }
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct TaskItemsPageDto {
    pub(crate) task_id: Uuid,
    pub(crate) offset: u64,
    pub(crate) total: u64,
    pub(crate) items: Vec<TaskItemDto>,
}

pub(crate) fn webview_safe_task_summary(mut task: TaskSummary) -> TaskSummary {
    task.error = task.error.map(sanitize_persisted_message);
    task
}

pub(crate) fn webview_safe_task_summaries(tasks: Vec<TaskSummary>) -> Vec<TaskSummary> {
    tasks.into_iter().map(webview_safe_task_summary).collect()
}

pub(crate) fn task_summaries_for_paths(paths: &LiosPaths) -> CommandResult<Vec<TaskSummary>> {
    TaskStore::open(&paths.database)
        .map_err(CommandError::from)?
        .list_summaries()
        .map(webview_safe_task_summaries)
        .map_err(CommandError::from)
}

pub(crate) fn task_summary_for_paths(
    paths: &LiosPaths,
    task_id: Uuid,
) -> CommandResult<Option<TaskSummary>> {
    TaskStore::open(&paths.database)
        .map_err(CommandError::from)?
        .get_summary(task_id)
        .map(|task| task.map(webview_safe_task_summary))
        .map_err(CommandError::from)
}

pub(crate) fn emit_task(app: &tauri::AppHandle, paths: &LiosPaths, task_id: Uuid) {
    if let Ok(Some(task)) = task_summary_for_paths(paths, task_id) {
        let _ = app.emit(
            "lios-task-updated",
            TaskUpdateEvent::Upsert {
                task: Box::new(task),
            },
        );
    }
}

pub(crate) fn emit_removed_tasks(app: &tauri::AppHandle, task_ids: Vec<Uuid>) {
    if !task_ids.is_empty() {
        let _ = app.emit("lios-task-updated", TaskUpdateEvent::Remove { task_ids });
    }
}

pub(crate) fn list_task_items_for_paths(
    paths: &LiosPaths,
    task_id: Uuid,
    offset: u64,
    limit: u64,
) -> CommandResult<TaskItemsPageDto> {
    const JAVASCRIPT_MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;

    if !(1..=200).contains(&limit) {
        return Err(CommandError::invalid_input(
            "task item page limit must be between 1 and 200",
        ));
    }
    if offset > JAVASCRIPT_MAX_SAFE_INTEGER || i64::try_from(offset).is_err() {
        return Err(CommandError::invalid_input(
            "task item page offset exceeds the supported range",
        ));
    }
    let store = TaskStore::open(&paths.database).map_err(CommandError::from)?;
    let page = store
        .get_items_page(task_id, offset, limit)
        .map_err(CommandError::from)?
        .ok_or_else(|| CommandError::invalid_input("task was not found"))?;
    Ok(TaskItemsPageDto {
        task_id,
        offset,
        total: page.total,
        items: page.items.into_iter().map(TaskItemDto::from).collect(),
    })
}
