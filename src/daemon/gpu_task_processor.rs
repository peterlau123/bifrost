// GPU task processor - coordinates GpuScheduler and Executor for GPU-aware task processing
// Handles the workflow: enqueue -> schedule -> execute_with_gpu -> release_gpu

use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc as tokio_mpsc;
use uuid::Uuid;

use crate::core::models::{Task, TaskResult};
use crate::daemon::executor::Executor;
use crate::daemon::gpu_scheduler::GpuScheduler;
use crate::daemon::gpu_monitor::GpuMonitor;

/// GPU task processor that coordinates scheduling and execution
///
/// Integrates GpuScheduler with Executor to provide GPU-aware task processing.
/// Handles the complete workflow from task enqueue to GPU release.
pub struct GpuTaskProcessor {
    gpu_scheduler: GpuScheduler,
    executor: Executor,
}

impl GpuTaskProcessor {
    /// Create a new GPU task processor
    ///
    /// # Arguments
    /// * `gpu_pool` - List of GPU IDs available for scheduling
    /// * `executor` - Executor instance for running tasks
    /// * `simulate_mode` - If true, GPU monitoring operates in simulation mode
    pub fn new(
        gpu_pool: Vec<u32>,
        executor: Executor,
        simulate_mode: bool,
    ) -> Result<Self, String> {
        let monitor = GpuMonitor::new(gpu_pool.clone(), simulate_mode);
        let scheduler = GpuScheduler::new(gpu_pool, monitor);

        Ok(Self {
            gpu_scheduler: scheduler,
            executor,
        })
    }

    /// Process a task with GPU scheduling
    ///
    /// This method coordinates the complete GPU task lifecycle:
    /// 1. Enqueue task to scheduler
    /// 2. Wait for GPU allocation
    /// 3. Execute with GPU isolation (CUDA_VISIBLE_DEVICES)
    /// 4. Release GPU after completion
    ///
    /// # Arguments
    /// * `task` - The task to process
    ///
    /// # Returns
    /// * `Ok(TaskResult)` - Task completed with result
    /// * `Err(String)` - Task processing failed
    pub async fn process_task(&mut self, task: Task) -> Result<TaskResult, String> {
        // 1. Enqueue task to scheduler
        let task_id = task.task_id;
        self.gpu_scheduler.enqueue(task);

        // 2. Wait for GPU allocation (schedule_next)
        let (scheduled_task, gpu_id) = self
            .gpu_scheduler
            .schedule_next()
            .ok_or("No available GPU for task execution")?;

        // 3. Execute with GPU isolation
        let result = self.executor.execute_with_gpu(&scheduled_task, gpu_id).await;

        // 4. Release GPU after completion
        self.gpu_scheduler
            .release_gpu(gpu_id, task_id)
            .map_err(|e| format!("Failed to release GPU: {}", e))?;

        result
    }

    /// Process tasks from watcher channel continuously
    ///
    /// Runs an infinite loop processing tasks from the watcher as they arrive.
    /// This is the main entry point for daemon operation.
    ///
    /// # Arguments
    /// * `watcher_rx` - Channel receiver for task file paths from AsyncFileWatcher
    pub async fn run(&mut self, mut watcher_rx: tokio_mpsc::Receiver<PathBuf>) {
        println!("GpuTaskProcessor started, waiting for tasks...");

        while let Some(task_path) = watcher_rx.recv().await {
            println!("New task file detected: {}", task_path.display());

            // Load task from JSON file
            match self.load_task_from_file(&task_path) {
                Ok(task) => {
                    // Process task with GPU scheduling
                    if let Err(e) = self.process_task(task).await {
                        eprintln!("Task processing failed: {}", e);
                    }
                }
                Err(e) => {
                    eprintln!("Failed to load task from file: {}", e);
                }
            }
        }

        println!("GpuTaskProcessor shutting down (channel closed)");
    }

    /// Load a task from a JSON file
    ///
    /// # Arguments
    /// * `path` - Path to the JSON task file
    ///
    /// # Returns
    /// * `Ok(Task)` - Successfully parsed task
    /// * `Err(String)` - Failed to read or parse file
    fn load_task_from_file(&self, path: &PathBuf) -> Result<Task, String> {
        let json = fs::read_to_string(path)
            .map_err(|e| format!("Failed to read file: {}", e))?;

        let task: Task = serde_json::from_str(&json)
            .map_err(|e| format!("Failed to parse JSON: {}", e))?;

        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{Task, TaskType, TaskStatus};
    use std::path::PathBuf;
    use std::time::Duration;
    use tokio::sync::mpsc;

    fn create_test_task(name: &str) -> Task {
        Task::new(format!("echo {}", name), TaskType::Shell)
            .with_timeout(5)
            .with_working_dir(PathBuf::from("."))
    }

    fn create_test_executor() -> Executor {
        let temp_dir = std::env::temp_dir();
        Executor::new(temp_dir.join("logs"), Duration::from_secs(30)).unwrap()
    }

    #[test]
    fn test_gpu_task_processor_new() {
        let executor = create_test_executor();
        let processor = GpuTaskProcessor::new(vec![0, 1], executor, true);
        assert!(processor.is_ok());
    }

    #[tokio::test]
    async fn test_process_task_success() {
        let executor = create_test_executor();
        let mut processor = GpuTaskProcessor::new(vec![0], executor, true).unwrap();

        let task = create_test_task("hello");
        let result = processor.process_task(task).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.output.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_process_task_no_gpu_available() {
        // Create processor with empty GPU pool - should fail
        let executor = create_test_executor();
        let mut processor = GpuTaskProcessor::new(vec![], executor, true).unwrap();

        let task = create_test_task("test");
        let result = processor.process_task(task).await;

        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No available GPU"));
    }

    #[tokio::test]
    async fn test_load_task_from_file() {
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let task_file = temp_dir.path().join("test_task.json");

        // Create a valid task JSON
        let task = create_test_task("from_file");
        let json = serde_json::to_string(&task).unwrap();

        let mut file = fs::File::create(&task_file).unwrap();
        file.write_all(json.as_bytes()).unwrap();
        file.sync_all().unwrap();

        let executor = create_test_executor();
        let processor = GpuTaskProcessor::new(vec![0], executor, true).unwrap();

        let loaded_task = processor.load_task_from_file(&task_file);
        assert!(loaded_task.is_ok());

        let loaded = loaded_task.unwrap();
        assert_eq!(loaded.command, "echo from_file");
    }

    #[tokio::test]
    async fn test_run_with_channel() {
        use std::io::Write;
        use tempfile::TempDir;

        let temp_dir = TempDir::new().unwrap();
        let task_file = temp_dir.path().join("channel_task.json");

        // Create a valid task JSON
        let task = create_test_task("channel_test");
        let json = serde_json::to_string(&task).unwrap();

        let mut file = fs::File::create(&task_file).unwrap();
        file.write_all(json.as_bytes()).unwrap();
        file.sync_all().unwrap();

        let executor = create_test_executor();
        let mut processor = GpuTaskProcessor::new(vec![0], executor, true).unwrap();

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
}
