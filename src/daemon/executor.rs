// Generic command executor using tokio subprocess.
//
// Handles: shell invocation, per-task timeout (0 = inherit daemon default),
// cancellation signal files, retries, artifact collection, stdout/stderr
// capture and log writing.

use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncRead;
use tokio::process::Command;
use tokio::time::{sleep, timeout, Duration};
use uuid::Uuid;

use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus};
use crate::daemon::logger::LogManager;

/// Result-file output cap. Full logs stay on disk under logs/<task_id>/.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Daemon-wide default when neither the task nor settings specify a timeout.
pub const DEFAULT_TIMEOUT_SECS: u64 = 5400;
/// How often a running task checks for a cancel request file.
const CANCEL_POLL: Duration = Duration::from_millis(300);

/// Tunables for the executor, derived from daemon settings.
#[derive(Clone, Debug)]
pub struct ExecutorOptions {
    /// Used when `task.timeout == 0`.
    pub default_timeout: Duration,
    /// Wrap the command in `sh -c` so shell syntax works.
    pub use_shell: bool,
    /// Upper bound on retries (task.retry_count is capped by this).
    pub max_retries: u8,
    /// Directory where cancel request files (`<task_id>.json`) are written.
    pub cancel_dir: Option<PathBuf>,
    /// Where declared artifacts are copied (`<task_id>_<name>`).
    pub artifacts_dir: Option<PathBuf>,
}

impl Default for ExecutorOptions {
    fn default() -> Self {
        Self {
            default_timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            use_shell: true,
            max_retries: 3,
            cancel_dir: None,
            artifacts_dir: None,
        }
    }
}

/// Why a single execution attempt did not produce output.
enum RunError {
    TimedOut(Duration),
    Cancelled,
    Spawn(String),
}

/// Command executor for running tasks
#[derive(Clone)]
pub struct Executor {
    log_manager: LogManager,
    opts: ExecutorOptions,
}

impl Executor {
    /// Create a new executor with log management (legacy signature).
    pub fn new(log_root: std::path::PathBuf, default_timeout: Duration) -> Result<Self, String> {
        Self::with_options(
            log_root,
            ExecutorOptions {
                default_timeout,
                ..Default::default()
            },
        )
    }

    /// Create an executor with full options.
    pub fn with_options(
        log_root: std::path::PathBuf,
        opts: ExecutorOptions,
    ) -> Result<Self, String> {
        let log_manager = LogManager::new(log_root)?;
        Ok(Self { log_manager, opts })
    }

    /// Per-task timeout; 0 means "inherit the daemon default".
    fn effective_timeout(&self, task: &Task) -> Duration {
        match task.timeout {
            0 => self.opts.default_timeout,
            secs => Duration::from_secs(secs),
        }
    }

