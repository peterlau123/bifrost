// Client status checking functionality
use crate::core::protocol::Protocol;
use crate::core::error::{BifrostError, Result};
use crate::core::models::{TaskStatus, TaskResult};
use uuid::Uuid;
use std::fs;

/// Status response for task queries
#[derive(Debug)]
pub struct StatusResponse {
    /// Task ID
    pub task_id: Uuid,
    /// Current status
    pub status: TaskStatus,
    /// Optional message
    pub message: Option<String>,
}

/// Query task status from status/ and results/ directories
pub fn query_status(protocol: &Protocol, task_id: Uuid) -> Result<StatusResponse> {
    let shared_storage = protocol.shared_storage();

    // First check if result exists (completed/failed)
    let results_dir = shared_storage.join("results");
    let result_file = results_dir.join(format!("{}_result.json", task_id));

    if result_file.exists() {
        // Task has completed
        let content = fs::read_to_string(&result_file)
            .map_err(BifrostError::IoError)?;

        let result: TaskResult = serde_json::from_str(&content)?;
        let status = result.status.clone();
        let duration = result.duration_secs();

        return Ok(StatusResponse {
            task_id,
            status,
            message: Some(format!("Task completed in {}s", duration)),
        });
    }

    // Check status directory for running/pending tasks
    let status_dir = shared_storage.join("status");
    let status_file = status_dir.join(format!("{}.json", task_id));

    if status_file.exists() {
        // Task is running or pending
        let content = fs::read_to_string(&status_file)
            .map_err(BifrostError::IoError)?;

        let status_data: serde_json::Value = serde_json::from_str(&content)?;

        let status_str = status_data.get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        let status = parse_status_string(status_str);

        let message = status_data.get("message")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());

        return Ok(StatusResponse {
            task_id,
            status,
            message,
        });
    }

    // Check if task exists in commands directory (pending)
    let task = protocol.read_task(&task_id);

    if task.is_ok() {
        return Ok(StatusResponse {
            task_id,
            status: TaskStatus::Pending,
            message: Some("Task is pending execution".to_string()),
        });
    }

    // Task not found anywhere
    Err(BifrostError::TaskNotFound(task_id))
}

/// Parse status string to TaskStatus enum
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

/// List all tasks with their statuses
pub fn list_all_statuses(protocol: &Protocol) -> Result<Vec<StatusResponse>> {
    let tasks = protocol.list_tasks()?;

    let statuses: Vec<StatusResponse> = tasks
        .iter()
        .filter_map(|task| {
            query_status(protocol, task.task_id).ok()
        })
        .collect();

    Ok(statuses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use crate::core::models::{Task, TaskOutput};
    use chrono::Utc;

    #[test]
    fn test_query_status_pending() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        // Submit a task
        let task = Task::new("echo test".to_string(), crate::core::models::TaskType::Shell);
        protocol.submit_task(&task).unwrap();

        // Query status - should be pending
        let status = query_status(&protocol, task.task_id).unwrap();
        assert_eq!(status.status, TaskStatus::Pending);
        assert!(status.message.unwrap().contains("pending"));
    }

    #[test]
    fn test_query_status_completed() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        // Submit a task
        let task = Task::new("echo test".to_string(), crate::core::models::TaskType::Shell);
        protocol.submit_task(&task).unwrap();

        // Create a result file
        let results_dir = temp_dir.path().join("results");
        fs::create_dir_all(&results_dir).unwrap();

        let result = TaskResult {
            task_id: task.task_id,
            status: TaskStatus::Completed,
            output: TaskOutput {
                stdout: "test".to_string(),
                stderr: "".to_string(),
                exit_code: Some(0),
            },
            start_time: Utc::now(),
            end_time: Utc::now(),
            retries_used: 0,
            artifacts: vec![],
            error_message: None,
        };

        let result_file = results_dir.join(format!("{}_result.json", task.task_id));
        fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

        // Query status - should be completed
        let status = query_status(&protocol, task.task_id).unwrap();
        assert_eq!(status.status, TaskStatus::Completed);
        assert!(status.message.unwrap().contains("completed"));
    }

    #[test]
    fn test_query_status_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let fake_id = Uuid::new_v4();
        let result = query_status(&protocol, fake_id);

        assert!(result.is_err());
    }

    #[test]
    fn test_parse_status_string() {
        assert_eq!(parse_status_string("pending"), TaskStatus::Pending);
        assert_eq!(parse_status_string("running"), TaskStatus::Running);
        assert_eq!(parse_status_string("completed"), TaskStatus::Completed);
        assert_eq!(parse_status_string("failed"), TaskStatus::Failed);
        assert_eq!(parse_status_string("timeout"), TaskStatus::Timeout);
        assert_eq!(parse_status_string("unknown"), TaskStatus::Pending);
    }
}