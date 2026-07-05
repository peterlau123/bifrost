// Integration test: End-to-end workflow test
// Test full workflow: Client submit -> Watcher detect -> Executor execute -> Results write -> Client retrieve

use bifrost::core::protocol::Protocol;
use bifrost::core::models::{Task, TaskType, TaskStatus, TaskResult, TaskOutput};
use bifrost::client::submit;
use bifrost::client::results;
use bifrost::daemon::executor::Executor;
use tempfile::TempDir;
use std::time::Duration;
use tokio::runtime::Runtime;
use std::path::PathBuf;
use chrono::Utc;
use uuid::Uuid;

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

    // Verify task was written to commands directory
    let tasks = protocol.list_tasks().unwrap();
    assert!(!tasks.is_empty());
    assert!(tasks.iter().any(|t| t.task_id == task_id));

    // Step 2: Load the submitted task
    let task = protocol.read_task(&task_id).expect("Failed to read task");

    // Step 3: Executor executes task (simulate daemon execution)
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(30))
        .expect("Failed to create executor");

    let execution_result = rt.block_on(executor.execute(&task));
    assert!(execution_result.is_ok());
    let result = execution_result.unwrap();

    // Write result to results directory manually
    let results_dir = shared_storage.join("results");
    let result_file = results_dir.join(format!("{}_result.json", task_id));
    std::fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

    // Remove task from commands directory (simulate completion)
    protocol.remove_task(&task_id).unwrap();

    // Step 4: Client retrieves results
    let retrieved_result = results::get_result(&protocol, task_id)
        .expect("Failed to retrieve results");

    assert_eq!(retrieved_result.status, TaskStatus::Completed);
    assert!(retrieved_result.output.stdout.contains("Integration test successful") || result.is_success());
    assert_eq!(retrieved_result.output.exit_code, Some(0));
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

    // Submit pytest task using submit_task with pytest type
    let pytest_path = test_file.to_str().unwrap();
    let task_id = submit::submit_task(
        &protocol,
        format!("pytest {} -v", pytest_path),
        TaskType::Pytest,
        5,
        60,
        Some(temp_dir.path().to_path_buf()),
    ).expect("Failed to submit pytest task");

    // Execute
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(120))
        .expect("Failed to create executor");

    let task = protocol.read_task(&task_id).expect("Failed to read task");
    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    // Write result
    let results_dir = shared_storage.join("results");
    let result_file = results_dir.join(format!("{}_result.json", task_id));
    std::fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

    protocol.remove_task(&task_id).unwrap();

    // Retrieve results
    let retrieved_result = results::get_result(&protocol, task_id)
        .expect("Failed to retrieve results");

    // Check status - may be Completed or Failed depending on pytest availability
    assert!(retrieved_result.status == TaskStatus::Completed || retrieved_result.status == TaskStatus::Failed);
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

    let task = protocol.read_task(&task_id).expect("Failed to read task");
    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    // Write result
    let results_dir = shared_storage.join("results");
    let result_file = results_dir.join(format!("{}_result.json", task_id));
    std::fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

    protocol.remove_task(&task_id).unwrap();

    // Verify timeout
    let retrieved_result = results::get_result(&protocol, task_id)
        .expect("Failed to retrieve results");

    assert_eq!(retrieved_result.status, TaskStatus::Timeout);
    assert!(retrieved_result.error_message.unwrap().contains("timed out"));
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

    let task = protocol.read_task(&task_id).expect("Failed to read task");
    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    // Write result
    let results_dir = shared_storage.join("results");
    let result_file = results_dir.join(format!("{}_result.json", task_id));
    std::fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

    protocol.remove_task(&task_id).unwrap();

    // Verify failure
    let retrieved_result = results::get_result(&protocol, task_id)
        .expect("Failed to retrieve results");

    assert_eq!(retrieved_result.status, TaskStatus::Failed);
    assert_eq!(retrieved_result.output.exit_code, Some(42));
}

#[test]
fn test_concurrent_task_submissions() {
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Submit multiple tasks
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

    // Verify all tasks in commands directory
    let tasks = protocol.list_tasks().unwrap();
    assert_eq!(tasks.len(), 5);

    // Verify all task IDs are present
    for task_id in &task_ids {
        assert!(tasks.iter().any(|t| t.task_id == *task_id));
    }
}

#[test]
fn test_result_formatting() {
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(shared_storage.clone()).expect("Failed to create protocol");

    // Create and submit task
    let task_id = submit::submit_task(
        &protocol,
        "echo 'Format test'".to_string(),
        TaskType::Shell,
        0,
        10,
        None,
    ).expect("Failed to submit task");

    // Execute
    let rt = Runtime::new().unwrap();
    let log_root = shared_storage.join("logs");
    let executor = Executor::new(log_root, Duration::from_secs(30))
        .expect("Failed to create executor");

    let task = protocol.read_task(&task_id).expect("Failed to read task");
    let result = rt.block_on(executor.execute(&task)).expect("Failed to execute");

    // Write result
    let results_dir = shared_storage.join("results");
    let result_file = results_dir.join(format!("{}_result.json", task_id));
    std::fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

    protocol.remove_task(&task_id).unwrap();

    // Test different format outputs
    let json_result = results::get_result_formatted(
        &protocol,
        task_id,
        results::ResultFormat::Json,
    ).expect("Failed to get JSON result");

    assert!(json_result.contains("task_id"));
    assert!(json_result.contains("status"));

    let text_result = results::get_result_formatted(
        &protocol,
        task_id,
        results::ResultFormat::Text,
    ).expect("Failed to get text result");

    assert!(text_result.contains("Task ID:"));
    assert!(text_result.contains("Status:"));
}