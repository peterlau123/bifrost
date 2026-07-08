// Log file management for task execution
// Creates logs/{task_id}/ directory structure for stdout/stderr

use std::path::{Path, PathBuf};
use std::fs;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Log manager for task execution output
#[derive(Clone)]
pub struct LogManager {
    log_root: PathBuf,
}

impl LogManager {
    /// Create a new log manager with specified root directory
    pub fn new(log_root: PathBuf) -> Result<Self, String> {
        // Ensure log root exists
        if !log_root.exists() {
            fs::create_dir_all(&log_root)
                .map_err(|e| format!("Failed to create log root: {}", e))?;
        }

        Ok(Self { log_root })
    }

    /// Create log directory for a specific task
    /// Returns the path to the task's log directory
    pub fn create_task_log_dir(&self, task_id: Uuid) -> Result<PathBuf, String> {
        let task_log_dir = self.log_root.join(task_id.to_string());

        fs::create_dir_all(&task_log_dir)
            .map_err(|e| format!("Failed to create task log dir: {}", e))?;

        Ok(task_log_dir)
    }

    /// Get paths for stdout and stderr log files
    pub fn get_log_paths(&self, task_id: Uuid) -> Result<(PathBuf, PathBuf), String> {
        let task_log_dir = self.create_task_log_dir(task_id)?;

        let stdout_path = task_log_dir.join("stdout.log");
        let stderr_path = task_log_dir.join("stderr.log");

        Ok((stdout_path, stderr_path))
    }

    /// Write stdout content to log file
    pub fn write_stdout(&self, task_id: Uuid, content: &str) -> Result<PathBuf, String> {
        let (stdout_path, _) = self.get_log_paths(task_id)?;

        fs::write(&stdout_path, content)
            .map_err(|e| format!("Failed to write stdout: {}", e))?;

        Ok(stdout_path)
    }

    /// Write stderr content to log file
    pub fn write_stderr(&self, task_id: Uuid, content: &str) -> Result<PathBuf, String> {
        let (_, stderr_path) = self.get_log_paths(task_id)?;

        fs::write(&stderr_path, content)
            .map_err(|e| format!("Failed to write stderr: {}", e))?;

        Ok(stderr_path)
    }

    /// Write execution metadata to log file
    pub fn write_metadata(
        &self,
        task_id: Uuid,
        start_time: DateTime<Utc>,
        end_time: DateTime<Utc>,
        exit_code: Option<i32>,
    ) -> Result<PathBuf, String> {
        let task_log_dir = self.create_task_log_dir(task_id)?;
        let metadata_path = task_log_dir.join("metadata.json");

        let metadata = serde_json::json!({
            "task_id": task_id.to_string(),
            "start_time": start_time.to_rfc3339(),
            "end_time": end_time.to_rfc3339(),
            "duration_secs": (end_time - start_time).num_seconds(),
            "exit_code": exit_code,
        });

        let content = serde_json::to_string_pretty(&metadata)
            .map_err(|e| format!("Failed to serialize metadata: {}", e))?;

        fs::write(&metadata_path, content)
            .map_err(|e| format!("Failed to write metadata: {}", e))?;

        Ok(metadata_path)
    }

    /// Get the log root directory
    pub fn log_root(&self) -> &Path {
        &self.log_root
    }

    /// Read stdout log content
    pub fn read_stdout(&self, task_id: Uuid) -> Result<String, String> {
        let (stdout_path, _) = self.get_log_paths(task_id)?;

        fs::read_to_string(&stdout_path)
            .map_err(|e| format!("Failed to read stdout: {}", e))
    }

    /// Read stderr log content
    pub fn read_stderr(&self, task_id: Uuid) -> Result<String, String> {
        let (_, stderr_path) = self.get_log_paths(task_id)?;

        fs::read_to_string(&stderr_path)
            .map_err(|e| format!("Failed to read stderr: {}", e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_log_manager_new() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");

        let manager = LogManager::new(log_root.clone());
        assert!(manager.is_ok());
        assert!(log_root.exists());
    }

    #[test]
    fn test_create_task_log_dir() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let manager = LogManager::new(log_root).unwrap();

        let task_id = Uuid::new_v4();
        let task_log_dir = manager.create_task_log_dir(task_id);

        assert!(task_log_dir.is_ok());
        let dir = task_log_dir.unwrap();
        assert!(dir.exists());
        assert_eq!(dir.file_name().unwrap().to_string_lossy(), task_id.to_string());
    }

    #[test]
    fn test_write_and_read_stdout() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let manager = LogManager::new(log_root).unwrap();

        let task_id = Uuid::new_v4();
        let content = "Test stdout content\nLine 2\nLine 3";

        let write_result = manager.write_stdout(task_id, content);
        assert!(write_result.is_ok());

        let read_result = manager.read_stdout(task_id);
        assert!(read_result.is_ok());
        assert_eq!(read_result.unwrap(), content);
    }

    #[test]
    fn test_write_stderr() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let manager = LogManager::new(log_root).unwrap();

        let task_id = Uuid::new_v4();
        let content = "Error: something went wrong";

        let write_result = manager.write_stderr(task_id, content);
        assert!(write_result.is_ok());

        let read_result = manager.read_stderr(task_id);
        assert!(read_result.is_ok());
        assert_eq!(read_result.unwrap(), content);
    }

    #[test]
    fn test_write_metadata() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let manager = LogManager::new(log_root).unwrap();

        let task_id = Uuid::new_v4();
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(5);

        let metadata_path = manager.write_metadata(task_id, start, end, Some(0));
        assert!(metadata_path.is_ok());

        // Read and verify metadata
        let content = fs::read_to_string(metadata_path.unwrap()).unwrap();
        let metadata: serde_json::Value = serde_json::from_str(&content).unwrap();

        assert_eq!(metadata["task_id"], task_id.to_string());
        assert_eq!(metadata["duration_secs"], 5);
        assert_eq!(metadata["exit_code"], 0);
    }
}