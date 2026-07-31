// Clean - purge files of completed tasks to prevent storage growth
//
// Long-running deployments accumulate files across commands/ results/ status/
// logs/ artifacts/. Tasks are considered "finished" when a result file
// exists in results/ (the daemon writes {task_id}_result.json on terminal
// states). Clean removes the full file set of finished tasks older than
// `older_than` days.
//
// Safety invariants:
//   - Only tasks WITH a result file are touched (pending/running are kept)
//   - heartbeat.json / settings.json are never removed
//   - --dry-run previews without deleting

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::core::error::{BifrostError, Result};

/// Files belonging to one finished task
#[derive(Debug, Clone)]
pub struct CleanCandidate {
    pub task_id: String,
    /// Result file path (results/{id}_result.json)
    pub result: PathBuf,
    /// Status file path (status/{id}.json), if exists
    pub status: Option<PathBuf>,
    /// Command files (commands/{id}.json + {id}.lock), if exist
    pub commands: Vec<PathBuf>,
    /// Log dir (logs/{id}/), if exists
    pub logs: Option<PathBuf>,
    /// Artifact files matching {id}_* and dirs matching {id}/, if exist
    pub artifacts: Vec<PathBuf>,
}

/// Scan finished tasks older than `older_than` and return what would be removed.
/// `older_than`: None = all finished tasks (respect age_limit), Some(days).
pub fn scan_finished(
    storage: &Path,
    older_than_days: u64,
    now: SystemTime,
) -> Result<Vec<CleanCandidate>> {
    let results_dir = storage.join("results");
    if !results_dir.exists() {
        return Ok(vec![]);
    }

    let mut candidates = Vec::new();
    for entry in std::fs::read_dir(&results_dir).map_err(BifrostError::IoError)? {
        let entry = entry.map_err(BifrostError::IoError)?;
        let name = entry.file_name().to_string_lossy().to_string();
        // pattern: {uuid}_result.json
        let Some(task_id) = name.strip_suffix("_result.json") else {
            continue;
        };
        // task_id must look like a uuid (36 chars + dashes) to avoid
        // matching unrelated files
        if task_id.len() != 36 {
            continue;
        }

        // age check: result file mtime
        let meta = entry.metadata().map_err(BifrostError::IoError)?;
        let modified = meta.modified().map_err(BifrostError::IoError)?;
        let age = now.duration_since(modified).unwrap_or(Duration::ZERO);
        if age < Duration::from_secs(older_than_days * 86400) {
            continue; // too fresh, keep
        }

        candidates.push(collect_for_task(storage, task_id, entry.path()));
    }
    Ok(candidates)
}

fn collect_for_task(storage: &Path, task_id: &str, result: PathBuf) -> CleanCandidate {
    let status = storage.join("status").join(format!("{}.json", task_id));
    let status = status.exists().then_some(status);

    let mut commands = Vec::new();
    for suffix in ["json", "lock"] {
        let p = storage.join("commands").join(format!("{}.{}", task_id, suffix));
        if p.exists() {
            commands.push(p);
        }
    }

    let logs = storage.join("logs").join(task_id);
    let logs = logs.exists().then_some(logs);

    // artifacts: {task_id}_* files and {task_id}/ dirs
    let mut artifacts = Vec::new();
    let arts_dir = storage.join("artifacts");
    if arts_dir.exists() {
        if let Ok(rd) = std::fs::read_dir(&arts_dir) {
            for e in rd.flatten() {
                let n = e.file_name().to_string_lossy().to_string();
                if n.starts_with(task_id) {
                    artifacts.push(e.path());
                }
            }
        }
    }

    CleanCandidate {
        task_id: task_id.to_string(),
        result,
        status,
        commands,
        logs,
        artifacts,
    }
}