    /// Execute a task and return the result, retrying transient failures.
    pub async fn execute(&self, task: &Task) -> Result<TaskResult, String> {
        let limit = self.effective_timeout(task);
        let max_attempts = task
            .retry_count
            .min(self.opts.max_retries)
            .saturating_add(1);
        let mut attempt: u8 = 0;

        loop {
            attempt += 1;
            let start_time = Utc::now();
            let outcome = self.run_once(task, limit).await;
            let end_time = Utc::now();

            match outcome {
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

                    // Retry only hard failures (non-zero exit); timeouts and
                    // cancellations are terminal by definition.
                    if status == TaskStatus::Failed && attempt < max_attempts {
                        eprintln!(
                            "task {} failed (exit {:?}), retry {}/{}",
                            task.task_id,
                            output.exit_code,
                            attempt,
                            max_attempts - 1
                        );
                        continue;
                    }

                    let artifacts = self.collect_artifacts(task);
                    self.clear_cancel(task.task_id);
                    return Ok(TaskResult {
                        task_id: task.task_id,
                        status,
                        output,
                        command: task.command.clone(),
                        start_time,
                        end_time,
                        duration_ms: (end_time - start_time).num_milliseconds(),
                        retries_used: attempt - 1,
                        artifacts,
                        error_message: None,
                    });
                }
                Err(RunError::TimedOut(d)) => {
                    let msg = format!("Task timed out after {} seconds", d.as_secs());
                    self.finish_error(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Timeout,
                        msg,
                        attempt,
                    )?;
                    return Ok(self.error_result(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Timeout,
                        format!("Task timed out after {} seconds", d.as_secs()),
                        attempt,
                    ));
                }
                Err(RunError::Cancelled) => {
                    let msg = "Task cancelled by client request".to_string();
                    self.finish_error(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Cancelled,
                        msg.clone(),
                        attempt,
                    )?;
                    return Ok(self.error_result(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Cancelled,
                        msg,
                        attempt,
                    ));
                }
                Err(RunError::Spawn(msg)) => {
                    if attempt < max_attempts {
                        eprintln!(
                            "task {} spawn failed ({}), retry {}/{}",
                            task.task_id,
                            msg,
                            attempt,
                            max_attempts - 1
                        );
                        continue;
                    }
                    self.finish_error(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Failed,
                        msg.clone(),
                        attempt,
                    )?;
                    return Ok(self.error_result(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Failed,
                        msg,
                        attempt,
                    ));
                }
            }
        }
    }

    /// Write stderr + metadata for an attempt that produced no output.
    fn finish_error(
        &self,
        task: &Task,
        start: chrono::DateTime<Utc>,
        end: chrono::DateTime<Utc>,
        _status: TaskStatus,
        msg: String,
        _attempt: u8,
    ) -> Result<(), String> {
        self.log_manager.write_stderr(task.task_id, &msg)?;
        self.log_manager
            .write_metadata(task.task_id, start, end, None, &task.command)?;
        Ok(())
    }

    fn error_result(
        &self,
        task: &Task,
        start_time: chrono::DateTime<Utc>,
        end_time: chrono::DateTime<Utc>,
        status: TaskStatus,
        msg: String,
        attempt: u8,
    ) -> TaskResult {
        self.clear_cancel(task.task_id);
        TaskResult {
            task_id: task.task_id,
            status,
            output: TaskOutput {
                stdout: String::new(),
                stderr: msg.clone(),
                exit_code: None,
            },
            command: task.command.clone(),
            start_time,
            end_time,
            duration_ms: (end_time - start_time).num_milliseconds(),
            retries_used: attempt.saturating_sub(1),
            artifacts: Vec::new(),
            error_message: Some(msg),
        }
    }

    /// Build the child process for one attempt.
    fn build_command(&self, task: &Task) -> Result<Command, RunError> {
        let mut cmd = if self.opts.use_shell {
            // `sh -c` so pipes, redirects, &&, $VAR and backgrounding work.
            // The command string comes from the task file, which is written
            // by the client into shared storage - same trust boundary as the
            // old argv path (whoever can write commands/ can already run code).
            let mut c = Command::new("sh");
            c.arg("-c").arg(&task.command);
            c
        } else {
            let args = shell_words::split(&task.command)
                .map_err(|e| RunError::Spawn(format!("Invalid command syntax: {}", e)))?;
            if args.is_empty() {
                return Err(RunError::Spawn("Empty command".to_string()));
            }
            let mut c = Command::new(&args[0]);
            if args.len() > 1 {
                c.args(&args[1..]);
            }
            c
        };

        if !task.working_dir.as_os_str().is_empty() && task.working_dir != Path::new(".") {
            cmd.current_dir(&task.working_dir);
        }
        for (key, value) in &task.env_vars {
            cmd.env(key, value);
        }
        Ok(cmd)
    }

    /// Execute a single attempt. Kills the whole process group on timeout or
    /// cancellation so grandchildren (`sh -c '...'`) do not leak.
    async fn run_once(&self, task: &Task, limit: Duration) -> Result<TaskOutput, RunError> {
        let mut cmd = self.build_command(task)?;
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        let mut child = cmd
            .spawn()
            .map_err(|e| RunError::Spawn(format!("Failed to spawn process: {}", e)))?;
        let pid = child.id();
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        // Drain both pipes concurrently - reading them sequentially
        // deadlocks when the child fills the other pipe (>64KB).
        let out_task = tokio::spawn(async move { read_pipe_to_string(stdout_pipe).await });
        let err_task = tokio::spawn(async move { read_pipe_to_string(stderr_pipe).await });

        // Reaper task owns the Child so the select arms below can still
        // trigger a kill by pid without fighting the borrow checker.
        let (tx, mut rx) = tokio::sync::oneshot::channel::<Result<Option<i32>, String>>();
        tokio::spawn(async move {
            let code = child
                .wait()
                .await
                .map(|s| s.code())
                .map_err(|e| e.to_string());
            let _ = tx.send(code);
        });

        let cancel_dir = self.opts.cancel_dir.clone();
        let tid = task.task_id;

        let outcome: Result<Option<i32>, RunError> = tokio::select! {
            r = &mut rx => match r {
                Ok(Ok(code)) => Ok(code),
                Ok(Err(e)) => Err(RunError::Spawn(e)),
                Err(_) => Err(RunError::Spawn("child reaper dropped".to_string())),
            },
            _ = sleep(limit) => {
                kill_group(pid);
                Err(RunError::TimedOut(limit))
            }
            _ = cancel_signal(cancel_dir, tid) => {
                kill_group(pid);
                Err(RunError::Cancelled)
            }
        };

        if outcome.is_err() {
            // Let SIGKILL land and reap, so we never leak a zombie.
            let _ = timeout(Duration::from_secs(10), &mut rx).await;
        }

        let stdout_data = out_task.await.unwrap_or_default();
        let stderr_data = err_task.await.unwrap_or_default();

        let exit_code = outcome?;
        Ok(TaskOutput {
            stdout: truncate_utf8(&stdout_data, MAX_OUTPUT_BYTES),
            stderr: truncate_utf8(&stderr_data, MAX_OUTPUT_BYTES),
            exit_code,
        })
    }

    /// Copy declared artifacts into `artifacts/<task_id>_<name>`.
    /// Returns the names actually collected (usable with results::read_artifact).
    fn collect_artifacts(&self, task: &Task) -> Vec<String> {
        let Some(dst_dir) = &self.opts.artifacts_dir else {
            return Vec::new();
        };
        if task.artifacts_expected.is_empty() {
            return Vec::new();
        }
        let base = if task.working_dir.as_os_str().is_empty() || task.working_dir == Path::new(".")
        {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            task.working_dir.clone()
        };
        if let Err(e) = std::fs::create_dir_all(dst_dir) {
            eprintln!("artifacts: cannot create {}: {}", dst_dir.display(), e);
            return Vec::new();
        }

        let mut collected = Vec::new();
        for declared in &task.artifacts_expected {
            let src = if Path::new(declared).is_absolute() {
                PathBuf::from(declared)
            } else {
                base.join(declared)
            };
            if !src.exists() {
                eprintln!(
                    "artifacts: {} declared but not produced ({})",
                    declared,
                    src.display()
                );
                continue;
            }
            let name = src
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "artifact".to_string());
            let dst = dst_dir.join(format!("{}_{}", task.task_id, name));
            let ok = if src.is_dir() {
                copy_dir(&src, &dst).is_ok()
            } else {
                std::fs::copy(&src, &dst).is_ok()
            };
            if ok {
                collected.push(name);
            } else {
                eprintln!("artifacts: failed to copy {}", src.display());
            }
        }
        collected
    }

    fn clear_cancel(&self, task_id: Uuid) {
        if let Some(dir) = &self.opts.cancel_dir {
            let _ = std::fs::remove_file(dir.join(format!("{}.json", task_id)));
        }
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
            .entry("CUDA_VISIBLE_DEVICES".to_string())
            .or_insert_with(|| gpu_id.to_string());
        self.execute(&task_with_gpu).await
    }
}

