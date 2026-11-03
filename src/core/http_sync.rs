// HTTP sync transport — Nexus raw hosted 仓库 → 本地 shared_storage 双向同步
//
// 设计决策（feat/cloud-storage，2026-11-03 与架构师/刘鑫对齐）：
// - **D1 atomic_upload=false**：Nexus Raw 每个 PUT 落独立 blob，文件级原子性天然成立；
//   status/results JSON 都很小，单次 PUT 默认即可。.tmp 两段式只是两个不同 key，
//   并非真正 rename，留成 opt-in。
// - **D4 transport="http"**：描述的是机制不是后端；"cloud" 太模糊。
// - **走 curl 子进程**而非 reqwest/ureq：零新依赖、复用系统 CA/代理、跨平台，
//   且 bifrost 已有 ssh_bridge 的 spawn 子进程先例。
// - **不实现 Bridge trait**：Nexus raw 无 list API，`list_pending_tasks` 无法表达。
//   而是提供 fetcher（远端 → 本地）+ pusher（本地 → 远端）两个同步器，
//   daemon 本身继续用本地 Protocol + inotify，HTTP 只是传输层。
//
// 目录约定（与 Protocol 一致）：
//   {base_url}/commands/<filename>   — client 上传，daemon 下载后删除
//   {base_url}/results/<filename>    — daemon 上传，client 下载后删除
//   {base_url}/status/<filename>     — daemon 上传，client 下载后删除
//   {base_url}/cancel/<filename>     — client 上传，daemon 下载后删除

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::core::error::{BifrostError, Result};
use crate::core::settings::HttpSection;

/// HTTP 同步器：封装一次同步循环所需的全部状态。
#[derive(Debug, Clone)]
pub struct HttpSync {
    /// 远端仓库根 URL（已 strip 尾部 /）
    pub base_url: String,
    /// HTTP basic auth（user, password）
    pub auth: Option<(String, String)>,
    /// 本地 shared_storage 根
    pub local_root: PathBuf,
    /// 单次请求超时
    pub timeout: Duration,
    /// atomic_upload 开关（D1：默认 false）
    pub atomic_upload: bool,
}

impl HttpSync {
    /// 从 settings 的 http section 构建。密码按 env > settings 顺序取：
    ///   BIFROST_HTTP_USER / BIFROST_HTTP_PASSWORD > http.user / http.password
    /// 实际部署严禁把真实密码写进 settings.json。
    pub fn from_settings(cfg: &HttpSection, local_root: PathBuf) -> Result<Self> {
        let base_url = cfg
            .base_url
            .clone()
            .ok_or_else(|| BifrostError::ConfigInvalid("http.base_url required".into()))?;
        let base_url = base_url.trim_end_matches('/').to_string();

        let user = std::env::var("BIFROST_HTTP_USER")
            .ok()
            .or_else(|| cfg.user.clone());
        let password = std::env::var("BIFROST_HTTP_PASSWORD")
            .ok()
            .or_else(|| cfg.password.clone());

        let auth = match (user, password) {
            (Some(u), Some(p)) => Some((u, p)),
            _ => None,
        };

        Ok(Self {
            base_url,
            auth,
            local_root,
            timeout: cfg
                .request_timeout
                .unwrap_or_else(|| Duration::from_secs(30)),
            atomic_upload: cfg.atomic_upload.unwrap_or(false),
        })
    }

    /// 拼出某个子目录下文件的远端 URL。
    fn remote_url(&self, subdir: &str, filename: &str) -> String {
        format!("{}/{}/{}", self.base_url, subdir, filename)
    }

    /// curl 公共参数（auth、超时、静默、fail-fast）。
    fn curl_base(&self) -> Command {
        let mut cmd = Command::new("curl");
        cmd.arg("--silent")
            .arg("--show-error")
            .arg("--fail") // 4xx/5xx 返回非零，不输出 body
            .arg("--max-time")
            .arg(self.timeout.as_secs().to_string());
        if let Some((u, p)) = &self.auth {
            cmd.arg("--user").arg(format!("{}:{}", u, p));
        }
        cmd
    }

    /// 把本地文件 PUT 到远端 subdir。
    ///
    /// D1 决策：atomic_upload=false（默认）单次 PUT 即可；
    /// atomic_upload=true 时先 PUT `<name>.tmp` 再 PUT 正式名
    /// （注意：在 Nexus raw 上这不是 rename，只是两个独立 key；
    /// 仅用于传大文件时降低被撞见半截文件的概率）。
    pub fn upload(&self, local_path: &Path, subdir: &str, remote_name: &str) -> Result<()> {
        if !local_path.exists() {
            return Err(BifrostError::IoError(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("local file not found: {}", local_path.display()),
            )));
        }

