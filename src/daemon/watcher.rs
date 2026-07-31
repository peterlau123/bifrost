// File event watcher using notify library
// Monitors commands/ directory for new task files with 500ms debounce

use notify::{Watcher, RecommendedWatcher, RecursiveMode, Event, EventKind};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::time::{Duration, Instant};
use tokio::sync::mpsc as tokio_mpsc;

// GpuTaskProcessor integration
use super::gpu_task_processor::GpuTaskProcessor;
use crate::daemon::executor::Executor;
use crate::core::batch_tracker::BatchTracker;

/// File watcher for detecting new task files
pub struct FileWatcher {
    watcher: RecommendedWatcher,
    event_receiver: Receiver<Result<Event, notify::Error>>,
    commands_dir: PathBuf,
    debounce_threshold: Duration,
    last_event_time: Option<Instant>,
}

impl FileWatcher {
    /// Create a new file watcher monitoring the commands directory
    /// Returns a watcher instance and an async channel for task file notifications
    pub fn new(commands_dir: PathBuf) -> Result<Self, String> {
        if !commands_dir.exists() {
            return Err(format!("Commands directory does not exist: {}", commands_dir.display()));
        }

        // Create channel for notify events
        let (tx, rx) = channel();

        // Create watcher with callback
        let watcher = RecommendedWatcher::new(
            move |res: Result<Event, notify::Error>| {
                if let Err(e) = tx.send(res) {
                    eprintln!("Failed to send event: {}", e);
                }
            },
            notify::Config::default(),
        ).map_err(|e| format!("Failed to create watcher: {}", e))?;

        let commands_dir_clone = commands_dir.clone();
        let mut watcher = Self {
            watcher,
            event_receiver: rx,
            commands_dir,
            debounce_threshold: Duration::from_millis(500),
            last_event_time: None,
        };

        // Start watching the commands directory
        watcher.watcher.watch(&commands_dir_clone, RecursiveMode::NonRecursive)
            .map_err(|e| format!("Failed to start watching: {}", e))?;

        Ok(watcher)
    }

    /// Wait for new task file creation with debounce
    /// Returns the path to the new JSON task file
    pub fn wait_for_new_task(&mut self) -> Result<Option<PathBuf>, String> {
        // Poll for events with timeout
        let timeout_duration = Duration::from_millis(100);

        loop {
            // Check for new events with timeout
            match self.event_receiver.recv_timeout(timeout_duration) {
                Ok(event_result) => {
                    match event_result {
                        Ok(event) => {
                            // Filter for file creation events
                            if self.is_new_json_file(&event) {
                                // Apply debounce logic
                                let now = Instant::now();

                                if let Some(last_time) = self.last_event_time {
                                    if now.duration_since(last_time) < self.debounce_threshold {
                                        // Skip this event (within debounce window)
                                        continue;
                                    }
                                }

                                self.last_event_time = Some(now);

                                // Extract the new file path
                                if let Some(path) = event.paths.first() {
                                    if path.extension().map(|ext| ext == "json").unwrap_or(false) {
                                        return Ok(Some(path.clone()));
                                    }
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("Watch error: {}", e);
                            continue;
                        }
                    }
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // No event within timeout, return None
                    return Ok(None);
                }
                Err(e) => {
                    return Err(format!("Channel error: {}", e));
                }
            }
        }
    }

    /// Check if event represents a new JSON file creation
    fn is_new_json_file(&self, event: &Event) -> bool {
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) => {
                // Check if it's in commands directory and is a JSON file
                event.paths.iter().any(|path| {
                    path.starts_with(&self.commands_dir) &&
                    path.extension().map(|ext| ext == "json").unwrap_or(false)
                })
            }
            _ => false,
        }
    }

    /// Stop watching
    pub fn stop(&mut self) -> Result<(), String> {
        self.watcher.unwatch(&self.commands_dir)
            .map_err(|e| format!("Failed to stop watching: {}", e))?;
        Ok(())
    }
}

/// Async version of file watcher for tokio runtime
pub struct AsyncFileWatcher {
    commands_dir: PathBuf,
    debounce_threshold: Duration,
}

impl AsyncFileWatcher {
    /// Create a new async file watcher
    pub fn new(commands_dir: PathBuf) -> Result<Self, String> {
        if !commands_dir.exists() {
            return Err(format!("Commands directory does not exist: {}", commands_dir.display()));
        }

        Ok(Self {
            commands_dir,
            debounce_threshold: Duration::from_millis(500),
        })
    }

