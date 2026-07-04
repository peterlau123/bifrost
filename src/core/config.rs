// Configuration parsing for bifrost client and daemon
use serde::{Deserialize, Serialize, Deserializer};
use std::path::PathBuf;
use std::time::Duration;
use std::fs;
use thiserror::Error;

/// Configuration errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    IoError(#[from] std::io::Error),

    #[error("Failed to parse YAML: {0}")]
    YamlError(#[from] serde_yaml::Error),

    #[error("Invalid duration format: {0}")]
    DurationError(String),
}

/// Custom deserializer for Duration using humantime
fn deserialize_duration<'de, D>(deserializer: D) -> Result<Duration, D::Error>
where
    D: Deserializer<'de>,
{
    let s = String::deserialize(deserializer)?;
    humantime::parse_duration(&s).map_err(serde::de::Error::custom)
}

/// Custom serializer for Duration using humantime
fn serialize_duration<S>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    let s = humantime::format_duration(*duration).to_string();
    serializer.serialize_str(&s)
}

/// Client configuration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientConfig {
    /// Shared storage path for task files
    pub shared_storage: String,

    /// Optional database file for local state
    pub database: Option<String>,

    /// Poll interval for checking new tasks
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub poll_interval: Duration,

    /// Heartbeat timeout before considering daemon dead
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub heartbeat_timeout: Duration,
}

/// Daemon configuration
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DaemonConfig {
    /// Shared storage path for task files
    pub shared_storage: String,

    /// Poll interval for checking new tasks
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub poll_interval: Duration,

    /// Maximum task execution timeout
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub task_timeout: Duration,

    /// Maximum retry attempts for failed tasks
    pub max_retries: u8,

    /// Heartbeat interval for task monitoring
    #[serde(deserialize_with = "deserialize_duration", serialize_with = "serialize_duration")]
    pub heartbeat_interval: Duration,

    /// Maximum concurrent tasks
    pub max_concurrent: usize,

    /// Working directory for task execution
    pub working_dir: String,
}

impl ClientConfig {
    /// Load client config from YAML file
    pub fn from_yaml(path: PathBuf) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(path)?;
        let config: ClientConfig = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    /// Save client config to YAML file
    pub fn to_yaml(&self, path: PathBuf) -> Result<(), ConfigError> {
        let content = serde_yaml::to_string(self)?;
        fs::write(path, content)?;
        Ok(())
    }
}

impl DaemonConfig {
    /// Load daemon config from YAML file
    pub fn from_yaml(path: PathBuf) -> Result<Self, ConfigError> {
        let content = fs::read_to_string(path)?;
        let config: DaemonConfig = serde_yaml::from_str(&content)?;
        Ok(config)
    }

    /// Save daemon config to YAML file
    pub fn to_yaml(&self, path: PathBuf) -> Result<(), ConfigError> {
        let content = serde_yaml::to_string(self)?;
        fs::write(path, content)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_duration_deserialization() {
        let yaml = "poll_interval: 2s";
        #[derive(Deserialize)]
        struct TestConfig {
            #[serde(deserialize_with = "deserialize_duration")]
            poll_interval: Duration,
        }
        let config: TestConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.poll_interval, Duration::from_secs(2));
    }

    #[test]
    fn test_duration_milliseconds() {
        let yaml = "poll_interval: 500ms";
        #[derive(Deserialize)]
        struct TestConfig {
            #[serde(deserialize_with = "deserialize_duration")]
            poll_interval: Duration,
        }
        let config: TestConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(config.poll_interval, Duration::from_millis(500));
    }
}