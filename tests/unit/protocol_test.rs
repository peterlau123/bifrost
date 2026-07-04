// Tests for file communication protocol
use bifrost::core::protocol::Protocol;
use bifrost::core::models::{Task, TaskType};
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;
use uuid::Uuid;

#[test]
fn test_submit_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf());

    let task = Task {
        task_id: Uuid::new_v4(),
        timestamp: Utc::now(),
        command: "test command".to_string(),
        task_type: TaskType::Shell,
        priority: 0,
        timeout: 300,
        retry_count: 3,
        env_vars: HashMap::new(),
        working_dir: PathBuf::from("/tmp"),
        artifacts_expected: vec![],
        metadata: HashMap::new(),
    };

    protocol.submit_task(&task).unwrap();

    // Verify commands directory was created
    assert!(temp_dir.path().join("commands").exists());

    // Verify task file was created with correct naming format
    let commands_dir = temp_dir.path().join("commands");
    let task_files: Vec<_> = std::fs::read_dir(&commands_dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();

    assert_eq!(task_files.len(), 1);

    let file_name = task_files[0].file_name().unwrap().to_str().unwrap();
    assert!(file_name.contains(&task.task_id.to_string()));
    assert!(file_name.ends_with(".json"));
}

#[test]
fn test_read_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf());

    let task = Task {
        task_id: Uuid::new_v4(),
        timestamp: Utc::now(),
        command: "read test".to_string(),
        task_type: TaskType::Shell,
        priority: 5,
        timeout: 60,
        retry_count: 1,
        env_vars: HashMap::new(),
        working_dir: PathBuf::from("/tmp"),
        artifacts_expected: vec![],
        metadata: HashMap::new(),
    };

    // Submit task first
    protocol.submit_task(&task).unwrap();

    // Read the task back
    let retrieved_task = protocol.read_task(&task.task_id).unwrap();

    assert_eq!(retrieved_task.task_id, task.task_id);
    assert_eq!(retrieved_task.command, task.command);
    assert_eq!(retrieved_task.task_type, task.task_type);
    assert_eq!(retrieved_task.priority, task.priority);
}

#[test]
fn test_read_task_not_found() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf());

    let non_existent_id = Uuid::new_v4();
    let result = protocol.read_task(&non_existent_id);

    assert!(result.is_err());
}

#[test]
fn test_protocol_creates_commands_dir() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf());

    // Protocol should create commands directory on initialization
    assert!(temp_dir.path().join("commands").exists());
}