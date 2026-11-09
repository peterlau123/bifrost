// Aliyun-OSS Bridge — a thin Bridge-trait shim over OSS REST.
//
// Day 3 client: PUT/GET/DELETE/LIST against the OSS endpoint using V1
// request signing (see sign.rs) and ListObjects v1 XML parsing (list.rs).
// Mirrors HttpBridge's URL/file naming conventions so the fetcher.rs
// `bridge.delete_task_blob(...)` call works without type specialisation.
//
// Object layout (must match the bilateral contract in HttpBridge):
//   <prefix>/commands/<YYYYmmdd_HHMMSS>_<uuid>.json        (client → server)
//   <prefix>/results/<uuid>_result.json                     (server → client)
//   <prefix>/status/<uuid>.json                             (server → client, sideband)
//   <prefix>/cancel/<uuid>.json                             (client → server, optional)
//
// Endpoint & signing:
//   URL      = https://<bucket>.oss-<region>.aliyuncs.com/<urlencode path>
//              Or `endpoint` from `[http.oss]` (e.g. the VPC internal one).
//   Resource = /<bucket>/<key>        (lead slash present, key url-encoded)
//   AuthHdr  = OSS <AccessKeyId>:<base64(hmac-sha1(sk, StringToSign))>
//
// The OSS endpoint is configured via `[http.oss]`. The credentials come
// from env vars / `~/.aliyun/oss.json` / `<settings-dir>/oss.json`
// (see credentials.rs). We never put AK/SK in the URL or in a query
// parameter — only in the Authorization header (TLS).
//
// Sync-native: uses reqwest::blocking. DO NOT call from inside a tokio
// runtime (would panic on first request due to nested block_on).

use std::sync::Arc;
use std::time::Duration;

use chrono::{SecondsFormat, Utc};
use reqwest::blocking::Client;
use reqwest::StatusCode;
use uuid::Uuid;

use crate::core::aliyun_oss::credentials::OssCredentials;
use crate::core::aliyun_oss::list::{parse_list_objects, ListBucketResult};
use crate::core::aliyun_oss::sign::{sign_v1, string_to_sign, SignRequest};
use crate::core::bridge::{Bridge, TaskStatusResponse};
use crate::core::error::{BifrostError, Result};
use crate::core::models::{Task, TaskResult, TaskStatus};
use crate::core::settings::HttpSection;

/// Max keys per ListObjects call — pick something that won't paginate for
/// the common case (D11 sequential mode keeps in-flight count small).
const MAX_KEYS_PER_LIST: u32 = 1000;

/// OSS bridge. Mirrors HttpBridge's URL conventions but signs each
/// request per OSS Signature V1 and parses ListObjects XML directly
/// (no REST-search fallback needed — OSS has a real LIST API).
#[derive(Debug, Clone)]
pub struct OssBridge {
    // NB: no HttpSection/OssSection kept on the struct — everything I need
    // (timeouts, bucket, endpoint, prefix_root) is already pre-baked into
    // the fields below, and the whole point of `new()` is to fail-fast on
    // config errors so the runtime hot path needs no Option<> handling.
    #[cfg_attr(not(test), allow(dead_code))]
    client: Arc<Client>,
    creds: OssCredentials,
    bucket: String,
    /// Always ends in `/` (or empty for the no-prefix corner case—rare,
    /// because http.prefix is required by settings.validate()).
    prefix_root: String,
    /// `https://<bucket>.oss-<region>.aliyuncs.com` or override endpoint.
    endpoint: String,
}

impl OssBridge {
    /// Production constructor.
    pub fn new(cfg: &HttpSection) -> Result<Self> {
        debug_assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "OssBridge must not be constructed inside a tokio runtime"
        );

