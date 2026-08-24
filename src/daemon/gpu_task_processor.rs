// GPU task processor - coordinates GpuScheduler and Executor for GPU-aware task processing
// Handles the workflow: enqueue -> schedule -> execute_with_gpu -> release_gpu

use chrono::Utc;
use std::fs;
use std::path::PathBuf;
use tokio::sync::mpsc as tokio_mpsc;
use tokio::task::JoinSet;
use uuid::Uuid;

use crate::core::batch_tracker::{BatchStatus, BatchTracker};
use crate::core::models::{Task, TaskResult, TaskStatus};
use crate::daemon::executor::Executor;
use crate::daemon::gpu_monitor::GpuMonitor;
use crate::daemon::gpu_scheduler::GpuScheduler;

/// RAII guard to ensure GPU release on drop (panic-safe)
struct GpuGuard<'a> {
    gpu_scheduler: &'a mut GpuScheduler,
    gpu_id: u32,
    task_id: Uuid,
    released: bool,
}

impl<'a> Drop for GpuGuard<'a> {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.gpu_scheduler.release_gpu(self.gpu_id, self.task_id);
        }
    }
}

impl<'a> GpuGuard<'a> {
    fn new(gpu_scheduler: &'a mut GpuScheduler, gpu_id: u32, task_id: Uuid) -> Self {
        Self {
            gpu_scheduler,
            gpu_id,
            task_id,
            released: false,
        }
    }

    fn release(mut self) -> Result<(), String> {
        self.released = true;
        self.gpu_scheduler
            .release_gpu(self.gpu_id, self.task_id)
            .map_err(|e| format!("Failed to release GPU: {}", e))
    }
}

/// GPU task processor that coordinates scheduling and execution
pub struct GpuTaskProcessor {
    gpu_scheduler: GpuScheduler,
    executor: Executor,
    batch_tracker: Option<BatchTracker>,
}

impl GpuTaskProcessor {
    pub fn new(
        gpu_pool: Vec<u32>,
        executor: Executor,
        simulate_mode: bool,
        batch_tracker: Option<BatchTracker>,
    ) -> Result<Self, String> {
        let monitor = GpuMonitor::new(gpu_pool.clone(), simulate_mode);
        let scheduler = GpuScheduler::new(gpu_pool, monitor);
        Ok(Self {
            gpu_scheduler: scheduler,
            executor,
            batch_tracker,
        })
    }

    pub async fn process_task(&mut self, task: Task) -> Result<TaskResult, String> {
        let task_id = task.task_id;
        self.gpu_scheduler.enqueue(task);

        let (scheduled_task, gpu_id) = self
            .gpu_scheduler
            .schedule_next()
            .await
            .ok_or("No available GPU for task execution")?;

        let gpu_guard = GpuGuard::new(&mut self.gpu_scheduler, gpu_id, task_id);
        let result = self
            .executor
            .execute_with_gpu(&scheduled_task, gpu_id)
            .await;
        let _ = gpu_guard.release();
        result
    }

    fn update_batch_progress(&mut self, task: &Task, result: &TaskResult) {
        if let Some(batch_id) = task.batch_id {
            if let Some(tracker) = &self.batch_tracker {
                if let Ok(mut progress) = tracker.load_progress(batch_id) {
                    let task_name = task
                        .task_name
                        .clone()
                        .unwrap_or_else(|| "unnamed".to_string());
                    progress.completed_tasks.push((
                        result.task_id,
                        result.status.clone(),
                        task_name,
                    ));
                    progress.updated_at = Utc::now();

                    if progress.completed_tasks.len() == progress.total_tasks {
                        let all_success = progress
                            .completed_tasks
                            .iter()
                            .all(|(_, status, _)| *status == TaskStatus::Completed);
                        progress.status = if all_success {
                            BatchStatus::Completed
                        } else {
                            BatchStatus::Failed
                        };
                    }

                    if let Err(e) = tracker.save_progress(&progress) {
                        eprintln!("Failed to update batch progress: {}", e);
                    } else {
                        println!(
                            "Updated batch {} progress: {} of {} tasks completed",
                            batch_id,
                            progress.completed_tasks.len(),
                            progress.total_tasks
                        );
                    }
                }
            }
        }
    }

    pub async fn run(&mut self, mut watcher_rx: tokio_mpsc::Receiver<PathBuf>) {
        println!("GpuTaskProcessor started, waiting for tasks...");
        let mut active_executions = JoinSet::new();

        while let Some(task_path) = watcher_rx.recv().await {
            println!("New task file detected: {}", task_path.display());

            match self.load_task_from_file(&task_path) {
                Ok(task) => {
                    self.gpu_scheduler.enqueue(task);

                    while let Some((scheduled_task, gpu_id)) =
                        self.gpu_scheduler.schedule_next().await
                    {
                        let task_id = scheduled_task.task_id;
                        let executor = self.executor.clone();
                        let mut gpu_scheduler = self.gpu_scheduler.clone();
                        let task_for_execution = scheduled_task.clone();

                        active_executions.spawn(async move {
                            let result =
                                executor.execute_with_gpu(&task_for_execution, gpu_id).await;
                            if let Err(e) = gpu_scheduler.release_gpu(gpu_id, task_id) {
                                eprintln!("Warning: Failed to release GPU {}: {}", gpu_id, e);
                            }
                            (task_for_execution, result)
                        });
                    }
                }
                Err(e) => eprintln!("Failed to load task from file: {}", e),
            }

            while let Some(result) = active_executions.try_join_next() {
                match result {
                    Ok((task, task_result)) => match task_result {
                        Ok(result) => {
                            println!("Task completed: {:?}", result.status);
                            self.update_batch_progress(&task, &result);
                        }
                        Err(e) => eprintln!("Task execution failed: {}", e),
                    },
                    Err(e) => eprintln!("Execution panicked: {}", e),
                }
            }
        }

        println!("Waiting for remaining tasks to complete...");
        while let Some(result) = active_executions.join_next().await {
            match result {
                Ok((task, task_result)) => match task_result {
                    Ok(result) => {
                        println!("Task completed: {:?}", result.status);
                        self.update_batch_progress(&task, &result);
                    }
                    Err(e) => eprintln!("Task execution failed: {}", e),
                },
                Err(e) => eprintln!("Execution panicked: {}", e),
            }
        }

        println!("GpuTaskProcessor shutting down (channel closed)");
    }

    fn load_task_from_file(&self, path: &PathBuf) -> Result<Task, String> {
        let json = fs::read_to_string(path).map_err(|e| format!("Failed to read file: {}", e))?;
        let task: Task =
            serde_json::from_str(&json).map_err(|e| format!("Failed to parse JSON: {}", e))?;
        Ok(task)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{Task, TaskStatus, TaskType};
    use std::time::Duration;

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
        let processor = GpuTaskProcessor::new(vec![0, 1], executor, true, None);
        assert!(processor.is_ok());
    }

    #[tokio::test]
    async fn test_process_task_success() {
        let executor = create_test_executor();
        let mut processor = GpuTaskProcessor::new(vec![0], executor, true, None).unwrap();
        let task = create_test_task("hello");
        let result = processor.process_task(task).await;

        assert!(result.is_ok());
        let result = result.unwrap();
        assert_eq!(result.status, TaskStatus::Completed);
        assert!(result.output.stdout.contains("hello"));
    }

    #[tokio::test]
    async fn test_process_task_no_gpu_available() {
        let executor = create_test_executor();
        let mut processor = GpuTaskProcessor::new(vec![], executor, true, None).unwrap();
        let task = create_test_task("test");
        let result = processor.process_task(task).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No available GPU"));
    }
}
