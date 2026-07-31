// Additional unit tests for coverage improvement
// Tests edge cases, error handling, and boundary conditions

use bifrost::core::error::BifrostError;
use bifrost::core::models::{Task, TaskOutput, TaskResult, TaskStatus, TaskType};
use bifrost::core::protocol::Protocol;
use bifrost::daemon::executor::Executor;
use bifrost::daemon::watcher::FileWatcher;
use chrono::{TimeZone, Utc};
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use uuid::Uuid;

// ==================== Models Edge Cases ====================

#[test]
fn test_task_priority_bounds() {
    // Test priority boundaries (0-255)
    let task_low = Task::new("cmd".to_string(), TaskType::Shell).with_priority(0);
    assert_eq!(task_low.priority, 0);

    let task_high = Task::new("cmd".to_string(), TaskType::Shell).with_priority(255);
    assert_eq!(task_high.priority, 255);

    let task_mid = Task::new("cmd".to_string(), TaskType::Shell).with_priority(128);
    assert_eq!(task_mid.priority, 128);
}

#[test]
fn test_task_timeout_bounds() {
    // Test timeout boundaries
    let task_min = Task::new("cmd".to_string(), TaskType::Shell).with_timeout(1);
    assert_eq!(task_min.timeout, 1);

    let task_max = Task::new("cmd".to_string(), TaskType::Shell).with_timeout(86400); // 24 hours
    assert_eq!(task_max.timeout, 86400);
}

#[test]
fn test_task_retry_bounds() {
    // Test retry boundaries
    let task_no_retry = Task::new("cmd".to_string(), TaskType::Shell).with_retry_count(0);
    assert_eq!(task_no_retry.retry_count, 0);

    let task_many_retry = Task::new("cmd".to_string(), TaskType::Shell).with_retry_count(10);
    assert_eq!(task_many_retry.retry_count, 10);
}

#[test]
fn test_task_type_serialization() {
    // Test all task types serialize correctly
    let types = vec![TaskType::Pytest, TaskType::Shell, TaskType::Custom];

    for task_type in types {
        let task = Task::new("cmd".to_string(), task_type.clone());
        let serialized = serde_json::to_string(&task).unwrap();
        let deserialized: Task = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.task_type, task_type);
    }
}

#[test]
fn test_task_status_serialization() {
    // Test all statuses serialize correctly
    let statuses = vec![
        TaskStatus::Pending,
        TaskStatus::Running,
        TaskStatus::Completed,
        TaskStatus::Failed,
        TaskStatus::Cancelled,
        TaskStatus::Timeout,
    ];

    for status in statuses {
        let result = TaskResult {
            task_id: Uuid::new_v4(),
            status: status.clone(),
            output: TaskOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: Some(0),
            },
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 0,
            retries_used: 0,
            artifacts: Vec::new(),
            error_message: None,
        };

        let serialized = serde_json::to_string(&result).unwrap();
        let deserialized: TaskResult = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized.status, status);
    }
}

#[test]
fn test_task_with_multiple_env_vars() {
    let task = Task::new("cmd".to_string(), TaskType::Shell)
        .with_env_var("VAR1".to_string(), "value1".to_string())
        .with_env_var("VAR2".to_string(), "value2".to_string())
        .with_env_var("VAR3".to_string(), "value3".to_string());

    assert_eq!(task.env_vars.len(), 3);
    assert_eq!(task.env_vars.get("VAR1"), Some(&"value1".to_string()));
    assert_eq!(task.env_vars.get("VAR2"), Some(&"value2".to_string()));
    assert_eq!(task.env_vars.get("VAR3"), Some(&"value3".to_string()));
}

#[test]
fn test_task_with_multiple_artifacts() {
    let task = Task::new("cmd".to_string(), TaskType::Shell)
        .with_artifact("report.json".to_string())
        .with_artifact("output.txt".to_string())
        .with_artifact("metrics.csv".to_string());

    assert_eq!(task.artifacts_expected.len(), 3);
    assert!(task.artifacts_expected.contains(&"report.json".to_string()));
    assert!(task.artifacts_expected.contains(&"output.txt".to_string()));
    assert!(task.artifacts_expected.contains(&"metrics.csv".to_string()));
}

#[test]
fn test_task_result_is_success() {
    // Test success case
    let success_result = TaskResult {
        task_id: Uuid::new_v4(),
        status: TaskStatus::Completed,
        output: TaskOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
        },
        start_time: Utc::now(),
        end_time: Utc::now(),
        duration_ms: 0,
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: None,
    };
    assert!(success_result.is_success());

    // Test failure cases
    let failure_statuses = vec![
        TaskStatus::Failed,
        TaskStatus::Timeout,
        TaskStatus::Cancelled,
        TaskStatus::Pending,
        TaskStatus::Running,
    ];

    for status in failure_statuses {
        let result = TaskResult {
            task_id: Uuid::new_v4(),
            status,
            output: TaskOutput {
                stdout: String::new(),
                stderr: String::new(),
                exit_code: None,
            },
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 0,
            retries_used: 0,
            artifacts: Vec::new(),
            error_message: None,
        };
        assert!(!result.is_success());
    }
}