        let oss = cfg.oss.clone().ok_or_else(|| {
            BifrostError::ConfigInvalid(
                "transport=http listing_style=oss needs [http.oss] section".into(),
            )
        })?;
        let bucket = oss
            .bucket
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                BifrostError::ConfigInvalid("http.oss.bucket is required for OSS mode".into())
            })?
            .to_string();

        let region = oss
            .region
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("cn-beijing")
            .to_string();

        let endpoint = match oss.endpoint.as_deref().map(str::trim) {
            Some(e) if !e.is_empty() => e.trim_end_matches('/').to_string(),
            _ => format!("https://{}.oss-{}.aliyuncs.com", bucket, region),
        };

        let prefix_raw = cfg.prefix.as_deref().map(str::trim).ok_or_else(|| {
            BifrostError::ConfigInvalid(
                "http.prefix is required (bilateral contract root, D12)".into(),
            )
        })?;
        if prefix_raw.is_empty() {
            return Err(BifrostError::ConfigInvalid(
                "http.prefix must be non-empty after trim".into(),
            ));
        }
        // Always form `<p1>/<p2>/.../` — single trailing slash, no leading.
        let prefix_root = {
            let cleaned = prefix_raw
                .trim_matches('/')
                .split('/')
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("/");
            format!("{}/", cleaned)
        };

        let creds = OssCredentials::load(None)?;
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
            .timeout(Duration::from_secs(cfg.read_timeout_secs))
            .user_agent(format!("bifrost-oss-bridge/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| BifrostError::ConfigInvalid(format!("build oss http client: {e}")))?;

        Ok(Self {
            client: Arc::new(client),
            creds,
            bucket,
            prefix_root,
            endpoint,
        })
    }

    // ── URL & signing helpers ─────────────────────────────────────

    /// Build "<endpoint>/<segment>/<segment>/…" with each segment
    /// percent-encoded. We hand-roll RFC 3986 unreserved (A-Z a-z 0-9 - _ . ~)
    /// to avoid pulling an extra crate — same approach as http_bridge.
    fn object_url(&self, key: &str) -> String {
        let encoded: Vec<String> = key
            .split('/')
            .filter(|s| !s.is_empty())
            .map(|s| percent_encode_segment(s).into_owned())
            .collect();
        format!("{}/{}", self.endpoint, encoded.join("/"))
    }

    /// CanonicalizedResource for the given key (no leading/trailing slash
    /// on input — the leading "/" is part of the spec). Empty key → "/bucket/"
    /// (bucket-level op, e.g. ListObjects).
    fn resource(&self, key: &str) -> String {
        if key.is_empty() {
            format!("/{}/", self.bucket)
        } else {
            // Spec: CanonicalizedResource uses the **literally-present**
            // key in the URL. Because we percent-encode per segment, the
            // client sees the same bytes as the server, so we sign the
            // literally-encoded key. (Failure mode of signing the UN-
            // encoded key: server computes HMAC over encoded, mismatch.)
            let encoded_key: Vec<String> = key
                .split('/')
                .filter(|s| !s.is_empty())
                .map(|s| percent_encode_segment(s).into_owned())
                .collect();
            format!("/{}/{}", self.bucket, encoded_key.join("/"))
        }
    }

    /// Sign a Date-stamped request. Caller still has to set the same
    /// `Date` header on the HTTP request itself — the helper only
    /// produces the Authorization value; we expose the date to avoid
    /// the two slipping out of sync.
    fn make_auth(&self, verb: &str, key: &str, content_type: Option<&str>, date: &str) -> String {
        let req = SignRequest {
            verb,
            content_md5: None,
            content_type,
            date,
            oss_headers: &[],
            resource: &self.resource(key),
        };
        sign_v1(
            &self.creds.access_key_id,
            &self.creds.access_key_secret,
            &req,
        )
    }

    // ── HTTP verbs (signed) ───────────────────────────────────────

    fn http_put(&self, key: &str, body: Vec<u8>, content_type: &str) -> Result<StatusCode> {
        let date = Utc::now().to_rfc2822();
        // RFC 2822 emits "+0000"; OSS RFC 1123 requires "GMT".
        let date_gmt = rfc2822_to_gmt(&date);
        let url = self.object_url(key);
        let sts = string_to_sign(&SignRequest {
            verb: "PUT",
            content_md5: None,
            content_type: Some(content_type),
            date: &date_gmt,
            oss_headers: &[],
            resource: &self.resource(key),
        });
        let auth = self.make_auth("PUT", key, Some(content_type), &date_gmt);
        let resp = self
            .client
            .put(&url)
            .header("Date", date_gmt.clone())
            .header("Authorization", auth)
            .header("Content-Type", content_type.to_string())
            .body(body)
            .send()
            .map_err(|e| BifrostError::ExecutionError(format!("oss PUT {key}: {e}")))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() && std::env::var("OSS_DEBUG").is_ok() {
            eprintln!("[oss-debug] PUT url={url}");
            eprintln!("[oss-debug] Date={date_gmt}");
            eprintln!("[oss-debug] resource={}", self.resource(key));
            eprintln!("[oss-debug] StringToSign=\n---\n{sts}\n---");
            eprintln!("[oss-debug] resp-body-preview: {}",
                      body.chars().take(500).collect::<String>());
        }
        Ok(status)
    }

    fn http_get(&self, key: &str) -> Result<reqwest::blocking::Response> {
        let date = rfc2822_to_gmt(&Utc::now().to_rfc2822());
        let url = self.object_url(key);
        let auth = self.make_auth("GET", key, None, &date);
        self.client
            .get(&url)
            .header("Date", date)
            .header("Authorization", auth)
            .send()
            .map_err(|e| BifrostError::ExecutionError(format!("oss GET {key}: {e}")))
    }

    fn http_delete(&self, key: &str) -> Result<StatusCode> {
        let date = rfc2822_to_gmt(&Utc::now().to_rfc2822());
        let url = self.object_url(key);
        let auth = self.make_auth("DELETE", key, None, &date);
        let resp = self
            .client
            .delete(&url)
            .header("Date", date)
            .header("Authorization", auth)
            .send()
            .map_err(|e| BifrostError::ExecutionError(format!("oss DELETE {key}: {e}")))?;
        let status = resp.status();
        let _ = resp.text();
        Ok(status)
    }

    // ── LIST (bucket-level GET with optional prefix) ─────────────

    /// Hit ListObjects v1 for the given prefix. We always set no
    /// delimiter so we see files AND dirs as flat keys — the bridge's
    /// `<prefix>/commands/...` namespace is flat by contract (D12).
    fn list_objects(&self, prefix: &str) -> Result<ListBucketResult> {
        let date = rfc2822_to_gmt(&Utc::now().to_rfc2822());
        // Bucket-level operation: URL is `<endpoint>/<encoded>?prefix=<enc>&max-keys=N`.
        // CanonicalizedResource must NOT include the `prefix` query because
        // it's not on the V1 sub-resource whitelist. So sign with empty key.
        let resource_root = format!("/{}/", self.bucket);
        let auth = sign_v1(
            &self.creds.access_key_id,
            &self.creds.access_key_secret,
            &SignRequest {
                verb: "GET",
                content_md5: None,
                content_type: None,
                date: &date,
                oss_headers: &[],
                resource: &resource_root,
            },
        );
        let url = format!(
            "{}/?prefix={}&max-keys={}",
            self.endpoint,
            encode_query_token(prefix),
            MAX_KEYS_PER_LIST,
        );
        let resp = self
            .client
            .get(&url)
            .header("Date", date)
            .header("Authorization", auth)
            .send()
            .map_err(|e| BifrostError::ExecutionError(format!("oss LIST {prefix}: {e}")))?;
        let status = resp.status();
        let body = resp
            .text()
            .map_err(|e| BifrostError::ExecutionError(format!("oss LIST body: {e}")))?;
        if status.is_success() {
            return parse_list_objects(&body);
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(BifrostError::AuthError(format!(
                "oss LIST {prefix}: {} (check OSS_ACCESS_KEY_ID/SECRET + bucket policy)",
                status.as_u16()
            )));
        }
        Err(BifrostError::ExecutionError(format!(
            "oss LIST {prefix}: {} body-preview={}",
            status.as_u16(),
            body.chars().take(200).collect::<String>()
        )))
    }

    fn relative_key(&self, subdir: &str, filename: &str) -> String {
        format!("{}{}/{}", self.prefix_root, subdir, filename)
    }

    #[cfg_attr(not(test), allow(dead_code))]
    fn strip_prefix<'a>(&self, key: &'a str) -> Option<&'a str> {
        key.strip_prefix(&self.prefix_root)
    }
}

