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
    /// Tracks the last GPU index assigned for round-robin allocation
    last_assigned_index: usize,
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
            last_assigned_index: 0,
        }
    }

    /// Add a task to the pending queue
    pub fn enqueue(&mut self, task: Task) {
        self.pending_queue.push_back(task);
    }

    /// Get the next available idle GPU using round-robin allocation
    ///
    /// Returns the next idle GPU in round-robin order to ensure fair distribution
    fn get_next_idle_gpu(&mut self) -> Option<u32> {
        let pool_size = self.gpu_pool.len();
        if pool_size == 0 {
            return None;
        }

        for i in 0..pool_size {
            // Start from last_assigned_index, cycle through all GPUs
            let idx = (self.last_assigned_index + i) % pool_size;
            let gpu_id = self.gpu_pool[idx];
            if let Some(tasks) = self.active_tasks.get(&gpu_id) {
                if tasks.is_empty() && self.monitor.is_gpu_idle(gpu_id) {
                    // Update last_assigned_index for next iteration
                    self.last_assigned_index = (idx + 1) % pool_size;
                    return Some(gpu_id);
                }
            }
        }
        None
    }

    /// Release a GPU after task completion
    ///
    /// # Arguments
    /// * `gpu_id` - The GPU to release
    /// * `task_id` - The task that completed on this GPU
    pub fn release_gpu(&mut self, gpu_id: u32, task_id: Uuid) -> Result<(), String> {
        if let Some(tasks) = self.active_tasks.get_mut(&gpu_id) {
            // Remove the completed task
            tasks.retain(|&id| id != task_id);

            if tasks.is_empty() {
                // GPU is now fully idle
                Ok(())
            } else {
                // GPU still has other active tasks
                Ok(())
            }
        } else {
            Err(format!("GPU {} not found in active_tasks", gpu_id))
        }
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
    fn test_schedule_next_no_tasks() {
        let gpu_pool = vec![0, 1];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);
        let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

        let result = scheduler.schedule_next();
        assert!(result.is_none());
    }
}
