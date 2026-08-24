// Shared-storage Bridge implementation
//
// Both the source (client) and target (server) machines read/write to the same
// filesystem. commands/ flows source -> target, results/ + status/ flow back.
use std::fs;
use std::path::{Path, PathBuf};

use crate::core::bridge::{Bridge, TaskStatusResponse};
use crate::core::error::{BifrostError, Result};
use crate::core::lock::atomic_write;
use crate::core::models::{Task, TaskResult, TaskStatus};
use uuid::Uuid;

/// Shared-storage transport implementing [`Bridge`].
///
/// Uses four directories under a shared root:
/// - `commands/`  - source writes, target reads
/// - `results/`   - target writes, source reads
/// - `status/`    - target writes progress updates
/// - `artifacts/` - execution artifacts
pub struct Protocol {
    /// Root directory for shared storage
    shared_storage: PathBuf,
    /// Directory for task commands
    commands_dir: PathBuf,
    /// Directory for task results
    results_dir: PathBuf,
    /// Directory for task status updates
    status_dir: PathBuf,
    /// Directory for task artifacts
    artifacts_dir: PathBuf,
}

impl Protocol {
    /// Create a new shared-storage bridge at the given root path.
    /// Creates all required subdirectories (commands, results, status, artifacts).
    pub fn new(shared_storage: PathBuf) -> Result<Self> {
        let commands_dir = shared_storage.join("commands");
        let results_dir = shared_storage.join("results");
        let status_dir = shared_storage.join("status");
        let artifacts_dir = shared_storage.join("artifacts");

        // Ensure all directories exist
        for dir in [&commands_dir, &results_dir, &status_dir, &artifacts_dir] {
            if !dir.exists() {
                fs::create_dir_all(dir).map_err(BifrostError::IoError)?;
            }
        }

        Ok(Self {
            shared_storage,
            commands_dir,
            results_dir,
            status_dir,
            artifacts_dir,
        })
    }

    /// Submit a task to the commands directory
    /// Writes task as JSON with filename format: {timestamp}_{task_id}.json
    pub fn submit_task(&self, task: &Task) -> Result<()> {
        // Ensure commands directory exists
        if !self.commands_dir.exists() {
            fs::create_dir_all(&self.commands_dir).map_err(BifrostError::IoError)?;
        }

        // Format filename with timestamp and task_id
        let timestamp_str = task.timestamp.format("%Y%m%d_%H%M%S");
        let filename = format!("{}_{}.json", timestamp_str, task.task_id);
        let filepath = self.commands_dir.join(filename);

        // Serialize task to JSON
        let json_content = serde_json::to_string_pretty(task)?;

        // Atomic write with file locking
        atomic_write(&filepath, json_content.as_bytes())?;

        Ok(())
    }

    /// Read a task by its ID from the commands directory
    pub fn read_task(&self, task_id: &Uuid) -> Result<Task> {
        // Scan commands directory for matching UUID
        if !self.commands_dir.exists() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }

