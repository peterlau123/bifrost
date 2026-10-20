// Generic command executor using tokio subprocess.
//
// Handles: shell invocation, per-task timeout (0 = inherit daemon default),
// cancellation signal files, retries, artifact collection, streaming
// stdout/stderr to log files while keeping a bounded copy for the result.

use chrono::Utc;
use std::collections::VecDeque;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::{sleep, timeout, Duration};
use uuid::Uuid;

use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus};
use crate::daemon::logger::LogManager;

/// Result-file output cap (head + tail together). Full output goes to
/// logs/<task_id>/stdout.log while the task runs.
pub const MAX_OUTPUT_BYTES: usize = 8 * 1024 * 1024;
/// Daemon-wide default when neither the task nor settings specify a timeout.
pub const DEFAULT_TIMEOUT_SECS: u64 = 5400;
/// How often a running task checks for a cancel request file.
const CANCEL_POLL: Duration = Duration::from_millis(300);
/// How long to wait for the reaper / pipe drain after SIGKILL.
const REAP_TIMEOUT: Duration = Duration::from_secs(5);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(5);

/// Tunables for the executor, derived from daemon settings.
#[derive(Clone, Debug)]
pub struct ExecutorOptions {
    /// Used when `task.timeout == 0`.
    pub default_timeout: Duration,
    /// Wrap the command in `sh -c` so shell syntax works.
    pub use_shell: bool,
    /// Upper bound on retries (task.retry_count is capped by this).
    /// Retries are opt-in: both this and task.retry_count must be > 0.
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
            max_retries: 0,
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

/// Bounded capture of one stream. Until `limit` is exceeded this is the whole
/// output; afterwards it keeps `limit/2` from the head and `limit/2` from the
/// tail, plus the omitted byte count.
#[derive(Default)]
struct Capture {
    buf: Vec<u8>,
    tail: VecDeque<u8>,
    total: u64,
    truncated: bool,
}

impl Capture {
    fn push(&mut self, chunk: &[u8], limit: usize) {
        self.total += chunk.len() as u64;
        let half = limit / 2;
        let keep = limit - half;
        if !self.truncated {
            self.buf.extend_from_slice(chunk);
            if self.buf.len() > limit {
                // Switch to head + rolling tail: keep the first half verbatim,
                // everything after it (and all later output) feeds the tail.
                let rest = self.buf.split_off(half);
                self.tail.extend(rest);
                while self.tail.len() > keep {
                    self.tail.pop_front();
                }
                self.truncated = true;
            }
        } else {
            self.tail.extend(chunk.iter().copied());
            while self.tail.len() > keep {
                self.tail.pop_front();
            }
        }
    }

    fn render(&self) -> String {
        if !self.truncated {
            return String::from_utf8_lossy(&self.buf).into_owned();
        }
        let head = String::from_utf8_lossy(&self.buf).into_owned();
        let tail_bytes: Vec<u8> = self.tail.iter().copied().collect();
        let tail = String::from_utf8_lossy(&tail_bytes).into_owned();
        let omitted = self.total.saturating_sub((head.len() + tail.len()) as u64);
        format!(
            "{}\n... [{} bytes omitted, full log in logs/] ...\n{}",
            head, omitted, tail
        )
    }
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

    /// Execute a task and return the result, retrying when the task opts in.
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
            let outcome = self.run_once(task, limit, attempt).await;
            let end_time = Utc::now();

            match outcome {
                Ok(output) => {
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

                    let artifacts = self.collect_artifacts(task).await;
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
                    let artifacts = self.collect_artifacts(task).await;
                    self.finish_error(task, start_time, end_time, &msg, attempt)?;
                    return Ok(self.error_result(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Timeout,
                        msg,
                        attempt,
                        artifacts,
                    ));
                }
                Err(RunError::Cancelled) => {
                    let msg = "Task cancelled by client request".to_string();
                    // A cancelled benchmark may still have written results.
                    let artifacts = self.collect_artifacts(task).await;
                    self.finish_error(task, start_time, end_time, &msg, attempt)?;
                    return Ok(self.error_result(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Cancelled,
                        msg,
                        attempt,
                        artifacts,
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
                        sleep(Duration::from_millis(500 * attempt as u64)).await;
                        continue;
                    }
                    self.finish_error(task, start_time, end_time, &msg, attempt)?;
                    return Ok(self.error_result(
                        task,
                        start_time,
                        end_time,
                        TaskStatus::Failed,
                        msg,
                        attempt,
                        Vec::new(),
                    ));
                }
            }
        }
    }

