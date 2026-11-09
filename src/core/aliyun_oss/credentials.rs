// Aliyun OSS credential loader (option C: env → config-file fallback).
//
// Priority:
//   1. env vars `OSS_ACCESS_KEY_ID` + `OSS_ACCESS_KEY_SECRET`
//   2. first readable file from the candidate paths:
//      - `${BIFROST_OSS_FILE}` if set  (explicit override)
//      - `~/.aliyun/oss.json`          (conventional)
//      - `<dir-of-settings>/oss.json`  (optional, caller passes parent path)
//
// HALF-SET env is an error: setting only one of the two env vars trips a
// `ConfigInvalid` that names the missing one. This saves operators from
// chasing silent "Anonymous access denied" API responses later.
//
// No `settings.json` plaintext credential field is offered:
// the wiki-copy convention says repo secrets belong in file 600 or env,
// not in the commit-tracked settings. (D6 keeps cleartext for Nexus
// `password` because Basic auth gives no other path; OSS env vars are
// already an industry standard so the credentials file is the secondary
// fallback only.)

use std::fs;
use std::path::{Path, PathBuf};

use crate::core::error::{BifrostError, Result};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OssCredentials {
    pub access_key_id: String,
    pub access_key_secret: String,
}

#[derive(Debug, Deserialize)]
struct OssJson {
    access_key_id: String,
    access_key_secret: String,
}

const ENV_ID: &str = "OSS_ACCESS_KEY_ID";
const ENV_SECRET: &str = "OSS_ACCESS_KEY_SECRET";
const ENV_FILE_OVERRIDE: &str = "BIFROST_OSS_FILE";

impl OssCredentials {
    /// Load with the full priority chain. `config_dir` is the directory
    /// containing settings.json (optional; if provided we look for an
    /// `oss.json` sibling there after `~/.aliyun/oss.json`).
    pub fn load(config_dir: Option<&Path>) -> Result<Self> {
        // 1) env priority — both must be present or it's an error
        let id_env = std::env::var(ENV_ID).ok();
        let secret_env = std::env::var(ENV_SECRET).ok();
        match (id_env, secret_env) {
            (Some(id), Some(secret)) => {
                if id.trim().is_empty() || secret.trim().is_empty() {
                    return Err(BifrostError::ConfigInvalid(format!(
                        "{}/{} present but blank; OSS requires both non-empty",
                        ENV_ID, ENV_SECRET
                    )));
                }
                return Ok(Self {
                    access_key_id: id.trim().to_string(),
                    access_key_secret: secret.trim().to_string(),
                });
            }
            (Some(_), None) => {
                return Err(BifrostError::ConfigInvalid(format!(
                    "{} set but {} missing — supplying only one OSS env var is a misconfig",
                    ENV_ID, ENV_SECRET
                )));
            }
            (None, Some(_)) => {
                return Err(BifrostError::ConfigInvalid(format!(
                    "{} set but {} missing — supplying only one OSS env var is a misconfig",
                    ENV_SECRET, ENV_ID
                )));
            }
            (None, None) => {}
        }

        // 2) config-file fallback with override → home → sibling-of-settings
        let mut candidates: Vec<PathBuf> = Vec::new();
        if let Ok(p) = std::env::var(ENV_FILE_OVERRIDE) {
            let trimmed = p.trim();
            if !trimmed.is_empty() {
                candidates.push(PathBuf::from(trimmed));
            }
        }
        let home = std::env::var("USERPROFILE")
            .or_else(|_| std::env::var("HOME"))
            .unwrap_or_default();
        if !home.is_empty() {
            candidates.push(PathBuf::from(home).join(".aliyun").join("oss.json"));
        }
        if let Some(dir) = config_dir {
            candidates.push(dir.join("oss.json"));
        }

        let mut attempts: Vec<String> = Vec::new();
        for path in candidates {
            match fs::read_to_string(&path) {
                Ok(content) => match serde_json::from_str::<OssJson>(&content) {
                    Ok(o) => {
                        let id = o.access_key_id.trim().to_string();
                        let secret = o.access_key_secret.trim().to_string();
                        if id.is_empty() || secret.is_empty() {
                            return Err(BifrostError::ConfigInvalid(format!(
                                "{}: `access_key_id` and `access_key_secret` must both be non-empty",
                                path.display()
                            )));
                        }
                        return Ok(Self {
                            access_key_id: id,
                            access_key_secret: secret,
                        });
                    }
                    Err(e) => {
                        return Err(BifrostError::ConfigInvalid(format!(
                            "{}: parse error: {}",
                            path.display(),
                            e
                        )));
                    }
                },
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    attempts.push(path.display().to_string());
                }
                Err(e) => {
                    return Err(BifrostError::ConfigInvalid(format!(
                        "{}: read error: {}",
                        path.display(),
                        e
                    )));
                }
            }
        }

