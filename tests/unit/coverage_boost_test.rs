// Additional unit tests for coverage improvement
// Tests edge cases, error handling, and boundary conditions

use bifrost::core::models::{Task, TaskType, TaskStatus, TaskResult, TaskOutput};
use bifrost::core::config::Config;
use bifrost::core::error::BifrostError;
use bifrost::core::protocol::Protocol;
use bifrost::daemon::watcher::Watcher;
use bifrost::daemon::executor::Executor;
use tempfile::TempDir;
use std::path::PathBuf;
use std::time::Duration;
use uuid::Uuid;
use chrono::{Utc, TimeZone};

// ==================== Models Edge Cases ====================

#[test]
fn test_task_priority_bounds() {
    // Test priority boundaries (0-255)
    let task_low = Task::new("cmd".to_string(), TaskType::Shell)
        .with_priority(0);
    assert_eq!(task_low.priority, 0);

    let task_high = Task::new("cmd".to_string(), TaskType::Shell)
        .with_priority(255);
    assert_eq!(task_high.priority, 255);

    let task_mid = Task::new("cmd".to_string(), TaskType::Shell)
        .with_priority(128);
    assert_eq!(task_mid.priority, 128);
}

#[test]
fn test_task_timeout_bounds() {
    // Test timeout boundaries
    let task_min = Task::new("cmd".to_string(), TaskType::Shell)
        .with_timeout(1);
    assert_eq!(task_min.timeout, 1);

    let task_max = Task::new("cmd".to_string(), TaskType::Shell)
        .with_timeout(86400); // 24 hours
    assert_eq!(task_max.timeout, 86400);
}

#[test]
fn test_task_retry_bounds() {
    // Test retry boundaries
    let task_no_retry = Task::new("cmd".to_string(), TaskType::Shell)
        .with_retry_count(0);
    assert_eq!(task_no_retry.retry_count, 0);

    let task_many_retry = Task::new("cmd".to_string(), TaskType::Shell)
        .with_retry_count(10);
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
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: None,
    };

    assert_eq!(result.duration_secs(), 330); // 5 minutes 30 seconds
}

// ==================== Config Edge Cases ====================

#[test]
fn test_config_default_values() {
    let config = Config::default();

    assert_eq!(config.shared_storage, PathBuf::from("/tmp/bifrost"));
    assert_eq!(config.log_level, "info");
    assert_eq!(config.poll_interval, 5);
    assert_eq!(config.max_concurrent_tasks, 4);
    assert_eq!(config.default_timeout, 3600);
}

#[test]
fn test_config_custom_values() {
    let config = Config {
        shared_storage: PathBuf::from("/custom/storage"),
        log_level: "debug".to_string(),
        poll_interval: 10,
        max_concurrent_tasks: 8,
        default_timeout: 7200,
    };

    assert_eq!(config.shared_storage, PathBuf::from("/custom/storage"));
    assert_eq!(config.log_level, "debug");
    assert_eq!(config.poll_interval, 10);
    assert_eq!(config.max_concurrent_tasks, 8);
    assert_eq!(config.default_timeout, 7200);
}

#[test]
fn test_config_yaml_serialization() {
    let config = Config {
        shared_storage: PathBuf::from("/tmp/test"),
        log_level: "warn".to_string(),
        poll_interval: 15,
        max_concurrent_tasks: 2,
        default_timeout: 1800,
    };

    let yaml = serde_yaml::to_string(&config).unwrap();
    assert!(yaml.contains("shared_storage: /tmp/test"));
    assert!(yaml.contains("log_level: warn"));
    assert!(yaml.contains("poll_interval: 15"));

    let deserialized: Config = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(deserialized.shared_storage, config.shared_storage);
    assert_eq!(deserialized.log_level, config.log_level);
}

#[test]
fn test_config_json_serialization() {
    let config = Config::default();

    let json = serde_json::to_string(&config).unwrap();
    assert!(json.contains("shared_storage"));
    assert!(json.contains("log_level"));

    let deserialized: Config = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.shared_storage, config.shared_storage);
}

// ==================== Error Handling Tests ====================

#[test]
fn test_bifrost_error_task_not_found() {
    let error = BifrostError::TaskNotFound(Uuid::new_v4());
    let error_string = error.to_string();
    assert!(error_string.contains("Task not found"));
}

#[test]
fn test_bifrost_error_protocol_error() {
    let error = BifrostError::ProtocolError("Connection failed".to_string());
    let error_string = error.to_string();
    assert!(error_string.contains("Protocol error"));
    assert!(error_string.contains("Connection failed"));
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
    let serde_error = serde_json::from_str::<Task>("invalid json").unwrap_err();
    let error = BifrostError::SerializationError(serde_error.to_string());
    assert!(error.to_string().contains("Serialization error"));
}

// ==================== Protocol Edge Cases ====================

#[test]
fn test_protocol_new_creates_directories() {
    let temp_dir = TempDir::new().unwrap();
    let storage = temp_dir.path().to_path_buf();

    let protocol = Protocol::new(storage.clone()).unwrap();

    assert!(storage.join("pending").exists());
    assert!(storage.join("results").exists());
    assert!(storage.join("completed").exists());
    assert!(storage.join("logs").exists());
}

#[test]
fn test_protocol_write_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let task = Task::new("test command".to_string(), TaskType::Shell);

    protocol.write_task(&task).unwrap();

    let task_file = temp_dir.path().join("pending").join(format!("{}.json", task.task_id));
    assert!(task_file.exists());

    let content = std::fs::read_to_string(task_file).unwrap();
    let loaded_task: Task = serde_json::from_str(&content).unwrap();
    assert_eq!(loaded_task.task_id, task.task_id);
    assert_eq!(loaded_task.command, task.command);
}

