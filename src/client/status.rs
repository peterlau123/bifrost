// Client status checking functionality
use crate::core::bridge::{Bridge, TaskStatusResponse};
use crate::core::error::Result;
use uuid::Uuid;

/// Query task status via the bridge
pub fn query_status(bridge: &dyn Bridge, task_id: Uuid) -> Result<TaskStatusResponse> {
    bridge.query_status(&task_id)
}

/// List all tasks with their statuses
pub fn list_all_statuses(bridge: &dyn Bridge) -> Result<Vec<TaskStatusResponse>> {
    let tasks = bridge.list_pending_tasks()?;
    let statuses: Vec<TaskStatusResponse> = tasks
        .iter()
        .filter_map(|task| bridge.query_status(&task.task_id).ok())
        .collect();
    Ok(statuses)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::protocol::Protocol;
    use tempfile::TempDir;
    use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus};
    use chrono::Utc;
    use std::fs;

    #[test]
    fn test_query_status_pending() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();
        let bridge: &dyn Bridge = &protocol;

        let task = Task::new("echo test".to_string(), crate::core::models::TaskType::Shell);
        protocol.submit_task(&task).unwrap();

        let status = query_status(bridge, task.task_id).unwrap();
        assert_eq!(status.status, TaskStatus::Pending);
        assert!(status.message.unwrap().contains("pending"));
    }

    #[test]
    fn test_query_status_completed() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();
        let bridge: &dyn Bridge = &protocol;

        let task = Task::new("echo test".to_string(), crate::core::models::TaskType::Shell);
        protocol.submit_task(&task).unwrap();

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
            duration_ms: 0,
            retries_used: 0,
            artifacts: vec![],
            error_message: None,
        };

        let result_file = results_dir.join(format!("{}_result.json", task.task_id));
        fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

        let status = query_status(bridge, task.task_id).unwrap();
        assert_eq!(status.status, TaskStatus::Completed);
        assert!(status.message.unwrap().contains("completed"));
    }

    #[test]
    fn test_query_status_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();
        let bridge: &dyn Bridge = &protocol;

        let fake_id = Uuid::new_v4();
        let result = query_status(bridge, fake_id);
        assert!(result.is_err());
    }
}