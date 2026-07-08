use serde::{Deserialize, Serialize, Deserializer};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;

#[derive(Error, Debug)]
pub enum SettingsError {
    #[error("I/O error: {0}")] Io(#[from] std::io::Error),
    #[error("JSON error: {0}")] Json(#[from] serde_json::Error),
    #[error("Invalid duration '{value}': {reason}")] Duration { value: String, reason: String },
}

fn parse_duration_str(s: &str) -> Result<Duration, SettingsError> {
    humantime::parse_duration(s).map_err(|e| SettingsError::Duration { value: s.into(), reason: e.to_string() })
}
fn deser_duration<'de, D: Deserializer<'de>>(d: D) -> Result<Option<Duration>, D::Error> {
    let s: Option<String> = Option::deserialize(d)?;
    match s { Some(v) => parse_duration_str(&v).map(Some).map_err(serde::de::Error::custom), None => Ok(None) }
}
fn ser_duration<S: serde::Serializer>(v: &Option<Duration>, s: S) -> Result<S::Ok, S::Error> {
    match v { Some(d) => s.serialize_str(&humantime::format_duration(*d).to_string()), None => s.serialize_none() }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BifrostSettings {
    pub shared_storage: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")] pub database: Option<PathBuf>,
    #[serde(default)] pub client: ClientSection,
    #[serde(default)] pub daemon: DaemonSection,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ClientSection {
    #[serde(default, deserialize_with = "deser_duration", serialize_with = "ser_duration")]
    pub poll_interval: Option<Duration>,
    #[serde(default, deserialize_with = "deser_duration", serialize_with = "ser_duration")]
    pub heartbeat_timeout: Option<Duration>,
}
impl Default for ClientSection { fn default() -> Self { Self { poll_interval: None, heartbeat_timeout: None } } }

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DaemonSection {
    #[serde(default, deserialize_with = "deser_duration", serialize_with = "ser_duration")]
    pub poll_interval: Option<Duration>,
    #[serde(default, deserialize_with = "deser_duration", serialize_with = "ser_duration")]
    pub task_timeout: Option<Duration>,
    pub max_retries: Option<u8>,
    #[serde(default, deserialize_with = "deser_duration", serialize_with = "ser_duration")]
    pub heartbeat_interval: Option<Duration>,
    pub max_concurrent: Option<usize>,
    pub working_dir: Option<PathBuf>,
}
impl Default for DaemonSection { fn default() -> Self { Self { poll_interval: None, task_timeout: None, max_retries: None, heartbeat_interval: None, max_concurrent: None, working_dir: None } } }

impl BifrostSettings {
    pub fn defaults() -> Self { Self { shared_storage: PathBuf::from("/tmp/bifrost"), database: None, client: ClientSection::default(), daemon: DaemonSection::default() } }
    pub fn path() -> PathBuf { dirs().join("settings.json") }
    pub fn db_path(&self) -> PathBuf { self.database.as_ref().cloned().unwrap_or_else(|| self.shared_storage.join("bifrost.db")) }
    pub fn validate(&self) -> Result<(), SettingsError> {
        if let Some(r) = self.daemon.max_retries { if r > 10 { return Err(SettingsError::Duration { value: r.to_string(), reason: "max_retries must be <= 10".into() }); } }
        if let Some(c) = self.daemon.max_concurrent { if c < 1 || c > 100 { return Err(SettingsError::Duration { value: c.to_string(), reason: "max_concurrent must be 1-100".into() }); } }
        Ok(())
    }
}

pub fn load() -> BifrostSettings {
    match std::fs::read_to_string(BifrostSettings::path()) {
        Ok(c) => match serde_json::from_str::<BifrostSettings>(&c) {
            Ok(s) => { if let Err(e) = s.validate() { eprintln!("Warning: invalid settings ({})", e); return BifrostSettings::defaults(); } s }
            Err(e) => { eprintln!("Warning: invalid settings.json ({})", e); BifrostSettings::defaults() }
        },
        Err(_) => BifrostSettings::defaults(),
    }
}

pub fn init() -> Result<PathBuf, SettingsError> {
    let d = dirs(); let p = BifrostSettings::path();
    std::fs::create_dir_all(&d)?;
    let j = serde_json::to_string_pretty(&BifrostSettings::defaults())?;
    let tmp = p.with_extension("tmp"); std::fs::write(&tmp, &j)?; std::fs::rename(&tmp, &p)?;
    Ok(p)
}

fn dirs() -> PathBuf { std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).unwrap_or_default().join(".bifrost") }

#[cfg(test)] mod tests { use super::*;
    #[test] fn test_defaults() { assert_eq!(BifrostSettings::defaults().shared_storage, PathBuf::from("/tmp/bifrost")); }
    #[test] fn test_db_path() { assert_eq!(BifrostSettings::defaults().db_path(), PathBuf::from("/tmp/bifrost/bifrost.db")); }
    #[test] fn test_parse_duration() { let s: BifrostSettings = serde_json::from_str(r#"{"shared_storage":"/t","client":{"poll_interval":"2s"},"daemon":{}}"#).unwrap(); assert_eq!(s.client.poll_interval, Some(Duration::from_secs(2))); }
    #[test] fn test_invalid_duration() { assert!(serde_json::from_str::<BifrostSettings>(r#"{"shared_storage":"/t","client":{"poll_interval":"blargh"},"daemon":{}}"#).is_err()); }
}
