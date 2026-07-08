// SQLite database module for task history tracking
//
// Stores task execution results and pytest reports for historical query.
// Tables:
//   - tasks:         Core task execution records
//   - artifacts:     Generated artifact files per task
//   - pytest_results: Structured pytest test results

use rusqlite::{Connection, params, Result as SqlResult};
use std::path::PathBuf;
use uuid::Uuid;
use chrono::Utc;

use crate::core::models::{Task, TaskResult, TaskStatus, TaskType};

/// Database manager for task history
///
/// Holds a persistent SQLite connection. The connection is Send but not Sync,
/// so use from a single thread or wrap in Arc<Mutex<...>> for sharing.
pub struct Database {
    conn: Connection,
    /// Path to the SQLite database file
    pub path: PathBuf,
}

impl Database {
    /// Open or create a database at the given path
    ///
    /// Creates the database file and all required tables if they don't exist.
    pub fn open(path: PathBuf) -> SqlResult<Self> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }

        let conn = Connection::open(&path)?;
        let db = Self { conn, path: path.clone() };
        db.create_tables()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing)
    ///
    /// The database lives only as long as this struct is alive.
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self { conn, path: PathBuf::from(":memory:") };
        db.create_tables()?;
        Ok(db)
    }

    /// Create all required tables if they don't exist
    fn create_tables(&self) -> SqlResult<()> {
        self.conn.execute_batch("
            -- Core task execution records
            CREATE TABLE IF NOT EXISTS tasks (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id         TEXT    NOT NULL UNIQUE,
                created_at      TEXT    NOT NULL,
                command         TEXT    NOT NULL,
                task_type       TEXT    NOT NULL,
                priority        INTEGER NOT NULL DEFAULT 0,
                timeout_secs    INTEGER NOT NULL DEFAULT 300,
                status          TEXT    NOT NULL DEFAULT 'Pending',
                exit_code       INTEGER,
                stdout          TEXT,
                stderr          TEXT,
                error_message   TEXT,
                working_dir     TEXT    NOT NULL DEFAULT '.',
                batch_id        TEXT,
                task_name       TEXT,
                retries_used    INTEGER NOT NULL DEFAULT 0,
                started_at      TEXT,
                completed_at    TEXT,
                duration_ms     INTEGER,
                artifacts_json  TEXT,
                env_vars_json   TEXT,
                metadata_json   TEXT,
                created_at_idx  INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            );

            CREATE INDEX IF NOT EXISTS idx_tasks_status     ON tasks(status);
            CREATE INDEX IF NOT EXISTS idx_tasks_created    ON tasks(created_at_idx);
            CREATE INDEX IF NOT EXISTS idx_tasks_task_type  ON tasks(task_type);
            CREATE INDEX IF NOT EXISTS idx_tasks_batch_id   ON tasks(batch_id);

            -- Artifact file records per task
            CREATE TABLE IF NOT EXISTS artifacts (
                id          INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id     TEXT    NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
                name        TEXT    NOT NULL,
                path        TEXT    NOT NULL,
                size_bytes  INTEGER,
                UNIQUE(task_id, name)
            );

            CREATE INDEX IF NOT EXISTS idx_artifacts_task_id ON artifacts(task_id);

            -- Structured pytest results (parsed from --json-report)
            CREATE TABLE IF NOT EXISTS pytest_results (
                id              INTEGER PRIMARY KEY AUTOINCREMENT,
                task_id         TEXT    NOT NULL UNIQUE REFERENCES tasks(task_id) ON DELETE CASCADE,
                passed          INTEGER NOT NULL DEFAULT 0,
                failed          INTEGER NOT NULL DEFAULT 0,
                skipped         INTEGER NOT NULL DEFAULT 0,
                total           INTEGER NOT NULL DEFAULT 0,
                duration_secs   REAL,
                collected       INTEGER,
                warnings        INTEGER,
                environment     TEXT,
                report_json     TEXT
            );

            CREATE INDEX IF NOT EXISTS idx_pytest_task_id ON pytest_results(task_id);
        ")?;
        Ok(())
    }

    /// Insert a new pending task record
    pub fn insert_task(&self, task: &Task) -> SqlResult<()> {
        self.conn.execute(
            "INSERT INTO tasks (task_id, created_at, command, task_type, priority,
                                timeout_secs, status, working_dir, batch_id, task_name,
                                env_vars_json, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
            params![
                task.task_id.to_string(),
                task.timestamp.to_rfc3339(),
                task.command,
                format!("{:?}", task.task_type),
                task.priority,
                task.timeout,
                format!("{:?}", TaskStatus::Pending),
                task.working_dir.to_string_lossy().to_string(),
                task.batch_id.map(|id| id.to_string()),
                task.task_name,
                serde_json::to_string(&task.env_vars).unwrap_or_default(),
                serde_json::to_string(&task.metadata).unwrap_or_default(),
            ],
        )?;

        Ok(())
    }

    /// Update task status to Running
    pub fn mark_running(&self, task_id: Uuid) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1, started_at = ?2 WHERE task_id = ?3",
            params![
                format!("{:?}", TaskStatus::Running),
                Utc::now().to_rfc3339(),
                task_id.to_string(),
            ],
        )?;

        Ok(())
    }

    /// Insert or update a completed task result
    pub fn upsert_result(&self, result: &TaskResult) -> SqlResult<()> {
        let duration_ms = result.duration_secs() * 1000;
        let artifacts_json = serde_json::to_string(&result.artifacts).unwrap_or_default();
        let stdout_truncated = truncate(&result.output.stdout, 100_000);
        let stderr_truncated = truncate(&result.output.stderr, 100_000);

        let updated = self.conn.execute(
            "UPDATE tasks SET
                status       = ?1,
                exit_code    = ?2,
                stdout       = ?3,
                stderr       = ?4,
                error_message = ?5,
                retries_used = ?6,
                completed_at = ?7,
                duration_ms  = ?8,
                artifacts_json = ?9
             WHERE task_id = ?10",
            params![
                format!("{:?}", result.status),
                result.output.exit_code,
                stdout_truncated,
                stderr_truncated,
                result.error_message,
                result.retries_used,
                result.end_time.to_rfc3339(),
                duration_ms,
                artifacts_json,
                result.task_id.to_string(),
            ],
        )?;

        if updated == 0 {
            // Task wasn't inserted yet (e.g., executor path), do a full insert
            self.conn.execute(
                "INSERT INTO tasks (
                    task_id, created_at, command, task_type, priority,
                    timeout_secs, status, exit_code, stdout, stderr,
                    error_message, working_dir, retries_used,
                    started_at, completed_at, duration_ms, artifacts_json
                ) VALUES (?1, ?2, '', 'Shell', 0, 300, ?3, ?4, ?5, ?6, ?7, '.', ?8, ?9, ?10, ?11, ?12)",
                params![
                    result.task_id.to_string(),
                    result.start_time.to_rfc3339(),
                    format!("{:?}", result.status),
                    result.output.exit_code,
                    stdout_truncated,
                    stderr_truncated,
                    result.error_message,
                    result.retries_used,
                    result.start_time.to_rfc3339(),
                    result.end_time.to_rfc3339(),
                    duration_ms,
                    artifacts_json,
                ],
            )?;
        }

        Ok(())
    }

    /// Insert artifacts for a task
    pub fn insert_artifacts(&self, task_id: Uuid, artifacts: &[String]) -> SqlResult<()> {
        for artifact in artifacts {
            let path = PathBuf::from(artifact);
            let name = path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| artifact.clone());
            let size = std::fs::metadata(&path).ok().map(|m| m.len() as i64);

            self.conn.execute(
                "INSERT OR IGNORE INTO artifacts (task_id, name, path, size_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    task_id.to_string(),
                    name,
                    artifact,
                    size,
                ],
            )?;
        }

        Ok(())
    }

    /// Insert or update pytest results for a task
    pub fn upsert_pytest_result(
        &self,
        task_id: Uuid,
        passed: i64,
        failed: i64,
        skipped: i64,
        total: i64,
        duration_secs: Option<f64>,
        collected: Option<i64>,
        warnings: Option<i64>,
        environment: Option<&str>,
        report_json: Option<&str>,
    ) -> SqlResult<()> {
        self.conn.execute(
            "INSERT INTO pytest_results (task_id, passed, failed, skipped, total,
                                         duration_secs, collected, warnings,
                                         environment, report_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(task_id) DO UPDATE SET
                 passed       = excluded.passed,
                 failed       = excluded.failed,
                 skipped      = excluded.skipped,
                 total        = excluded.total,
                 duration_secs = excluded.duration_secs,
                 collected    = excluded.collected,
                 warnings     = excluded.warnings,
                 environment  = excluded.environment,
                 report_json  = excluded.report_json",
            params![
                task_id.to_string(),
                passed,
                failed,
                skipped,
                total,
                duration_secs,
                collected,
                warnings,
                environment,
                report_json,
            ],
        )?;

        Ok(())
    }

    /// Query task history with optional filters
    pub fn query_tasks(
        &self,
        status_filter: Option<TaskStatus>,
        task_type_filter: Option<TaskType>,
        limit: i64,
        offset: i64,
    ) -> SqlResult<Vec<TaskHistoryRecord>> {
        let mut sql = String::from(
            "SELECT task_id, command, task_type, status, exit_code,
                    error_message, created_at, started_at, completed_at,
                    duration_ms, retries_used, batch_id, task_name
             FROM tasks WHERE 1=1"
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();

        if let Some(ref status) = status_filter {
            sql.push_str(&format!(" AND status = ?{}", param_values.len() + 1));
            param_values.push(Box::new(format!("{:?}", status)));
        }

        if let Some(ref task_type) = task_type_filter {
            sql.push_str(&format!(" AND task_type = ?{}", param_values.len() + 1));
            param_values.push(Box::new(format!("{:?}", task_type)));
        }

        sql.push_str(" ORDER BY created_at DESC");
        sql.push_str(&format!(" LIMIT ?{}", param_values.len() + 1));
        param_values.push(Box::new(limit));
        sql.push_str(&format!(" OFFSET ?{}", param_values.len() + 1));
        param_values.push(Box::new(offset));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> = param_values.iter()
            .map(|p| p.as_ref())
            .collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let records = stmt.query_map(params_ref.as_slice(), |row| {
            Ok(TaskHistoryRecord {
                task_id: row.get::<_, String>(0)?,
                command: row.get(1)?,
                task_type: row.get(2)?,
                status: row.get(3)?,
                exit_code: row.get(4)?,
                error_message: row.get(5)?,
                created_at: row.get(6)?,
                started_at: row.get(7)?,
                completed_at: row.get(8)?,
                duration_ms: row.get(9)?,
                retries_used: row.get(10)?,
                batch_id: row.get(11)?,
                task_name: row.get(12)?,
            })
        })?;

        let mut results = Vec::new();
        for record in records {
            results.push(record?);
        }

        Ok(results)
    }

    /// Query detailed task result by task_id
    pub fn get_task_by_id(&self, task_id: Uuid) -> SqlResult<Option<TaskDetailRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT task_id, created_at, command, task_type, priority,
                    timeout_secs, status, exit_code, stdout, stderr,
                    error_message, working_dir, batch_id, task_name,
                    retries_used, started_at, completed_at, duration_ms,
                    artifacts_json, env_vars_json, metadata_json
             FROM tasks WHERE task_id = ?1"
        )?;

        let mut rows = stmt.query_map(params![task_id.to_string()], |row| {
            Ok(TaskDetailRecord {
                task_id: row.get::<_, String>(0)?,
                created_at: row.get(1)?,
                command: row.get(2)?,
                task_type: row.get(3)?,
                priority: row.get(4)?,
                timeout_secs: row.get(5)?,
                status: row.get(6)?,
                exit_code: row.get(7)?,
                stdout: row.get(8)?,
                stderr: row.get(9)?,
                error_message: row.get(10)?,
                working_dir: row.get(11)?,
                batch_id: row.get(12)?,
                task_name: row.get(13)?,
                retries_used: row.get(14)?,
                started_at: row.get(15)?,
                completed_at: row.get(16)?,
                duration_ms: row.get(17)?,
                artifacts_json: row.get(18)?,
                env_vars_json: row.get(19)?,
                metadata_json: row.get(20)?,
                artifacts: Vec::new(),
                pytest_result: None,
            })
        })?;

        match rows.next() {
            Some(Ok(mut record)) => {
                // Also fetch artifacts and pytest results
                record.artifacts = self.get_artifacts_for_task(task_id)?;
                record.pytest_result = self.get_pytest_result_for_task(task_id)?;
                Ok(Some(record))
            }
            _ => Ok(None),
        }
    }

    /// Get artifacts for a task
    fn get_artifacts_for_task(&self, task_id: Uuid) -> SqlResult<Vec<ArtifactRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, path, size_bytes FROM artifacts WHERE task_id = ?1"
        )?;

        let records = stmt.query_map(params![task_id.to_string()], |row| {
            Ok(ArtifactRecord {
                name: row.get(0)?,
                path: row.get(1)?,
                size_bytes: row.get(2)?,
            })
        })?;

        let mut results = Vec::new();
        for record in records {
            results.push(record?);
        }
        Ok(results)
    }

    /// Get pytest result for a task
    fn get_pytest_result_for_task(&self, task_id: Uuid) -> SqlResult<Option<PytestResultRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT passed, failed, skipped, total, duration_secs,
                    collected, warnings, environment, report_json
             FROM pytest_results WHERE task_id = ?1"
        )?;

        let mut rows = stmt.query_map(params![task_id.to_string()], |row| {
            Ok(PytestResultRecord {
                passed: row.get(0)?,
                failed: row.get(1)?,
                skipped: row.get(2)?,
                total: row.get(3)?,
                duration_secs: row.get(4)?,
                collected: row.get(5)?,
                warnings: row.get(6)?,
                environment: row.get(7)?,
                report_json: row.get(8)?,
            })
        })?;

        match rows.next() {
            Some(Ok(record)) => Ok(Some(record)),
            _ => Ok(None),
        }
    }

    /// Get summary statistics
    pub fn get_summary_stats(&self) -> SqlResult<TaskSummaryStats> {
        let total: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks", [], |row| row.get(0)
        )?;

        let by_status: Vec<(String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT status, COUNT(*) FROM tasks GROUP BY status"
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        let by_type: Vec<(String, i64)> = {
            let mut stmt = self.conn.prepare(
                "SELECT task_type, COUNT(*) FROM tasks GROUP BY task_type"
            )?;
            let rows = stmt.query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })?;
            let mut v = Vec::new();
            for r in rows {
                v.push(r?);
            }
            v
        };

        let last_24h: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE created_at_idx > strftime('%s', 'now', '-1 day')",
            [],
            |row| row.get(0),
        )?;

        let avg_duration: Option<f64> = self.conn.query_row(
            "SELECT AVG(duration_ms) FROM tasks WHERE duration_ms IS NOT NULL",
            [],
            |row| row.get(0),
        )?;

        Ok(TaskSummaryStats {
            total,
            by_status,
            by_type,
            last_24h,
            avg_duration_ms: avg_duration,
        })
    }
}

