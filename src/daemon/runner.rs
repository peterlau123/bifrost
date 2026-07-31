// SimpleDaemon
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

pub async fn run_daemon(s: BifrostSettings, sd: Arc<AtomicBool>) -> Result<(), String> {
    let ss = s.shared_storage.clone(); let cd = ss.join("commands"); let ld = ss.join("logs");
    if !cd.exists() { std::fs::create_dir_all(&cd).map_err(|e| format!("mkdir: {}", e))?; }
    let p = Protocol::new(ss.clone()).map_err(|e| format!("p: {}", e))?;
    let to = s.daemon.task_timeout.unwrap_or(Duration::from_secs(300));
    let ex = Executor::new(ld, to).map_err(|e| format!("e: {}", e))?;
    let mut hb = Heartbeat::new(ss.clone()).map_err(|e| format!("hb: {}", e))?;
    let hi = s.daemon.heartbeat_interval.unwrap_or(Duration::from_secs(60));
    let hbs = sd.clone();
    tokio::spawn(async move { loop { if hbs.load(Ordering::Relaxed) { break; }
        hb.update_status(crate::daemon::heartbeat::DaemonStatus::Running); hb.update_task_counts(0, 0); let _ = hb.write_heartbeat();
        tokio::time::sleep(hi).await; } });
    let mut rx = AsyncFileWatcher::new(cd.clone()).map_err(|e| format!("w: {}", e))?.watch_async().await;
    println!("Daemon ready, watching {}", cd.display());
    loop {
        if sd.load(Ordering::Relaxed) { break; }
        tokio::select! {
            Some(path) = rx.recv() => {
                let c = match std::fs::read_to_string(&path) { Ok(c) => c, Err(e) => { eprintln!("r: {}", e); continue; } };
                let t: crate::core::models::Task = match serde_json::from_str(&c) { Ok(t) => t, Err(e) => { eprintln!("p: {}", e); continue; } };
                let tid = t.task_id; let _ = p.write_status(&tid, &TaskStatus::Running, Some("executing"));
                match ex.execute(&t).await {
                    Ok(r) => { let _ = p.write_result(&tid, &r); let m = match r.status { TaskStatus::Completed => Some("ok"), _ => None };
                        let _ = p.write_status(&tid, &r.status, m); let _ = p.remove_task(&tid);
                        println!("  {}: {} ({}s)", tid, r.status, r.duration_secs()); }
                    Err(e) => { eprintln!("exec: {}", e); }
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
    println!("Daemon stopped."); Ok(())
}
