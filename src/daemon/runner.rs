// Server runner
//
// Robustness design (hardened):
//   - Inotify events are the fast path, but a periodic fallback scan
//     (every 5s) picks up tasks that inotify missed (watcher failure,
//     event overflow, or tasks submitted before the server started).
//     This makes the server self-healing: even with a dead watcher,
//     new tasks are still consumed.
//   - Claim marker: commands/{name}.processing created atomically
//     (create_new). Whichever path (inotify or scan) creates it first
//     owns the task; the other skips it. Prevents double execution.
//   - Read/parse failures are retried (up to 3x with backoff); if a
//     task still can't be parsed, a Failed result is written so the
//     client never sees a forever-Pending task.
//   - Executor errors also produce a Failed result instead of silence.
use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus};
use crate::core::protocol::Protocol;
use crate::core::settings::BifrostSettings;
use crate::daemon::executor::Executor;
use crate::daemon::heartbeat::Heartbeat;
use crate::daemon::watcher::AsyncFileWatcher;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Fallback scan interval: catches tasks inotify missed (watcher death,
/// event overflow, pre-server submissions). GPFS inotify is unreliable for
/// rename events, so keep this tight (100ms) as a safety net.
const FALLBACK_SCAN_INTERVAL: Duration = Duration::from_millis(100);
/// Read/parse retries with 200ms backoff.
const MAX_READ_RETRIES: u32 = 3;
const RETRY_BACKOFF: Duration = Duration::from_millis(200);

pub async fn run_server(s: BifrostSettings, sd: Arc<AtomicBool>) -> Result<(), String> {
    let ss = s.shared_storage.clone();
    let cd = ss.join("commands");
    let ld = ss.join("logs");
    if !cd.exists() {
        std::fs::create_dir_all(&cd).map_err(|e| format!("mkdir: {}", e))?;
    }
    // Auto-create daemon working_dir if configured but doesn't exist
    if let Some(ref wd) = s.daemon.working_dir {
        if !wd.exists() {
            std::fs::create_dir_all(wd).map_err(|e| format!("mkdir working_dir: {}", e))?;
            println!("Created working_dir: {}", wd.display());
        }
        // Set as process cwd so tasks without explicit working_dir use this
        std::env::set_current_dir(wd).map_err(|e| format!("set_current_dir: {}", e))?;
    }
    let p = Arc::new(Protocol::new(ss.clone()).map_err(|e| format!("p: {}", e))?);
    let to = s.daemon.task_timeout.unwrap_or(Duration::from_secs(300));
    let ex = Executor::new(ld, to).map_err(|e| format!("e: {}", e))?;
    let mut hb = Heartbeat::new(ss.clone()).map_err(|e| format!("hb: {}\n", e))?;
    let hi = s
        .daemon
        .heartbeat_interval
        .unwrap_or(Duration::from_secs(60));
    let hbs = sd.clone();
    let active_count = Arc::new(AtomicUsize::new(0));
    let hb_active = active_count.clone();
    // ponytail: heartbeat 用独立 std::thread 而非 tokio::spawn —
    // write_heartbeat 是 GPFS 同步写 (fs::write), 放在 tokio worker 上
    // 会被高并发任务饥饿 (阻塞 syscall 占满 worker), 表现为心跳停更。
    // 独立线程与任务线程池隔离, 心跳永远按时写。
    std::thread::spawn(move || loop {
        if hbs.load(Ordering::Relaxed) {
            break;
        }
        hb.update_status(crate::daemon::heartbeat::DaemonStatus::Running);
        hb.update_task_counts(hb_active.load(Ordering::Relaxed), 0);
        let _ = hb.write_heartbeat();
        std::thread::sleep(hi);
    });

    let mut rx = AsyncFileWatcher::new(cd.clone())
        .map_err(|e| format!("w: {}", e))?
        .watch_async()
        .await;

    // 并发执行: 尊重 max_concurrent 配置 (README 示例默认 10)
    let mc = s.daemon.max_concurrent.unwrap_or(10).clamp(1, 100);
    let sem = Arc::new(tokio::sync::Semaphore::new(mc));
    println!(
        "Server ready, watching {} (max_concurrent={}, fallback scan {}s)",
        cd.display(),
        mc,
        FALLBACK_SCAN_INTERVAL.as_secs()
    );

    // Initial catch-up: tasks submitted before the server started
    // (e.g. during restart) must not be silently lost.
    for path in scan_pending(&cd) {
        spawn_task(
            p.clone(),
            ex.clone(),
            sem.clone(),
            active_count.clone(),
            path,
        );
    }

    let mut last_scan = Instant::now();
    loop {
        if sd.load(Ordering::Relaxed) {
            break;
        }
        tokio::select! {
            Some(path) = rx.recv() => {
                spawn_task(p.clone(), ex.clone(), sem.clone(), active_count.clone(), path);
            }
            _ = tokio::time::sleep(FALLBACK_SCAN_INTERVAL) => {
                // Fallback scan: self-healing against watcher death and
                // missed events. Cheap (readdir) and runs at most once
                // per FALLBACK_SCAN_INTERVAL.
                if last_scan.elapsed() >= FALLBACK_SCAN_INTERVAL {
                    last_scan = Instant::now();
                    // ponytail: readdir on GPFS is a blocking syscall — running it
                    // inline starves the async workers (heartbeat + task futures)
                    // under high concurrency (observed: daemon "freezes" with 8+
                    // concurrent batches). Move it to the blocking pool.
                    let cd2 = cd.clone();
                    let p2 = p.clone();
                    let ex2 = ex.clone();
                    let sem2 = sem.clone();
                    let ac2 = active_count.clone();
                    tokio::spawn(async move {
                        let paths = tokio::task::spawn_blocking(move || scan_pending(&cd2))
                            .await
                            .unwrap_or_default();
                        for path in paths {
                            spawn_task(p2.clone(), ex2.clone(), sem2.clone(), ac2.clone(), path);
                        }
                    });
                }
            }
        }
    }
    println!("Server stopped.");
    Ok(())
}

/// Spawn a task processing future. Claim-marker dedup happens inside so
/// inotify events and fallback scans never double-execute a task.
fn spawn_task(
    p: Arc<Protocol>,
    ex: Executor,
    sem: Arc<tokio::sync::Semaphore>,
    active: Arc<AtomicUsize>,
    path: std::path::PathBuf,
) {
    tokio::spawn(async move {
        let _permit = sem.acquire().await;
        // Claim marker: atomic create_new - whoever creates it first owns
        // the task. .processing is excluded from scans, so no re-entry.
        // ponytail: create_new on GPFS is a blocking syscall; run in the
        // blocking pool so a slow GPFS open can't stall async workers.
        let marker = path.with_extension("processing");
        let marker_for_claim = marker.clone();
        let claimed = tokio::task::spawn_blocking(move || {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker_for_claim)
                .is_ok()
        })
        .await
        .unwrap_or(false);
        if !claimed {
            return; // already claimed by another event/scan
        }

        active.fetch_add(1, Ordering::Relaxed);
        let result = process_one(&p, &ex, &path).await;
        active.fetch_sub(1, Ordering::Relaxed);
        let _ = std::fs::remove_file(&marker);

        if let Err(e) = result {
            eprintln!("task {} failed: {}", path.display(), e);
        }
    });
}

