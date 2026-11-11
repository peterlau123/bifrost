// HTTP Bridge - client <-> transport <-> target over a dumb HTTP relay
// (e.g. Nexus Raw hosted repo, `curl --upload-file` + `wget` semantics).
//
// Design: docs/design/http-bridge.md (D1~D14, 2026-11-06 a13792a).
//
// The daemon NEVER selects this transport (G1): it always listens on the
// local shared_storage via Protocol; HTTP is strictly the client /
// fetcher / pusher path.
//
// Sync story (§4.2 note): the Bridge trait is sync throughout, SshBridge
// is blocking, so we pick `reqwest::blocking`. **Do NOT call this from
// inside a tokio runtime** — the daemon is on tokio, but the daemon never
// picks this transport; client CLI / fetcher / pusher are all sync mains.
// Crash-early guard: constructors `debug_assert!` no runtime is current.
//
// Auth model (D5/D6): HTTP Basic over HTTPS only; or `kind="none"` for
// open intranet GETs. Passwords live in the settings file (mode 600).
//
// Error model (§4.5):
//   - network / 5xx   → retry 3x (1s → 2s → 4s) then ExecutionError
//   - connect / read timeout → ExecutionError
//   - 2xx (incl. 201, 204) → success; == instead of `== 200`!
//   - 404 (read)       → TaskNotFound; (DELETE) → Ok (already gone)
//   - 401 / 403        → AuthError, no retry
//   - 405 (DELETE)     → warning + rely on ledger (D3/D9)
//   - 404 body trap    → check status first, never Content-Length
//
// URL encoding (D13): percent-encode **per segment** because `/` inside
// `prefix` is a path separator, not data. We hand-roll the RFC 3986
// unreserved charset (A-Z a-z 0-9 - _ . ~) instead of pulling in the
// `urlencoding` crate for a single helper.

use std::sync::Arc;
use std::time::Duration;

use reqwest::blocking::Client;
use reqwest::StatusCode;
use serde::Deserialize;
use uuid::Uuid;

use crate::core::bridge::{Bridge, TaskStatusResponse};
use crate::core::error::{BifrostError, Result};
use crate::core::models::{Task, TaskResult, TaskStatus};
use crate::core::settings::{AuthKind, HttpSection, ListingStyle};

/// Max number of search pages to consume on a single `list_pending_tasks`
/// call. Defends against a degenerate `q` matching the whole repo (D8).
/// Nexus caps at 50 items/page, so 20 pages = 1000 candidate paths.
const MAX_SEARCH_PAGES: usize = 20;

/// Retry policy: 3 attempts total (initial + 2 retries) with 1s/2s/4s
/// backoff. Network and 5xx only (§4.5).
const RETRY_DELAYS_MS: [u64; 2] = [1_000, 2_000];

/// HTTP bridge. Holds the validated `[http]` section, an Arc-wrapped
/// blocking reqwest::Client, and two pre-computed URL bases:
///   - `object_base`:  `<base_url>/<prefix>`   (task/result blobs live here)
///   - `search_base`:  `<scheme>://<host>/service/rest/v1/search/assets`
///     (Nexus REST search lives outside /repository/)
#[derive(Debug, Clone)]
pub struct HttpBridge {
    cfg: HttpSection,
    client: Arc<Client>,
    /// Normalized prefix "<prefix>/". Kept for tests + `clone()`gers in
    /// fetches/pushers that want to inspect the contract (D12/D13).
    #[allow(dead_code)]
    normalized_prefix: String,
    object_base: String,   // "<base_url>/<normalized_prefix>"
    search_base: String,   // derived from base_url origin
    listing_query: String, // explicit or last segment of prefix
    /// Repo-relative prefix used for `path.starts_with` filtering of search
    /// results. Search returns paths relative to the repo ROOT (e.g.
    /// `uploads/team-lx/x/commands/y.json`), so prefixing with just
    /// `<prefix>/commands/` would never match. We rebuild the full
    /// repo-relative prefix from `base_url`'s path + normalized prefix.
    repo_prefix: String, // e.g. "uploads/team-lx/x/" ("" if base has no path)
}

impl HttpBridge {
    /// Production constructor. Panics in debug builds if called from inside
    /// a tokio runtime (would deadlock the blocking client).
    pub fn new(cfg: &HttpSection) -> Result<Self> {
        Self::build(cfg, false)
    }

    /// Test-only constructor that tolerates `http://` URLs (for mock
    /// servers on loopback). `#[cfg(test)]` keeps it out of production;
    /// integration tests use `new()` with a loopback http URL since
    /// settings.validate() already whitelists 127.0.0.1/localhost.
    #[cfg(test)]
    pub fn new_unchecked(cfg: &HttpSection) -> Result<Self> {
        Self::build(cfg, true)
    }

    fn build(cfg: &HttpSection, allow_insecure_http: bool) -> Result<Self> {
        debug_assert!(
            tokio::runtime::Handle::try_current().is_err(),
            "HttpBridge must not be constructed inside a tokio runtime \
             (reqwest::blocking will panic on first request)"
        );

        let prefix_owned = cfg.prefix.clone().ok_or_else(|| {
            BifrostError::ConfigInvalid(
                "http.prefix is required (bilateral contract root, D12)".into(),
            )
        })?;
        let prefix = prefix_owned.trim();
        if prefix.is_empty() {
            return Err(BifrostError::ConfigInvalid(
                "http.prefix must be non-empty after trim (D12)".into(),
            ));
        }

        let base_url_owned = cfg
            .base_url
            .clone()
            .ok_or_else(|| BifrostError::ConfigInvalid("http.base_url is required".into()))?;
        let base_url = base_url_owned.trim_end_matches('/');
        let is_https = base_url.starts_with("https://");
        let is_loopback_http =
            base_url.starts_with("http://127.0.0.1") || base_url.starts_with("http://localhost");
        if !is_https && !is_loopback_http && !allow_insecure_http {
            return Err(BifrostError::ConfigInvalid(
                "http.base_url must be https:// (loopback http allowed only for tests)".into(),
            ));
        }

        // D13: normalize prefix to "<prefix>/" — trim leading/trailing `/`,
        // collapse consecutive `/`, then append a single trailing `/`.
        // `starts_with("team-lx/sess")` would otherwise match a peer
        // namespace like `team-lx/session-evil/...` — cross-prefix task
        // theft is a security issue, not a performance one.
        let normalized_prefix = normalize_prefix(prefix);

        // Object URLs for blobs (tasks/results/status/cancel). Path segment
        // encoding happens per-segment below (see `object_url`).
        let object_base = format!("{}/{}", base_url, normalized_prefix);

        // Repo-relative prefix for filtering search results. Search returns
        // paths relative to the repo ROOT; e.g. for
        // base_url=https://host/repository/tmp-raw/uploads the repo prefix
        // is "uploads/". Stripping that part off and prefixing with
        // normalized_prefix yields the exact filter key. If base_url has no
        // path after /repository/<repo>/, repo_prefix is "".
        let repo_base_prefix = extract_repo_base_prefix(base_url);
        let repo_prefix = format!("{}{}", repo_base_prefix, normalized_prefix);

        // Search API: derived from base_url origin (NOT from /repository/),
        // because /service/rest/v1 is a sibling of /repository on Nexus.
        let search_base = derive_search_base(base_url)?;

        // Listing query (D8): explicit, else last non-empty segment of
        // prefix — the most distinctive token.
        let listing_query = cfg
            .listing_query
            .clone()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| {
                normalized_prefix
                    .rsplit('/')
                    .find(|s| !s.is_empty())
                    .unwrap_or("")
                    .to_string()
            });
        if listing_query.is_empty() && cfg.listing_style == ListingStyle::RestSearch {
            return Err(BifrostError::ConfigInvalid(
                "listing_query cannot be derived from prefix; set http.listing_query explicitly"
                    .into(),
            ));
        }

