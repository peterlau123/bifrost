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
    fn remove_task(&self, task_id: &Uuid) -> Result<()>;
}
