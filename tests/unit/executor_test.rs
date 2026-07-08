// Unit tests for command executor

use bifrost::daemon::executor::Executor;
use bifrost::core::models::{Task, TaskType, TaskStatus};
use tempfile::TempDir;
use std::path::PathBuf;
use std::time::Duration;
use tokio::runtime::Runtime;

fn create_test_task(command: String) -> Task {
    Task::new(command, TaskType::Shell)
        .with_timeout(10)
        .with_working_dir(PathBuf::from("."))
}

#[test]
fn test_executor_new() {
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root.clone(), Duration::from_secs(30));
    assert!(executor.is_ok());
    assert!(log_root.exists());
}

#[test]
fn test_execute_shell_command() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root.clone(), Duration::from_secs(30)).unwrap();
    let task = create_test_task("echo 'Hello, Executor!'".to_string());

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    assert_eq!(result.output.exit_code, Some(0));
    assert!(result.output.stdout.contains("Hello, Executor!"));
    assert!(result.is_success());

    // Verify logs
    assert!(log_root.join(task.task_id.to_string()).exists());
    assert!(log_root.join(task.task_id.to_string()).join("stdout.log").exists());
}

#[test]
fn test_execute_failed_command() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();
    let task = create_test_task("exit 42".to_string());

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Failed);
    assert_eq!(result.output.exit_code, Some(42));
    assert!(!result.is_success());
}

#[test]
fn test_execute_timeout() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Sleep command with 2 second timeout
    let task = create_test_task("sleep 5".to_string())
        .with_timeout(2);

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Timeout);
    assert!(result.error_message.unwrap().contains("timed out"));
}

#[test]
fn test_stdout_truncation() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Python script to generate large output
    let task = create_test_task(
        "python -c \"print('X' * 2000)\"".to_string()
    );

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);

    // Check truncation (max 1000 chars + "...")
    assert!(result.output.stdout.len() <= 1003);
    assert!(result.output.stdout.ends_with("..."));
}

#[test]
fn test_execute_with_env_vars() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    let task = create_test_task("echo $TEST_VAR".to_string())
        .with_env_var("TEST_VAR".to_string(), "custom_value".to_string());

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    assert!(result.output.stdout.contains("custom_value"));
}

#[test]
fn test_execute_with_working_dir() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");
    let work_dir = temp_dir.path().join("work");
    fs::create_dir(&work_dir).unwrap();

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    let task = Task::new("pwd".to_string(), TaskType::Shell)
        .with_timeout(10)
        .with_working_dir(work_dir.clone());

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    // Working directory should appear in stdout
}

#[test]
fn test_execute_python_command() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    let task = Task::new(
        "python -c \"import sys; print('Python version:', sys.version)\"".to_string(),
        TaskType::Pytest,
    ).with_timeout(10);

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    assert!(result.output.stdout.contains("Python version"));
}

#[test]
fn test_executor_log_metadata() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root.clone(), Duration::from_secs(30)).unwrap();
    let task = create_test_task("echo 'test'".to_string());

    let result = rt.block_on(executor.execute(&task, None));
    assert!(result.is_ok());

    // Check metadata file
    let metadata_path = log_root.join(task.task_id.to_string()).join("metadata.json");
    assert!(metadata_path.exists());

    let metadata_content = std::fs::read_to_string(metadata_path).unwrap();
    let metadata: serde_json::Value = serde_json::from_str(&metadata_content).unwrap();

    assert_eq!(metadata["task_id"], task.task_id.to_string());
    assert!(metadata["start_time"].is_string());
    assert!(metadata["end_time"].is_string());
    assert!(metadata["duration_secs"].is_number());
    assert_eq!(metadata["exit_code"], 0);
}

#[test]
fn test_execute_with_gpu_injection() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();
    let task = create_test_task("echo $CUDA_VISIBLE_DEVICES".to_string());

    let result = rt.block_on(executor.execute_with_gpu(&task, 5));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    assert!(result.output.stdout.contains("5"));
}