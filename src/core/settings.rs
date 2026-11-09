use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SettingsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid duration '{value}': {reason}")]
    Duration { value: String, reason: String },
}

fn parse_duration_str(s: &str) -> Result<Duration, SettingsError> {
    humantime::parse_duration(s).map_err(|e| SettingsError::Duration {
        value: s.into(),
        reason: e.to_string(),
    })
}
fn deser_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    match s {
        Some(v) => parse_duration_str(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}
fn ser_duration<S: serde::Serializer>(v: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(d) => s.serialize_str(&humantime::format_duration(*d).to_string()),
        None => s.serialize_none(),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BifrostSettings {
    pub shared_storage: PathBuf,
    /// Transport type: "shared" (default), "ssh", or "http".
    #[serde(default)]
    pub transport: Transport,
    /// SSH transport config (required when transport == "ssh").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<SshSection>,
    /// HTTP transport config (required when transport == "http"; D12 prefix 必填契约).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<HttpSection>,
    #[serde(default)]
    pub client: ClientSection,
    #[serde(default)]
    pub daemon: DaemonSection,
}

/// Transport selection for the bridge.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Shared-storage bridge (GPFS/NFS): both sides read/write the same dirs.
    #[default]
    Shared,
    /// SSH bridge: client reaches the target machine's dirs over SSH.
    Ssh,
    /// HTTP bridge: client PUT/GET against a dumb HTTP relay (e.g. Nexus Raw),
    /// target-side fetcher pulls tasks into local commands/ (daemon unchanged).
    Http,
}

impl Transport {
    pub fn is_ssh(&self) -> bool {
        matches!(self, Transport::Ssh)
    }
    pub fn is_http(&self) -> bool {
        matches!(self, Transport::Http)
    }
}

fn default_10secs() -> u64 {
    10
}
fn default_30secs() -> u64 {
    30
}
fn default_false() -> bool {
    false
}
fn default_listing_style() -> ListingStyle {
    ListingStyle::RestSearch
}

/// HTTP auth kind for the relay (D5 预留 "none" 给内网匿名 GET 场景).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// HTTP Basic auth (username + password). Only meaningful over https.
    #[default]
    Basic,
    /// No auth header at all (anonymous read-only intranets).
    None,
}

/// HTTP auth credentials block (D6: 明文 + 文件 600 权限, 不进 ENV).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct HttpAuthSection {
    #[serde(default)]
    pub kind: AuthKind,
    /// Required when kind == "basic".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Required when kind == "basic".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
}

/// How the fetcher lists "what tasks exist on the relay" (§4.4).
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ListingStyle {
    /// Nexus REST search API (`/service/rest/v1/search/assets`) + local prefix
    /// filter. Default for Nexus Raw; verifies against /service/rest/v1.
    #[serde(rename = "rest-search")]
    #[default]
    RestSearch,
    /// GET the directory and parse HTML `<a href>` links. Only works on
    /// autoindex-style backends (nginx). **Unsupported on Nexus Raw** (404).
    #[serde(rename = "html")]
    Html,
    /// No listing support: fetcher cannot operate; client-only setups only.
    #[serde(rename = "none")]
    None,
}

