// Test for TaskManifest parsing and batch task models
use bifrost::core::models::{TaskManifest, TaskItem, TaskType};
use std::path::PathBuf;

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
}