#[test]
fn test_protocol_read_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let task = Task::new("test".to_string(), TaskType::Shell);
    protocol.write_task(&task).unwrap();

    let loaded_task = protocol.read_task(task.task_id).unwrap();
    assert_eq!(loaded_task.task_id, task.task_id);
    assert_eq!(loaded_task.command, task.command);
}

#[test]
fn test_protocol_read_nonexistent_task() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let result = protocol.read_task(Uuid::new_v4());
    assert!(result.is_err());
}

#[test]
fn test_protocol_write_result() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let result = TaskResult {
        task_id: Uuid::new_v4(),
        status: TaskStatus::Completed,
        output: TaskOutput {
            stdout: "output".to_string(),
            stderr: String::new(),
            exit_code: Some(0),
        },
        start_time: Utc::now(),
        end_time: Utc::now(),
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: None,
    };

    protocol.write_result(&result).unwrap();

    let result_file = temp_dir.path().join("results").join(format!("{}.json", result.task_id));
    assert!(result_file.exists());
}

#[test]
fn test_protocol_move_to_completed() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let task = Task::new("test".to_string(), TaskType::Shell);
    protocol.write_task(&task).unwrap();

    protocol.move_to_completed(task.task_id).unwrap();

    // Pending file should be removed
    assert!(!temp_dir.path().join("pending").join(format!("{}.json", task.task_id)).exists());

    // Completed file should exist
    assert!(temp_dir.path().join("completed").join(format!("{}.json", task.task_id)).exists());
}

// ==================== Watcher Edge Cases ====================

#[test]
fn test_watcher_empty_pending() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    let watcher = Watcher::new(temp_dir.path().to_path_buf(), Duration::from_secs(1)).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let tasks = rt.block_on(watcher.scan_pending()).unwrap();

    assert_eq!(tasks.len(), 0);
}

#[test]
fn test_watcher_multiple_tasks() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    // Submit multiple tasks with different priorities
    let task1 = Task::new("cmd1".to_string(), TaskType::Shell).with_priority(10);
    let task2 = Task::new("cmd2".to_string(), TaskType::Shell).with_priority(5);
    let task3 = Task::new("cmd3".to_string(), TaskType::Shell).with_priority(15);

    protocol.write_task(&task1).unwrap();
    protocol.write_task(&task2).unwrap();
    protocol.write_task(&task3).unwrap();

    let watcher = Watcher::new(temp_dir.path().to_path_buf(), Duration::from_secs(1)).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let tasks = rt.block_on(watcher.scan_pending()).unwrap();

    assert_eq!(tasks.len(), 3);

    // Should be sorted by priority (ascending)
    assert_eq!(tasks[0].priority, 5);
    assert_eq!(tasks[1].priority, 10);
    assert_eq!(tasks[2].priority, 15);
}

#[test]
fn test_watcher_task_priority_ordering() {
    let temp_dir = TempDir::new().unwrap();
    let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

    // Submit tasks in random priority order
    let priorities = vec![100, 5, 50, 1, 25, 75, 10];
    for priority in priorities {
        let task = Task::new(format!("cmd-{}", priority), TaskType::Shell)
            .with_priority(priority);
        protocol.write_task(&task).unwrap();
    }

    let watcher = Watcher::new(temp_dir.path().to_path_buf(), Duration::from_secs(1)).unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let tasks = rt.block_on(watcher.scan_pending()).unwrap();

    // Verify sorted order
    let sorted_priorities: Vec<u8> = tasks.iter().map(|t| t.priority).collect();
    assert_eq!(sorted_priorities, vec![1, 5, 10, 25, 50, 75, 100]);
}

// ==================== Executor Edge Cases ====================

#[test]
fn test_executor_zero_timeout() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Task with zero timeout should fail immediately
    let task = Task::new("echo test".to_string(), TaskType::Shell)
        .with_timeout(0);

    let result = rt.block_on(executor.execute(&task)).unwrap();

    // Zero timeout should trigger timeout error
    assert_eq!(result.status, TaskStatus::Timeout);
}

#[test]
fn test_executor_empty_command() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Empty command should succeed (sh -c "" returns 0)
    let task = Task::new("".to_string(), TaskType::Shell)
        .with_timeout(5);

    let result = rt.block_on(executor.execute(&task)).unwrap();

    // Empty command should succeed or fail gracefully
    assert!(result.status == TaskStatus::Completed || result.status == TaskStatus::Failed);
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

    // Generate large stderr (more than 1000 chars)
    let task = Task::new(
        "sh -c 'for i in $(seq 1 100); do echo Error line $i >&2; done'".to_string(),
        TaskType::Shell,
    ).with_timeout(10);

    let result = rt.block_on(executor.execute(&task)).unwrap();

    assert_eq!(result.status, TaskStatus::Completed);
    // stderr is NOT truncated (only stdout is truncated)
    assert!(result.output.stderr.len() > 1000);
}

#[test]
fn test_executor_concurrent_tasks() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Execute multiple tasks concurrently
    let tasks: Vec<Task> = (0..5)
        .map(|i| Task::new(format!("echo task-{}", i), TaskType::Shell).with_timeout(5))
        .collect();

    let futures: Vec<_> = tasks.iter()
        .map(|task| executor.execute(task))
        .collect();

    let results = rt.block_on(futures::future::join_all(futures));

    // All should succeed
    for result in results {
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
    }
}