        // HTTP client (blocking; rustls-tls; no global timeout — large
        // artifacts would be killed, see design §4.5).
        let client = Client::builder()
            .connect_timeout(Duration::from_secs(cfg.connect_timeout_secs))
            .timeout(Duration::from_secs(cfg.read_timeout_secs))
            .user_agent(format!("bifrost-http-bridge/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| {
                BifrostError::ConfigInvalid(format!("failed to build http client: {e}"))
            })?;

        Ok(Self {
            cfg: cfg.clone(),
            client: Arc::new(client),
            normalized_prefix,
            object_base,
            search_base,
            listing_query,
            repo_prefix,
        })
    }

    /// Object URL for `<subdir>/<filename>` under the normalized prefix.
    /// Path segments are percent-encoded individually (D13).
    fn object_url(&self, subdir: &str, filename: &str) -> String {
        format!(
            "{}{}/{}",
            self.object_base,
            encode_segment(subdir),
            encode_segment(filename)
        )
    }

    /// Direct-PUT entry point for the pusher: forwards a local file to
    /// `<base_url>/<prefix>/<subdir>/<filename>` as the request body.
    /// Unlike `Bridge::write_result/write_status` (which rebuild JSON via
    /// serde), this takes raw bytes so the pusher can stream a file
    /// straight from disk without an intermediate parse step. Naming kept
    /// symmetric with `object_url` for predictability.
    ///
    /// Success is the 2xx family (Nexus PUT new or update is 201, see
    /// appendix A). 401/403 → AuthError; other 4xx/5xx → ExecutionError.
    pub fn put_path(&self, subdir: &str, filename: &str, body: Vec<u8>) -> Result<()> {
        let url = self.object_url(subdir, filename);
        let status = self.http_put(&url, body)?;
        if status.is_success() {
            return Ok(());
        }
        if matches!(status, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            return Err(BifrostError::AuthError(format!(
                "put_path {}: {} (check http.auth)",
                subdir,
                status.as_u16()
            )));
        }
        Err(BifrostError::ExecutionError(format!(
            "put_path {}: {}",
            subdir,
            status.as_u16()
        )))
    }

    /// Direct-DELETE entry point for the fetcher. Same D3 semantics as
    /// `remove_task`, but takes the FULL filename (including the `<ts>_`
    /// timestamp prefix baked into command filenames). The Bridge trait's
    /// `remove_task(task_id)` only knows the UUID and cannot intercept the
    /// timestamp always prepended by submit_task, so callers that already
    /// know the concrete remote blob name (fetcher, pusher) MUST use this
    /// entry point.
    pub fn delete_path(&self, subdir: &str, filename: &str) -> Result<()> {
        let url = self.object_url(subdir, filename);
        let status = self.http_delete(&url)?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()), // D3: already gone
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("delete_path {}", status.as_u16()),
            )),
            StatusCode::METHOD_NOT_ALLOWED => {
                // D3/D9: warning + rely on the ledger, do not error out.
                log::warn!(
                    "delete_path: METHOD_NOT_ALLOWED on {}/{} — backend lacks DELETE; idempotency relies on fetcher ledger",
                    subdir, filename
                );
                Ok(())
            }
            other => Err(BifrostError::ExecutionError(format!(
                "delete_path: {}",
                other.as_u16()
            ))),
        }
    }

    /// URL for the Nexus REST search endpoint. `continuationToken` is the
    /// pagination cursor handed back in the previous page (D8).
    fn search_url(&self, continuation: Option<&str>) -> String {
        let repo = self
            .cfg
            .repository
            .as_deref()
            .expect("settings.validate() enforces repository for rest-search");
        let mut url = format!(
            "{}?repository={}&q={}",
            self.search_base,
            encode_query_param(repo),
            encode_query_param(&self.listing_query),
        );
        if let Some(tok) = continuation {
            url.push_str("&continuationToken=");
            url.push_str(&encode_query_param(tok));
        }
        url
    }

    /// Apply Basic auth if configured. No-op for kind="none" (D5).
    fn with_auth(
        &self,
        req: reqwest::blocking::RequestBuilder,
    ) -> reqwest::blocking::RequestBuilder {
        if let Some(auth) = &self.cfg.auth {
            if auth.kind == AuthKind::Basic {
                let user = auth.user.as_deref().unwrap_or("");
                let pass = auth.password.as_deref().unwrap_or("");
                return req.basic_auth(user, Some(pass));
            }
        }
        req
    }

    /// Map an HTTP response to Result, honoring §4.5 error classes.
    /// `op` is a short probe name used in log/error strings.
    fn check_response(
        &self,
        op: &str,
        resp: reqwest::blocking::Response,
    ) -> Result<reqwest::blocking::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        // Peek a truncated body for debug context (avoid leaking huge pages).
        let body_preview = resp
            .text()
            .unwrap_or_default()
            .chars()
            .take(200)
            .collect::<String>();
        match status {
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                Err(BifrostError::AuthError(format!(
                    "{}: {} {} (check http.auth settings)",
                    op,
                    status.as_u16(),
                    body_preview
                )))
            }
            StatusCode::NOT_FOUND => Err(BifrostError::TaskNotFound(Uuid::nil())), // caller maps real id
            other => Err(BifrostError::ExecutionError(format!(
                "{}: unexpected {} {}",
                op,
                other.as_u16(),
                body_preview
            ))),
        }
    }

    /// Retry wrapper (§4.5): retry on network errors and 5xx up to the
    /// RETRY_DELAYS_MS cadence.
    fn with_retry<F>(&self, op: &str, mut f: F) -> Result<reqwest::blocking::Response>
    where
        F: FnMut() -> reqwest::Result<reqwest::blocking::Response>,
    {
        let mut last_err: Option<BifrostError> = None;
        for (attempt, delay_ms) in std::iter::once(&0u64)
            .chain(RETRY_DELAYS_MS.iter())
            .enumerate()
        {
            if attempt > 0 {
                std::thread::sleep(Duration::from_millis(*delay_ms));
            }
            match f() {
                Ok(resp) => {
                    // server 5xx → treat as retryable
                    if resp.status().is_server_error() && attempt < RETRY_DELAYS_MS.len() {
                        last_err = Some(BifrostError::ExecutionError(format!(
                            "{}: {} (retrying)",
                            op,
                            resp.status().as_u16()
                        )));
                        continue;
                    }
                    return Ok(resp);
                }
                Err(e) => {
                    last_err = Some(BifrostError::ExecutionError(format!(
                        "{}: network error: {}",
                        op, e
                    )));
                }
            }
        }
        Err(last_err
            .unwrap_or_else(|| BifrostError::ExecutionError(format!("{}: retries exhausted", op))))
    }

    fn http_put(&self, url: &str, body: Vec<u8>) -> Result<StatusCode> {
        let resp = self.with_retry("PUT", || {
            self.with_auth(self.client.put(url))
                .body(body.clone())
                .send()
        })?;
        let status = resp.status();
        // Consume body (avoid connection-pool starvation on errors).
        let _ = resp.text();
        Ok(status)
    }

    fn http_get(&self, url: &str) -> Result<reqwest::blocking::Response> {
        self.with_retry("GET", || self.with_auth(self.client.get(url)).send())
    }

    fn http_delete(&self, url: &str) -> Result<StatusCode> {
        let resp = self.with_retry("DELETE", || self.with_auth(self.client.delete(url)).send())?;
        let status = resp.status();
        let _ = resp.text();
        Ok(status)
    }
}

