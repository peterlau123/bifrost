// SSH Bridge implementation
//
// Transport-agnostic Bridge over SSH: the client reaches the target
// machine's directories (commands/ results/ status/ artifacts/) through
// ssh invocations instead of a shared filesystem. The server (daemon)
// on the target machine keeps using the local Protocol bridge — both
// sides share the same directory semantics, so a shared-storage daemon
// and an SSH client interoperate without changes.
//
// File contents travel over the ssh process stdin (`cat >` on the remote
// side), so no quoting/escaping of payloads is ever needed; remote paths
// are shell-quoted.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use uuid::Uuid;

use crate::core::bridge::{Bridge, TaskStatusResponse};
use crate::core::error::{BifrostError, Result};
use crate::core::models::{Task, TaskResult, TaskStatus};
use crate::core::settings::SshSection;

/// SSH transport implementing [`Bridge`].
pub struct SshBridge {
    /// Target hostname or IP.
    host: String,
    /// SSH user (None = current user).
    user: Option<String>,
    /// SSH port (None = 22).
    port: Option<u16>,
    /// Remote directory acting as shared_storage root.
    remote_dir: PathBuf,
    /// Connect timeout seconds for each ssh invocation.
    connect_timeout: u64,
    /// Test-only: run remote commands locally instead of over ssh.
    local: bool,
}

/// Quote a path for safe inclusion in a remote shell command.
fn sh_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

impl SshBridge {
    /// Create a new SSH bridge from the ssh config section.
    pub fn new(cfg: &SshSection) -> Result<Self> {
        let host = cfg
            .host
            .clone()
            .ok_or_else(|| BifrostError::ConfigInvalid("ssh.host required".into()))?;
        let remote_dir = cfg
            .remote_dir
            .clone()
            .ok_or_else(|| BifrostError::ConfigInvalid("ssh.remote_dir required".into()))?;
        Ok(Self {
            host,
            user: cfg.user.clone(),
            port: cfg.port,
            remote_dir,
            connect_timeout: cfg.connect_timeout.map(|d| d.as_secs()).unwrap_or(10),
            local: false,
        })
    }

    /// Build the `ssh [user@]host ...` command prefix.
    fn ssh_command(&self) -> Command {
        let mut cmd = Command::new("ssh");
        cmd.arg("-o").arg("BatchMode=yes");
        cmd.arg("-o")
            .arg(format!("ConnectTimeout={}", self.connect_timeout));
        if let Some(p) = self.port {
            cmd.arg("-p").arg(p.to_string());
        }
        let target = match &self.user {
            Some(u) => format!("{}@{}", u, self.host),
            None => self.host.clone(),
        };
        cmd.arg(target);
        cmd
    }

