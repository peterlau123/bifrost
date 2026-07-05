// Generic command executor using tokio subprocess
// Handles timeout, stdout/stderr capture, and log file writing

use tokio::process::Command;
use tokio::time::{timeout, Duration};
use std::process::Stdio;
use uuid::Uuid;
use chrono::{DateTime, Utc};

use crate::core::models::{Task, TaskResult, TaskStatus, TaskOutput};
use crate::daemon::logger::LogManager;

/// Command executor for running tasks
pub struct Executor {
    log_manager: LogManager,
    default_timeout: Duration,
}

impl Executor {
    /// Create a new executor with log management
    pub fn new(log_root: std::path::PathBuf, default_timeout: Duration) -> Result<Self, String> {
        let log_manager = LogManager::new(log_root)?;

        Ok(Self {
            log_manager,
            default_timeout,
        })
    }

    /// Execute a task and return the result
    pub async fn execute(&self, task: &Task) -> Result<TaskResult, String> {
        let start_time = Utc::now();

        // Determine timeout
        let task_timeout = Duration::from_secs(task.timeout);
        let effective_timeout = if task_timeout > self.default_timeout {
            self.default_timeout
        } else {
            task_timeout
        };

        // Execute command with timeout
        let execution_result = timeout(
            effective_timeout,
            self.execute_command(task),
        ).await;

        let end_time = Utc::now();

        // Process execution result
        match execution_result {
            Ok(Ok(output)) => {
                // Write logs
                self.log_manager.write_stdout(task.task_id, &output.stdout)?;
                self.log_manager.write_stderr(task.task_id, &output.stderr)?;
                self.log_manager.write_metadata(
                    task.task_id,
                    start_time,
                    end_time,
                    output.exit_code,
                )?;

                // Determine status
                let status = if output.exit_code == Some(0) {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };

                Ok(TaskResult {
                    task_id: task.task_id,
                    status,
                    output,
                    start_time,
                    end_time,
                    retries_used: 0,
                    artifacts: Vec::new(),
                    error_message: None,
                })
            }

            Ok(Err(e)) => {
                // Execution error
                let error_msg = format!("Execution error: {}", e);

                let output = TaskOutput {
                    stdout: String::new(),
                    stderr: error_msg.clone(),
                    exit_code: None,
                };

                // Write error to stderr log
                self.log_manager.write_stderr(task.task_id, &error_msg)?;
                self.log_manager.write_metadata(
                    task.task_id,
                    start_time,
                    end_time,
                    None,
                )?;

                Ok(TaskResult {
                    task_id: task.task_id,
                    status: TaskStatus::Failed,
                    output,
                    start_time,
                    end_time,
                    retries_used: 0,
                    artifacts: Vec::new(),
                    error_message: Some(error_msg),
                })
            }

            Err(_) => {
                // Timeout
                let error_msg = format!("Task timed out after {} seconds", effective_timeout.as_secs());

                let output = TaskOutput {
                    stdout: String::new(),
                    stderr: error_msg.clone(),
                    exit_code: None,
                };

                // Write timeout to stderr log
                self.log_manager.write_stderr(task.task_id, &error_msg)?;
                self.log_manager.write_metadata(
                    task.task_id,
                    start_time,
                    end_time,
                    None,
                )?;

                Ok(TaskResult {
                    task_id: task.task_id,
                    status: TaskStatus::Timeout,
                    output,
                    start_time,
                    end_time,
                    retries_used: 0,
                    artifacts: Vec::new(),
                    error_message: Some(error_msg),
                })
            }
        }
    }

