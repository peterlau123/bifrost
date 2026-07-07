// tests/unit/batch_tracker_test.rs
use bifrost::core::batch_tracker::{BatchTracker, BatchProgress, BatchStatus};
use bifrost::core::models::TaskStatus;
use tempfile::TempDir;
use uuid::Uuid;
use chrono::Utc;
use std::path::PathBuf;

#[test]
fn test_save_and_load_progress() {
    let temp_dir = TempDir::new().unwrap();
    let batch_dir = temp_dir.path().join("batch_progress");
    std::fs::create_dir_all(&batch_dir).unwrap();

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
