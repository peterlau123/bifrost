// Test for TaskManifest parsing and batch task models
use bifrost::core::models::{TaskManifest, TaskItem, TaskType, BatchProgress, BatchStatus, TaskStatus};
use std::path::PathBuf;
use uuid::Uuid;
use chrono::Utc;

#[test]
fn test_parse_task_manifest() {
    let json = r#"
    {
      "batch_name": "Test Batch",
      "description": "Test batch execution",
      "tasks": [
        {
          "task_name": "test_task_1",
          "description": "First test task",
          "command": "pytest tests/test_a.py",
          "task_type": "pytest",
          "timeout": 600,
          "priority": 10,
          "working_dir": "/workspace",
          "env_vars": {"CUDA_DEVICE": "0"},
          "artifacts_expected": ["report.json"],
          "metadata": {"category": "performance"}
        }
      ]
    }
    "#;

    let manifest: TaskManifest = serde_json::from_str(json).unwrap();
    assert_eq!(manifest.batch_name, "Test Batch");
    assert_eq!(manifest.tasks.len(), 1);
    assert_eq!(manifest.tasks[0].task_type, TaskType::Pytest);

    // Complete field verification for TaskItem
    assert_eq!(manifest.tasks[0].task_name, "test_task_1");
    assert_eq!(manifest.tasks[0].description, "First test task");
    assert_eq!(manifest.tasks[0].command, "pytest tests/test_a.py");
    assert_eq!(manifest.tasks[0].timeout, 600);
    assert_eq!(manifest.tasks[0].priority, 10);
    assert_eq!(manifest.tasks[0].working_dir, Some(PathBuf::from("/workspace")));
    assert_eq!(manifest.tasks[0].env_vars.get("CUDA_DEVICE"), Some(&"0".to_string()));
    assert!(manifest.tasks[0].artifacts_expected.contains(&"report.json".to_string()));
    assert_eq!(manifest.tasks[0].metadata.get("category"), Some(&"performance".to_string()));
}

#[test]
fn test_batch_progress_serialization() {
    let progress = BatchProgress {
        batch_id: Uuid::new_v4(),
        manifest_path: PathBuf::from("/test/tasks.json"),
        total_tasks: 10,
        current_index: 5,
        submitted_tasks: vec![(0, Uuid::new_v4(), "task_0".to_string())],
        completed_tasks: vec![(Uuid::new_v4(), TaskStatus::Completed, "task_0".to_string())],
        status: BatchStatus::Running,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    };

    let json = serde_json::to_string(&progress).unwrap();
    let loaded: BatchProgress = serde_json::from_str(&json).unwrap();

    assert_eq!(loaded.total_tasks, 10);
    assert_eq!(loaded.current_index, 5);
    assert_eq!(loaded.status, BatchStatus::Running);
}
