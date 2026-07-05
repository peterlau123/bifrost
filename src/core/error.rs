// Error types for bifrost
use thiserror::Error;
use uuid::Uuid;
use std::io;

/// Bifrost error types
#[derive(Debug, Error)]
pub enum BifrostError {
    /// Task not found in queue
    #[error("Task not found: {0}")]
    TaskNotFound(Uuid),

    /// File lock operation failed
    #[error("File lock failed: {0}")]
    LockError(#[from] fs2::LockError),

    /// JSON parsing/serialization error
    #[error("JSON parse error: {0}")]
    JsonError(#[from] serde_json::Error),

    /// Command execution failed
    #[error("Command execution failed: {0}")]
    ExecutionError(String),

    /// Heartbeat timeout detected
    #[error("Heartbeat timeout for task {0}")]
    HeartbeatTimeout(Uuid),

    /// Task execution timeout
    #[error("Task timeout after {0} seconds")]
    TaskTimeout(u64),

    /// Configuration file not found
    #[error("Config file not found: {0}")]
    ConfigNotFound(String),

    /// Invalid configuration
    #[error("Invalid configuration: {0}")]
    ConfigInvalid(String),

    /// I/O error
    #[error("I/O error: {0}")]
    IoError(#[from] io::Error),

    /// YAML parsing error
    #[error("YAML parse error: {0}")]
    YamlError(#[from] serde_yaml::Error),

    /// Serialization error
    #[error("Serialization error: {0}")]
    SerializationError(String),

    /// Queue operation failed
    #[error("Queue operation failed: {0}")]
    QueueError(String),

    /// Artifact not found
    #[error("Artifact not found: {0}")]
    ArtifactNotFound(String),

    /// Process spawn failed
    #[error("Failed to spawn process: {0}")]
    ProcessSpawnError(String),

    /// Maximum retries exceeded
    #[error("Maximum retries exceeded for task {0}")]
    MaxRetriesExceeded(Uuid),

    /// Task already exists
    #[error("Task already exists: {0}")]
    TaskAlreadyExists(Uuid),

    /// Invalid task status transition
    #[error("Invalid status transition from {from} to {to}")]
    InvalidStatusTransition {
        from: String,
        to: String,
    },
}

/// Result type alias for Bifrost operations
pub type Result<T> = std::result::Result<T, BifrostError>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let id = Uuid::new_v4();
        let error = BifrostError::TaskNotFound(id);
        assert!(error.to_string().contains("Task not found"));
    }

    #[test]
    fn test_error_from_io() {
        let io_error = io::Error::new(io::ErrorKind::NotFound, "file missing");
        let bifrost_error: BifrostError = io_error.into();
        assert!(matches!(bifrost_error, BifrostError::IoError(_)));
    }

    #[test]
    fn test_result_type() {
        fn returns_result() -> Result<String> {
            Ok("success".to_string())
        }

        assert!(returns_result().is_ok());
    }
}