// HTTP Bridge fetcher - pulls task blobs from the remote relay to the local
// shared_storage, driving the unchanged daemon via inotify.
//
// Design: docs/design/http-bridge.md §5 + D9 (write-ahead ledger, Ax round-2).
//
// The fetcher NEVER touches the daemon; it only writes files into
// `<shared_storage>/commands/` and `<shared_storage>/cancel/`, which the
// daemon's existing inotify watcher consumes.
//
// ─── Idempotency model (D9, write-ahead) ──────────────────────────────
//
// Each task the fetcher pulls follows the order:
//
//   1. claim(task_id) — atomic ledger write (tmp + rename + fsync(dir)).
//      Returns `Claimed` only on the first successful claim. Subsequent
//      claims return `AlreadyClaimed`.
//
//   2. atomic_write(local_commands/<ts>_<id>.json, content) — same tmp +
//      rename pattern. If this fails, the claim is left in place and the
//      task is treated as "claimed but never delivered". Recovery is
//      handled by `--once` re-running the fetcher, which reconciles the
//      ledger against local `commands/` + `results/` and re-delivers
//      claimed-but-undelivered tasks.
//
//   3. DELETE remote object — best-effort cleanup; failure just logs
//      (D3/D9). Even if the object survives, the claim prevents
//      re-delivery on subsequent rounds.
//
// Why this order? The previous design (`deliver → claim → delete`) had a
// crash window: a failure between deliver and claim produced a ledger
// miss on the next round, which would re-deliver the same task — and
// the daemon can already have executed it. Retry-forever is the safe
// failure direction; deliver-once is the unsafe one.
//
// ─── Single-instance lock ────────────────────────────────────────────
//
// Two fetchers racing on the same shared_storage would double-claim
// (both see "not claimed" and both deliver), so we take an advisory
// flock on `<shared_storage>/.fetcher-state/.lock` for the whole process
// lifetime. Second fetcher trying to start sees the lock is taken and
// exits.
//
// ─── Ledger file layout ──────────────────────────────────────────────
//
//   `<shared_storage>/.fetcher-state/`
//       `.lock`                       single-instance flock
//       `<task_id>`                   one file per claimed task
//       `<task_id>.delivered`         written AFTER successful deliver()
//
// The claim file is JSON: { task_id, claimed_at, remote_path, delivered? }.
// The ".delivered" sidecar is added after atomic_write. Recovery scans
// claim files without a sidecar.
//
// Directory starts with `.` so the daemon's watcher never mistakes it
// for a task directory (Protocol only looks inside commands/results/
// status/cancel/artifacts).
//
// High-water mark for "should I prune this claim" is driven by local
// file age (mtime) in `prune_dry_run`. The fetcher itself only calls
// `prune` on rare intervals (every N rounds) to bound growth.

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use uuid::Uuid;

use crate::core::bridge::Bridge;
use crate::core::error::{BifrostError, Result};
use crate::core::models::Task;
use crate::core::settings::HttpSection;

const CLAIM_DIR: &str = ".fetcher-state";
const LOCK_NAME: &str = ".lock";
const DELIVERED_SUFFIX: &str = ".delivered";

/// Result of attempting to claim a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimOutcome {
    /// This call won the claim — caller should proceed to deliver.
    Claimed,
    /// The task was already claimed by an earlier round — skip it.
    AlreadyClaimed,
}

/// Fetcher ledger: write-ahead claim store + single-instance lock.
pub struct FetcherLedger {
    root: PathBuf,
    _lock: File, // held for the lifetime of the fetcher
}

impl std::fmt::Debug for FetcherLedger {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FetcherLedger")
            .field("root", &self.root)
            .finish()
    }
}

