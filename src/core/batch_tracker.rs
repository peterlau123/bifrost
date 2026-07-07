// BatchTracker - Manages batch progress file operations
use std::fs;
use std::path::{Path, PathBuf};
use serde::{Serialize, Deserialize};
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Re-export from models for convenience
pub use crate::core::models::{BatchProgress, BatchStatus, TaskStatus};

/// Error type for batch tracker operations
#[derive(Debug)]
pub enum BatchTrackerError {
    Io(std::io::Error),
    Json(serde_json::Error),
    NotFound(String),
}

impl std::fmt::Display for BatchTrackerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BatchTrackerError::Io(e) => write!(f, "IO error: {}", e),
            BatchTrackerError::Json(e) => write!(f, "JSON error: {}", e),
            BatchTrackerError::NotFound(s) => write!(f, "Not found: {}", s),
        }
    }
}

impl std::error::Error for BatchTrackerError {}

impl From<std::io::Error> for BatchTrackerError {
    fn from(e: std::io::Error) -> Self {
        BatchTrackerError::Io(e)
    }
}

impl From<serde_json::Error> for BatchTrackerError {
    fn from(e: serde_json::Error) -> Self {
        BatchTrackerError::Json(e)
    }
}

/// Tracks batch progress through file operations
#[derive(Debug, Clone)]
pub struct BatchTracker {
    /// Directory where batch progress files are stored
    pub batch_dir: PathBuf,
}

impl BatchTracker {
    /// Create a new BatchTracker with the specified batch directory
    pub fn new(batch_dir: PathBuf) -> Self {
        Self { batch_dir }
    }

    /// Get the progress file path for a batch
    fn progress_file_path(&self, batch_id: Uuid) -> PathBuf {
        self.batch_dir.join(format!("{}.json", batch_id))
    }

    /// Save batch progress to a JSON file
    ///
    /// # Arguments
    /// * `progress` - The BatchProgress to save
    ///
    /// # Returns
    /// * `Ok(())` if successful
    /// * `Err(BatchTrackerError)` if an error occurs
    pub fn save_progress(&self, progress: &BatchProgress) -> Result<(), BatchTrackerError> {
        // Ensure the batch directory exists
        fs::create_dir_all(&self.batch_dir)?;

        let file_path = self.progress_file_path(progress.batch_id);
        let json = serde_json::to_string_pretty(progress)?;
        fs::write(&file_path, json)?;

        Ok(())
    }

    /// Load batch progress from a JSON file
    ///
    /// # Arguments
    /// * `batch_id` - The UUID of the batch to load
    ///
    /// # Returns
    /// * `Ok(BatchProgress)` if found and parsed successfully
    /// * `Err(BatchTrackerError)` if file not found or parse error
    pub fn load_progress(&self, batch_id: Uuid) -> Result<BatchProgress, BatchTrackerError> {
        let file_path = self.progress_file_path(batch_id);

        if !file_path.exists() {
            return Err(BatchTrackerError::NotFound(format!(
                "Progress file for batch {} not found",
                batch_id
            )));
        }

        let json = fs::read_to_string(&file_path)?;
        let progress = serde_json::from_str(&json)?;

        Ok(progress)
    }

    /// List all active (non-completed) batches
    ///
    /// # Returns
    /// * `Ok(Vec<BatchProgress>)` - List of active batch progress files
    pub fn list_active_batches(&self) -> Result<Vec<BatchProgress>, BatchTrackerError> {
        let mut active_batches = Vec::new();

        if !self.batch_dir.exists() {
            return Ok(active_batches);
        }

        for entry in fs::read_dir(&self.batch_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Only process .json files
            if path.extension().map_or(false, |ext| ext == "json") {
                let json = fs::read_to_string(&path)?;
                if let Ok(progress) = serde_json::from_str::<BatchProgress>(&json) {
                    // Only include non-completed batches
                    if progress.status != BatchStatus::Completed &&
                       progress.status != BatchStatus::Failed &&
                       progress.status != BatchStatus::Cancelled {
                        active_batches.push(progress);
                    }
                }
            }
        }

        Ok(active_batches)
    }

    /// Cleanup old batch progress files
    ///
    /// # Arguments
    /// * `older_than_days` - Remove batches older than this many days
    ///
    /// # Returns
    /// * `Ok(usize)` - Number of files removed
    pub fn cleanup_old_batches(&self, older_than_days: i64) -> Result<usize, BatchTrackerError> {
        let mut removed_count = 0;

        if !self.batch_dir.exists() {
            return Ok(0);
        }

        let cutoff_time = Utc::now() - chrono::Duration::days(older_than_days);

        for entry in fs::read_dir(&self.batch_dir)? {
            let entry = entry?;
            let path = entry.path();

            // Only process .json files
            if path.extension().map_or(false, |ext| ext == "json") {
                if let Ok(json) = fs::read_to_string(&path) {
                    if let Ok(progress) = serde_json::from_str::<BatchProgress>(&json) {
                        if progress.updated_at < cutoff_time {
                            fs::remove_file(&path)?;
                            removed_count += 1;
                        }
                    }
                }
            }
        }

        Ok(removed_count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_save_and_load_progress() {
        let temp_dir = TempDir::new().unwrap();
        let batch_dir = temp_dir.path().join("batch_progress");
        fs::create_dir_all(&batch_dir).unwrap();

        let tracker = BatchTracker::new(batch_dir);
        let batch_id = Uuid::new_v4();

        let progress = BatchProgress {
            batch_id,
            manifest_path: PathBuf::from("/test/tasks.json"),
            total_tasks: 10,
            current_index: 0,
            submitted_tasks: vec![(0, Uuid::new_v4(), "task_0".to_string())],
            completed_tasks: vec![(Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string())],
            status: BatchStatus::Running,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        tracker.save_progress(&progress).unwrap();
        let loaded = tracker.load_progress(batch_id).unwrap();

        assert_eq!(loaded.batch_id, batch_id);
        assert_eq!(loaded.total_tasks, 10);
        assert_eq!(loaded.current_index, 0);
        assert_eq!(loaded.status, BatchStatus::Running);
    }
}
