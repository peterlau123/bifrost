pub mod watcher; pub mod executor; pub mod logger; pub mod heartbeat;
pub mod gpu_scheduler; pub mod gpu_monitor; pub mod gpu_task_processor;

use crate::core::settings::BifrostSettings;
use crate::core::protocol::Protocol;

pub struct Daemon { settings: BifrostSettings, protocol: Protocol }

impl Daemon {
    pub fn new(settings: BifrostSettings) -> Result<Self, String> {
        let protocol = Protocol::new(settings.shared_storage.clone()).map_err(|e| format!("{}", e))?;
        Ok(Self { settings, protocol })
    }
    pub async fn run(&self) -> Result<(), String> { Err("Daemon run loop not implemented yet".into()) }
}
