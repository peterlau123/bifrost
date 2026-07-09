// Launcher - sequential job execution engine
use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;
use crate::client::status;
use crate::core::db::Database;
use crate::core::error::{BifrostError, Result as BifrostResult};
use crate::core::job::{JobDefinition, JobResult, JobTaskResult};
use crate::core::models::{Task, TaskStatus, TaskType};
use crate::core::protocol::Protocol;

const POLL: Duration = Duration::from_secs(2);

pub fn launch_job(protocol: &Protocol, db: Option<&Database>, job: JobDefinition) -> BifrostResult<JobResult> {
    let total = job.tasks.len();
    eprintln!("Job ''{}'' ({} tasks)", job.name, total);
    let mut jr = JobResult::new(job.name.clone(), total);
    for (i, ti) in job.tasks.iter().enumerate() {
        let label = format!("[{}/{}] {}", i+1, total, ti.name);
        eprintln!("{}", label);
        let task = Task::new(ti.command.clone(), TaskType::Custom)
            .with_priority(ti.priority).with_timeout(ti.timeout).with_retry_count(0);
        let tid = task.task_id;
        protocol.submit_task(&task).map_err(|e| BifrostError::ConfigInvalid(format!("submit: {}", e)))?;
        if let Some(ref db) = db { let _ = db.insert_task(&task, None); }
        let start = Instant::now();
        let limit = Duration::from_secs(ti.timeout + 30);
        loop {
            if start.elapsed() > limit {
                jr.record_task(JobTaskResult { name: ti.name.clone(), task_id: tid,
                    exit_code: None, status: "Timeout".into(), stdout: String::new(), stderr: String::new(),
                    duration_secs: start.elapsed().as_secs() as i64,
                    error_message: Some(format!("timeout {}s", ti.timeout)), artifacts: vec![] });
                break;
            }
            match status::query_status(protocol, tid) {
                Ok(s) => match s.status {
                    TaskStatus::Pending | TaskStatus::Running => { std::thread::sleep(POLL); }
                    _ => { let r = fetch_result(protocol, ti, tid, start.elapsed());
                        eprintln!("{} {}", label, s.status); jr.record_task(r); break; }
                },
                Err(BifrostError::TaskNotFound(_)) => { std::thread::sleep(POLL); }
                Err(e) => { jr.record_task(JobTaskResult { name: ti.name.clone(), task_id: tid,
                    exit_code: None, status: "Error".into(), stdout: String::new(), stderr: String::new(),
                    duration_secs: start.elapsed().as_secs() as i64,
                    error_message: Some(format!("query: {}", e)), artifacts: vec![] }); break; }
            }
        }
    }
    jr.finalize();
    eprintln!("Done: {} ok, {} failed in {}s", jr.completed_tasks, jr.failed_tasks, jr.total_duration_secs);
    Ok(jr)
}

fn fetch_result(protocol: &Protocol, ti: &crate::core::job::JobTask, tid: Uuid, elapsed: Duration) -> JobTaskResult {
    match crate::client::results::get_result(protocol, tid) {
        Ok(r) => JobTaskResult { name: ti.name.clone(), task_id: tid, exit_code: r.output.exit_code,
            status: format!("{}", r.status), stdout: r.output.stdout, stderr: r.output.stderr,
            duration_secs: r.duration_secs(), error_message: r.error_message, artifacts: r.artifacts },
        Err(_) => JobTaskResult { name: ti.name.clone(), task_id: tid, exit_code: None,
            status: "Completed".into(), stdout: String::new(), stderr: String::new(),
            duration_secs: elapsed.as_secs() as i64, error_message: None, artifacts: ti.artifacts.clone() },
    }
}
