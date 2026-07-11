use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::catalog::ConflictResolution;
use crate::config::RepoConfig;
use crate::{LiosError, Result};

const TASK_SCHEMA_VERSION: i64 = 1;
const INVALID_TASK_SPEC_MESSAGE: &str = "persisted task specification is invalid";
const DEFAULT_TASK_CHUNK_SIZE: usize = 128 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Preparing,
    Running,
    Paused,
    Retrying,
    Committing,
    Failed,
    Completed,
    Canceled,
}

impl TaskState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Preparing => "Preparing",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Retrying => "Retrying",
            Self::Committing => "Committing",
            Self::Failed => "Failed",
            Self::Completed => "Completed",
            Self::Canceled => "Canceled",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "Queued" => Ok(Self::Queued),
            "Preparing" => Ok(Self::Preparing),
            "Running" => Ok(Self::Running),
            "Paused" => Ok(Self::Paused),
            "Retrying" => Ok(Self::Retrying),
            "Committing" => Ok(Self::Committing),
            "Failed" => Ok(Self::Failed),
            "Completed" => Ok(Self::Completed),
            "Canceled" => Ok(Self::Canceled),
            _ => Err(LiosError::DataCorruption(format!(
                "unknown persisted task state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TaskSpec {
    Upload {
        account_id: String,
        space_id: String,
        repo: RepoConfig,
        parent_node_id: String,
        source_paths: Vec<PathBuf>,
        #[serde(default = "default_task_chunk_size")]
        chunk_size: usize,
        conflict_resolutions: Vec<ConflictResolution>,
    },
    Delete {
        account_id: String,
        space_id: String,
        repo: RepoConfig,
        node_ids: Vec<String>,
    },
    Download {
        account_id: String,
        space_id: String,
        repo: RepoConfig,
        node_ids: Vec<String>,
        output_dir: PathBuf,
    },
    VerifySpace {
        account_id: String,
        space_id: String,
        repo: RepoConfig,
        full: bool,
    },
    RebuildCatalog {
        account_id: String,
        space_id: String,
        repo: RepoConfig,
    },
}

impl TaskSpec {
    pub fn account_id(&self) -> &str {
        match self {
            Self::Upload { account_id, .. }
            | Self::Delete { account_id, .. }
            | Self::Download { account_id, .. }
            | Self::VerifySpace { account_id, .. }
            | Self::RebuildCatalog { account_id, .. } => account_id,
        }
    }

    pub fn space_id(&self) -> &str {
        match self {
            Self::Upload { space_id, .. }
            | Self::Delete { space_id, .. }
            | Self::Download { space_id, .. }
            | Self::VerifySpace { space_id, .. }
            | Self::RebuildCatalog { space_id, .. } => space_id,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Upload { .. } => "upload",
            Self::Delete { .. } => "delete",
            Self::Download { .. } => "download",
            Self::VerifySpace { .. } => "verify",
            Self::RebuildCatalog { .. } => "rebuild",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskItemState {
    Queued,
    Running,
    Skipped,
    Failed,
    Completed,
}

impl TaskItemState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Skipped => "Skipped",
            Self::Failed => "Failed",
            Self::Completed => "Completed",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "Queued" => Ok(Self::Queued),
            "Running" => Ok(Self::Running),
            "Skipped" => Ok(Self::Skipped),
            "Failed" => Ok(Self::Failed),
            "Completed" => Ok(Self::Completed),
            _ => Err(LiosError::DataCorruption(format!(
                "unknown persisted task item state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskItem {
    pub id: Uuid,
    pub task_id: Uuid,
    pub name: String,
    pub source_path: Option<PathBuf>,
    pub size: u64,
    pub state: TaskItemState,
    pub phase: Option<String>,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CheckpointState {
    Pending,
    Uploaded,
    Committed,
}

impl CheckpointState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Uploaded => "Uploaded",
            Self::Committed => "Committed",
        }
    }

    fn from_str(value: &str) -> Result<Self> {
        match value {
            "Pending" => Ok(Self::Pending),
            "Uploaded" => Ok(Self::Uploaded),
            "Committed" => Ok(Self::Committed),
            _ => Err(LiosError::DataCorruption(format!(
                "unknown persisted checkpoint state: {value}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskObjectCheckpoint {
    pub task_id: Uuid,
    pub remote_path: String,
    pub oid: String,
    pub size: u64,
    pub state: CheckpointState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileContentIndexEntry {
    pub account_id: String,
    pub space_id: String,
    pub content_sha256: String,
    pub object_id: String,
    pub size: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TaskRecoveryReport {
    pub requeued: usize,
    pub failed_unrecoverable: usize,
    pub failed_invalid_spec: usize,
    pub needs_reconciliation: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: Uuid,
    pub account_id: String,
    pub space_id: String,
    pub state: TaskState,
    pub label: String,
    pub phase: Option<String>,
    pub progress_total: u64,
    pub progress_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub speed_bps: u64,
    pub eta_seconds: Option<u64>,
    pub attempt: u32,
    pub created_at: String,
    pub updated_at: String,
    pub error: Option<String>,
    pub items: Vec<TaskItem>,
}

impl TaskRecord {
    pub fn queued(label: impl Into<String>, progress_total: u64) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            id: Uuid::new_v4(),
            account_id: String::new(),
            space_id: String::new(),
            state: TaskState::Queued,
            label: label.into(),
            phase: None,
            progress_total,
            progress_done: 0,
            bytes_total: 0,
            bytes_done: 0,
            speed_bps: 0,
            eta_seconds: None,
            attempt: 0,
            created_at: now.clone(),
            updated_at: now,
            error: None,
            items: Vec::new(),
        }
    }

    pub fn queued_for_spec(spec: &TaskSpec) -> Self {
        let mut task = Self::queued(spec.label(), 0);
        task.account_id = spec.account_id().to_string();
        task.space_id = spec.space_id().to_string();
        task
    }
}

pub struct TaskStore {
    connection: rusqlite::Connection,
}

impl TaskStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        if let Some(parent) = path.as_ref().parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut connection = rusqlite::Connection::open(path)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;
            "#,
        )?;
        enable_wal_with_retry(&connection)?;
        connection.execute_batch("PRAGMA synchronous = NORMAL;")?;
        migrate_task_store(&mut connection)?;
        Ok(Self { connection })
    }

    pub fn insert(&self, task: &TaskRecord) -> Result<()> {
        self.upsert_task(task, None)
    }

    fn upsert_task(&self, task: &TaskRecord, spec_json: Option<&str>) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT INTO tasks
                (id, account_id, space_id, state, label, phase, progress_total, progress_done,
                 bytes_total, bytes_done, speed_bps, eta_seconds, attempt, spec_json,
                 created_at, updated_at, error)
            VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
            ON CONFLICT(id) DO UPDATE SET
                account_id = excluded.account_id,
                space_id = excluded.space_id,
                state = excluded.state,
                label = excluded.label,
                phase = excluded.phase,
                progress_total = excluded.progress_total,
                progress_done = excluded.progress_done,
                bytes_total = excluded.bytes_total,
                bytes_done = excluded.bytes_done,
                speed_bps = excluded.speed_bps,
                eta_seconds = excluded.eta_seconds,
                attempt = excluded.attempt,
                spec_json = COALESCE(excluded.spec_json, tasks.spec_json),
                updated_at = excluded.updated_at,
                error = excluded.error
            "#,
            rusqlite::params![
                task.id.to_string(),
                &task.account_id,
                &task.space_id,
                task.state.as_str(),
                &task.label,
                &task.phase,
                sqlite_integer(task.progress_total, "task progress total")?,
                sqlite_integer(task.progress_done, "task progress completed")?,
                sqlite_integer(task.bytes_total, "task byte total")?,
                sqlite_integer(task.bytes_done, "task completed bytes")?,
                sqlite_integer(task.speed_bps, "task speed")?,
                task.eta_seconds
                    .map(|value| sqlite_integer(value, "task ETA"))
                    .transpose()?,
                task.attempt as i64,
                spec_json,
                &task.created_at,
                &task.updated_at,
                &task.error,
            ],
        )?;
        Ok(())
    }

    pub fn update_phase(&self, id: Uuid, phase: Option<String>) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET phase = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![id.to_string(), phase, now_timestamp()],
        )?;
        Ok(())
    }

    pub fn mark_running_interrupted(&self, message: &str) -> Result<()> {
        self.connection.execute(
            r#"
            UPDATE tasks
            SET state = ?1, phase = NULL, error = ?2, updated_at = ?3
            WHERE state IN (?4, ?5, ?6, ?7)
            "#,
            rusqlite::params![
                TaskState::Failed.as_str(),
                message,
                now_timestamp(),
                TaskState::Preparing.as_str(),
                TaskState::Running.as_str(),
                TaskState::Retrying.as_str(),
                TaskState::Committing.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn update_progress(&self, id: Uuid, done: u64, total: u64) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET progress_done = ?2, progress_total = ?3, updated_at = ?4 WHERE id = ?1",
            rusqlite::params![
                id.to_string(),
                sqlite_integer(done, "task progress completed")?,
                sqlite_integer(total, "task progress total")?,
                now_timestamp()
            ],
        )?;
        Ok(())
    }

    pub fn update_transfer(
        &self,
        id: Uuid,
        done: u64,
        total: u64,
        bytes_done: u64,
        bytes_total: u64,
        speed_bps: u64,
    ) -> Result<()> {
        self.connection.execute(
            r#"
            UPDATE tasks
            SET progress_done = ?2,
                progress_total = ?3,
                bytes_done = ?4,
                bytes_total = ?5,
                speed_bps = ?6,
                updated_at = ?7
            WHERE id = ?1
            "#,
            rusqlite::params![
                id.to_string(),
                sqlite_integer(done, "task progress completed")?,
                sqlite_integer(total, "task progress total")?,
                sqlite_integer(bytes_done, "task completed bytes")?,
                sqlite_integer(bytes_total, "task byte total")?,
                sqlite_integer(speed_bps, "task speed")?,
                now_timestamp(),
            ],
        )?;
        Ok(())
    }

    pub fn update_state(&self, id: Uuid, state: TaskState, error: Option<String>) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET state = ?2, error = ?3, updated_at = ?4 WHERE id = ?1",
            rusqlite::params![id.to_string(), state.as_str(), error, now_timestamp()],
        )?;
        Ok(())
    }

    pub fn update_eta(&self, id: Uuid, eta_seconds: Option<u64>) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET eta_seconds = ?2, updated_at = ?3 WHERE id = ?1",
            rusqlite::params![
                id.to_string(),
                eta_seconds
                    .map(|value| sqlite_integer(value, "task ETA"))
                    .transpose()?,
                now_timestamp(),
            ],
        )?;
        Ok(())
    }

    pub fn insert_with_spec(&self, task: &TaskRecord, spec: &TaskSpec) -> Result<()> {
        let spec_json = serde_json::to_string(spec)?;
        self.upsert_task(task, Some(&spec_json))
    }

    pub fn load_spec(&self, id: Uuid) -> Result<Option<TaskSpec>> {
        let spec = self
            .connection
            .query_row(
                "SELECT spec_json FROM tasks WHERE id = ?1",
                rusqlite::params![id.to_string()],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;
        spec.flatten()
            .map(|json| serde_json::from_str(&json).map_err(LiosError::from))
            .transpose()
    }

    pub fn list_queued_with_specs(&self) -> Result<Vec<(TaskRecord, TaskSpec)>> {
        let mut queued = Vec::new();
        for task in self
            .list()?
            .into_iter()
            .filter(|task| task.state == TaskState::Queued)
        {
            if let Some(spec) = self.load_spec(task.id)? {
                queued.push((task, spec));
            }
        }
        Ok(queued)
    }

    pub fn claim_queued(&mut self, id: Uuid) -> Result<Option<TaskSpec>> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let row = transaction
            .query_row(
                "SELECT account_id, space_id, state, spec_json FROM tasks WHERE id = ?1",
                rusqlite::params![id.to_string()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .optional()?;
        let Some((account_id, space_id, state, spec_json)) = row else {
            transaction.commit()?;
            return Ok(None);
        };
        if TaskState::from_str(&state)? != TaskState::Queued {
            transaction.commit()?;
            return Ok(None);
        }
        let Some(spec_json) = spec_json else {
            transaction.commit()?;
            return Ok(None);
        };
        let spec: TaskSpec = serde_json::from_str(&spec_json)?;
        if spec.account_id() != account_id || spec.space_id() != space_id {
            return Err(LiosError::DataCorruption(
                "persisted task ownership does not match its specification".to_string(),
            ));
        }
        let changed = transaction.execute(
            r#"
            UPDATE tasks
            SET state = ?2, phase = 'preparing', error = NULL, updated_at = ?3
            WHERE id = ?1 AND state = ?4 AND spec_json IS NOT NULL
            "#,
            rusqlite::params![
                id.to_string(),
                TaskState::Preparing.as_str(),
                now_timestamp(),
                TaskState::Queued.as_str(),
            ],
        )?;
        if changed != 1 {
            return Err(LiosError::DataCorruption(
                "queued task could not be claimed atomically".to_string(),
            ));
        }
        transaction.commit()?;
        Ok(Some(spec))
    }

    pub fn upsert_item(&self, item: &TaskItem) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT INTO task_items
                (id, task_id, name, source_path, size, state, phase, bytes_done, bytes_total, error)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                source_path = excluded.source_path,
                size = excluded.size,
                state = excluded.state,
                phase = excluded.phase,
                bytes_done = excluded.bytes_done,
                bytes_total = excluded.bytes_total,
                error = excluded.error
            "#,
            rusqlite::params![
                item.id.to_string(),
                item.task_id.to_string(),
                &item.name,
                item.source_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                sqlite_integer(item.size, "task item size")?,
                item.state.as_str(),
                &item.phase,
                sqlite_integer(item.bytes_done, "task item completed bytes")?,
                sqlite_integer(item.bytes_total, "task item byte total")?,
                &item.error,
            ],
        )?;
        Ok(())
    }

    pub fn list_items(&self, task_id: Uuid) -> Result<Vec<TaskItem>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT id, name, source_path, size, state, phase, bytes_done, bytes_total, error
            FROM task_items
            WHERE task_id = ?1
            ORDER BY rowid ASC
            "#,
        )?;
        let rows = statement.query_map(rusqlite::params![task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, Option<String>>(8)?,
            ))
        })?;
        let mut items = Vec::new();
        for row in rows {
            let (id, name, source_path, size, state, phase, bytes_done, bytes_total, error) = row?;
            items.push(TaskItem {
                id: Uuid::parse_str(&id)?,
                task_id,
                name,
                source_path: source_path.map(PathBuf::from),
                size: persisted_u64(size, "task item size")?,
                state: TaskItemState::from_str(&state)?,
                phase,
                bytes_done: persisted_u64(bytes_done, "task item completed bytes")?,
                bytes_total: persisted_u64(bytes_total, "task item byte total")?,
                error,
            });
        }
        Ok(items)
    }

    pub fn upsert_checkpoint(&self, checkpoint: &TaskObjectCheckpoint) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT INTO task_object_checkpoints (task_id, remote_path, oid, size, state)
            VALUES (?1, ?2, ?3, ?4, ?5)
            ON CONFLICT(task_id, remote_path) DO UPDATE SET
                oid = excluded.oid,
                size = excluded.size,
                state = excluded.state
            "#,
            rusqlite::params![
                checkpoint.task_id.to_string(),
                &checkpoint.remote_path,
                &checkpoint.oid,
                sqlite_integer(checkpoint.size, "task checkpoint size")?,
                checkpoint.state.as_str(),
            ],
        )?;
        Ok(())
    }

    pub fn list_checkpoints(&self, task_id: Uuid) -> Result<Vec<TaskObjectCheckpoint>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT remote_path, oid, size, state
            FROM task_object_checkpoints
            WHERE task_id = ?1
            ORDER BY remote_path ASC
            "#,
        )?;
        let rows = statement.query_map(rusqlite::params![task_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        let mut checkpoints = Vec::new();
        for row in rows {
            let (remote_path, oid, size, state) = row?;
            checkpoints.push(TaskObjectCheckpoint {
                task_id,
                remote_path,
                oid,
                size: persisted_u64(size, "task checkpoint size")?,
                state: CheckpointState::from_str(&state)?,
            });
        }
        Ok(checkpoints)
    }

    pub fn upsert_content_index(&self, entry: &FileContentIndexEntry) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT INTO file_content_index
                (account_id, space_id, content_sha256, object_id, size, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(account_id, space_id, content_sha256) DO UPDATE SET
                object_id = excluded.object_id,
                size = excluded.size,
                updated_at = excluded.updated_at
            "#,
            rusqlite::params![
                &entry.account_id,
                &entry.space_id,
                &entry.content_sha256,
                &entry.object_id,
                sqlite_integer(entry.size, "file content index size")?,
                &entry.updated_at,
            ],
        )?;
        Ok(())
    }

    pub fn find_content_index(
        &self,
        account_id: &str,
        space_id: &str,
        content_sha256: &str,
    ) -> Result<Option<FileContentIndexEntry>> {
        let row = self
            .connection
            .query_row(
                r#"
                SELECT object_id, size, updated_at
                FROM file_content_index
                WHERE account_id = ?1 AND space_id = ?2 AND content_sha256 = ?3
                "#,
                rusqlite::params![account_id, space_id, content_sha256],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(object_id, size, updated_at)| {
            Ok(FileContentIndexEntry {
                account_id: account_id.to_string(),
                space_id: space_id.to_string(),
                content_sha256: content_sha256.to_string(),
                object_id,
                size: persisted_u64(size, "file content index size")?,
                updated_at,
            })
        })
        .transpose()
    }

    pub fn recover_after_restart(
        &mut self,
        unrecoverable_message: &str,
    ) -> Result<TaskRecoveryReport> {
        let transaction = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let invalid_spec_ids = {
            let mut statement = transaction.prepare(
                r#"
                SELECT id, spec_json
                FROM tasks
                WHERE spec_json IS NOT NULL AND state IN (?1, ?2, ?3, ?4, ?5, ?6)
                "#,
            )?;
            let rows = statement.query_map(
                rusqlite::params![
                    TaskState::Queued.as_str(),
                    TaskState::Preparing.as_str(),
                    TaskState::Running.as_str(),
                    TaskState::Paused.as_str(),
                    TaskState::Retrying.as_str(),
                    TaskState::Committing.as_str(),
                ],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )?;
            let mut invalid = Vec::new();
            for row in rows {
                let (id, spec_json) = row?;
                if serde_json::from_str::<TaskSpec>(&spec_json).is_err() {
                    invalid.push(id);
                }
            }
            invalid
        };
        for id in &invalid_spec_ids {
            transaction.execute(
                r#"
                UPDATE task_items
                SET state = ?1, phase = NULL, error = ?2
                WHERE task_id = ?3 AND state IN (?4, ?5)
                "#,
                rusqlite::params![
                    TaskItemState::Failed.as_str(),
                    INVALID_TASK_SPEC_MESSAGE,
                    id,
                    TaskItemState::Queued.as_str(),
                    TaskItemState::Running.as_str(),
                ],
            )?;
            transaction.execute(
                "UPDATE tasks SET state = ?1, phase = NULL, error = ?2, updated_at = ?3 WHERE id = ?4",
                rusqlite::params![
                    TaskState::Failed.as_str(),
                    INVALID_TASK_SPEC_MESSAGE,
                    now_timestamp(),
                    id,
                ],
            )?;
        }
        transaction.execute(
            r#"
            UPDATE task_items
            SET state = ?1, phase = NULL, error = NULL
            WHERE state = ?2
              AND task_id IN (
                  SELECT id FROM tasks
                  WHERE spec_json IS NOT NULL AND state IN (?3, ?4, ?5)
              )
            "#,
            rusqlite::params![
                TaskItemState::Queued.as_str(),
                TaskItemState::Running.as_str(),
                TaskState::Preparing.as_str(),
                TaskState::Running.as_str(),
                TaskState::Retrying.as_str(),
            ],
        )?;
        let requeued = transaction.execute(
            r#"
            UPDATE tasks
            SET state = ?1,
                phase = NULL,
                error = NULL,
                attempt = attempt + 1,
                updated_at = ?2
            WHERE spec_json IS NOT NULL AND state IN (?3, ?4, ?5)
            "#,
            rusqlite::params![
                TaskState::Queued.as_str(),
                now_timestamp(),
                TaskState::Preparing.as_str(),
                TaskState::Running.as_str(),
                TaskState::Retrying.as_str(),
            ],
        )?;
        transaction.execute(
            r#"
            UPDATE task_items
            SET state = ?1, phase = NULL, error = ?2
            WHERE task_id IN (
                SELECT id FROM tasks
                WHERE spec_json IS NULL AND state IN (?3, ?4, ?5, ?6, ?7, ?8)
            ) AND state IN (?9, ?10)
            "#,
            rusqlite::params![
                TaskItemState::Failed.as_str(),
                unrecoverable_message,
                TaskState::Queued.as_str(),
                TaskState::Preparing.as_str(),
                TaskState::Running.as_str(),
                TaskState::Paused.as_str(),
                TaskState::Retrying.as_str(),
                TaskState::Committing.as_str(),
                TaskItemState::Queued.as_str(),
                TaskItemState::Running.as_str(),
            ],
        )?;
        let failed_unrecoverable = transaction.execute(
            r#"
            UPDATE tasks
            SET state = ?1, phase = NULL, error = ?2, updated_at = ?3
            WHERE spec_json IS NULL AND state IN (?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            rusqlite::params![
                TaskState::Failed.as_str(),
                unrecoverable_message,
                now_timestamp(),
                TaskState::Queued.as_str(),
                TaskState::Preparing.as_str(),
                TaskState::Running.as_str(),
                TaskState::Paused.as_str(),
                TaskState::Retrying.as_str(),
                TaskState::Committing.as_str(),
            ],
        )?;
        let needs_reconciliation = transaction.query_row(
            "SELECT COUNT(*) FROM tasks WHERE spec_json IS NOT NULL AND state = ?1",
            rusqlite::params![TaskState::Committing.as_str()],
            |row| row.get::<_, i64>(0),
        )?;
        let needs_reconciliation = usize::try_from(needs_reconciliation).map_err(|_| {
            LiosError::DataCorruption("persisted committing task count is invalid".to_string())
        })?;
        transaction.commit()?;
        Ok(TaskRecoveryReport {
            requeued,
            failed_unrecoverable,
            failed_invalid_spec: invalid_spec_ids.len(),
            needs_reconciliation,
        })
    }

    pub fn delete(&self, id: Uuid) -> Result<()> {
        self.connection.execute(
            "DELETE FROM tasks WHERE id = ?1",
            rusqlite::params![id.to_string()],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<TaskRecord>> {
        let raw_tasks = {
            let mut statement = self.connection.prepare(
                r#"
                SELECT id, account_id, space_id, state, label, phase, progress_total,
                       progress_done, bytes_total, bytes_done, speed_bps, eta_seconds,
                       attempt, created_at, updated_at, error
                FROM tasks
                ORDER BY rowid ASC
                "#,
            )?;
            let rows = statement.query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, i64>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                    row.get::<_, i64>(12)?,
                    row.get::<_, String>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                ))
            })?;
            let mut raw = Vec::new();
            for row in rows {
                raw.push(row?);
            }
            raw
        };
        let mut tasks = Vec::new();
        for row in raw_tasks {
            let (
                id,
                account_id,
                space_id,
                state,
                label,
                phase,
                progress_total,
                progress_done,
                bytes_total,
                bytes_done,
                speed_bps,
                eta_seconds,
                attempt,
                created_at,
                updated_at,
                error,
            ) = row;
            let id = Uuid::parse_str(&id)?;
            tasks.push(TaskRecord {
                id,
                account_id,
                space_id,
                state: TaskState::from_str(&state)?,
                label,
                phase,
                progress_total: persisted_u64(progress_total, "task progress total")?,
                progress_done: persisted_u64(progress_done, "task progress completed")?,
                bytes_total: persisted_u64(bytes_total, "task byte total")?,
                bytes_done: persisted_u64(bytes_done, "task completed bytes")?,
                speed_bps: persisted_u64(speed_bps, "task speed")?,
                eta_seconds: eta_seconds
                    .map(|value| persisted_u64(value, "task ETA"))
                    .transpose()?,
                attempt: u32::try_from(persisted_u64(attempt, "task attempt")?).map_err(|_| {
                    LiosError::DataCorruption("persisted task attempt exceeds u32".to_string())
                })?,
                created_at,
                updated_at,
                error,
                items: self.list_items(id)?,
            });
        }
        Ok(tasks)
    }

    pub fn get(&self, id: Uuid) -> Result<Option<TaskRecord>> {
        Ok(self.list()?.into_iter().find(|task| task.id == id))
    }
}

