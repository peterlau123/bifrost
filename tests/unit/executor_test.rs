// Unit tests for command executor

use bifrost::core::models::{Task, TaskStatus, TaskType};
use bifrost::daemon::executor::Executor;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
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

    let result = rt.block_on(executor.execute(&task));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    assert_eq!(result.output.exit_code, Some(0));
    assert!(result.output.stdout.contains("Hello, Executor!"));
    assert!(result.is_success());

    // Verify logs
    assert!(log_root.join(task.task_id.to_string()).exists());
    assert!(log_root
        .join(task.task_id.to_string())
        .join("stdout.log")
        .exists());
}

#[test]
fn test_execute_failed_command() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();
    let task = create_test_task("exit 42".to_string());

    let result = rt.block_on(executor.execute(&task));
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
    let task = create_test_task("sleep 5".to_string()).with_timeout(2);

    let result = rt.block_on(executor.execute(&task));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Timeout);
    assert!(result.error_message.unwrap().contains("timed out"));
}

#[test]
fn test_stdout_not_truncated() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    // Python script to generate large output
    let task = create_test_task("head -c 2000 /dev/zero | tr '\\000' X".to_string());

    let result = rt.block_on(executor.execute(&task));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);

    // 现在 stdout 不再截断（见 src 内联 test_9mb_output_not_truncated），完整保留 2000 个 X
    let stdout = &result.output.stdout;
    assert_eq!(stdout.trim_end().len(), 2000, "stdout 应完整保留");
    assert!(!stdout.ends_with("..."), "stdout 不应被截断加省略号");
}

#[test]
fn test_execute_with_env_vars() {
    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    let task = create_test_task("echo $TEST_VAR".to_string())
        .with_env_var("TEST_VAR".to_string(), "custom_value".to_string());

    let result = rt.block_on(executor.execute(&task));
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

    let result = rt.block_on(executor.execute(&task));
    assert!(result.is_ok());

    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    // Working directory should appear in stdout
}

#[test]
fn test_execute_python_command() {
    // 这个用例真的在验证"执行 python 命令"，所以保留 python 依赖；
    // 但 CI 镜像（Gitee Go 的 build@rust）不一定有 python，缺了就跳过而不是红
    let has_python = std::process::Command::new("python")
        .arg("-c")
        .arg("pass")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !has_python {
        eprintln!("skip test_execute_python_command: 环境里没有 python，跳过");
        return;
    }

    let rt = Runtime::new().unwrap();
    let temp_dir = TempDir::new().unwrap();
    let log_root = temp_dir.path().join("logs");

    let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

    let task = Task::new(
        "python -c \"import sys; print('Python version:', sys.version)\"".to_string(),
        TaskType::Pytest,
    )
    .with_timeout(10);

    let result = rt.block_on(executor.execute(&task));
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

    let result = rt.block_on(executor.execute(&task));
    assert!(result.is_ok());

    // Check metadata file
    let metadata_path = log_root
        .join(task.task_id.to_string())
        .join("metadata.json");
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
