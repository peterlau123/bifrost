// HTTP Bridge pusher - uploads local results/ status/ cancel/ blobs to the remote
// relay, then deletes the local copy after a confirmed PUT. Symmetric to the
// fetcher, but in the (target → relay) direction.
//
// Design: docs/design/http-bridge.md §5 "Pusher 行为" + D11 (sequential mode).
//
// Why "delete local after PUT" is enough for V1:
//   sequential mode guarantees exactly one in-flight task, so no concurrent
//   writer can race the pusher. The local filesystem is therefore a perfect
//   work queue: if the file is there, we haven't pushed it yet; if it's
//   gone, we have. No manifest needed.
//
// Local files we care about:
//   results/<task_id>_result.json    → remote results/<task_id>_result.json
//   status/<task_id>.json            → remote status/<task_id>.json
//   cancel/<task_id>.json            → remote cancel/<task_id>.json
//
// Other dirs:
//   commands/                     CLI submits locally; the daemon removes
//                                these on completion; the pusher must NOT
//                                upload a local commands file (that would
//                                double-submit).
//   artifacts/, logs/             Out of scope for the HTTP transport in
//                                V1 (too big for the bare PUT/GET relay
//                                pattern; dual-stream bandwidth channel
//                                §7 YAGNI).
//   .fetcher-state/               Ledger; never uploaded.
//   heartbeat.json / scan.json    Daemon-local liveness; not relayed.

use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::Result;
use crate::core::http_bridge::HttpBridge;
use crate::core::settings::HttpSection;

/// One pusher sweep over the local shared_storage. Returns how many of each
/// kind got uploaded, plus the files that failed (so the caller can warn
/// without losing the rest of the round).
pub struct Pusher {
    shared_storage: PathBuf,
    bridge: HttpBridge,
}

/// Per-round outcome.
#[derive(Debug, Default, Clone, Copy)]
pub struct PushRoundOutcome {
    pub results: usize,
    pub status: usize,
    pub cancel: usize,
    pub failed: usize,
}

impl Pusher {
    /// Construct from settings' `[http]` section. Returns Err if the config
    /// is invalid (cannot derive URL, missing prefix, https violation, etc).
    pub fn new(shared_storage: &Path, http_cfg: &HttpSection) -> Result<Self> {
        let bridge = HttpBridge::new(http_cfg)?;
        Ok(Self {
            shared_storage: shared_storage.to_path_buf(),
            bridge,
        })
    }

    /// One round: walk each of the three directories, PUT each file's bytes
    /// to the matching remote path, then unlink the local file on success.
    pub fn push_round(&self) -> Result<PushRoundOutcome> {
        self.recompute_round()
    }

    /// Helper that does the canonical three-dir sweep without the early-exit
    /// dance. Returns accurate per-dir succeeded + total-failed counts.
    fn recompute_round(&self) -> Result<PushRoundOutcome> {
        let mut out = PushRoundOutcome::default();
        for (subdir, kind) in [
            ("results", PushKind::Result),
            ("status", PushKind::Status),
            ("cancel", PushKind::Cancel),
        ] {
            let (ok, fail) = self.push_dir(subdir, kind)?;
            out.failed += fail;
            match kind {
                PushKind::Result => out.results += ok,
                PushKind::Status => out.status += ok,
                PushKind::Cancel => out.cancel += ok,
            }
        }
        Ok(out)
    }

