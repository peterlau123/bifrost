use serde::{Deserialize, Deserializer, Serialize};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SettingsError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("Invalid duration '{value}': {reason}")]
    Duration { value: String, reason: String },
}

fn parse_duration_str(s: &str) -> Result<Duration, SettingsError> {
    humantime::parse_duration(s).map_err(|e| SettingsError::Duration {
        value: s.into(),
        reason: e.to_string(),
    })
}
fn deser_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    match s {
        Some(v) => parse_duration_str(&v)
            .map(Some)
            .map_err(serde::de::Error::custom),
        None => Ok(None),
    }
}
fn ser_duration<S: serde::Serializer>(v: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match v {
        Some(d) => s.serialize_str(&humantime::format_duration(*d).to_string()),
        None => s.serialize_none(),
    }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BifrostSettings {
    pub shared_storage: PathBuf,
    /// Transport type: "shared" (default) or "ssh".
    #[serde(default)]
    pub transport: Transport,
    /// SSH transport config (required when transport == "ssh").
    #[serde(default)]
    pub ssh: Option<SshSection>,
    #[serde(default)]
    pub client: ClientSection,
    #[serde(default)]
    pub daemon: DaemonSection,
}

/// Transport selection for the bridge.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Transport {
    /// Shared-storage bridge (GPFS/NFS): both sides read/write the same dirs.
    #[default]
    Shared,
    /// SSH bridge: client reaches the target machine's dirs over SSH.
    Ssh,
}

impl Transport {
    pub fn is_ssh(&self) -> bool {
        matches!(self, Transport::Ssh)
    }
}

/// SSH bridge configuration (used when `transport == "ssh"`).
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct SshSection {
    /// Target hostname or IP (required).
    pub host: Option<String>,
    /// SSH user (default: current user).
    pub user: Option<String>,
    /// Remote directory on the target machine that plays the role of
    /// shared_storage: commands/ results/ status/ artifacts/ live under it.
    pub remote_dir: Option<PathBuf>,
    /// SSH port (default: 22).
    pub port: Option<u16>,
    /// Connect timeout for each ssh invocation (default: 10s).
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration"
    )]
    pub connect_timeout: Option<Duration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ClientSection {
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration"
    )]
    pub poll_interval: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration"
    )]
    pub heartbeat_timeout: Option<Duration>,
}

#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DaemonSection {
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration"
    )]
    pub poll_interval: Option<Duration>,
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration"
    )]
    pub task_timeout: Option<Duration>,
    pub max_retries: Option<u8>,
    #[serde(
        default,
        deserialize_with = "deser_duration",
        serialize_with = "ser_duration"
    )]
    pub heartbeat_interval: Option<Duration>,
    pub max_concurrent: Option<usize>,
    pub working_dir: Option<PathBuf>,
}

impl BifrostSettings {
    pub fn defaults() -> Self {
        Self {
            shared_storage: dirs().join("data"),
            transport: Transport::Shared,
            ssh: None,
            client: ClientSection::default(),
            daemon: DaemonSection::default(),
        }
    }
    pub fn path() -> PathBuf {
        dirs().join("settings.json")
    }
    pub fn validate(&self) -> Result<(), SettingsError> {
        if let Some(r) = self.daemon.max_retries {
            if r > 10 {
                return Err(SettingsError::Duration {
                    value: r.to_string(),
                    reason: "max_retries must be <= 10".into(),
                });
            }
        }
        if let Some(c) = self.daemon.max_concurrent {
            if !(1..=100).contains(&c) {
                return Err(SettingsError::Duration {
                    value: c.to_string(),
                    reason: "max_concurrent must be 1-100".into(),
                });
            }
        }
        if self.transport.is_ssh() {
            let ssh = self.ssh.as_ref().ok_or_else(|| SettingsError::Duration {
                value: "ssh".into(),
                reason: "transport=ssh requires an 'ssh' config section".into(),
            })?;
            if ssh.host.is_none() || ssh.remote_dir.is_none() {
                return Err(SettingsError::Duration {
                    value: "ssh".into(),
                    reason: "ssh config requires 'host' and 'remote_dir'".into(),
                });
            }
        }
        Ok(())
    }
}