    /// Spawn a remote shell command. In test mode (`local`), the command
    /// runs through `sh -c` on this machine instead of over ssh.
    fn spawn_remote(&self, remote_cmd: &str) -> Result<Child> {
        if self.local {
            let mut cmd = Command::new("sh");
            cmd.arg("-c").arg(remote_cmd);
            cmd.stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped());
            return cmd
                .spawn()
                .map_err(|e| BifrostError::ExecutionError(format!("local spawn failed: {}", e)));
        }
        self.ssh_command()
            .arg(remote_cmd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| BifrostError::ExecutionError(format!("ssh spawn failed: {}", e)))
    }

    /// Run a remote shell command; return stdout on success.
    fn run_remote(&self, remote_cmd: &str) -> Result<String> {
        let output = self
            .spawn_remote(remote_cmd)?
            .wait_with_output()
            .map_err(|e| BifrostError::ExecutionError(format!("ssh wait failed: {}", e)))?;
        if !output.status.success() {
            return Err(BifrostError::ExecutionError(format!(
                "remote command failed (exit {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Write `content` to `remote_path`: the payload travels over the
    /// process stdin (ssh forwards it to the remote `cat >`), so no
    /// quoting/escaping of the content is ever needed.
    ///
    /// The write is atomic on the remote side (tmp file + mv): the daemon's
    /// watcher only sees a complete file, never a half-written one
    /// (mirrors Protocol's atomic_write semantics — GPFS/SSH 跨节点读半截
    /// 会触发 "cannot parse task file").
    fn write_remote(&self, remote_path: &str, content: &str) -> Result<()> {
        let dir = remote_path.rsplit_once('/').map(|(d, _)| d).unwrap_or(".");
        let tmp = format!("{}.tmp", remote_path);
        let cmd = format!(
            "mkdir -p {} && cat > {} && mv {} {}",
            sh_quote(dir),
            sh_quote(&tmp),
            sh_quote(&tmp),
            sh_quote(remote_path)
        );
        let mut child = self.spawn_remote(&cmd)?;
        child
            .stdin
            .take()
            .expect("stdin piped")
            .write_all(content.as_bytes())
            .map_err(|e| BifrostError::ExecutionError(format!("stdin write failed: {}", e)))?;
        let output = child
            .wait_with_output()
            .map_err(|e| BifrostError::ExecutionError(format!("remote wait failed: {}", e)))?;
        if !output.status.success() {
            return Err(BifrostError::ExecutionError(format!(
                "remote write failed (exit {}): {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(())
    }

    /// Delete every file under `remote_dir/<subdir>` whose name contains the task id.
    fn remove_remote_matching(&self, subdir: &str, task_id: &Uuid) -> Result<()> {
        let base = sh_quote(&self.remote_dir.join(subdir).to_string_lossy());
        let id = task_id.to_string();
        let cmd = format!(
            "ls {base} 2>/dev/null | grep {id} | while read f; do rm -f {base}/\"$f\"; done",
            base = base,
            id = sh_quote(&id)
        );
        self.run_remote(&cmd)?;
        Ok(())
    }

    /// Read the content of `remote_dir/<subdir>/<filename>` if it exists.
    fn read_remote_file(&self, subdir: &str, filename: &str) -> Result<Option<String>> {
        let path = self.remote_dir.join(subdir).join(filename);
        let cmd = format!("cat {}", sh_quote(&path.to_string_lossy()));
        match self.run_remote(&cmd) {
            Ok(out) => Ok(Some(out)),
            Err(BifrostError::ExecutionError(_)) => Ok(None),
            Err(e) => Err(e),
        }
    }

    /// Task filename shared with the shared-storage protocol.
    fn task_filename(&self, task: &Task) -> String {
        let ts = task.timestamp.format("%Y%m%d_%H%M%S");
        format!("{}_{}.json", ts, task.task_id)
    }

    /// Age in seconds of the remote heartbeat.json (None if missing/unreadable).
    /// Both `date` and `stat` run on the target machine, so no clock skew.
    pub fn heartbeat_age_secs(&self) -> Option<u64> {
        let hb = self.remote_dir.join("heartbeat.json");
        let cmd = format!(
            "test -f {} && echo $(( $(date +%s) - $(stat -c %Y {}) )) || echo -1",
            sh_quote(&hb.to_string_lossy()),
            sh_quote(&hb.to_string_lossy())
        );
        let out = self.run_remote(&cmd).ok()?;
        let age: i64 = out.trim().parse().ok()?;
        (age >= 0).then_some(age as u64)
    }
}

impl Bridge for SshBridge {
    fn submit_task(&self, task: &Task) -> Result<()> {
        let json = serde_json::to_string_pretty(task)?;
        let path = self
            .remote_dir
            .join("commands")
            .join(self.task_filename(task));
        self.write_remote(&path.to_string_lossy(), &json)
    }

    fn query_status(&self, task_id: &Uuid) -> Result<TaskStatusResponse> {
        // 1. Completed? check results/<id>_result.json
        if let Some(content) =
            self.read_remote_file("results", &format!("{}_result.json", task_id))?
        {
            let result: TaskResult = serde_json::from_str(&content)?;
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status: result.status.clone(),
                message: Some(format!("Task completed in {}ms", result.duration_ms())),
            });
        }
        // 2. Running/pending? check status/<id>.json
        if let Some(content) = self.read_remote_file("status", &format!("{}.json", task_id))? {
            let data: serde_json::Value = serde_json::from_str(&content)?;
            let status_str = data
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let status = parse_status_string(status_str);
            let message = data
                .get("message")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string());
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status,
                message,
            });
        }
        // 3. Pending? task file still in commands/
        if self.read_task(task_id).is_ok() {
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status: TaskStatus::Pending,
                message: Some("Task is pending execution".to_string()),
            });
        }
        Err(BifrostError::TaskNotFound(*task_id))
    }

    fn get_result(&self, task_id: &Uuid) -> Result<TaskResult> {
        let content = self
            .read_remote_file("results", &format!("{}_result.json", task_id))?
            .ok_or(BifrostError::TaskNotFound(*task_id))?;
        Ok(serde_json::from_str(&content)?)
    }

    fn list_pending_tasks(&self) -> Result<Vec<Task>> {
        let base = sh_quote(&self.remote_dir.join("commands").to_string_lossy());
        let cmd = format!(
            "ls {base} 2>/dev/null | grep '\\.json$' | while read f; do echo \"===FILE===$f\"; cat {base}/\"$f\"; echo; done",
            base = base
        );
        let out = self.run_remote(&cmd)?;
        let mut tasks = Vec::new();
        for chunk in out.split("===FILE===").skip(1) {
            // chunk = "<filename>\n<json>"
            if let Some(pos) = chunk.find('\n') {
                if let Ok(t) = serde_json::from_str::<Task>(&chunk[pos + 1..]) {
                    tasks.push(t);
                }
            }
        }
        Ok(tasks)
    }

    fn read_task(&self, task_id: &Uuid) -> Result<Task> {
        let base = sh_quote(&self.remote_dir.join("commands").to_string_lossy());
        let id = task_id.to_string();
        let cmd = format!(
            "ls {base} 2>/dev/null | grep {id} | grep '\\.json$' | head -1 | while read f; do cat {base}/\"$f\"; done",
            base = base,
            id = sh_quote(&id)
        );
        let out = self.run_remote(&cmd)?;
        if out.trim().is_empty() {
            return Err(BifrostError::TaskNotFound(*task_id));
        }
        Ok(serde_json::from_str(&out)?)
    }

    fn write_result(&self, task_id: &Uuid, result: &TaskResult) -> Result<()> {
        let json = serde_json::to_string_pretty(result)?;
        let path = self
            .remote_dir
            .join("results")
            .join(format!("{}_result.json", task_id));
        self.write_remote(&path.to_string_lossy(), &json)
    }

    fn write_status(
        &self,
        task_id: &Uuid,
        status: &TaskStatus,
        message: Option<&str>,
    ) -> Result<()> {
        let mut map = serde_json::Map::new();
        map.insert("task_id".into(), serde_json::json!(task_id.to_string()));
        map.insert("status".into(), serde_json::json!(format!("{}", status)));
        if let Some(msg) = message {
            map.insert("message".into(), serde_json::json!(msg));
        }
        let json = serde_json::to_string_pretty(&map)?;
        let path = self
            .remote_dir
            .join("status")
            .join(format!("{}.json", task_id));
        self.write_remote(&path.to_string_lossy(), &json)
    }

    fn remove_task(&self, task_id: &Uuid) -> Result<()> {
        // Remove both the .json command file and its .lock sidecar.
        self.remove_remote_matching("commands", task_id)
    }
}

fn parse_status_string(s: &str) -> TaskStatus {
    match s.to_lowercase().as_str() {
        "pending" => TaskStatus::Pending,
        "running" => TaskStatus::Running,
        "completed" | "success" => TaskStatus::Completed,
        "failed" | "error" => TaskStatus::Failed,
        "cancelled" => TaskStatus::Cancelled,
        "timeout" => TaskStatus::Timeout,
        _ => TaskStatus::Pending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::TaskType;
    use tempfile::TempDir;

    /// Build a bridge pointed at a local temp dir, with a fake ssh that
    /// executes the remote command locally (so tests need no real sshd).
    fn local_bridge(tmp: &TempDir) -> SshBridge {
        SshBridge {
            host: "localhost".into(),
            user: None,
            port: None,
            remote_dir: tmp.path().to_path_buf(),
            connect_timeout: 5,
            local: true,
        }
    }

    #[test]
    fn test_submit_writes_remote_file() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let task = Task::new("echo hi".into(), TaskType::Shell);
        b.submit_task(&task).unwrap();
        let dir = tmp.path().join("commands");
        let files: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(files.len(), 1, "任务文件必须写入远端 commands/");
        let content = std::fs::read_to_string(files[0].path()).unwrap();
        let parsed: Task = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed.command, "echo hi");
        assert_eq!(parsed.task_id, task.task_id);
    }

    #[test]
    fn test_read_task_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let task =
            Task::new("echo hi".into(), TaskType::Shell).with_env_var("K".into(), "V".into());
        b.submit_task(&task).unwrap();
        let got = b.read_task(&task.task_id).unwrap();
        assert_eq!(got.command, "echo hi");
        assert_eq!(got.env_vars.get("K").unwrap(), "V");
    }

    #[test]
    fn test_query_status_pending_then_completed() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let task = Task::new("echo hi".into(), TaskType::Shell);
        b.submit_task(&task).unwrap();

        // Pending while the command file is still there.
        let s = b.query_status(&task.task_id).unwrap();
        assert_eq!(s.status, TaskStatus::Pending);

        // Daemon consumes the command file and writes a result.
        let dir = tmp.path().join("commands");
        for f in std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()) {
            std::fs::remove_file(f.path()).unwrap();
        }
        b.write_result(
            &task.task_id,
            &TaskResult {
                task_id: task.task_id,
                status: TaskStatus::Completed,
                output: crate::core::models::TaskOutput {
                    stdout: "hi".into(),
                    stderr: String::new(),
                    exit_code: Some(0),
                },
                command: "echo hi".into(),
                start_time: chrono::Utc::now(),
                end_time: chrono::Utc::now(),
                duration_ms: 5,
                retries_used: 0,
                artifacts: vec![],
                error_message: None,
            },
        )
        .unwrap();

        let s = b.query_status(&task.task_id).unwrap();
        assert_eq!(s.status, TaskStatus::Completed);
        assert!(s.message.unwrap().contains("5ms"));
    }

    #[test]
    fn test_query_status_not_found() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let id = Uuid::new_v4();
        assert!(matches!(
            b.query_status(&id),
            Err(BifrostError::TaskNotFound(_))
        ));
    }

    #[test]
    fn test_get_result_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let task = Task::new("echo hi".into(), TaskType::Shell);
        b.write_result(
            &task.task_id,
            &TaskResult {
                task_id: task.task_id,
                status: TaskStatus::Failed,
                output: crate::core::models::TaskOutput {
                    stdout: String::new(),
                    stderr: "boom".into(),
                    exit_code: Some(1),
                },
                command: "echo hi".into(),
                start_time: chrono::Utc::now(),
                end_time: chrono::Utc::now(),
                duration_ms: 3,
                retries_used: 0,
                artifacts: vec![],
                error_message: Some("boom".into()),
            },
        )
        .unwrap();
        let r = b.get_result(&task.task_id).unwrap();
        assert_eq!(r.status, TaskStatus::Failed);
        assert_eq!(r.output.stderr, "boom");
    }

    #[test]
    fn test_list_pending_tasks_multiple() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let t1 = Task::new("cmd1".into(), TaskType::Shell);
        let t2 = Task::new("cmd2".into(), TaskType::Shell);
        b.submit_task(&t1).unwrap();
        b.submit_task(&t2).unwrap();
        let tasks = b.list_pending_tasks().unwrap();
        assert_eq!(tasks.len(), 2);
        let cmds: Vec<_> = tasks.iter().map(|t| t.command.as_str()).collect();
        assert!(cmds.contains(&"cmd1") && cmds.contains(&"cmd2"));
    }

    #[test]
    fn test_remove_task_deletes_command_file() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let task = Task::new("echo hi".into(), TaskType::Shell);
        b.submit_task(&task).unwrap();
        b.remove_task(&task.task_id).unwrap();
        assert!(b.read_task(&task.task_id).is_err());
    }

    #[test]
    fn test_write_status_parse() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        let id = Uuid::new_v4();
        b.write_status(&id, &TaskStatus::Running, Some("working"))
            .unwrap();
        let s = b.query_status(&id).unwrap();
        assert_eq!(s.status, TaskStatus::Running);
        assert_eq!(s.message.as_deref(), Some("working"));
    }

    #[test]
    fn test_write_remote_with_special_chars() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        // 命令含引号/美元符/中文——base64 传输必须原样保存
        let task = Task::new(
            "echo \"it's $HOME 你好\" && ls 'a b'".into(),
            TaskType::Shell,
        );
        b.submit_task(&task).unwrap();
        let got = b.read_task(&task.task_id).unwrap();
        assert_eq!(got.command, "echo \"it's $HOME 你好\" && ls 'a b'");
    }

    #[test]
    fn test_ssh_bridge_from_settings() {
        let cfg = SshSection {
            host: Some("h1".into()),
            user: Some("u1".into()),
            remote_dir: Some(PathBuf::from("/r")),
            port: Some(2222),
            connect_timeout: None,
        };
        let b = SshBridge::new(&cfg).unwrap();
        assert_eq!(b.host, "h1");
        assert_eq!(b.user.as_deref(), Some("u1"));
        assert_eq!(b.port, Some(2222));
        assert_eq!(b.remote_dir, PathBuf::from("/r"));

        // 缺 host 必须报错
        let bad = SshSection {
            host: None,
            remote_dir: Some(PathBuf::from("/r")),
            ..SshSection::default()
        };
        assert!(SshBridge::new(&bad).is_err());
    }

    #[test]
    fn test_sh_quote() {
        assert_eq!(sh_quote("/a/b"), "'/a/b'");
        assert_eq!(sh_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn test_empty_commands_dir_lists_nothing() {
        let tmp = TempDir::new().unwrap();
        let b = local_bridge(&tmp);
        assert!(b.list_pending_tasks().unwrap().is_empty());
    }
}