    /// Scan a local subdir and PUT each file. Returns (succeeded, failed).
    /// Skips `.tmp.*` (in-progress atomic writes by daemon) and `.*` files
    /// (editor swap) aggressively: we only move *_result.json under results/
    /// and *.json elsewhere.
    fn push_dir(&self, subdir: &str, kind: PushKind) -> Result<(usize, usize)> {
        let local = self.shared_storage.join(subdir);
        let Ok(rd) = fs::read_dir(&local) else {
            // Missing directory is normal for a freshly-initialised storage;
            // return zero rather than failing the whole pusher loop.
            return Ok((0, 0));
        };
        let mut ok = 0;
        let mut fail = 0;
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || name.ends_with(".tmp") {
                continue;
            }
            if !kind.accepts(&name) {
                continue;
            }
            let path = entry.path();
            let body = match fs::read(&path) {
                Ok(b) => b,
                Err(e) => {
                    log::warn!("pusher: read {}: {e}", path.display());
                    fail += 1;
                    continue;
                }
            };
            match self.bridge.put_path(subdir, &name, body) {
                Ok(()) => {
                    if let Err(e) = fs::remove_file(&path) {
                        log::warn!(
                            "pusher: uploaded {} but cannot remove local: {e} (will retry next round and might duplicate)",
                            path.display()
                        );
                    }
                    ok += 1;
                }
                Err(e) => {
                    // network/auth errors: leave the file in place for the
                    // next round, just tally and continue with the rest of
                    // the sweep.
                    log::warn!("pusher: upload {}/{}: {e}", subdir, name);
                    fail += 1;
                }
            }
        }
        Ok((ok, fail))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PushKind {
    Result,
    Status,
    Cancel,
}

impl PushKind {
    fn accepts(&self, name: &str) -> bool {
        match self {
            PushKind::Result => name.ends_with("_result.json"),
            PushKind::Status => name.ends_with(".json") && !name.ends_with("_result.json"),
            PushKind::Cancel => name.ends_with(".json"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::settings::{AuthKind, HttpAuthSection, HttpSection};
    use std::io::{Read as IoRead, Write as IoWrite};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc as StdArc;
    use tempfile::TempDir;

    // Reuse the same minimal HTTP/1.0 mock pattern from http_bridge.rs but
    // locally scoped (no shared test infra to import across modules without
    // unused-dep warnings).
    type Handler = Box<dyn Fn(&str, &str, Vec<(String, String)>, Vec<u8>) -> u16 + Send + Sync>;
    type MockReq = (String, String, Vec<(String, String)>, Vec<u8>);

    fn spawn_mock(hits: StdArc<AtomicUsize>, handler: Handler) -> std::net::SocketAddr {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock");
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut s) = stream else { break };
                let Some((method, path, hdrs, body)) = read_minimal(&mut s) else {
                    continue;
                };
                hits.fetch_add(1, Ordering::SeqCst);
                let status = handler(&method, &path, hdrs, body);
                let status_text = match status {
                    200 => "OK",
                    201 => "Created",
                    204 => "No Content",
                    401 => "Unauthorized",
                    500 => "Internal Server Error",
                    _ => "Unknown",
                };
                let _ = s.write_all(
                    format!(
                        "HTTP/1.1 {} {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        status, status_text
                    )
                    .as_bytes(),
                );
                let _ = s.shutdown(std::net::Shutdown::Both);
            }
        });
        addr
    }

