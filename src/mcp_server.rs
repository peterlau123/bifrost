// MCP (Model Context Protocol) server for bifrost
//
// Exposes bifrost as structured tools so agents can submit tasks to the
// offline H20 node reliably: schema-validated args, structured errors,
// and a health check that prevents the classic "submitted before server
// was ready → task lost" failure mode.
//
// Tools:
//   bifrost_submit  - submit a command task (returns task_id)
//   bifrost_status  - query task status
//   bifrost_result  - fetch full task result (stdout/stderr/duration)
//   bifrost_health  - check daemon heartbeat freshness (call before submit)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::SystemTime;

use rmcp::{
    handler::server::router::tool::ToolRouter, handler::server::wrapper::Parameters, serve_server,
    tool, tool_handler, tool_router, transport::stdio, ServerHandler,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::core::bridge::Bridge;
use crate::core::models::TaskType;
use crate::core::protocol::Protocol;
use crate::core::settings::BifrostSettings;

/// MCP server state: shared storage path + settings
#[derive(Debug, Clone)]
pub struct McpServer {
    tool_router: ToolRouter<Self>,
    settings: Arc<BifrostSettings>,
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for McpServer {}

impl McpServer {
    /// Create a new MCP server from settings
    pub fn new(settings: BifrostSettings) -> Self {
        Self {
            tool_router: Self::tool_router(),
            settings: Arc::new(settings),
        }
    }

    fn bridge(&self) -> Protocol {
        Protocol::new(self.settings.shared_storage.clone()).expect("failed to create bridge")
    }

    /// Server's own process group id is never negative; -pgid kills the group
    fn heartbeat_age_secs(&self) -> Option<u64> {
        let hb = self.settings.shared_storage.join("heartbeat.json");
        let meta = std::fs::metadata(&hb).ok()?;
        let modified = meta.modified().ok()?;
        let age = SystemTime::now().duration_since(modified).ok()?.as_secs();
        Some(age)
    }
}

#[tool_router(router = tool_router)]
impl McpServer {
    /// Submit a shell command as a task to the offline node. Returns the task_id.
    /// Example command: "sh -c 'echo hello'" (shell features need sh -c wrapping).
    #[tool(
        name = "bifrost_submit",
        description = "Submit a command task to the offline H20 node. Returns task_id as JSON. Complex commands (redirects, &&, $VAR, background &) must be wrapped in sh -c '...'."
    )]
    pub async fn submit(
        &self,
        Parameters(req): Parameters<SubmitRequest>,
    ) -> Result<String, String> {
        let bridge: &dyn Bridge = &self.bridge();
        let task_type = if req.command.trim_start().starts_with("pytest") {
            TaskType::Pytest
        } else {
            TaskType::Shell
        };
        let working_dir = req.working_dir.map(PathBuf::from);
        let task_id = crate::client::submit::submit_task(
            bridge,
            req.command,
            task_type,
            req.priority.unwrap_or(0),
            req.timeout.unwrap_or(300),
            working_dir,
        )
        .map_err(|e| format!("submit failed: {}", e))?;
        Ok(serde_json::json!({ "task_id": task_id.to_string(), "status": "Pending" }).to_string())
    }

    /// Query the status of a task by its task_id. Returns status + message.
    #[tool(
        name = "bifrost_status",
        description = "Query task status by task_id. Returns JSON with status (Pending/Running/Completed/Failed/Timeout) and message."
    )]
    pub async fn status(
        &self,
        Parameters(req): Parameters<StatusRequest>,
    ) -> Result<String, String> {
        use uuid::Uuid;
        let tid = Uuid::parse_str(&req.task_id).map_err(|e| format!("invalid task_id: {}", e))?;
        let bridge: &dyn Bridge = &self.bridge();
        let resp = crate::client::status::query_status(bridge, tid)
            .map_err(|e| format!("status query failed: {}", e))?;
        Ok(serde_json::json!({
            "task_id": req.task_id,
            "status": format!("{}", resp.status),
            "message": resp.message,
        })
        .to_string())
    }

    /// Fetch the full result of a completed/failed/timed-out task.
    /// Returns stdout, stderr, exit_code, duration_ms, error_message.
    #[tool(
        name = "bifrost_result",
        description = "Fetch full task result by task_id. Returns JSON with status, stdout, stderr, exit_code, duration_ms, error_message. Call after status shows a terminal state (Completed/Failed/Timeout)."
    )]
    pub async fn result(
        &self,
        Parameters(req): Parameters<StatusRequest>,
    ) -> Result<String, String> {
        use uuid::Uuid;
        let tid = Uuid::parse_str(&req.task_id).map_err(|e| format!("invalid task_id: {}", e))?;
        let bridge: &dyn Bridge = &self.bridge();
        let r = crate::client::results::get_result(bridge, tid)
            .map_err(|e| format!("result fetch failed: {}", e))?;
        Ok(serde_json::json!({
            "task_id": r.task_id.to_string(),
            "status": format!("{}", r.status),
            "exit_code": r.output.exit_code,
            "stdout": r.output.stdout,
            "stderr": r.output.stderr,
            "duration_ms": r.duration_ms(),
            "error_message": r.error_message,
        })
        .to_string())
    }

    /// Check whether the daemon (server) on the offline node is alive.
    /// Call this BEFORE submitting tasks: if not alive, submissions are
    /// written but never consumed (inotify does not scan pre-existing files).
    /// Returns JSON: {alive: bool, heartbeat_age_secs, heartbeat_timeout_secs}.
    #[tool(
        name = "bifrost_health",
        description = "Check daemon heartbeat freshness. Call before submitting tasks. Returns JSON: alive (bool), heartbeat_age_secs, heartbeat_timeout_secs. If alive=false, the server is down or stale - do not submit."
    )]
    pub async fn health(&self) -> Result<String, String> {
        let timeout_secs = self
            .settings
            .client
            .heartbeat_timeout
            .unwrap_or(std::time::Duration::from_secs(180))
            .as_secs();
        let age = self.heartbeat_age_secs();
        let alive = age.map(|a| a < timeout_secs).unwrap_or(false);
        Ok(serde_json::json!({
            "alive": alive,
            "heartbeat_age_secs": age,
            "heartbeat_timeout_secs": timeout_secs,
        })
        .to_string())
    }
}

