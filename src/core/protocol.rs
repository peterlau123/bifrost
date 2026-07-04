// File communication protocol for task submission and retrieval
use std::path::{Path, PathBuf};
use std::fs;
use std::io;

use crate::core::models::Task;
use crate::core::lock::atomic_write;
use crate::core::error::{BifrostError, Result};
use uuid::Uuid;

/// File-based communication protocol for task management
pub struct Protocol {
    /// Root directory for shared storage
    shared_storage: PathBuf,
    /// Directory for task commands
    commands_dir: PathBuf,
}

impl Protocol {
    /// Create a new protocol instance with the given shared storage path
    pub fn new(shared_storage: PathBuf) -> Self {
        let commands_dir = shared_storage.join("commands");

        // Ensure commands directory exists
        if !commands_dir.exists() {
            fs::create_dir_all(&commands_dir).ok();
        }

        Self {
            shared_storage,
            commands_dir,
        }
    }

    /// Submit a task to the commands directory
    /// Writes task as JSON with filename format: {timestamp}_{task_id}.json
    pub fn submit_task(&self, task: &Task) -> Result<()> {
        // Ensure commands directory exists
        if !self.commands_dir.exists() {
            fs::create_dir_all(&self.commands_dir)
                .map_err(BifrostError::IoError)?;
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
                e.file_name()
                    .to_string_lossy()
                    .contains(&task_id.to_string())
            })
            .collect();

        if entries.is_empty() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }

        // Read the matching file
        let filepath = entries[0].path();
        let content = fs::read_to_string(&filepath)
            .map_err(BifrostError::IoError)?;

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
                e.file_name()
                    .to_string_lossy()
                    .contains(&task_id.to_string())
            })
            .collect();

        if entries.is_empty() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }

        // Delete the matching file
        fs::remove_file(entries[0].path())
            .map_err(BifrostError::IoError)?;

        Ok(())
    }

    /// Get the commands directory path
    pub fn commands_dir(&self) -> &Path {
        &self.commands_dir
    }

    /// Get the shared storage root path
    pub fn shared_storage(&self) -> &Path {
        &self.shared_storage
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use chrono::Utc;
    use std::collections::HashMap;

    fn create_test_task() -> Task {
        Task {
            task_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            command: "test command".to_string(),
            task_type: crate::core::models::TaskType::Shell,
            priority: 0,
            timeout: 300,
            retry_count: 3,
            env_vars: HashMap::new(),
            working_dir: PathBuf::from("/tmp"),
            artifacts_expected: vec![],
            metadata: HashMap::new(),
        }
    }

    #[test]
    fn test_protocol_new() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf());

        assert!(protocol.commands_dir().exists());
    }

    #[test]
    fn test_submit_and_read_task() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf());

        let task = create_test_task();
        protocol.submit_task(&task).unwrap();

        let retrieved = protocol.read_task(&task.task_id).unwrap();
        assert_eq!(retrieved.task_id, task.task_id);
        assert_eq!(retrieved.command, task.command);
    }

    #[test]
    fn test_list_tasks() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf());

        let task1 = create_test_task();
        let task2 = create_test_task();

        protocol.submit_task(&task1).unwrap();
        protocol.submit_task(&task2).unwrap();

        let tasks = protocol.list_tasks().unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn test_remove_task() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf());

        let task = create_test_task();
        protocol.submit_task(&task).unwrap();

        protocol.remove_task(&task.task_id).unwrap();

        let result = protocol.read_task(&task.task_id);
        assert!(result.is_err());
    }
}