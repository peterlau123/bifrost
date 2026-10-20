// `bifrost warmup` —— daemon 上线前的环境自检。
//
// 只做"能提前发现、且能据此改配置"的检查，不启动服务、不写业务文件：
//   1. 共享存储是否可写（写一个临时探针文件再删）
//   2. 共享存储的文件系统类型（GPFS / NFS / XFS / Lustre …）
//   3. 时钟自检（即 daemon 启动时那次 probe，见 daemon/clock_probe.rs）
//   4. 现有 daemon 是否活着（heartbeat.json 年龄）
//   5. GPU 可见性（有 nvidia-smi 时）
//
// 退出码：0 = 可上线；1 = 有阻断项（存储不可写 / 时钟异常）。

use crate::core::settings::BifrostSettings;
use crate::daemon::clock_probe;
use std::path::Path;
use std::time::{Duration, SystemTime};

/// statfs 的魔数 → 文件系统名（只需要覆盖常见的共享/本地文件系统）。
fn fs_name(magic: i64) -> &'static str {
    match magic as u64 {
        0x4750_4653 => "gpfs",
        0x6969 => "nfs",
        0x5846_5342 => "xfs",
        0xEF53 => "ext4",
        0x0102_1994 => "tmpfs",
        0x794c_7630 => "overlayfs",
        0x0BD0_0BD0 => "lustre",
        0x9123_683E => "cifs/smb",
        _ => "unknown",
    }
}

fn fs_type_of(path: &Path) -> Option<&'static str> {
    use std::ffi::CString;
    let c = CString::new(path.to_string_lossy().as_bytes()).ok()?;
    let mut buf: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c.as_ptr(), &mut buf) };
    if rc == 0 {
        Some(fs_name(buf.f_type))
    } else {
        None
    }
}

fn heartbeat_age(shared: &Path) -> Option<u64> {
    let meta = std::fs::metadata(shared.join("heartbeat.json")).ok()?;
    SystemTime::now()
        .duration_since(meta.modified().ok()?)
        .ok()
        .map(|d| d.as_secs())
}

fn gpu_count() -> Option<usize> {
    let out = std::process::Command::new("nvidia-smi")
        .arg("-L")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| l.starts_with("GPU "))
            .count(),
    )
}

fn ok(msg: &str) {
    println!("  \u{2713} {}", msg);
}
fn warn(msg: &str) {
    println!("  ! {}", msg);
}
fn bad(msg: &str) {
    println!("  \u{2717} {}", msg);
}

/// Run the checks. Returns the process exit code.
pub fn run(settings: &BifrostSettings) -> i32 {
    let mut fatal = false;
    println!("bifrost warmup");
    println!("  shared_storage: {}", settings.shared_storage.display());

    // 1+2. 存储可写性与文件系统
    let shared = &settings.shared_storage;
    if !shared.exists() {
        match std::fs::create_dir_all(shared) {
            Ok(()) => warn(&format!("created {}", shared.display())),
            Err(e) => {
                bad(&format!("cannot create {}: {}", shared.display(), e));
                fatal = true;
            }
        }
    }
    if shared.exists() {
        let probe = shared.join(".warmup_probe");
        match std::fs::write(&probe, b"ok") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                ok("shared storage is writable");
            }
            Err(e) => {
                bad(&format!("shared storage not writable: {}", e));
                fatal = true;
            }
        }
        match fs_type_of(shared) {
            Some(fs) => ok(&format!("filesystem: {}", fs)),
            None => warn("filesystem type unknown (statfs failed)"),
        }
        let stale_note = match fs_type_of(shared) {
            Some("gpfs") | Some("nfs") | Some("lustre") | Some("cifs/smb") => {
                Some("network/shared fs: inotify may drop events - the fallback scan is what keeps tasks flowing")
            }
            _ => None,
        };
        if let Some(note) = stale_note {
            warn(note);
        }
    }

    // 3. 时钟自检
    let health = clock_probe::probe();
    if health.is_frozen() {
        bad(&format!(
            "clock: {} - sleep() may hang here; set daemon.scan_mode=\"spin\"",
            health.summary()
        ));
        fatal = true;
    } else {
        ok(&format!(
            "clock: {} (sleep-driven scanning is safe)",
            health.summary()
        ));
    }

    // 4. 已有 daemon？
    match heartbeat_age(shared) {
        Some(age) if age < 180 => ok(&format!("daemon heartbeat {}s old (alive)", age)),
        Some(age) => warn(&format!(
            "daemon heartbeat {}s old (stale - daemon down or wedged?)",
            age
        )),
        None => warn("no heartbeat.json yet (daemon not started here?)"),
    }

    // 5. GPU
    match gpu_count() {
        Some(n) if n > 0 => {
            ok(&format!("{} GPU(s) visible via nvidia-smi", n));
            if let Some(pool) = &settings.daemon.gpu_pool {
                if !pool.is_empty() && pool.len() != n {
                    warn(&format!(
                        "daemon.gpu_pool has {} ids but {} GPUs visible",
                        pool.len(),
                        n
                    ));
                }
            }
        }
        Some(_) => warn("nvidia-smi reports 0 GPUs"),
        None => warn("nvidia-smi not available (CPU-only host or not on PATH)"),
    }

    // 建议值提示（不阻断）
    let t = settings
        .daemon
        .task_timeout
        .unwrap_or(Duration::from_secs(5400));
    println!(
        "  . task_timeout={}s, max_concurrent={}, scan_mode={:?}, shell={}",
        t.as_secs(),
        settings.daemon.max_concurrent.unwrap_or(10),
        settings.daemon.scan_mode,
        settings.daemon.use_shell.unwrap_or(true)
    );

    if fatal {
        println!("warmup: FAILED (fix the \u{2717} items above before starting the daemon)");
        1
    } else {
        println!("warmup: OK");
        0
    }
}