impl Bridge for HttpBridge {
    fn submit_task(&self, task: &Task) -> Result<()> {
        let body = serde_json::to_vec(task)?;
        let filename = task_filename(task);

        if self.cfg.atomic_upload {
            // D1 opt-in: `.tmp` then real name. Two PUTs — the second is
            // the "rename". If the first succeeds and the second fails,
            // the .tmp will sit orphaned (we could DELETE it here, but
            // the design prefers leaving that to cleanup routines).
            let tmp_url = self.object_url("commands", &format!("{filename}.tmp"));
            let s = self.http_put(&tmp_url, body.clone())?;
            if !s.is_success() {
                return Err(BifrostError::ExecutionError(format!(
                    "submit_task(.tmp): {}",
                    s.as_u16()
                )));
            }
        }

        let url = self.object_url("commands", &filename);
        let status = self.http_put(&url, body)?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("submit_task: {} (check http.auth)", status.as_u16()),
            )),
            s => Err(BifrostError::ExecutionError(format!(
                "submit_task: {}",
                s.as_u16()
            ))),
        }
    }

    fn query_status(&self, task_id: &Uuid) -> Result<TaskStatusResponse> {
        // Tier 1: results/<id>_result.json → Completed
        let result_url = self.object_url("results", &format!("{task_id}_result.json"));
        let r1 = self.http_get(&result_url);
        let result_hit = matches!(&r1, Ok(r) if r.status().is_success());
        if result_hit {
            return Ok(TaskStatusResponse {
                task_id: *task_id,
                status: TaskStatus::Completed,
                message: Some("found in results/".into()),
            });
        }

        // Tier 2: status/<id>.json → Running / status value
        let status_url = self.object_url("status", &format!("{task_id}.json"));
        let r2 = self.http_get(&status_url);
        if let Ok(resp) = r2 {
            if resp.status().is_success() {
                let status: TaskStatus = resp
                    .json::<StatusRecord>()
                    .ok()
                    .and_then(|s| match s.status.as_str() {
                        "Pending" => Some(TaskStatus::Pending),
                        "Running" => Some(TaskStatus::Running),
                        "Completed" => Some(TaskStatus::Completed),
                        "Failed" => Some(TaskStatus::Failed),
                        "Timeout" => Some(TaskStatus::Timeout),
                        "Cancelled" => Some(TaskStatus::Cancelled),
                        _ => None,
                    })
                    .unwrap_or(TaskStatus::Running);
                return Ok(TaskStatusResponse {
                    task_id: *task_id,
                    status,
                    message: Some("found in status/".into()),
                });
            }
        }

        // Tier 3 (D10/D14): search the commands/ prefix as a Pending
        // fallback. `listing_style=none` → D14: no third tier; the caller's
        // wait_timeout expiry converts that into TaskLost. Here we just
        // faithfully report what we found.
        let mut found_where: Vec<&'static str> = Vec::new();
        if result_hit {
            found_where.push("results");
        }
        // (r2 handled above)
        let third_tier_available = self.cfg.listing_style == ListingStyle::RestSearch;
        if third_tier_available {
            match self.search_in_commands(task_id) {
                Ok(true) => {
                    return Ok(TaskStatusResponse {
                        task_id: *task_id,
                        status: TaskStatus::Pending,
                        message: Some("pending in commands/ (not yet claimed)".into()),
                    });
                }
                Ok(false) => { /* fall through */ }
                Err(e) => {
                    // Search itself failed — design says "only search failure
                    // keeps reporting Pending". Surface the error message
                    // but report Pending, NOT TaskLost.
                    return Ok(TaskStatusResponse {
                        task_id: *task_id,
                        status: TaskStatus::Pending,
                        message: Some(format!(
                            "search failed (transient): {e}; reporting Pending per D10"
                        )),
                    });
                }
            }
        }

        // Nothing found anywhere. The caller (CLI wait loop) uses its own
        // wait_timeout to decide when this becomes TaskLost (D14). Report
        // Pending with the three-tier probe results so ops can tell
        // "never arrived" apart from "dropped by cleanup".
        let none_msg = if third_tier_available {
            "not found in results/, status/, or commands/ (will become TaskLost at wait_timeout)"
                .to_string()
        } else {
            "not found in results/ or status/ (listing_style=none: D14 → TaskLost at wait_timeout)"
                .to_string()
        };
        Ok(TaskStatusResponse {
            task_id: *task_id,
            status: TaskStatus::Pending,
            message: Some(none_msg),
        })
    }

    fn get_result(&self, task_id: &Uuid) -> Result<TaskResult> {
        let url = self.object_url("results", &format!("{task_id}_result.json"));
        let resp = self.http_get(&url)?;
        let status = resp.status();
        if status == StatusCode::NOT_FOUND {
            return Err(BifrostError::TaskNotFound(*task_id));
        }
        let resp = self.check_response("get_result", resp)?;
        let body = resp
            .text()
            .map_err(|e| BifrostError::ExecutionError(format!("get_result: body read: {e}")))?;
        // Distinguish "404 page with body" (handled above) from real JSON.
        serde_json::from_str(&body).map_err(BifrostError::JsonError)
    }

    fn list_pending_tasks(&self) -> Result<Vec<Task>> {
        if self.cfg.listing_style != ListingStyle::RestSearch {
            return Err(BifrostError::ConfigInvalid(
                "list_pending_tasks requires listing_style=rest-search".into(),
            ));
        }
        let mut tasks: Vec<Task> = Vec::new();
        let mut continuation: Option<String> = None;
        let prefix_match = format!("{}commands/", self.repo_prefix);
        for _page in 0..MAX_SEARCH_PAGES {
            let url = self.search_url(continuation.as_deref());
            let resp = self.http_get(&url)?;
            let resp = self.check_response("search", resp)?;
            let page: SearchPage = resp
                .json()
                .map_err(|e| BifrostError::ExecutionError(format!("search parse: {e}")))?;

            let mut matched = 0;
            for item in page.items.iter() {
                let path = item.path.as_str();
                if !path.starts_with(&prefix_match) {
                    continue;
                }
                if !path.ends_with(".json") || path.ends_with(".tmp.json") {
                    continue;
                }
                matched += 1;
                let obj_url = format!(
                    "{}{}",
                    self.search_base
                        .replace("/service/rest/v1/search/assets", "/repository"),
                    // derive base from search_base origin + /repository/<repo>/<path>
                    // (we recompute below properly)
                    ""
                );
                // Rebuild the actual GET URL: <base_url>/<path>, where the
                // path returned by search is relative to the repo root.
                // We have self.cfg.base_url that ALREADY includes the repo
                // path (e.g. .../repository/tmp-raw/uploads). Search returns
                // "uploads/team-lx/.../commands/x.json" — so we strip the
                // `/uploads` tail from base_url and re-append path.
                // Actually simpler: derive the URL from origin + /repository/<repo>/<path>.
                let _ = obj_url; // silence unused from the abandoned line
                let get_url = format!(
                    "{}/repository/{}/{}",
                    origin_of(self.cfg.base_url.as_deref().unwrap_or_default()),
                    self.cfg.repository.as_deref().unwrap_or_default(),
                    path
                );
                let r = self.http_get(&get_url)?;
                let resp = self.check_response("read_task_blob", r)?;
                let body = resp
                    .text()
                    .map_err(|e| BifrostError::ExecutionError(format!("read blob: {e}")))?;
                let task: Task = serde_json::from_str(&body)?;
                tasks.push(task);
            }
            log::debug!(
                "search page: matched={} continuation={:?}",
                matched,
                page.continuation_token
            );
            match page.continuation_token {
                Some(tok) => continuation = Some(tok),
                None => break,
            }
        }
        Ok(tasks)
    }

    fn read_task(&self, task_id: &Uuid) -> Result<Task> {
        // Strategy: list_pending_tasks (which hits commands/ only) then
        // filter by id. With D11 sequential mode there's at most one task
        // in flight, so this is cheap.
        let tasks = self.list_pending_tasks()?;
        tasks
            .into_iter()
            .find(|t| t.task_id == *task_id)
            .ok_or(BifrostError::TaskNotFound(*task_id))
    }

    fn write_result(&self, task_id: &Uuid, result: &TaskResult) -> Result<()> {
        let body = serde_json::to_vec(result)?;
        let url = self.object_url("results", &format!("{task_id}_result.json"));
        let s = self.http_put(&url, body)?;
        if s.is_success() {
            Ok(())
        } else if matches!(s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            Err(BifrostError::AuthError(format!(
                "write_result: {}",
                s.as_u16()
            )))
        } else {
            Err(BifrostError::ExecutionError(format!(
                "write_result: {}",
                s.as_u16()
            )))
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
            timestamp: chrono::Utc::now().to_rfc3339(),
        };
        let body = serde_json::to_vec(&rec)?;
        let url = self.object_url("status", &format!("{task_id}.json"));
        let s = self.http_put(&url, body)?;
        if s.is_success() {
            Ok(())
        } else if matches!(s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            Err(BifrostError::AuthError(format!(
                "write_status: {}",
                s.as_u16()
            )))
        } else {
            Err(BifrostError::ExecutionError(format!(
                "write_status: {}",
                s.as_u16()
            )))
        }
    }

    fn remove_task(&self, task_id: &Uuid) -> Result<()> {
        let url = self.object_url("commands", &format!("{task_id}.json"));
        let status = self.http_delete(&url)?;
        match status {
            s if s.is_success() => Ok(()),
            StatusCode::NOT_FOUND => Ok(()), // already gone
            StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(BifrostError::AuthError(
                format!("remove_task: {} (check http.auth)", status.as_u16()),
            )),
            StatusCode::METHOD_NOT_ALLOWED => {
                // D3/D9: log-and-rely-on-ledger. Not silent.
                log::warn!(
                    "remove_task: METHOD_NOT_ALLOWED — backend lacks DELETE; \
                     idempotency relies on fetcher ledger (D9)"
                );
                Ok(())
            }
            s => Err(BifrostError::ExecutionError(format!(
                "remove_task: {}",
                s.as_u16()
            ))),
        }
    }

    fn cancel_task(&self, task_id: &Uuid) -> Result<()> {
        let rec = serde_json::json!({
            "task_id": task_id.to_string(),
            "timestamp": chrono::Utc::now().to_rfc3339(),
        });
        let url = self.object_url("cancel", &format!("{task_id}.json"));
        let s = self.http_put(&url, rec.to_string().into_bytes())?;
        if s.is_success() {
            Ok(())
        } else if matches!(s, StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN) {
            Err(BifrostError::AuthError(format!(
                "cancel_task: {}",
                s.as_u16()
            )))
        } else {
            Err(BifrostError::ExecutionError(format!(
                "cancel_task: {}",
                s.as_u16()
            )))
        }
    }

    /// HTTP Bridge override: preserve the `<ts>_<id>.json` filename
    /// returned by submit_task, so DELETE targets the exact PUTted
    /// resource. The default trait impl falls back to remove_task(id)
    /// which would DELETE a non-existent key and silently return Ok
    /// under the D3-404-ok rule.
    fn task_blob_name(&self, task: &Task) -> String {
        task_filename(task)
    }

    fn delete_task_blob(&self, subdir: &str, task: &Task) -> Result<()> {
        self.delete_path(subdir, &task_filename(task))
    }
}

