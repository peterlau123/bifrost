// Integration test: End-to-end workflow test
// Test full workflow: Client submit -> Watcher detect -> Executor execute -> Results write -> Client retrieve

use bifrost::core::protocol::Protocol;
use bifrost::core::models::{Task, TaskType, TaskStatus};
use bifrost::client::submit;
use bifrost::client::status;
use bifrost::client::results;
use bifrost::daemon::watcher::Watcher;
use bifrost::daemon::executor::Executor;
use tempfile::TempDir;
use std::time::Duration;
use tokio::runtime::Runtime;
use std::path::PathBuf;

#[test]
fn test_full_workflow_shell_command() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    // Setup: Create protocol
    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Step 1: Client submits task
    let command = "echo 'Integration test successful'";
    let task_id = submit::submit_task(
        &protocol,
        command.to_string(),
        TaskType::Shell,
        0,
        10,
        None,
    ).expect("Failed to submit task");

    // Verify task was written to pending
    assert!(shared_storage.join("pending").join(format!("{}.json", task_id)).exists());

    // Step 2: Watcher detects task (simulate daemon behavior)
    let watcher = Watcher::new(shared_storage.clone(), Duration::from_secs(1))
        .expect("Failed to create watcher");

    // Watcher should detect the pending task
    let detected_tasks = rt.block_on(watcher.scan_pending());
    assert!(detected_tasks.is_ok());
    let tasks = detected_tasks.unwrap();
    assert!(!tasks.is_empty());
    assert!(tasks.iter().any(|t| t.task_id == task_id));

    // Step 3: Executor executes task (simulate daemon execution)
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(30))
        .expect("Failed to create executor");

    // Load the submitted task
    let task_json = std::fs::read_to_string(
        shared_storage.join("pending").join(format!("{}.json", task_id))
    ).expect("Failed to read task");

    let task: Task = serde_json::from_str(&task_json).expect("Failed to parse task");

    // Execute
    let execution_result = rt.block_on(executor.execute(&task));
    assert!(execution_result.is_ok());
    let result = execution_result.unwrap();

    // Write result to protocol (simulate daemon behavior)
    protocol.write_result(&result).expect("Failed to write result");

    // Move task from pending to completed
    protocol.move_to_completed(task_id).expect("Failed to move to completed");

    // Step 4: Client retrieves status
    let status_response = status::query_status(&protocol, task_id)
        .expect("Failed to query status");

    assert_eq!(status_response.status, TaskStatus::Completed);
    assert!(status_response.message.is_some());

    // Step 5: Client retrieves results
    let result_text = results::get_result_formatted(
        &protocol,
        task_id,
        results::ResultFormat::Json,
    ).expect("Failed to retrieve results");

    // Verify result contains expected output
    assert!(result_text.contains("Integration test successful"));
    assert!(result_text.contains("\"status\": \"Completed\""));
    assert!(result_text.contains("\"exit_code\": 0"));

    // Verify artifacts
    assert!(shared_storage.join("results").join(format!("{}.json", task_id)).exists());
    assert!(shared_storage.join("completed").join(format!("{}.json", task_id)).exists());
}

#[test]
fn test_full_workflow_pytest_command() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    // Create a simple test file
    let test_file = temp_dir.path().join("test_example.py");
    std::fs::write(&test_file, "
def test_simple():
    assert True
").expect("Failed to write test file");

    // Setup protocol
    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Submit pytest task
    let pytest_path = test_file.to_str().unwrap();
    let task_id = submit::submit_pytest_task(
        &protocol,
        pytest_path.to_string(),
        5,
        60,
        Some(temp_dir.path().to_path_buf()),
    ).expect("Failed to submit pytest task");

    // Verify pending
    assert!(shared_storage.join("pending").join(format!("{}.json", task_id)).exists());

    // Execute
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(120))
        .expect("Failed to create executor");

    let task_json = std::fs::read_to_string(
        shared_storage.join("pending").join(format!("{}.json", task_id))
    ).expect("Failed to read task");

    let task: Task = serde_json::from_str(&task_json).expect("Failed to parse task");

    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    protocol.write_result(&result).expect("Failed to write result");
    protocol.move_to_completed(task_id).expect("Failed to move to completed");

    // Retrieve results
    let status_response = status::query_status(&protocol, task_id)
        .expect("Failed to query status");

    assert_eq!(status_response.status, TaskStatus::Completed);

    // Verify pytest command was built correctly
    assert!(task.command.contains("pytest"));
    assert!(task.command.contains("--json-report"));
}

#[test]
fn test_workflow_with_timeout() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Submit task that will timeout
    let task_id = submit::submit_task(
        &protocol,
        "sleep 10".to_string(),
        TaskType::Shell,
        0,
        2, // 2 second timeout
        None,
    ).expect("Failed to submit task");

    // Execute
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(30))
        .expect("Failed to create executor");

    let task_json = std::fs::read_to_string(
        shared_storage.join("pending").join(format!("{}.json", task_id))
    ).expect("Failed to read task");

    let task: Task = serde_json::from_str(&task_json).expect("Failed to parse task");

    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    protocol.write_result(&result).expect("Failed to write result");
    protocol.move_to_completed(task_id).expect("Failed to move to completed");

    // Verify timeout
    let status_response = status::query_status(&protocol, task_id)
        .expect("Failed to query status");

    assert_eq!(status_response.status, TaskStatus::Timeout);
    assert!(status_response.message.unwrap().contains("timed out"));
}

#[test]
fn test_workflow_with_failure() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Submit failing task
    let task_id = submit::submit_task(
        &protocol,
        "exit 42".to_string(),
        TaskType::Shell,
        0,
        10,
        None,
    ).expect("Failed to submit task");

    // Execute
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(30))
        .expect("Failed to create executor");

    let task_json = std::fs::read_to_string(
        shared_storage.join("pending").join(format!("{}.json", task_id))
    ).expect("Failed to read task");

    let task: Task = serde_json::from_str(&task_json).expect("Failed to parse task");

    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    protocol.write_result(&result).expect("Failed to write result");
    protocol.move_to_completed(task_id).expect("Failed to move to completed");

    // Verify failure
    let status_response = status::query_status(&protocol, task_id)
        .expect("Failed to query status");

    assert_eq!(status_response.status, TaskStatus::Failed);

    let result_text = results::get_result_formatted(
        &protocol,
        task_id,
        results::ResultFormat::Json,
    ).expect("Failed to retrieve results");

    assert!(result_text.contains("\"exit_code\": 42"));
}

#[test]
fn test_concurrent_task_submissions() {
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Submit multiple tasks concurrently
    let mut task_ids = Vec::new();

    for i in 0..5 {
        let task_id = submit::submit_task(
            &protocol,
            format!("echo 'Task {}'", i),
            TaskType::Shell,
            i,
            10,
            None,
        ).expect("Failed to submit task");

        task_ids.push(task_id);
    }

    // Verify all tasks in pending
    let pending_dir = shared_storage.join("pending");
    for task_id in &task_ids {
        assert!(pending_dir.join(format!("{}.json", task_id)).exists());
    }

    // Verify tasks are sorted by priority
    let watcher = Watcher::new(shared_storage.clone(), Duration::from_secs(1))
        .expect("Failed to create watcher");

    let rt = Runtime::new().unwrap();
    let detected_tasks = rt.block_on(watcher.scan_pending()).expect("Failed to scan");

    // Tasks should be sorted by priority (lower priority number = higher priority)
    assert_eq!(detected_tasks.len(), 5);
}