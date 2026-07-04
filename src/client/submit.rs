// Client submit functionality
use crate::core::models::{Task, TaskType};
use crate::core::protocol::Protocol;
use crate::core::error::Result;
use std::path::PathBuf;
use uuid::Uuid;

/// Submit a task to the daemon via shared storage
pub fn submit_task(
    protocol: &Protocol,
    command: String,
    task_type: TaskType,
    priority: u8,
    timeout: u64,
    working_dir: Option<PathBuf>,
) -> Result<Uuid> {
    // Create task
    let task = Task::new(command.clone(), task_type.clone())
        .with_priority(priority)
        .with_timeout(timeout);

    // Set working directory if provided
    let task = if let Some(wd) = working_dir {
        task.with_working_dir(wd)
    } else {
        task
    };

    // Get task ID before submission
    let task_id = task.task_id;

    // Submit via protocol
    protocol.submit_task(&task)?;

    Ok(task_id)
}

/// Create and submit a pytest task (convenience wrapper)
pub fn submit_pytest_task(
    protocol: &Protocol,
    test_path: String,
    priority: u8,
    timeout: u64,
    working_dir: Option<PathBuf>,
) -> Result<Uuid> {
    use crate::client::pytest::create_pytest_task;

    let task = create_pytest_task(&test_path, priority, timeout, working_dir);
    let task_id = task.task_id;

    protocol.submit_task(&task)?;

    Ok(task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_submit_task() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = submit_task(
            &protocol,
            "echo hello".to_string(),
            TaskType::Shell,
            10,
            300,
            None,
        ).unwrap();

        // Verify task was created
        assert!(!task_id.is_nil());

        // Verify task file exists
        let commands_dir = temp_dir.path().join("commands");
        let files: Vec<_> = std::fs::read_dir(&commands_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        assert_eq!(files.len(), 1);
        assert!(files[0].file_name().to_string_lossy().contains(&task_id.to_string()));
    }

    #[test]
    fn test_submit_task_with_working_dir() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = submit_task(
            &protocol,
            "pytest tests/".to_string(),
            TaskType::Pytest,
            5,
            600,
            Some(PathBuf::from("/workspace")),
        ).unwrap();

        // Read back the task to verify working_dir was set
        let task = protocol.read_task(&task_id).unwrap();
        assert_eq!(task.working_dir, PathBuf::from("/workspace"));
    }

    #[test]
    fn test_submit_pytest_task() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = submit_pytest_task(
            &protocol,
            "tests/unit/".to_string(),
            10,
            300,
            None,
        ).unwrap();

        // Read back the task
        let task = protocol.read_task(&task_id).unwrap();

        assert_eq!(task.task_type, TaskType::Pytest);
        assert!(task.command.contains("--json-report"));
        assert!(task.artifacts_expected.contains(&"report.json".to_string()));
    }
}