impl HttpBridge {
    /// D10 third tier: search commands/ for `<task_id>` presence.
    /// Uses prefix-scoped listing then string-match on the filename.
    fn search_in_commands(&self, task_id: &Uuid) -> Result<bool> {
        let needle = task_id.to_string();
        let prefix_match = format!("{}commands/", self.repo_prefix);
        let mut continuation: Option<String> = None;
        for _page in 0..MAX_SEARCH_PAGES {
            let url = self.search_url(continuation.as_deref());
            let resp = self.http_get(&url)?;
            let resp = self.check_response("search_commands", resp)?;
            let page: SearchPage = resp
                .json()
                .map_err(|e| BifrostError::ExecutionError(format!("search parse: {e}")))?;
            for item in page.items.iter() {
                let p = item.path.as_str();
                if p.starts_with(&prefix_match) && p.contains(&needle) {
                    return Ok(true);
                }
            }
            match page.continuation_token {
                Some(tok) => continuation = Some(tok),
                None => break,
            }
        }
        Ok(false)
    }
}

// ─── helpers ─────────────────────────────────────────────────────────────

/// Canonically name the on-relay command blob for a task: `<ts>_<id>.json`.
/// The `<ts>_` prefix is why `read_task` can't just HEAD-guess a filename
/// (see §4.2 table).
fn task_filename(task: &Task) -> String {
    format!(
        "{}_{}.json",
        task.timestamp.format("%Y%m%d_%H%M%S"),
        task.task_id
    )
}