fn migrate_task_store(connection: &mut rusqlite::Connection) -> Result<()> {
    let version = connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if version > TASK_SCHEMA_VERSION {
        return Err(LiosError::DataCorruption(format!(
            "task database schema version {version} is newer than supported version {TASK_SCHEMA_VERSION}"
        )));
    }
    if version == TASK_SCHEMA_VERSION {
        return Ok(());
    }

    let transaction =
        connection.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version = transaction.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))?;
    if version > TASK_SCHEMA_VERSION {
        return Err(LiosError::DataCorruption(format!(
            "task database schema version {version} is newer than supported version {TASK_SCHEMA_VERSION}"
        )));
    }
    if version == TASK_SCHEMA_VERSION {
        transaction.commit()?;
        return Ok(());
    }

    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS tasks (
            id TEXT PRIMARY KEY NOT NULL,
            account_id TEXT NOT NULL DEFAULT '',
            space_id TEXT NOT NULL DEFAULT '',
            state TEXT NOT NULL,
            label TEXT NOT NULL,
            phase TEXT,
            progress_total INTEGER NOT NULL,
            progress_done INTEGER NOT NULL,
            bytes_total INTEGER NOT NULL DEFAULT 0,
            bytes_done INTEGER NOT NULL DEFAULT 0,
            speed_bps INTEGER NOT NULL DEFAULT 0,
            eta_seconds INTEGER,
            attempt INTEGER NOT NULL DEFAULT 0,
            spec_json TEXT,
            created_at TEXT NOT NULL DEFAULT '',
            updated_at TEXT NOT NULL DEFAULT '',
            error TEXT
        );
        "#,
    )?;
    ensure_column(&transaction, "phase", "TEXT")?;
    ensure_column(&transaction, "bytes_total", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&transaction, "bytes_done", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&transaction, "speed_bps", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&transaction, "account_id", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&transaction, "space_id", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&transaction, "eta_seconds", "INTEGER")?;
    ensure_column(&transaction, "attempt", "INTEGER NOT NULL DEFAULT 0")?;
    ensure_column(&transaction, "spec_json", "TEXT")?;
    ensure_column(&transaction, "created_at", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&transaction, "updated_at", "TEXT NOT NULL DEFAULT ''")?;
    let now = now_timestamp();
    transaction.execute(
        "UPDATE tasks SET created_at = ?1 WHERE created_at = ''",
        rusqlite::params![&now],
    )?;
    transaction.execute(
        "UPDATE tasks SET updated_at = ?1 WHERE updated_at = ''",
        rusqlite::params![&now],
    )?;
    transaction.execute_batch(
        r#"
        CREATE TABLE IF NOT EXISTS task_items (
            id TEXT PRIMARY KEY NOT NULL,
            task_id TEXT NOT NULL,
            name TEXT NOT NULL,
            source_path TEXT,
            size INTEGER NOT NULL,
            state TEXT NOT NULL,
            phase TEXT,
            bytes_done INTEGER NOT NULL DEFAULT 0,
            bytes_total INTEGER NOT NULL DEFAULT 0,
            error TEXT,
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_task_items_task_id ON task_items(task_id);
        CREATE TABLE IF NOT EXISTS task_object_checkpoints (
            task_id TEXT NOT NULL,
            remote_path TEXT NOT NULL,
            oid TEXT NOT NULL,
            size INTEGER NOT NULL,
            state TEXT NOT NULL,
            PRIMARY KEY(task_id, remote_path),
            FOREIGN KEY(task_id) REFERENCES tasks(id) ON DELETE CASCADE
        );
        CREATE TABLE IF NOT EXISTS file_content_index (
            account_id TEXT NOT NULL,
            space_id TEXT NOT NULL,
            content_sha256 TEXT NOT NULL,
            object_id TEXT NOT NULL,
            size INTEGER NOT NULL,
            updated_at TEXT NOT NULL,
            PRIMARY KEY(account_id, space_id, content_sha256)
        );
        CREATE INDEX IF NOT EXISTS idx_tasks_state ON tasks(state);
        CREATE INDEX IF NOT EXISTS idx_tasks_space_state
            ON tasks(account_id, space_id, state);
        "#,
    )?;
    transaction.pragma_update(None, "user_version", TASK_SCHEMA_VERSION)?;
    transaction.commit()?;
    Ok(())
}

