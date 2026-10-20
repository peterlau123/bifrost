// Server runner
//
// Robustness design:
//   - Inotify events are the fast path; a low-frequency fallback scan picks
//     up tasks inotify missed (watcher failure, event overflow, submissions
//     made while the daemon was down). The scan thread sleeps between ticks
//     (no busy-polling), so an idle daemon costs ~0 CPU.
//   - Claim marker: commands/{name}.processing created atomically
//     (create_new). Whichever path (inotify or scan) creates it first owns
//     the task; the other skips it. A keepalive thread refreshes the marker
//     mtime while the task runs, and markers older than
//     `stale_marker_timeout` are reclaimed, so a crashed daemon cannot wedge
//     a task forever.
//   - Read/parse failures are retried (up to 3x with backoff); if a task
//     still can't be parsed, a Failed result is written so the client never
//     sees a forever-Pending task.
//   - Executor errors also produce a Failed result instead of silence.
//   - Optional GPU pool: each task gets an exclusive GPU id injected as
//     CUDA_VISIBLE_DEVICES (unless the task sets it itself).
use crate::core::models::{Task, TaskOutput, TaskResult, TaskStatus};
use crate::core::protocol::Protocol;
use crate::core::settings::{BifrostSettings, ScanMode};
use crate::daemon::clock_probe;
use crate::daemon::executor::{Executor, ExecutorOptions, DEFAULT_TIMEOUT_SECS};
use crate::daemon::gpu_pool::GpuPool;
use crate::daemon::heartbeat::Heartbeat;
use crate::daemon::watcher::AsyncFileWatcher;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

/// Fallback scan cadence when `daemon.scan_interval` is not configured.
const DEFAULT_SCAN_INTERVAL: Duration = Duration::from_secs(1);
/// Read/parse retries with 200ms backoff.
const MAX_READ_RETRIES: u32 = 3;
const RETRY_BACKOFF: Duration = Duration::from_millis(200);
/// Upper bound for the claim-marker keepalive tick; shortened automatically
/// when stale_marker_timeout is small.
const MARKER_KEEPALIVE_MAX: Duration = Duration::from_secs(60);
const MARKER_KEEPALIVE_MIN: Duration = Duration::from_secs(2);

