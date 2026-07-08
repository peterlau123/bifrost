// SQLite database module for task history tracking
//
// Schema design:
//   tasks          - Core task metadata (lightweight, for fast queries)
//   task_outputs   - stdout/stderr (separated to keep tasks table slim)
//   task_env_vars  - Environment variables (normalized key-value pairs)
//   artifacts      - Generated artifact files per task
//   pytest_results - Structured pytest test reports
//
// Constraints:
//   - All timestamps are INTEGER (unix epoch seconds, UTC)
//   - status and task_type have CHECK constraints
//   - Foreign keys enforced via PRAGMA

use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, Result as SqlResult};
use std::path::PathBuf;
use uuid::Uuid;

use crate::core::models::{Task, TaskResult, TaskStatus, TaskType};

/// Database manager for task history
pub struct Database {
    conn: Connection,
    /// Path to the SQLite database file
    pub path: PathBuf,
    /// Shared storage root for computing log/artifact paths at runtime
    shared_storage: Option<PathBuf>,
}

impl Database {
    /// Open or create a database at the given path with optional shared storage root.
    ///
    /// `shared_storage` is used at runtime to compute log directory paths
    /// (`{shared_storage}/logs/{task_id}/`). Pass it here or set later via
    /// [`Database::set_shared_storage`].
    pub fn open(path: PathBuf, shared_storage: Option<PathBuf>) -> SqlResult<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        }

        let conn = Connection::open(&path)?;
        let db = Self {
            conn,
            path: path.clone(),
            shared_storage,
        };
        db.initialize()?;
        Ok(db)
    }

    /// Open an in-memory database (for testing)
    pub fn open_in_memory() -> SqlResult<Self> {
        let conn = Connection::open_in_memory()?;
        let db = Self {
            conn,
            path: PathBuf::from(":memory:"),
            shared_storage: None,
        };
        db.initialize()?;
        Ok(db)
    }

    /// Enable foreign keys and create tables
    fn initialize(&self) -> SqlResult<()> {
        // ── Foreign keys MUST be enabled per-connection ──
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;

        self.conn.execute_batch("
            -- Core task metadata (lightweight, no large text fields)
            CREATE TABLE IF NOT EXISTS tasks (
                task_id         TEXT    NOT NULL PRIMARY KEY,
                created_at      INTEGER NOT NULL,
                command         TEXT    NOT NULL,
                task_type       TEXT    NOT NULL CHECK(task_type IN ('Shell', 'Pytest', 'Custom')),
                priority        INTEGER NOT NULL DEFAULT 0,
                timeout_secs    INTEGER NOT NULL DEFAULT 300,
                status          TEXT    NOT NULL DEFAULT 'Pending'
                                    CHECK(status IN ('Pending','Running','Completed','Failed','Cancelled','Timeout')),
                exit_code       INTEGER,
                error_message   TEXT,
                working_dir     TEXT    NOT NULL DEFAULT '.',
                batch_id        TEXT,
                task_name       TEXT,
                retries_used    INTEGER NOT NULL DEFAULT 0,
                started_at      INTEGER,
                completed_at    INTEGER,
                duration_ms     INTEGER,
                metadata_json   TEXT,
                task_group      TEXT,
                run_attempt     INTEGER NOT NULL DEFAULT 1,
                git_commit      TEXT,
                environment     TEXT,
                trigger         TEXT
            );

            -- Index for task_group trend queries (e.g. \'vllm-daily-regression\')
            CREATE INDEX IF NOT EXISTS idx_tasks_task_group   ON tasks(task_group);
            -- Composite index for the most common query pattern: filter by status then sort by time
            CREATE INDEX IF NOT EXISTS idx_tasks_status_created ON tasks(status, created_at);
            CREATE INDEX IF NOT EXISTS idx_tasks_task_type      ON tasks(task_type);
            CREATE INDEX IF NOT EXISTS idx_tasks_batch_id       ON tasks(batch_id);

            -- Large I/O content separated from tasks to keep queries fast
            CREATE TABLE IF NOT EXISTS task_outputs (
                task_id     TEXT NOT NULL PRIMARY KEY REFERENCES tasks(task_id) ON DELETE CASCADE,
                stdout      TEXT,
                stderr      TEXT
            );

            -- Environment variables normalized (key-value pairs)
            CREATE TABLE IF NOT EXISTS task_env_vars (
                task_id     TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
                key         TEXT NOT NULL,
                value       TEXT NOT NULL,
                PRIMARY KEY (task_id, key)
            );

            -- Artifact file records per task
            CREATE TABLE IF NOT EXISTS artifacts (
                task_id     TEXT NOT NULL REFERENCES tasks(task_id) ON DELETE CASCADE,
                name        TEXT NOT NULL,
                path        TEXT NOT NULL,
                size_bytes  INTEGER,
                PRIMARY KEY (task_id, name)
            );

            -- Structured pytest results (duration_ms for consistency with tasks)
            CREATE TABLE IF NOT EXISTS pytest_results (
                task_id     TEXT NOT NULL PRIMARY KEY REFERENCES tasks(task_id) ON DELETE CASCADE,
                passed      INTEGER NOT NULL DEFAULT 0,
                failed      INTEGER NOT NULL DEFAULT 0,
                skipped     INTEGER NOT NULL DEFAULT 0,
                total       INTEGER NOT NULL DEFAULT 0,
                duration_ms INTEGER,
                collected   INTEGER,
                warnings    INTEGER,
                environment TEXT,
                report_json TEXT
            );
        ")?;
        Ok(())
    }

    // ─── Status/task_type helpers ────────────────────────────────

    fn status_to_str(s: &TaskStatus) -> &'static str {
        match s {
            TaskStatus::Pending => "Pending",
            TaskStatus::Running => "Running",
            TaskStatus::Completed => "Completed",
            TaskStatus::Failed => "Failed",
            TaskStatus::Cancelled => "Cancelled",
            TaskStatus::Timeout => "Timeout",
        }
    }

    fn task_type_str(t: &TaskType) -> &'static str {
        match t {
            TaskType::Shell => "Shell",
            TaskType::Pytest => "Pytest",
            TaskType::Custom => "Custom",
        }
    }

    fn ts_to_int(dt: &DateTime<Utc>) -> i64 {
        dt.timestamp()
    }

    fn int_to_ts_str(ts: Option<i64>) -> Option<String> {
        ts.and_then(|t| DateTime::from_timestamp(t, 0))
            .map(|dt| dt.to_rfc3339())
    }

    // ─── Write operations ─────────────────────────────────────────

    /// Insert a new pending task record
    ///
    /// # Arguments
    /// * `task` - The task to insert
    /// * `meta` - Optional extra tracking metadata (task_group, git_commit, etc.)
    pub fn insert_task(&self, task: &Task, meta: Option<&TaskMeta>) -> SqlResult<()> {
        let default_meta = TaskMeta::default();
        let meta = meta.unwrap_or(&default_meta);

        self.conn.execute(
            "INSERT INTO tasks (task_id, created_at, command, task_type, priority,
                                timeout_secs, status, working_dir, batch_id, task_name,
                                metadata_json, task_group, git_commit,
                                environment, trigger)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
            params![
                task.task_id.to_string(),
                Self::ts_to_int(&task.timestamp),
                task.command,
                Self::task_type_str(&task.task_type),
                task.priority,
                task.timeout,
                Self::status_to_str(&TaskStatus::Pending),
                task.working_dir.to_string_lossy().to_string(),
                task.batch_id.map(|id| id.to_string()),
                task.task_name,
                serde_json::to_string(&task.metadata).unwrap_or_default(),
                meta.task_group,
                meta.git_commit,
                meta.environment,
                meta.trigger,
            ],
        )?;

        // Normalized env vars
        for (key, value) in &task.env_vars {
            self.conn.execute(
                "INSERT INTO task_env_vars (task_id, key, value) VALUES (?1, ?2, ?3)",
                params![task.task_id.to_string(), key, value],
            )?;
        }

        Ok(())
    }

    /// Update task status to Running
    pub fn mark_running(&self, task_id: Uuid) -> SqlResult<()> {
        self.conn.execute(
            "UPDATE tasks SET status = ?1, started_at = ?2 WHERE task_id = ?3",
            params![
                Self::status_to_str(&TaskStatus::Running),
                Self::ts_to_int(&Utc::now()),
                task_id.to_string(),
            ],
        )?;
        Ok(())
    }

    /// Insert or update a completed task result
    pub fn upsert_result(&self, result: &TaskResult) -> SqlResult<()> {
        let duration_ms = result.duration_secs() * 1000;

        // Upsert the lightweight metadata
        let updated = self.conn.execute(
            "UPDATE tasks SET
                status       = ?1,
                exit_code    = ?2,
                error_message = ?3,
                retries_used = ?4,
                completed_at = ?5,
                duration_ms  = ?6
             WHERE task_id = ?7",
            params![
                Self::status_to_str(&result.status),
                result.output.exit_code,
                result.error_message,
                result.retries_used,
                Self::ts_to_int(&result.end_time),
                duration_ms,
                result.task_id.to_string(),
            ],
        )?;

        if updated == 0 {
            // Fallback: task wasn't inserted yet — do a full insert
            self.conn.execute(
                "INSERT INTO tasks (task_id, created_at, command, task_type, priority,
                                    timeout_secs, status, exit_code, error_message,
                                    working_dir, retries_used, started_at, completed_at,
                                    duration_ms)
                 VALUES (?1, ?2, '', 'Shell', 0, 300, ?3, ?4, ?5, '.', ?6, ?7, ?8, ?9)",
                params![
                    result.task_id.to_string(),
                    Self::ts_to_int(&result.start_time),
                    Self::status_to_str(&result.status),
                    result.output.exit_code,
                    result.error_message,
                    result.retries_used,
                    Self::ts_to_int(&result.start_time),
                    Self::ts_to_int(&result.end_time),
                    duration_ms,
                ],
            )?;
        }

        // Upsert stdout/stderr into the separate outputs table
        let stdout_truncated = truncate(&result.output.stdout, 100_000);
        let stderr_truncated = truncate(&result.output.stderr, 100_000);

        self.conn.execute(
            "INSERT INTO task_outputs (task_id, stdout, stderr)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(task_id) DO UPDATE SET
                 stdout = excluded.stdout,
                 stderr = excluded.stderr",
            params![
                result.task_id.to_string(),
                stdout_truncated,
                stderr_truncated,
            ],
        )?;

        Ok(())
    }

    /// Insert artifacts for a task
    pub fn insert_artifacts(&self, task_id: Uuid, artifacts: &[String]) -> SqlResult<()> {
        for artifact in artifacts {
            let path = PathBuf::from(artifact);
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| artifact.clone());
            let size = std::fs::metadata(&path).ok().map(|m| m.len() as i64);

            self.conn.execute(
                "INSERT OR IGNORE INTO artifacts (task_id, name, path, size_bytes)
                 VALUES (?1, ?2, ?3, ?4)",
                params![task_id.to_string(), name, artifact, size],
            )?;
        }
        Ok(())
    }

    /// Insert or update pytest results for a task
    ///
    /// NOTE: `duration_ms` here is the pytest-reported duration (may differ from
    /// total wall-clock duration stored in tasks.duration_ms).
    pub fn upsert_pytest_result(
        &self,
        task_id: Uuid,
        passed: i64,
        failed: i64,
        skipped: i64,
        total: i64,
        duration_ms: Option<i64>,
        collected: Option<i64>,
        warnings: Option<i64>,
        environment: Option<&str>,
        report_json: Option<&str>,
    ) -> SqlResult<()> {
        self.conn.execute(
            "INSERT INTO pytest_results (task_id, passed, failed, skipped, total,
                                         duration_ms, collected, warnings,
                                         environment, report_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
             ON CONFLICT(task_id) DO UPDATE SET
                 passed       = excluded.passed,
                 failed       = excluded.failed,
                 skipped      = excluded.skipped,
                 total        = excluded.total,
                 duration_ms  = excluded.duration_ms,
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
                duration_ms,
                collected,
                warnings,
                environment,
                report_json,
            ],
        )?;
        Ok(())
    }

    // ─── Query operations ─────────────────────────────────────────

    /// Query task history with optional filters
    pub fn query_tasks(
        &self,
        status_filter: Option<TaskStatus>,
        task_type_filter: Option<TaskType>,
        limit: i64,
        offset: i64,
    ) -> SqlResult<Vec<TaskHistoryRecord>> {
        let mut sql = String::from(
            "SELECT task_id, created_at, command, task_type, status, exit_code,
                    error_message, started_at, completed_at,
                    duration_ms, retries_used, batch_id, task_name
             FROM tasks WHERE 1=1",
        );
        let mut param_values: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
        let mut param_idx = 1;

        if let Some(ref status) = status_filter {
            sql.push_str(&format!(" AND status = ?{}", param_idx));
            param_values.push(Box::new(Self::status_to_str(status).to_string()));
            param_idx += 1;
        }

        if let Some(ref task_type) = task_type_filter {
            sql.push_str(&format!(" AND task_type = ?{}", param_idx));
            param_values.push(Box::new(Self::task_type_str(task_type).to_string()));
            param_idx += 1;
        }

        sql.push_str(" ORDER BY created_at DESC");
        sql.push_str(&format!(" LIMIT ?{}", param_idx));
        param_values.push(Box::new(limit));
        param_idx += 1;
        sql.push_str(&format!(" OFFSET ?{}", param_idx));
        param_values.push(Box::new(offset));

        let params_ref: Vec<&dyn rusqlite::types::ToSql> =
            param_values.iter().map(|p| p.as_ref()).collect();

        let mut stmt = self.conn.prepare(&sql)?;
        let records = stmt.query_map(params_ref.as_slice(), |row| {
            Ok(TaskHistoryRecord {
                task_id: row.get::<_, String>(0)?,
                created_at: Self::int_to_ts_str(row.get::<_, Option<i64>>(1)?),
                command: row.get(2)?,
                task_type: row.get(3)?,
                status: row.get(4)?,
                exit_code: row.get(5)?,
                error_message: row.get(6)?,
                started_at: Self::int_to_ts_str(row.get::<_, Option<i64>>(7)?),
                completed_at: Self::int_to_ts_str(row.get::<_, Option<i64>>(8)?),
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
    ///
    /// Joins with task_outputs, task_env_vars, artifacts, and pytest_results.
    pub fn get_task_by_id(&self, task_id: Uuid) -> SqlResult<Option<TaskDetailRecord>> {
        let tid = task_id.to_string();

        let mut stmt = self.conn.prepare(
            "SELECT task_id, created_at, command, task_type, priority,
                    timeout_secs, status, exit_code, error_message, working_dir,
                    batch_id, task_name, retries_used, started_at, completed_at,
                    duration_ms, metadata_json
             FROM tasks WHERE task_id = ?1",
        )?;

        let mut rows = stmt.query_map(params![tid], |row| {
            Ok(TaskDetailRecord {
                task_id: row.get::<_, String>(0)?,
                created_at: Self::int_to_ts_str(row.get::<_, Option<i64>>(1)?),
                command: row.get(2)?,
                task_type: row.get(3)?,
                priority: row.get(4)?,
                timeout_secs: row.get(5)?,
                status: row.get(6)?,
                exit_code: row.get(7)?,
                error_message: row.get(8)?,
                working_dir: row.get(9)?,
                batch_id: row.get(10)?,
                task_name: row.get(11)?,
                retries_used: row.get(12)?,
                started_at: Self::int_to_ts_str(row.get::<_, Option<i64>>(13)?),
                completed_at: Self::int_to_ts_str(row.get::<_, Option<i64>>(14)?),
                duration_ms: row.get(15)?,
                metadata_json: row.get(16)?,
                // Filled below after the row
                stdout: None,
                stderr: None,
                env_vars: Vec::new(),
                artifacts: Vec::new(),
                pytest_result: None,
            })
        })?;

        match rows.next() {
            Some(Ok(mut record)) => {
                // Fetch from related tables
                record.stdout = self.get_output(tid.as_str(), OutputColumn::Stdout)?;
                record.stderr = self.get_output(tid.as_str(), OutputColumn::Stderr)?;
                record.env_vars = self.get_env_vars(tid.as_str())?;
                record.artifacts = self.get_artifacts_for_task(tid.as_str())?;
                record.pytest_result = self.get_pytest_result_for_task(tid.as_str())?;
                Ok(Some(record))
            }
            _ => Ok(None),
        }
    }

    /// Read a single output column from task_outputs
    fn get_output(&self, task_id: &str, column: OutputColumn) -> SqlResult<Option<String>> {
        let sql = format!(
            "SELECT {} FROM task_outputs WHERE task_id = ?1",
            column.name()
        );
        self.conn
            .query_row(&sql, params![task_id], |row| row.get(0))
            .optional()
    }

    /// Read env vars for a task
    fn get_env_vars(&self, task_id: &str) -> SqlResult<Vec<(String, String)>> {
        let mut stmt = self
            .conn
            .prepare("SELECT key, value FROM task_env_vars WHERE task_id = ?1 ORDER BY key")?;
        let rows = stmt.query_map(params![task_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }

    /// Get artifacts for a task
    fn get_artifacts_for_task(&self, task_id: &str) -> SqlResult<Vec<ArtifactRecord>> {
        let mut stmt = self
            .conn
            .prepare("SELECT name, path, size_bytes FROM artifacts WHERE task_id = ?1")?;
        let rows = stmt.query_map(params![task_id], |row| {
            Ok(ArtifactRecord {
                name: row.get(0)?,
                path: row.get(1)?,
                size_bytes: row.get(2)?,
            })
        })?;
        let mut v = Vec::new();
        for r in rows {
            v.push(r?);
        }
        Ok(v)
    }

    /// Get pytest result for a task
    fn get_pytest_result_for_task(&self, task_id: &str) -> SqlResult<Option<PytestResultRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT passed, failed, skipped, total, duration_ms,
                    collected, warnings, environment, report_json
             FROM pytest_results WHERE task_id = ?1",
        )?;
        let mut rows = stmt.query_map(params![task_id], |row| {
            Ok(PytestResultRecord {
                passed: row.get(0)?,
                failed: row.get(1)?,
                skipped: row.get(2)?,
                total: row.get(3)?,
                duration_ms: row.get(4)?,
                collected: row.get(5)?,
                warnings: row.get(6)?,
                environment: row.get(7)?,
                report_json: row.get(8)?,
            })
        })?;
        match rows.next() {
            Some(Ok(r)) => Ok(Some(r)),
            _ => Ok(None),
        }
    }

    /// Helper: execute a GROUP BY query returning (String, i64) pairs
    fn collect_pairs(&self, sql: &str) -> SqlResult<Vec<(String, i64)>> {
        let mut stmt = self.conn.prepare(sql)?;
        let mut results = Vec::new();
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            results.push(row?);
        }
        Ok(results)
    }

    /// Set the shared storage root path (used for log/artifact path computation)
    pub fn set_shared_storage(&mut self, path: PathBuf) {
        self.shared_storage = Some(path);
    }

    /// Compute the log directory for a task based on shared storage path.
    ///
    /// Returns `{shared_storage}/logs/{task_id}/` if `shared_storage` is configured,
    /// or `None` if no shared storage path has been set.
    pub fn get_log_dir(&self, task_id: Uuid) -> Option<PathBuf> {
        self.shared_storage
            .as_ref()
            .map(|root| root.join("logs").join(task_id.to_string()))
    }

    /// Parse a pytest JSON report string and record structured results.
    ///
    /// Extracts `passed`, `failed`, `skipped`, `total`, `duration`,
    /// `collected`, `warnings`, and `environment` from the report summary
    /// and writes them to the `pytest_results` table.
    pub fn record_pytest_report(&self, task_id: Uuid, report_content: &str) -> SqlResult<()> {
        use serde_json::Value;

        let report: Value = serde_json::from_str(report_content)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;

        let summary = report.get("summary").ok_or_else(|| {
            let e: Box<dyn std::error::Error + Send + Sync> = "No summary in pytest report".into();
            rusqlite::Error::ToSqlConversionFailure(e)
        })?;

        let passed = summary.get("passed").and_then(|v| v.as_i64()).unwrap_or(0);
        let failed = summary.get("failed").and_then(|v| v.as_i64()).unwrap_or(0);
        let skipped = summary.get("skipped").and_then(|v| v.as_i64()).unwrap_or(0);
        let total = summary.get("total").and_then(|v| v.as_i64()).unwrap_or(0);
        let duration_secs = summary.get("duration").and_then(|v| v.as_f64());
        let collected = summary.get("collected").and_then(|v| v.as_i64());
        let warnings = summary.get("warnings").and_then(|v| v.as_i64());

        let environment = report.get("environment").map(|v| v.to_string());
        let report_json = Some(report_content);

        self.upsert_pytest_result(
            task_id,
            passed,
            failed,
            skipped,
            total,
            duration_secs.map(|d| (d * 1000.0) as i64),
            collected,
            warnings,
            environment.as_deref(),
            report_json,
        )
    }

    /// Get summary statistics
    pub fn get_summary_stats(&self) -> SqlResult<TaskSummaryStats> {
        let total: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;

        let by_status = self.collect_pairs("SELECT status, COUNT(*) FROM tasks GROUP BY status")?;

        let by_type =
            self.collect_pairs("SELECT task_type, COUNT(*) FROM tasks GROUP BY task_type")?;

        let last_24h: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE created_at > strftime('%s', 'now', '-1 day')",
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

/// Extra metadata for SQLite-only tracking fields that don't exist on the Task model.
///
/// These are database-layer concerns (grouping, environment, provenance)
/// rather than command-layer concerns, so they live here, not in models::Task.
#[derive(Default)]
pub struct TaskMeta {
    pub task_group: Option<String>,
    pub git_commit: Option<String>,
    pub environment: Option<String>,
    pub trigger: Option<String>,
}

/// Which output column to read from task_outputs
enum OutputColumn {
    Stdout,
    Stderr,
}

impl OutputColumn {
    fn name(&self) -> &'static str {
        match self {
            OutputColumn::Stdout => "stdout",
            OutputColumn::Stderr => "stderr",
        }
    }
}

/// Helper: Optional row helper
trait OptionalRow {
    fn optional(self) -> SqlResult<Option<String>>;
}

impl OptionalRow for Result<String, rusqlite::Error> {
    fn optional(self) -> SqlResult<Option<String>> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

/// Truncate string to max_bytes, appending "..." if truncated.
///
/// Uses char_indices to find a safe UTF-8 boundary so multi-byte
/// characters are never split.
fn truncate(s: &str, max_bytes: usize) -> String {
    if s.len() > max_bytes {
        let safe_boundary = s
            .char_indices()
            .take_while(|(i, _)| *i < max_bytes)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        let mut result = s[..safe_boundary].to_string();
        result.push_str("...");
        result
    } else {
        s.to_string()
    }
}

// ─── Query result types ───────────────────────────────────────────

/// Summary record for task history listing (lightweight, no large fields)
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskHistoryRecord {
    pub task_id: String,
    pub created_at: Option<String>,
    pub command: String,
    pub task_type: String,
    pub status: String,
    pub exit_code: Option<i32>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub retries_used: Option<i64>,
    pub batch_id: Option<String>,
    pub task_name: Option<String>,
}

/// Full detail record for a single task (includes large I/O and related rows)
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
    pub error_message: Option<String>,
    pub working_dir: String,
    pub batch_id: Option<String>,
    pub task_name: Option<String>,
    pub retries_used: Option<i64>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_ms: Option<i64>,
    pub metadata_json: Option<String>,
    // Large I/O (separate table, fetched on demand)
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    // Normalized related tables
    pub env_vars: Vec<(String, String)>,
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
    pub duration_ms: Option<i64>,
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
    use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus, TaskType};
    use chrono::Utc;

    fn setup_db() -> Database {
        Database::open_in_memory().unwrap()
    }

    #[test]
    fn test_create_tables() {
        let db = setup_db();
        let tables: Vec<String> = db
            .conn
            .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .filter_map(|r| r.ok())
            .collect();

        assert!(tables.contains(&"tasks".to_string()), "tasks table missing");
        assert!(
            tables.contains(&"task_outputs".to_string()),
            "task_outputs table missing"
        );
        assert!(
            tables.contains(&"task_env_vars".to_string()),
            "task_env_vars table missing"
        );
        assert!(
            tables.contains(&"artifacts".to_string()),
            "artifacts table missing"
        );
        assert!(
            tables.contains(&"pytest_results".to_string()),
            "pytest_results table missing"
        );
    }

    #[test]
    fn test_foreign_keys_enabled() {
        let db = setup_db();
        let enabled: bool = db
            .conn
            .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
            .unwrap();
        assert!(enabled, "foreign_keys PRAGMA must be ON");
    }

    #[test]
    fn test_check_constraint_status() {
        let db = setup_db();
        let result = db.conn.execute(
            "INSERT INTO tasks (task_id, created_at, command, task_type, status)
             VALUES ('bad-status', 0, 'x', 'Shell', 'InvalidStatus')",
            [],
        );
        assert!(
            result.is_err(),
            "CHECK constraint should reject invalid status"
        );
    }

    #[test]
    fn test_check_constraint_task_type() {
        let db = setup_db();
        let result = db.conn.execute(
            "INSERT INTO tasks (task_id, created_at, command, task_type, status)
             VALUES ('bad-type', 0, 'x', 'Unknown', 'Pending')",
            [],
        );
        assert!(
            result.is_err(),
            "CHECK constraint should reject invalid task_type"
        );
    }

    #[test]
    fn test_foreign_key_cascade() {
        let db = setup_db();
        // Insert a valid task
        db.conn
            .execute(
                "INSERT INTO tasks (task_id, created_at, command, task_type, status)
             VALUES ('fk-test', 1000, 'echo', 'Shell', 'Completed')",
                [],
            )
            .unwrap();
        // Insert an artifact referencing it
        db.conn.execute(
            "INSERT INTO artifacts (task_id, name, path) VALUES ('fk-test', 'out.log', '/tmp/out.log')",
            [],
        ).unwrap();
        // Delete the task — cascade should remove the artifact
        db.conn
            .execute("DELETE FROM tasks WHERE task_id = 'fk-test'", [])
            .unwrap();
        let count: i64 = db
            .conn
            .query_row(
                "SELECT COUNT(*) FROM artifacts WHERE task_id = 'fk-test'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0, "ON DELETE CASCADE should remove child rows");
    }

    #[test]
    fn test_insert_and_query_task() {
        let db = setup_db();
        let task = Task::new("echo hello".to_string(), TaskType::Shell)
            .with_priority(10)
            .with_timeout(300)
            .with_task_name("test_task".to_string())
            .with_env_var("PATH".to_string(), "/usr/bin".to_string());

        db.insert_task(&task, None).unwrap();

        let records = db.query_tasks(None, None, 10, 0).unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].command, "echo hello");
        assert_eq!(records[0].task_name, Some("test_task".to_string()));
        assert_eq!(records[0].status, "Pending");
        assert!(
            records[0].created_at.is_some(),
            "created_at should be present"
        );
    }

    #[test]
    fn test_mark_running_and_upsert_result() {
        let db = setup_db();
        let task = Task::new("sleep 1".to_string(), TaskType::Shell);
        let task_id = task.task_id;

        db.insert_task(&task, None).unwrap();
        db.mark_running(task_id).unwrap();

        // Verify running
        let records = db
            .query_tasks(Some(TaskStatus::Running), None, 10, 0)
            .unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].started_at.is_some());

        // Complete the task
        let result = TaskResult {
            task_id,
            status: TaskStatus::Completed,
            output: TaskOutput {
                stdout: "done\nline2".to_string(),
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

        // Verify completed via query
        let records = db
            .query_tasks(Some(TaskStatus::Completed), None, 10, 0)
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].exit_code, Some(0));
        assert!(records[0].duration_ms.is_some());

        // Verify stdout/stderr in separate table
        let detail = db.get_task_by_id(task_id).unwrap().unwrap();
        assert_eq!(detail.stdout, Some("done\nline2".to_string()));
        assert_eq!(detail.stderr, Some(String::new()));
    }

    #[test]
    fn test_pytest_result() {
        let db = setup_db();
        let task = Task::new("pytest tests/".to_string(), TaskType::Pytest);
        let task_id = task.task_id;

        db.insert_task(&task, None).unwrap();

        db.upsert_pytest_result(
            task_id,
            10,         // passed
            2,          // failed
            1,          // skipped
            13,         // total
            Some(5500), // duration_ms
            Some(13),   // collected
            Some(0),    // warnings
            Some("Linux"),
            Some(r#"{"summary":{"passed":10,"failed":2,"total":13}}"#),
        )
        .unwrap();

        let detail = db.get_task_by_id(task_id).unwrap().unwrap();
        let pytest = detail
            .pytest_result
            .expect("pytest_result should be present");
        assert_eq!(pytest.passed, 10);
        assert_eq!(pytest.failed, 2);
        assert_eq!(pytest.total, 13);
        assert_eq!(pytest.duration_ms, Some(5500));
        assert!(pytest.report_json.is_some());
    }

    #[test]
    fn test_artifacts() {
        let db = setup_db();
        let task = Task::new("make".to_string(), TaskType::Shell);
        let task_id = task.task_id;

        db.insert_task(&task, None).unwrap();
        db.insert_artifacts(
            task_id,
            &["output.log".to_string(), "report.xml".to_string()],
        )
        .unwrap();

        let detail = db.get_task_by_id(task_id).unwrap().unwrap();
        assert_eq!(detail.artifacts.len(), 2);
    }

    #[test]
    fn test_env_vars() {
        let db = setup_db();
        let task = Task::new("cmd".to_string(), TaskType::Shell)
            .with_env_var("KEY1".to_string(), "val1".to_string())
            .with_env_var("KEY2".to_string(), "val2".to_string());
        let task_id = task.task_id;

        db.insert_task(&task, None).unwrap();

        let detail = db.get_task_by_id(task_id).unwrap().unwrap();
        assert_eq!(detail.env_vars.len(), 2);
        assert!(detail.env_vars.contains(&("KEY1".into(), "val1".into())));
        assert!(detail.env_vars.contains(&("KEY2".into(), "val2".into())));
    }

    #[test]
    fn test_summary_stats() {
        let db = setup_db();

        let t1 = Task::new("cmd1".to_string(), TaskType::Shell);
        db.insert_task(&t1, None).unwrap();

        let t2 = Task::new("cmd2".to_string(), TaskType::Shell);
        db.insert_task(&t2, None).unwrap();
        db.mark_running(t2.task_id).unwrap();

        let t3 = Task::new("pytest x".to_string(), TaskType::Pytest);
        db.insert_task(&t3, None).unwrap();

        let stats = db.get_summary_stats().unwrap();
        assert_eq!(stats.total, 3);

        let running_count = stats
            .by_status
            .iter()
            .find(|(s, _)| s == "Running")
            .map(|(_, c)| *c)
            .unwrap_or(0);
        assert_eq!(running_count, 1);
    }

    #[test]
    fn test_insert_duplicate_task_id() {
        let db = setup_db();
        let task = Task::new("cmd".to_string(), TaskType::Shell);
        db.insert_task(&task, None).unwrap();

        // Duplicate INSERT should fail due to PRIMARY KEY
        let result = db.insert_task(&task, None);
        assert!(result.is_err(), "Duplicate task_id should be rejected");

        // upsert_result on same task_id should succeed (UPDATE + INSERT fallback)
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
        assert!(
            db.upsert_result(&tr).is_ok(),
            "upsert_result on existing task_id should work"
        );
    }

    #[test]
    fn test_timestamp_integer() {
        let db = setup_db();
        let now = Utc::now();
        let now_ts = now.timestamp();

        let task = Task::new("ts-test".to_string(), TaskType::Shell);
        db.insert_task(&task, None).unwrap();

        // Verify the stored created_at is an integer
        let stored: i64 = db
            .conn
            .query_row(
                "SELECT created_at FROM tasks WHERE task_id = ?1",
                params![task.task_id.to_string()],
                |row| row.get(0),
            )
            .unwrap();
        // Should be close to `now_ts` (within a few seconds)
        assert!(
            (stored - now_ts).abs() < 5,
            "created_at should be an INTEGER unix timestamp"
        );
    }

    #[test]
    fn test_task_meta_fields() {
        let db = setup_db();
        let task = Task::new("vllm test".to_string(), TaskType::Pytest);

        let meta = TaskMeta {
            task_group: Some("vllm-daily-regression".to_string()),
            git_commit: Some("a1b2c3d".to_string()),
            environment: Some(r#"{"gpu":"A100-80G","cuda":"12.4"}"#.to_string()),
            trigger: Some("cli".to_string()),
        };

        db.insert_task(&task, Some(&meta)).unwrap();

        // Verify the fields via SQLite directly
        let (task_group, git_commit, environment, trigger): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = db
            .conn
            .query_row(
                "SELECT task_group, git_commit, environment, trigger FROM tasks WHERE task_id = ?1",
                params![task.task_id.to_string()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(task_group, Some("vllm-daily-regression".to_string()));
        assert_eq!(git_commit, Some("a1b2c3d".to_string()));
        assert_eq!(
            environment,
            Some(r#"{"gpu":"A100-80G","cuda":"12.4"}"#.to_string())
        );
        assert_eq!(trigger, Some("cli".to_string()));
    }

    #[test]
    fn test_get_log_dir() {
        let mut db = setup_db();
        let task_id = Uuid::new_v4();

        // Before setting shared_storage, get_log_dir returns None
        assert!(db.get_log_dir(task_id).is_none());

        // After setting, returns the computed path
        db.set_shared_storage(PathBuf::from("/shared"));
        let log_dir = db
            .get_log_dir(task_id)
            .expect("get_log_dir should return Some");

        assert_eq!(
            log_dir,
            PathBuf::from("/shared/logs").join(task_id.to_string())
        );
        assert!(log_dir.ends_with(task_id.to_string()));
    }
}
