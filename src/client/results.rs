// Client results retrieval functionality
use crate::core::bridge::Bridge;
use crate::core::error::{BifrostError, Result};
use crate::core::models::TaskResult;
use crate::core::protocol::Protocol;
use std::fs;
use std::path::PathBuf;
use uuid::Uuid;

/// Result response formats
#[derive(Debug)]
pub enum ResultFormat {
    /// JSON format (default)
    Json,
    /// YAML format
    Yaml,
    /// Text format (human-readable)
    Text,
}

/// Retrieve task results via the bridge
pub fn get_result(bridge: &dyn Bridge, task_id: Uuid) -> Result<TaskResult> {
    bridge.get_result(&task_id)
}

/// Get result in formatted output (shared-storage specific)
pub fn get_result_formatted(
    protocol: &Protocol,
    task_id: Uuid,
    format: ResultFormat,
) -> Result<String> {
    let result = get_result(protocol, task_id)?;

    match format {
        ResultFormat::Json => serde_json::to_string_pretty(&result)
            .map_err(|e| BifrostError::SerializationError(e.to_string())),
        ResultFormat::Yaml => {
            // YAML format (simplified - using debug format for now)
            Ok(format!("{:#?}", result))
        }
        ResultFormat::Text => format_result_text(&result),
    }
}

/// Format result as human-readable text
fn format_result_text(result: &TaskResult) -> Result<String> {
    let mut output = String::new();

    output.push_str(&format!("Task ID: {}\n", result.task_id));
    output.push_str(&format!("Status: {}\n", result.status));
    output.push_str(&format!("Duration: {} seconds\n", result.duration_secs()));
    output.push_str(&format!("Retries: {}\n", result.retries_used));

    if let Some(code) = result.output.exit_code {
        output.push_str(&format!("Exit Code: {}\n", code));
    }

    if !result.output.stdout.is_empty() {
        output.push_str("\n--- STDOUT ---\n");
        output.push_str(&result.output.stdout);
        output.push('\n');
    }

    if !result.output.stderr.is_empty() {
        output.push_str("\n--- STDERR ---\n");
        output.push_str(&result.output.stderr);
        output.push('\n');
    }

    if !result.artifacts.is_empty() {
        output.push_str("\n--- ARTIFACTS ---\n");
        for artifact in &result.artifacts {
            output.push_str(&format!("{}\n", artifact));
        }
    }

    if let Some(error) = &result.error_message {
        output.push_str(&format!("\nError: {}\n", error));
    }

    Ok(output)
}

/// Get artifact file path (with path traversal protection)
pub fn get_artifact_path(
    protocol: &Protocol,
    task_id: Uuid,
    artifact_name: &str,
) -> Result<PathBuf> {
    // Validate artifact_name doesn't contain path separators, traversal sequences, or null bytes
    if artifact_name.contains('/')
        || artifact_name.contains('\\')
        || artifact_name.contains("..")
        || artifact_name.contains('\0')
        || artifact_name.is_empty()
    {
        return Err(BifrostError::ConfigInvalid(
            "Invalid artifact name: contains path separators, traversal sequences, or null bytes"
                .to_string(),
        ));
    }

    let shared_storage = protocol.shared_storage();
    let artifacts_dir = shared_storage.join("artifacts");

    let artifact_path = artifacts_dir.join(format!("{}_{}", task_id, artifact_name));

    if !artifact_path.exists() {
        return Err(BifrostError::IoError(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("Artifact not found: {}", artifact_name),
        )));
    }

    // Effective path traversal check: canonicalize both paths and verify containment
    let canonical_artifact = artifact_path
        .canonicalize()
        .map_err(BifrostError::IoError)?;
    let canonical_dir = artifacts_dir
        .canonicalize()
        .map_err(BifrostError::IoError)?;

    if !canonical_artifact.starts_with(&canonical_dir) {
        return Err(BifrostError::ConfigInvalid(
            "Artifact path traversal detected".to_string(),
        ));
    }

    Ok(artifact_path)
}