pub async fn run_server(s: BifrostSettings, sd: Arc<AtomicBool>) -> Result<(), String> {
    let ss = s.shared_storage.clone();
    let cd = ss.join("commands");
    let ld = ss.join("logs");
    let cancel_dir = ss.join("cancel");
    let artifacts_dir = ss.join("artifacts");
    for dir in [&cd, &cancel_dir, &artifacts_dir] {
        if !dir.exists() {
            std::fs::create_dir_all(dir).map_err(|e| format!("mkdir {}: {}", dir.display(), e))?;
        }
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

    let default_timeout = s
        .daemon
        .task_timeout
        .unwrap_or(Duration::from_secs(DEFAULT_TIMEOUT_SECS));
    let scan_interval = s.daemon.scan_interval.unwrap_or(DEFAULT_SCAN_INTERVAL);
    let stale_after = s
        .daemon
        .stale_marker_timeout
        .unwrap_or_else(|| default_timeout.saturating_mul(2));
    let use_shell = s.daemon.use_shell.unwrap_or(true);
    let cancel_poll = s.daemon.cancel_poll.unwrap_or(Duration::from_secs(1));
    let log_max_bytes = crate::daemon::executor::DEFAULT_LOG_MAX_BYTES;
    let log_backups = s.daemon.log_backups.unwrap_or(1);

    // 启动自检：只有确认单调时钟不可靠才背 busy-poll 的 CPU 成本。
    // H20 上记录过时钟冻死导致 sleep 永不返回；H200 等新机器大概率正常。
    let scan_mode = s.daemon.scan_mode;
    let health = match scan_mode {
        ScanMode::Sleep => clock_probe::ClockHealth::Ok,
        ScanMode::Spin => clock_probe::probe(), // 仍然探一次，仅用于日志
        ScanMode::Auto => clock_probe::probe(),
    };
    let spin = matches!(scan_mode, ScanMode::Spin) || health.is_frozen();
    // Retries are opt-in per task (Task.retry_count defaults to 0). This is
    // only a ceiling: unset means "no cap", not "no retries", so
    // `client submit --retries N` works without daemon-side config.
    let max_retries = s.daemon.max_retries.unwrap_or(u8::MAX);
    let gpu_ids = s.daemon.gpu_pool.clone().unwrap_or_default();
    // Keepalive must fire well inside the stale window, otherwise a running
    // task can be mistaken for a crashed one.
    let keepalive = (stale_after / 3).clamp(MARKER_KEEPALIVE_MIN, MARKER_KEEPALIVE_MAX);

    let p = Arc::new(Protocol::new(ss.clone()).map_err(|e| format!("p: {}", e))?);
    let ex = Executor::with_options(
        ld,
        ExecutorOptions {
            default_timeout,
            use_shell,
            max_retries,
            cancel_dir: Some(cancel_dir.clone()),
            artifacts_dir: Some(artifacts_dir),
            cancel_poll,
            log_max_bytes,
            log_backups,
        },
    )
    .map_err(|e| format!("e: {}", e))?;
    let gpu = Arc::new(GpuPool::new(gpu_ids.clone()));

    let mut hb = Heartbeat::new(ss.clone()).map_err(|e| format!("hb: {}\n", e))?;
    let hi = s
        .daemon
        .heartbeat_interval
        .unwrap_or(Duration::from_secs(60));
    let hbs = sd.clone();
    let active_count = Arc::new(AtomicUsize::new(0));
    let hb_active = active_count.clone();
    // The heartbeat runs on its own std::thread (not tokio::spawn): a GPFS
    // synchronous write on an async worker gets starved at high concurrency
    // and the heartbeat stops updating.
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

    // Concurrency: max_concurrent tasks at once (README default 10). With a
    // GPU pool the effective ceiling is the pool size - extra slots would only
    // sit idle waiting for a device.
    let mut mc = s.daemon.max_concurrent.unwrap_or(10).clamp(1, 100);
    if !gpu_ids.is_empty() {
        mc = mc.min(gpu_ids.len());
    }
    let sem = Arc::new(tokio::sync::Semaphore::new(mc));
    // Tasks owned by this process. Reclaim never touches these, regardless of
    // marker age or clock skew.
    let active_tasks: Arc<Mutex<HashSet<uuid::Uuid>>> = Arc::new(Mutex::new(HashSet::new()));

    // Fallback scan: a dedicated thread that sleeps between ticks. It used to
    // busy-poll with yield_now() to survive a frozen CLOCK_MONOTONIC on H20
    // VMs, but that burned a full core on every daemon. The watcher remains
    // the event-driven fast path, and the keepalive + stale reclaim below make
    // a missed scan recoverable instead of fatal.
    {
        let cd_s = cd.clone();
        let p_s = p.clone();
        let ex_s = ex.clone();
        let sem_s = sem.clone();
        let ac_s = active_count.clone();
        let sd_s = sd.clone();
        let gpu_s = gpu.clone();
        let at_s = active_tasks.clone();
        let ss_beacon = ss.clone();
        let handle = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let mut ticks: u64 = 0;
            let mut yields: u32 = 0;
            while !sd_s.load(Ordering::Relaxed) {
                // spin 模式不依赖任何定时器；sleep 模式正常节流。
                let should_scan = if spin {
                    yields = yields.wrapping_add(1);
                    yields.is_multiple_of(10_000)
                } else {
                    true
                };
                if !should_scan {
                    std::thread::yield_now();
                    continue;
                }
                ticks += 1;
                // Liveness beacon: the MCP health check can tell a dead scan
                // thread from a quiet one without relying on this thread's
                // own clock.
                write_scan_beacon(&ss_beacon, ticks);
                // No point scanning when every slot is busy.
                if sem_s.available_permits() > 0 {
                    for path in scan_pending(&cd_s) {
                        let p2 = p_s.clone();
                        let ex2 = ex_s.clone();
                        let sem2 = sem_s.clone();
                        let ac2 = ac_s.clone();
                        let gpu2 = gpu_s.clone();
                        let at2 = at_s.clone();
                        let ka2 = keepalive;
                        let h = handle.clone();
                        h.spawn(async move {
                            spawn_task(p2, ex2, sem2, ac2, at2, gpu2, ka2, path);
                        });
                    }
                }
                if spin {
                    std::thread::yield_now();
                } else {
                    std::thread::sleep(scan_interval);
                }
            }
        });
    }
    // Reclaim runs on its own thread so a wedged scan thread cannot disable
    // self-healing. Slow cadence: this only stats markers.
    {
        let cd_r = cd.clone();
        let sd_r = sd.clone();
        let at_r = active_tasks.clone();
        std::thread::spawn(move || {
            while !sd_r.load(Ordering::Relaxed) {
                reclaim_stale_markers(&cd_r, stale_after, &at_r);
                std::thread::sleep(Duration::from_secs(10));
            }
        });
    }
    if spin {
        eprintln!(
            "WARNING: clock self-check says {} - fallback scan switched to busy-poll \
             (this costs ~1 CPU core; set daemon.scan_mode=\"sleep\" to override)",
            health.summary()
        );
    } else {
        println!("  clock: {} (sleep-driven scan)", health.summary());
    }
    println!(
        "Server ready, watching {} (max_concurrent={}, scan={}ms, shell={}, gpus={:?})",
        cd.display(),
        mc,
        scan_interval.as_millis(),
        use_shell,
        gpu_ids
    );

    // Initial catch-up: tasks submitted before the server started
    // (e.g. during restart) must not be silently lost.
    for path in scan_pending(&cd) {
        spawn_task(
            p.clone(),
            ex.clone(),
            sem.clone(),
            active_count.clone(),
            active_tasks.clone(),
            gpu.clone(),
            keepalive,
            path,
        );
    }

    loop {
        if sd.load(Ordering::Relaxed) {
            break;
        }
        // 每 500ms 醒一次：否则安静的 daemon 永远卡在 rx.recv() 上，
        // Ctrl+C 只置了标志、没人看，进程就"杀不死"（H200 实测）。
        match tokio::time::timeout(Duration::from_millis(500), rx.recv()).await {
            Ok(Some(path)) => {
                spawn_task(
                    p.clone(),
                    ex.clone(),
                    sem.clone(),
                    active_count.clone(),
                    active_tasks.clone(),
                    gpu.clone(),
                    keepalive,
                    path,
                );
            }
            Ok(None) => {
                // watcher 通道关闭：不再有事件，靠兜底扫描继续消费任务。
                eprintln!("watcher channel closed; relying on fallback scan");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
            Err(_) => {} // 超时：回到循环顶部检查 shutdown
        }
    }
    println!("Server stopped.");
    Ok(())
}

