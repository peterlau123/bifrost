// Bridge abstraction - transport-agnostic task communication
//
// A Bridge connects the source machine (client) and the target machine (server).
// The same trait is used by both sides; which methods each side calls depends on
// its role:
//   - Source (client): submit_task, query_status, get_result
//   - Target (server): list_pending_tasks, read_task, write_result, write_status, remove_task
//
// Currently only SharedStorage is implemented (file-based, bidirectional directories).
// Future implementations could include SSH, message queues, etc.

use uuid::Uuid;

use crate::core::error::{BifrostError, Result};
use crate::core::models::{Task, TaskResult, TaskStatus};
use crate::core::settings::BifrostSettings;

/// Status of a task, returned by [`Bridge::query_status`]
#[derive(Debug, Clone)]
pub struct TaskStatusResponse {
    pub task_id: Uuid,
    pub status: TaskStatus,
    pub message: Option<String>,
}

/// Create the bridge configured by the given settings.
///
/// - `transport: "shared"` (default) → shared-storage [`Protocol`]
/// - `transport: "ssh"` → [`SshBridge`] reaching the target over SSH
///
/// The daemon on the target machine always uses the shared-storage
/// bridge (it operates on the local directories); the SSH bridge is for
/// clients that have no shared filesystem with the target but can SSH.
pub fn create_bridge(settings: &BifrostSettings) -> Result<Box<dyn Bridge>> {
    if settings.transport.is_ssh() {
        let ssh = settings.ssh.as_ref().ok_or_else(|| {
            BifrostError::ConfigInvalid("transport=ssh but no ssh section".into())
        })?;
        Ok(Box::new(crate::core::ssh_bridge::SshBridge::new(ssh)?))
    } else if settings.transport.is_http() {
        let http = settings.http.as_ref().ok_or_else(|| {
            BifrostError::ConfigInvalid("transport=http but no http section".into())
        })?;
        Ok(Box::new(crate::core::http_bridge::HttpBridge::new(http)?))
    } else {
        Ok(Box::new(crate::core::protocol::Protocol::new(
            settings.shared_storage.clone(),
        )?))
    }
}

/// Transport-agnostic bridge between source and target machines.
///
/// Implementations provide the communication channel for submitting tasks,
/// querying their status, and retrieving results. The transport may be a
/// shared filesystem, an SSH connection, a message bus, or anything else
/// that can carry these operations.
pub trait Bridge: Send + Sync {
    // ── Source side (client) ──────────────────────────────────────

    /// Submit a task for execution on the target machine.
    fn submit_task(&self, task: &Task) -> Result<()>;

    /// Query the current status of a task.
    fn query_status(&self, task_id: &Uuid) -> Result<TaskStatusResponse>;

    /// Retrieve the result of a completed task.
    fn get_result(&self, task_id: &Uuid) -> Result<TaskResult>;

    // ── Target side (server) ──────────────────────────────────────

    /// List tasks waiting to be executed.
    fn list_pending_tasks(&self) -> Result<Vec<Task>>;

    /// Read a single task by ID.
    fn read_task(&self, task_id: &Uuid) -> Result<Task>;

    /// Write the result of a completed task.
    fn write_result(&self, task_id: &Uuid, result: &TaskResult) -> Result<()>;

    /// Update the status of a task.
    fn write_status(
        &self,
        task_id: &Uuid,
        status: &TaskStatus,
        message: Option<&str>,
    ) -> Result<()>;

    /// Remove a task after it has been processed.
    ///
    /// **HTTP transports**: prefer [`Bridge::delete_task_blob`] which takes the
    /// full remote filename (including any timestamp prefix). Some HTTP
    /// implementations encode additional data in the command filename
    /// (e.g. a `<ts>_` prefix preserved from `submit_task`), and an id-only
    /// URL misses the actual object — Nexus returns 404, which D3 treats
    /// as "already gone" — producing a silent no-op rather than the cleanup
    /// the caller expected. (Lesson learned Day 5; see
    /// docs/design/http-bridge-review.md §议题 1.)
    fn remove_task(&self, task_id: &Uuid) -> Result<()>;

    /// Remote-relative filename for a command blob. Default impl yields
    /// `<task_id>.json` (SSH transport convention). Override when your
    /// transport encodes additional info into the filename (e.g. HTTP
    /// transport's `<ts>_<id>.json` from `submit_task`). Caller passes
    /// the result to [`Bridge::delete_task_blob`] so DELETE hits the
    /// exact PUT destination.
    fn task_blob_name(&self, task: &Task) -> String {
        format!("{}.json", task.task_id)
    }

    /// Delete a command blob at `<subdir>/<task_blob_name(task)>`.
    /// Default impl delegates to [`Bridge::remove_task`] which conflates
    /// blob name with bare task id and is therefore transport-agnostic
    /// only when `task_blob_name` is also overridden to be a pure
    /// `<id>.json`. Transports like HttpBridge override this with a
    /// direct DELETE on the filename produced by their overridden
    /// `task_blob_name`.
    fn delete_task_blob(&self, subdir: &str, task: &Task) -> Result<()> {
        let _ = subdir; // subclasses that care use it; trait-default keeps SSH convention (subdir ignored).
        self.remove_task(&task.task_id)
    }

    // ── Both sides ────────────────────────────────────────────────

    /// Request cancellation of a task. Transports that cannot signal the
    /// target machine return an error rather than pretending it worked.
    fn cancel_task(&self, task_id: &Uuid) -> Result<()> {
        let _ = task_id;
        Err(BifrostError::ConfigInvalid(
            "task cancellation is not supported by this transport".into(),
        ))
    }
}