/// Run the MCP server over stdio (blocking). Hermes connects via:
/// `hermes mcp add bifrost --command <bifrost> --args mcp-serve`
pub async fn run(settings: BifrostSettings) -> Result<(), String> {
    let server = McpServer::new(settings);
    eprintln!("bifrost MCP server ready (stdio)");
    let running = serve_server(server, stdio())
        .await
        .map_err(|e| format!("MCP serve failed: {}", e))?;
    let _ = running.waiting().await;
    Ok(())
}

// ── Tool request/response schemas ─────────────────────────────────────

/// Submit request
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct SubmitRequest {
    /// Shell command to execute (use sh -c '...' for shell features)
    command: String,
    /// Timeout in seconds (default 300)
    #[serde(default)]
    timeout: Option<u64>,
    /// Priority 0-255, lower is higher (default 0)
    #[serde(default)]
    priority: Option<u8>,
    /// Working directory on the target node (default: daemon's cwd)
    #[serde(default)]
    working_dir: Option<String>,
}

/// Status/result request (shared shape)
#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct StatusRequest {
    /// Task UUID (from bifrost_submit)
    task_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_settings(tmp: &TempDir) -> BifrostSettings {
        BifrostSettings {
            shared_storage: tmp.path().to_path_buf(),
            client: crate::core::settings::ClientSection {
                poll_interval: None,
                heartbeat_timeout: Some(std::time::Duration::from_secs(180)),
            },
            daemon: crate::core::settings::DaemonSection::default(),
        }
    }

    #[tokio::test]
    async fn test_health_no_heartbeat() {
        let tmp = TempDir::new().unwrap();
        let srv = McpServer::new(test_settings(&tmp));
        let out = srv.health().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["alive"], false, "无 heartbeat 文件时 alive 必须为 false");
        assert_eq!(v["heartbeat_timeout_secs"], 180);
    }

    #[tokio::test]
    async fn test_health_fresh_heartbeat() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("heartbeat.json"), "{}").unwrap();
        let srv = McpServer::new(test_settings(&tmp));
        let out = srv.health().await.unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["alive"], true, "新鲜心跳时 alive 必须为 true");
    }

    #[tokio::test]
    async fn test_submit_returns_task_id() {
        let tmp = TempDir::new().unwrap();
        let srv = McpServer::new(test_settings(&tmp));
        let out = srv
            .submit(Parameters(SubmitRequest {
                command: "echo mcp-test".into(),
                timeout: Some(30),
                priority: None,
                working_dir: None,
            }))
            .await
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(v["status"], "Pending");
        let tid = v["task_id"].as_str().unwrap();
        assert!(
            uuid::Uuid::parse_str(tid).is_ok(),
            "task_id 必须是合法 UUID"
        );
        // 任务文件必须落盘
        let files: Vec<_> = std::fs::read_dir(tmp.path().join("commands"))
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().map(|x| x == "json").unwrap_or(false))
            .collect();
        assert_eq!(files.len(), 1);
    }

    #[tokio::test]
    async fn test_status_not_found_error() {
        let tmp = TempDir::new().unwrap();
        let srv = McpServer::new(test_settings(&tmp));
        let err = srv
            .status(Parameters(StatusRequest {
                task_id: uuid::Uuid::new_v4().to_string(),
            }))
            .await
            .unwrap_err();
        assert!(
            err.contains("status query failed"),
            "任务不存在应返回结构化错误, got: {}",
            err
        );
    }

    #[tokio::test]
    async fn test_status_invalid_uuid() {
        let tmp = TempDir::new().unwrap();
        let srv = McpServer::new(test_settings(&tmp));
        let err = srv
            .status(Parameters(StatusRequest {
                task_id: "not-a-uuid".into(),
            }))
            .await
            .unwrap_err();
        assert!(
            err.contains("invalid task_id"),
            "非法 UUID 应报参数错误, got: {}",
            err
        );
    }
}
