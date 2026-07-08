// Client submit functionality
use crate::core::models::{Task, TaskType, TaskManifest, BatchProgress, BatchStatus};
use crate::core::protocol::Protocol;
use crate::core::batch_tracker::BatchTracker;
use crate::core::error::{BifrostError, Result};
use std::path::PathBuf;
use uuid::Uuid;
use chrono::Utc;

/// Submit a task to the daemon via shared storage
pub fn submit_task(
    protocol: &Protocol,
    command: String,
    task_type: TaskType,
    priority: u8,
    timeout: u64,
    working_dir: Option<PathBuf>,
) -> Result<Uuid> {
    // Create task
    let task = Task::new(command.clone(), task_type.clone())
        .with_priority(priority)
        .with_timeout(timeout);

    // Set working directory if provided
    let task = if let Some(wd) = working_dir {
        task.with_working_dir(wd)
    } else {
        task
    };

    // Get task ID before submission
    let task_id = task.task_id;

    // Submit via protocol
    protocol.submit_task(&task)?;

    Ok(task_id)
}

/// Create and submit a pytest task (convenience wrapper)
pub fn submit_pytest_task(
    protocol: &Protocol,
    test_path: String,
    priority: u8,
    timeout: u64,
    working_dir: Option<PathBuf>,
) -> Result<Uuid> {
    use crate::client::pytest::create_pytest_task;

    let task = create_pytest_task(&test_path, priority, timeout, working_dir);
    let task_id = task.task_id;

    protocol.submit_task(&task)?;

    Ok(task_id)
}

/// Submit a batch manifest and create batch progress tracking
///
/// Reads a TaskManifest JSON file, generates UUID for batch, creates
/// BatchProgress file, and submits all tasks to daemon.
///
/// # Arguments
/// * `protocol` - Protocol instance for task submission
/// * `batch_tracker` - BatchTracker for progress tracking
/// * `manifest_path` - Path to TaskManifest JSON file
///
/// # Returns
/// * `Ok(Uuid)` - Batch ID for tracking
/// * `Err(Error)` - Submission failed
pub fn submit_batch_manifest(
    protocol: &Protocol,
    batch_tracker: &BatchTracker,
    manifest_path: &PathBuf,
) -> Result<Uuid> {
    // Read manifest file
    let manifest_json = std::fs::read_to_string(manifest_path)
        .map_err(BifrostError::IoError)?;

    let manifest: TaskManifest = serde_json::from_str(&manifest_json)
        .map_err(BifrostError::JsonError)?;

    // Generate batch ID
    let batch_id = Uuid::new_v4();
    let now = Utc::now();

    // Create initial BatchProgress
    let mut progress = BatchProgress {
        batch_id,
        manifest_path: manifest_path.clone(),
        total_tasks: manifest.tasks.len(),
        current_index: 0,
        submitted_tasks: Vec::new(),
        completed_tasks: Vec::new(),
        status: BatchStatus::Submitting,
        created_at: now,
        updated_at: now,
    };

    // Submit each task in the manifest
    for (index, task_item) in manifest.tasks.iter().enumerate() {
        // Convert TaskItem to Task
        let task = Task::new(task_item.command.clone(), task_item.task_type.clone())
            .with_timeout(task_item.timeout)
            .with_priority(task_item.priority)
            .with_working_dir(task_item.working_dir.clone().unwrap_or_else(|| PathBuf::from(".")))
            .with_batch_id(batch_id)
            .with_task_name(task_item.task_name.clone());

        // Add environment variables
        let task = task_item.env_vars.iter().fold(task, |t, (k, v)| {
            t.with_env_var(k.clone(), v.clone())
        });

        // Submit task
        let task_id = task.task_id;
        protocol.submit_task(&task)?;

        // Track submitted task
        progress.submitted_tasks.push((index, task_id, task_item.task_name.clone()));
        progress.current_index = index + 1;
        progress.updated_at = Utc::now();
    }

    // Update status to Running after all tasks submitted
    progress.status = BatchStatus::Running;
    progress.updated_at = Utc::now();

    // Save initial progress
    batch_tracker.save_progress(&progress)
        .map_err(|e| BifrostError::ConfigInvalid(e.to_string()))?;

    Ok(batch_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_submit_task() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = submit_task(
            &protocol,
            "echo hello".to_string(),
            TaskType::Shell,
            10,
            300,
            None,
        ).unwrap();

        // Verify task was created
        assert!(!task_id.is_nil());

        // Verify task file exists
        let commands_dir = temp_dir.path().join("commands");
        let files: Vec<_> = std::fs::read_dir(&commands_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();

        assert_eq!(files.len(), 1);
        assert!(files[0].file_name().to_string_lossy().contains(&task_id.to_string()));
    }

    #[test]
    fn test_submit_task_with_working_dir() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = submit_task(
            &protocol,
            "pytest tests/".to_string(),
            TaskType::Pytest,
            5,
            600,
            Some(PathBuf::from("/workspace")),
        ).unwrap();

        // Read back the task to verify working_dir was set
        let task = protocol.read_task(&task_id).unwrap();
        assert_eq!(task.working_dir, PathBuf::from("/workspace"));
    }

    #[test]
    fn test_submit_pytest_task() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = submit_pytest_task(
            &protocol,
            "tests/unit/".to_string(),
            10,
            300,
            None,
        ).unwrap();

        // Read back the task
        let task = protocol.read_task(&task_id).unwrap();

        assert_eq!(task.task_type, TaskType::Pytest);
        assert!(task.command.contains("--json-report"));
        assert!(task.artifacts_expected.contains(&"report.json".to_string()));
    }
}