    /// Execute the actual command with safe parsing (no shell injection)
    async fn execute_command(&self, task: &Task) -> Result<TaskOutput, String> {
        // Parse command safely using shell-words to avoid shell injection
        let args = shell_words::split(&task.command)
            .map_err(|e| format!("Invalid command syntax: {}", e))?;

        if args.is_empty() {
            return Err("Empty command".to_string());
        }

        // Build command without shell interpolation
        let mut cmd = Command::new(&args[0]);
        if args.len() > 1 {
            cmd.args(&args[1..]);
        }

        // Set working directory
        cmd.current_dir(&task.working_dir);

        // Set environment variables
        for (key, value) in &task.env_vars {
            cmd.env(key, value);
        }

        // Capture stdout and stderr
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Spawn process
        let mut child = cmd.spawn()
            .map_err(|e| format!("Failed to spawn process: {}", e))?;

        // Wait for completion
        let status = child.wait().await
            .map_err(|e| format!("Failed to wait for process: {}", e))?;

        // Read stdout and stderr
        let stdout_data = if let Some(mut stdout) = child.stdout.take() {
            use tokio::io::AsyncReadExt;
            let mut buffer = String::new();
            stdout.read_to_string(&mut buffer).await
                .map_err(|e| format!("Failed to read stdout: {}", e))?;
            buffer
        } else {
            String::new()
        };

        let stderr_data = if let Some(mut stderr) = child.stderr.take() {
            use tokio::io::AsyncReadExt;
            let mut buffer = String::new();
            stderr.read_to_string(&mut buffer).await
                .map_err(|e| format!("Failed to read stderr: {}", e))?;
            buffer
        } else {
            String::new()
        };

        // Truncate stdout to 1000 chars
        let stdout_truncated = if stdout_data.len() > 1000 {
            format!("{}...", &stdout_data[..1000])
        } else {
            stdout_data
        };

        Ok(TaskOutput {
            stdout: stdout_truncated,
            stderr: stderr_data,
            exit_code: status.code(),
        })
    }

    /// Get log manager reference
    pub fn log_manager(&self) -> &LogManager {
        &self.log_manager
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::path::PathBuf;
    use std::collections::HashMap;

    fn create_test_task() -> Task {
        Task::new("echo 'Hello, World!'".to_string(), crate::core::models::TaskType::Shell)
            .with_timeout(5)
            .with_working_dir(PathBuf::from("."))
    }

    #[tokio::test]
    async fn test_execute_shell_command() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");

        let executor = Executor::new(log_root.clone(), Duration::from_secs(30)).unwrap();
        let task = create_test_task();

        let result = executor.execute(&task).await;

        assert!(result.is_ok());
        let result = result.unwrap();

        assert_eq!(result.task_id, task.task_id);
        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.output.stdout.contains("Hello, World!"));
        assert_eq!(result.output.exit_code, Some(0));
        assert!(result.is_success());

        // Verify logs were created
        assert!(log_root.join(task.task_id.to_string()).exists());
        assert!(log_root.join(task.task_id.to_string()).join("stdout.log").exists());
    }

    #[tokio::test]
    async fn test_execute_failed_command() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");

        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

        let task = Task::new("exit 1".to_string(), crate::core::models::TaskType::Shell)
            .with_timeout(5);

        let result = executor.execute(&task).await;

        assert!(result.is_ok());
        let result = result.unwrap();

        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.output.exit_code, Some(1));
        assert!(!result.is_success());
    }

    #[tokio::test]
    async fn test_execute_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");

        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

        // Command that sleeps for longer than timeout
        let task = Task::new("sleep 10".to_string(), crate::core::models::TaskType::Shell)
            .with_timeout(2);

        let result = executor.execute(&task).await;

        assert!(result.is_ok());
        let result = result.unwrap();

        assert_eq!(result.status, TaskStatus::Timeout);
        assert!(result.error_message.unwrap().contains("timed out"));
    }

    #[tokio::test]
    async fn test_stdout_truncation() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");

        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

        // Generate large output (more than 1000 chars)
        let task = Task::new(
            "python -c \"print('A' * 2000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        ).with_timeout(5);

        let result = executor.execute(&task).await;

        assert!(result.is_ok());
        let result = result.unwrap();

        // Check truncation
        assert!(result.output.stdout.len() <= 1003); // 1000 + "..."
        assert!(result.output.stdout.ends_with("..."));
    }

    #[tokio::test]
    async fn test_execute_with_env_vars() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");

        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();

        let task = Task::new(
            "echo $TEST_VAR".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5)
        .with_env_var("TEST_VAR".to_string(), "test_value".to_string());

        let result = executor.execute(&task).await;

        assert!(result.is_ok());
        let result = result.unwrap();

        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.output.stdout.contains("test_value"));
    }
}