/// Delete all files of the given candidates. Returns total files/dirs removed.
pub fn purge(candidates: &[CleanCandidate]) -> Result<usize> {
    let mut removed = 0;
    for c in candidates {
        let paths: Vec<PathBuf> = std::iter::once(c.result.clone())
            .chain(c.status.iter().cloned())
            .chain(c.commands.iter().cloned())
            .chain(c.artifacts.iter().cloned())
            .collect();
        for p in paths {
            if p.is_file() {
                std::fs::remove_file(&p).map_err(BifrostError::IoError)?;
                removed += 1;
            }
        }
        if let Some(dir) = &c.logs {
            if dir.is_dir() {
                std::fs::remove_dir_all(dir).map_err(BifrostError::IoError)?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, content: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// 构造一个存储区: 2 个过期任务(有 result) + 1 个新任务(有 result) + 1 个未完成任务(无 result)
    fn setup_storage(tmp: &Path) {
        // 过期任务 A (7 天前)
        let a = "aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa";
        write(&tmp.join("results").join(format!("{}_result.json", a)), "{}");
        write(&tmp.join("status").join(format!("{}.json", a)), "{}");
        write(&tmp.join("commands").join(format!("{}.json", a)), "{}");
        write(&tmp.join("logs").join(a).join("stdout.log"), "x");
        write(&tmp.join("artifacts").join(format!("{}_report.json", a)), "{}");
        // 过期任务 B (仅 result + status, 无 logs/artifacts)
        let b = "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb";
        write(&tmp.join("results").join(format!("{}_result.json", b)), "{}");
        write(&tmp.join("status").join(format!("{}.json", b)), "{}");
        // 新任务 C (今天的)
        let c = "cccccccc-cccc-cccc-cccc-cccccccccccc";
        write(&tmp.join("results").join(format!("{}_result.json", c)), "{}");
        // 未完成任务 D (只有 commands, 无 result → 不应清理)
        let d = "dddddddd-dddd-dddd-dddd-dddddddddddd";
        write(&tmp.join("commands").join(format!("{}.json", d)), "{}");
        // 保护文件
        write(&tmp.join("heartbeat.json"), "{}");
        write(&tmp.join("settings.json"), "{}");
    }

    fn set_mtime_old(path: &Path) {
        let old = SystemTime::now() - Duration::from_secs(10 * 86400);
        let file = std::fs::File::open(path).unwrap();
        file.set_modified(old).unwrap();
    }

    #[test]
    fn test_scan_only_finished_old_tasks() {
        let tmp = tempfile::TempDir::new().unwrap();
        setup_storage(tmp.path());

        // 让 A、B 的 result 文件变旧
        set_mtime_old(&tmp.path().join("results").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa_result.json"));
        set_mtime_old(&tmp.path().join("results").join("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb_result.json"));

        let now = SystemTime::now();
        let cands = scan_finished(tmp.path(), 7, now).unwrap();
        let ids: Vec<&str> = cands.iter().map(|c| c.task_id.as_str()).collect();
        assert_eq!(ids, vec!["aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa",
                             "bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb"],
                   "只应扫到过期的已完成任务 A/B, 实际: {:?}", ids);

        // A 的配套文件应全部收集
        let a = cands.iter().find(|c| c.task_id.starts_with("aaaa")).unwrap();
        assert!(a.status.is_some(), "A 的 status 文件应收集");
        assert_eq!(a.commands.len(), 1, "A 的 commands 残留应收集");
        assert!(a.logs.is_some(), "A 的 logs 目录应收集");
        assert_eq!(a.artifacts.len(), 1, "A 的 artifacts 应收集");
        // B 无 logs/artifacts 时保持 None/空
        let b = cands.iter().find(|c| c.task_id.starts_with("bbbb")).unwrap();
        assert!(b.logs.is_none());
        assert!(b.artifacts.is_empty());
    }

    #[test]
    fn test_purge_removes_all_files_keeps_protected() {
        let tmp = tempfile::TempDir::new().unwrap();
        setup_storage(tmp.path());
        set_mtime_old(&tmp.path().join("results").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa_result.json"));
        set_mtime_old(&tmp.path().join("results").join("bbbbbbbb-bbbb-bbbb-bbbb-bbbbbbbbbbbb_result.json"));

        let cands = scan_finished(tmp.path(), 7, SystemTime::now()).unwrap();
        let removed = purge(&cands).unwrap();

        // A: result+status+command+logs dir+artifact = 5; B: result+status = 2
        assert_eq!(removed, 7, "共删除 7 项, 实际: {}", removed);

        // A 的所有文件消失
        assert!(!tmp.path().join("results").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa_result.json").exists());
        assert!(!tmp.path().join("status").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa.json").exists());
        assert!(!tmp.path().join("commands").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa.json").exists());
        assert!(!tmp.path().join("logs").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa").exists());
        assert!(!tmp.path().join("artifacts").join("aaaaaaaa-aaaa-aaaa-aaaa-aaaaaaaaaaaa_report.json").exists());

        // 保护文件仍在
        assert!(tmp.path().join("heartbeat.json").exists(), "heartbeat 必须保留");
        assert!(tmp.path().join("settings.json").exists(), "settings 必须保留");

        // 新任务 C 与未完成 D 保留
        assert!(tmp.path().join("results").join("cccccccc-cccc-cccc-cccc-cccccccccccc_result.json").exists());
        assert!(tmp.path().join("commands").join("dddddddd-dddd-dddd-dddd-dddddddddddd.json").exists());
    }
}