pub fn load() -> BifrostSettings {
    match std::fs::read_to_string(BifrostSettings::path()) {
        Ok(c) => match serde_json::from_str::<BifrostSettings>(&c) {
            Ok(s) => {
                if let Err(e) = s.validate() {
                    eprintln!("Warning: invalid settings ({})", e);
                    return BifrostSettings::defaults();
                }
                s
            }
            Err(e) => {
                eprintln!("Warning: invalid settings.json ({})", e);
                BifrostSettings::defaults()
            }
        },
        Err(_) => BifrostSettings::defaults(),
    }
}

pub fn init() -> Result<PathBuf, SettingsError> {
    let d = dirs();
    let p = BifrostSettings::path();
    std::fs::create_dir_all(&d)?;
    let j = serde_json::to_string_pretty(&BifrostSettings::defaults())?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, &j)?;
    std::fs::rename(&tmp, &p)?;
    Ok(p)
}

fn dirs() -> PathBuf {
    std::env::var("USERPROFILE")
        .or_else(|_| std::env::var("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".bifrost")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_defaults() {
        assert_eq!(
            BifrostSettings::defaults().shared_storage,
            dirs().join("data")
        );
    }

    #[test]
    fn test_parse_duration() {
        let s: BifrostSettings = serde_json::from_str(
            r#"{"shared_storage":"/t","client":{"poll_interval":"2s"},"daemon":{}}"#,
        )
        .unwrap();
        assert_eq!(s.client.poll_interval, Some(Duration::from_secs(2)));
    }
    #[test]
    fn test_invalid_duration() {
        assert!(serde_json::from_str::<BifrostSettings>(
            r#"{"shared_storage":"/t","client":{"poll_interval":"blargh"},"daemon":{}}"#
        )
        .is_err());
    }

    #[test]
    fn test_transport_defaults_to_shared() {
        let s: BifrostSettings = serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap();
        assert_eq!(s.transport, Transport::Shared);
        assert!(s.ssh.is_none());
    }

    #[test]
    fn test_transport_ssh_parse() {
        let s: BifrostSettings = serde_json::from_str(
            r#"{"shared_storage":"/t","transport":"ssh","ssh":{"host":"h1","remote_dir":"/r"}}"#,
        )
        .unwrap();
        assert_eq!(s.transport, Transport::Ssh);
        assert!(s.transport.is_ssh());
        let ssh = s.ssh.unwrap();
        assert_eq!(ssh.host.as_deref(), Some("h1"));
        assert_eq!(ssh.remote_dir.unwrap(), std::path::PathBuf::from("/r"));
    }

    #[test]
    fn test_transport_ssh_unknown_falls_back_shared() {
        // Unknown transport value: serde errors (no silent fallback).
        assert!(serde_json::from_str::<BifrostSettings>(
            r#"{"shared_storage":"/t","transport":"carrier-pigeon"}"#
        )
        .is_err());
    }

    #[test]
    fn test_validate_ssh_requires_section_and_fields() {
        let no_section = BifrostSettings {
            transport: Transport::Ssh,
            ssh: None,
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        assert!(no_section.validate().is_err());

        let no_host = BifrostSettings {
            transport: Transport::Ssh,
            ssh: Some(SshSection {
                host: None,
                remote_dir: Some(std::path::PathBuf::from("/r")),
                ..SshSection::default()
            }),
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        assert!(no_host.validate().is_err());

        let ok = BifrostSettings {
            transport: Transport::Ssh,
            ssh: Some(SshSection {
                host: Some("h1".into()),
                remote_dir: Some(std::path::PathBuf::from("/r")),
                ..SshSection::default()
            }),
            ..serde_json::from_str(r#"{"shared_storage":"/t"}"#).unwrap()
        };
        assert!(ok.validate().is_ok());
    }
}
