// Integration test: Batch GPU scheduling workflow
// Test batch manifest submission -> GPU scheduling -> batch progress tracking

use bifrost::core::protocol::Protocol;
use bifrost::core::models::{TaskManifest, TaskItem, TaskType, TaskStatus};
use bifrost::core::batch_tracker::{BatchTracker, BatchStatus};
use bifrost::client::submit;
use tempfile::TempDir;
use std::path::PathBuf;
use std::fs;
use uuid::Uuid;

#[test]
fn test_batch_manifest_submission() {
    let temp_dir = TempDir::new().unwrap();
    let shared_storage = temp_dir.path().to_path_buf();
    let batch_progress_dir = temp_dir.path().join("batch_progress");

    // Setup: Create protocol and batch tracker
    let protocol = Protocol::new(shared_storage.clone())
        .expect("Failed to create protocol");

    let batch_tracker = BatchTracker::new(batch_progress_dir.clone());

    // Create test manifest
    let manifest = TaskManifest {
        batch_name: "Test Batch".to_string(),
        description: "Integration test batch".to_string(),
        tasks: vec![
            TaskItem {
                task_name: "test_task_1".to_string(),
                description: "First test task".to_string(),
                command: "echo task1".to_string(),
                task_type: TaskType::Shell,
                timeout: 10,
                priority: 0,
                working_dir: None,
                env_vars: std::collections::HashMap::new(),
                artifacts_expected: vec!["output1.log".to_string()],
                metadata: std::collections::HashMap::new(),
            },
            TaskItem {
                task_name: "test_task_2".to_string(),
                description: "Second test task".to_string(),
                command: "echo task2".to_string(),
                task_type: TaskType::Shell,
                timeout: 10,
                priority: 5,
                working_dir: None,
                env_vars: std::collections::HashMap::new(),
                artifacts_expected: vec!["output2.log".to_string()],
                metadata: std::collections::HashMap::new(),
            },
        ],
    };

    // Write manifest to file
    let manifest_path = temp_dir.path().join("test_manifest.json");
    let manifest_json = serde_json::to_string(&manifest).unwrap();
    fs::write(&manifest_path, manifest_json).unwrap();

    // Submit batch manifest
    let batch_id = submit::submit_batch_manifest(&protocol, &batch_tracker, &manifest_path)
        .expect("Failed to submit batch manifest");

    // Verify batch ID generated
    assert!(!batch_id.is_nil(), "Batch ID should not be nil");

    // Verify tasks submitted to commands directory
    let commands_dir = shared_storage.join("commands");
    assert!(commands_dir.exists(), "Commands directory should exist");

    let command_files: Vec<_> = fs::read_dir(&commands_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();

    assert_eq!(command_files.len(), 2, "Should have 2 task files submitted");

    // Verify batch progress file created
    let progress = batch_tracker.load_progress(batch_id)
        .expect("Failed to load batch progress");

    assert_eq!(progress.batch_id, batch_id);
    assert_eq!(progress.total_tasks, 2);
    assert_eq!(progress.status, BatchStatus::Running);
    assert_eq!(progress.submitted_tasks.len(), 2);

    // Verify each submitted task has batch_id set
    for (_, task_id, task_name) in &progress.submitted_tasks {
        let task = protocol.read_task(&task_id)
            .expect("Failed to read submitted task");

        assert_eq!(task.batch_id, Some(batch_id));
        assert_eq!(task.task_name, Some(task_name.clone()));
    }
}

#[test]
fn test_batch_progress_tracking() {
    let temp_dir = TempDir::new().unwrap();
    let batch_progress_dir = temp_dir.path().join("batch_progress");

    let batch_tracker = BatchTracker::new(batch_progress_dir.clone());

    // Create initial batch progress
    let batch_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    let progress = bifrost::core::models::BatchProgress {
        batch_id,
        manifest_path: PathBuf::from("/test/manifest.json"),
        total_tasks: 5,
        current_index: 0,
        submitted_tasks: vec![
            (0, Uuid::new_v4(), "task_0".to_string()),
            (1, Uuid::new_v4(), "task_1".to_string()),
        ],
        completed_tasks: vec![
            (Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string()),
        ],
        status: BatchStatus::Running,
        created_at: now,
        updated_at: now,
    };

    // Save progress
    batch_tracker.save_progress(&progress)
        .expect("Failed to save batch progress");

    // Load progress
    let loaded = batch_tracker.load_progress(batch_id)
        .expect("Failed to load batch progress");

    assert_eq!(loaded.batch_id, batch_id);
    assert_eq!(loaded.total_tasks, 5);
    assert_eq!(loaded.completed_tasks.len(), 1);
    assert_eq!(loaded.status, BatchStatus::Running);

    // Update progress with task completion
    let mut updated = loaded.clone();
    updated.completed_tasks.push((
        Uuid::new_v4(),
        TaskStatus::Completed,
        "task_1".to_string(),
    ));
    updated.updated_at = chrono::Utc::now();

    batch_tracker.save_progress(&updated)
        .expect("Failed to save updated progress");

    // Load again to verify update
    let reloaded = batch_tracker.load_progress(batch_id)
        .expect("Failed to reload batch progress");

    assert_eq!(reloaded.completed_tasks.len(), 2);
}

#[test]
fn test_list_active_batches() {
    let temp_dir = TempDir::new().unwrap();
    let batch_progress_dir = temp_dir.path().join("batch_progress");
    fs::create_dir_all(&batch_progress_dir).unwrap();

    let batch_tracker = BatchTracker::new(batch_progress_dir.clone());

    // Create multiple batch progress files
    let running_batch_id = Uuid::new_v4();
    let completed_batch_id = Uuid::new_v4();
    let now = chrono::Utc::now();

    // Running batch
    let running_progress = bifrost::core::models::BatchProgress {
        batch_id: running_batch_id,
        manifest_path: PathBuf::from("/test/running.json"),
        total_tasks: 10,
        current_index: 3,
        submitted_tasks: vec![(0, Uuid::new_v4(), "task_0".to_string())],
        completed_tasks: vec![(Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string())],
        status: BatchStatus::Running,
        created_at: now,
        updated_at: now,
    };

    // Completed batch
    let completed_progress = bifrost::core::models::BatchProgress {
        batch_id: completed_batch_id,
        manifest_path: PathBuf::from("/test/completed.json"),
        total_tasks: 5,
        current_index: 5,
        submitted_tasks: vec![(0, Uuid::new_v4(), "task_0".to_string())],
        completed_tasks: vec![
            (Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string()),
            (Uuid::new_v4(), TaskStatus::Completed, "task_1".to_string()),
            (Uuid::new_v4(), TaskStatus::Completed, "task_2".to_string()),
            (Uuid::new_v4(), TaskStatus::Completed, "task_3".to_string()),
            (Uuid::new_v4(), TaskStatus::Completed, "task_4".to_string()),
        ],
        status: BatchStatus::Completed,
        created_at: now,
        updated_at: now,
    };

    // Save both batches
    batch_tracker.save_progress(&running_progress).unwrap();
    batch_tracker.save_progress(&completed_progress).unwrap();

    // List active batches (should only return Running batch)
    let active_batches = batch_tracker.list_active_batches()
        .expect("Failed to list active batches");

    assert_eq!(active_batches.len(), 1, "Should only list active batches");
    assert_eq!(active_batches[0].batch_id, running_batch_id);
    assert_eq!(active_batches[0].status, BatchStatus::Running);

    // Completed batch should not be in active list
    assert!(!active_batches.iter().any(|b| b.batch_id == completed_batch_id));
}

#[test]
fn test_batch_cleanup_old_batches() {
    let temp_dir = TempDir::new().unwrap();
    let batch_progress_dir = temp_dir.path().join("batch_progress");
    fs::create_dir_all(&batch_progress_dir).unwrap();

    let batch_tracker = BatchTracker::new(batch_progress_dir.clone());

    // Create old batch (7 days ago)
    let old_batch_id = Uuid::new_v4();
    let old_time = chrono::Utc::now() - chrono::Duration::days(8);

    let old_progress = bifrost::core::models::BatchProgress {
        batch_id: old_batch_id,
        manifest_path: PathBuf::from("/test/old.json"),
        total_tasks: 1,
        current_index: 1,
        submitted_tasks: vec![(0, Uuid::new_v4(), "task_0".to_string())],
        completed_tasks: vec![(Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string())],
        status: BatchStatus::Completed,
        created_at: old_time,
        updated_at: old_time,
    };

    // Create recent batch (1 day ago)
    let recent_batch_id = Uuid::new_v4();
    let recent_time = chrono::Utc::now() - chrono::Duration::days(1);

    let recent_progress = bifrost::core::models::BatchProgress {
        batch_id: recent_batch_id,
        manifest_path: PathBuf::from("/test/recent.json"),
        total_tasks: 1,
        current_index: 1,
        submitted_tasks: vec![(0, Uuid::new_v4(), "task_0".to_string())],
        completed_tasks: vec![(Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string())],
        status: BatchStatus::Completed,
        created_at: recent_time,
        updated_at: recent_time,
    };

    // Save both batches
    batch_tracker.save_progress(&old_progress).unwrap();
    batch_tracker.save_progress(&recent_progress).unwrap();

    // Cleanup batches older than 7 days
    let removed_count = batch_tracker.cleanup_old_batches(7)
        .expect("Failed to cleanup old batches");

    assert_eq!(removed_count, 1, "Should remove 1 old batch");

    // Verify old batch removed, recent batch still exists
    assert!(batch_tracker.load_progress(old_batch_id).is_err(), "Old batch should be removed");
    assert!(batch_tracker.load_progress(recent_batch_id).is_ok(), "Recent batch should still exist");
}