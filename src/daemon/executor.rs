// Generic command executor using tokio subprocess
// Handles timeout, stdout/stderr capture, and log file writing

use chrono::Utc;
use std::process::Stdio;
use tokio::process::Command;
use tokio::time::{timeout, Duration};

use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus};
use crate::daemon::logger::LogManager;

/// Command executor for running tasks
#[derive(Clone)]
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

        let task_timeout = Duration::from_secs(task.timeout);
        let effective_timeout = if task_timeout > self.default_timeout {
            self.default_timeout
        } else {
            task_timeout
        };

        // execute_command 内部处理超时并 kill 整个进程组
        let execution_result = self.execute_command(task, effective_timeout).await;

        let end_time = Utc::now();

        let result = match execution_result {
            Ok(output) => {
                self.log_manager
                    .write_stdout(task.task_id, &output.stdout)?;
                self.log_manager
                    .write_stderr(task.task_id, &output.stderr)?;
                self.log_manager.write_metadata(
                    task.task_id,
                    start_time,
                    end_time,
                    output.exit_code,
                    &task.command,
                )?;

                let status = if output.exit_code == Some(0) {
                    TaskStatus::Completed
                } else {
                    TaskStatus::Failed
                };

                TaskResult {
                    task_id: task.task_id,
                    status,
                    output,
                    command: task.command.clone(),
                    start_time,
                    end_time,
                    duration_ms: (end_time - start_time).num_milliseconds(),
                    retries_used: 0,
                    artifacts: Vec::new(),
                    error_message: None,
                }
            }
            Err(e) => {
                let error_msg = e;
                let timed_out = error_msg.contains("Task timed out");
                let output = TaskOutput {
                    stdout: String::new(),
                    stderr: error_msg.clone(),
                    exit_code: None,
                };
                self.log_manager.write_stderr(task.task_id, &error_msg)?;
                self.log_manager.write_metadata(
                    task.task_id,
                    start_time,
                    end_time,
                    None,
                    &task.command,
                )?;

                TaskResult {
                    task_id: task.task_id,
                    status: if timed_out {
                        TaskStatus::Timeout
                    } else {
                        TaskStatus::Failed
                    },
                    output,
                    command: task.command.clone(),
                    start_time,
                    end_time,
                    duration_ms: (end_time - start_time).num_milliseconds(),
                    retries_used: 0,
                    artifacts: Vec::new(),
                    error_message: Some(error_msg),
                }
            }
        };

        Ok(result)
    }

    /// Execute the actual command with safe parsing (no shell injection)
    /// Timeout handling: kills the whole process group so grandchildren
    /// (e.g. `sh -c '...'` children) don't leak as orphans.
    async fn execute_command(
        &self,
        task: &Task,
        effective_timeout: Duration,
    ) -> Result<TaskOutput, String> {
        let args = shell_words::split(&task.command)
            .map_err(|e| format!("Invalid command syntax: {}", e))?;

        if args.is_empty() {
            return Err("Empty command".to_string());
        }

        let mut cmd = Command::new(&args[0]);
        if args.len() > 1 {
            cmd.args(&args[1..]);
        }

        // Only set current_dir if task specifies a non-trivial path.
        // Default ("." or empty) means inherit daemon's cwd (set via set_current_dir).
        if !task.working_dir.as_os_str().is_empty() && task.working_dir != *"." {
            cmd.current_dir(&task.working_dir);
        }
        for (key, value) in &task.env_vars {
            cmd.env(key, value);
        }

        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Put the child in its own process group so a timeout can kill the
        // entire tree (child + grandchildren), not just the direct child.
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| format!("Failed to spawn process: {}", e))?;

        // Read stdout/stderr CONCURRENTLY with waiting for the child to exit.
        // Reading only AFTER wait() deadlocks on >64KB output: the child
        // blocks writing to a full pipe while we block waiting for exit.
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        let wait_result = timeout(effective_timeout, async {
            let stdout_data = match stdout_pipe {
                Some(p) => read_pipe_to_string(p, "stdout").await?,
                None => String::new(),
            };
            let stderr_data = match stderr_pipe {
                Some(p) => read_pipe_to_string(p, "stderr").await?,
                None => String::new(),
            };
            let status = child
                .wait()
                .await
                .map_err(|e| format!("Failed to wait for process: {}", e))?;
            Ok::<(std::process::ExitStatus, String, String), String>((
                status,
                stdout_data,
                stderr_data,
            ))
        })
        .await;

        let (status, stdout_data, stderr_data) = match wait_result {
            Ok(res) => res?,
            Err(_) => {
                // Kill the whole process group: prevents orphaned grandchildren
                // like `sh -c 'sleep 30'` whose sleep survives a plain child kill.
                #[cfg(unix)]
                unsafe {
                    if let Some(pid) = child.id() {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                // Reap the child so it doesn't become a zombie
                let _ = child.wait().await;
                return Err(format!(
                    "Task timed out after {} seconds",
                    effective_timeout.as_secs()
                ));
            }
        };

        // Truncate stdout/stderr to 1MB, safe at UTF-8 boundaries.
        // A naive &stdout_data[..N] panics when byte N splits a multi-byte
        // char (e.g. Chinese output) - which would lose the task result
        // entirely. 1MB holds full pytest output for normal runs while
        // bounding result-file size against pathological output.
        const MAX_OUTPUT_BYTES: usize = 1024 * 1024;
        let stdout_truncated = truncate_utf8(&stdout_data, MAX_OUTPUT_BYTES);
        let stderr_truncated = truncate_utf8(&stderr_data, MAX_OUTPUT_BYTES);

        Ok(TaskOutput {
            stdout: stdout_truncated,
            stderr: stderr_truncated,
            exit_code: status.code(),
        })
    }

    /// Get log manager reference
    pub fn log_manager(&self) -> &LogManager {
        &self.log_manager
    }

    /// Execute a task with GPU isolation by injecting CUDA_VISIBLE_DEVICES
    pub async fn execute_with_gpu(&self, task: &Task, gpu_id: u32) -> Result<TaskResult, String> {
        let mut task_with_gpu = task.clone();
        task_with_gpu
            .env_vars
            .insert("CUDA_VISIBLE_DEVICES".to_string(), gpu_id.to_string());
        self.execute(&task_with_gpu).await
    }
}

