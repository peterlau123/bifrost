// Job definition
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobDefinition {
    pub name: String,
    pub description: Option<String>,
    pub tasks: Vec<JobTask>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobTask {
    pub name: String,
    pub command: String,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
    #[serde(default)]
    pub priority: u8,
    pub working_dir: Option<PathBuf>,
    #[serde(default)]
    pub env_vars: HashMap<String, String>,
    #[serde(default)]
    pub artifacts: Vec<String>,
    #[serde(default)]
    pub ignore_failure: bool,
    #[serde(default)]
    pub metadata: HashMap<String, String>,
}
fn default_timeout() -> u64 {
    300
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobTaskResult {
    pub name: String,
    pub task_id: Uuid,
    pub exit_code: Option<i32>,
    pub status: String,
    pub stdout: String,
    pub stderr: String,
    pub duration_secs: i64,
    pub error_message: Option<String>,
    pub artifacts: Vec<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct JobResult {
    pub name: String,
    pub status: String,
    pub task_results: Vec<JobTaskResult>,
    pub total_duration_secs: i64,
    pub launched_at: DateTime<Utc>,
    pub total_tasks: usize,
    pub completed_tasks: usize,
    pub failed_tasks: usize,
}
impl JobResult {
    pub fn new(name: String, total_tasks: usize) -> Self {
        Self {
            name,
            status: "Running".to_string(),
            task_results: Vec::with_capacity(total_tasks),
            total_duration_secs: 0,
            launched_at: Utc::now(),
            total_tasks,
            completed_tasks: 0,
            failed_tasks: 0,
        }
    }
    pub fn record_task(&mut self, result: JobTaskResult) {
        if result.status != "Completed" {
            self.failed_tasks += 1;
        } else {
            self.completed_tasks += 1;
        }
        self.task_results.push(result);
    }
    pub fn finalize(&mut self) {
        let elapsed = Utc::now() - self.launched_at;
        self.total_duration_secs = elapsed.num_seconds();
        self.status = if self.failed_tasks == 0 {
            "Completed".to_string()
        } else {
            "CompletedWithFailures".to_string()
        };
    }
}
pub fn load_job(path: &PathBuf) -> Result<JobDefinition, String> {
    let c = std::fs::read_to_string(path).map_err(|e| format!("Failed: {}", e))?;
    serde_yaml::from_str(&c).map_err(|e| format!("YAML: {}", e))
}
