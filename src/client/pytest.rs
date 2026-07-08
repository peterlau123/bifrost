// Pytest convenience command builder
use crate::core::models::{Task, TaskType};
use std::path::PathBuf;

/// Build a pytest command with JSON report flag
/// Adds --json-report and --json-report-file automatically
pub fn build_pytest_command(test_path: &str) -> String {
    format!(
        "pytest {} --json-report --json-report-file=report.json -v",
        test_path
    )
}

/// Create a pytest Task from test path
pub fn create_pytest_task(
    test_path: &str,
    priority: u8,
    timeout: u64,
    working_dir: Option<PathBuf>,
) -> Task {
    let command = build_pytest_command(test_path);

    let task = Task::new(command, TaskType::Pytest)
        .with_priority(priority)
        .with_timeout(timeout)
        .with_artifact("report.json".to_string());

    // Set working directory if provided
    if let Some(wd) = working_dir {
        task.with_working_dir(wd)
    } else {
        task
    }
}

/// Parse pytest JSON report and extract summary
/// Returns a summary string with passed/failed/total counts
pub fn parse_pytest_summary(report_content: &str) -> Result<String, String> {
    use serde_json::Value;

    let report: Value = serde_json::from_str(report_content)
        .map_err(|e| format!("Failed to parse JSON report: {}", e))?;

    let summary = report
        .get("summary")
        .ok_or_else(|| "No summary field in report".to_string())?;

    let passed = summary.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);

    let failed = summary.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);

    let total = summary.get("total").and_then(|v| v.as_u64()).unwrap_or(0);

    let duration = summary
        .get("duration")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);

    Ok(format!(
        "Tests: {} passed, {} failed, {} total ({}s)",
        passed, failed, total, duration
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_pytest_command() {
        let cmd = build_pytest_command("tests/");
        assert!(cmd.contains("pytest tests/"));
        assert!(cmd.contains("--json-report"));
        assert!(cmd.contains("--json-report-file=report.json"));
        assert!(cmd.contains("-v"));
    }

    #[test]
    fn test_create_pytest_task() {
        let task = create_pytest_task("tests/unit/", 10, 600, None);

        assert_eq!(task.task_type, TaskType::Pytest);
        assert_eq!(task.priority, 10);
        assert_eq!(task.timeout, 600);
        assert!(task.command.contains("pytest tests/unit/"));
        assert!(task.artifacts_expected.contains(&"report.json".to_string()));
    }

    #[test]
    fn test_create_pytest_task_with_working_dir() {
        let task = create_pytest_task("tests/", 5, 300, Some(PathBuf::from("/workspace/project")));

        assert_eq!(task.working_dir, PathBuf::from("/workspace/project"));
    }

    #[test]
    fn test_parse_pytest_summary() {
        let report = r#"{
            "summary": {
                "passed": 10,
                "failed": 2,
                "total": 12,
                "duration": 5.5
            }
        }"#;

        let summary = parse_pytest_summary(report).unwrap();
        assert!(summary.contains("10 passed"));
        assert!(summary.contains("2 failed"));
        assert!(summary.contains("12 total"));
        assert!(summary.contains("5.5s"));
    }

    #[test]
    fn test_parse_pytest_summary_invalid_json() {
        let result = parse_pytest_summary("invalid json");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Failed to parse JSON report"));
    }

    #[test]
    fn test_parse_pytest_summary_missing_summary() {
        let report = r#"{"tests": []}"#;
        let result = parse_pytest_summary(report);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No summary field"));
    }
}