/// Read, parse, execute one task file and write the result back.
/// Never leaves a task in a forever-Pending state: parse failures and
/// executor errors both produce a Failed TaskResult.
async fn process_one(
    p: &Arc<Protocol>,
    ex: &Executor,
    path: &std::path::Path,
) -> Result<(), String> {
    let task = match read_task_retry(path).await {
        Some(t) => t,
        None => {
            let err = format!(
                "cannot parse task file (after {} retries)",
                MAX_READ_RETRIES
            );
            write_failed_result(p, path, &err).await;
            return Ok(()); // handled: Failed result written
        }
    };

    let tid = task.task_id;
    let p1 = p.clone();
    let _ = blocking_write(move || p1.write_status(&tid, &TaskStatus::Running, Some("executing")))
        .await;
    match ex.execute(&task).await {
        Ok(r) => {
            let m = match r.status {
                TaskStatus::Completed => Some("ok"),
                _ => None,
            };
            let r_status = r.status.clone();
            let r_dur = r.duration_ms();
            println!("  {}: {} ({}ms)", tid, r_status, r_dur);
            // ponytail: result/status/remove 都是 GPFS atomic_write (flock +
            // write + rename), 阻塞 syscall 移到 blocking pool, 避免高并发
            // 时占满 async worker 导致心跳/其他任务饥饿。
            let p2 = p.clone();
            let _ = blocking_write(move || p2.write_result(&tid, &r)).await;
            let p3 = p.clone();
            let _ = blocking_write(move || p3.write_status(&tid, &r_status, m)).await;
            let p4 = p.clone();
            let _ = blocking_write(move || p4.remove_task(&tid)).await;
        }
        Err(e) => {
            // Executor error (spawn failure etc.): write a Failed result
            // so the client sees a terminal state, not a forever Pending.
            eprintln!("exec {}: {}", tid, e);
            let p5 = p.clone();
            let _ = blocking_write(move || {
                p5.write_result(
                    &tid,
                    &TaskResult {
                        task_id: tid,
                        status: TaskStatus::Failed,
                        output: TaskOutput {
                            stdout: String::new(),
                            stderr: e.clone(),
                            exit_code: None,
                        },
                        command: task.command.clone(),
                        start_time: chrono::Utc::now(),
                        end_time: chrono::Utc::now(),
                        duration_ms: 0,
                        retries_used: 0,
                        artifacts: Vec::new(),
                        error_message: Some(e.clone()),
                    },
                )
            })
            .await;
            let p6 = p.clone();
            let _ = blocking_write(move || {
                p6.write_status(&tid, &TaskStatus::Failed, Some("executor error"))
            })
            .await;
            let p7 = p.clone();
            let _ = blocking_write(move || p7.remove_task(&tid)).await;
        }
    }
    Ok(())
}