/// HTTP bridge configuration (used when `transport == "http"`).
/// See `docs/design/http-bridge.md` §3 / §4.6 for the bilateral contract (D12).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HttpSection {
    /// Base URL of the HTTP relay's upload area. **Must be `https://`**
    /// (Basic auth would otherwise go over the wire in cleartext; rejected
    /// at `validate()` time). All resource paths resolve to
    /// `<base_url>/<prefix>/<subdir>/<filename>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// Repository name (e.g. `tmp-raw`). Required when
    /// `listing_style == "rest-search"` because the search API lives outside
    /// `/repository/<repo>/` and cannot be derived from `base_url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    /// Auth credentials block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth: Option<HttpAuthSection>,
    /// Bilateral contract root (D12). Client and daemon must agree on the
    /// exact value. Recommended form `<owner>/<project>` (e.g.
    /// `team-lx/bifrost-http-bridge`). MUST NOT carry a timestamp — changing
    /// it means "new namespace". Validated as non-empty by `validate()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<String>,
    /// Search query (`q`) for Nexus REST search (D8). **STRONGLY recommend
    /// setting this explicitly in production.** The default derives from the
    /// last path segment of `prefix`, but Nexus search is **tokenized OR**:
    /// `bifrost-http-bridge` searches as `bifrost OR http OR bridge`. A
    /// generic token like `http` will pull in every "http"-related object
    /// in the repo (112 hits in Ax's probe appendix B). Local
    /// `path.starts_with(repo_prefix)` filtering still protects
    /// correctness/security, but the search page count escalates badly
    /// (cost/latency). Production deployments: always set this to the
    /// most distinctive single token in your prefix. Caller-explicit wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listing_query: Option<String>,
    /// TCP/TLS connect timeout (seconds).
    #[serde(default = "default_10secs")]
    pub connect_timeout_secs: u64,
    /// Inactivity read timeout (seconds). NOT a total request deadline —
    /// large blobs need long overall transfers (26 MiB/s single-stream on
    /// the Nexus probe).
    #[serde(default = "default_30secs")]
    pub read_timeout_secs: u64,
    /// Fetcher and client-side poll cadence (seconds). Default raised to 10s
    /// per D11 (sequential mode): 5s polling has zero gain when only one
    /// task is in flight at a time.
    #[serde(default = "default_10secs")]
    pub poll_interval_secs: u64,
    /// Optional two-step upload ("file.tmp then file") for future backends
    /// that don't have Nexus Raw's per-PUT atomicity. Default false (D1).
    #[serde(default = "default_false")]
    pub atomic_upload: bool,
    /// How the fetcher enumerates task objects on the relay.
    #[serde(default = "default_listing_style")]
    pub listing_style: ListingStyle,
}

// 手工 Default: 保证 Rust 代码里 HttpSection::default() 与 JSON 缺省值保持一致,
// 不会出现「序列化 round-trip 后 u64 字段变 0」的隐性漂移.
impl Default for HttpSection {
    fn default() -> Self {
        Self {
            base_url: None,
            repository: None,
            auth: None,
            prefix: None,
            listing_query: None,
            connect_timeout_secs: default_10secs(),
            read_timeout_secs: default_30secs(),
            poll_interval_secs: default_10secs(),
            atomic_upload: default_false(),
            listing_style: default_listing_style(),
        }
    }
}

/// SSH bridge configuration (used when `transport == "ssh"`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SshSection {
    /// Target hostname or IP (required).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// SSH user (default: current user).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Remote directory on the target machine that plays the role of
    /// shared_storage: commands/ results/ status/ artifacts/ live under it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_dir: Option<PathBuf>,
    /// SSH port (default: 22).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// Connect timeout for each ssh invocation (default: 10s).
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub connect_timeout: Option<Duration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ClientSection {
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub poll_interval: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub heartbeat_timeout: Option<Duration>,
    /// Hard ceiling on how long a client waits for one job task when the task
    /// itself inherits the daemon timeout (default 12h). Prevents an
    /// indefinitely blocked client when the daemon is down.
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub wait_timeout: Option<Duration>,
}