        let entries: Vec<_> = fs::read_dir(&self.commands_dir)
            .map_err(BifrostError::IoError)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                // Only match .json task files, never .lock sidecars
                e.path()
                    .extension()
                    .map(|ext| ext == "json")
                    .unwrap_or(false)
                    && e.file_name()
                        .to_string_lossy()
                        .contains(&task_id.to_string())
            })
            .collect();

        if entries.is_empty() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }

        // Read the matching file
        let filepath = entries[0].path();
        let content = fs::read_to_string(&filepath).map_err(BifrostError::IoError)?;

        // Deserialize task
        let task: Task = serde_json::from_str(&content)?;

        Ok(task)
    }

    /// List all pending tasks in the commands directory
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        if !self.commands_dir.exists() {
            return Ok(Vec::new());
        }

        let tasks: Vec<Task> = fs::read_dir(&self.commands_dir)
            .map_err(BifrostError::IoError)?
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let content = fs::read_to_string(e.path()).ok()?;
                serde_json::from_str::<Task>(&content).ok()
            })
            .collect();

        Ok(tasks)
    }

    /// Delete a task file from the commands directory
    pub fn remove_task(&self, task_id: &Uuid) -> Result<()> {
        if !self.commands_dir.exists() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }

        let entries: Vec<_> = fs::read_dir(&self.commands_dir)
            .map_err(BifrostError::IoError)?
            .filter_map(|e| e.ok())
            .filter(|e| {
                let name = e.file_name().to_string_lossy().to_string();
                // 只删 .json 命令文件 + .lock 伴生文件；**绝不删 .processing**
                // claim marker——否则 watcher/scan 双通道的第二次 claim 会
                // 成功 → 读已删的 .json 失败 → write_failed_result 覆盖
                // 已完成的结果（2026-08-24 e2e job J1/J4 Failed parse 根因）。
                name.contains(&task_id.to_string()) && !name.ends_with(".processing")
            })
            .collect();

        if entries.is_empty() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }

        // Delete ALL files matching the task id (.json command + .lock sidecar).
        // The .lock file shares the same uuid in its name, so a naive
        // entries[0] deletion could remove the lock while leaving the task
        // JSON behind (observed under rapid submission).
        for entry in &entries {
            fs::remove_file(entry.path()).map_err(BifrostError::IoError)?;
        }

        Ok(())
    }

    /// Get the commands directory path
    pub fn commands_dir(&self) -> &Path {
        &self.commands_dir
    }

    /// Get the results directory path
    pub fn results_dir(&self) -> &Path {
        &self.results_dir
    }

    /// Get the status directory path
    pub fn status_dir(&self) -> &Path {
        &self.status_dir
    }

    /// Get the artifacts directory path
    pub fn artifacts_dir(&self) -> &Path {
        &self.artifacts_dir
    }

    /// Get the shared storage root path
    pub fn write_result(&self, task_id: &Uuid, result: &TaskResult) -> Result<()> {
        if !self.results_dir.exists() {
            fs::create_dir_all(&self.results_dir).map_err(BifrostError::IoError)?;
        }
        atomic_write(
            &self.results_dir.join(format!("{}_result.json", task_id)),
            serde_json::to_string_pretty(result)?.as_bytes(),
        )
    }
    pub fn write_status(
        &self,
        task_id: &Uuid,
        status: &TaskStatus,
        message: Option<&str>,
    ) -> Result<()> {
        if !self.status_dir.exists() {
            fs::create_dir_all(&self.status_dir).map_err(BifrostError::IoError)?;
        }
        let filepath = self.status_dir.join(format!("{}.json", task_id));
        let mut map = serde_json::Map::new();
        map.insert("task_id".into(), serde_json::json!(task_id.to_string()));
        map.insert("status".into(), serde_json::json!(format!("{}", status)));
        if let Some(msg) = message {
            map.insert("message".into(), serde_json::json!(msg));
        }
        atomic_write(&filepath, serde_json::to_string_pretty(&map)?.as_bytes())
    }
    pub fn remove_command_file(&self, task_id: &Uuid) -> Result<()> {
        if !self.commands_dir.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.commands_dir).map_err(BifrostError::IoError)? {
            let entry = entry.map_err(BifrostError::IoError)?;
            let name = entry.file_name().to_string_lossy().to_string();
            // 与 remove_task 同规则：不删 .processing claim marker
            if name.contains(&task_id.to_string()) && !name.ends_with(".processing") {
                fs::remove_file(entry.path()).map_err(BifrostError::IoError)?;
            }
        }
        Ok(())
    }

    pub fn shared_storage(&self) -> &Path {
        &self.shared_storage
    }

    /// Query task status by checking results/ then status/ then commands/.
    fn query_task_status(&self, task_id: &Uuid) -> Result<TaskStatusResponse> {
        // 1. Completed? check results/
        let result_file = self.results_dir.join(format!("{}_result.json", task_id));
        if result_file.exists() {
            let content = fs::read_to_string(&result_file).map_err(BifrostError::IoError)?;
            let result: TaskResult = serde_json::from_str(&content)?;
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status: result.status.clone(),
                message: Some(format!("Task completed in {}ms", result.duration_ms())),
            });
        }

        // 2. Running/pending? check status/
        let status_file = self.status_dir.join(format!("{}.json", task_id));
        if status_file.exists() {
            let content = fs::read_to_string(&status_file).map_err(BifrostError::IoError)?;
            let data: serde_json::Value = serde_json::from_str(&content)?;
            let status_str = data
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let status = parse_status_string(status_str);
            let message = data
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status,
                message,
            });
        }

        // 3. Pending? check commands/
        if self.read_task(task_id).is_ok() {
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status: TaskStatus::Pending,
                message: Some("Task is pending execution".to_string()),
            });
        }

        Err(BifrostError::TaskNotFound(*task_id))
    }

    /// Retrieve a completed task's result from results/.
    fn get_task_result(&self, task_id: &Uuid) -> Result<TaskResult> {
        let result_file = self.results_dir.join(format!("{}_result.json", task_id));
        if !result_file.exists() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }
        let content = fs::read_to_string(&result_file).map_err(BifrostError::IoError)?;
        let result: TaskResult = serde_json::from_str(&content)?;
        Ok(result)
    }
}