/// Spawn a task processing future. Claim-marker dedup happens inside so
/// inotify events and fallback scans never double-execute a task.
#[allow(clippy::too_many_arguments)]
fn spawn_task(
    p: Arc<Protocol>,
    ex: Executor,
    sem: Arc<tokio::sync::Semaphore>,
    active: Arc<AtomicUsize>,
    active_tasks: Arc<Mutex<HashSet<uuid::Uuid>>>,
    gpu: Arc<GpuPool>,
    keepalive_tick: Duration,
    path: PathBuf,
) {
    tokio::spawn(async move {
        let _permit = sem.acquire().await;
        // Claim marker: atomic create_new - whoever creates it first owns
        // the task. .processing is excluded from scans, so no re-entry.
        let marker = path.with_extension("processing");
        let marker_for_claim = marker.clone();
        let task_id = task_id_from_path(&path);
        let owner = marker_owner_line(task_id);
        let claimed = tokio::task::spawn_blocking(move || {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&marker_for_claim)
            {
                Ok(mut f) => {
                    // Ownership info lets the reclaimer tell "another live
                    // daemon owns this" from "the owner died".
                    let _ = std::io::Write::write_all(&mut f, owner.as_bytes());
                    true
                }
                Err(_) => false,
            }
        })
        .await
        .unwrap_or(false);
        if !claimed {
            return; // already claimed by another event/scan (or stale marker)
        }
        if let Some(tid) = task_id {
            if let Ok(mut set) = active_tasks.lock() {
                set.insert(tid);
            }
        }

        // Refresh the marker mtime while we run so the stale reclaimer does
        // not steal a legitimately long task.
        let stop_keepalive = Arc::new(AtomicBool::new(false));
        let keepalive =
            spawn_marker_keepalive(marker.clone(), stop_keepalive.clone(), keepalive_tick);

        active.fetch_add(1, Ordering::Relaxed);
        let result = process_one(&p, &ex, &gpu, &path).await;
        active.fetch_sub(1, Ordering::Relaxed);
        if let Some(tid) = task_id {
            if let Ok(mut set) = active_tasks.lock() {
                set.remove(&tid);
            }
        }

        stop_keepalive.store(true, Ordering::Relaxed);
        let _ = keepalive.join();
        let _ = std::fs::remove_file(&marker);

        if let Err(e) = result {
            eprintln!("task {} failed: {}", path.display(), e);
        }
    });
}