/// Truncate a UTF-8 string to at most `max_bytes` bytes without panicking
/// on multi-byte char boundaries. Appends "..." when truncation happened.
fn truncate_utf8(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }
    // find the largest char-boundary index <= max_bytes
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}...", &s[..end])
}

/// Drain a child pipe (stdout/stderr) fully into a String.
/// Generic over both ChildStdout and ChildStderr via AsyncRead.
async fn read_pipe_to_string<R>(mut pipe: R, what: &str) -> Result<String, String>
where
    R: tokio::io::AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let mut buf = String::new();
    pipe.read_to_string(&mut buf)
        .await
        .map_err(|e| format!("Failed to read {}: {}", what, e))?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn create_test_task() -> Task {
        Task::new(
            "echo 'Hello, World!'".to_string(),
            crate::core::models::TaskType::Shell,
        )
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
        assert!(log_root.join(task.task_id.to_string()).exists());
        assert!(log_root
            .join(task.task_id.to_string())
            .join("stdout.log")
            .exists());
    }

    #[tokio::test]
    async fn test_execute_failed_command() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();
        let task = Task::new(
            "sh -c 'exit 1'".to_string(),
            crate::core::models::TaskType::Shell,
        )
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
        let task =
            Task::new("sleep 10".to_string(), crate::core::models::TaskType::Shell).with_timeout(2);

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
        let task = Task::new(
            "python -c \"print('A' * 2000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5);

        let result = executor.execute(&task).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        // 2000 chars well under the 1MB cap: kept in full, no truncation
        assert!(
            result.output.stdout.contains("AAAA"),
            "stdout 应保留完整内容"
        );
        assert!(
            !result.output.stdout.ends_with("..."),
            "1MB 上限下 2000 字符不应截断"
        );
    }

    #[tokio::test]
    async fn test_stdout_truncation_multibyte_no_panic() {
        // 回归测试 Bug E: 中文输出超 1000 字节时, 旧代码 &s[..1000] 会 panic
        // (切片落在多字节字符中间), 导致任务结果丢失.
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();
        let task = Task::new(
            "python -c \"print('你好' * 500)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5);

        let result = executor.execute(&task).await;
        assert!(result.is_ok(), "中文超长输出不应 panic");
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
        // 1500 chars well under the 1MB cap: kept in full
        assert!(
            result.output.stdout.contains("你好"),
            "中文输出应保留完整内容"
        );
        assert!(!result.output.stdout.ends_with("..."));
        // 输出必须仍是合法 UTF-8 (没有半截字符)
        assert!(std::str::from_utf8(result.output.stdout.as_bytes()).is_ok());
    }

    #[test]
    fn test_truncate_utf8_boundary() {
        // 直接测 truncate_utf8 在边界的行为
        assert_eq!(truncate_utf8("hello", 100), "hello");
        assert_eq!(truncate_utf8("hello world", 5), "hello...");
        // 中文: 3 字节/字符, 1000 字节边界正好落在字符中间时必须回退
        let cn = "你".repeat(500); // 1500 字节
        let t = truncate_utf8(&cn, 1000);
        assert!(t.len() <= 1003);
        assert!(t.ends_with("..."));
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
        // 精确对齐边界时 (1000 = 333*3 + 1 → 回退到 999 = 333 个字符)
        assert!(t.starts_with(&"你".repeat(333)));
    }

    #[tokio::test]
    async fn test_stdout_truncation_over_1mb() {
        // 超出 1MB 上限时仍应安全截断 (不 panic, 合法 UTF-8)
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let executor = Executor::new(log_root, Duration::from_secs(60)).unwrap();
        let task = Task::new(
            "python3 -u -c \"import sys; sys.stdout.write('A' * 1100000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(60);

        let result = executor.execute(&task).await;
        assert!(result.is_ok(), "1MB 超长输出不应 panic");
        let result = result.unwrap();
        assert!(
            result.output.stdout.len() <= 1024 * 1024 + 4,
            "截断到 1MB 上限附近, 实际长度: {}",
            result.output.stdout.len()
        );
        assert!(
            result.output.stdout.ends_with("..."),
            "超限应截断, 实际长度: {}",
            result.output.stdout.len()
        );
        assert!(std::str::from_utf8(result.output.stdout.as_bytes()).is_ok());
    }

    #[tokio::test]
    async fn test_execute_with_env_vars() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let executor = Executor::new(log_root, Duration::from_secs(30)).unwrap();
        let task = Task::new(
            "sh -c 'echo $TEST_VAR'".to_string(),
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