        if self.atomic_upload {
            let tmp_name = format!("{}.tmp", remote_name);
            let tmp_url = self.remote_url(subdir, &tmp_name);
            self.curl_put(local_path, &tmp_url)?;
            // 第二步：正式名
            let final_url = self.remote_url(subdir, remote_name);
            self.curl_put(local_path, &final_url)?;
            // 清理 .tmp（Nexus raw 上它不是 rename，所以多余一个 key 要删）
            let _ = self.delete(subdir, &tmp_name); // best-effort
            Ok(())
        } else {
            let url = self.remote_url(subdir, remote_name);
            self.curl_put(local_path, &url)
        }
    }

    fn curl_put(&self, local_path: &Path, url: &str) -> Result<()> {
        let out = self
            .curl_base()
            .arg("--upload-file")
            .arg(local_path)
            .arg(url)
            .output()
            .map_err(|e| BifrostError::ExecutionError(format!("curl spawn failed: {}", e)))?;
        if !out.status.success() {
            return Err(BifrostError::ExecutionError(format!(
                "curl PUT {} failed (exit {}): {}",
                url,
                out.status,
                String::from_utf8_lossy(&out.stderr).trim()
            )));
        }
        Ok(())
    }

    /// 从远端下载文件到本地。远端 404 返回 Ok(false)，其它错误上抛。
    pub fn download(&self, subdir: &str, remote_name: &str, local_path: &Path) -> Result<bool> {
        if let Some(parent) = local_path.parent() {
            std::fs::create_dir_all(parent).map_err(BifrostError::IoError)?;
        }
        let url = self.remote_url(subdir, remote_name);
        let out = self
            .curl_base()
            .arg("--output")
            .arg(local_path)
            .arg(&url)
            .output()
            .map_err(|e| BifrostError::ExecutionError(format!("curl spawn failed: {}", e)))?;
        if out.status.success() {
            return Ok(true);
        }
        // curl --fail 对 404  exit 22；其它错误原样上抛
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("404") || stderr.contains("The requested URL returned error: 404") {
            return Ok(false);
        }
        Err(BifrostError::ExecutionError(format!(
            "curl GET {} failed (exit {}): {}",
            url,
            out.status,
            stderr.trim()
        )))
    }

    /// HEAD 探测远端对象是否存在。
    pub fn exists(&self, subdir: &str, remote_name: &str) -> Result<bool> {
        let url = self.remote_url(subdir, remote_name);
        let out = self
            .curl_base()
            .arg("--head")
            .arg(&url)
            .output()
            .map_err(|e| BifrostError::ExecutionError(format!("curl spawn failed: {}", e)))?;
        Ok(out.status.success())
    }

    /// 删除远端对象。404 视为成功（幂等）。
    pub fn delete(&self, subdir: &str, remote_name: &str) -> Result<()> {
        let url = self.remote_url(subdir, remote_name);
        let out = self
            .curl_base()
            .arg("--request")
            .arg("DELETE")
            .arg(&url)
            .output()
            .map_err(|e| BifrostError::ExecutionError(format!("curl spawn failed: {}", e)))?;
        if out.status.success() {
            return Ok(());
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("404") {
            return Ok(()); // 已经不在远端，幂等
        }
        Err(BifrostError::ExecutionError(format!(
            "curl DELETE {} failed (exit {}): {}",
            url,
            out.status,
            stderr.trim()
        )))
    }

    /// 询问 Nexus 的 listing 接口拿某个 prefix 下的对象名列表。
    ///
    /// Nexus raw hosted 仓库支持 `/service/rest/v1/assets?repository=<repo>&<q>`，
    /// 但 tmp-raw 这种"随用随删"通道通常只给 PUT/GET/DELETE 最小权限账号，
    /// 列目录权限大概率没有 —— 所以 bifrost 的 fetcher 走另一条路：
    /// **靠本地维护一份 manifest（已下载的 remote_name 列表）+ 远端 HEAD 探测已知 id**。
    ///
    /// 这个函数只是占位，目前总是返回 Err，提示调用方走 manifest 路径。
    /// 未来若 SRE 给 bifrost 专用账号开了 list 权限，在这里实现真正的 listing。
    pub fn list_remote(&self, _subdir: &str) -> Result<Vec<String>> {
        Err(BifrostError::ConfigInvalid(
            "Nexus raw 账号无 listing 权限；fetcher 使用 manifest 跟踪远端对象".into(),
        ))
    }
}

// ─── fetcher / pusher 同步循环 ───────────────────────────────────────────

/// fetcher：把远端 commands/cancel 拉到本地 shared_storage，让本地 daemon
/// 的 Protocol+watcher 能消费。同时把本地 results/status 推回远端（pusher 的对偶）。
///
/// Nexus raw 没有列目录能力，所以策略是：
/// 1. **被动方向**（本地 → 远端）：本地文件系统可变，直接扫目录上传 + 成功后删本地
/// 2. **主动方向**（远端 → 本地）：远端对象名不可枚举，要求 client 在 PUT 任务时
///    额外维护一个远端 manifest (`manifest/commands.txt`)，fetcher 先下载 manifest、
///    按名单逐个 GET，下载成功后从 manifest 剔除并 PUT 回去
///
/// 注：manifest 方案在 V1 先用最简实现 —— 单行一个 filename，PUT 全量覆盖。
/// 并发写的安全问题在 V1 不解决（同一时间窗一般只有一个 client）。
pub struct Fetcher {
    pub sync: HttpSync,
    pub state_file: PathBuf,
}