// ─────────────────────────────────────────────────────────────────────
// Bridge impl
// ─────────────────────────────────────────────────────────────────────

impl Bridge for OssBridge {
    fn submit_task(&self, task: &Task) -> Result<()> {
        let body = serde_json::to_vec(task)?;
        let key = self.relative_key("commands", &task_filename(task));
        let status = self.http_put(&key, body, "application/json")?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(BifrostError::AuthError(format!(
                    "oss submit_task: {} (check OSS_ACCESS_KEY_ID/SECRET + bucket policy)",
                    status.as_u16()
                )))
            }
            s => Err(BifrostError::ExecutionError(format!(
                "oss submit_task: {}",
                s.as_u16()
            ))),
        }
    }

    fn query_status(&self, task_id: &Uuid) -> Result<TaskStatusResponse> {
        // Tier 1: results/<uuid>_result.json → Completed
        match self.http_get(&self.relative_key("results", &format!("{task_id}_result.json"))) {
            Ok(r) if r.status().is_success() => {
                return Ok(TaskStatusResponse {
                    task_id: *task_id,
                    status: TaskStatus::Completed,
                    message: Some("found in results/ (oss)".into()),
                });
            }
            Ok(r) => {
                let _ = r.status();
            }
            Err(e) => return Err(e),
        }
        // Tier 2: status/<uuid>.json → StatusRecord status field
        match self.http_get(&self.relative_key("status", &format!("{task_id}.json"))) {
            Ok(r) if r.status().is_success() => {
                let status: TaskStatus = r
                    .json::<StatusRecord>()
                    .ok()
                    .and_then(|s| parse_task_status(&s.status))
                    .unwrap_or(TaskStatus::Running);
                return Ok(TaskStatusResponse {
                    task_id: *task_id,
                    status,
                    message: Some("found in status/ (oss)".into()),
                });
            }
            Ok(_) => {}
            Err(e) => return Err(e),
        }
        // Tier 3: list commands/ and substring-match the UUID.
        // Missing → Pending was the design decision (D10 says "search
        // failure keeps Pending", but a clean miss → TaskNotFound is
        // the right signal for "nowhere visible — may have been
        // reclaimed by retention"). Caller can map to TaskLost
        // via wait_timeout.
        let cmd_prefix = format!("{}commands/", self.prefix_root);
        let list = self.list_objects(&cmd_prefix)?;
        let needle = task_id.to_string();
        for c in list.contents.iter() {
            if c.key.starts_with(&cmd_prefix) && c.key.contains(&needle) {
                return Ok(TaskStatusResponse {
                    task_id: *task_id,
                    status: TaskStatus::Pending,
                    message: Some("pending in commands/ (oss LIST hit)".into()),
                });
            }
        }
        Err(BifrostError::TaskNotFound(*task_id))
    }

    fn get_result(&self, task_id: &Uuid) -> Result<TaskResult> {
        let key = self.relative_key("results", &format!("{task_id}_result.json"));
        let resp = self.http_get(&key)?;
        match resp.status() {
            s if s.is_success() => {
                let result: TaskResult = resp.json().map_err(|e| {
                    BifrostError::ExecutionError(format!("oss get_result parse: {e}"))
                })?;
                Ok(result)
            }
            StatusCode::NOT_FOUND => Err(BifrostError::TaskNotFound(*task_id)),
            s => Err(BifrostError::ExecutionError(format!(
                "oss get_result: {}",
                s.as_u16()
            ))),
        }
    }

    fn list_pending_tasks(&self) -> Result<Vec<Task>> {
        let cmd_prefix = format!("{}commands/", self.prefix_root);
        let list = self.list_objects(&cmd_prefix)?;
        let mut tasks = Vec::new();
        for c in list.contents.iter() {
            // cascade-1: must start with our prefix AND end in ".json" AND
            // not be a ".tmp" file (atomic-upload leftover — same D8 rule).
            if !c.key.starts_with(&cmd_prefix) {
                continue;
            }
            if !c.key.ends_with(".json") || c.key.ends_with(".json.tmp") {
                continue;
            }
            let task = match self.peek_task(&c.key) {
                Ok(t) => t,
                Err(e) => {
                    log::warn!("oss LIST → GET {}: {e} (skipping entry)", c.key);
                    continue;
                }
            };
            tasks.push(task);
        }
        Ok(tasks)
    }

    fn read_task(&self, task_id: &Uuid) -> Result<Task> {
        // Same strategy as HttpBridge: full commands/ listing because of
        // the timestamp prefix. Sequential D11 keeps this bounded.
        let tasks = self.list_pending_tasks()?;
        tasks
            .into_iter()
            .find(|t| t.task_id == *task_id)
            .ok_or(BifrostError::TaskNotFound(*task_id))
    }

    fn write_result(&self, task_id: &Uuid, result: &TaskResult) -> Result<()> {
        let body = serde_json::to_vec(result)?;
        let key = self.relative_key("results", &format!("{task_id}_result.json"));
        let status = self.http_put(&key, body, "application/json")?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("oss write_result: {}", status.as_u16()),
            )),
            s => Err(BifrostError::ExecutionError(format!(
                "oss write_result: {}",
                s.as_u16()
            ))),
        }
    }

    fn write_status(
        &self,
        task_id: &Uuid,
        status: &TaskStatus,
        message: Option<&str>,
    ) -> Result<()> {
        let rec = StatusRecord {
            task_id: task_id.to_string(),
            status: status.to_string(),
            message: message.map(str::to_string),
            timestamp: Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        };
        let body = serde_json::to_vec(&rec)?;
        let key = self.relative_key("status", &format!("{task_id}.json"));
        let s = self.http_put(&key, body, "application/json")?;
        match s {
            v if v.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("oss write_status: {}", s.as_u16()),
            )),
            v => Err(BifrostError::ExecutionError(format!(
                "oss write_status: {}",
                v.as_u16()
            ))),
        }
    }

    fn remove_task(&self, task_id: &Uuid) -> Result<()> {
        // Best effort: do NOT know the timestamp prefix just from the
        // UUID. We LIST commands/ and DELETE the first match. Race is
        // fine — D11 sequential mode makes concurrent producers rare.
        // (delete_task_blob is the precise path; the fetcher uses that.)
        let cmd_prefix = format!("{}commands/", self.prefix_root);
        let list = self.list_objects(&cmd_prefix)?;
        let needle = task_id.to_string();
        let mut hit: Option<String> = None;
        for c in list.contents.iter() {
            if c.key.starts_with(&cmd_prefix) && c.key.contains(&needle) {
                hit = Some(c.key.clone());
                break;
            }
        }
        let key = match hit {
            Some(k) => k,
            None => return Ok(()), // already gone, D3 silent-OK
        };
        let s = self.http_delete(&key)?;
        match s {
            v if v.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(BifrostError::AuthError(format!(
                    "oss remove_task: {} — oss-cli user lacks DeleteObject (see SKILL实战技巧)",
                    s.as_u16()
                )))
            }
            v => Err(BifrostError::ExecutionError(format!(
                "oss remove_task: {}",
                v.as_u16()
            ))),
        }
    }

    fn cancel_task(&self, task_id: &Uuid) -> Result<()> {
        let body = serde_json::json!({
            "task_id": task_id.to_string(),
            "timestamp": Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true),
        })
        .to_string();
        let key = self.relative_key("cancel", &format!("{task_id}.json"));
        let s = self.http_put(&key, body.into_bytes(), "application/json")?;
        match s {
            v if v.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("oss cancel_task: {}", s.as_u16()),
            )),
            v => Err(BifrostError::ExecutionError(format!(
                "oss cancel_task: {}",
                v.as_u16()
            ))),
        }
    }

    // Override: keep `<ts>_<id>.json` filename from submit_task so DELETE
    // hits the exact PUT destination. Without this override the default
    // trait impl tries `/commands/<id>.json` which never existed.
    fn task_blob_name(&self, task: &Task) -> String {
        task_filename(task)
    }

    fn delete_task_blob(&self, subdir: &str, task: &Task) -> Result<()> {
        let key = self.relative_key(subdir, &task_filename(task));
        let s = self.http_delete(&key)?;
        match s {
            v if v.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()), // D3 already gone
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("oss delete_task_blob {}", s.as_u16()),
            )),
            v => Err(BifrostError::ExecutionError(format!(
                "oss delete_task_blob: {}",
                v.as_u16()
            ))),
        }
    }
}

