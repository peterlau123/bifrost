// Client submit functionality
use crate::core::batch_tracker::BatchTracker;
use crate::core::error::{BifrostError, Result};
use crate::core::models::{BatchProgress, BatchStatus, Task, TaskManifest, TaskType};
use crate::core::protocol::Protocol;
use chrono::Utc;
use std::path::PathBuf;
use uuid::Uuid;

/// Submit a single command task to the daemon via shared storage
pub fn submit_task(
    protocol: &Protocol,
    command: String,
    task_type: TaskType,
    priority: u8,
    timeout: u64,
    working_dir: Option<PathBuf>,
) -> Result<Uuid> {
    let task = Task::new(command, task_type)
        .with_priority(priority)
        .with_timeout(timeout);

    let task = if let Some(wd) = working_dir {
        task.with_working_dir(wd)
    } else {
        task
    };

    let task_id = task.task_id;
    protocol.submit_task(&task)?;
    Ok(task_id)
}

/// Submit a batch manifest and create batch progress tracking
pub fn submit_batch_manifest(
    protocol: &Protocol,
    batch_tracker: &BatchTracker,
    manifest_path: &PathBuf,
) -> Result<Uuid> {
    let manifest_json = std::fs::read_to_string(manifest_path).map_err(BifrostError::IoError)?;
    let manifest: TaskManifest =
        serde_json::from_str(&manifest_json).map_err(BifrostError::JsonError)?;

    let batch_id = Uuid::new_v4();
    let now = Utc::now();

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

    for (index, task_item) in manifest.tasks.iter().enumerate() {
        let task = Task::new(task_item.command.clone(), task_item.task_type.clone())
            .with_timeout(task_item.timeout)
            .with_priority(task_item.priority)
            .with_working_dir(
                task_item.working_dir.clone().unwrap_or_else(|| PathBuf::from(".")),
            )
            .with_batch_id(batch_id)
            .with_task_name(task_item.task_name.clone());

        let task = task_item.env_vars.iter()
            .fold(task, |t, (k, v)| t.with_env_var(k.clone(), v.clone()));

        let task_id = task.task_id;
        protocol.submit_task(&task)?;

        progress.submitted_tasks.push((index, task_id, task_item.task_name.clone()));
        progress.current_index = index + 1;
        progress.updated_at = Utc::now();
    }

    progress.status = BatchStatus::Running;
    progress.updated_at = Utc::now();

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

        assert!(!task_id.is_nil());

        let commands_dir = temp_dir.path().join("commands");
        let files: Vec<_> = std::fs::read_dir(&commands_dir)
            .unwrap().filter_map(|e| e.ok()).collect();
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
            5, 600,
            Some(PathBuf::from("/workspace")),
        ).unwrap();

        let task = protocol.read_task(&task_id).unwrap();
        assert_eq!(task.working_dir, PathBuf::from("/workspace"));
    }
}
