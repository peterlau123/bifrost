// Core data models for bifrost
use serde::{Serialize, Deserialize};
use uuid::Uuid;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;

/// Task type enumeration
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TaskType {
    /// Pytest test execution
    Pytest,
    /// Shell command execution
    Shell,
    /// Custom command type
    Custom,
}

/// Task status enumeration
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    /// Task is waiting to be executed
    Pending,
    /// Task is currently being executed
    Running,
    /// Task completed successfully
    Completed,
    /// Task failed
    Failed,
    /// Task was cancelled
    Cancelled,
    /// Task timed out
    Timeout,
}

/// Task definition - represents a single command execution task
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Task {
    /// Unique task identifier
    pub task_id: Uuid,
    /// Task creation timestamp
    pub timestamp: DateTime<Utc>,
    /// Command to execute
    pub command: String,
    /// Type of task
    pub task_type: TaskType,
    /// Task priority (0-255, lower is higher priority)
    pub priority: u8,
    /// Timeout in seconds
    pub timeout: u64,
    /// Number of retry attempts
    pub retry_count: u8,
    /// Environment variables for the task
    pub env_vars: HashMap<String, String>,
    /// Working directory for command execution
    pub working_dir: PathBuf,
    /// Expected artifact files
    pub artifacts_expected: Vec<String>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Task output - captures stdout and stderr
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskOutput {
    /// Standard output
    pub stdout: String,
    /// Standard error
    pub stderr: String,
    /// Exit code
    pub exit_code: Option<i32>,
}

/// Task result - the final result of task execution
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskResult {
    /// Task ID this result belongs to
    pub task_id: Uuid,
    /// Final status
    pub status: TaskStatus,
    /// Task output
    pub output: TaskOutput,
    /// Execution start time
    pub start_time: DateTime<Utc>,
    /// Execution end time
    pub end_time: DateTime<Utc>,
    /// Number of retries used
    pub retries_used: u8,
    /// Artifact paths generated
    pub artifacts: Vec<String>,
    /// Error message if failed
    pub error_message: Option<String>,
}

/// Heartbeat information for task monitoring
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HeartbeatInfo {
    /// Task ID
    pub task_id: Uuid,
    /// Last heartbeat timestamp
    pub last_heartbeat: DateTime<Utc>,
    /// Process ID
    pub pid: Option<u32>,
    /// Current progress (0-100)
    pub progress: u8,
}

impl Task {
    /// Create a new task with default values
    pub fn new(command: String, task_type: TaskType) -> Self {
        Self {
            task_id: Uuid::new_v4(),
            timestamp: Utc::now(),
            command,
            task_type,
            priority: 0,
            timeout: 300,
            retry_count: 3,
            env_vars: HashMap::new(),
            working_dir: std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")),
            artifacts_expected: Vec::new(),
            metadata: HashMap::new(),
        }
    }

    /// Set priority
    pub fn with_priority(mut self, priority: u8) -> Self {
        self.priority = priority;
        self
    }

    /// Set timeout
    pub fn with_timeout(mut self, timeout: u64) -> Self {
        self.timeout = timeout;
        self
    }

    /// Set retry count
    pub fn with_retry_count(mut self, retry_count: u8) -> Self {
        self.retry_count = retry_count;
        self
    }

    /// Set working directory
    pub fn with_working_dir(mut self, working_dir: PathBuf) -> Self {
        self.working_dir = working_dir;
        self
    }

    /// Add environment variable
    pub fn with_env_var(mut self, key: String, value: String) -> Self {
        self.env_vars.insert(key, value);
        self
    }

    /// Add expected artifact
    pub fn with_artifact(mut self, artifact: String) -> Self {
        self.artifacts_expected.push(artifact);
        self
    }
}

impl TaskResult {
    /// Check if the task was successful
    pub fn is_success(&self) -> bool {
        self.status == TaskStatus::Completed
    }

    /// Get execution duration in seconds
    pub fn duration_secs(&self) -> i64 {
        (self.end_time - self.start_time).num_seconds()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_builder() {
        let task = Task::new("pytest tests/".to_string(), TaskType::Pytest)
            .with_priority(10)
            .with_timeout(600)
            .with_retry_count(5)
            .with_working_dir(PathBuf::from("/workspace"))
            .with_env_var("DEBUG".to_string(), "1".to_string())
            .with_artifact("report.json".to_string());

        assert_eq!(task.command, "pytest tests/");
        assert_eq!(task.priority, 10);
        assert_eq!(task.timeout, 600);
        assert_eq!(task.retry_count, 5);
        assert_eq!(task.working_dir, PathBuf::from("/workspace"));
        assert_eq!(task.env_vars.get("DEBUG"), Some(&"1".to_string()));
        assert!(task.artifacts_expected.contains(&"report.json".to_string()));
    }

    #[test]
    fn test_task_result_duration() {
        let start = Utc::now();
        let end = start + chrono::Duration::seconds(42);

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

        assert_eq!(result.duration_secs(), 42);
        assert!(result.is_success());
    }
}