/// Truncate string to max_chars, appending "..." if truncated
fn truncate(s: &str, max_chars: usize) -> String {
    if s.len() > max_chars {
        format!("{}...", &s[..max_chars])
    } else {
        s.to_string()
    }
}

// ─── Query result types ───────────────────────────────────────────

/// Summary record for task history listing
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskHistoryRecord {
    pub task_id: String,
    pub command: String,
    pub task_type: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub error_message: Option<String>,
    pub created_at: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub retries_used: Option<i64>,
    pub batch_id: Option<String>,
    pub task_name: Option<String>,
}

/// Full detail record for a single task
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskDetailRecord {
    pub task_id: String,
    pub created_at: Option<String>,
    pub command: String,
    pub task_type: String,
    pub priority: i64,
    pub timeout_secs: i64,
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub error_message: Option<String>,
    pub working_dir: String,
    pub batch_id: Option<String>,
    pub task_name: Option<String>,
    pub retries_used: Option<i64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub artifacts_json: Option<String>,
    pub env_vars_json: Option<String>,
    pub metadata_json: Option<String>,
    pub artifacts: Vec<ArtifactRecord>,
    pub pytest_result: Option<PytestResultRecord>,
}

/// Artifact record
#[derive(Debug, Clone, serde::Serialize)]
pub struct ArtifactRecord {
    pub name: String,
    pub path: String,
    pub size_bytes: Option<i64>,
}