/// D13: normalize a user-supplied prefix to "<prefix>/". Trims whitespace,
/// strips leading/trailing slashes, collapses internal `//`, guarantees a
/// single trailing `/`. Returns "<p1>/<p2>/.../<pN>/" for non-empty input.
fn normalize_prefix(p: &str) -> String {
    let p = p.trim().trim_matches('/');
    if p.is_empty() {
        return String::new();
    }
    let collapsed = p
        .split('/')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("/");
    format!("{collapsed}/")
}

/// Extract the repo-relative path prefix from a Nexus Raw base_url.
/// The base_url format is `<scheme>://<host>/repository/<repo>[/<path>]`;
/// the `<path>` component is what a search response's `path` field begins
/// with when describing objects under that area.
///
/// Example: `https://host/repository/tmp-raw/uploads` → `"uploads/"`
/// Example: `https://host/repository/tmp-raw`         → `""`
fn extract_repo_base_prefix(base_url: &str) -> String {
    // Find `/repository/<repo>/` and take everything after the repo name.
    let after_repo = base_url
        .split_once("/repository/")
        .and_then(|(_, rest)| rest.split_once('/').map(|(_, tail)| tail))
        .unwrap_or("");
    let trimmed = after_repo.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}/")
    }
}

/// RFC 3986 unreserved characters: A-Z a-z 0-9 - _ . ~
/// Everything else gets percent-encoded UTF-8 bytes.
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Query params are form-style: + for space, percent for the rest.
fn encode_query_param(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{:02X}", b)),
        }
    }
    out
}

/// Extract "scheme://host[:port]" from a URL. We deliberately avoid the
/// `url` crate to keep dep weight down; this is forgiving enough for our
/// controlled inputs (we required `http://` or `https://` in `build()`).
fn origin_of(base_url: &str) -> String {
    let after_scheme = base_url
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or(base_url);
    let host = after_scheme.split('/').next().unwrap_or("");
    let scheme = base_url.split("://").next().unwrap_or("https");
    if host.is_empty() {
        base_url.trim_end_matches('/').to_string()
    } else {
        format!("{scheme}://{host}")
    }
}

/// Build the search base: `<origin>/service/rest/v1/search/assets`.
fn derive_search_base(base_url: &str) -> Result<String> {
    let origin = origin_of(base_url);
    if origin == base_url && !base_url.contains("://") {
        return Err(BifrostError::ConfigInvalid(format!(
            "http.base_url does not look like an absolute URL: {base_url}"
        )));
    }
    Ok(format!(
        "{}/service/rest/v1/search/assets",
        origin.trim_end_matches('/')
    ))
}

/// Minimal Nexus search response shape (we ignore most fields).
#[derive(Debug, Deserialize)]
struct SearchPage {
    #[serde(default)]
    items: Vec<SearchItem>,
    #[serde(rename = "continuationToken", default)]
    continuation_token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SearchItem {
    path: String,
}

/// Local sideband status record (written under `status/`).
#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct StatusRecord {
    task_id: String,
    status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    timestamp: String,
}