/// Read artifact content
pub fn read_artifact(protocol: &Protocol, task_id: Uuid, artifact_name: &str) -> Result<String> {
    let artifact_path = get_artifact_path(protocol, task_id, artifact_name)?;

    fs::read_to_string(&artifact_path).map_err(BifrostError::IoError)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::{TaskOutput, TaskResult, TaskStatus};
    use chrono::Utc;
    use tempfile::TempDir;

    fn create_test_result(task_id: Uuid) -> TaskResult {
        TaskResult {
            task_id,
            status: TaskStatus::Completed,
            output: TaskOutput {
                stdout: "test output".to_string(),
                stderr: "".to_string(),
                exit_code: Some(0),
            },
            start_time: Utc::now(),
            end_time: Utc::now(),
            duration_ms: 0,
            retries_used: 0,
            artifacts: vec!["report.json".to_string()],
            error_message: None,
        }
    }

    #[test]
    fn test_get_result() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        // Create a result file
        let results_dir = temp_dir.path().join("results");
        fs::create_dir_all(&results_dir).unwrap();

        let task_id = Uuid::new_v4();
        let result = create_test_result(task_id);

        let result_file = results_dir.join(format!("{}_result.json", task_id));
        fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

        // Retrieve result
        let retrieved = get_result(&protocol, task_id).unwrap();

        assert_eq!(retrieved.task_id, task_id);
        assert_eq!(retrieved.status, TaskStatus::Completed);
        assert_eq!(retrieved.output.stdout, "test output");
    }

    #[test]
    fn test_get_result_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let fake_id = Uuid::new_v4();
        let result = get_result(&protocol, fake_id);

        assert!(result.is_err());
    }

    #[test]
    fn test_get_result_formatted_json() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let results_dir = temp_dir.path().join("results");
        fs::create_dir_all(&results_dir).unwrap();

        let task_id = Uuid::new_v4();
        let result = create_test_result(task_id);

        let result_file = results_dir.join(format!("{}_result.json", task_id));
        fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

        let formatted = get_result_formatted(&protocol, task_id, ResultFormat::Json).unwrap();

        assert!(formatted.contains("task_id"));
        assert!(formatted.contains("test output"));
    }

    #[test]
    fn test_get_result_formatted_text() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let results_dir = temp_dir.path().join("results");
        fs::create_dir_all(&results_dir).unwrap();

        let task_id = Uuid::new_v4();
        let result = create_test_result(task_id);

        let result_file = results_dir.join(format!("{}_result.json", task_id));
        fs::write(&result_file, serde_json::to_string_pretty(&result).unwrap()).unwrap();

        let formatted = get_result_formatted(&protocol, task_id, ResultFormat::Text).unwrap();

        assert!(formatted.contains("Task ID:"));
        assert!(formatted.contains("Status: Completed"));
        assert!(formatted.contains("STDOUT"));
        assert!(formatted.contains("test output"));
    }

    #[test]
    fn test_format_result_text() {
        let task_id = Uuid::new_v4();
        let result = create_test_result(task_id);

        let text = format_result_text(&result).unwrap();

        assert!(text.contains(&task_id.to_string()));
        assert!(text.contains("Completed"));
        assert!(text.contains("test output"));
        assert!(text.contains("STDOUT"));
        assert!(text.contains("report.json"));
        assert!(text.contains("ARTIFACTS"));
    }

    #[test]
    fn test_read_artifact() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        // Create artifact directory and file
        let artifacts_dir = temp_dir.path().join("artifacts");
        fs::create_dir_all(&artifacts_dir).unwrap();

        let task_id = Uuid::new_v4();
        let artifact_content = "{\"tests\": []}";
        let artifact_file = artifacts_dir.join(format!("{}_report.json", task_id));
        fs::write(&artifact_file, artifact_content).unwrap();

        // Read artifact
        let content = read_artifact(&protocol, task_id, "report.json").unwrap();
        assert_eq!(content, artifact_content);
    }

    #[test]
    fn test_read_artifact_not_found() {
        let temp_dir = TempDir::new().unwrap();
        let protocol = Protocol::new(temp_dir.path().to_path_buf()).unwrap();

        let task_id = Uuid::new_v4();
        let result = read_artifact(&protocol, task_id, "missing.json");

        assert!(result.is_err());
    }
}