/// Read, parse, execute one task file and write the result back.
/// Never leaves a task in a forever-Pending state: parse failures and
/// executor errors both produce a terminal TaskResult.
async fn process_one(
    p: &Arc<Protocol>,
    ex: &Executor,
    gpu: &Arc<GpuPool>,
    path: &Path,
) -> Result<(), String> {
    let task = match read_task_retry(path).await {
        Some(t) => t,
        None => {
            let err = format!(
                "cannot parse task file (after {} retries)",
                MAX_READ_RETRIES
            );
            // Dual-channel race guard: watcher event + fallback scan can both
            // hand the same file to spawn_task. After the first handler
            // finishes it deletes the .json, so a second claim that fails to
            // read must NOT write a Failed result over the real one.
            if !path.exists() {
                return Ok(());
            }
            write_failed_result(p, path, &err).await;
            return Ok(());
        }
    };

    let tid = task.task_id;

    // Cancel requested while the task was still queued?
    if p.has_cancel(&tid) {
        write_terminal_result(
            p,
            &task,
            TaskStatus::Cancelled,
            "cancelled before execution",
        )
        .await;
        p.clear_cancel(&tid);
        return Ok(());
    }

    let p1 = p.clone();
    let _ = blocking_write(move || p1.write_status(&tid, &TaskStatus::Running, Some("executing")))
        .await;

    // Optional GPU allocation: one task per GPU id, injected as
    // CUDA_VISIBLE_DEVICES unless the task already pins one.
    let mut task_for_exec = task.clone();
    let mut _gpu_lease = None;
    if gpu.is_enabled() {
        // Waiting for a device can take hours, so stay cancelable while queued.
        let cancel_dir = p.cancel_dir().to_path_buf();
        let pinned = task_for_exec
            .env_vars
            .get("CUDA_VISIBLE_DEVICES")
            .and_then(|v| v.split(',').next())
            .and_then(|v| v.trim().parse::<u32>().ok());
        let lease = match pinned {
            // A task that pins a device still reserves that slot, otherwise it
            // can collide with a task the pool hands the same GPU to.
            Some(id) => {
                tokio::select! {
                    biased;
                    l = gpu.clone().acquire_specific(id) => l,
                    _ = wait_cancel(&cancel_dir, tid, ex.cancel_poll()) => None,
                }
            }
            None => {
                tokio::select! {
                    biased;
                    l = gpu.clone().acquire() => l,
                    _ = wait_cancel(&cancel_dir, tid, ex.cancel_poll()) => None,
                }
            }
        };
        match lease {
            Some(lease) => {
                task_for_exec
                    .env_vars
                    .entry("CUDA_VISIBLE_DEVICES".to_string())
                    .or_insert_with(|| lease.gpu_id.to_string());
                _gpu_lease = Some(lease);
            }
            None => {
                if p.has_cancel(&tid) {
                    write_terminal_result(
                        p,
                        &task,
                        TaskStatus::Cancelled,
                        "cancelled while waiting for a GPU",
                    )
                    .await;
                    p.clear_cancel(&tid);
                    return Ok(());
                }
            }
        }
    }

    match ex.execute(&task_for_exec).await {
        Ok(r) => {
            let m = match r.status {
                TaskStatus::Completed => Some("ok"),
                _ => None,
            };
            let r_status = r.status.clone();
            let r_dur = r.duration_ms();
            println!("  {}: {} ({}ms)", tid, r_status, r_dur);
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

/// Resolve when a cancel request file appears for this task.
async fn wait_cancel(dir: &Path, task_id: uuid::Uuid, poll: Duration) {
    let path = dir.join(format!("{}.json", task_id));
    while !path.exists() {
        tokio::time::sleep(poll).await;
    }
}

/// Write a terminal result + status and consume the task file.
async fn write_terminal_result(p: &Arc<Protocol>, task: &Task, status: TaskStatus, message: &str) {
    let tid = task.task_id;
    let now = chrono::Utc::now();
    let msg = message.to_string();
    let result = TaskResult {
        task_id: tid,
        status: status.clone(),
        output: TaskOutput {
            stdout: String::new(),
            stderr: msg.clone(),
            exit_code: None,
        },
        command: task.command.clone(),
        start_time: now,
        end_time: now,
        duration_ms: 0,
        retries_used: 0,
        artifacts: Vec::new(),
        error_message: Some(msg.clone()),
    };
    let p1 = p.clone();
    let _ = blocking_write(move || p1.write_result(&tid, &result)).await;
    let p2 = p.clone();
    let status2 = status.clone();
    let msg2 = msg.clone();
    let _ = blocking_write(move || p2.write_status(&tid, &status2, Some(msg2.as_str()))).await;
    let p3 = p.clone();
    let _ = blocking_write(move || p3.remove_task(&tid)).await;
}

/// Touch a task's claim marker periodically until stopped.
fn spawn_marker_keepalive(
    marker: PathBuf,
    stop: Arc<AtomicBool>,
    tick: Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        // Sleep in 1s slices so shutdown does not wait a full tick.
        while !stop.load(Ordering::Relaxed) {
            let mut waited = Duration::ZERO;
            while waited < tick {
                if stop.load(Ordering::Relaxed) {
                    return;
                }
                std::thread::sleep(Duration::from_secs(1));
                waited += Duration::from_secs(1);
            }
            if let Ok(f) = std::fs::OpenOptions::new().write(true).open(&marker) {
                let _ = f.set_modified(SystemTime::now());
            }
        }
    })
}

/// Delete claim markers left behind by a crashed/killed daemon.
///
/// A marker is reclaimed only when none of these hold:
///   * its task is in this process's active set (we own it right now),
///   * its owner line names this host and that pid is still alive,
///   * it is younger than `stale_after`.
///
/// The age check is the last resort for markers owned by another host,
/// where process liveness cannot be checked.
fn reclaim_stale_markers(
    commands_dir: &Path,
    stale_after: Duration,
    active: &Arc<Mutex<HashSet<uuid::Uuid>>>,
) {
    let Ok(entries) = std::fs::read_dir(commands_dir) else {
        return;
    };
    let now = SystemTime::now();
    let local_host = local_hostname();
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.ends_with(".processing") {
            continue;
        }
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(modified) = meta.modified() else {
            continue;
        };
        let age = now.duration_since(modified).unwrap_or_default();
        if age < stale_after {
            continue;
        }

        let marker_path = entry.path();
        let owner = read_marker_owner(&marker_path);
        if let Some(tid) = owner.task {
            if let Ok(set) = active.lock() {
                if set.contains(&tid) {
                    continue; // ours, still running
                }
            }
        }
        if owner.host.as_deref() == Some(local_host.as_str()) {
            if let Some(pid) = owner.pid {
                if pid_alive(pid) {
                    continue; // another live daemon on this host owns it
                }
            }
        }

        let task_path = marker_path.with_extension("json");
        if task_path.exists() {
            eprintln!(
                "reclaiming stale claim marker {} (idle {}s) - task will re-run",
                name,
                age.as_secs()
            );
        }
        let _ = std::fs::remove_file(&marker_path);
    }
}

