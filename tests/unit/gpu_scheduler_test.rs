// Unit tests for GPU scheduler and monitor

use bifrost::core::models::{Task, TaskType};
use bifrost::daemon::gpu_monitor::GpuMonitor;
use bifrost::daemon::gpu_scheduler::GpuScheduler;
use std::path::PathBuf;

fn create_test_task(name: &str) -> Task {
    Task::new(
        format!("pytest tests/test_{}.py -v", name),
        TaskType::Pytest,
    )
    .with_timeout(300)
    .with_working_dir(PathBuf::from("/workspace"))
}

#[tokio::test]
async fn test_schedule_next_with_idle_gpu() {
    let gpu_pool = vec![0, 1, 2];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    scheduler.enqueue(create_test_task("task1"));
    let (task, gpu_id) = scheduler.schedule_next().await.unwrap();
    assert_eq!(gpu_id, 0);
    // Verify the task command contains our test task name
    assert!(task.command.contains("test_task1"));
}

#[tokio::test]
async fn test_schedule_next_returns_none_when_empty() {
    let gpu_pool = vec![0, 1];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    // No tasks enqueued
    let result = scheduler.schedule_next().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_schedule_next_with_no_available_gpus() {
    let gpu_pool = vec![0];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    // Enqueue two tasks but only one GPU
    scheduler.enqueue(create_test_task("task1"));
    scheduler.enqueue(create_test_task("task2"));

    // First task takes GPU 0
    let (task1, gpu0) = scheduler.schedule_next().await.unwrap();
    assert_eq!(gpu0, 0);
    assert!(task1.command.contains("test_task1"));

    // Second task should return None since no GPUs available
    let result = scheduler.schedule_next().await;
    assert!(result.is_none());
}

#[tokio::test]
async fn test_gpu_monitor_is_gpu_idle_in_simulation_mode() {
    let gpu_pool = vec![0, 1, 2];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);

    // In simulation mode, all GPUs should be idle
    for gpu_id in gpu_pool {
        assert!(monitor.is_gpu_idle(gpu_id).await);
    }
}

#[tokio::test]
async fn test_schedule_multiple_tasks_across_gpus() {
    let gpu_pool = vec![0, 1, 2];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    // Enqueue three tasks
    scheduler.enqueue(create_test_task("task1"));
    scheduler.enqueue(create_test_task("task2"));
    scheduler.enqueue(create_test_task("task3"));

    // Schedule all three tasks
    let (_, gpu1) = scheduler.schedule_next().await.unwrap();
    let (_, gpu2) = scheduler.schedule_next().await.unwrap();
    let (_, gpu3) = scheduler.schedule_next().await.unwrap();

    // Should use GPUs 0, 1, 2
    assert_eq!(gpu1, 0);
    assert_eq!(gpu2, 1);
    assert_eq!(gpu3, 2);
}

#[tokio::test]
async fn test_release_gpu_after_task_completion() {
    let gpu_pool = vec![0, 1, 2];
    let monitor = GpuMonitor::new(gpu_pool.clone(), true);
    let mut scheduler = GpuScheduler::new(gpu_pool, monitor);

    let task1 = create_test_task("task1");
    let task2 = create_test_task("task2");

    scheduler.enqueue(task1.clone());
    scheduler.enqueue(task2.clone());

    // Schedule both tasks
    let (scheduled1, gpu0) = scheduler.schedule_next().await.unwrap();
    let (scheduled2, gpu1) = scheduler.schedule_next().await.unwrap();

    // Release GPU 0
    scheduler.release_gpu(gpu0, scheduled1.task_id).unwrap();

    // 分配是 round-robin：游标已经走到 gpu2，所以下一个拿到的是没被占用的 gpu2
    let task3 = create_test_task("task3");
    scheduler.enqueue(task3);
    let (scheduled3, next_gpu) = scheduler.schedule_next().await.unwrap();
    assert_ne!(next_gpu, gpu1, "仍被 task2 占用的 GPU 不该被再分配");
    assert_eq!(next_gpu, 2, "round-robin 游标应指向下一个空闲 GPU");

    // 把 gpu2 也释放，游标回到 0，此时刚释放的 gpu0 才会被复用
    scheduler.release_gpu(next_gpu, scheduled3.task_id).unwrap();
    scheduler.release_gpu(gpu1, scheduled2.task_id).unwrap();
    scheduler.enqueue(create_test_task("task4"));
    let (_, reused) = scheduler.schedule_next().await.unwrap();
    assert_eq!(reused, gpu0, "释放后的 GPU 应能被重新分配（轮询回到 0）");
}