        Err(BifrostError::ConfigInvalid(format!(
            "OSS credentials not found.\n  Set {} and {} env vars, or create one of:\n  {}\n  `oss.json` shape: {{\"access_key_id\":\"...\",\"access_key_secret\":\"...\"}} (chmod 600)",
            ENV_ID,
            ENV_SECRET,
            attempts.join("\n  ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::MutexGuard;
    use tempfile::TempDir;

    // Env-var tests serialize against each other because process env is
    // process-global. Lock serializes them while keeping other upper tests
    // parallel.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct ScopedEnv {
        prev: Vec<(&'static str, Option<String>)>,
        _guard: MutexGuard<'static, ()>,
    }

    impl ScopedEnv {
        fn new() -> Self {
            let guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let keys = [ENV_ID, ENV_SECRET, ENV_FILE_OVERRIDE];
            let prev = keys.iter().map(|k| (*k, std::env::var(k).ok())).collect();
            Self {
                prev,
                _guard: guard,
            }
        }
        fn set(&self, key: &'static str, value: Option<&str>) {
            match value {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }

    impl Drop for ScopedEnv {
        fn drop(&mut self) {
            for (k, v) in &self.prev {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }

    fn write_json(dir: &TempDir, name: &str, body: &str) -> std::path::PathBuf {
        let p = dir.path().join(name);
        fs::write(&p, body).unwrap();
        p
    }

    #[test]
    fn env_both_present_wins() {
        let env = ScopedEnv::new();
        env.set(ENV_ID, Some("env-id"));
        env.set(ENV_SECRET, Some("env-secret"));
        env.set(ENV_FILE_OVERRIDE, None);
        let c = OssCredentials::load(None).expect("env should satisfy");
        assert_eq!(c.access_key_id, "env-id");
        assert_eq!(c.access_key_secret, "env-secret");
    }

    #[test]
    fn env_supersedes_file() {
        let dir = TempDir::new().unwrap();
        write_json(
            &dir,
            "oss.json",
            r#"{"access_key_id":"file-id","access_key_secret":"file-secret"}"#,
        );

        let env = ScopedEnv::new();
        env.set(ENV_ID, Some("env-id"));
        env.set(ENV_SECRET, Some("env-secret"));
        env.set(
            ENV_FILE_OVERRIDE,
            Some(dir.path().join("oss.json").to_str().unwrap()),
        );

        let c = OssCredentials::load(None).expect("env wins");
        assert_eq!(c.access_key_id, "env-id");
        assert_eq!(c.access_key_secret, "env-secret");
    }

    #[test]
    fn file_satisfies_when_env_unset() {
        let dir = TempDir::new().unwrap();
        write_json(
            &dir,
            "oss.json",
            r#"{"access_key_id":"file-id","access_key_secret":"file-secret"}"#,
        );

        let env = ScopedEnv::new();
        env.set(ENV_ID, None);
        env.set(ENV_SECRET, None);
        env.set(
            ENV_FILE_OVERRIDE,
            Some(dir.path().join("oss.json").to_str().unwrap()),
        );

        let c = OssCredentials::load(None).expect("file should satisfy");
        assert_eq!(c.access_key_id, "file-id");
        assert_eq!(c.access_key_secret, "file-secret");
    }

    #[test]
    fn env_only_id_is_half_set_error() {
        let env = ScopedEnv::new();
        env.set(ENV_ID, Some("id-only"));
        env.set(ENV_SECRET, None);
        let err = OssCredentials::load(None).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains(ENV_SECRET),
            "err mentions missing secret: {msg}"
        );
    }

    #[test]
    fn env_only_secret_is_half_set_error() {
        let env = ScopedEnv::new();
        env.set(ENV_ID, None);
        env.set(ENV_SECRET, Some("secret-only"));
        let err = OssCredentials::load(None).unwrap_err();
        assert!(err.to_string().contains(ENV_ID));
    }

    #[test]
    fn empty_env_vars_rejected() {
        let env = ScopedEnv::new();
        env.set(ENV_ID, Some(""));
        env.set(ENV_SECRET, Some("nonempty"));
        let err = OssCredentials::load(None).unwrap_err();
        assert!(err.to_string().contains("blank"));
    }

    #[test]
    fn empty_file_fields_rejected() {
        let dir = TempDir::new().unwrap();
        write_json(
            &dir,
            "oss.json",
            r#"{"access_key_id":"  ","access_key_secret":"x"}"#,
        );
        let env = ScopedEnv::new();
        env.set(ENV_ID, None);
        env.set(ENV_SECRET, None);
        env.set(
            ENV_FILE_OVERRIDE,
            Some(dir.path().join("oss.json").to_str().unwrap()),
        );
        let err = OssCredentials::load(None).unwrap_err();
        assert!(err.to_string().contains("non-empty"));
    }

    #[test]
    fn bad_file_json_is_explicit_error() {
        let dir = TempDir::new().unwrap();
        write_json(&dir, "oss.json", "{not json");
        let env = ScopedEnv::new();
        env.set(ENV_ID, None);
        env.set(ENV_SECRET, None);
        env.set(
            ENV_FILE_OVERRIDE,
            Some(dir.path().join("oss.json").to_str().unwrap()),
        );
        let err = OssCredentials::load(None).unwrap_err();
        assert!(err.to_string().contains("parse error"));
    }

    #[test]
    fn config_dir_sibling_file_resolves() {
        let env = ScopedEnv::new();
        env.set(ENV_ID, None);
        env.set(ENV_SECRET, None);
        env.set(ENV_FILE_OVERRIDE, None);

        let dir = TempDir::new().unwrap();
        write_json(
            &dir,
            "oss.json",
            r#"{"access_key_id":"sibling-id","access_key_secret":"sibling-secret"}"#,
        );

        let c = OssCredentials::load(Some(dir.path())).expect("sibling file should satisfy");
        assert_eq!(c.access_key_id, "sibling-id");
    }

    #[test]
    fn missing_everywhere_produces_instruction_message() {
        let env = ScopedEnv::new();
        env.set(ENV_ID, None);
        env.set(ENV_SECRET, None);
        env.set(ENV_FILE_OVERRIDE, None);

        let empty = TempDir::new().unwrap();
        let err = OssCredentials::load(Some(empty.path())).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("OSS_ACCESS_KEY_ID"), "actionable error: {msg}");
        assert!(
            msg.contains("OSS_ACCESS_KEY_SECRET"),
            "actionable error: {msg}"
        );
        assert!(msg.contains("oss.json"), "shows the alt file shape");
    }
}