/// How the fallback scanner is driven.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum ScanMode {
    /// Probe the clock at startup and only spin when it looks frozen.
    #[default]
    Auto,
    /// Always timer-driven (lowest CPU). Safe on healthy machines.
    Sleep,
    /// Always busy-poll. Only for machines whose monotonic clock freezes.
    Spin,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DaemonSection {
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub poll_interval: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub task_timeout: Option<Duration>,
    /// Ceiling for per-task retries. Unset = no cap (the task's own
    /// `retry_count` decides; it defaults to 0, i.e. run once).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u8>,
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub heartbeat_interval: Option<Duration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
    /// Run commands through `sh -c` so shell syntax (pipes, redirects,
    /// `&&`, `$VAR`, trailing `&`) works. Default: true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_shell: Option<bool>,
    /// Fallback scan cadence for task files that inotify missed. Default: 1s.
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub scan_interval: Option<Duration>,
    /// Age after which an orphaned `.processing` claim marker counts as stale
    /// and is reclaimed, so a crashed daemon cannot wedge a task forever.
    /// Default: 2x task_timeout.
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub stale_marker_timeout: Option<Duration>,
    /// GPU ids handed out one task at a time; the assigned id is injected as
    /// CUDA_VISIBLE_DEVICES (unless the task sets it itself).
    ///
    /// - absent  → auto-detect every GPU on the machine (`nvidia-smi`)
    /// - `[]`    → disabled (CPU-only scheduling)
    /// - `[0,1]` → use exactly these ids
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gpu_pool: Option<Vec<u32>>,
    /// How often a running task polls for a cancel request. Default 1s:
    /// fast enough to stop a runaway benchmark, cheap enough for shared FS.
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration",
        skip_serializing_if = "Option::is_none"
    )]
    pub cancel_poll: Option<Duration>,
    /// Fallback-scan driver: `auto` (default) probes the clock at startup and
    /// only busy-polls when the monotonic clock looks frozen; `sleep` forces
    /// timer-driven scanning; `spin` forces busy-polling.
    #[serde(default)]
    pub scan_mode: ScanMode,
    /// How many rotated files to keep (`.1` .. `.N`). Default 1, max 9.
    /// Rotate size is fixed at 64MB per stream (summaries live in the result,
    /// not in the rotation budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_backups: Option<usize>,
}