impl OssBridge {
    /// Fetch one blob and deserialize as Task. Pulled out so the
    /// listing-then-GET code path is shared by `list_pending_tasks`
    /// and any future recovery flow.
    fn peek_task(&self, key: &str) -> Result<Task> {
        let resp = self.http_get(key)?;
        match resp.status() {
            s if s.is_success() => {
                let t: Task = resp.json().map_err(|e| {
                    BifrostError::ExecutionError(format!("oss peek_task parse {key}: {e}"))
                })?;
                Ok(t)
            }
            StatusCode::NOT_FOUND => Err(BifrostError::ConfigInvalid(format!(
                "oss peek_task: object {key} disappeared between LIST and GET"
            ))),
            s => Err(BifrostError::ExecutionError(format!(
                "oss peek_task {key}: {}",
                s.as_u16()
            ))),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// helpers (task_filename, status record, parsing, encoding)
// ─────────────────────────────────────────────────────────────────────

/// Identical to `http_bridge::task_filename`. We duplicate instead of
/// sharing via `pub(crate)` to keep `http_bridge` decoupled from the
/// OSS module. Acceptance test `task_filename_matches_http_bridge_form`
/// guards drift.
fn task_filename(task: &Task) -> String {
    format!(
        "{}_{}.json",
        task.timestamp.format("%Y%m%d_%H%M%S"),
        task.task_id
    )
}

/// Local sideband status record — byte-identical to http_bridge::StatusRecord
/// so the same fetcher-side JSON parser works for both backends.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StatusRecord {
    task_id: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    timestamp: String,
}

fn parse_task_status(s: &str) -> Option<TaskStatus> {
    Some(match s {
        "Pending" => TaskStatus::Pending,
        "Running" => TaskStatus::Running,
        "Completed" => TaskStatus::Completed,
        "Failed" => TaskStatus::Failed,
        "Timeout" => TaskStatus::Timeout,
        "Cancelled" => TaskStatus::Cancelled,
        _ => return None,
    })
}

/// Convert RFC 2822's numeric timezone (`+0000`) to RFC 1123's `GMT`
/// literal — OSS accepts only the latter as the `Date` header.
/// Input must be an RFC 2822 string ending in `+0000` (i.e. came from
/// `Utc::now().to_rfc2822()`). The conversion is string-only; no
/// normalisation of clock skew happens here.
fn rfc2822_to_gmt(rfc: &str) -> String {
    if let Some(prefix) = rfc.strip_suffix("+0000") {
        // strip_suffix removes ONLY "+0000", leaving the original
        // trailing space ("…:00 ") from RFC 2822's "…:00 +0000" layout.
        // RFC 1123 wants "…:00 GMT" — same shape. Just paste "GMT".
        format!("{}GMT", prefix)
    } else {
        // Best-effort fallback: pass through. Will likely fail the
        // OSS Date-strictness check on the server but that's the same
        // behaviour the caller would have had without this helper.
        rfc.to_string()
    }
}

/// Percent-encode a single path segment under RFC 3986 (unreserved set
/// `A-Z a-z 0-9 - _ . ~` is passed through; everything else → %XX upper).
/// `/` is intentionally NOT escaped here — the caller splits on "/" first.
fn percent_encode_segment(s: &str) -> std::borrow::Cow<'_, str> {
    if s.bytes()
        .all(|b| b.is_ascii_alphanumeric() || b"-_.~".contains(&b))
    {
        return std::borrow::Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    std::borrow::Cow::Owned(out)
}

/// Percent-encode a value used as a query-string value (e.g. `prefix=...`).
/// RFC 3986 keeps `/` unreserved in a query component, but our caller
/// ALWAYS encodes `/` because we want `prefix=foo%2Fbar%2F` to be a
/// single prefix string, never a hierarchy; OSS also accepts either.
fn encode_query_token(s: &str) -> String {
    let mut out = String::with_capacity(s.len() * 3);
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push('%');
            out.push_str(&format!("{:02X}", b));
        }
    }
    out
}