fn enable_wal_with_retry(connection: &rusqlite::Connection) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut retry_delay = Duration::from_millis(5);
    loop {
        match connection.execute_batch("PRAGMA journal_mode = WAL;") {
            Ok(()) => return Ok(()),
            Err(error) if sqlite_is_busy(&error) && Instant::now() < deadline => {
                std::thread::sleep(retry_delay);
                retry_delay = std::cmp::min(retry_delay * 2, Duration::from_millis(100));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn sqlite_is_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(inner, _)
            if matches!(
                inner.code,
                rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked
            )
    )
}

fn ensure_column(
    connection: &rusqlite::Transaction<'_>,
    column: &str,
    definition: &str,
) -> Result<()> {
    let columns = {
        let mut statement = connection.prepare("PRAGMA table_info(tasks)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        let mut columns = Vec::new();
        for row in rows {
            columns.push(row?);
        }
        columns
    };
    if !columns.iter().any(|existing| existing == column) {
        connection.execute(
            &format!("ALTER TABLE tasks ADD COLUMN {column} {definition}"),
            [],
        )?;
    }
    Ok(())
}

fn sqlite_integer(value: u64, field: &str) -> Result<i64> {
    i64::try_from(value).map_err(|_| {
        LiosError::DataCorruption(format!(
            "{field} exceeds the supported SQLite integer range"
        ))
    })
}

fn persisted_u64(value: i64, field: &str) -> Result<u64> {
    u64::try_from(value)
        .map_err(|_| LiosError::DataCorruption(format!("persisted {field} cannot be negative")))
}

fn now_timestamp() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn default_task_chunk_size() -> usize {
    DEFAULT_TASK_CHUNK_SIZE
}
