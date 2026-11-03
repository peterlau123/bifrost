// HTTP sync transport — Nexus raw hosted 仓库 ↔ 本地 shared_storage 双向同步
//
// 设计决策（feat/cloud-storage，2026-11-03 与架构师/刘鑫对齐）：
// - **D1 atomic_upload=false**：Nexus Raw 每个 PUT 落独立 blob，文件级原子性天然成立；
//   status/results JSON 都很小，单次 PUT 默认即可。.tmp 两段式只是两个不同 key，
//   并非真正 rename，留成 opt-in。
// - **D4 transport="http"**：描述的是机制不是后端；"cloud" 太模糊。
// - **走 curl 子进程**而非 reqwest/ureq：零新依赖、复用系统 CA/代理、跨平台，
//   且 bifrost 已有 ssh_bridge 的 spawn 子进程先例。
// - **不实现 Bridge trait**：Nexus raw 无 list API，`list_pending_tasks` 无法表达。
//   而是提供 Syncer，包含 fetch（远端 → 本地）+ push（本地 → 远端）两个方向，
//   daemon 本身继续用本地 Protocol + inotify，HTTP 只是传输层。
//
// 目录约定（与 Protocol 一致）：
//   远端 {base_url}/commands/<filename>          ↔ 本地 commands/<filename>
//   远端 {base_url}/commands_manifest.txt        （pusher 写，fetcher 读后删）
//   远端 {base_url}/results/<filename>           ↔ 本地 results/<filename>
//   远端 {base_url}/results_manifest.txt         （pusher 写，fetcher 读后删）
//   远端 {base_url}/status/<filename>            ↔ 本地 status/<filename>
//   远端 {base_url}/status_manifest.txt          （pusher 写，fetcher 读后删）
//   远端 {base_url}/cancel/<filename>            ↔ 本地 cancel/<filename>
//   远端 {base_url}/cancel_manifest.txt          （pusher 写，fetcher 读后删）
//
// 上传后是否删本地：
// - commands/：上传后【不删】本地（仍是 Protocol 的 inbox，等 daemon 消费）
// - results/、status/：上传后【删掉】本地（Nexus raw 是真源，本地只是 outbox）
//   —— 否则 daemon 每轮都会重复上传同一个文件
//   —— 又一个考虑：tmp-raw 是"随用随删"通道，本地多保留一份其实是冗余
// - cancel/：上传后【删掉】本地（理由同 results/）

use std::collections::HashSet;
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

    /// 拼出某个子目录下文件的远端 URL。`subdir=""` 表示仓库根。
    fn remote_url(&self, subdir: &str, filename: &str) -> String {
        if subdir.is_empty() {
            format!("{}/{}", self.base_url, filename)
        } else {
            format!("{}/{}/{}", self.base_url, subdir, filename)
        }
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
        // curl --fail 对 404 exit 22；其它错误原样上抛
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
}

// ─── 模块级工具函数（不依赖 self，便于测试与复用）────────────────────────

