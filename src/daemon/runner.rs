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
/// event overflow, pre-server submissions).
const FALLBACK_SCAN_INTERVAL: Duration = Duration::from_secs(5);
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
    let mut hb = Heartbeat::new(ss.clone()).map_err(|e| format!("hb: {}", e))?;
    let hi = s
        .daemon
        .heartbeat_interval
        .unwrap_or(Duration::from_secs(60));
    let hbs = sd.clone();
    let active_count = Arc::new(AtomicUsize::new(0));
    let hb_active = active_count.clone();
    tokio::spawn(async move {
        loop {
            if hbs.load(Ordering::Relaxed) {
                break;
            }
            hb.update_status(crate::daemon::heartbeat::DaemonStatus::Running);
            hb.update_task_counts(hb_active.load(Ordering::Relaxed), 0);
            let _ = hb.write_heartbeat();
            tokio::time::sleep(hi).await;
        }
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
            _ = tokio::time::sleep(Duration::from_millis(500)) => {
                // Fallback scan: self-healing against watcher death and
                // missed events. Cheap (readdir) and runs at most once
                // per FALLBACK_SCAN_INTERVAL.
                if last_scan.elapsed() >= FALLBACK_SCAN_INTERVAL {
                    last_scan = Instant::now();
                    for path in scan_pending(&cd) {
                        spawn_task(p.clone(), ex.clone(), sem.clone(), active_count.clone(), path.clone());
                    }
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
        let marker = path.with_extension("processing");
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(_) => {}
            Err(_) => return, // already claimed by another event/scan
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
async fn process_one(p: &Protocol, ex: &Executor, path: &std::path::Path) -> Result<(), String> {
    let task = match read_task_retry(path).await {
        Some(t) => t,
        None => {
            let err = format!(
                "cannot parse task file (after {} retries)",
                MAX_READ_RETRIES
            );
            write_failed_result(p, path, &err);
            return Ok(()); // handled: Failed result written
        }
    };

    let tid = task.task_id;
    let _ = p.write_status(&tid, &TaskStatus::Running, Some("executing"));
    match ex.execute(&task).await {
        Ok(r) => {
            let m = match r.status {
                TaskStatus::Completed => Some("ok"),
                _ => None,
            };
            let _ = p.write_result(&tid, &r);
            let _ = p.write_status(&tid, &r.status, m);
            let _ = p.remove_task(&tid);
            println!("  {}: {} ({}ms)", tid, r.status, r.duration_ms());
        }
        Err(e) => {
            // Executor error (spawn failure etc.): write a Failed result
            // so the client sees a terminal state, not a forever Pending.
            eprintln!("exec {}: {}", tid, e);
            let _ = p.write_result(
                &tid,
                &TaskResult {
                    task_id: tid,
                    status: TaskStatus::Failed,
                    output: TaskOutput {
                        stdout: String::new(),
                        stderr: e.clone(),
                        exit_code: None,
                    },
                    start_time: chrono::Utc::now(),
                    end_time: chrono::Utc::now(),
                    duration_ms: 0,
                    retries_used: 0,
                    artifacts: Vec::new(),
                    error_message: Some(e),
                },
            );
            let _ = p.write_status(&tid, &TaskStatus::Failed, Some("executor error"));
            let _ = p.remove_task(&tid);
        }
    }
    Ok(())
}

/// Read + parse a task file with retries. Returns None when it still fails.
async fn read_task_retry(path: &std::path::Path) -> Option<Task> {
    for attempt in 0..MAX_READ_RETRIES {
        match std::fs::read_to_string(path) {
            Ok(content) => {
                if let Ok(t) = serde_json::from_str::<Task>(&content) {
                    return Some(t);
                }
            }
            Err(_) => { /* file may still be being written; retry below */ }
        }
        if attempt + 1 < MAX_READ_RETRIES {
            tokio::time::sleep(RETRY_BACKOFF).await;
        }
    }
    None
}

/// Write a minimal Failed result for a task that could not even be parsed.
/// task_id is extracted from the filename ({timestamp}_{uuid}.json).
fn write_failed_result(p: &Protocol, path: &std::path::Path, error: &str) {
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
        start_time: chrono::Utc::now(),
        end_time: chrono::Utc::now(),
        duration_ms: 0,
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: Some(error.to_string()),
    };
    let _ = p.write_result(&tid, &result);
    let _ = p.write_status(&tid, &TaskStatus::Failed, Some("parse error"));
    let _ = p.remove_task(&tid);
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