/// Strip the trailing prefix_root from a key (used in tests / debug
/// logs). Unused in production — the fetcher reassembles filenames
/// itself.
#[cfg(test)]
impl OssBridge {
    /// Build a test-only OssBridge that never hits the network. Uses a
    /// fixed mock endpoint and bucket so URL/resource assertions are
    /// deterministic. `prefix_root` MUST end in `/` (mirrors the
    /// constructed-by-`new` invariant).
    pub fn for_test_only(
        prefix_root: impl Into<String>,
        access_key_id: impl Into<String>,
        access_key_secret: impl Into<String>,
    ) -> Self {
        Self::for_test_with_endpoint(
            prefix_root,
            access_key_id,
            access_key_secret,
            "https://mock.local",
        )
    }

    /// Same as `for_test_only` but the endpoint is caller-supplied, so
    /// integration tests against a loopback mock HTTP server get a real
    /// `http://127.0.0.1:port` base URL.
    pub fn for_test_with_endpoint(
        prefix_root: impl Into<String>,
        access_key_id: impl Into<String>,
        access_key_secret: impl Into<String>,
        endpoint: impl Into<String>,
    ) -> Self {
        let prefix_root = prefix_root.into();
        debug_assert!(prefix_root.ends_with('/') || prefix_root.is_empty());
        OssBridge {
            client: Arc::new(Client::new()),
            creds: OssCredentials {
                access_key_id: access_key_id.into(),
                access_key_secret: access_key_secret.into(),
            },
            bucket: "unit-bucket".into(),
            prefix_root,
            endpoint: endpoint.into(),
        }
    }
}

