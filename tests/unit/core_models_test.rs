// Unit tests for core data models
use bifrost::core::models::{Task, TaskType};
use chrono::Utc;
use std::collections::HashMap;
use std::path::PathBuf;
use uuid::Uuid;

#[test]
fn test_task_creation() {
    let task = Task {
        task_id: Uuid::new_v4(),
        timestamp: Utc::now(),
        command: "pytest tests/test.py".to_string(),
        task_type: TaskType::Pytest,
        priority: 0,
        timeout: 300,
        retry_count: 3,
        shell: None,
        env_vars: HashMap::new(),
        working_dir: PathBuf::from("/tmp"),
        artifacts_expected: vec!["report.json".to_string()],
        metadata: HashMap::new(),
        batch_id: None,
        task_name: None,
    };

    assert_eq!(task.command, "pytest tests/test.py");
    assert_eq!(task.timeout, 300);
}