pub struct Pusher {
    pub sync: HttpSync,
}

impl Fetcher {
    pub fn new(sync: HttpSync) -> Self {
        let state_file = sync.local_root.join(".fetcher_state.json");
        Self { sync, state_file }
    }

    /// 从远端 manifest 下载新的 commands/cancel，落到本地目录。
    /// 每成功拉一个文件，同时从远端 DELETE 掉，并把 manifest 更新回远端。
    pub fn sync_once(&self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();

        // 1. 下载远端 manifest
        let manifest_local = self.sync.local_root.join(".remote_manifest.txt");
        let manifest_exists = self
            .sync
            .download(".", "commands_manifest.txt", &manifest_local)?;

        if !manifest_exists {
            return Ok(stats); // 远端还没任何任务，直接返回
        }

        let manifest_content =
            std::fs::read_to_string(&manifest_local).map_err(BifrostError::IoError)?;
        let remote_files: Vec<String> = manifest_content
            .lines()
            .map(|l| l.trim())
            .filter(|l| !l.is_empty())
            .map(|l| l.to_string())
            .collect();

        // 2. 逐个下载 + 落地 + 远端删除
        let mut remaining = Vec::new();
        for name in remote_files {
            let local = self.sync.local_root.join("commands").join(&name);
            match self.sync.download("commands", &name, &local) {
                Ok(true) => {
                    // 远端删除
                    if let Err(e) = self.sync.delete("commands", &name) {
                        eprintln!("[fetcher] delete remote {} failed: {}", name, e);
                        // 不中断，但保留在 manifest 里下次重试删
                        remaining.push(name.clone());
                    } else {
                        stats.fetched += 1;
                    }
                }
                Ok(false) => {
                    // 已经被别人取走，从 manifest 剔除
                }
                Err(e) => {
                    eprintln!("[fetcher] download {} failed: {}", name, e);
                    remaining.push(name.clone()); // 留下次重试
                }
            }
        }

        // 3. 更新远端 manifest
        let new_manifest = remaining.join("\n");
        let new_manifest_local = self.sync.local_root.join(".remote_manifest.new");
        std::fs::write(&new_manifest_local, &new_manifest).map_err(BifrostError::IoError)?;
        self.sync
            .upload(&new_manifest_local, ".", "commands_manifest.txt")?;
        let _ = std::fs::remove_file(&new_manifest_local);
        let _ = std::fs::remove_file(&manifest_local);

        Ok(stats)
    }
}

impl Pusher {
    pub fn new(sync: HttpSync) -> Self {
        Self { sync }
    }

    /// 把本地 results/、status/ 下所有文件推到远端，成功后本地删除。
    pub fn sync_once(&self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        for subdir in ["results", "status"] {
            let dir = self.sync.local_root.join(subdir);
            if !dir.exists() {
                continue;
            }
            for entry in std::fs::read_dir(&dir).map_err(BifrostError::IoError)? {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => continue,
                };
                let path = entry.path();
                if !path.is_file() {
                    continue;
                }
                // 跳过 .tmp / .lock 伴生
                let name = entry.file_name().to_string_lossy().to_string();
                if name.ends_with(".tmp") || name.ends_with(".lock") {
                    continue;
                }
                self.sync.upload(&path, subdir, &name)?;
                // 上传成功后删除本地
                let _ = std::fs::remove_file(&path);
                stats.pushed += 1;
            }
        }
        Ok(stats)
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub fetched: usize,
    pub pushed: usize,
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_sync_url_construction() {
        let sync = HttpSync {
            base_url: "https://nexus.example.com/repository/tmp-raw/bifrost".into(),
            auth: Some(("u".into(), "p".into())),
            local_root: PathBuf::from("/tmp/x"),
            timeout: Duration::from_secs(30),
            atomic_upload: false,
        };
        assert_eq!(
            sync.remote_url("commands", "abc.json"),
            "https://nexus.example.com/repository/tmp-raw/bifrost/commands/abc.json"
        );
    }

    #[test]
    fn test_from_settings_env_overrides() {
        // 这个测试只验证字段读取，不真的发 HTTP
        let cfg = HttpSection {
            base_url: Some("https://example.com/raw".into()),
            user: Some("cfg_user".into()),
            password: Some("cfg_pass".into()),
            ..Default::default()
        };
        let sync = HttpSync::from_settings(&cfg, PathBuf::from("/tmp/x")).unwrap();
        assert_eq!(sync.auth, Some(("cfg_user".into(), "cfg_pass".into())));
        assert!(!sync.atomic_upload); // D1 默认 false
    }

    #[test]
    fn test_atomic_upload_defaults_false() {
        // D1 决策硬编码验证：不设 atomic_upload 字段时必须是 false
        let cfg = HttpSection {
            base_url: Some("https://example.com/raw".into()),
            ..Default::default()
        };
        let sync = HttpSync::from_settings(&cfg, PathBuf::from("/tmp/x")).unwrap();
        assert!(!sync.atomic_upload);
    }
}