/// Run a blocking Protocol/GPFS write on the blocking pool.
async fn blocking_write<F, R>(f: F) -> Option<R>
where
    F: FnOnce() -> R + Send + 'static,
    R: Send + 'static,
{
    tokio::task::spawn_blocking(f).await.ok()
}

/// Read + parse a task file with retries. Returns None when it still fails.
async fn read_task_retry(path: &std::path::Path) -> Option<Task> {
    for attempt in 0..MAX_READ_RETRIES {
        // ponytail: fs::read_to_string on GPFS is a blocking syscall; run in
        // the blocking pool so a slow read can't stall async workers.
        let p = path.to_path_buf();
        let content = tokio::task::spawn_blocking(move || std::fs::read_to_string(&p))
            .await
            .ok()
            .and_then(|r| r.ok());
        if let Some(content) = content {
            if let Ok(t) = serde_json::from_str::<Task>(&content) {
                return Some(t);
            }
        }
        if attempt + 1 < MAX_READ_RETRIES {
            tokio::time::sleep(RETRY_BACKOFF).await;
        }
    }
    None
}

/// Write a minimal Failed result for a task that could not even be parsed.
/// task_id is extracted from the filename ({timestamp}_{uuid}.json).
async fn write_failed_result(p: &Arc<Protocol>, path: &std::path::Path, error: &str) {
    let Some(tid) = task_id_from_path(path) else {
        eprintln!("cannot extract task id from {}", path.display());
        return;
    };
    let result = TaskResult {
        task_id: tid,
        status: TaskStatus::Failed,
        output: TaskOutput {
            stdout: String::new(),
            stderr: error.to_string(),
            exit_code: None,
        },
        command: String::new(), // task file unparseable, no command available
        start_time: chrono::Utc::now(),
        end_time: chrono::Utc::now(),
        duration_ms: 0,
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: Some(error.to_string()),
    };
    let p1 = p.clone();
    let _ = blocking_write(move || p1.write_result(&tid, &result)).await;
    let p2 = p.clone();
    let _ = blocking_write(move || p2.write_status(&tid, &TaskStatus::Failed, Some("parse error")))
        .await;
    let p3 = p.clone();
    let _ = blocking_write(move || p3.remove_task(&tid)).await;
}