impl BifrostSettings {
    pub fn defaults() -> Self {
        Self {
            shared_storage: dirs().join("data"),
            transport: Transport::Shared,
            ssh: None,
            http: None,
            client: ClientSection::default(),
            daemon: DaemonSection::default(),
        }
    }
    pub fn path() -> PathBuf {
        dirs().join("settings.json")
    }
    pub fn validate(&self) -> Result<(), SettingsError> {
        if let Some(r) = self.daemon.max_retries {
            if r > 10 {
                return Err(SettingsError::Duration {
                    value: r.to_string(),
                    reason: "max_retries must be <= 10".into(),
                });
            }
        }
        if let Some(c) = self.daemon.max_concurrent {
            if !(1..=100).contains(&c) {
                return Err(SettingsError::Duration {
                    value: c.to_string(),
                    reason: "max_concurrent must be 1-100".into(),
                });
            }
        }
        if let Some(b) = self.daemon.log_backups {
            if b > 9 {
                return Err(SettingsError::Duration {
                    value: b.to_string(),
                    reason: "log_backups must be <= 9".into(),
                });
            }
        }
        if let Some(gpus) = &self.daemon.gpu_pool {
            if gpus.len() > 64 {
                return Err(SettingsError::Duration {
                    value: gpus.len().to_string(),
                    reason: "gpu_pool supports at most 64 devices".into(),
                });
            }
        }
        if self.transport.is_ssh() {
            let ssh = self.ssh.as_ref().ok_or_else(|| SettingsError::Duration {
                value: "ssh".into(),
                reason: "transport=ssh requires an 'ssh' config section".into(),
            })?;
            if ssh.host.is_none() || ssh.remote_dir.is_none() {
                return Err(SettingsError::Duration {
                    value: "ssh".into(),
                    reason: "ssh config requires 'host' and 'remote_dir'".into(),
                });
            }
        }
        if self.transport.is_http() {
            let http = self.http.as_ref().ok_or_else(|| SettingsError::Duration {
                value: "http".into(),
                reason: "transport=http requires an 'http' config section".into(),
            })?;
            // D12 契约: prefix 必填, 非空字符串. 双方 (client + daemon) 必须
            // 抄同一份 [http] 段配置, prefix 是 discovery / upload / result 的根.
            let prefix = http.prefix.as_deref().unwrap_or("").trim();
            if prefix.is_empty() {
                return Err(SettingsError::Duration {
                    value: "http.prefix".into(),
                    reason: "transport=http requires 'http.prefix' (non-empty; \
                             bilateral contract root, D12)"
                        .into(),
                });
            }
            let base_url = http.base_url.as_deref().unwrap_or("").trim();
            if base_url.is_empty() {
                return Err(SettingsError::Duration {
                    value: "http.base_url".into(),
                    reason: "transport=http requires 'http.base_url'".into(),
                });
            }
            // Basic auth 明文 + https 强制 (loopback 单测例外).
            let is_https = base_url.starts_with("https://");
            let is_loopback_http = base_url.starts_with("http://127.0.0.1")
                || base_url.starts_with("http://localhost");
            if !is_https && !is_loopback_http {
                return Err(SettingsError::Duration {
                    value: "http.base_url".into(),
                    reason: "http.base_url must be https:// (Basic auth cleartext); \
                             loopback http://127.0.0.1 allowed only in tests"
                        .into(),
                });
            }
            // rest-search 必须能推出 repo 名.
            if http.listing_style == ListingStyle::RestSearch {
                let repo = http.repository.as_deref().unwrap_or("").trim();
                if repo.is_empty() {
                    return Err(SettingsError::Duration {
                        value: "http.repository".into(),
                        reason: "listing_style=rest-search requires 'http.repository' (search API \
                             is on /service/rest/v1 and cannot be derived from base_url)"
                            .into(),
                    });
                }
                // D13: base_url 必须与 repository 名字一致——否则 search 查 A 库,
                // PUT 进 B 库, 任务永远拉不到, 症状跟「fetcher 没起」一模一样.
                // 校验仅作为配置期快速失败; loopback 测试 base_url 例外.
                if is_https {
                    let expect_marker = format!("/repository/{}/", repo);
                    if !base_url.contains(&expect_marker) {
                        return Err(SettingsError::Duration {
                            value: "http.base_url".into(),
                            reason: format!(
                                "http.base_url must contain '{}' to match http.repository \
                                 (otherwise search hits one repo, PUT lands in another)",
                                expect_marker.trim_end_matches('/')
                            ),
                        });
                    }
                }
            }
            // basic 必须连 user+password 一起给.
            if let Some(auth) = &http.auth {
                if auth.kind == AuthKind::Basic {
                    let u = auth.user.as_deref().unwrap_or("").trim();
                    let p = auth.password.as_deref().unwrap_or("").trim();
                    if u.is_empty() || p.is_empty() {
                        return Err(SettingsError::Duration {
                            value: "http.auth".into(),
                            reason: "auth.kind=basic requires both 'http.auth.user' and \
                                 'http.auth.password'"
                                .into(),
                        });
                    }
                }
            }
        }
        Ok(())
    }
}

pub fn load() -> BifrostSettings {
    match std::fs::read_to_string(BifrostSettings::path()) {
        Ok(c) => match serde_json::from_str::<BifrostSettings>(&c) {
            Ok(s) => {
                if let Err(e) = s.validate() {
                    eprintln!("Warning: invalid settings ({})", e);
                    return BifrostSettings::defaults();
                }
                s
            }
            Err(e) => {
                eprintln!("Warning: invalid settings.json ({})", e);
                BifrostSettings::defaults()
            }
        },
        Err(_) => BifrostSettings::defaults(),
    }
}

