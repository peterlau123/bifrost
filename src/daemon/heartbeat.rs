// Heartbeat mechanism for daemon health monitoring
use crate::core::error::{BifrostError, Result};
use serde::{Serialize, Deserialize};
use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::Duration;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Heartbeat information for daemon monitoring
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct HeartbeatInfo {
    /// Machine/daemon identifier
    pub machine_id: Uuid,
    /// Last heartbeat timestamp
    pub timestamp: DateTime<Utc>,
    /// Daemon status
    pub status: DaemonStatus,
    /// Number of active tasks
    pub active_tasks: usize,
    /// Number of pending tasks
    pub pending_tasks: usize,
}

/// Daemon status enum
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub enum DaemonStatus {
    /// Daemon is running normally
    Running,
    /// Daemon is idle (no tasks)
    Idle,
    /// Daemon is shutting down
    ShuttingDown,
    /// Daemon encountered an error
    Error,
}

/// Heartbeat manager for periodic health updates
pub struct Heartbeat {
    /// Heartbeat info
    info: HeartbeatInfo,
    /// Heartbeat file path
    heartbeat_file: PathBuf,
    /// Stop flag for background thread
    stop_flag: Arc<AtomicBool>,
}

impl Heartbeat {
    /// Create new heartbeat manager
    pub fn new(shared_storage: PathBuf) -> Result<Self> {
        let machine_id = Uuid::new_v4();
        let heartbeat_file = shared_storage.join("heartbeat.json");

        let info = HeartbeatInfo {
            machine_id,
            timestamp: Utc::now(),
            status: DaemonStatus::Idle,
            active_tasks: 0,
            pending_tasks: 0,
        };

        Ok(Self {
            info,
            heartbeat_file,
            stop_flag: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Write heartbeat to file
    pub fn write_heartbeat(&self) -> Result<()> {
        let content = serde_json::to_string_pretty(&self.info)?;
        fs::write(&self.heartbeat_file, content)
            .map_err(BifrostError::IoError)?;
        Ok(())
    }

    /// Update heartbeat status
    pub fn update_status(&mut self, status: DaemonStatus) {
        self.info.status = status;
        self.info.timestamp = Utc::now();
    }

    /// Update task counts
    pub fn update_task_counts(&mut self, active: usize, pending: usize) {
        self.info.active_tasks = active;
        self.info.pending_tasks = pending;
        self.info.timestamp = Utc::now();
    }

    /// Start background heartbeat thread
    /// Writes heartbeat every 60 seconds until stop_flag is set
    pub fn start_background_thread(self) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let interval = Duration::from_secs(60);

            while !self.stop_flag.load(Ordering::Relaxed) {
                if let Err(e) = self.write_heartbeat() {
                    eprintln!("Heartbeat write error: {}", e);
                }

                thread::sleep(interval);
            }
        })
    }

    /// Stop the heartbeat background thread
    pub fn stop(&self) {
        self.stop_flag.store(true, Ordering::Relaxed);
    }

    /// Get heartbeat info
    pub fn info(&self) -> &HeartbeatInfo {
        &self.info
    }

    /// Read heartbeat from file
    pub fn read_heartbeat(path: &PathBuf) -> Result<HeartbeatInfo> {
        let content = fs::read_to_string(path)
            .map_err(BifrostError::IoError)?;
        let info: HeartbeatInfo = serde_json::from_str(&content)?;
        Ok(info)
    }

    /// Check if daemon is alive based on heartbeat timestamp
    /// Returns true if heartbeat is within the last 2 minutes
    pub fn is_alive(heartbeat: &HeartbeatInfo) -> bool {
        let elapsed = Utc::now().signed_duration_since(heartbeat.timestamp);
        elapsed.num_seconds() < 120
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_heartbeat_new() {
        let temp_dir = TempDir::new().unwrap();
        let heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        assert_eq!(heartbeat.info().status, DaemonStatus::Idle);
        assert_eq!(heartbeat.info().active_tasks, 0);
        assert_eq!(heartbeat.info().pending_tasks, 0);
    }

    #[test]
    fn test_heartbeat_write() {
        let temp_dir = TempDir::new().unwrap();
        let heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        heartbeat.write_heartbeat().unwrap();

        let heartbeat_file = temp_dir.path().join("heartbeat.json");
        assert!(heartbeat_file.exists());

        let content = fs::read_to_string(&heartbeat_file).unwrap();
        assert!(content.contains("machine_id"));
        assert!(content.contains("timestamp"));
    }

    #[test]
    fn test_heartbeat_read() {
        let temp_dir = TempDir::new().unwrap();
        let heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        heartbeat.write_heartbeat().unwrap();

        let heartbeat_file = temp_dir.path().join("heartbeat.json");
        let info = Heartbeat::read_heartbeat(&heartbeat_file).unwrap();

        assert_eq!(info.status, DaemonStatus::Idle);
        assert_eq!(info.machine_id, heartbeat.info().machine_id);
    }

    #[test]
    fn test_heartbeat_update_status() {
        let temp_dir = TempDir::new().unwrap();
        let mut heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        heartbeat.update_status(DaemonStatus::Running);
        assert_eq!(heartbeat.info().status, DaemonStatus::Running);
    }

    #[test]
    fn test_heartbeat_update_task_counts() {
        let temp_dir = TempDir::new().unwrap();
        let mut heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        heartbeat.update_task_counts(3, 5);
        assert_eq!(heartbeat.info().active_tasks, 3);
        assert_eq!(heartbeat.info().pending_tasks, 5);
    }

    #[test]
    fn test_heartbeat_is_alive() {
        let temp_dir = TempDir::new().unwrap();
        let mut heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        // Current timestamp - should be alive
        heartbeat.info.timestamp = Utc::now();
        assert!(Heartbeat::is_alive(heartbeat.info()));

        // Timestamp 3 minutes ago - should not be alive
        heartbeat.info.timestamp = Utc::now() - chrono::Duration::seconds(180);
        assert!(!Heartbeat::is_alive(heartbeat.info()));
    }

    #[test]
    fn test_heartbeat_stop() {
        let temp_dir = TempDir::new().unwrap();
        let heartbeat = Heartbeat::new(temp_dir.path().to_path_buf()).unwrap();

        heartbeat.stop();
        assert!(heartbeat.stop_flag.load(Ordering::Relaxed));
    }
}