    /// Append the failure reason to the attempt's stderr log + write metadata.
    fn finish_error(
        &self,
        task: &Task,
        start: chrono::DateTime<Utc>,
        end: chrono::DateTime<Utc>,
        msg: &str,
        attempt: u8,
    ) -> Result<(), String> {
        if let Ok((_, stderr_path)) = self.attempt_log_paths(task.task_id, attempt) {
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&stderr_path)
            {
                let _ = writeln!(f, "\n[executor] {}", msg);
            }
        }
        self.log_manager
            .write_metadata(task.task_id, start, end, None, &task.command)?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn error_result(
        &self,
        task: &Task,
        start_time: chrono::DateTime<Utc>,
        end_time: chrono::DateTime<Utc>,
        status: TaskStatus,
        msg: String,
        attempt: u8,
        artifacts: Vec<String>,
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
            artifacts,
            error_message: Some(msg),
        }
    }

    /// stdout/stderr paths for a given attempt (attempt 1 keeps the classic
    /// names so existing tooling keeps working).
    fn attempt_log_paths(&self, task_id: Uuid, attempt: u8) -> Result<(PathBuf, PathBuf), String> {
        let (stdout_path, stderr_path) = self.log_manager.get_log_paths(task_id)?;
        if attempt <= 1 {
            return Ok((stdout_path, stderr_path));
        }
        let dir = stdout_path.parent().unwrap_or(Path::new("."));
        Ok((
            dir.join(format!("stdout.attempt{}.log", attempt)),
            dir.join(format!("stderr.attempt{}.log", attempt)),
        ))
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
    async fn run_once(
        &self,
        task: &Task,
        limit: Duration,
        attempt: u8,
    ) -> Result<TaskOutput, RunError> {
        let mut cmd = self.build_command(task)?;
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());
        #[cfg(unix)]
        cmd.process_group(0);

        let (stdout_path, stderr_path) = self
            .attempt_log_paths(task.task_id, attempt)
            .unwrap_or_else(|_| (PathBuf::new(), PathBuf::new()));

        let mut child = cmd
            .spawn()
            .map_err(|e| RunError::Spawn(format!("Failed to spawn process: {}", e)))?;
        let pid = child.id();
        let stdout_pipe = child.stdout.take();
        let stderr_pipe = child.stderr.take();

        // Drain both pipes concurrently into their log files - reading them
        // sequentially deadlocks when the child fills the other pipe, and
        // holding the whole stream in memory OOMs on hour-long logs.
        let out_task = tokio::spawn(drain_to_file(stdout_pipe, stdout_path));
        let err_task = tokio::spawn(drain_to_file(stderr_pipe, stderr_path));

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

        // `biased` polls the child first: if it already exited we must report
        // its real status, not a timeout/cancel that raced with the exit.
        let outcome: Result<Option<i32>, RunError> = tokio::select! {
            biased;
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
            let _ = timeout(REAP_TIMEOUT, &mut rx).await;
        }

        let stdout_cap = finish_capture(out_task).await;
        let stderr_cap = finish_capture(err_task).await;

        let exit_code = outcome?;
        Ok(TaskOutput {
            stdout: stdout_cap.render(),
            stderr: stderr_cap.render(),
            exit_code,
        })
    }

    /// Copy declared artifacts into `artifacts/<task_id>_<name>`.
    /// Returns the names actually collected (usable with results::read_artifact).
    /// Sources must live under the task's working directory.
    async fn collect_artifacts(&self, task: &Task) -> Vec<String> {
        let Some(dst_dir) = self.opts.artifacts_dir.clone() else {
            return Vec::new();
        };
        if task.artifacts_expected.is_empty() {
            return Vec::new();
        }
        let task = task.clone();
        // File copies are blocking syscalls; keep them off async workers.
        tokio::task::spawn_blocking(move || collect_artifacts_blocking(&task, &dst_dir))
            .await
            .unwrap_or_default()
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

/// Blocking artifact collection, confined to the task's working directory.
fn collect_artifacts_blocking(task: &Task, dst_dir: &Path) -> Vec<String> {
    let base = if task.working_dir.as_os_str().is_empty() || task.working_dir == Path::new(".") {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    } else {
        task.working_dir.clone()
    };
    let base = base.canonicalize().unwrap_or(base);
    if let Err(e) = std::fs::create_dir_all(dst_dir) {
        eprintln!("artifacts: cannot create {}: {}", dst_dir.display(), e);
        return Vec::new();
    }

    let mut collected: Vec<String> = Vec::new();
    for declared in &task.artifacts_expected {
        let raw = Path::new(declared);
        if raw.is_absolute() {
            eprintln!(
                "artifacts: refusing absolute path {} (must be under the working dir)",
                declared
            );
            continue;
        }
        let src = base.join(raw);
        let Ok(src_canon) = src.canonicalize() else {
            eprintln!("artifacts: {} declared but not produced", declared);
            continue;
        };
        if !src_canon.starts_with(&base) {
            eprintln!("artifacts: {} escapes the working dir, skipped", declared);
            continue;
        }
        let name = src_canon
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| "artifact".to_string());
        if collected.contains(&name) {
            eprintln!("artifacts: duplicate name {}, skipped", name);
            continue;
        }
        let dst = dst_dir.join(format!("{}_{}", task.task_id, name));
        let ok = if src_canon.is_dir() {
            copy_dir(&src_canon, &dst).is_ok()
        } else {
            std::fs::copy(&src_canon, &dst).is_ok()
        };
        if ok {
            collected.push(name);
        } else {
            eprintln!("artifacts: failed to copy {}", src_canon.display());
        }
    }
    collected
}

