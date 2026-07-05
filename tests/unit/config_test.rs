// Configuration parsing tests
use bifrost::core::config::{ClientConfig, DaemonConfig};
use std::path::PathBuf;
use std::time::Duration;
use tempfile::TempDir;

fn create_test_client_yaml(temp_dir: &TempDir) -> PathBuf {
    let yaml_content = r#"
shared_storage: "/tmp/bifrost"
database: "tasks.db"
poll_interval: "2s"
heartbeat_timeout: "180s"
"#;
    let config_path = temp_dir.path().join("client.yaml");
    std::fs::write(&config_path, yaml_content).unwrap();
    config_path
}

fn create_test_daemon_yaml(temp_dir: &TempDir) -> PathBuf {
    let yaml_content = r#"
shared_storage: "/tmp/bifrost"
poll_interval: "500ms"
task_timeout: "300s"
max_retries: 3
heartbeat_interval: "60s"
max_concurrent: 10
working_dir: "/tmp/bifrost/work"
"#;
    let config_path = temp_dir.path().join("daemon.yaml");
    std::fs::write(&config_path, yaml_content).unwrap();
    config_path
}

#[test]
fn test_load_client_config() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = create_test_client_yaml(&temp_dir);

    let config = ClientConfig::from_yaml(config_path).unwrap();
    assert_eq!(config.poll_interval, Duration::from_secs(2));
}

#[test]
fn test_load_daemon_config() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = create_test_daemon_yaml(&temp_dir);

    let config = DaemonConfig::from_yaml(config_path).unwrap();
    assert_eq!(config.poll_interval, Duration::from_millis(500));
    assert_eq!(config.max_concurrent, 10);
}

#[test]
fn test_client_config_fields() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = create_test_client_yaml(&temp_dir);

    let config = ClientConfig::from_yaml(config_path).unwrap();
    assert_eq!(config.shared_storage, PathBuf::from("/tmp/bifrost"));
    assert_eq!(config.database, Some(PathBuf::from("tasks.db")));
    assert_eq!(config.heartbeat_timeout, Duration::from_secs(180));
}

#[test]
fn test_daemon_config_fields() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = create_test_daemon_yaml(&temp_dir);

    let config = DaemonConfig::from_yaml(config_path).unwrap();
    assert_eq!(config.shared_storage, PathBuf::from("/tmp/bifrost"));
    assert_eq!(config.task_timeout, Duration::from_secs(300));
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.heartbeat_interval, Duration::from_secs(60));
    assert_eq!(config.working_dir, PathBuf::from("/tmp/bifrost/work"));
}

#[test]
fn test_client_config_default() {
    let config = ClientConfig::default();

    assert_eq!(config.shared_storage, PathBuf::from("/tmp/bifrost"));
    assert_eq!(config.poll_interval, Duration::from_secs(2));
    assert_eq!(config.heartbeat_timeout, Duration::from_secs(180));
}

#[test]
fn test_daemon_config_default() {
    let config = DaemonConfig::default();

    assert_eq!(config.shared_storage, PathBuf::from("/tmp/bifrost"));
    assert_eq!(config.poll_interval, Duration::from_millis(500));
    assert_eq!(config.task_timeout, Duration::from_secs(300));
    assert_eq!(config.max_concurrent, 10);
    assert_eq!(config.max_retries, 3);
}

#[test]
fn test_config_validation_poll_interval_too_small() {
    let temp_dir = TempDir::new().unwrap();
    let yaml_content = r#"
shared_storage: "/tmp/bifrost"
poll_interval: "10ms"
heartbeat_timeout: "180s"
"#;
    let config_path = temp_dir.path().join("invalid_client.yaml");
    std::fs::write(&config_path, yaml_content).unwrap();

    let result = ClientConfig::from_yaml(config_path);
    assert!(result.is_err());
}

#[test]
fn test_config_validation_max_retries_out_of_range() {
    let temp_dir = TempDir::new().unwrap();
    let yaml_content = r#"
shared_storage: "/tmp/bifrost"
poll_interval: "500ms"
task_timeout: "300s"
max_retries: 20
heartbeat_interval: "60s"
max_concurrent: 10
working_dir: "/tmp/bifrost/work"
"#;
    let config_path = temp_dir.path().join("invalid_daemon.yaml");
    std::fs::write(&config_path, yaml_content).unwrap();

    let result = DaemonConfig::from_yaml(config_path);
    assert!(result.is_err());
}

#[test]
fn test_config_serialization() {
    let temp_dir = TempDir::new().unwrap();
    let config_path = create_test_client_yaml(&temp_dir);

    let config = ClientConfig::from_yaml(config_path).unwrap();

    // Serialize back to YAML
    let yaml_output = serde_yaml::to_string(&config).unwrap();
    assert!(yaml_output.contains("shared_storage"));
    assert!(yaml_output.contains("poll_interval"));
    assert!(yaml_output.contains("heartbeat_timeout"));

    // Deserialize again
    let config2: ClientConfig = serde_yaml::from_str(&yaml_output).unwrap();
    assert_eq!(config2.shared_storage, config.shared_storage);
    assert_eq!(config2.poll_interval, config.poll_interval);
}