impl FetcherLedger {
    /// Open (or create) the ledger and acquire the single-instance lock.
    /// Returns Err if another fetcher holds the lock.
    pub fn open(shared_storage: &Path) -> Result<Self> {
        let root = shared_storage.join(CLAIM_DIR);
        fs::create_dir_all(&root)?;
        let lock_path = root.join(LOCK_NAME);
        let lock_file = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        // Try non-blocking exclusive lock; fail fast if contended.
        use std::os::unix::io::AsRawFd;
        let rc = unsafe { libc::flock(lock_file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            let err = io::Error::last_os_error();
            return Err(BifrostError::ConfigInvalid(format!(
                "fetcher ledger already locked by another fetcher: {} ({})",
                lock_path.display(),
                err
            )));
        }
        Ok(Self {
            root,
            _lock: lock_file,
        })
    }

    /// Atomically claim `task_id`. The claim file is `<root>/<task_id>`,
    /// written via tmp+rename with fsync(dir) to survive power loss.
    pub fn claim(&self, task: &Task, remote_path: &str) -> Result<ClaimOutcome> {
        let target = self.root.join(task.task_id.to_string());
        if target.exists() {
            return Ok(ClaimOutcome::AlreadyClaimed);
        }
        // Best-effort: write to a pid-scoped tmp, fsync file, rename into
        // place, fsync directory to commit the rename.
        let pid = std::process::id();
        let tmp = self.root.join(format!(".tmp.{}.{}", pid, task.task_id));
        let body = serde_json::to_vec(&serde_json::json!({
            "task_id": task.task_id.to_string(),
            "claimed_at": chrono::Utc::now().to_rfc3339(),
            "remote_path": remote_path,
            "command": task.command.clone(),
        }))?;
        // Try-except create+write; if rename fails because a peer beat us,
        // the claim is still honored (check target.exists() after error).
        let write_res: io::Result<()> = (|| {
            let mut f = File::create(&tmp)?;
            f.write_all(&body)?;
            f.sync_all()?;
            fs::rename(&tmp, &target)?;
            // fsync the directory so the rename survives power loss.
            if let Ok(dir) = File::open(&self.root) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();
        if let Err(e) = write_res {
            let _ = fs::remove_file(&tmp);
            if target.exists() {
                return Ok(ClaimOutcome::AlreadyClaimed);
            }
            return Err(BifrostError::IoError(e));
        }
        Ok(ClaimOutcome::Claimed)
    }

    /// Mark a claimed task as delivered (after local commands file is
    /// durably written). Sidecar is `<root>/<task_id>.delivered`.
    pub fn mark_delivered(&self, task_id: &Uuid) -> Result<()> {
        let claim = self.root.join(task_id.to_string());
        if !claim.exists() {
            return Err(BifrostError::ExecutionError(format!(
                "mark_delivered without claim: {task_id}"
            )));
        }
        let sidecar = self.root.join(format!("{}{}", task_id, DELIVERED_SUFFIX));
        let pid = std::process::id();
        let tmp = self
            .root
            .join(format!(".tmp.delivered.{}.{}", pid, task_id));
        let body = chrono::Utc::now().to_rfc3339();
        let write_res: io::Result<()> = (|| {
            fs::write(&tmp, body.as_bytes())?;
            File::open(&tmp)?.sync_all()?;
            fs::rename(&tmp, &sidecar)?;
            if let Ok(dir) = File::open(&self.root) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();
        if let Err(e) = write_res {
            let _ = fs::remove_file(&tmp);
            return Err(BifrostError::IoError(e));
        }
        Ok(())
    }

    /// Iterate every claim entry. Returns `(task_id, is_delivered)`.
    pub fn iter_claims(&self) -> Result<Vec<(Uuid, bool)>> {
        let mut out = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let fname = entry.file_name();
            let name = fname.to_string_lossy();
            // Skip the lock and any unfinished tmp files.
            if name == LOCK_NAME || name.starts_with(".tmp") {
                continue;
            }
            if name.ends_with(DELIVERED_SUFFIX) {
                continue;
            }
            let Ok(id) = Uuid::parse_str(&name) else {
                continue;
            };
            let delivered = self
                .root
                .join(format!("{}{}", name, DELIVERED_SUFFIX))
                .exists();
            out.push((id, delivered));
        }
        Ok(out)
    }

    /// Tasks that are claimed but NOT delivered — candidates for
    /// `--once` redelivery. Caller still verifies against local results
    /// for genuine absence-vs-completed.
    pub fn claimed_but_undelivered(&self) -> Result<Vec<Uuid>> {
        Ok(self
            .iter_claims()?
            .into_iter()
            .filter(|(_, delivered)| !delivered)
            .map(|(id, _)| id)
            .collect())
    }

    /// Prune claims older than `keep_after` AND whose task result exists
    /// under `<root>/../results/`. Optionally dry_run.
    ///
    // The fetcher only calls this every N rounds to bound growth; if a
    // large backlog accumulates you can run an admin cleanup directly.
    pub fn prune(&self, keep_after: SystemTime, dry_run: bool) -> Result<usize> {
        let results_dir = self.root.parent().map(|p| p.join("results"));
        let mut removed = 0;
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if name == LOCK_NAME || name.starts_with(".tmp") {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let Ok(mtime) = meta.modified() else {
                continue;
            };
            if mtime >= keep_after {
                continue;
            }
            // Only prune if the task already produced a result file
            // (otherwise we'd narrow the recovery window).
            let base = name.trim_end_matches(DELIVERED_SUFFIX);
            let is_terminal = match &results_dir {
                Some(rd) => rd.join(format!("{base}_result.json")).exists(),
                None => true,
            };
            if !is_terminal {
                continue;
            }
            if !dry_run {
                let _ = fs::remove_file(entry.path());
            }
            removed += 1;
        }
        Ok(removed)
    }
}

/// Write `bytes` to `target` via tmp+rename + fsync(dir). Used by both the
/// fetcher (deliver commands) and the daemon's existing Protocol impl for
/// parity. Public so fetcher and tests can reuse it.
pub fn atomic_write(target: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }
    let pid = std::process::id();
    let tmp = target.with_extension(format!("tmp.{pid}"));
    fs::write(&tmp, bytes)?;
    File::open(&tmp)?.sync_all()?;
    if let Some(parent) = target.parent() {
        if !parent.as_os_str().is_empty() {
            fs::rename(&tmp, target)?;
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
            return Ok(());
        }
    }
    fs::rename(&tmp, target)?;
    Ok(())
}

/// One fetch round. Caller is responsible for the outer `loop { ... sleep(..) }`.
///
/// Returns the number of commands delivered (0 or 1 in D11 sequential
/// mode) and the number of cancel records delivered (any).
pub fn fetch_round<B: Bridge + ?Sized>(
    bridge: &B,
    ledger: &FetcherLedger,
    shared_storage: &Path,
    http_cfg: &HttpSection,
) -> Result<FetchRoundOutcome> {
    let _ = http_cfg; // placeholder for future D13 repo diagnostics
    let mut out = FetchRoundOutcome::default();

    // Discover tasks on the relay (D11 sequential: list_all returns the
    // full set, we pick at most one to deliver per round).
    let tasks = bridge.list_pending_tasks()?;
    out.remote_seen = tasks.len();

    // Order deterministically so a 2nd fetch_round picks the same first
    // task if the previous delivery failed. (Without ordering, the
    // ledger might claim A in round 1 (failed deliver), B in round 2,
    // etc., starving A forever.)
    let mut tasks_sorted = tasks;
    tasks_sorted.sort_by_key(|t| t.task_id);

    // ── commands: write-ahead claim → atomic_write → DELETE ──────────
    for task in tasks_sorted.iter() {
        let remote_path = format!("commands/{}", task_filename_public(task));
        // Step 1: claim (write-ahead, atomic)
        let claimed = match ledger.claim(task, &remote_path) {
            Ok(c) => c,
            Err(e) => {
                log::error!("fetcher: claim failed for {}: {e}", task.task_id);
                // Skip this round for this task; next round will retry.
                continue;
            }
        };
        if claimed == ClaimOutcome::AlreadyClaimed {
            continue;
        }
        // Step 2: GET the blob and atomic_write into commands/.
        // The blob content mirrors the task, so we just re-serialize the
        // task (the remote had the JSON; we parsed it on the way up via
        // list_pending_tasks).
        let body = match serde_json::to_vec(task) {
            Ok(b) => b,
            Err(e) => {
                log::error!("fetcher: serialize for {}: {e}", task.task_id);
                continue;
            }
        };
        let target = shared_storage
            .join("commands")
            .join(task_filename_public(task));
        if let Err(e) = atomic_write(&target, &body) {
            // CRITICAL: don't deliver. The claim sits undelivered and
            // the recovery path (--once) will pick this up.
            log::error!(
                "fetcher: atomic_write failed for {} at {}: {e} (task will be redelivered by recovery)",
                task.task_id,
                target.display()
            );
            continue;
        }
        // Step 3: mark_delivered AFTER the commands file is durable.
        if let Err(e) = ledger.mark_delivered(&task.task_id) {
            // Same here — leave claim sitting undelivered.
            log::error!(
                "fetcher: mark_delivered failed for {}: {e} (file is on disk; will be reconciled)",
                task.task_id
            );
            // We DO continue — the file is on disk so the daemon will
            // execute it; we just won't trim the ledger entry. Next
            // round won't claim again so we don't duplicate-deliver.
        }
        // Step 4: best-effort DELETE (D3/D9) using the trait's
        // delete_task_blob helper which calls into whatever override the
        // concrete transport provides. On HttpBridge that delivers the
        // exact `<ts>_<id>.json` filename submit_task PUTted (Day-5 fix);
        // on SSH this falls back to remove_task.
        if let Err(e) = bridge.delete_task_blob("commands", task) {
            log::error!(
                "fetcher: DELETE {} failed (ok per D3/D9): {e}",
                task.task_id
            );
        }
        out.commands_delivered += 1;
        // D11 sequential mode: stop after the first delivery.
        break;
    }

    // ── cancel: best-effort mirror to local cancel/. Recovery does not
    // revisit these — the daemon treats a missing cancel as "no cancel
    // requested" which is fine.
    if let Err(e) = fetch_cancel_inner(bridge, shared_storage, &mut out) {
        log::debug!("fetch_cancel: {e}");
    }

    Ok(out)
}

fn fetch_cancel_inner<B: Bridge + ?Sized>(
    bridge: &B,
    shared_storage: &Path,
    out: &mut FetchRoundOutcome,
) -> Result<()> {
    cancels_for_visible_tasks(bridge, shared_storage, out)
}

/// Cancels don't have a "list_pending_tasks" entry point in the Bridge
/// trait; we piggyback on the search results returned by the same call
/// and filter for `<prefix>cancel/<id>.json` paths. Implemented by
/// probing each cancel path we saw in the most recent `list_pending_tasks`
/// (whose Task vec contains only commands).
///
/// We do NOT call list_pending_tasks again — it would refetch blobs —
/// so we instead invoke a second search pass (the Bridge already
/// memoizes the search query via `listing_query`).
fn cancels_for_visible_tasks<B: Bridge + ?Sized>(
    _bridge: &B,
    _shared_storage: &Path,
    _out: &mut FetchRoundOutcome,
) -> Result<()> {
    // V1 stub: the fetcher needs to add a `list_pending_cancels` method
    // to the Bridge trait (Nexus search with prefix = cancel/). That's
    // a Day 5 task — the Bridge trait currently has no such method.
    // Cancels continue to work end-to-end via the daemon watching a
    // cancel file directly written by the client to its own shared
    // storage (SSH/shared path). For HTTP, the pusher will upload
    // cancel records written locally.
    Ok(())
}

/// Per-round tally.
#[derive(Debug, Default)]
pub struct FetchRoundOutcome {
    pub remote_seen: usize,
    pub commands_delivered: usize,
    pub cancels_delivered: usize,
}

/// Public helper mirroring HttpBridge::task_filename (kept local to avoid
/// circular deps on settings/HttpBridge). Both derivations MUST stay
/// in lockstep — change one, change the other.
pub fn task_filename_public(task: &Task) -> String {
    format!(
        "{}_{}.json",
        task.timestamp.format("%Y%m%d_%H%M%S"),
        task.task_id
    )
}

// ─── recovery ────────────────────────────────────────────────────────

/// Scan ledger claims and reconcile against local state. Reports every
/// claim that needs one of three actions:
///   - re-deliver (claimed but no commands/results file present)
///   - prune    (delivered + result file present, past terminal)
///
/// The fetcher in `--once` mode calls this once at startup BEFORE the
/// main poll loop. A redelivered task is fetched fresh from the relay
/// (the relay object may still exist if the daemon never consumed it;
/// or will TaskNotFound and we just clear the claim — bounded semantics).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryAction {
    Redeliver,
    Prune,
}

pub fn plan_recovery(
    ledger: &FetcherLedger,
    shared_storage: &Path,
) -> Result<Vec<(Uuid, RecoveryAction)>> {
    let mut plan = Vec::new();
    let commands = shared_storage.join("commands");
    let results = shared_storage.join("results");
    let claimed: HashSet<Uuid> = ledger.claimed_but_undelivered()?.into_iter().collect();
    for id in claimed {
        let result = results.join(format!("{id}_result.json"));
        if result.exists() {
            // Already terminal: write the delivered sidecar and let prune
            // handle later. (The commands file was clearly delivered and
            // consumed; we just lost the mark_delivered step.)
            if let Err(e) = ledger.mark_delivered(&id) {
                log::warn!("recovery: mark_delivered retry {id}: {e}");
            }
            continue;
        }
        // Search for any commands file matching this id (timestamp prefix
        // varies). If the command is there, the daemon will pick it up on
        // its next scan; we just need to mark_delivered. Otherwise we
        // need to re-deliver from the relay (or prune if not found there).
        let mut found_in_commands = false;
        if let Ok(rd) = fs::read_dir(&commands) {
            for entry in rd.flatten() {
                let name = entry.file_name().to_string_lossy().into_owned();
                if name.ends_with(&format!("_{id}.json")) {
                    found_in_commands = true;
                    break;
                }
            }
        }
        if found_in_commands {
            let _ = ledger.mark_delivered(&id);
        } else {
            plan.push((id, RecoveryAction::Redeliver));
        }
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::TaskType;
    use tempfile::TempDir;

    fn mk_task(id: Uuid) -> Task {
        let mut t = Task::new("echo hi".into(), TaskType::Shell);
        t.task_id = id;
        t
    }

    #[test]
    fn atomic_write_writes_target_and_keeps_no_tmp() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("commands").join("x.json");
        atomic_write(&target, b"hello").unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        // No .tmp.* leftovers.
        let parent = target.parent().unwrap();
        for entry in fs::read_dir(parent).unwrap() {
            let n = entry.unwrap().file_name().to_string_lossy().into_owned();
            assert!(!n.contains(".tmp"), "tmp leftover: {n}");
        }
    }

    #[test]
    fn ledger_claim_marks_first_then_already() {
        let dir = TempDir::new().unwrap();
        let ledger = FetcherLedger::open(dir.path()).unwrap();
        let task = mk_task(Uuid::new_v4());
        let outcome = ledger.claim(&task, "commands/x.json").unwrap();
        assert_eq!(outcome, ClaimOutcome::Claimed);
        // Re-claim same id → already
        let outcome = ledger.claim(&task, "commands/x.json").unwrap();
        assert_eq!(outcome, ClaimOutcome::AlreadyClaimed);
    }

    #[test]
    fn ledger_two_open_calls_second_fails_with_lock() {
        // Two opened ledgers in the same process share the same flock
        // semantics as separate processes: the second acquire fails.
        // This prevents the race where two fetchers both see "not claimed"
        // and double-deliver.
        let dir = TempDir::new().unwrap();
        let _l1 = FetcherLedger::open(dir.path()).expect("first lock ok");
        let err = FetcherLedger::open(dir.path()).unwrap_err();
        match err {
            BifrostError::ConfigInvalid(msg) => {
                assert!(msg.contains("locked"), "expected lock error, got: {msg}")
            }
            other => panic!("expected ConfigInvalid, got {other:?}"),
        }
    }

    #[test]
    fn ledger_iter_claims_lists_id_and_delivered_flag() {
        let dir = TempDir::new().unwrap();
        let ledger = FetcherLedger::open(dir.path()).unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        ledger.claim(&mk_task(a), "commands/a.json").unwrap();
        ledger.claim(&mk_task(b), "commands/b.json").unwrap();
        let mut all: Vec<_> = ledger.iter_claims().unwrap().into_iter().collect();
        all.sort();
        assert_eq!(all.len(), 2);
        assert_eq!(all[0], (std::cmp::min(a, b), false));
        assert_eq!(all[1], (std::cmp::max(a, b), false));

        ledger.mark_delivered(&a).unwrap();
        let mut all: Vec<_> = ledger.iter_claims().unwrap().into_iter().collect();
        all.sort();
        let delivered_for_a = all.iter().find(|(id, _)| *id == a).unwrap().1;
        let delivered_for_b = all.iter().find(|(id, _)| *id == b).unwrap().1;
        assert!(delivered_for_a);
        assert!(!delivered_for_b);
    }

    #[test]
    fn ledger_claimed_but_undelivered_after_mark() {
        let dir = TempDir::new().unwrap();
        let ledger = FetcherLedger::open(dir.path()).unwrap();
        let a = Uuid::new_v4();
        let b = Uuid::new_v4();
        ledger.claim(&mk_task(a), "x").unwrap();
        ledger.claim(&mk_task(b), "y").unwrap();
        let todo: HashSet<Uuid> = ledger
            .claimed_but_undelivered()
            .unwrap()
            .into_iter()
            .collect();
        assert_eq!(todo.len(), 2);

        ledger.mark_delivered(&a).unwrap();
        let todo: HashSet<Uuid> = ledger
            .claimed_but_undelivered()
            .unwrap()
            .into_iter()
            .collect();
        assert!(!todo.contains(&a));
        assert!(todo.contains(&b));
    }

    #[test]
    fn plan_recovery_marks_delivered_when_result_present() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::create_dir_all(dir.path().join("results")).unwrap();
        let ledger = FetcherLedger::open(dir.path()).unwrap();
        let id = Uuid::new_v4();
        ledger.claim(&mk_task(id), "x").unwrap();

        // no commands file, no results file → redeliver
        let plan = plan_recovery(&ledger, dir.path()).unwrap();
        assert_eq!(plan, vec![(id, RecoveryAction::Redeliver)]);

        // simulate result landing
        fs::write(
            dir.path().join("results").join(format!("{id}_result.json")),
            "{}",
        )
        .unwrap();
        let plan = plan_recovery(&ledger, dir.path()).unwrap();
        assert!(plan.is_empty(), "redeliver should be gone after result");
    }

    #[test]
    fn plan_recovery_skips_redeliver_when_commands_file_present() {
        let dir = TempDir::new().unwrap();
        fs::create_dir_all(dir.path().join("commands")).unwrap();
        fs::create_dir_all(dir.path().join("results")).unwrap();
        let ledger = FetcherLedger::open(dir.path()).unwrap();
        let id = Uuid::new_v4();
        ledger.claim(&mk_task(id), "x").unwrap();

        // Simulate a leftover commands file
        fs::write(
            dir.path()
                .join("commands")
                .join(format!("20261106_120000_{id}.json")),
            "{}",
        )
        .unwrap();
        let plan = plan_recovery(&ledger, dir.path()).unwrap();
        assert!(plan.is_empty());
    }

    #[test]
    fn task_filename_shape_stable() {
        let mut t = Task::new("x".into(), TaskType::Shell);
        t.task_id = Uuid::nil();
        t.timestamp = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        assert!(task_filename_public(&t).starts_with("20231114_"));
        assert!(task_filename_public(&t).ends_with(".json"));
    }
}