    fn read_minimal(stream: &mut TcpStream) -> Option<MockReq> {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok()?;
        let mut buf = Vec::new();
        let mut chunk = [0u8; 2048];
        let mut content_length = 0usize;
        let mut header_end = None;
        for _ in 0..16 {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = find_crlf(&buf) {
                            header_end = Some(pos);
                            let head = String::from_utf8_lossy(&buf[..pos]).to_string();
                            for line in head.lines() {
                                if line.to_ascii_lowercase().starts_with("content-length:") {
                                    if let Some(v) = line.split(':').nth(1) {
                                        content_length = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(he) = header_end {
                        let body_have = buf.len() - (he + 4);
                        if body_have >= content_length {
                            break;
                        }
                    }
                }
                Err(_) => break,
            }
        }
        let he = header_end?;
        let head_str = String::from_utf8_lossy(&buf[..he]).to_string();
        let mut lines = head_str.lines();
        let request_line = lines.next()?;
        let mut parts = request_line.split_whitespace();
        let method = parts.next()?.to_string();
        let path = parts.next()?.to_string();
        let hdrs: Vec<(String, String)> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        let body = buf[(he + 4)..].to_vec();
        Some((method, path, hdrs, body))
    }

    fn find_crlf(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn mk_settings(addr: std::net::SocketAddr) -> HttpSection {
        HttpSection {
            base_url: Some(format!("http://{}/repository/tmp-raw/uploads", addr)),
            repository: Some("tmp-raw".into()),
            auth: Some(HttpAuthSection {
                kind: AuthKind::Basic,
                user: Some("u".into()),
                password: Some("p".into()),
            }),
            prefix: Some("team-lx/push-test".into()),
            ..HttpSection::default()
        }
    }

    #[test]
    fn push_round_moves_results_status_cancel_and_deletes_local() {
        let hits = StdArc::new(AtomicUsize::new(0));
        let addr = spawn_mock(StdArc::clone(&hits), Box::new(|_, _, _, _| 201));
        let settings = mk_settings(addr);

        let dir = TempDir::new().unwrap();
        for sub in ["results", "status", "cancel"] {
            fs::create_dir_all(dir.path().join(sub)).unwrap();
        }
        fs::write(
            dir.path().join("results").join("id_result.json"),
            r#"{"task_id":"id"}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("status").join("id.json"),
            r#"{"task_id":"id","status":"Completed"}"#,
        )
        .unwrap();
        fs::write(
            dir.path().join("cancel").join("id.json"),
            r#"{"task_id":"id"}"#,
        )
        .unwrap();

        let pusher = Pusher::new(dir.path(), &settings).unwrap();
        let out = pusher.push_round().unwrap();
        assert_eq!(out.results, 1);
        assert_eq!(out.status, 1);
        assert_eq!(out.cancel, 1);
        assert_eq!(out.failed, 0);
        // local files removed after successful upload
        assert!(!dir.path().join("results/id_result.json").exists());
        assert!(!dir.path().join("status/id.json").exists());
        assert!(!dir.path().join("cancel/id.json").exists());
        // three PUT requests
        assert_eq!(hits.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn push_round_skips_commands_logs_fetcher_state() {
        let hits = StdArc::new(AtomicUsize::new(0));
        let addr = spawn_mock(StdArc::clone(&hits), Box::new(|_, _, _, _| 201));
        let settings = mk_settings(addr);

        let dir = TempDir::new().unwrap();
        // Files we MUST NOT upload.
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::create_dir_all(dir.path().join("logs")).unwrap();
        fs::create_dir_all(dir.path().join(".fetcher-state")).unwrap();
        fs::write(dir.path().join("commands").join("cmd_xxx.json"), "{}").unwrap();
        fs::write(dir.path().join("logs").join("task.log"), "log line").unwrap();
        fs::write(dir.path().join(".fetcher-state").join("abc"), "{}").unwrap();

        // Nothing to push
        let pusher = Pusher::new(dir.path(), &settings).unwrap();
        let out = pusher.push_round().unwrap();
        assert_eq!(out.results, 0);
        assert_eq!(out.status, 0);
        assert_eq!(out.cancel, 0);
        assert_eq!(hits.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn push_round_keeps_local_on_401() {
        let hits = StdArc::new(AtomicUsize::new(0));
        let addr = spawn_mock(StdArc::clone(&hits), Box::new(|_, _, _, _| 401));
        let settings = mk_settings(addr);

        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("results")).unwrap();
        let target = dir.path().join("results/id_result.json");
        fs::write(&target, "{}").unwrap();

        let pusher = Pusher::new(dir.path(), &settings).unwrap();
        let out = pusher.push_round().unwrap();
        assert_eq!(out.results, 0);
        assert_eq!(out.failed, 1);
        // local file preserved for retry next round
        assert!(target.exists());
    }

    #[test]
    fn push_round_filters_tmp_and_non_json() {
        let hits = StdArc::new(AtomicUsize::new(0));
        let addr = spawn_mock(StdArc::clone(&hits), Box::new(|_, _, _, _| 201));
        let settings = mk_settings(addr);

        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("results")).unwrap();
        fs::write(dir.path().join("results/a_result.json"), "{}").unwrap();
        fs::write(dir.path().join("results/b_result.json.tmp"), "{}").unwrap();
        fs::write(dir.path().join("results/c.txt"), "not json").unwrap();

        let pusher = Pusher::new(dir.path(), &settings).unwrap();
        let out = pusher.push_round().unwrap();
        assert_eq!(out.results, 1);
        // The accepted file is gone; the others remain.
        assert!(!dir.path().join("results/a_result.json").exists());
        assert!(dir.path().join("results/b_result.json.tmp").exists());
        assert!(dir.path().join("results/c.txt").exists());
    }

    #[test]
    fn push_round_empty_when_dirs_missing() {
        let addr = spawn_mock(StdArc::new(AtomicUsize::new(0)), Box::new(|_, _, _, _| 201));
        let settings = mk_settings(addr);
        let dir = TempDir::new().unwrap();
        // No subdirs at all → no error, zero counts.
        let pusher = Pusher::new(dir.path(), &settings).unwrap();
        let out = pusher.push_round().unwrap();
        assert_eq!(out.results, 0);
        assert_eq!(out.status, 0);
        assert_eq!(out.cancel, 0);
    }
}