    /// Async wait for new task files
    /// Returns a tokio channel receiver for new task file paths
    pub async fn watch_async(&self) -> tokio_mpsc::Receiver<PathBuf> {
        let (tx, rx) = tokio_mpsc::channel(100);

        // Spawn blocking task for notify watcher
        let commands_dir = self.commands_dir.clone();
        let debounce = self.debounce_threshold;

        tokio::spawn(async move {
            // Run watcher in blocking context
            tokio::task::spawn_blocking(move || {
                if let Ok(mut watcher) = FileWatcher::new(commands_dir) {
                    let mut last_time = None;

                    loop {
                        match watcher.wait_for_new_task() {
                            Ok(Some(path)) => {
                                // Apply additional debounce
                                let now = Instant::now();
                                if let Some(last) = last_time {
                                    if now.duration_since(last) < debounce {
                                        continue;
                                    }
                                }
                                last_time = Some(now);

                                // Send path through channel
                                if tx.blocking_send(path).is_err() {
                                    break;
                                }
                            }
                            Ok(None) => {
                                // No event, continue polling
                                std::thread::sleep(Duration::from_millis(100));
                            }
                            Err(e) => {
                                eprintln!("Watcher error: {}", e);
                                break;
                            }
                        }
                    }
                }
            }).await;
        });

        rx
    }
}

/// Integration example: Run watcher with GpuTaskProcessor
///
/// This function demonstrates how to wire up the AsyncFileWatcher
/// with GpuTaskProcessor for GPU-aware task processing.
///
/// # Example
/// ```rust,no_run
/// use std::path::PathBuf;
/// use std::time::Duration;
/// use bifrost::daemon::watcher::{AsyncFileWatcher, run_with_gpu_processor};
///
/// #[tokio::main]
/// async fn main() {
///     let commands_dir = PathBuf::from("/shared/commands");
///     let log_dir = PathBuf::from("/shared/logs");
///     let gpu_pool = vec![0, 1, 2, 3]; // 4 GPUs
///
///     run_with_gpu_processor(commands_dir, log_dir, gpu_pool, false).await;
/// }
/// ```
pub async fn run_with_gpu_processor(
    commands_dir: PathBuf,
    log_dir: PathBuf,
    gpu_pool: Vec<u32>,
    simulate_mode: bool,
    batch_tracker: Option<BatchTracker>,
) -> Result<(), String> {
    // Create async file watcher
    let watcher = AsyncFileWatcher::new(commands_dir)?;
    let rx = watcher.watch_async().await;

    // Create executor with 5-minute default timeout
    let executor = Executor::new(log_dir, Duration::from_secs(300))
        .map_err(|e| format!("Failed to create executor: {}", e))?;

    // Create GPU task processor
    let mut processor = GpuTaskProcessor::new(gpu_pool, executor, simulate_mode, batch_tracker)
        .map_err(|e| format!("Failed to create GPU task processor: {}", e))?;

    // Start processing tasks from watcher
    println!("Starting GPU task processor with watcher...");
    processor.run(rx).await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use std::fs;
    use std::io::Write;

    #[test]
    fn test_watcher_new() {
        let temp_dir = TempDir::new().unwrap();
        let commands_dir = temp_dir.path().join("commands");
        fs::create_dir(&commands_dir).unwrap();

        let watcher = FileWatcher::new(commands_dir);
        assert!(watcher.is_ok());
    }

    #[test]
    fn test_watcher_nonexistent_dir() {
        let watcher = FileWatcher::new(PathBuf::from("/nonexistent/path"));
        assert!(watcher.is_err());
    }

    #[test]
    fn test_detect_new_task_file() {
        let temp_dir = TempDir::new().unwrap();
        let commands_dir = temp_dir.path().join("commands");
        fs::create_dir(&commands_dir).unwrap();

        let mut watcher = FileWatcher::new(commands_dir.clone()).unwrap();

        // Create a new JSON file
        let task_file = commands_dir.join("test_task.json");
        let mut file = fs::File::create(&task_file).unwrap();
        file.write_all(b"{\"test\": \"data\"}").unwrap();
        file.sync_all().unwrap();

        // Wait briefly for event to propagate
        std::thread::sleep(Duration::from_millis(200));

        // Check for new file
        let result = watcher.wait_for_new_task();
        assert!(result.is_ok());

        // May detect the file or return None depending on timing
        match result.unwrap() {
            Some(path) => {
                assert_eq!(path.extension().unwrap(), "json");
                assert!(path.starts_with(&commands_dir));
            }
            None => {
                // File may have been detected during debounce period
                // Try again
                std::thread::sleep(Duration::from_millis(600));
                let result2 = watcher.wait_for_new_task().unwrap();
                assert!(result2.is_some());
            }
        }

        watcher.stop().unwrap();
    }
}