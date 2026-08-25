// Core data models for bifrost
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;
use std::path::PathBuf;
use uuid::Uuid;

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

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Pending => write!(f, "Pending"),
            TaskStatus::Running => write!(f, "Running"),
            TaskStatus::Completed => write!(f, "Completed"),
            TaskStatus::Failed => write!(f, "Failed"),
            TaskStatus::Cancelled => write!(f, "Cancelled"),
            TaskStatus::Timeout => write!(f, "Timeout"),
        }
    }
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
    /// Batch ID if this task belongs to a batch
    pub batch_id: Option<Uuid>,
    /// Task name (for batch tasks)
    pub task_name: Option<String>,
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
    /// The original command that produced this result (for traceability)
    #[serde(default)]
    pub command: String,
    /// Execution start time
    pub start_time: DateTime<Utc>,
    /// Execution end time
    pub end_time: DateTime<Utc>,
    /// Execution duration in milliseconds (derived from start/end time)
    #[serde(default)]
    pub duration_ms: i64,
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
            working_dir: PathBuf::from("."),
            artifacts_expected: Vec::new(),
            metadata: HashMap::new(),
            batch_id: None,
            task_name: None,
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

    /// Set batch ID (for batch tasks)
    pub fn with_batch_id(mut self, batch_id: Uuid) -> Self {
        self.batch_id = Some(batch_id);
        self
    }

    /// Set task name (for batch tasks)
    pub fn with_task_name(mut self, task_name: String) -> Self {
        self.task_name = Some(task_name);
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

    /// Get execution duration in milliseconds.
    /// Falls back to computing from start/end time when the field was not
    /// populated (e.g. results written by older versions).
    pub fn duration_ms(&self) -> i64 {
        if self.duration_ms > 0 {
            self.duration_ms
        } else {
            (self.end_time - self.start_time).num_milliseconds()
        }
    }
}

/// Task item within a batch manifest
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TaskItem {
    /// Task name identifier
    pub task_name: String,
    /// Task description
    pub description: String,
    /// Command to execute
    pub command: String,
    /// Type of task
    pub task_type: TaskType,
    /// Timeout in seconds
    pub timeout: u64,
    /// Priority (0-255, lower is higher priority)
    pub priority: u8,
    /// Working directory for command execution
    pub working_dir: Option<PathBuf>,
    /// Environment variables for the task
    pub env_vars: HashMap<String, String>,
    /// Expected artifact files
    pub artifacts_expected: Vec<String>,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Batch task manifest - defines a batch of tasks to execute
#[derive(Serialize, Deserialize, Debug)]
pub struct TaskManifest {
    /// Batch name identifier
    pub batch_name: String,
    /// Batch description
    pub description: String,
    /// List of tasks in the batch
    pub tasks: Vec<TaskItem>,
}

/// Batch status enumeration
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum BatchStatus {
    /// Batch is being submitted
    Submitting,
    /// Batch is currently running
    Running,
    /// Batch completed successfully
    Completed,
    /// Batch failed
    Failed,
    /// Batch was cancelled
    Cancelled,
}

impl fmt::Display for BatchStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BatchStatus::Submitting => write!(f, "Submitting"),
            BatchStatus::Running => write!(f, "Running"),
            BatchStatus::Completed => write!(f, "Completed"),
            BatchStatus::Failed => write!(f, "Failed"),
            BatchStatus::Cancelled => write!(f, "Cancelled"),
        }
    }
}

/// Batch progress tracking
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BatchProgress {
    /// Unique batch identifier
    pub batch_id: Uuid,
    /// Path to the manifest file
    pub manifest_path: PathBuf,
    /// Total number of tasks in batch
    pub total_tasks: usize,
    /// Current task index being processed
    pub current_index: usize,
    /// Submitted tasks: (index, task_id, task_name)
    pub submitted_tasks: Vec<(usize, Uuid, String)>,
    /// Completed tasks: (task_id, status, task_name)
    pub completed_tasks: Vec<(Uuid, TaskStatus, String)>,
    /// Current batch status
    pub status: BatchStatus,
    /// Creation timestamp
    pub created_at: DateTime<Utc>,
    /// Last update timestamp
    pub updated_at: DateTime<Utc>,
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
            command: "echo test".to_string(),
            start_time: start,
            end_time: end,
            duration_ms: 0,
            retries_used: 0,
            artifacts: Vec::new(),
            error_message: None,
        };

        assert_eq!(result.duration_secs(), 42);
        assert!(result.is_success());
    }
}
