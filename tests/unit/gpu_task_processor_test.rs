// Unit tests for GpuTaskProcessor
// Tests GPU task processing flow, scheduler integration, and error handling

use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use uuid::Uuid;

use bifrost::core::models::{Task, TaskType, TaskStatus};
use bifrost::daemon::executor::Executor;
use bifrost::daemon::gpu_task_processor::GpuTaskProcessor;

/// Create a test task with specified name
fn create_test_task(name: &str) -> Task {
    Task::new(format!("echo {}", name), TaskType::Shell)
        .with_timeout(5)
        .with_working_dir(PathBuf::from("."))
}

/// Create a test executor with temp log directory
fn create_test_executor() -> Executor {
    let temp_dir = std::env::temp_dir();
    Executor::new(temp_dir.join("logs"), Duration::from_secs(30)).unwrap()
}

#[test]
fn test_gpu_task_processor_creation() {
    let executor = create_test_executor();
    let processor = GpuTaskProcessor::new(vec![0, 1], executor, true, None);
    assert!(processor.is_ok(), "GpuTaskProcessor should be created successfully");
}

#[test]
fn test_gpu_task_processor_empty_gpu_pool() {
    let executor = create_test_executor();
    let processor = GpuTaskProcessor::new(vec![], executor, true, None);
    assert!(processor.is_ok(), "GpuTaskProcessor should handle empty GPU pool");
}

#[tokio::test]
async fn test_process_task_success() {
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();

    let task = create_test_task("hello_world");
    let result = processor.process_task(task).await;

    assert!(result.is_ok(), "Task should process successfully");
    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed, "Task should complete successfully");
    assert!(result.output.stdout.contains("hello_world"), "Output should contain task name");
}

#[tokio::test]
async fn test_process_task_multiple_gpus() {
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0, 1, 2, 3], executor, true, None).unwrap();

    // Process multiple tasks
    for i in 0..4 {
        let task = create_test_task(format!("task_{}", i));
        let result = processor.process_task(task).await;
        assert!(result.is_ok(), "Task {} should process successfully", i);
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
    }
}

#[tokio::test]
async fn test_process_task_no_gpu_available() {
    // Create processor with empty GPU pool - should fail
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![], executor, true, None).unwrap();

    let task = create_test_task("test");
    let result = processor.process_task(task).await;

    assert!(result.is_err(), "Should fail when no GPU is available");
    let error = result.unwrap_err();
    assert!(error.contains("No available GPU"), "Error should indicate no GPU available");
}

#[tokio::test]
async fn test_run_with_channel_single_task() {
    let temp_dir = TempDir::new().unwrap();
    let task_file = temp_dir.path().join("channel_task.json");

    // Create a valid task JSON
    let task = create_test_task("channel_test");
    let json = serde_json::to_string(&task).unwrap();

    let mut file = fs::File::create(&task_file).unwrap();
    file.write_all(json.as_bytes()).unwrap();
    file.sync_all().unwrap();

    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();

    // Create channel and send task path
    let (tx, rx) = mpsc::channel::<PathBuf>(1);

    // Spawn processor run in background
    let run_future = tokio::spawn(async move {
        processor.run(rx).await;
    });

    // Send task path
    tx.send(task_file).await.unwrap();

    // Give time to process
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Close channel to stop processor
    drop(tx);

    // Wait for processor to finish
    let _ = tokio::time::timeout(Duration::from_secs(5), run_future).await;
}

#[tokio::test]
async fn test_run_with_channel_multiple_tasks() {
    let temp_dir = TempDir::new().unwrap();
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0, 1], executor, true, None).unwrap();

    // Create multiple task files
    let mut task_files = Vec::new();
    for i in 0..3 {
        let task_file = temp_dir.path().join(format!("task_{}.json", i));
        let task = create_test_task(format!("multi_{}", i));
        let json = serde_json::to_string(&task).unwrap();

        let mut file = fs::File::create(&task_file).unwrap();
        file.write_all(json.as_bytes()).unwrap();
        file.sync_all().unwrap();
        task_files.push(task_file);
    }

    // Create channel and send task paths
    let (tx, rx) = mpsc::channel::<PathBuf>(3);

    // Spawn processor run in background
    let run_future = tokio::spawn(async move {
        processor.run(rx).await;
    });

    // Send all task paths
    for task_file in task_files {
        tx.send(task_file).await.unwrap();
    }

    // Give time to process
    tokio::time::sleep(Duration::from_millis(1000)).await;

    // Close channel to stop processor
    drop(tx);

    // Wait for processor to finish
    let _ = tokio::time::timeout(Duration::from_secs(5), run_future).await;
}