/// Await a drain task, but never hang forever: a SIGKILL that missed (wrong
/// pgid, recycled pid) would leave the pipe open.
async fn finish_capture(task: tokio::task::JoinHandle<Capture>) -> Capture {
    match timeout(DRAIN_TIMEOUT, task).await {
        Ok(Ok(cap)) => cap,
        Ok(Err(e)) => {
            eprintln!("drain task panicked: {}", e);
            Capture::default()
        }
        Err(_) => {
            eprintln!("drain timed out after {:?}", DRAIN_TIMEOUT);
            Capture::default()
        }
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

/// Drain a child pipe into `path`, keeping a bounded capture for the result.
async fn drain_to_file<R>(pipe: Option<R>, path: PathBuf) -> Capture
where
    R: AsyncRead + Unpin,
{
    let mut cap = Capture::default();
    let Some(mut pipe) = pipe else {
        return cap;
    };
    let mut file = match std::fs::File::create(&path) {
        Ok(f) => Some(f),
        Err(e) => {
            if !path.as_os_str().is_empty() {
                eprintln!("cannot create log {}: {}", path.display(), e);
            }
            None
        }
    };

    let mut buf = vec![0u8; 64 * 1024];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => {
                let chunk = &buf[..n];
                if let Some(f) = file.as_mut() {
                    let _ = f.write_all(chunk);
                }
                cap.push(chunk, MAX_OUTPUT_BYTES);
            }
            Err(e) => {
                eprintln!("read from child pipe failed: {}", e);
                break;
            }
        }
    }
    if let Some(mut f) = file {
        let _ = f.flush();
    }
    cap
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn executor(tmp: &TempDir) -> Executor {
        Executor::new(tmp.path().join("logs"), Duration::from_secs(30)).unwrap()
    }

    fn retrying_executor(tmp: &TempDir, max_retries: u8) -> Executor {
        Executor::with_options(
            tmp.path().join("logs"),
            ExecutorOptions {
                default_timeout: Duration::from_secs(30),
                max_retries,
                ..Default::default()
            },
        )
        .unwrap()
    }

    #[tokio::test]
    async fn test_execute_shell_command() {
        let temp_dir = TempDir::new().unwrap();
        let log_root = temp_dir.path().join("logs");
        let executor = Executor::new(log_root.clone(), Duration::from_secs(30)).unwrap();
        let task = Task::new(
            "echo 'Hello, World!'".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(5);

        let result = executor.execute(&task).await.unwrap();
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

        let result = executor.execute(&task).await.unwrap();
        assert_eq!(result.status, TaskStatus::Failed);
        assert_eq!(result.output.exit_code, Some(1));
        assert!(!result.is_success());
    }

    /// 默认不重试：非零退出只跑一次（避免起服务类任务被重复拉起）。
    #[tokio::test]
    async fn test_no_retry_by_default() {
        let temp_dir = TempDir::new().unwrap();
        // max_retries 默认 0
        let ex = executor(&temp_dir);
        let marker = temp_dir.path().join("attempts.log");
        let task = Task::new(
            format!("echo x >> {}; exit 3", marker.display()),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10)
        .with_retry_count(5);

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Failed);
        assert_eq!(r.retries_used, 0, "daemon 未开启重试时不得重试");
        assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 1);
    }

    /// 显式开启重试后按 retry_count 重试，日志按 attempt 分开保存。
    #[tokio::test]
    async fn test_retry_on_failure_when_enabled() {
        let temp_dir = TempDir::new().unwrap();
        let ex = retrying_executor(&temp_dir, 3);
        let marker = temp_dir.path().join("attempts.log");
        let task = Task::new(
            format!("echo x >> {}; exit 3", marker.display()),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(10)
        .with_retry_count(2);

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Failed);
        assert_eq!(r.retries_used, 2, "应重试 2 次");
        assert_eq!(std::fs::read_to_string(&marker).unwrap().lines().count(), 3);

        // 每次尝试的日志都在，不会被后一次覆盖
        let dir = temp_dir.path().join("logs").join(task.task_id.to_string());
        assert!(dir.join("stdout.log").exists());
        assert!(dir.join("stdout.attempt2.log").exists());
        assert!(dir.join("stdout.attempt3.log").exists());
    }

    /// 成功任务不重试。
    #[tokio::test]
    async fn test_no_retry_on_success() {
        let temp_dir = TempDir::new().unwrap();
        let ex = retrying_executor(&temp_dir, 3);
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

        let result = executor.execute(&task).await.unwrap();
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

    /// 超时任务也要收集 artifacts（长 benchmark 常中途写出结果）。
    #[tokio::test]
    async fn test_artifacts_collected_on_timeout() {
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
            "echo partial > result.json; sleep 30".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(2)
        .with_working_dir(work)
        .with_artifact("result.json".to_string());

        let r = ex.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Timeout);
        assert_eq!(r.artifacts, vec!["result.json".to_string()]);
    }

    /// 绝对路径 / 越界路径的 artifact 拒绝收集。
    #[tokio::test]
    async fn test_artifact_path_confinement() {
        let temp_dir = TempDir::new().unwrap();
        let work = temp_dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::write(temp_dir.path().join("secret.txt"), "s").unwrap();
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
            .with_working_dir(work)
            .with_artifact("../secret.txt".to_string());
        let r = ex.execute(&task).await.unwrap();
        assert!(r.artifacts.is_empty(), "越界路径不得被收集");
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
        assert!(!result.output.stdout.contains("bytes omitted"));
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
        assert!(std::str::from_utf8(result.output.stdout.as_bytes()).is_ok());
    }

    /// 修复项 9：超过 8MB 才截断，且保留尾部（benchmark 汇总在末尾）。
    #[tokio::test]
    async fn test_stdout_truncation_over_8mb_keeps_tail() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -u -c \"import sys; sys.stdout.write('A' * 9000000); sys.stdout.write('TAILMARK')\""
                .to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(120);

        let result = executor.execute(&task).await.unwrap();
        assert!(
            result.output.stdout.len() <= MAX_OUTPUT_BYTES + 100,
            "载荷应有上限，实际 {}",
            result.output.stdout.len()
        );
        assert!(result.output.stdout.contains("bytes omitted"));
        assert!(
            result.output.stdout.contains("TAILMARK"),
            "尾部必须保留（benchmark 汇总在最后）"
        );
    }

    /// 完整输出必须落在日志文件里（不被截断）。
    #[tokio::test]
    async fn test_full_output_written_to_log() {
        let temp_dir = TempDir::new().unwrap();
        let executor = executor(&temp_dir);
        let task = Task::new(
            "python3 -u -c \"import sys; sys.stdout.write('B' * 9000000)\"".to_string(),
            crate::core::models::TaskType::Shell,
        )
        .with_timeout(120);
        let r = executor.execute(&task).await.unwrap();
        assert_eq!(r.status, TaskStatus::Completed);
        let log = temp_dir
            .path()
            .join("logs")
            .join(task.task_id.to_string())
            .join("stdout.log");
        assert_eq!(std::fs::metadata(&log).unwrap().len(), 9_000_000);
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
            "echo $TEST_VAR".to_string(),
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