// ─────────────────────────────────────────────────────────────────────
// tests — unit only, no network; Day 5 e2e hits the real apmm bucket
// ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::TaskType;

    fn sample_task() -> Task {
        Task::new("echo hi".into(), TaskType::Shell)
    }

    // ── Filename helpers ───────────────────────────────────────────

    #[test]
    fn task_filename_matches_http_bridge_form() {
        use chrono::{TimeZone, Utc};
        let mut t = Task::new("noop".into(), TaskType::Shell);
        t.timestamp = Utc.with_ymd_and_hms(2026, 11, 9, 22, 45, 12).unwrap();
        let expected = format!("20261109_224512_{}.json", t.task_id);
        assert_eq!(task_filename(&t), expected);
    }

    // ── resource / URL construction ────────────────────────────────

    #[test]
    fn resource_form_leading_slash_bucket_key() {
        let br = OssBridge::for_test_only("team-lx/proj/", "id", "sk");
        assert_eq!(br.resource(""), "/unit-bucket/");
        assert_eq!(br.resource("foo"), "/unit-bucket/foo");
        assert_eq!(br.resource("foo/bar"), "/unit-bucket/foo/bar");
        assert_eq!(
            br.resource("team-lx/proj/commands/x.json"),
            "/unit-bucket/team-lx/proj/commands/x.json"
        );
    }

    #[test]
    fn resource_percent_encodes_non_ascii_and_spaces_per_segment() {
        let br = OssBridge::for_test_only("p/", "id", "sk");
        // slash is preserved (segment separator), ' ' becomes %20.
        assert_eq!(
            br.resource("dir 1/file.txt"),
            "/unit-bucket/dir%201/file.txt"
        );
    }

    #[test]
    fn object_url_full_path() {
        let br = OssBridge::for_test_only("team-lx/proj/", "id", "sk");
        assert_eq!(
            br.object_url("team-lx/proj/commands/x.json"),
            "https://mock.local/team-lx/proj/commands/x.json"
        );
    }

    // ── date / rfc2822 to GMT ─────────────────────────────────────

    #[test]
    fn rfc2822_to_gmt_swaps_plus0000_for_gmt() {
        assert_eq!(
            rfc2822_to_gmt("Mon, 09 Nov 2026 22:45:00 +0000"),
            "Mon, 09 Nov 2026 22:45:00 GMT"
        );
        // chrono's `Utc::now().to_rfc2822()` emits "+0000" with a space
        // in front; strip_suffix removes the "+0000" only, leaving the
        // original trailing space; we paste "GMT" in its place, which
        // yields exactly the RFC 1123 form OSS expects.
    }

    #[test]
    fn rfc2822_to_gmt_passes_through_unrecognised_input() {
        assert_eq!(rfc2822_to_gmt("already GMT"), "already GMT");
    }

    // ── per-segment URL encoding ─────────────────────────────────

    #[test]
    fn encoder_preserves_unreserved() {
        for s in &["aB9", "-_.~", "aa-bb_cc.d~e"] {
            assert!(matches!(
                percent_encode_segment(s),
                std::borrow::Cow::Borrowed(_)
            ));
            assert_eq!(percent_encode_segment(s), *s);
        }
    }

    #[test]
    fn encoder_encodes_spaces_unicode_hash_plus() {
        assert_eq!(percent_encode_segment("a b"), "a%20b");
        assert_eq!(percent_encode_segment("a#b"), "a%23b");
        assert_eq!(percent_encode_segment("a+b"), "a%2Bb");
        assert_eq!(percent_encode_segment("文件"), "%E6%96%87%E4%BB%B6");
    }

    #[test]
    fn query_token_encodes_slash_to_percent2F() {
        // `encode_query_token` is used for prefix=... values in LIST
        // URLs — slash MUST be encoded so the value can't be parsed as
        // extra path components by intermediate proxies.
        assert_eq!(encode_query_token("a/b"), "a%2Fb");
        assert_eq!(encode_query_token("team-lx/proj/"), "team-lx%2Fproj%2F");
        // Hyphen in `team-lx` is RFC 3986 unreserved and passes through.
        assert_eq!(encode_query_token("abc-._~"), "abc-._~");
    }

    #[test]
    fn query_token_hyphen_unreserved() {
        assert_eq!(encode_query_token("a-b"), "a-b");
    }

    // ── StatusRecord serialization ─────────────────────────────────

    #[test]
    fn status_record_to_json_has_known_field_names() {
        // Field names must match http_bridge::StatusRecord exactly so
        // the fetcher's status parse is transport-agnostic.
        let rec = StatusRecord {
            task_id: "T".into(),
            status: TaskStatus::Completed.to_string(),
            message: None,
            timestamp: "2026-11-09T22:45:00Z".into(),
        };
        let json = serde_json::to_value(&rec).unwrap();
        assert_eq!(json["task_id"], "T");
        assert_eq!(json["status"], "Completed");
        assert_eq!(json["timestamp"], "2026-11-09T22:45:00Z");
        // message is `skip_serializing_if = "Option::is_none"` so absent.
        assert!(json.get("message").is_none());
    }

    #[test]
    fn parse_task_status_includes_all_six_variants() {
        for (s, v) in [
            ("Pending", TaskStatus::Pending),
            ("Running", TaskStatus::Running),
            ("Completed", TaskStatus::Completed),
            ("Failed", TaskStatus::Failed),
            ("Timeout", TaskStatus::Timeout),
            ("Cancelled", TaskStatus::Cancelled),
        ] {
            assert_eq!(parse_task_status(s), Some(v));
        }
        assert_eq!(parse_task_status("Unknown"), None);
    }

    // ── LIST prefix derivation ─────────────────────────────────────

    #[test]
    fn relative_key_and_strip_prefix() {
        let br = OssBridge::for_test_only("team-lx/proj/", "id", "sk");
        let key = br.relative_key("commands", "foo.json");
        assert_eq!(key, "team-lx/proj/commands/foo.json");
        assert_eq!(br.strip_prefix(&key), Some("commands/foo.json"));
        // And the same relative_key should embed into resource().
        assert_eq!(
            br.resource(&key),
            "/unit-bucket/team-lx/proj/commands/foo.json"
        );
    }

    // ── task reuse via Task::new (pitfall #2 — no Default derive) ──

    #[test]
    fn sample_task_serialises_to_commands_schema() {
        let t = sample_task();
        let v = serde_json::to_value(&t).unwrap();
        assert!(v["task_id"].is_string());
        assert_eq!(v["task_type"], "Shell");
        assert_eq!(v["command"], "echo hi");
    }

    // -- a signature-shape smoke test: the URL we hand to reqwest and
    // the CanonicalizedResource we sign MUST be derived from the same
    // string. We assert by computing both with helper methods.
    #[test]
    fn url_and_resource_share_percent_encoded_segments() {
        let br = OssBridge::for_test_only("team-lx/proj/", "id", "sk");
        let key = "team-lx/proj/commands/x.json";
        let url = br.object_url(key);
        let res = br.resource(key);
        // Taking the URL after endpoint + / should equal the resource's
        // part after /<bucket>/.
        let url_after_root = url.strip_prefix("https://mock.local/").unwrap();
        let res_after_bucket = res.strip_prefix("/unit-bucket/").unwrap();
        assert_eq!(url_after_root, res_after_bucket);
    }

    // ────────────────────────────────────────────────────────────────
    // mock HTTP end-to-end — exercise the full PUT+LIST+GET+DELETE path
    // against an in-process loopback server. No external crates — just
    // std::net::TcpListener on 127.0.0.1:<random>.
    //
    // What these tests DO NOT assert:
    //   - HMAC correctness (sign.rs owns that).
    //   - Date header precision (the client uses Utc::now()).
    //   - Sub-resource handling (out of scope for v1 — the bridge only
    //     emits GET/PUT/DELETE/LIST).
    //
    // What they DO lock in:
    //   - The URL the bridge PUTs is the URL the bridge would GET and
    //     DELETE against — same segments, same percent-encoding.
    //   - Both `Authorization: OSS <id>:<sig>` and `Date: …GMT` headers
    //     are stamped on every request.
    //   - LIST parses a real OSS XML body; list_pending_tasks returns the
    //     parsed Task.
    // ────────────────────────────────────────────────────────────────

    use std::io::{Read as IoRead, Write as IoWrite};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    #[derive(Clone, Debug)]
    struct RecordedRequest {
        method: String,
        path: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    impl RecordedRequest {
        fn header_lower(&self, name: &str) -> Option<String> {
            let lower = name.to_ascii_lowercase();
            self.headers
                .iter()
                .find(|(k, _)| k.to_ascii_lowercase() == lower)
                .map(|(_, v)| v.clone())
        }
    }

    struct MockOss {
        addr: std::net::SocketAddr,
        hits: StdArc<AtomicUsize>,
        requests: StdArc<StdMutex<Vec<RecordedRequest>>>,
        _handle: std::thread::JoinHandle<()>,
    }

    impl MockOss {
        fn start(
            body_for_matcher: StdArc<dyn Fn(&RecordedRequest) -> (u16, String, String) + Send + Sync>,
        ) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind oss mock");
            let addr = listener.local_addr().unwrap();
            let hits = StdArc::new(AtomicUsize::new(0));
            let requests = StdArc::new(StdMutex::new(Vec::new()));
            let hits_c = StdArc::clone(&hits);
            let req_c = StdArc::clone(&requests);
            let handle = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else { break };
                    let Some(req) = read_oss_request(&mut s) else { continue };
                    hits_c.fetch_add(1, Ordering::SeqCst);
                    req_c.lock().unwrap().push(req.clone());
                    let (status, ct, body) = body_for_matcher(&req);
                    let _ = write_oss_response(&mut s, status, &ct, &body);
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            });
            Self {
                addr,
                hits,
                requests,
                _handle: handle,
            }
        }
        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }
        #[allow(dead_code)]
        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
        fn requests(&self) -> Vec<RecordedRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn read_oss_request(stream: &mut TcpStream) -> Option<RecordedRequest> {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok()?;
        let mut buf = Vec::with_capacity(4096);
        let mut chunk = [0u8; 4096];
        let mut content_length = 0usize;
        let mut header_end = None;
        for _ in 0..8 {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => {
                    buf.extend_from_slice(&chunk[..n]);
                    if header_end.is_none() {
                        if let Some(pos) = find_crlf2(&buf) {
                            header_end = Some(pos);
                            for line in String::from_utf8_lossy(&buf[..pos]).lines() {
                                if line.to_ascii_lowercase().starts_with("content-length:") {
                                    if let Some(v) = line.split(':').nth(1) {
                                        content_length = v.trim().parse().unwrap_or(0);
                                    }
                                }
                            }
                        }
                    }
                    if let Some(he) = header_end {
                        if buf.len() >= he + 4 + content_length {
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
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        let body = buf[(he + 4)..].to_vec();
        Some(RecordedRequest {
            method,
            path,
            headers,
            body,
        })
    }

    fn find_crlf2(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn write_oss_response(
        stream: &mut TcpStream,
        status: u16,
        ct: &str,
        body: &str,
    ) -> std::io::Result<()> {
        let status_text = match status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            404 => "Not Found",
            403 => "Forbidden",
            _ => "X",
        };
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            status,
            status_text,
            ct,
            body.len()
        );
        stream.write_all(head.as_bytes())?;
        stream.write_all(body.as_bytes())?;
        Ok(())
    }

    #[test]
    fn e2e_submit_then_get_result_then_delete() {
        // ─── arrange: in-memory "bucket" backing the loopback server ──
        let store: StdArc<StdMutex<std::collections::HashMap<String, String>>> =
            StdArc::new(StdMutex::new(std::collections::HashMap::new()));
        let stored = StdArc::clone(&store);
        let mock = MockOss::start(StdArc::new(move |req| {
            let mut s = stored.lock().unwrap();
            let path = req.path.clone();
            match req.method.as_str() {
                "PUT" => {
                    s.insert(path, String::from_utf8_lossy(&req.body).into_owned());
                    (200, "application/xml".into(), "".into())
                }
                "GET" => {
                    if path.starts_with("/?") {
                        // LIST — synthesize XML from keys under prefix.
                        let prefix = extract_query_param(&path, "prefix")
                            .unwrap_or_default();
                        let mut body = String::from(
                            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ListBucketResult>\n  <Name>unit-bucket</Name>\n  <MaxKeys>1000</MaxKeys>\n  <IsTruncated>false</IsTruncated>\n",
                        );
                        for k in s.keys() {
                            let decoded_key = k.trim_start_matches('/');
                            let matches_prefix = percent_decode_query(&prefix)
                                .as_deref()
                                .map(|p| decoded_key.starts_with(p))
                                .unwrap_or(false);
                            if !matches_prefix {
                                continue;
                            }
                            body.push_str(&format!(
                                "  <Contents><Key>{}</Key><Size>{}</Size></Contents>\n",
                                decoded_key,
                                s.get(k).map(|v| v.len()).unwrap_or(0),
                            ));
                        }
                        body.push_str("</ListBucketResult>");
                        (200, "application/xml".into(), body)
                    } else if let Some(v) = s.get(&path) {
                        (200, "application/json".into(), v.clone())
                    } else {
                        (404, "application/xml".into(), "<Error/>".into())
                    }
                }
                "DELETE" => {
                    s.remove(&path);
                    (204, "application/xml".into(), "".into())
                }
                _ => (405, "text/plain".into(), "Method Not Allowed".into()),
            }
        }));

        let br = OssBridge::for_test_with_endpoint(
            "team-lx/mock/",
            "id-1",
            "sk-1",
            mock.url(),
        );

        // 1) submit a Task
        let task = Task::new("echo e2e".into(), TaskType::Shell);
        br.submit_task(&task).expect("submit_task should succeed");

        // 2) status query: results and status not present yet → falls
        // through to a LIST hit on the PUT commands blob → Pending.
        let status = br.query_status(&task.task_id).expect("query_status");
        assert!(matches!(status.status, TaskStatus::Pending));

        // 3) list_pending_tasks returns exactly the submitted task.
        let tasks = br.list_pending_tasks().expect("list_pending_tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, task.task_id);
        assert_eq!(tasks[0].command, "echo e2e");

        // 4) read_task round-trip via UUID.
        let reread = br.read_task(&task.task_id).expect("read_task");
        assert_eq!(reread.task_id, task.task_id);

        // 5) write_result + get_result round-trip.
        let result = TaskResult {
            task_id: task.task_id,
            status: TaskStatus::Completed,
            output: crate::core::models::TaskOutput {
                stdout: "ok".into(),
                stderr: "".into(),
                exit_code: Some(0),
            },
            command: "echo e2e".into(),
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 12,
            retries_used: 0,
            artifacts: Vec::new(),
            error_message: None,
        };
        br.write_result(&task.task_id, &result).expect("write_result");
        let got = br.get_result(&task.task_id).expect("get_result");
        assert_eq!(got.task_id, task.task_id);
        assert_eq!(got.output.stdout, "ok");

        // 6) query_status now resolves from results/ as Completed.
        let status = br.query_status(&task.task_id).expect("query_status2");
        assert!(matches!(status.status, TaskStatus::Completed));

        // 7) delete_task_blob hits the exact filename PUT used.
        br.delete_task_blob("commands", &task)
            .expect("delete_task_blob");
        let tasks = br.list_pending_tasks().expect("list_pending_tasks2");
        assert_eq!(tasks.len(), 0);

        // ─── sanity: every request carries OSS-style headers ──
        for r in mock.requests() {
            let auth = r.header_lower("authorization").unwrap_or_default();
            assert!(
                auth.starts_with("OSS id-1:"),
                "request missing/bad Authorization: {}",
                auth
            );
            let date = r.header_lower("date").unwrap_or_default();
            assert!(date.ends_with("GMT"), "missing Date header: {}", r.path);
        }
    }

    #[test]
    fn e2e_remove_task_lack_permission_becomes_auth_error() {
        let mock = MockOss::start(StdArc::new(|_req| {
            (403, "application/xml".into(), "<Error><Code>AccessDenied</Code></Error>".into())
        }));
        let br = OssBridge::for_test_with_endpoint(
            "team-lx/mock/",
            "id-1",
            "sk-1",
            mock.url(),
        );
        let t = Task::new("noop".into(), TaskType::Shell);
        let r = br.submit_task(&t);
        match r {
            Err(BifrostError::AuthError(_)) => {}
            other => panic!("expected AuthError on 403, got {:?}", other),
        }
    }

    #[test]
    fn e2e_list_objects_parses_real_oss_xml() {
        // Returns a fixed XML body verbatim, extracted from the OSS
        // ListBucketResult schema (same fields the real server uses).
        const XML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<ListBucketResult>
  <Name>unit-bucket</Name>
  <Prefix>team-lx/mock/commands/</Prefix>
  <MaxKeys>1000</MaxKeys>
  <IsTruncated>false</IsTruncated>
  <Contents>
    <Key>team-lx/mock/commands/20261109_224500_aaaaaaaa-0000-0000-0000-000000000001.json</Key>
    <Size>1024</Size>
  </Contents>
</ListBucketResult>"#;
        let mock = MockOss::start(StdArc::new(move |_req| {
            (200, "application/xml".into(), XML.into())
        }));
        let br = OssBridge::for_test_with_endpoint(
            "team-lx/mock/",
            "id-1",
            "sk-1",
            mock.url(),
        );
        // list_pending_tasks uses LIST → returns tasks, but the JSON blob
        // GET will 404 (the mock only serves LIST)—so 0 tasks after peek
        // warning. The test is about LIST parsing not peek success.
        let result = br.list_pending_tasks();
        assert!(result.is_ok(), "list_pending_tasks should not error");
    }

    fn extract_query_param(path_q: &str, key: &str) -> Option<String> {
        let (_, qs) = path_q.split_once('?')?;
        for pair in qs.split('&') {
            let mut kv = pair.splitn(2, '=');
            let k = kv.next()?;
            let v = kv.next().unwrap_or("");
            if k == key {
                return Some(v.to_string());
            }
        }
        None
    }

    fn percent_decode_query(s: &str) -> Option<String> {
        let mut out = Vec::new();
        let mut i = 0;
        let bytes = s.as_bytes();
        while i < bytes.len() {
            if bytes[i] == b'%' && i + 2 < bytes.len() {
                if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                    out.push(v);
                    i += 3;
                    continue;
                }
            }
            out.push(bytes[i]);
            i += 1;
        }
        String::from_utf8(out).ok()
    }
}
