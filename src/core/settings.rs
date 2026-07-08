use serde::{Deserialize, Serialize};
use std::path::PathBuf;
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct BifrostSettings {
    pub shared_storage: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database: Option<PathBuf>,
    #[serde(default)] pub client: ClientSection,
    #[serde(default)] pub daemon: DaemonSection,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct ClientSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_timeout: Option<String>,
}
#[derive(Serialize, Deserialize, Debug, Clone, Default)]
pub struct DaemonSection {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poll_interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_timeout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u8>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub heartbeat_interval: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_concurrent: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub working_dir: Option<PathBuf>,
}
impl BifrostSettings {
    pub fn defaults() -> Self { Self { shared_storage: PathBuf::from("/tmp/bifrost"), database: None, client: ClientSection::default(), daemon: DaemonSection::default() } }
    pub fn path() -> PathBuf { dirs().join("settings.json") }
    pub fn db_path(&self) -> PathBuf { self.database.as_ref().map(|p| p.clone()).unwrap_or_else(|| self.shared_storage.join("bifrost.db")) }
}
pub fn load() -> BifrostSettings {
    match std::fs::read_to_string(BifrostSettings::path()) {
        Ok(c) => serde_json::from_str(&c).unwrap_or_else(|e| { eprintln!("Warning: invalid settings.json ({})", e); BifrostSettings::defaults() }),
        Err(_) => BifrostSettings::defaults(),
    }
}
pub fn init() -> Result<PathBuf, String> {
    let d = dirs(); let p = BifrostSettings::path();
    std::fs::create_dir_all(&d).map_err(|e| format!("{}", e))?;
    let j = serde_json::to_string_pretty(&BifrostSettings::defaults()).map_err(|e| format!("{}", e))?;
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, &j).map_err(|e| format!("{}", e))?;
    std::fs::rename(&tmp, &p).map_err(|e| format!("{}", e))?;
    Ok(p)
}
fn dirs() -> PathBuf { std::env::var("USERPROFILE").or_else(|_| std::env::var("HOME")).map(PathBuf::from).unwrap_or_default().join(".bifrost") }
#[cfg(test)] mod tests { use super::*;
    #[test] fn test_defaults() { assert_eq!(BifrostSettings::defaults().shared_storage, PathBuf::from("/tmp/bifrost")); }
    #[test] fn test_db_path() { assert_eq!(BifrostSettings::defaults().db_path(), PathBuf::from("/tmp/bifrost/bifrost.db")); }
}