/// Which side of the deployment a settings file is being generated for.
///
/// 两端写的是同一份 settings.json，但"哪一半有意义"不同：client 只用 `client.*`，
/// server 只用 `daemon.*`。分开 init 才能把该侧的默认值写实、把另一侧留空，
/// 而不是给每台机器都塞一份满配配置。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitRole {
    Client,
    Server,
}

/// Outcome of a settings init.
pub struct InitOutcome {
    pub path: PathBuf,
    /// false = the file already existed and was left untouched (no --force).
    pub created: bool,
}

/// Generate a settings file for one side.
///
/// `storage` is optional at the CLI level (it is a global flag), but deploying
/// without it is almost always a mistake: the default is a machine-local
/// directory, so the client and the daemon never see the same data and tasks
/// stay Pending forever. Callers print a warning in that case.
pub fn init_for(
    role: InitRole,
    storage: Option<PathBuf>,
    path: Option<PathBuf>,
    force: bool,
) -> Result<InitOutcome, SettingsError> {
    let p = path.unwrap_or_else(BifrostSettings::path);
    if p.exists() && !force {
        return Ok(InitOutcome {
            path: p,
            created: false,
        });
    }
    if let Some(parent) = p.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let mut s = BifrostSettings::defaults();
    if let Some(dir) = storage {
        s.shared_storage = dir;
    }
    match role {
        InitRole::Client => {
            // 只写 client 侧关心的项：轮询间隔、心跳超时、等待上限。
            s.client.poll_interval = Some(Duration::from_secs(2));
            s.client.heartbeat_timeout = Some(Duration::from_secs(180));
            s.client.wait_timeout = Some(Duration::from_secs(12 * 3600));
        }
        InitRole::Server => {
            // 写实 daemon 侧默认值；gpu_pool 保持 None = 自动探测本机全部卡。
            s.daemon.task_timeout = Some(Duration::from_secs(5400));
            s.daemon.max_concurrent = Some(10);
            s.daemon.use_shell = Some(true);
            s.daemon.scan_interval = Some(Duration::from_secs(1));
            s.daemon.gpu_pool = None;
        }
    }
    s.validate()?;

    let j = serde_json::to_string_pretty(&s)?;
    let tmp = p.with_extension("json.tmp");
    std::fs::write(&tmp, &j)?;
    std::fs::rename(&tmp, &p)?;
    Ok(InitOutcome {
        path: p,
        created: true,
    })
}

/// Legacy entry point (`bifrost server --init`): always overwrite with defaults.
pub fn init() -> Result<PathBuf, SettingsError> {
    Ok(init_for(InitRole::Server, None, None, true)?.path)
}

