use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Running,
    Paused,
    Failed,
    Completed,
    Canceled,
}

impl TaskState {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Queued => "Queued",
            Self::Running => "Running",
            Self::Paused => "Paused",
            Self::Failed => "Failed",
            Self::Completed => "Completed",
            Self::Canceled => "Canceled",
        }
    }

    fn from_str(value: &str) -> Self {
        match value {
            "Running" => Self::Running,
            "Paused" => Self::Paused,
            "Failed" => Self::Failed,
            "Completed" => Self::Completed,
            "Canceled" => Self::Canceled,
            _ => Self::Queued,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRecord {
    pub id: Uuid,
    pub state: TaskState,
    pub label: String,
    pub phase: Option<String>,
    pub progress_total: u64,
    pub progress_done: u64,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub speed_bps: u64,
    pub error: Option<String>,
}

impl TaskRecord {
    pub fn queued(label: impl Into<String>, progress_total: u64) -> Self {
        Self {
            id: Uuid::new_v4(),
            state: TaskState::Queued,
            label: label.into(),
            phase: None,
            progress_total,
            progress_done: 0,
            bytes_total: 0,
            bytes_done: 0,
            speed_bps: 0,
            error: None,
        }
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
        let connection = rusqlite::Connection::open(path)?;
        connection.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS tasks (
                id TEXT PRIMARY KEY NOT NULL,
                state TEXT NOT NULL,
                label TEXT NOT NULL,
                phase TEXT,
                progress_total INTEGER NOT NULL,
                progress_done INTEGER NOT NULL,
                bytes_total INTEGER NOT NULL DEFAULT 0,
                bytes_done INTEGER NOT NULL DEFAULT 0,
                speed_bps INTEGER NOT NULL DEFAULT 0,
                error TEXT
            );
            "#,
        )?;
        let columns = {
            let mut statement = connection.prepare("PRAGMA table_info(tasks)")?;
            let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
            let mut found = Vec::new();
            for column in columns {
                found.push(column?);
            }
            found
        };
        if !columns.iter().any(|column| column == "phase") {
            connection.execute("ALTER TABLE tasks ADD COLUMN phase TEXT", [])?;
        }
        if !columns.iter().any(|column| column == "bytes_total") {
            connection.execute(
                "ALTER TABLE tasks ADD COLUMN bytes_total INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "bytes_done") {
            connection.execute(
                "ALTER TABLE tasks ADD COLUMN bytes_done INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        if !columns.iter().any(|column| column == "speed_bps") {
            connection.execute(
                "ALTER TABLE tasks ADD COLUMN speed_bps INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
        Ok(Self { connection })
    }

    pub fn insert(&self, task: &TaskRecord) -> Result<()> {
        self.connection.execute(
            r#"
            INSERT OR REPLACE INTO tasks
                (id, state, label, phase, progress_total, progress_done, bytes_total, bytes_done, speed_bps, error)
            VALUES
                (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            "#,
            rusqlite::params![
                task.id.to_string(),
                task.state.as_str(),
                &task.label,
                &task.phase,
                task.progress_total as i64,
                task.progress_done as i64,
                task.bytes_total as i64,
                task.bytes_done as i64,
                task.speed_bps as i64,
                &task.error,
            ],
        )?;
        Ok(())
    }

    pub fn update_phase(&self, id: Uuid, phase: Option<String>) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET phase = ?2 WHERE id = ?1",
            rusqlite::params![id.to_string(), phase],
        )?;
        Ok(())
    }

    pub fn mark_running_interrupted(&self, message: &str) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET state = ?1, phase = NULL, error = ?2 WHERE state = ?3",
            rusqlite::params![
                TaskState::Failed.as_str(),
                message,
                TaskState::Running.as_str()
            ],
        )?;
        Ok(())
    }

    pub fn update_progress(&self, id: Uuid, done: u64, total: u64) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET progress_done = ?2, progress_total = ?3 WHERE id = ?1",
            rusqlite::params![id.to_string(), done as i64, total as i64],
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
                speed_bps = ?6
            WHERE id = ?1
            "#,
            rusqlite::params![
                id.to_string(),
                done as i64,
                total as i64,
                bytes_done as i64,
                bytes_total as i64,
                speed_bps as i64
            ],
        )?;
        Ok(())
    }

    pub fn update_state(&self, id: Uuid, state: TaskState, error: Option<String>) -> Result<()> {
        self.connection.execute(
            "UPDATE tasks SET state = ?2, error = ?3 WHERE id = ?1",
            rusqlite::params![id.to_string(), state.as_str(), error],
        )?;
        Ok(())
    }

    pub fn delete(&self, id: Uuid) -> Result<()> {
        self.connection.execute(
            "DELETE FROM tasks WHERE id = ?1",
            rusqlite::params![id.to_string()],
        )?;
        Ok(())
    }

    pub fn list(&self) -> Result<Vec<TaskRecord>> {
        let mut statement = self.connection.prepare(
            r#"
            SELECT id, state, label, phase, progress_total, progress_done, bytes_total, bytes_done, speed_bps, error
            FROM tasks
            ORDER BY rowid ASC
            "#,
        )?;
        let rows = statement.query_map([], |row| {
            let id: String = row.get(0)?;
            let state: String = row.get(1)?;
            Ok((
                id,
                state,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, Option<String>>(9)?,
            ))
        })?;

        let mut tasks = Vec::new();
        for row in rows {
            let (
                id,
                state,
                label,
                phase,
                progress_total,
                progress_done,
                bytes_total,
                bytes_done,
                speed_bps,
                error,
            ) = row?;
            tasks.push(TaskRecord {
                id: Uuid::parse_str(&id)?,
                state: TaskState::from_str(&state),
                label,
                phase,
                progress_total: progress_total.max(0) as u64,
                progress_done: progress_done.max(0) as u64,
                bytes_total: bytes_total.max(0) as u64,
                bytes_done: bytes_done.max(0) as u64,
                speed_bps: speed_bps.max(0) as u64,
                error,
            });
        }
        Ok(tasks)
    }
}
