// Launcher - sequential job execution engine
use crate::client::status;
use crate::core::bridge::Bridge;
use crate::core::error::{BifrostError, Result as BifrostResult};
use crate::core::job::{JobDefinition, JobResult, JobTaskResult};
use crate::core::models::{Task, TaskStatus, TaskType};
use std::time::{Duration, Instant};
use uuid::Uuid;

const POLL: Duration = Duration::from_secs(2);

pub fn launch_job(bridge: &dyn Bridge, job: JobDefinition) -> BifrostResult<JobResult> {
    let total = job.tasks.len();
    eprintln!("Job '{}' ({} tasks)", job.name, total);
    let mut jr = JobResult::new(job.name.clone(), total);
    for (i, ti) in job.tasks.iter().enumerate() {
        let label = format!("[{}/{}] {}", i + 1, total, ti.name);
        eprintln!("{}", label);
        let mut task = Task::new(ti.command.clone(), TaskType::Custom)
            .with_priority(ti.priority)
            .with_timeout(ti.timeout)
            .with_retry_count(0);
        // JobTask 的 working_dir / env_vars 必须传递, 否则 YAML 中配置被静默忽略
        if let Some(wd) = &ti.working_dir {
            task = task.with_working_dir(wd.clone());
        }
        for (k, v) in &ti.env_vars {
            task = task.with_env_var(k.clone(), v.clone());
        }
        let tid = task.task_id;
        bridge
            .submit_task(&task)
            .map_err(|e| BifrostError::ConfigInvalid(format!("submit: {}", e)))?;
        let start = Instant::now();
        let limit = Duration::from_secs(ti.timeout + 30);
        loop {
            if start.elapsed() > limit {
                jr.record_task(JobTaskResult {
                    name: ti.name.clone(),
                    task_id: tid,
                    exit_code: None,
                    status: "Timeout".into(),
                    stdout: String::new(),
                    stderr: String::new(),
                    duration_secs: start.elapsed().as_secs() as i64,
                    error_message: Some(format!("timeout {}s", ti.timeout)),
                    artifacts: vec![],
                });
                break;
            }
            match status::query_status(bridge, tid) {
                Ok(s) => match s.status {
                    TaskStatus::Pending | TaskStatus::Running => {
                        std::thread::sleep(POLL);
                    }
                    _ => {
                        let r = fetch_result(bridge, ti, tid, start.elapsed());
                        eprintln!("{} {}", label, s.status);
                        jr.record_task(r);
                        break;
                    }
                },
                Err(BifrostError::TaskNotFound(_)) => {
                    std::thread::sleep(POLL);
                }
                Err(e) => {
                    jr.record_task(JobTaskResult {
                        name: ti.name.clone(),
                        task_id: tid,
                        exit_code: None,
                        status: "Error".into(),
                        stdout: String::new(),
                        stderr: String::new(),
                        duration_secs: start.elapsed().as_secs() as i64,
                        error_message: Some(format!("query: {}", e)),
                        artifacts: vec![],
                    });
                    break;
                }
            }
        }
    }
    jr.finalize();
    eprintln!(
        "Done: {} ok, {} failed in {}s",
        jr.completed_tasks, jr.failed_tasks, jr.total_duration_secs
    );
    Ok(jr)
}

fn fetch_result(
    bridge: &dyn Bridge,
    ti: &crate::core::job::JobTask,
    tid: Uuid,
    elapsed: Duration,
) -> JobTaskResult {
    match crate::client::results::get_result(bridge, tid) {
        Ok(r) => JobTaskResult {
            name: ti.name.clone(),
            task_id: tid,
            exit_code: r.output.exit_code,
            status: format!("{}", r.status),
            stdout: r.output.stdout.clone(),
            stderr: r.output.stderr.clone(),
            duration_secs: r.duration_secs(),
            error_message: r.error_message,
            artifacts: r.artifacts,
        },
        Err(_) => JobTaskResult {
            name: ti.name.clone(),
            task_id: tid,
            exit_code: None,
            status: "Completed".into(),
            stdout: String::new(),
            stderr: String::new(),
            duration_secs: elapsed.as_secs() as i64,
            error_message: None,
            artifacts: ti.artifacts.clone(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bridge::TaskStatusResponse;
    use crate::core::error::Result as BifrostResult;
    use crate::core::job::JobTask;
    use crate::core::models::TaskResult;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};

    /// Mock bridge that captures submitted tasks and reports them Completed.
    /// Lets launch_job run without a real server.
    struct MockBridge {
        submitted: Arc<Mutex<Vec<Task>>>,
    }
    impl MockBridge {
        fn new() -> (Self, Arc<Mutex<Vec<Task>>>) {
            let submitted = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    submitted: submitted.clone(),
                },
                submitted,
            )
        }
    }
    impl Bridge for MockBridge {
        fn submit_task(&self, task: &Task) -> BifrostResult<()> {
            self.submitted.lock().unwrap().push(task.clone());
            Ok(())
        }
        fn query_status(&self, _task_id: &Uuid) -> BifrostResult<TaskStatusResponse> {
            Ok(TaskStatusResponse {
                task_id: Uuid::nil(),
                status: TaskStatus::Completed,
                message: None,
            })
        }
        fn get_result(&self, _task_id: &Uuid) -> BifrostResult<TaskResult> {
            Err(BifrostError::TaskNotFound(Uuid::nil()))
        }
        fn list_pending_tasks(&self) -> BifrostResult<Vec<Task>> {
            Ok(vec![])
        }
        fn read_task(&self, _task_id: &Uuid) -> BifrostResult<Task> {
            Err(BifrostError::TaskNotFound(Uuid::nil()))
        }
        fn write_result(&self, _task_id: &Uuid, _result: &TaskResult) -> BifrostResult<()> {
            Ok(())
        }
        fn write_status(
            &self,
            _task_id: &Uuid,
            _status: &TaskStatus,
            _message: Option<&str>,
        ) -> BifrostResult<()> {
            Ok(())
        }
        fn remove_task(&self, _task_id: &Uuid) -> BifrostResult<()> {
            Ok(())
        }
    }

    /// JobTask 的 working_dir / env_vars 必须传递到提交的 Task 上 (回归测试).
    /// Bug: launcher 之前只设置 priority/timeout, YAML 中的 wd/env 被静默忽略.
    #[test]
    fn test_launch_job_passes_working_dir_and_env() {
        let job = JobDefinition {
            name: "t".into(),
            description: None,
            tasks: vec![JobTask {
                name: "check-env".into(),
                command: "pwd".into(),
                timeout: 30,
                priority: 5,
                working_dir: Some(PathBuf::from("/tmp")),
                env_vars: [("MY_VAR".to_string(), "job-env-ok".to_string())].into(),
                artifacts: vec![],
                ignore_failure: false,
                metadata: Default::default(),
            }],
        };
        let (bridge, submitted) = MockBridge::new();
        let jr = launch_job(&bridge, job).unwrap();
        assert_eq!(jr.completed_tasks, 1);
        let t = submitted.lock().unwrap()[0].clone();
        assert_eq!(t.working_dir, PathBuf::from("/tmp"), "working_dir 必须传递");
        assert_eq!(
            t.env_vars.get("MY_VAR").map(|s| s.as_str()),
            Some("job-env-ok"),
            "env_vars 必须传递"
        );
        assert_eq!(t.priority, 5);
        assert_eq!(t.timeout, 30);
    }
}
