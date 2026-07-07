// GPU scheduler for managing task execution across GPU resources
use std::collections::{HashMap, VecDeque};
use uuid::Uuid;

use crate::core::models::Task;
use super::gpu_monitor::GpuMonitor;

/// GPU scheduler for managing task execution across GPU resources
///
/// Maintains a pool of GPUs and schedules tasks to available GPUs.
/// Uses CUDA_VISIBLE_DEVICES for GPU isolation.
pub struct GpuScheduler {
    gpu_pool: Vec<u32>,
    /// Maps GPU ID to list of task IDs currently running on that GPU
    active_tasks: HashMap<u32, Vec<Uuid>>,
    /// Queue of pending tasks waiting for GPU allocation
    pending_queue: VecDeque<Task>,
    monitor: GpuMonitor,
}

impl GpuScheduler {
    /// Create a new GPU scheduler
    ///
    /// # Arguments
    /// * `gpu_pool` - List of GPU IDs available for scheduling
    /// * `monitor` - GPU monitor instance for checking GPU status
    pub fn new(gpu_pool: Vec<u32>, monitor: GpuMonitor) -> Self {
        let mut active_tasks = HashMap::new();
        for &gpu_id in &gpu_pool {
            active_tasks.insert(gpu_id, Vec::new());
        }

        Self {
            gpu_pool,
            active_tasks,
            pending_queue: VecDeque::new(),
            monitor,
        }
    }

    /// Add a task to the pending queue
    pub fn enqueue(&mut self, task: Task) {
        self.pending_queue.push_back(task);
    }

    /// Get the next available idle GPU
    ///
    /// Returns the first GPU that has no active tasks assigned to it
    fn get_next_idle_gpu(&mut self) -> Option<u32> {
        for &gpu_id in &self.gpu_pool {
            if let Some(tasks) = self.active_tasks.get(&gpu_id) {
                if tasks.is_empty() && self.monitor.is_gpu_idle(gpu_id) {
                    return Some(gpu_id);
                }
            }
        }
        None
    }

    /// Schedule the next pending task to an available GPU
    ///
    /// Returns Some((task, gpu_id)) if a task was scheduled,
    /// or None if no tasks are pending or no GPUs are available
    pub fn schedule_next(&mut self) -> Option<(Task, u32)> {
        if self.pending_queue.is_empty() {
            return None;
        }

        let gpu_id = self.get_next_idle_gpu()?;
        let task = self.pending_queue.pop_front()?;

        // Add task to active tasks for this GPU
        self.active_tasks
            .entry(gpu_id)
            .or_insert_with(Vec::new)
            .push(task.task_id);

        Some((task, gpu_id))
    }

    /// Release a GPU after task completion
    ///
    /// Removes the task from the active tasks list for the specified GPU
    ///
    /// # Arguments
    /// * `gpu_id` - The GPU ID to release
    /// * `task_id` - The task ID that completed
    pub fn release_gpu(&mut self, gpu_id: u32, task_id: Uuid) {
        if let Some(tasks) = self.active_tasks.get_mut(&gpu_id) {
            tasks.retain(|&id| id != task_id);
        }
    }

    /// Get the number of active tasks across all GPUs
    pub fn get_active_task_count(&self) -> usize {
        self.active_tasks.values().map(|tasks| tasks.len()).sum()
    }

    /// Get the number of pending tasks in the queue
    pub fn get_pending_count(&self) -> usize {
        self.pending_queue.len()
    }

    /// Get the list of GPUs in the pool
    pub fn get_gpu_pool(&self) -> &[u32] {
        &self.gpu_pool
    }

    /// Get the GPU monitor
    pub fn get_monitor(&self) -> &GpuMonitor {
        &self.monitor
    }

    /// Check if a specific GPU has active tasks
    pub fn is_gpu_busy(&self, gpu_id: u32) -> bool {
        self.active_tasks
            .get(&gpu_id)
            .map(|tasks| !tasks.is_empty())
            .unwrap_or(false)
    }

    /// Get all active task IDs for a specific GPU
    pub fn get_gpu_tasks(&self, gpu_id: u32) -> Option<&Vec<Uuid>> {
        self.active_tasks.get(&gpu_id)
    }

    /// Get a summary of scheduler state
    pub fn get_status_summary(&self) -> SchedulerStatus {
        SchedulerStatus {
            total_gpus: self.gpu_pool.len(),
            active_tasks: self.get_active_task_count(),
            pending_tasks: self.get_pending_count(),
            available_gpus: self.gpu_pool.len() - self.active_tasks.values().filter(|v| !v.is_empty()).count(),
        }
    }
}

/// Status summary for the GPU scheduler
#[derive(Debug, Clone)]
pub struct SchedulerStatus {
    /// Total number of GPUs in the pool
    pub total_gpus: usize,
    /// Number of active tasks
    pub active_tasks: usize,
    /// Number of pending tasks
    pub pending_tasks: usize,
    /// Number of available GPUs
    pub available_gpus: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::TaskType;
    use std::path::PathBuf;

    fn create_test_task(name: &str) -> Task {
        Task::new(format!("echo {}", name), TaskType::Shell)
            .with_timeout(300)
            .with_working_dir(PathBuf::from("/tmp"))
    }

    #[test]
    fn test_scheduler_new() {
        let gpu_pool = vec![0, 1, 2];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);
        let scheduler = GpuScheduler::new(gpu_pool.clone(), monitor);

        assert_eq!(scheduler.get_gpu_pool(), &[0, 1, 2]);
        assert_eq!(scheduler.get_active_task_count(), 0);
        assert_eq!(scheduler.get_pending_count(), 0);
    }

    #[test]
    fn test_enqueue_task() {
        let gpu_pool = vec![0, 1];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);
        let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

        let task = create_test_task("test1");
        scheduler.enqueue(task);

        assert_eq!(scheduler.get_pending_count(), 1);
    }

    #[test]
    fn test_schedule_next_no_tasks() {
        let gpu_pool = vec![0, 1];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);
        let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

        let result = scheduler.schedule_next();
        assert!(result.is_none());
    }

    #[test]
    fn test_release_gpu() {
        let gpu_pool = vec![0];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);
        let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

        let task = create_test_task("test1");
        let task_id = task.task_id;
        scheduler.enqueue(task);

        let (_, gpu_id) = scheduler.schedule_next().unwrap();
        assert_eq!(scheduler.get_active_task_count(), 1);

        scheduler.release_gpu(gpu_id, task_id);
        assert_eq!(scheduler.get_active_task_count(), 0);
    }

    #[test]
    fn test_status_summary() {
        let gpu_pool = vec![0, 1, 2];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);
        let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

        let status = scheduler.get_status_summary();
        assert_eq!(status.total_gpus, 3);
        assert_eq!(status.active_tasks, 0);
        assert_eq!(status.pending_tasks, 0);
        assert_eq!(status.available_gpus, 3);

        scheduler.enqueue(create_test_task("test1"));
        scheduler.schedule_next();

        let status = scheduler.get_status_summary();
        assert_eq!(status.active_tasks, 1);
        assert_eq!(status.pending_tasks, 0);
        assert_eq!(status.available_gpus, 2);
    }
}