/// List pending task files (.json only - .lock/.tmp/.processing excluded).
fn scan_pending(commands_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(commands_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".json") {
                out.push(e.path());
            }
        }
    }
    out
}

/// Extract task_id (Uuid) from a {timestamp}_{uuid}.json filename.
fn task_id_from_path(path: &std::path::Path) -> Option<uuid::Uuid> {
    let name = path.file_name()?.to_string_lossy().to_string();
    let stem = name.strip_suffix(".json")?;
    // format: 20260731_103000_{uuid} - uuid is the last 36 chars
    let uuid_part = stem.rsplit('_').next()?;
    uuid::Uuid::parse_str(uuid_part).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_task_id_from_path() {
        let p = std::path::Path::new("20260731_103000_550e8400-e29b-41d4-a716-446655440000.json");
        assert_eq!(
            task_id_from_path(p),
            Some(uuid::Uuid::parse_str("550e8400-e29b-41d4-a716-446655440000").unwrap())
        );
        assert_eq!(task_id_from_path(std::path::Path::new("bad.json")), None);
        assert_eq!(
            task_id_from_path(std::path::Path::new("20260731_x.json")),
            None
        );
    }

    #[test]
    fn test_scan_pending_excludes_sidecars() {
        let tmp = TempDir::new().unwrap();
        let cd = tmp.path().join("commands");
        std::fs::create_dir_all(&cd).unwrap();
        std::fs::write(
            cd.join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.json"),
            "{}",
        )
        .unwrap();
        std::fs::write(
            cd.join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.lock"),
            "",
        )
        .unwrap();
        std::fs::write(
            cd.join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.processing"),
            "",
        )
        .unwrap();
        std::fs::write(
            cd.join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.json.tmp123"),
            "{}",
        )
        .unwrap();

        let found = scan_pending(&cd);
        assert_eq!(found.len(), 1, "只应返回 .json 文件, 实际: {:?}", found);
        assert!(found[0].to_string_lossy().ends_with(".json"));
    }

    #[test]
    fn test_claim_marker_is_exclusive() {
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.json");
        std::fs::write(&path, "{}").unwrap();
        let marker = path.with_extension("processing");

        // 第一次创建成功 (领取)
        let first = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker);
        assert!(first.is_ok(), "第一次领取应成功");
        // 第二次创建失败 (已被领取 → 去重)
        let second = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker);
        assert!(second.is_err(), "第二次领取必须失败 (防重复执行)");
    }

    #[tokio::test]
    async fn test_read_task_retry_good_json() {
        let tmp = TempDir::new().unwrap();
        let task = Task::new("echo hi".into(), crate::core::models::TaskType::Shell);
        let path = tmp
            .path()
            .join(format!("20260731_103000_{}.json", task.task_id));
        std::fs::write(&path, serde_json::to_string(&task).unwrap()).unwrap();
        let t = read_task_retry(&path).await;
        assert!(t.is_some(), "合法 JSON 应解析成功");
        assert_eq!(t.unwrap().task_id, task.task_id);
    }

    #[tokio::test]
    async fn test_read_task_retry_bad_json_returns_none() {
        let tmp = TempDir::new().unwrap();
        let path = tmp
            .path()
            .join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.json");
        std::fs::write(&path, "{ not valid json").unwrap();
        let t = read_task_retry(&path).await;
        assert!(t.is_none(), "坏 JSON 重试后应返回 None");
    }
}