#[derive(Default)]
struct MarkerOwner {
    host: Option<String>,
    pid: Option<u32>,
    task: Option<uuid::Uuid>,
}

/// Marker payload: `host=<h>;pid=<p>;task=<uuid>;started=<unix>`.
fn marker_owner_line(task: Option<uuid::Uuid>) -> String {
    format!(
        "host={};pid={};task={};started={}\n",
        local_hostname(),
        std::process::id(),
        task.map(|t| t.to_string()).unwrap_or_default(),
        SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    )
}

fn read_marker_owner(path: &Path) -> MarkerOwner {
    let mut owner = MarkerOwner::default();
    let Ok(content) = std::fs::read_to_string(path) else {
        return owner;
    };
    for field in content.split(';') {
        let Some((k, v)) = field.trim().split_once('=') else {
            continue;
        };
        match k.trim() {
            "host" if !v.is_empty() => owner.host = Some(v.trim().to_string()),
            "pid" => owner.pid = v.trim().parse().ok(),
            "task" => owner.task = uuid::Uuid::parse_str(v.trim()).ok(),
            _ => {}
        }
    }
    owner
}

fn local_hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .or_else(|_| std::env::var("HOSTNAME"))
        .unwrap_or_else(|_| "unknown".into())
}

fn pid_alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{}", pid)).exists()
}