fn parse_status_string(s: &str) -> TaskStatus {
    match s.to_lowercase().as_str() {
        "pending" => TaskStatus::Pending,
        "running" => TaskStatus::Running,
        "completed" | "success" => TaskStatus::Completed,
        "failed" | "error" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "timeout" => TaskStatus::Timeout,
        _ => TaskStatus::Pending,
    }
}

impl Bridge for Protocol {
    fn submit_task(&self, task: &Task) -> Result<()> {
        Protocol::submit_task(self, task)
    }

    fn query_status(&self, task_id: &Uuid) -> Result<TaskStatusResponse> {
        self.query_task_status(task_id)
    }

    fn get_result(&self, task_id: &Uuid) -> Result<TaskResult> {
        self.get_task_result(task_id)
    }

    fn list_pending_tasks(&self) -> Result<Vec<Task>> {
        self.list_tasks()
    }

    fn read_task(&self, task_id: &Uuid) -> Result<Task> {
        Protocol::read_task(self, task_id)
    }

    fn write_result(&self, task_id: &Uuid, result: &TaskResult) -> Result<()> {
        Protocol::write_result(self, task_id, result)
    }

    fn write_status(
        &self,
        task_id: &Uuid,
        status: &TaskStatus,
        message: Option<&str>,
    ) -> Result<()> {
        Protocol::write_status(self, task_id, status, message)
    }

    fn remove_task(&self, task_id: &Uuid) -> Result<()> {
        Protocol::remove_task(self, task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn ts_prefix(tid: &Uuid) -> String {
        format!("20260824_120000_{}", tid)
    }

    /// remove_task 必须删除 .json + .lock 伴生文件，但**绝不删 .processing**
    /// claim marker——否则 watcher/scan 双通道第二次 claim 成功会覆盖结果
    /// （2026-08-24 e2e job J1/J4 Failed parse 根因回归测试）。
    #[test]
    fn test_remove_task_keeps_processing_marker() {
        let tmp = TempDir::new().unwrap();
        let p = Protocol::new(tmp.path().to_path_buf()).unwrap();
        let tid = Uuid::new_v4();
        let base = tmp.path().join("commands").join(ts_prefix(&tid));

        std::fs::write(base.with_extension("json"), "{}").unwrap();
        std::fs::write(base.with_extension("lock"), "").unwrap();
        std::fs::write(base.with_extension("processing"), "").unwrap();

        p.remove_task(&tid).unwrap();

        let remaining: Vec<String> = fs::read_dir(tmp.path().join("commands"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            remaining,
            vec![format!("{}.processing", ts_prefix(&tid))],
            "remove_task 只应删除 .json/.lock，.processing claim marker 必须保留"
        );
    }

    /// remove_command_file 同规则。
    #[test]
    fn test_remove_command_file_keeps_processing_marker() {
        let tmp = TempDir::new().unwrap();
        let p = Protocol::new(tmp.path().to_path_buf()).unwrap();
        let tid = Uuid::new_v4();
        let base = tmp.path().join("commands").join(ts_prefix(&tid));

        std::fs::write(base.with_extension("json"), "{}").unwrap();
        std::fs::write(base.with_extension("processing"), "").unwrap();

        p.remove_command_file(&tid).unwrap();

        let remaining: Vec<String> = fs::read_dir(tmp.path().join("commands"))
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(
            remaining,
            vec![format!("{}.processing", ts_prefix(&tid))],
            "remove_command_file 不应删 .processing claim marker"
        );
    }
}