/// Resolve when a cancel request file appears (never, if cancellation is off).
async fn cancel_signal(dir: Option<PathBuf>, task_id: Uuid) {
    match dir {
        Some(d) => {
            let path = d.join(format!("{}.json", task_id));
            loop {
                if path.exists() {
                    return;
                }
                sleep(CANCEL_POLL).await;
            }
        }
        None => std::future::pending::<()>().await,
    }
}

/// SIGKILL an entire process group (negative pid).
fn kill_group(pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
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

/// Drain a child pipe fully into a String (empty on error / no pipe).
async fn read_pipe_to_string<R>(pipe: Option<R>) -> String
where
    R: AsyncRead + Unpin,
{
    use tokio::io::AsyncReadExt;
    let Some(mut pipe) = pipe else {
        return String::new();
    };
    let mut buf = String::new();
    match pipe.read_to_string(&mut buf).await {
        Ok(_) => buf,
        Err(e) => {
            eprintln!("Failed to read child pipe: {}", e);
            buf
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn create_test_task() -> Task {
        Task::new(
            "echo 'Hello, World!'".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5)
        .with_working_dir(PathBuf::from("."))
    }

    fn executor(tmp: &TempDir) -> Executor {
        Executor::new(tmp.path().join("logs"), Duration::from_secs(30)).unwrap()
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

    /// 修复项 2：shell 语法（管道、重定向、&&、变量）必须可用。
    #[tokio::test]
    async fn test_shell_syntax_supported() {
        let temp_dir = TempDir::new().unwrap();
        let ex = executor(&temp_dir);
        let task = Task::new(
            "X=abc; echo \"$X\" | tr a-z A-Z && echo done".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10);

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(
            r.status,
            TaskStatus::Completed,
            "stderr={}",
            r.output.stderr
        );
        assert!(
            r.output.stdout.contains("ABC"),
            "stdout={}",
            r.output.stdout
        );
        assert!(r.output.stdout.contains("done"));
    }

    /// use_shell=false 时保持旧的 argv 语义（无 shell 解释）。
    #[tokio::test]
    async fn test_without_shell_does_not_interpret() {
        let temp_dir = TempDir::new().unwrap();
        let ex = Executor::with_options(
            temp_dir.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(10),
                use_shell: false,
                ..Default::default()
            },
        )
        .unwrap();
        let task = Task::new(
            "echo $HOME".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10);

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Completed);
        assert!(
            r.output.stdout.contains("$HOME"),
            "无 shell 时变量不应展开: {}",
            r.output.stdout
        );
    }

    #[tokio::test]
    async fn test_execute_failed_command() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "sh -c 'exit 1'".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5)
        .with_retry_count(0);

        let result = executor.execute(&task).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.output.exit_code, Some(1));
        assert!(!result.is_success());
    }

    /// 修复项 8：失败任务按 retry_count 重试，且 retries_used 如实记录。
    #[tokio::test]
    async fn test_retry_on_failure() {
        let temp_dir = TempDir::new().unwrap();
        let marker = temp_dir.path().join("attempts.log");
        let ex = Executor::with_options(
            temp_dir.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(30),
                max_retries: 3,
                ..Default::default()
            },
        )
        .unwrap();
        let task = Task::new(
            format!("echo x >> {}; exit 3", marker.display()),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10)
        .with_retry_count(2);

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Failed);
        assert_eq!(r.retries_used, 2, "应重试 2 次");
        let attempts = std::fs::read_to_string(&marker).unwrap();
        assert_eq!(
            attempts.lines().count(),
            3,
            "共执行 3 次（1 次 + 2 次重试）"
        );
    }

    /// 成功任务不重试。
    #[tokio::test]
    async fn test_no_retry_on_success() {
        let temp_dir = TempDir::new().unwrap();
        let ex = executor(&temp_dir);
        let task = Task::new("true".to_string(), crate::core::models::TaskType::Shell)
            .with_timeout(5)
            .with_retry_count(3);
        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.retries_used, 0);
    }

    /// 修复项 1：timeout=0 继承 daemon 默认值。
    #[tokio::test]
    async fn test_timeout_zero_inherits_default() {
        let temp_dir = TempDir::new().unwrap();
        let ex = Executor::with_options(
            temp_dir.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(60),
                ..Default::default()
            },
        )
        .unwrap();
        let task = Task::new("true".to_string(), crate::core::models::TaskType::Shell);
        assert_eq!(task.timeout, 0);
        assert_eq!(ex.effective_timeout(&task), Duration::from_secs(60));

        let explicit = task.clone().with_timeout(7);
        assert_eq!(
            ex.effective_timeout(&explicit),
            Duration::from_secs(7),
            "任务显式 timeout 必须覆盖 daemon 默认（不再被 min() 钳制）"
        );
    }

    #[tokio::test]
    async fn test_execute_timeout() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task =
            Task::new("sleep 10".to_string(), crate::core::models::TaskType::Shell).with_timeout(2);

        let result = executor.execute(&task).await;
        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Timeout);
        assert!(result.error_message.unwrap().contains("timed out"));
    }

    /// 修复项 6：cancel 请求文件出现后杀掉进程组并返回 Cancelled。
    #[tokio::test]
    async fn test_cancel_signal_kills_task() {
        let temp_dir = TempDir::new().unwrap();
        let cancel_dir = temp_dir.path().join("cancel");
        std::fs::create_dir_all(&cancel_dir).unwrap();
        let ex = Executor::with_options(
            temp_dir.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(60),
                cancel_dir: Some(cancel_dir.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let task = Task::new("sleep 30".to_string(), crate::core::models::TaskType::Shell)
            .with_timeout(30);
        let tid = task.task_id;
        let signal = cancel_dir.join(format!("{}.json", tid));

        // Run the execution concurrently with the cancel trigger.
        let exec = ex.execute(&task);
        let fire = async {
            tokio::time::sleep(Duration::from_millis(500)).await;
            std::fs::write(&signal, "{}").unwrap();
        };
        let (exec_result, _) = tokio::join!(exec, fire);

        let r = exec_result.unwrap();
        assert_eq!(r.status, TaskStatus::Cancelled);
        assert!(!signal.exists(), "客户端 cancel 文件应在任务结束后清理");
    }

    /// 修复项 7：声明 artifacts 后必须真的落盘并出现在 result.artifacts。
    #[tokio::test]
    async fn test_artifacts_collected() {
        let temp_dir = TempDir::new().unwrap();
        let work = temp_dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        let arts = temp_dir.path().join("artifacts");
        let ex = Executor::with_options(
            temp_dir.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(30),
                artifacts_dir: Some(arts.clone()),
                ..Default::default()
            },
        )
        .unwrap();

        let task = Task::new(
            "echo '{\"ok\":true}' > report.json".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10)
        .with_working_dir(work.clone())
        .with_artifact("report.json".to_string());

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Completed);
        assert_eq!(r.artifacts, vec!["report.json".to_string()]);
        let stored = arts.join(format!("{}_report.json", task.task_id));
        assert!(stored.exists(), "artifact 应复制到 {}", stored.display());
        assert!(std::fs::read_to_string(&stored).unwrap().contains("ok"));
    }

    /// 未生成的 artifact 不应报错，只是不记录。
    #[tokio::test]
    async fn test_missing_artifact_is_tolerated() {
        let temp_dir = TempDir::new().unwrap();
        let arts = temp_dir.path().join("artifacts");
        let ex = Executor::with_options(
            temp_dir.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(30),
                artifacts_dir: Some(arts),
                ..Default::default()
            },
        )
        .unwrap();
        let task = Task::new("true".to_string(), crate::core::models::TaskType::Shell)
            .with_timeout(5)
            .with_artifact("nope.bin".to_string());
        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Completed);
        assert!(r.artifacts.is_empty());
    }

    #[tokio::test]
    async fn test_stdout_truncation() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -c \"print('A' * 2000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10);

        let result = executor.execute(&task).await.unwrap();
        assert!(result.output.stdout.contains("AAAA"));
        assert!(!result.output.stdout.ends_with("..."));
    }

    #[tokio::test]
    async fn test_stdout_truncation_multibyte_no_panic() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -c \"print('你好' * 500)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10);

        let result = executor.execute(&task).await.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.output.stdout.contains("你好"));
        assert!(!result.output.stdout.ends_with("..."));
        assert!(std::str::from_utf8(result.output.stdout.as_bytes()).is_ok());
    }

    #[test]
    fn test_truncate_utf8_boundary() {
        assert_eq!(truncate_utf8("hello", 100), "hello");
        assert_eq!(truncate_utf8("hello world", 5), "hello...");
        let cn = "你".repeat(500); // 1500 字节
        let t = truncate_utf8(&cn, 1000);
        assert!(t.len() <= 1003);
        assert!(t.ends_with("..."));
        assert!(std::str::from_utf8(t.as_bytes()).is_ok());
        assert!(t.starts_with(&"你".repeat(333)));
    }

    /// 修复项 9：超过 8MB 才截断。
    #[tokio::test]
    async fn test_stdout_truncation_over_8mb() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -u -c \"import sys; sys.stdout.write('A' * 9000000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(120);

        let result = executor.execute(&task).await.unwrap();
        assert!(result.output.stdout.len() <= MAX_OUTPUT_BYTES + 4);
        assert!(result.output.stdout.ends_with("..."));
        assert!(std::str::from_utf8(result.output.stdout.as_bytes()).is_ok());
    }

    /// 1MB~8MB 之间的输出不再被截断（旧版本会丢）。
    #[tokio::test]
    async fn test_2mb_output_not_truncated() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -u -c \"import sys; sys.stdout.write('B' * 2000000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(120);
        let r = executor.execute(&task).await.unwrap();
        assert_eq!(r.output.stdout.len(), 2_000_000);
    }

    /// 子进程往 stderr 灌大输出时不能死锁（旧的串行读管道会卡住）。
    #[tokio::test]
    async fn test_large_stderr_does_not_deadlock() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -u -c \"import sys; sys.stderr.write('E' * 300000); print('ok')\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(30);
        let r = tokio::time::timeout(Duration::from_secs(20), executor.execute(&task))
            .await
            .expect("执行不应死锁")
            .unwrap();
        assert_eq!(r.status, TaskStatus::Completed);
        assert!(r.output.stdout.contains("ok"));
        assert_eq!(r.output.stderr.len(), 300_000);
    }

    #[tokio::test]
    async fn test_execute_with_env_vars() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "sh -c 'echo $TEST_VAR'".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5)
        .with_env_var("TEST_VAR".to_string(), "test_value".to_string());

        let result = executor.execute(&task).await.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.output.stdout.contains("test_value"));
    }

    #[tokio::test]
    async fn test_execute_with_gpu_injects_env() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "echo \"cuda=$CUDA_VISIBLE_DEVICES\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5);
        let r = executor.execute_with_gpu(&task, 3).await.unwrap();
        assert!(r.output.stdout.contains("cuda=3"));
    }
}
