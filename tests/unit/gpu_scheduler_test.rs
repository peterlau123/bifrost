// Unit tests for GPU scheduler and monitor

use bifrost::daemon::gpu_scheduler::GpuScheduler;
use bifrost::daemon::gpu_monitor::GpuMonitor;
use bifrost::core::models::{Task, TaskType};
use std::path::PathBuf;

fn create_test_task(name: &str) -> Task {
    Task::new(format!("pytest tests/test_{}.py -v", name), TaskType::Pytest)
        .with_timeout(300)
        .with_working_dir(PathBuf::from("/workspace"))
}

#[test]
fn test_schedule_next_with_idle_gpu() {
    let gpu_pool = vec![0, 1, 2];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    scheduler.enqueue(create_test_task("task1"));
    let (task, gpu_id) = scheduler.schedule_next().unwrap();
    assert_eq!(gpu_id, 0);
    // Verify the task command contains our test task name
    assert!(task.command.contains("test_task1"));
}

#[test]
fn test_schedule_next_returns_none_when_empty() {
    let gpu_pool = vec![0, 1];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    // No tasks enqueued
    let result = scheduler.schedule_next();
    assert!(result.is_none());
}

#[test]
fn test_schedule_next_with_no_available_gpus() {
    let gpu_pool = vec![0];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    // Enqueue two tasks but only one GPU
    scheduler.enqueue(create_test_task("task1"));
    scheduler.enqueue(create_test_task("task2"));

    // First task takes GPU 0
    let (task1, gpu0) = scheduler.schedule_next().unwrap();
    assert_eq!(gpu0, 0);
    assert!(task1.command.contains("test_task1"));

    // Second task should return None since no GPUs available
    let result = scheduler.schedule_next();
    assert!(result.is_none());
}

#[test]
fn test_gpu_monitor_is_gpu_idle_in_simulation_mode() {
    let gpu_pool = vec![0, 1, 2];
    let mut monitor = GpuMonitor::new(gpu_pool.clone(), true);

    // In simulation mode, all GPUs should be idle
    for gpu_id in gpu_pool {
        assert!(monitor.is_gpu_idle(gpu_id));
    }
}

#[test]
fn test_schedule_multiple_tasks_across_gpus() {
    let gpu_pool = vec![0, 1, 2];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    // Enqueue three tasks
    scheduler.enqueue(create_test_task("task1"));
    scheduler.enqueue(create_test_task("task2"));
    scheduler.enqueue(create_test_task("task3"));

    // Schedule all three tasks
    let (_, gpu1) = scheduler.schedule_next().unwrap();
    let (_, gpu2) = scheduler.schedule_next().unwrap();
    let (_, gpu3) = scheduler.schedule_next().unwrap();

    // Should use GPUs 0, 1, 2
    assert_eq!(gpu1, 0);
    assert_eq!(gpu2, 1);
    assert_eq!(gpu3, 2);
}

#[test]
fn test_release_gpu_makes_it_available_again() {
    let gpu_pool = vec![0];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    scheduler.enqueue(create_test_task("task1"));
    let (task, gpu_id) = scheduler.schedule_next().unwrap();
    assert_eq!(gpu_id, 0);

    // Now no GPUs should be available
    assert!(scheduler.schedule_next().is_none());

    // Release the GPU
    scheduler.release_gpu(gpu_id, task.task_id);

    // Now we can schedule another task
    scheduler.enqueue(create_test_task("task2"));
    let (task2, gpu_id2) = scheduler.schedule_next().unwrap();
    assert_eq!(gpu_id2, 0);
    assert!(task2.command.contains("test_task2"));
}

#[test]
fn test_get_active_tasks() {
    let gpu_pool = vec![0, 1];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    assert_eq!(scheduler.get_active_task_count(), 0);

    scheduler.enqueue(create_test_task("task1"));
    scheduler.schedule_next().unwrap();

    assert_eq!(scheduler.get_active_task_count(), 1);

    scheduler.enqueue(create_test_task("task2"));
    scheduler.schedule_next().unwrap();

    assert_eq!(scheduler.get_active_task_count(), 2);
}

#[test]
fn test_get_pending_count() {
    let gpu_pool = vec![0];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    assert_eq!(scheduler.get_pending_count(), 0);

    scheduler.enqueue(create_test_task("task1"));
    scheduler.enqueue(create_test_task("task2"));

    assert_eq!(scheduler.get_pending_count(), 2);

    // Schedule one task
    scheduler.schedule_next().unwrap();

    // One should still be pending
    assert_eq!(scheduler.get_pending_count(), 1);
}