fn dirs() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".bifrost")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_init_for_client_keeps_daemon_section_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("client.json");
        let out = init_for(
            InitRole::Client,
            Some(PathBuf::from("/shared/bifrost")),
            Some(path.clone()),
            false,
        )
        .unwrap();
        assert!(out.created);
        let s: BifrostSettings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(s.shared_storage, PathBuf::from("/shared/bifrost"));
        assert_eq!(s.client.poll_interval, Some(Duration::from_secs(2)));
        assert_eq!(s.client.wait_timeout, Some(Duration::from_secs(12 * 3600)));
        // 没写过的字段不落盘，读回来是 None（= 用默认）
        assert_eq!(s.daemon.max_concurrent, None);
        assert_eq!(s.daemon.task_timeout, None);
    }

    #[test]
    fn test_init_for_server_fills_daemon_and_leaves_client_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("server.json");
        init_for(
            InitRole::Server,
            Some(PathBuf::from("/data/liuxin/bifrost")),
            Some(path.clone()),
            false,
        )
        .unwrap();
        let s: BifrostSettings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(s.daemon.max_concurrent, Some(10));
        assert_eq!(s.daemon.task_timeout, Some(Duration::from_secs(5400)));
        assert_eq!(s.daemon.use_shell, Some(true));
        // 不配置 gpu_pool = 自动探测全部卡
        assert_eq!(s.daemon.gpu_pool, None);
        assert_eq!(s.client.poll_interval, None);
    }

    #[test]
    fn test_init_does_not_clobber_without_force() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("s.json");
        init_for(
            InitRole::Server,
            Some(PathBuf::from("/a")),
            Some(path.clone()),
            false,
        )
        .unwrap();
        let again = init_for(
            InitRole::Server,
            Some(PathBuf::from("/b")),
            Some(path.clone()),
            false,
        )
        .unwrap();
        assert!(!again.created, "第二次不该覆盖");
        let s: BifrostSettings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(s.shared_storage, PathBuf::from("/a"));

        // --force 才覆盖
        let forced = init_for(
            InitRole::Server,
            Some(PathBuf::from("/b")),
            Some(path.clone()),
            true,
        )
        .unwrap();
        assert!(forced.created);
        let s: BifrostSettings =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(s.shared_storage, PathBuf::from("/b"));
    }

    #[test]
    fn test_init_creates_parent_dirs() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("a/b/c/settings.json");
        init_for(InitRole::Client, None, Some(path.clone()), false).unwrap();
        assert!(path.exists());
    }
    #[test]
    fn test_defaults() {
        assert_eq!(
            BifrostSettings::defaults().shared_storage,
            dirs().join("data")
        );
    }

    #[test]
    fn test_parse_duration() {
        let s: BifrostSettings = serde_json::from_str(
            r#"{"shared_storage":"/t","client":{"poll_interval":"2s"},"daemon":{}}"#,
        )
        .unwrap();
        assert_eq!(s.client.poll_interval, Some(Duration::from_secs(2)));
    }
    #[test]
    fn test_invalid_duration() {
        assert!(serde_json::from_str::<BifrostSettings>(
            r#"{"shared_storage":"/t","client":{"poll_interval":"blargh"},"daemon":{}}"#
        )
        .is_err());
    }

    #[test]
    fn test_scan_mode_default_and_parse() {
        let s: BifrostSettings =
            serde_json::from_str(r#"{"shared_storage":"/t","daemon":{}}"#).unwrap();
        assert_eq!(
            s.daemon.scan_mode,
            ScanMode::Auto,
            "默认应为 auto（先探后兜底）"
        );

        for (raw, want) in [
            ("\"auto\"", ScanMode::Auto),
            ("\"sleep\"", ScanMode::Sleep),
            ("\"spin\"", ScanMode::Spin),
        ] {
            let j = format!(
                r#"{{"shared_storage":"/t","daemon":{{"scan_mode":{}}}}}"#,
                raw
            );
            let s: BifrostSettings = serde_json::from_str(&j).unwrap();
            assert_eq!(s.daemon.scan_mode, want, "解析 {}", raw);
        }

        // 未知值必须报错，不能静默回退
        assert!(serde_json::from_str::<BifrostSettings>(
            r#"{"shared_storage":"/t","daemon":{"scan_mode":"turbo"}}"#
        )
        .is_err());
    }

    #[test]
    fn test_transport_defaults_to_shared() {
        let s: BifrostSettings = serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap();
        assert_eq!(s.transport, Transport::Shared);
        assert!(s.ssh.is_none());
    }

    #[test]
    fn test_transport_ssh_parse() {
        let s: BifrostSettings = serde_json::from_str(
            r#"{"shared_storage":"/t","transport":"ssh","ssh":{"host":"h1","remote_dir":"/r"}}"#,
        )
        .unwrap();
        assert_eq!(s.transport, Transport::Ssh);
        assert!(s.transport.is_ssh());
        let ssh = s.ssh.unwrap();
        assert_eq!(ssh.host.as_deref(), Some("h1"));
        assert_eq!(ssh.remote_dir.unwrap(), std::path::PathBuf::from("/r"));
    }

    #[test]
    fn test_transport_ssh_unknown_falls_back_shared() {
        // Unknown transport value: serde errors (no silent fallback).
        assert!(serde_json::from_str::<BifrostSettings>(
            r#"{"shared_storage":"/t","transport":"carrier-pigeon"}"#
        )
        .is_err());
    }

    #[test]
    fn test_validate_ssh_requires_section_and_fields() {
        let no_section = BifrostSettings {
            transport: Transport::Ssh,
            ssh: None,
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        assert!(no_section.validate().is_err());

        let no_host = BifrostSettings {
            transport: Transport::Ssh,
            ssh: Some(SshSection {
                host: None,
                remote_dir: Some(std::path::PathBuf::from("/r")),
                ..SshSection::default()
            }),
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        assert!(no_host.validate().is_err());

        let ok = BifrostSettings {
            transport: Transport::Ssh,
            ssh: Some(SshSection {
                host: Some("h1".into()),
                remote_dir: Some(std::path::PathBuf::from("/r")),
                ..SshSection::default()
            }),
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        assert!(ok.validate().is_ok());
    }

    // ── HTTP bridge settings validation (Day 1; D12 prefix 必填, D6 basic auth) ──

    fn http_section_valid() -> HttpSection {
        HttpSection {
            base_url: Some("https://nexus.example.com/repository/tmp-raw/uploads".into()),
            repository: Some("tmp-raw".into()),
            auth: Some(HttpAuthSection {
                kind: AuthKind::Basic,
                user: Some("u".into()),
                password: Some("p".into()),
            }),
            prefix: Some("team-lx/bifrost-http-bridge".into()),
            ..HttpSection::default()
        }
    }

    fn http_settings(http: HttpSection) -> BifrostSettings {
        BifrostSettings {
            transport: Transport::Http,
            http: Some(http),
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        }
    }

    #[test]
    fn http_valid_settings_passes() {
        assert!(http_settings(http_section_valid()).validate().is_ok());
    }

    #[test]
    fn http_settings_round_trip_json() {
        let s = http_settings(http_section_valid());
        let json = serde_json::to_string(&s).unwrap();
        let back: BifrostSettings = serde_json::from_str(&json).unwrap();
        let http = back.http.as_ref().expect("http section present");
        assert_eq!(http.prefix.as_deref(), Some("team-lx/bifrost-http-bridge"));
        assert_eq!(http.repository.as_deref(), Some("tmp-raw"));
        assert_eq!(http.poll_interval_secs, 10, "D11 顺序模式默认 10s");
        assert_eq!(http.connect_timeout_secs, 10);
        assert_eq!(http.read_timeout_secs, 30);
        assert!(!http.atomic_upload, "D1 默认单次 PUT");
        assert_eq!(
            http.listing_style,
            ListingStyle::RestSearch,
            "D8 默认 rest-search"
        );
    }

    #[test]
    fn http_requires_section_when_transport_http() {
        let s = BifrostSettings {
            transport: Transport::Http,
            http: None,
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        let err = s.validate().unwrap_err().to_string();
        assert!(
            err.contains("http"),
            "error mentions missing http section: {}",
            err
        );
    }

    #[test]
    fn http_prefix_required_for_d12_contract() {
        let mut h = http_section_valid();
        h.prefix = None;
        let err = http_settings(h).validate().unwrap_err().to_string();
        assert!(err.contains("http.prefix"), "prefix 必填: {}", err);

        let mut h = http_section_valid();
        h.prefix = Some("   ".into()); // whitespace-only is also empty
        let err = http_settings(h).validate().unwrap_err().to_string();
        assert!(err.contains("http.prefix"), "prefix 不可为纯空白: {}", err);
    }

    #[test]
    fn http_base_url_must_be_https_unless_loopback() {
        // 非 https + 非 loopback -> 拒绝
        let mut h = http_section_valid();
        h.base_url = Some("http://nexus.example.com/repository/tmp-raw/uploads".into());
        assert!(http_settings(h).validate().is_err());

        // loopback http -> 通过 (单测场景 §8)
        let mut h = http_section_valid();
        h.base_url = Some("http://127.0.0.1:8081".into());
        assert!(http_settings(h).validate().is_ok());

        let mut h = http_section_valid();
        h.base_url = Some("http://localhost:8081".into());
        assert!(http_settings(h).validate().is_ok());
    }

    #[test]
    fn http_base_url_missing_rejected() {
        let mut h = http_section_valid();
        h.base_url = None;
        let err = http_settings(h).validate().unwrap_err().to_string();
        assert!(err.contains("http.base_url"), "base_url 必填: {}", err);
    }

    #[test]
    fn http_rest_search_requires_repository() {
        let mut h = http_section_valid();
        h.repository = None;
        let err = http_settings(h).validate().unwrap_err().to_string();
        assert!(
            err.contains("http.repository"),
            "rest-search 必须给 repo: {}",
            err
        );
    }

    #[test]
    fn http_html_listing_does_not_need_repository() {
        // 后端选 html 时, repository 字段不再强制 (非 Nexus)
        let mut h = http_section_valid();
        h.repository = None;
        h.listing_style = ListingStyle::Html;
        assert!(http_settings(h).validate().is_ok());
    }

    #[test]
    fn http_basic_auth_requires_user_and_password() {
        let mut h = http_section_valid();
        if let Some(a) = h.auth.as_mut() {
            a.password = None;
        }
        let err = http_settings(h).validate().unwrap_err().to_string();
        assert!(
            err.contains("http.auth"),
            "basic auth 需要 user+password: {}",
            err
        );

        let mut h = http_section_valid();
        if let Some(a) = h.auth.as_mut() {
            a.user = None;
        }
        assert!(http_settings(h).validate().is_err());
    }

    #[test]
    fn http_auth_kind_none_does_not_need_credentials() {
        // D5: kind=none 时不要求 user/password
        let mut h = http_section_valid();
        h.auth = Some(HttpAuthSection {
            kind: AuthKind::None,
            user: None,
            password: None,
        });
        assert!(http_settings(h).validate().is_ok());
    }

    #[test]
    fn http_no_auth_section_ok() {
        // auth 段整体省略 (匿名 GET 内网) -> ok
        let mut h = http_section_valid();
        h.auth = None;
        assert!(http_settings(h).validate().is_ok());
    }

    #[test]
    fn http_base_url_must_match_repository_when_rest_search() {
        // D13: base_url 不含 /repository/<repository>/ -> 配置期拒绝
        let mut h = http_section_valid();
        h.repository = Some("wrong-repo".into());
        let err = http_settings(h).validate().unwrap_err().to_string();
        assert!(
            err.contains("/repository/wrong-repo"),
            "D13 校验: base_url 必须含 repository 路径段: {}",
            err
        );

        // OK 情况: base_url 匹配
        let mut h = http_section_valid();
        h.repository = Some("tmp-raw".into()); // base_url 里就是 /repository/tmp-raw/
        assert!(http_settings(h).validate().is_ok());

        // loopback http 豁免 (单测场景没有真 repo 路径)
        let mut h = http_section_valid();
        h.base_url = Some("http://127.0.0.1:8081".into());
        h.repository = Some("whatever".into());
        assert!(http_settings(h).validate().is_ok());
    }

    #[test]
    fn http_listing_query_defaults_to_prefix_last_segment() {
        // D8 二轮: listing_query 默认 = prefix 最后一段
        let h = http_section_valid();
        let q = h
            .listing_query
            .as_deref()
            .map(str::to_string)
            .unwrap_or_else(|| {
                h.prefix
                    .as_deref()
                    .unwrap_or("")
                    .rsplit('/')
                    .next()
                    .unwrap_or("")
                    .to_string()
            });
        assert_eq!(q, "bifrost-http-bridge");
    }
}