/// Write scan liveness so a wedged fallback scanner is detectable.
fn write_scan_beacon(shared: &Path, ticks: u64) {
    let payload = serde_json::json!({
        "pid": std::process::id(),
        "ticks": ticks,
        "scanned_at": chrono::Utc::now().to_rfc3339(),
    });
    let tmp = shared.join(".scan.json.tmp");
    if std::fs::write(&tmp, payload.to_string()).is_ok() {
        let _ = std::fs::rename(&tmp, shared.join("scan.json"));
    }
}

/// Run a blocking Protocol/shared-fs write on the blocking pool.
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

        let first = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker);
        assert!(first.is_ok(), "第一次领取应成功");
        let second = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker);
        assert!(second.is_err(), "第二次领取必须失败 (防重复执行)");
    }

    /// 修复项 5：陈旧的 claim marker 必须被回收，否则任务永久卡死。
    #[test]
    fn test_reclaim_stale_markers() {
        let tmp = TempDir::new().unwrap();
        let cd = tmp.path().join("commands");
        std::fs::create_dir_all(&cd).unwrap();
        let task = cd.join("20260731_103000_550e8400-e29b-41d4-a716-446655440000.json");
        std::fs::write(&task, "{}").unwrap();
        let marker = task.with_extension("processing");
        std::fs::write(&marker, "").unwrap();
        let active = Arc::new(Mutex::new(HashSet::new()));

        // 新鲜 marker 不能被回收
        reclaim_stale_markers(&cd, Duration::from_secs(3600), &active);
        assert!(marker.exists(), "未过期的 marker 不应被删除");

        // 把 mtime 拨回 2 小时前
        let old = SystemTime::now() - Duration::from_secs(7200);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap();
        f.set_modified(old).unwrap();
        drop(f);

        reclaim_stale_markers(&cd, Duration::from_secs(3600), &active);
        assert!(!marker.exists(), "过期 marker 应被回收，任务重新可领取");
        assert!(task.exists(), "回收 marker 不应删除任务文件本身");
    }

    /// marker 归属校验：本机存活 pid 拥有的过期 marker 不得被回收。
    #[test]
    fn test_reclaim_skips_live_owner() {
        let tmp = TempDir::new().unwrap();
        let cd = tmp.path().join("commands");
        std::fs::create_dir_all(&cd).unwrap();
        let tid = uuid::Uuid::new_v4();
        let task = cd.join(format!("20260731_103000_{}.json", tid));
        std::fs::write(&task, "{}").unwrap();
        let marker = task.with_extension("processing");
        std::fs::write(&marker, marker_owner_line(Some(tid))).unwrap();

        let old = SystemTime::now() - Duration::from_secs(7200);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap();
        f.set_modified(old).unwrap();
        drop(f);

        let active = Arc::new(Mutex::new(HashSet::new()));
        reclaim_stale_markers(&cd, Duration::from_secs(10), &active);
        assert!(marker.exists(), "本机存活进程持有的 marker 不应被回收");
    }

    /// marker 归属校验：本进程正在跑的任务即使 mtime 很旧也不回收。
    #[test]
    fn test_reclaim_skips_active_task() {
        let tmp = TempDir::new().unwrap();
        let cd = tmp.path().join("commands");
        std::fs::create_dir_all(&cd).unwrap();
        let tid = uuid::Uuid::new_v4();
        let task = cd.join(format!("20260731_103000_{}.json", tid));
        std::fs::write(&task, "{}").unwrap();
        let marker = task.with_extension("processing");
        std::fs::write(
            &marker,
            format!("host=other;pid=99999999;task={};started=0", tid),
        )
        .unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        let f = std::fs::OpenOptions::new()
            .write(true)
            .open(&marker)
            .unwrap();
        f.set_modified(old).unwrap();
        drop(f);

        let active = Arc::new(Mutex::new(HashSet::new()));
        active.lock().unwrap().insert(tid);
        reclaim_stale_markers(&cd, Duration::from_secs(1), &active);
        assert!(marker.exists(), "活跃任务不得被回收");
    }

    /// 修复项 6：排队中的任务被 cancel 时直接写 Cancelled 结果。
    #[tokio::test]
    async fn test_cancel_before_execution() {
        let tmp = TempDir::new().unwrap();
        let p = Arc::new(Protocol::new(tmp.path().to_path_buf()).unwrap());
        let ex = Executor::new(tmp.path().join("logs"), Duration::from_secs(30)).unwrap();
        let gpu = Arc::new(GpuPool::new(vec![]));

        let task = Task::new("echo hi".into(), crate::core::models::TaskType::Shell);
        let tid = task.task_id;
        p.submit_task(&task).unwrap();
        p.request_cancel(&tid).unwrap();

        let path = tmp.path().join("commands").join(format!(
            "{}_{}.json",
            task.timestamp.format("%Y%m%d_%H%M%S"),
            tid
        ));
        process_one(&p, &ex, &gpu, &path).await.unwrap();

        let result_file = tmp
            .path()
            .join("results")
            .join(format!("{}_result.json", tid));
        let r: TaskResult =
            serde_json::from_str(&std::fs::read_to_string(result_file).unwrap()).unwrap();
        assert_eq!(r.status, TaskStatus::Cancelled);
        assert!(!p.has_cancel(&tid), "cancel 请求处理完应清理");
    }

    /// 修复项 4：GPU 池开启时任务应拿到 CUDA_VISIBLE_DEVICES。
    #[tokio::test]
    async fn test_gpu_pool_assigns_device() {
        let tmp = TempDir::new().unwrap();
        let p = Arc::new(Protocol::new(tmp.path().to_path_buf()).unwrap());
        let ex = Executor::new(tmp.path().join("logs"), Duration::from_secs(30)).unwrap();
        let gpu = Arc::new(GpuPool::new(vec![5]));

        let task = Task::new(
            "echo \"dev=$CUDA_VISIBLE_DEVICES\"".into(),
            crate::core::models::TaskType::Shell,
        );
        let tid = task.task_id;
        p.submit_task(&task).unwrap();
        let path = tmp.path().join("commands").join(format!(
            "{}_{}.json",
            task.timestamp.format("%Y%m%d_%H%M%S"),
            tid
        ));
        process_one(&p, &ex, &gpu, &path).await.unwrap();

        let result_file = tmp
            .path()
            .join("results")
            .join(format!("{}_result.json", tid));
        let r: TaskResult =
            serde_json::from_str(&std::fs::read_to_string(result_file).unwrap()).unwrap();
        assert!(
            r.output.stdout.contains("dev=5"),
            "stdout={}",
            r.output.stdout
        );
    }

    /// 任务自己指定的 CUDA_VISIBLE_DEVICES 优先于 GPU 池。
    #[tokio::test]
    async fn test_task_pinned_gpu_not_overridden() {
        let tmp = TempDir::new().unwrap();
        let p = Arc::new(Protocol::new(tmp.path().to_path_buf()).unwrap());
        let ex = Executor::new(tmp.path().join("logs"), Duration::from_secs(30)).unwrap();
        let gpu = Arc::new(GpuPool::new(vec![5]));

        let task = Task::new(
            "echo \"dev=$CUDA_VISIBLE_DEVICES\"".into(),
            crate::core::models::TaskType::Shell,
        )
        .with_env_var("CUDA_VISIBLE_DEVICES".into(), "7".into());
        let tid = task.task_id;
        p.submit_task(&task).unwrap();
        let path = tmp.path().join("commands").join(format!(
            "{}_{}.json",
            task.timestamp.format("%Y%m%d_%H%M%S"),
            tid
        ));
        process_one(&p, &ex, &gpu, &path).await.unwrap();

        let result_file = tmp
            .path()
            .join("results")
            .join(format!("{}_result.json", tid));
        let r: TaskResult =
            serde_json::from_str(&std::fs::read_to_string(result_file).unwrap()).unwrap();
        assert!(
            r.output.stdout.contains("dev=7"),
            "stdout={}",
            r.output.stdout
        );
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

    /// 双通道竞态回归：任务文件已被第一个处理者消费（删除），第二个
    /// 处理者 claim 成功但 read 失败——**不得写 Failed result 覆盖已完成
    /// 的结果**（2026-08-24 e2e job J1/J4 Failed parse 根因）。
    #[tokio::test]
    async fn test_process_one_skips_missing_task_file() {
        let tmp = TempDir::new().unwrap();
        let p = Arc::new(Protocol::new(tmp.path().to_path_buf()).unwrap());
        let ex = Executor::new(tmp.path().join("logs"), Duration::from_secs(60)).unwrap();
        let gpu = Arc::new(GpuPool::new(vec![]));

        let tid = uuid::Uuid::new_v4();
        let path = tmp
            .path()
            .join("commands")
            .join(format!("20260731_103000_{}.json", tid));

        let r = process_one(&p, &ex, &gpu, &path).await;
        assert!(r.is_ok(), "缺失任务文件应静默返回 Ok");
        let result_file = tmp
            .path()
            .join("results")
            .join(format!("{}_result.json", tid));
        assert!(
            !result_file.exists(),
            "任务文件已被消费时不得写 Failed result 覆盖已完成结果"
        );
    }
}