/// Pytest result record
#[derive(Debug, Clone, serde::Serialize)]
pub struct PytestResultRecord {
    pub passed: i64,
    pub failed: i64,
    pub skipped: i64,
    pub total: i64,
    pub duration_secs: Option<f64>,
    pub collected: Option<i64>,
    pub warnings: Option<i64>,
    pub environment: Option<String>,
    pub report_json: Option<String>,
}

/// Summary statistics for the task database
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskSummaryStats {
    pub total: i64,
    pub by_status: Vec<(String, i64)>,
    pub by_type: Vec<(String, i64)>,
    pub last_24h: i64,
    pub avg_duration_ms: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{Task, TaskResult, TaskOutput, TaskStatus, TaskType};
    use chrono::Utc;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_tables() {
        let db = setup_db();
        // Verify tables exist by querying sqlite_master
        let tables: Vec<String> = db.conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"tasks".to_string()));
        assert!(tables.contains(&"artifacts".to_string()));
        assert!(tables.contains(&"pytest_results".to_string()));
    }

    #[test]
    fn test_insert_and_query_task() {
        let db = setup_db();
        let task = Task::new("echo hello".to_string(), TaskType::Shell)
            .with_priority(10)
            .with_timeout(300)
            .with_task_name("test_task".to_string());

        db.insert_task(&task).unwrap();

        let records = db.query_tasks(None, None, 10, 0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].command, "echo hello");
        assert_eq!(records[0].task_name, Some("test_task".to_string()));
        assert_eq!(records[0].status, "Pending");
    }

    #[test]
    fn test_mark_running_and_upsert_result() {
        let db = setup_db();
        let task = Task::new("sleep 1".to_string(), TaskType::Shell);
        let task_id = task.task_id;

        db.insert_task(&task).unwrap();
        db.mark_running(task_id).unwrap();

        // Verify running
        let records = db.query_tasks(Some(TaskStatus::Running), None, 10, 0).unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].started_at.is_some());

        // Complete the task
        let result = TaskResult {
            task_id,
            status: TaskStatus::Completed,
            output: TaskOutput {
                stdout: "done".to_string(),
                stderr: String::new(),
                exit_code: Some(0),
            },
            start_time: Utc::now() - chrono::Duration::seconds(5),
            end_time: Utc::now(),
            retries_used: 0,
            artifacts: vec!["report.json".to_string()],
            error_message: None,
        };

        db.upsert_result(&result).unwrap();

        // Verify completed
        let records = db.query_tasks(Some(TaskStatus::Completed), None, 10, 0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].exit_code, Some(0));
        assert!(records[0].duration_ms.is_some());
    }

    #[test]
    fn test_pytest_result() {
        let db = setup_db();
        let task = Task::new("pytest tests/".to_string(), TaskType::Pytest);
        let task_id = task.task_id;

        db.insert_task(&task).unwrap();

        // Insert pytest result
        db.upsert_pytest_result(
            task_id,
            10,    // passed
            2,     // failed
            1,     // skipped
            13,    // total
            Some(5.5),  // duration_secs
            Some(13),   // collected
            Some(0),    // warnings
            Some("Linux"),  // environment
            Some(r#"{"summary":{"passed":10,"failed":2,"total":13}}"#),
        ).unwrap();

        // Query detail
        let detail = db.get_task_by_id(task_id).unwrap();
        assert!(detail.is_some());
        let detail = detail.unwrap();

        let pytest = detail.pytest_result;
        assert!(pytest.is_some());
        let pytest = pytest.unwrap();
        assert_eq!(pytest.passed, 10);
        assert_eq!(pytest.failed, 2);
        assert_eq!(pytest.total, 13);
        assert!(pytest.report_json.is_some());
    }

    #[test]
    fn test_artifacts() {
        let db = setup_db();
        let task = Task::new("make".to_string(), TaskType::Shell);
        let task_id = task.task_id;

        db.insert_task(&task).unwrap();
        db.insert_artifacts(task_id, &["output.log".to_string(), "report.xml".to_string()]).unwrap();

        let detail = db.get_task_by_id(task_id).unwrap().unwrap();
        assert_eq!(detail.artifacts.len(), 2);
    }

    #[test]
    fn test_summary_stats() {
        let db = setup_db();

        // Insert 3 tasks with different statuses
        let t1 = Task::new("cmd1".to_string(), TaskType::Shell);
        db.insert_task(&t1).unwrap();

        let t2 = Task::new("cmd2".to_string(), TaskType::Shell);
        db.insert_task(&t2).unwrap();
        db.mark_running(t2.task_id).unwrap();

        let t3 = Task::new("pytest x".to_string(), TaskType::Pytest);
        db.insert_task(&t3).unwrap();

        let stats = db.get_summary_stats().unwrap();
        assert_eq!(stats.total, 3);

        let running_count = stats.by_status.iter()
            .find(|(s, _)| s == "Running")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(running_count, 1);
    }

    #[test]
    fn test_insert_duplicate_task_id() {
        let db = setup_db();
        let task = Task::new("cmd".to_string(), TaskType::Shell);

        db.insert_task(&task).unwrap();

        // duplicate INSERT should fail with UNIQUE constraint
        let result = db.insert_task(&task);
        assert!(result.is_err());

        // upsert_result on same task_id should work (UPDATE then INSERT fallback)
        let tr = TaskResult {
            task_id: task.task_id,
            status: TaskStatus::Completed,
            output: TaskOutput {
                stdout: "done".to_string(),
                stderr: String::new(),
                exit_code: Some(0),
            },
            start_time: Utc::now(),
            end_time: Utc::now(),
            retries_used: 0,
            artifacts: vec![],
            error_message: None,
        };
        assert!(db.upsert_result(&tr).is_ok());
    }
}