/// 读取 manifest（一行一个文件名，忽略空行）。文件不存在视为空集。
pub fn read_manifest(path: &Path) -> Result<HashSet<String>> {
    if !path.exists() {
        return Ok(HashSet::new());
    }
    let content = std::fs::read_to_string(path).map_err(BifrostError::IoError)?;
    Ok(content
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// 把 manifest 集合写回（空集 = 写空串而非删文件，远端更容易判断）。
pub fn write_manifest(path: &Path, set: &HashSet<String>) -> Result<()> {
    let mut names: Vec<String> = set.iter().cloned().collect();
    names.sort();
    let content = if names.is_empty() {
        String::new()
    } else {
        format!("{}\n", names.join("\n"))
    };
    std::fs::write(path, content).map_err(BifrostError::IoError)
}

/// 某个子目录对应的 manifest 文件名（commands → commands_manifest.txt）。
pub fn manifest_name(subdir: &str) -> String {
    format!("{}_manifest.txt", subdir)
}

/// 列出本地某个子目录下所有"应参与同步"的文件名（排除 .tmp/.lock/.processing/.partial）。
pub fn list_local_files(dir: &Path) -> Result<Vec<String>> {
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).map_err(BifrostError::IoError)? {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.ends_with(".tmp")
            || name.ends_with(".lock")
            || name.ends_with(".processing")
            || name.ends_with(".partial")
        {
            continue;
        }
        out.push(name);
    }
    Ok(out)
}

// ─── Syncer：一对 fetch+push ─────────────────────────────────────────────

pub struct Syncer {
    pub sync: HttpSync,
}

/// 单方向（fetch 或 push）一轮的统计。
#[derive(Debug, Default, Clone, Copy)]
pub struct SyncStats {
    pub fetched: usize,
    pub pushed: usize,
}

impl Syncer {
    pub fn new(sync: HttpSync) -> Self {
        Self { sync }
    }

    /// 把远端 manifest 里列出的 `<subdir>/<name>` 拉到本地，成功后：
    ///   1. 把 name 从远端 manifest 里剔除
    ///   2. DELETE 远端 `<subdir>/<name>`（可选；tmp-raw 反正要删，节省空间）
    /// 回写远端 manifest 时，**先重读远端 manifest 再 merge** —— 避免我们 fetch
    /// 期间对端又推了一批新文件导致 manifest 被覆盖丢条目。
    pub fn fetch_subdir(&self, subdir: &str) -> Result<usize> {
        let manifest_name = manifest_name(subdir);

        // 1. 拉远端 manifest（不存在 = 没新东西，直接 0）
        let remote_manifest_local = self
            .sync
            .local_root
            .join(format!(".fetch_{}", manifest_name));
        if !self
            .sync
            .download("", &manifest_name, &remote_manifest_local)?
        {
            let _ = std::fs::remove_file(&remote_manifest_local);
            return Ok(0);
        }
        let remote_set = read_manifest(&remote_manifest_local)?;
        let _ = std::fs::remove_file(&remote_manifest_local);

        // 2. 逐个下载 + 落地
        let mut fetched_names = HashSet::new();
        for name in &remote_set {
            let local = self.sync.local_root.join(subdir).join(name);
            match self.sync.download(subdir, name, &local) {
                Ok(true) => {
                    fetched_names.insert(name.clone());
                }
                Ok(false) => {
                    // 远端对象已被别人清掉 —— 当作已处理（从 manifest 剔除）
                    fetched_names.insert(name.clone());
                }
                Err(e) => {
                    eprintln!("[sync] download {}/{} failed: {}", subdir, name, e);
                    // 保留在 manifest 里，下次重试
                }
            }
        }
        if fetched_names.is_empty() {
            return Ok(0);
        }

        // 3. DELETE 远端（成功下载过的）
        for name in &fetched_names {
            if let Err(e) = self.sync.delete(subdir, name) {
                eprintln!("[sync] delete remote {}/{} failed: {}", subdir, name, e);
            }
        }

        // 4. 回写远端 manifest：**先重读远端，再 merge**，避免覆盖对方刚加的条目
        let remote_manifest_local2 = self
            .sync
            .local_root
            .join(format!(".fetch2_{}", manifest_name));
        let mut new_remote = if self
            .sync
            .download("", &manifest_name, &remote_manifest_local2)?
        {
            read_manifest(&remote_manifest_local2)?
        } else {
            HashSet::new()
        };
        let _ = std::fs::remove_file(&remote_manifest_local2);
        for name in &fetched_names {
            new_remote.remove(name);
        }
        write_manifest(&remote_manifest_local2, &new_remote)?;
        self.sync
            .upload(&remote_manifest_local2, "", &manifest_name)?;
        let _ = std::fs::remove_file(&remote_manifest_local2);

        Ok(fetched_names.len())
    }

    /// 把本地 `<subdir>/` 下所有未在远端 manifest 里的文件推到远端，
    /// 并把它们加入远端 manifest。
    ///
    /// `delete_local_after_push`：
    ///   - commands：false，保留本地让 Protocol/inotify 消费
    ///   - results/status/cancel：true，Nexus raw 是真源
    pub fn push_subdir(&self, subdir: &str, delete_local_after_push: bool) -> Result<usize> {
        let manifest_name = manifest_name(subdir);
        let dir = self.sync.local_root.join(subdir);
        let local_names = list_local_files(&dir)?;
        if local_names.is_empty() {
            return Ok(0);
        }

        // 1. 拉远端 manifest（不存在就是空）
        let remote_manifest_local = self
            .sync
            .local_root
            .join(format!(".push_{}", manifest_name));
        let mut remote_set = if self
            .sync
            .download("", &manifest_name, &remote_manifest_local)?
        {
            read_manifest(&remote_manifest_local)?
        } else {
            HashSet::new()
        };

        // 2. 对本地每个文件：不在远端 manifest → 推 + 加入 manifest
        let mut pushed_count = 0;
        let mut any_upload_failed = false;
        for name in &local_names {
            if remote_set.contains(name) {
                continue; // 已在远端（可能对面还没 fetch 走）
            }
            let local_path = dir.join(name);
            match self.sync.upload(&local_path, subdir, name) {
                Ok(()) => {
                    remote_set.insert(name.clone());
                    pushed_count += 1;
                }
                Err(e) => {
                    eprintln!("[sync] upload {}/{} failed: {}", subdir, name, e);
                    any_upload_failed = true;
                }
            }
        }

        // 3. 上传过的全部成功后，回写远端 manifest
        if pushed_count > 0 {
            write_manifest(&remote_manifest_local, &remote_set)?;
            self.sync
                .upload(&remote_manifest_local, "", &manifest_name)?;
        }
        let _ = std::fs::remove_file(&remote_manifest_local);

        // 4. 可选：删除本地已上传的文件
        if delete_local_after_push && pushed_count > 0 {
            for name in &local_names {
                // 只删【这一轮成功推上去的 + 远端 manifest 里已经有了的】
                if remote_set.contains(name) {
                    let p = dir.join(name);
                    if let Err(e) = std::fs::remove_file(&p) {
                        eprintln!("[sync] remove local {} failed: {}", p.display(), e);
                    }
                }
            }
        }

        if any_upload_failed {
            eprintln!(
                "[sync] push_subdir {} partial: some files not uploaded",
                subdir
            );
        }
        Ok(pushed_count)
    }

    /// 一轮完整同步：所有 4 个子目录先 fetch 再 push。
    ///
    /// 注意 fetch 与 push 的顺序：
    ///   - client 侧：fetch 拿 results/status；push 推 commands/cancel
    ///   - daemon 侧：fetch 拿 commands/cancel；push 推 results/status
    ///
    /// 两端都跑同一个 `bifrost sync`，对称最简单。V1 就这样，单向拆分以后再加。
    pub fn sync_once(&self) -> Result<SyncStats> {
        let mut stats = SyncStats::default();
        for subdir in ["commands", "results", "status", "cancel"] {
            let fetched = self.fetch_subdir(subdir)?;
            stats.fetched += fetched;
        }
        // push: commands 不删本地（是 daemon 的 inbox），其它删
        stats.pushed += self.push_subdir("commands", false)?;
        for subdir in ["results", "status", "cancel"] {
            stats.pushed += self.push_subdir(subdir, true)?;
        }
        Ok(stats)
    }
}

// ─── tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::{TcpListener, TcpStream};

    // ── 纯函数测试（不发 HTTP）────────────────────────────────────────────

    #[test]
    fn test_remote_url_with_and_without_subdir() {
        let sync = HttpSync {
            base_url: "https://n/raw".into(),
            auth: None,
            local_root: PathBuf::from("/tmp/x"),
            timeout: Duration::from_secs(30),
            atomic_upload: false,
        };
        assert_eq!(
            sync.remote_url("commands", "a.json"),
            "https://n/raw/commands/a.json"
        );
        assert_eq!(
            sync.remote_url("", "commands_manifest.txt"),
            "https://n/raw/commands_manifest.txt"
        );
    }

    #[test]
    fn test_from_settings_env_overrides() {
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

    #[test]
    fn test_manifest_name() {
        assert_eq!(manifest_name("commands"), "commands_manifest.txt");
        assert_eq!(manifest_name("results"), "results_manifest.txt");
    }

    #[test]
    fn test_read_write_manifest_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("m.txt");

        // 空集 → 空文件
        let empty: HashSet<String> = HashSet::new();
        write_manifest(&p, &empty).unwrap();
        assert_eq!(read_manifest(&p).unwrap(), empty);

        // 非空：一行一个，排序可重现
        let mut set = HashSet::new();
        set.insert("b.json".to_string());
        set.insert("a.json".to_string());
        write_manifest(&p, &set).unwrap();
        let got = read_manifest(&p).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.contains("a.json"));
        assert!(got.contains("b.json"));

        // 重写时去重排序
        let content = std::fs::read_to_string(&p).unwrap();
        assert_eq!(content, "a.json\nb.json\n");
    }

    #[test]
    fn test_read_manifest_missing_file_is_empty() {
        let tmp = tempfile::TempDir::new().unwrap();
        let p = tmp.path().join("nonexistent.txt");
        let s = read_manifest(&p).unwrap();
        assert!(s.is_empty());
    }

    #[test]
    fn test_list_local_files_filters_sidecars() {
        let tmp = tempfile::TempDir::new().unwrap();
        let dir = tmp.path().join("results");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("good.json"), "{}").unwrap();
        std::fs::write(dir.join("skipme.tmp"), "").unwrap();
        std::fs::write(dir.join("skipme.lock"), "").unwrap();
        std::fs::write(dir.join("skipme.processing"), "").unwrap();
        std::fs::write(dir.join("skipme.partial"), "").unwrap();
        let names = list_local_files(&dir).unwrap();
        assert_eq!(names, vec!["good.json".to_string()]);
    }

    // ── Mock HTTP server（用 std TcpListener，零外部依赖）─────────────────

    /// 接收 N 个 HTTP 请求 (method, path, body)，回固定的 (status, body)。
    /// 返回 RxJoinHandle —— join 它会返回收集到的所有请求。
    struct MockServer {
        addr: std::net::SocketAddr,
        handle: Option<std::thread::JoinHandle<Vec<(String, String, Vec<u8>)>>>,
    }

    impl MockServer {
        /// `expect` = 期望收到的请求个数；每个都回 `response_status`。
        fn spawn(expect: usize, response_status: u16, response_body: &'static str) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let handle = std::thread::spawn(move || {
                let mut captured = Vec::new();
                for _ in 0..expect {
                    let (mut stream, _) = listener.accept().unwrap();
                    let (method, path, body) = Self::read_request(&mut stream);
                    captured.push((method, path, body));
                    let status_line = match response_status {
                        200 => "200 OK",
                        201 => "201 Created",
                        404 => "404 Not Found",
                        500 => "500 Internal Server Error",
                        _ => "200 OK",
                    };
                    let resp = format!(
                        "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        status_line,
                        response_body.len(),
                        response_body
                    );
                    stream.write_all(resp.as_bytes()).unwrap();
                    stream.flush().unwrap();
                }
                captured
            });
            Self {
                addr,
                handle: Some(handle),
            }
        }

        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }

        fn read_request(stream: &mut TcpStream) -> (String, String, Vec<u8>) {
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut request_line = String::new();
            reader.read_line(&mut request_line).unwrap();
            let parts: Vec<&str> = request_line.trim().split_whitespace().collect();
            let method = parts.get(0).unwrap_or(&"").to_string();
            let path = parts.get(1).unwrap_or(&"").to_string();

            // Read headers
            let mut content_length = 0usize;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).unwrap();
                let line = line.trim();
                if line.is_empty() {
                    break;
                }
                if let Some(v) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                    content_length = v.trim().parse().unwrap_or(0);
                }
            }
            let mut body = vec![0u8; content_length];
            if content_length > 0 {
                reader.read_exact(&mut body).unwrap();
            }
            (method, path, body)
        }

        fn join(mut self) -> Vec<(String, String, Vec<u8>)> {
            self.handle.take().unwrap().join().unwrap()
        }
    }

    fn make_sync(server_url: &str, local_root: PathBuf) -> HttpSync {
        HttpSync {
            base_url: server_url.into(),
            auth: None,
            local_root,
            timeout: Duration::from_secs(5),
            atomic_upload: false,
        }
    }

    #[test]
    fn test_upload_against_mock() {
        let server = MockServer::spawn(1, 201, "");
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("a.json");
        std::fs::write(&f, "{\"k\":\"v\"}").unwrap();

        let sync = make_sync(&server.url(), tmp.path().to_path_buf());
        sync.upload(&f, "commands", "a.json").unwrap();

        let captured = server.join();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, "PUT");
        assert_eq!(captured[0].1, "/commands/a.json");
        assert_eq!(captured[0].2, b"{\"k\":\"v\"}");
    }

    #[test]
    fn test_download_404_returns_false() {
        let server = MockServer::spawn(1, 404, "not found");
        let tmp = tempfile::TempDir::new().unwrap();
        let f = tmp.path().join("out.json");

        let sync = make_sync(&server.url(), tmp.path().to_path_buf());
        let got = sync.download("commands", "nope.json", &f).unwrap();
        assert!(!got);
        assert!(!f.exists(), "404 不应该在本地残留文件");
    }

    #[test]
    fn test_delete_remote_404_is_idempotent_ok() {
        let server = MockServer::spawn(1, 404, "");
        let tmp = tempfile::TempDir::new().unwrap();
        let sync = make_sync(&server.url(), tmp.path().to_path_buf());
        // 删除远端不存在对象 → 应该 Ok(())，幂等
        sync.delete("commands", "ghost.json").unwrap();
        let captured = server.join();
        assert_eq!(captured[0].0, "DELETE");
    }

    #[test]
    fn test_fetch_subdir_happy_path() {
        // Scenario: 远端有 commands_manifest.txt（列了 a.json）+ commands/a.json
        // 期望：fetcher 下载 manifest → 下载 a.json → 落盘到 commands/ →
        //       DELETE 远端 commands/a.json → 重读远端 manifest（已空）→
        //       上传空 manifest
        //
        // 共 5 个请求：manifest GET, a GET, a DELETE, manifest GET再读, manifest PUT
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://{}", addr);

        let handle = std::thread::spawn(move || {
            let mut responses = vec![
                (200u16, "a.json\n".to_string()), // GET /commands_manifest.txt
                (200u16, "{\"task\":\"x\"}".to_string()), // GET /commands/a.json
                (200u16, "".to_string()),         // DELETE /commands/a.json
                (404u16, "".to_string()), // GET /commands_manifest.txt (re-read; already gone)
                (201u16, "".to_string()), // PUT /commands_manifest.txt
            ];
            let mut captured = Vec::new();
            for _ in 0..5 {
                let (mut stream, _) = listener.accept().unwrap();
                let (m, p, b) = MockServer::read_request(&mut stream);
                captured.push((m, p, b));
                let (status, body) = responses.remove(0);
                let status_line = if status == 200 {
                    "200 OK"
                } else if status == 201 {
                    "201 Created"
                } else {
                    "404 Not Found"
                };
                let resp = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
                stream.flush().unwrap();
            }
            captured
        });

        let tmp = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(tmp.path().join("commands")).unwrap();
        let sync = make_sync(&server_url, tmp.path().to_path_buf());
        let fetcher = Syncer::new(sync);
        let got = fetcher.fetch_subdir("commands").unwrap();
        assert_eq!(got, 1);
        // 文件必须落地
        let local = tmp.path().join("commands").join("a.json");
        assert!(local.exists());
        assert_eq!(std::fs::read_to_string(&local).unwrap(), "{\"task\":\"x\"}");

        let captured = handle.join().unwrap();
        assert_eq!(captured[0].0, "GET");
        assert_eq!(captured[0].1, "/commands_manifest.txt");
        assert_eq!(captured[1].0, "GET");
        assert_eq!(captured[1].1, "/commands/a.json");
        assert_eq!(captured[2].0, "DELETE");
        assert_eq!(captured[2].1, "/commands/a.json");
        assert_eq!(captured[3].0, "GET");
        assert_eq!(captured[3].1, "/commands_manifest.txt");
        assert_eq!(captured[4].0, "PUT");
        assert_eq!(captured[4].1, "/commands_manifest.txt");
    }

    #[test]
    fn test_push_subdir_adds_to_manifest_and_skips_existing() {
        // Scenario: 本地 results/ 里有两个文件 r1.json + r2.json；
        //           远端 manifest 已经包含 r1.json。
        // 期望：pusher 只上传 r2.json，并把 r1+r2 都写回 manifest；
        //       delete_local_after_push=true  → 本地两个文件都删（r1 被认为「已在远端」）。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://{}", addr);

        let handle = std::thread::spawn(move || {
            let responses = vec![
                (200u16, "r1.json\n".to_string()), // GET /results_manifest.txt
                (201u16, "".to_string()),          // PUT /results/r2.json
                (201u16, "".to_string()),          // PUT /results_manifest.txt
            ];
            let mut captured = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let (m, p, b) = MockServer::read_request(&mut stream);
                captured.push((m, p, b));
                let status_line = if status == 200 {
                    "200 OK"
                } else {
                    "201 Created"
                };
                let resp = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
            captured
        });

        let tmp = tempfile::TempDir::new().unwrap();
        let results_dir = tmp.path().join("results");
        std::fs::create_dir_all(&results_dir).unwrap();
        std::fs::write(results_dir.join("r1.json"), "{\"r\":1}").unwrap();
        std::fs::write(results_dir.join("r2.json"), "{\"r\":2}").unwrap();

        let sync = make_sync(&server_url, tmp.path().to_path_buf());
        let pusher = Syncer::new(sync);
        let pushed = pusher.push_subdir("results", true).unwrap();
        assert_eq!(pushed, 1, "r2 是新上传");

        // delete_local_after_push=true + r1/r2 都在 manifest → 本地两个都该被删
        assert!(!results_dir.join("r1.json").exists());
        assert!(!results_dir.join("r2.json").exists());

        let captured = handle.join().unwrap();
        assert_eq!(captured[0].0, "GET");
        assert_eq!(captured[0].1, "/results_manifest.txt");
        assert_eq!(captured[1].0, "PUT");
        assert_eq!(captured[1].1, "/results/r2.json");
        assert_eq!(captured[1].2, b"{\"r\":2}");
        assert_eq!(captured[2].0, "PUT");
        assert_eq!(captured[2].1, "/results_manifest.txt");
        let body = String::from_utf8(captured[2].2.clone()).unwrap();
        assert!(body.contains("r1.json") && body.contains("r2.json"));
    }

    #[test]
    fn test_push_commands_preserves_local_file() {
        // commands 目录是 daemon 的 inbox —— 上传后必须保留本地，
        // 否则 daemon 的 inotify 永远看不到文件。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_url = format!("http://{}", addr);

        let handle = std::thread::spawn(move || {
            let responses = vec![
                (404u16, "".to_string()), // GET /commands_manifest.txt (不存在)
                (201u16, "".to_string()), // PUT /commands/t1.json
                (201u16, "".to_string()), // PUT /commands_manifest.txt
            ];
            let mut captured = Vec::new();
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                let (m, p, b) = MockServer::read_request(&mut stream);
                captured.push((m, p, b));
                let status_line = if status == 404 {
                    "404 Not Found"
                } else {
                    "201 Created"
                };
                let resp = format!(
                    "HTTP/1.1 {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    status_line,
                    body.len(),
                    body
                );
                stream.write_all(resp.as_bytes()).unwrap();
            }
            captured
        });

        let tmp = tempfile::TempDir::new().unwrap();
        let cmd_dir = tmp.path().join("commands");
        std::fs::create_dir_all(&cmd_dir).unwrap();
        std::fs::write(cmd_dir.join("t1.json"), "{\"cmd\":\"hi\"}").unwrap();

        let sync = make_sync(&server_url, tmp.path().to_path_buf());
        let pusher = Syncer::new(sync);
        let pushed = pusher.push_subdir("commands", false).unwrap();
        assert_eq!(pushed, 1);

        // 关键：delete_local_after_push=false，本地文件必须还在
        assert!(cmd_dir.join("t1.json").exists(), "daemon inbox 不能删本地");

        let captured = handle.join().unwrap();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[1].1, "/commands/t1.json");
    }
}