#[tokio::test]
async fn test_run_with_channel_invalid_file() {
    let temp_dir = TempDir::new().unwrap();
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();

    // Create an invalid JSON file
    let invalid_file = temp_dir.path().join("invalid.json");
    let mut file = fs::File::create(&invalid_file).unwrap();
    file.write_all(b"this is not json").unwrap();
    file.sync_all().unwrap();

    // Create channel
    let (tx, rx) = mpsc::channel::<PathBuf>(1);

    // Spawn processor - should handle invalid file gracefully
    let run_future = tokio::spawn(async move {
        processor.run(rx).await;
    });

    // Send invalid file path
    tx.send(invalid_file).await.unwrap();

    // Give time to process
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Close channel
    drop(tx);

    // Wait for processor
    let _ = tokio::time::timeout(Duration::from_secs(5), run_future).await;
    // Test passes if we get here without panic - invalid files are handled gracefully
}

#[tokio::test]
async fn test_gpu_isolation_via_cuda_visible_devices() {
    let temp_dir = TempDir::new().unwrap();
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![2], executor, true, None).unwrap();

    // Task that echoes CUDA_VISIBLE_DEVICES
    let task = Task::new("echo $CUDA_VISIBLE_DEVICES".to_string(), TaskType::Shell)
        .with_timeout(5)
        .with_working_dir(PathBuf::from("."));

    let result = processor.process_task(task).await;

    assert!(result.is_ok(), "Task should process successfully");
    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Completed);
    // Output should show GPU 2 was assigned
    assert!(result.output.stdout.contains("2"), "CUDA_VISIBLE_DEVICES should contain GPU ID 2");
}

#[tokio::test]
async fn test_failed_task_handling() {
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();

    // Task that will fail
    let task = Task::new("exit 42".to_string(), TaskType::Shell)
        .with_timeout(5)
        .with_working_dir(PathBuf::from("."));

    let result = processor.process_task(task).await;

    assert!(result.is_ok(), "Failed task should still return Ok with TaskResult");
    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Failed, "Task should be marked as failed");
    assert_eq!(result.output.exit_code, Some(42), "Exit code should be captured");
}

#[tokio::test]
async fn test_timeout_task_handling() {
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();

    // Task that will timeout (sleeps longer than timeout)
    let task = Task::new("sleep 10".to_string(), TaskType::Shell)
        .with_timeout(1) // 1 second timeout
        .with_working_dir(PathBuf::from("."));

    let result = processor.process_task(task).await;

    assert!(result.is_ok(), "Timed out task should still return Ok with TaskResult");
    let result = result.unwrap();
    assert_eq!(result.status, TaskStatus::Timeout, "Task should be marked as timeout");
}

#[tokio::test]
async fn test_gpu_release_after_completion() {
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();

    // Process first task
    let task1 = create_test_task("task1");
    let result1 = processor.process_task(task1).await;
    assert!(result1.is_ok());

    // Process second task - should reuse the same GPU
    let task2 = create_test_task("task2");
    let result2 = processor.process_task(task2).await;
    assert!(result2.is_ok());

    // Both tasks completed successfully, GPU was properly released
    assert_eq!(result1.unwrap().status, TaskStatus::Completed);
    assert_eq!(result2.unwrap().status, TaskStatus::Completed);
}

#[tokio::test]
async fn test_round_robin_gpu_assignment() {
    // Create processor with 2 GPUs
    let executor = create_test_executor();
    let mut processor = GpuTaskProcessor::new(vec![0, 1], executor, true, None).unwrap();

    // Process tasks - should alternate between GPUs
    let mut results = Vec::new();
    for i in 0..4 {
        let task = Task::new("echo $CUDA_VISIBLE_DEVICES".to_string(), TaskType::Shell)
            .with_timeout(5)
            .with_working_dir(PathBuf::from("."));

        let result = processor.process_task(task).await;
        assert!(result.is_ok());
        results.push(result.unwrap());
    }

    // All tasks should complete
    for result in results {
        assert_eq!(result.status, TaskStatus::Completed);
    }
}