// ─── tests ───────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::TaskType;
    use crate::core::settings::{AuthKind, HttpAuthSection, HttpSection};

    fn valid_cfg() -> HttpSection {
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

    // ── constructor ─────────────────────────────────────────────
    #[test]
    fn constructor_builds_with_valid_cfg() {
        let b = HttpBridge::new_unchecked(&valid_cfg()).unwrap();
        assert_eq!(b.normalized_prefix, "team-lx/bifrost-http-bridge/");
        assert_eq!(
            b.object_base,
            "https://nexus.example.com/repository/tmp-raw/uploads/team-lx/bifrost-http-bridge/"
        );
        assert_eq!(
            b.search_base,
            "https://nexus.example.com/service/rest/v1/search/assets"
        );
        assert_eq!(b.listing_query, "bifrost-http-bridge");
    }

    #[test]
    fn constructor_rejects_non_https_production_url() {
        let mut cfg = valid_cfg();
        cfg.base_url = Some("http://evil.example.com/x".into());
        let err = HttpBridge::new(&cfg).unwrap_err();
        assert!(matches!(err, BifrostError::ConfigInvalid(_)));
    }

    #[test]
    fn constructor_allows_loopback_http() {
        let mut cfg = valid_cfg();
        cfg.base_url = Some("http://127.0.0.1:8081".into());
        assert!(HttpBridge::new(&cfg).is_ok());
    }

    #[test]
    fn constructor_uses_explicit_listing_query_when_set() {
        let mut cfg = valid_cfg();
        cfg.listing_query = Some("custom-token".into());
        let b = HttpBridge::new_unchecked(&cfg).unwrap();
        assert_eq!(b.listing_query, "custom-token");
    }

    // ── D13 prefix normalization ────────────────────────────────
    #[test]
    fn normalize_prefix_appends_trailing_slash() {
        assert_eq!(normalize_prefix("a/b"), "a/b/");
        assert_eq!(normalize_prefix("a/b/"), "a/b/");
        assert_eq!(normalize_prefix("/a/b/"), "a/b/");
        assert_eq!(normalize_prefix("a//b"), "a/b/");
        assert_eq!(normalize_prefix("  a/b  "), "a/b/");
    }

    // ── segment encoding ────────────────────────────────────────
    #[test]
    fn encode_segment_handles_rfc3986_unreserved() {
        assert_eq!(encode_segment("abc-DEF_123.~"), "abc-DEF_123.~");
        assert_eq!(encode_segment("hello world"), "hello%20world");
        assert_eq!(encode_segment("#tag"), "%23tag");
        assert_eq!(encode_segment(" plus "), "%20plus%20");
        assert_eq!(encode_segment("%pct"), "%25pct");
        assert_eq!(encode_segment("日本"), "%E6%97%A5%E6%9C%AC");
    }

    // ── object URL ──────────────────────────────────────────────
    #[test]
    fn object_url_concatenates_properly() {
        let b = HttpBridge::new_unchecked(&valid_cfg()).unwrap();
        assert_eq!(
            b.object_url("commands", "foo.json"),
            "https://nexus.example.com/repository/tmp-raw/uploads/team-lx/bifrost-http-bridge/commands/foo.json"
        );
        assert_eq!(
            b.object_url("results", "id with space_result.json"),
            "https://nexus.example.com/repository/tmp-raw/uploads/team-lx/bifrost-http-bridge/results/id%20with%20space_result.json"
        );
    }

    #[test]
    fn search_url_uses_repo_and_listing_query() {
        let b = HttpBridge::new_unchecked(&valid_cfg()).unwrap();
        let url = b.search_url(None);
        assert!(url.starts_with(
            "https://nexus.example.com/service/rest/v1/search/assets?repository=tmp-raw&q=bifrost-http-bridge"
        ));
        let url2 = b.search_url(Some("tok123"));
        assert!(url2.contains("&continuationToken=tok123"));
    }

    // ── derive_search_base on odd inputs ───────────────────────
    #[test]
    fn derive_search_base_strips_path() {
        assert_eq!(
            derive_search_base("https://example.com/repository/tmp-raw/uploads").unwrap(),
            "https://example.com/service/rest/v1/search/assets"
        );
        assert_eq!(
            derive_search_base("http://127.0.0.1:8081").unwrap(),
            "http://127.0.0.1:8081/service/rest/v1/search/assets"
        );
    }

    // ── task filename ───────────────────────────────────────────
    #[test]
    fn task_filename_includes_ts_and_id() {
        let mut t = Task::new("echo hi".into(), TaskType::Shell);
        let id = Uuid::nil();
        t.task_id = id;
        t.timestamp = chrono::DateTime::from_timestamp(1_700_000_000, 0).unwrap();
        let name = task_filename(&t);
        assert!(name.starts_with("20231114_"), "got {name}");
        assert!(name.ends_with(&format!("_{id}.json")));
    }

    // ── D13 boundary protection test: prefix eviction ──────────
    #[test]
    fn prefix_normalization_blocks_cross_prefix_match() {
        // "team-lx/sess" 会命中 "team-lx/session-evil";归一化后不会
        let cfg = HttpSection {
            prefix: Some("team-lx/sess".into()),
            ..valid_cfg()
        };
        let b = HttpBridge::new_unchecked(&cfg).unwrap();
        assert!(b.normalized_prefix.ends_with("sess/"));
        // 用归一化后的前缀作 starts_with, 不会误命中 evil
        let attacker = "team-lx/session-evil/commands/x.json";
        assert!(!attacker.starts_with(&b.normalized_prefix));
    }

    // ── mock HTTP server (std::net only, zero extra deps) ──────────
    //
    // Spawns a tiny http/1.0 responder on 127.0.0.1:<ephemeral>. Records
    // every request and dispatches canned responses by (method, path-prefix).
    // Enough to exercise the blocking client end-to-end without pulling
    // in axum/tiny_http.

    use std::io::{Read as IoRead, Write as IoWrite};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc as StdArc, Mutex as StdMutex};

    type Handler = Box<dyn Fn(&MockRequest) -> MockResponse + Send + Sync>;

    #[derive(Debug, Clone)]
    struct MockRequest {
        method: String,
        path: String,
        #[allow(dead_code)]
        headers: Vec<(String, String)>,
        body: Vec<u8>,
    }

    #[derive(Debug, Clone)]
    struct MockResponse {
        status: u16,
        body: Vec<u8>,
        content_type: String,
    }

    impl MockResponse {
        fn ok_json(body: &str) -> Self {
            Self {
                status: 200,
                body: body.as_bytes().to_vec(),
                content_type: "application/json".into(),
            }
        }
        fn created() -> Self {
            Self {
                status: 201,
                body: Vec::new(),
                content_type: "text/plain".into(),
            }
        }
        fn not_found() -> Self {
            Self {
                status: 404,
                body: b"<html>404 page</html>".to_vec(),
                content_type: "text/html".into(),
            }
        }
    }

    struct MockServer {
        addr: std::net::SocketAddr,
        requests: StdArc<StdMutex<Vec<MockRequest>>>,
        hits: StdArc<AtomicUsize>,
        _handle: std::thread::JoinHandle<()>,
    }

    impl MockServer {
        fn start(handler: Handler) -> Self {
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind mock");
            let addr = listener.local_addr().unwrap();
            let requests = StdArc::new(StdMutex::new(Vec::new()));
            let hits = StdArc::new(AtomicUsize::new(0));
            let req_clone = StdArc::clone(&requests);
            let hits_clone = StdArc::clone(&hits);
            let handle = std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut s) = stream else {
                        break;
                    };
                    let Some(request) = read_request(&mut s) else {
                        continue;
                    };
                    hits_clone.fetch_add(1, Ordering::SeqCst);
                    req_clone.lock().unwrap().push(request.clone());
                    let response = handler(&request);
                    write_response(&mut s, &response);
                    let _ = s.shutdown(std::net::Shutdown::Both);
                }
            });
            Self {
                addr,
                requests,
                hits,
                _handle: handle,
            }
        }
        #[allow(dead_code)]
        fn url(&self) -> String {
            format!("http://{}", self.addr)
        }
        #[allow(dead_code)]
        fn hits(&self) -> usize {
            self.hits.load(Ordering::SeqCst)
        }
        #[allow(dead_code)]
        fn requests(&self) -> Vec<MockRequest> {
            self.requests.lock().unwrap().clone()
        }
    }

    fn read_request(stream: &mut TcpStream) -> Option<MockRequest> {
        stream
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .ok()?;
        // Read headers + body. HTTP/1.0 style — no chunked.
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
                        if let Some(pos) = find_double_crlf(&buf) {
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
        let headers: Vec<(String, String)> = lines
            .filter_map(|l| {
                l.split_once(':')
                    .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
            })
            .collect();
        let body = buf[(he + 4)..].to_vec();
        Some(MockRequest {
            method,
            path,
            headers,
            body,
        })
    }

    fn find_double_crlf(buf: &[u8]) -> Option<usize> {
        buf.windows(4).position(|w| w == b"\r\n\r\n")
    }

    fn write_response(stream: &mut TcpStream, r: &MockResponse) {
        let status_text = match r.status {
            200 => "OK",
            201 => "Created",
            204 => "No Content",
            404 => "Not Found",
            405 => "Method Not Allowed",
            401 => "Unauthorized",
            500 => "Internal Server Error",
            _ => "Unknown",
        };
        let head = format!(
            "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            r.status,
            status_text,
            r.content_type,
            r.body.len()
        );
        let _ = stream.write_all(head.as_bytes());
        let _ = stream.write_all(&r.body);
    }

    // ── mock round-trip tests ────────────────────────────────────

    fn cfg_for_mock(mock: &MockServer, prefix: &str) -> HttpSection {
        HttpSection {
            // Use a Nexus-shaped URL so `extract_repo_base_prefix` yields
            // "uploads/" and search-result paths match the repo-relative
            // shape the real server returns (e.g.
            // `uploads/team-lx/test/commands/x.json`).
            base_url: Some(format!("{}/repository/tmp-raw/uploads", mock.url())),
            repository: Some("tmp-raw".into()),
            auth: Some(HttpAuthSection {
                kind: AuthKind::Basic,
                user: Some("alice".into()),
                password: Some("secret".into()),
            }),
            prefix: Some(prefix.into()),
            ..HttpSection::default()
        }
    }

    #[test]
    fn submit_task_puts_to_commands_with_basic_auth() {
        let mock = MockServer::start(Box::new(|req| {
            if req.method == "PUT" && req.path.contains("/commands/") {
                MockResponse::created()
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let task = Task::new("echo hi".into(), TaskType::Shell);
        b.submit_task(&task).expect("submit_task");

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 1);
        let r = &reqs[0];
        assert_eq!(r.method, "PUT");
        assert!(
            r.path.contains("/team-lx/test/commands/"),
            "path: {}",
            r.path
        );
        assert!(r.path.ends_with(".json"));
        // Basic auth header: alice:secret → YWxpY2U6c2VjcmV0
        let auth_header = r
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("Authorization"))
            .map(|(_, v)| v.clone())
            .expect("Authorization header present");
        assert_eq!(auth_header, "Basic YWxpY2U6c2VjcmV0");
        // body is the JSON-serialized task
        let body_str = String::from_utf8(r.body.clone()).unwrap();
        assert!(body_str.contains("\"command\":\"echo hi\""));
    }

    #[test]
    fn submit_task_atomic_upload_puts_tmp_then_real() {
        let mock = MockServer::start(Box::new(|req| {
            if req.method == "PUT" {
                MockResponse::created()
            } else {
                MockResponse::not_found()
            }
        }));
        let mut cfg = cfg_for_mock(&mock, "team-lx/test");
        cfg.atomic_upload = true;
        let b = HttpBridge::new_unchecked(&cfg).unwrap();
        let task = Task::new("echo hi".into(), TaskType::Shell);
        b.submit_task(&task).expect("submit_task");

        let reqs = mock.requests();
        assert_eq!(reqs.len(), 2);
        assert!(
            reqs[0].path.ends_with(".json.tmp"),
            "first PUT .tmp: {}",
            reqs[0].path
        );
        assert!(
            reqs[1].path.ends_with(".json"),
            "second PUT real: {}",
            reqs[1].path
        );
        assert!(!reqs[1].path.ends_with(".tmp"));
    }

    #[test]
    fn submit_task_propagates_401_as_auth_error() {
        let mock = MockServer::start(Box::new(|_req| MockResponse {
            status: 401,
            body: b"bad creds".to_vec(),
            content_type: "text/plain".into(),
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let task = Task::new("echo hi".into(), TaskType::Shell);
        let err = b.submit_task(&task).unwrap_err();
        assert!(matches!(err, BifrostError::AuthError(_)), "got {err}");
    }

    #[test]
    fn get_result_returns_task_result_when_present() {
        let result = TaskResult {
            task_id: Uuid::nil(),
            status: TaskStatus::Completed,
            output: crate::core::models::TaskOutput {
                stdout: "hello".into(),
                stderr: "".into(),
                exit_code: Some(0),
            },
            command: "echo hi".into(),
            start_time: chrono::Utc::now(),
            end_time: chrono::Utc::now(),
            duration_ms: 100,
            retries_used: 0,
            artifacts: Vec::new(),
            error_message: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let mock = MockServer::start(Box::new(move |req| {
            if req.method == "GET" && req.path.contains("/results/") {
                MockResponse::ok_json(&json)
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::nil();
        let got = b.get_result(&id).expect("get_result");
        assert_eq!(got.output.exit_code, Some(0));
        assert_eq!(got.output.stdout, "hello");
    }

    #[test]
    fn get_result_maps_404_to_task_not_found() {
        let mock = MockServer::start(Box::new(|_req| MockResponse::not_found()));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::new_v4();
        let err = b.get_result(&id).unwrap_err();
        assert!(matches!(err, BifrostError::TaskNotFound(_)), "got {err}");
    }

    #[test]
    fn remove_task_404_is_successful_noop() {
        // D3: 404 on DELETE means "already gone" → Ok
        let mock = MockServer::start(Box::new(|_req| MockResponse::not_found()));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::new_v4();
        b.remove_task(&id).expect("remove_task 404 → Ok");
    }

    #[test]
    fn remove_task_405_is_warning_but_ok() {
        // D3/D9: METHOD_NOT_ALLOWED → warn, rely on ledger; still Ok
        let mock = MockServer::start(Box::new(|_req| MockResponse {
            status: 405,
            body: Vec::new(),
            content_type: "text/plain".into(),
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::new_v4();
        b.remove_task(&id).expect("remove_task 405 → Ok (warned)");
    }

    #[test]
    fn remove_task_401_is_auth_error() {
        let mock = MockServer::start(Box::new(|_req| MockResponse {
            status: 401,
            body: Vec::new(),
            content_type: "text/plain".into(),
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let err = b.remove_task(&Uuid::new_v4()).unwrap_err();
        assert!(matches!(err, BifrostError::AuthError(_)));
    }

    #[test]
    fn cancel_task_puts_to_cancel_subdir() {
        let mock = MockServer::start(Box::new(|req| {
            if req.method == "PUT" && req.path.contains("/cancel/") {
                MockResponse::created()
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::new_v4();
        b.cancel_task(&id).expect("cancel_task");
        let reqs = mock.requests();
        assert!(reqs[0].path.contains("/team-lx/test/cancel/"));
    }

    #[test]
    fn query_status_completed_via_results_tier() {
        let result = TaskResult {
            task_id: Uuid::nil(),
            status: TaskStatus::Completed,
            output: crate::core::models::TaskOutput {
                stdout: "".into(),
                stderr: "".into(),
                exit_code: Some(0),
            },
            command: "ok".into(),
            start_time: chrono::Utc::now(),
            end_time: chrono::Utc::now(),
            duration_ms: 0,
            retries_used: 0,
            artifacts: Vec::new(),
            error_message: None,
        };
        let json = serde_json::to_string(&result).unwrap();
        let mock = MockServer::start(Box::new(move |req| {
            if req.path.contains("/results/") {
                MockResponse::ok_json(&json)
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::nil();
        let s = b.query_status(&id).expect("query_status");
        assert!(matches!(s.status, TaskStatus::Completed));
    }

    #[test]
    fn query_status_running_via_status_tier_when_no_result() {
        let status_rec = r#"{"task_id":"x","status":"Running","timestamp":"2026-11-06T00:00:00Z"}"#;
        let mock = MockServer::start(Box::new(move |req| {
            if req.path.contains("/results/") {
                MockResponse::not_found()
            } else if req.path.contains("/status/") {
                MockResponse::ok_json(status_rec)
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::nil();
        let s = b.query_status(&id).expect("query_status");
        assert!(matches!(s.status, TaskStatus::Running));
    }

    #[test]
    fn query_status_pending_when_everything_404s() {
        // All three tiers miss + search returns empty → Pending (will be
        // escalated to TaskLost by caller's wait_timeout per D10/D14)
        let mock = MockServer::start(Box::new(|req| {
            if req.path.contains("/service/rest/v1/search") {
                MockResponse::ok_json(r#"{"items":[],"continuationToken":null}"#)
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let id = Uuid::new_v4();
        let s = b.query_status(&id).expect("query_status");
        assert!(matches!(s.status, TaskStatus::Pending));
    }

    #[test]
    fn list_pending_tasks_filters_tmp_and_non_json_and_out_of_prefix() {
        // D8: must discard .tmp, non-.json, and anything outside the
        // normalized prefix. Also exercises pagination (2 pages).
        let page1 = r#"{
            "items":[
                {"path":"uploads/team-lx/test/commands/aaa_00000000-0000-0000-0000-000000000000.json"},
                {"path":"uploads/team-lx/test/commands/bbb.json.tmp"},
                {"path":"uploads/team-lx/test/commands/ccc.txt"},
                {"path":"uploads/other-person/commands/ddd.json"}
            ],
            "continuationToken":"page2"
        }"#;
        let page2 = r#"{"items":[],"continuationToken":null}"#;
        let mock = MockServer::start(Box::new(move |req| {
            if req.path.contains("/service/rest/v1/search") && req.path.contains("page2") {
                MockResponse::ok_json(page2)
            } else if req.path.contains("/service/rest/v1/search") {
                MockResponse::ok_json(page1)
            } else if req.path.contains("/commands/aaa_") {
                let task = Task::new("echo hi".into(), TaskType::Shell);
                MockResponse::ok_json(&serde_json::to_string(&task).unwrap())
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let tasks = b.list_pending_tasks().expect("list_pending_tasks");
        assert_eq!(tasks.len(), 1, "only aaa survives all filters");
    }

    #[test]
    fn list_pending_tasks_walks_multiple_pages_until_no_token() {
        // D8: pagination completeness. Target is on page 3 (out of 3);
        // an implementation that stops at page 1 fails this test.
        // Each page returns 1+ valid SearchItem objects (shape: {"path": "..."}).
        let mk_page = |paths_json_items: &str, next: Option<&str>| {
            let tok = next
                .map(|t| format!("\"{}\"", t))
                .unwrap_or_else(|| "null".into());
            format!(
                r#"{{"items":[{}],"continuationToken":{}}}"#,
                paths_json_items, tok
            )
        };
        let task = Task::new("echo hi".into(), TaskType::Shell);
        let task_json = serde_json::to_string(&task).unwrap();
        let page1 = mk_page(r#"{"path":"uploads/elsewhere/x.json"}"#, Some("tokA"));
        let page2 = mk_page(r#"{"path":"uploads/elsewhere/y.json"}"#, Some("tokB"));
        let page3 = mk_page(
            &format!(
                r#"{{"path":"uploads/team-lx/test/commands/zzz_{}.json"}}"#,
                Uuid::nil()
            ),
            None,
        );
        let mock = MockServer::start(Box::new(move |req| {
            if req.path.contains("/service/rest/v1/search") {
                if req.path.contains("tokB") {
                    MockResponse::ok_json(&page3)
                } else if req.path.contains("tokA") {
                    MockResponse::ok_json(&page2)
                } else {
                    MockResponse::ok_json(&page1)
                }
            } else if req.path.contains("/commands/zzz_") {
                MockResponse::ok_json(&task_json)
            } else {
                MockResponse::not_found()
            }
        }));
        let b = HttpBridge::new_unchecked(&cfg_for_mock(&mock, "team-lx/test")).unwrap();
        let tasks = b.list_pending_tasks().expect("list");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].command, "echo hi");
    }
}
