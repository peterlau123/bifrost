// Configuration parsing tests
use bifrost::core::config::{ClientConfig, DaemonConfig};
use std::path::PathBuf;
use std::time::Duration;

#[test]
fn test_load_client_config() {
    let config = ClientConfig::from_yaml(PathBuf::from("config/client.yaml")).unwrap();
    assert_eq!(config.poll_interval, Duration::from_secs(2));
}

#[test]
fn test_load_daemon_config() {
    let config = DaemonConfig::from_yaml(PathBuf::from("config/daemon.yaml")).unwrap();
    assert_eq!(config.poll_interval, Duration::from_millis(500));
    assert_eq!(config.max_concurrent, 10);
}

#[test]
fn test_client_config_fields() {
    let config = ClientConfig::from_yaml(PathBuf::from("config/client.yaml")).unwrap();
    assert_eq!(config.shared_storage, "/shared/storage");
    assert_eq!(config.database, Some("tasks.db".to_string()));
    assert_eq!(config.heartbeat_timeout, Duration::from_secs(180));
}

#[test]
fn test_daemon_config_fields() {
    let config = DaemonConfig::from_yaml(PathBuf::from("config/daemon.yaml")).unwrap();
    assert_eq!(config.shared_storage, "/shared/storage");
    assert_eq!(config.task_timeout, Duration::from_secs(300));
    assert_eq!(config.max_retries, 3);
    assert_eq!(config.heartbeat_interval, Duration::from_secs(60));
    assert_eq!(config.working_dir, "/path/to/workspace");
}