#[test]
fn test_task_result_duration_calculation() {
    let start = Utc.with_ymd_and_hms(2026, 7, 4, 10, 0, 0).unwrap();
    let end = Utc.with_ymd_and_hms(2026, 7, 4, 10, 5, 30).unwrap();

    let result = TaskResult {
        task_id: Uuid::new_v4(),
        status: TaskStatus::Completed,
        output: TaskOutput {
            stdout: String::new(),
            stderr: String::new(),
            exit_code: Some(0),
        },
        start_time: start,
        end_time: end,
        duration_ms: 0,
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: None,
    };

    assert_eq!(result.duration_secs(), 330); // 5 minutes 30 seconds
}

// ==================== Error Handling Tests ====================

#[test]
fn test_bifrost_error_task_not_found() {
    let error = BifrostError::TaskNotFound(Uuid::new_v4());
    let error_string = error.to_string();
    assert!(error_string.contains("Task not found"));
}

#[test]
fn test_bifrost_error_io_error() {
    let io_error = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "Access denied");
    let error = BifrostError::from(io_error);
    let error_string = error.to_string();
    assert!(error_string.contains("Access denied"));
}

#[test]
fn test_bifrost_error_serialization_error() {
    let error = BifrostError::SerializationError("JSON parse failed".to_string());
    assert!(error.to_string().contains("Serialization error"));
}

#[test]
fn test_bifrost_error_config_invalid() {
    let error = BifrostError::ConfigInvalid("Invalid path".to_string());
    assert!(error.to_string().contains("Invalid configuration"));
}

// ==================== Protocol Edge Cases ====================

#[test]
fn test_protocol_new_creates_directories() {
    let temp_dir = TempDir::new().unwrap();
    let storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(storage.clone()).unwrap();

    assert!(storage.join("commands").exists());
    assert!(storage.join("results").exists());
    assert!(storage.join("status").exists());
    assert!(storage.join("artifacts").exists());
}

#[test]
fn test_protocol_submit_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let task = Task::new("test command".to_string(), TaskType::Shell);

    protocol.submit_task(&task).unwrap();

    // Task should exist in commands directory
    let tasks = protocol.list_tasks().unwrap();
    assert_eq!(tasks.len(), 1);
    assert_eq!(tasks[0].task_id, task.task_id);
}

#[test]
fn test_protocol_read_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let task = Task::new("test".to_string(), TaskType::Shell);
    protocol.submit_task(&task).unwrap();

    let loaded_task = protocol.read_task(&task.task_id).unwrap();
    assert_eq!(loaded_task.task_id, task.task_id);
    assert_eq!(loaded_task.command, task.command);
}

#[test]
fn test_protocol_read_nonexistent_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let result = protocol.read_task(&Uuid::new_v4());
    assert!(result.is_err());
}

#[test]
fn test_protocol_remove_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let task = Task::new("test".to_string(), TaskType::Shell);
    protocol.submit_task(&task).unwrap();

    protocol.remove_task(&task.task_id).unwrap();

    let result = protocol.read_task(&task.task_id);
    assert!(result.is_err());
}

// ==================== Watcher Edge Cases ====================

#[test]
fn test_watcher_new() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    std::fs::create_dir(&commands_dir).unwrap();

    let watcher = FileWatcher::new(commands_dir);
    assert!(watcher.is_ok());
}

#[test]
fn test_watcher_nonexistent_dir() {
    let watcher = FileWatcher::new(PathBuf::from("/nonexistent/path"));
    assert!(watcher.is_err());
}

#[test]
fn test_watcher_detect_new_file() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    std::fs::create_dir(&commands_dir).unwrap();

    let mut watcher = FileWatcher::new(commands_dir.clone()).unwrap();

    // Create a new JSON file
    let task_file = commands_dir.join("test_task.json");
    std::fs::write(&task_file, "{\"test\": \"data\"}").unwrap();

    // Wait for event propagation
    std::thread::sleep(Duration::from_millis(600));

    // Check for new file
    let result = watcher.wait_for_new_task();
    assert!(result.is_ok());

    watcher.stop().unwrap();
}

// ==================== Executor Edge Cases ====================

#[test]
fn test_executor_zero_timeout() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Task with zero timeout should fail immediately
    let task = Task::new("echo test".to_string(), TaskType::Shell).with_timeout(0);

    let result = rt.block_on(executor.execute(&task)).unwrap();

    // Zero timeout should trigger timeout error
    assert_eq!(result.status, TaskStatus::Timeout);
}

#[test]
fn test_executor_invalid_working_dir() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Invalid working directory
    let task = Task::new("pwd".to_string(), TaskType::Shell)
        .with_timeout(5)
        .with_working_dir(PathBuf::from("/nonexistent/path"));

    let result = rt.block_on(executor.execute(&task)).unwrap();

    // Should fail due to invalid working directory
    assert_eq!(result.status, TaskStatus::Failed);
    assert!(result.error_message.is_some());
}

#[test]
fn test_executor_large_stderr() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Generate large stderr using python (shell-words compatible)
    let task = Task::new(
        "python -c \"import sys; [sys.stderr.write(f'Error line {i}\\n') for i in range(100)]\""
            .to_string(),
        TaskType::Shell,
    )
    .with_timeout(10);

    let result = rt.block_on(executor.execute(&task)).unwrap();

    // stderr is NOT truncated (only stdout is truncated)
    assert!(result.output.stderr.len() > 1000 || result.status == TaskStatus::Failed);
}
