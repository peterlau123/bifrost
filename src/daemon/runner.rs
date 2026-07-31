// Server runner
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use crate::core::models::TaskStatus;
use crate::core::bridge::Bridge;
use crate::core::protocol::Protocol;
use crate::core::settings::BifrostSettings;
use crate::daemon::executor::Executor;
use crate::daemon::heartbeat::Heartbeat;
use crate::daemon::watcher::AsyncFileWatcher;

pub async fn run_server(s: BifrostSettings, sd: Arc<AtomicBool>) -> Result<(), String> {
    let ss = s.shared_storage.clone(); let cd = ss.join("commands"); let ld = ss.join("logs");
    if !cd.exists() { std::fs::create_dir_all(&cd).map_err(|e| format!("mkdir: {}", e))?; }
    let p = Arc::new(Protocol::new(ss.clone()).map_err(|e| format!("p: {}", e))?);
    let to = s.daemon.task_timeout.unwrap_or(Duration::from_secs(300));
    let ex = Executor::new(ld, to).map_err(|e| format!("e: {}", e))?;
    let mut hb = Heartbeat::new(ss.clone()).map_err(|e| format!("hb: {}", e))?;
    let hi = s.daemon.heartbeat_interval.unwrap_or(Duration::from_secs(60));
    let hbs = sd.clone();
    tokio::spawn(async move { loop { if hbs.load(Ordering::Relaxed) { break; }
        hb.update_status(crate::daemon::heartbeat::DaemonStatus::Running); hb.update_task_counts(0, 0); let _ = hb.write_heartbeat();
        tokio::time::sleep(hi).await; } });
    let mut rx = AsyncFileWatcher::new(cd.clone()).map_err(|e| format!("w: {}", e))?.watch_async().await;
    // 并发执行: 尊重 max_concurrent 配置 (README 示例默认 10)
    let mc = s.daemon.max_concurrent.unwrap_or(10).clamp(1, 100);
    let sem = Arc::new(tokio::sync::Semaphore::new(mc));
    println!("Server ready, watching {} (max_concurrent={})", cd.display(), mc);
    loop {
        if sd.load(Ordering::Relaxed) { break; }
        tokio::select! {
            Some(path) = rx.recv() => {
                let p = p.clone(); let ex = ex.clone(); let sem = sem.clone();
                tokio::spawn(async move {
                    let _permit = sem.acquire().await;
                    let c = match std::fs::read_to_string(&path) { Ok(c) => c, Err(e) => { eprintln!("r: {}", e); return; } };
                    let t: crate::core::models::Task = match serde_json::from_str(&c) { Ok(t) => t, Err(e) => { eprintln!("p: {}", e); return; } };
                    let tid = t.task_id; let _ = p.write_status(&tid, &TaskStatus::Running, Some("executing"));
                    match ex.execute(&t).await {
                        Ok(r) => { let _ = p.write_result(&tid, &r); let m = match r.status { TaskStatus::Completed => Some("ok"), _ => None };
                            let _ = p.write_status(&tid, &r.status, m); let _ = p.remove_task(&tid);
                            println!("  {}: {} ({}ms)", tid, r.status, r.duration_ms()); }
                        Err(e) => { eprintln!("exec: {}", e); }
                    }
                });
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
    println!("Server stopped."); Ok(())
}
