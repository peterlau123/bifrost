// HTTP Bridge - client <-> transport <-> target over a dumb HTTP relay
// (e.g. Nexus Raw hosted repo, `curl --upload-file` + `wget` semantics).
//
// Day 1 scope: compiled, fully-typed placeholder. Every method returns a
// ConfigInvalid error pointing at the design doc; the real implementation
// lands in Day 2 (reqwest::blocking + submit/read/status round-trip).
//
// The daemon NEVER selects this transport: it only talks to the local
// shared_storage via Protocol. The HTTP bridge is strictly a client-side
// (and fetcher/pusher-side) construct (see `docs/design/http-bridge.md` §1 G1).

use uuid::Uuid;

use crate::core::bridge::{Bridge, TaskStatusResponse};
use crate::core::error::{BifrostError, Result};
use crate::core::models::{Task, TaskResult, TaskStatus};
use crate::core::settings::HttpSection;

/// Stub HTTP bridge. Constructing it validates the `[http]` section against
/// the bilateral contract (D12); every `Bridge` method then reports
/// "not implemented yet" so Day 1 stays reviewable but does nothing useful.
#[derive(Debug)]
pub struct HttpBridge {
    #[allow(dead_code)] // Day 2 will build a reqwest::blocking::Client out of this.
    cfg: HttpSection,
}

impl HttpBridge {
    pub fn new(cfg: &HttpSection) -> Result<Self> {
        // Cheap belt-and-suspenders: settings.validate() already enforces these,
        // but a hand-rolled HttpSection built by tests/tools can otherwise
        // produce panics in Day 2 URL concat. Fail loud and early.
        let prefix = cfg.prefix.as_deref().unwrap_or("").trim();
        if prefix.is_empty() {
            return Err(BifrostError::ConfigInvalid(
                "http.prefix is required (bilateral contract root, D12)".into(),
            ));
        }
        let base_url = cfg.base_url.as_deref().unwrap_or("").trim();
        if base_url.is_empty() {
            return Err(BifrostError::ConfigInvalid(
                "http.base_url is required".into(),
            ));
        }
        Ok(Self { cfg: cfg.clone() })
    }

    fn not_implemented(method: &str) -> BifrostError {
        BifrostError::ConfigInvalid(format!(
            "http bridge `{}` is not implemented yet; see docs/design/http-bridge.md (Day 2 plan)",
            method
        ))
    }
}

impl Bridge for HttpBridge {
    fn submit_task(&self, _task: &Task) -> Result<()> {
        Err(Self::not_implemented("submit_task"))
    }

    fn query_status(&self, _task_id: &Uuid) -> Result<TaskStatusResponse> {
        Err(Self::not_implemented("query_status"))
    }

    fn get_result(&self, _task_id: &Uuid) -> Result<TaskResult> {
        Err(Self::not_implemented("get_result"))
    }

    fn list_pending_tasks(&self) -> Result<Vec<Task>> {
        Err(Self::not_implemented("list_pending_tasks"))
    }

    fn read_task(&self, _task_id: &Uuid) -> Result<Task> {
        Err(Self::not_implemented("read_task"))
    }

    fn write_result(&self, _task_id: &Uuid, _result: &TaskResult) -> Result<()> {
        Err(Self::not_implemented("write_result"))
    }

    fn write_status(
        &self,
        _task_id: &Uuid,
        _status: &TaskStatus,
        _message: Option<&str>,
    ) -> Result<()> {
        Err(Self::not_implemented("write_status"))
    }

    fn remove_task(&self, _task_id: &Uuid) -> Result<()> {
        Err(Self::not_implemented("remove_task"))
    }

    fn cancel_task(&self, _task_id: &Uuid) -> Result<()> {
        Err(Self::not_implemented("cancel_task"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::TaskType;
    use crate::core::settings::{AuthKind, HttpAuthSection, HttpSection, ListingStyle};

    fn valid_cfg() -> HttpSection {
        HttpSection {
            base_url: Some("https://nexus.example.com/repository/tmp-raw/uploads".to_string()),
            repository: Some("tmp-raw".to_string()),
            auth: Some(HttpAuthSection {
                kind: AuthKind::Basic,
                user: Some("u".to_string()),
                password: Some("p".to_string()),
            }),
            prefix: Some("team-lx/bifrost-http-bridge".to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn construct_ok_with_valid_section() {
        assert!(HttpBridge::new(&valid_cfg()).is_ok());
    }

    #[test]
    fn construct_rejects_empty_prefix() {
        let mut cfg = valid_cfg();
        cfg.prefix = Some("".into());
        let err = HttpBridge::new(&cfg).unwrap_err();
        assert!(matches!(err, BifrostError::ConfigInvalid(_)));
    }

    #[test]
    fn construct_rejects_missing_base_url() {
        let mut cfg = valid_cfg();
        cfg.base_url = None;
        let err = HttpBridge::new(&cfg).unwrap_err();
        assert!(matches!(err, BifrostError::ConfigInvalid(_)));
    }

    #[test]
    fn every_method_returns_not_implemented() {
        let b = HttpBridge::new(&valid_cfg()).unwrap();
        let id = Uuid::new_v4();
        let task = Task::new("echo hi".into(), TaskType::Shell);
        assert!(b.submit_task(&task).is_err());
        assert!(b.query_status(&id).is_err());
        assert!(b.get_result(&id).is_err());
        assert!(b.list_pending_tasks().is_err());
        assert!(b.read_task(&id).is_err());
        assert!(b.write_status(&id, &TaskStatus::Running, None).is_err());
        assert!(b.remove_task(&id).is_err());
        assert!(b.cancel_task(&id).is_err());
    }

    #[test]
    fn listing_style_default_is_rest_search() {
        // D8: rest-search is the default for Nexus Raw.
        let cfg = HttpSection::default();
        assert_eq!(cfg.listing_style, ListingStyle::RestSearch);
    }
}
