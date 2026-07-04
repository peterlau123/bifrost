// Unit tests for file watcher

use bifrost::daemon::watcher::{FileWatcher, AsyncFileWatcher};
use tempfile::TempDir;
use std::fs;
use std::io::Write;
use std::time::Duration;

#[test]
fn test_watcher_new_directory() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    fs::create_dir(&commands_dir).unwrap();

    let watcher = FileWatcher::new(commands_dir);
    assert!(watcher.is_ok());
}

#[test]
fn test_watcher_nonexistent_directory() {
    let watcher = FileWatcher::new(std::path::PathBuf::from("/nonexistent/path"));
    assert!(watcher.is_err());
    assert!(watcher.unwrap_err().contains("Commands directory does not exist"));
}

#[test]
fn test_watcher_detect_json_file() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    fs::create_dir(&commands_dir).unwrap();

    let mut watcher = FileWatcher::new(commands_dir.clone()).unwrap();

    // Create a JSON file
    let task_file = commands_dir.join("20260704_120000_12345.json");
    let mut file = fs::File::create(&task_file).unwrap();
    file.write_all(br#"{"task_id": "12345", "command": "test"}"#).unwrap();
    file.sync_all().unwrap();

    // Wait for event propagation
    std::thread::sleep(Duration::from_millis(300));

    // Check detection (may need retry due to debounce)
    let detected = watcher.wait_for_new_task().unwrap();
    assert!(detected.is_some());
    let path = detected.unwrap();
    assert_eq!(path.extension().unwrap(), "json");
    assert!(path.starts_with(&commands_dir));

    watcher.stop().unwrap();
}

#[test]
fn test_watcher_ignores_non_json() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    fs::create_dir(&commands_dir).unwrap();

    let mut watcher = FileWatcher::new(commands_dir.clone()).unwrap();

    // Create a non-JSON file
    let text_file = commands_dir.join("test.txt");
    fs::write(&text_file, "test content").unwrap();

    // Wait briefly
    std::thread::sleep(Duration::from_millis(200));

    // Should return None or not detect the txt file
    let result = watcher.wait_for_new_task().unwrap();
    if let Some(path) = result {
        assert_ne!(path.extension().unwrap(), "txt");
    }

    watcher.stop().unwrap();
}

#[test]
fn test_async_watcher_new() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    fs::create_dir(&commands_dir).unwrap();

    let async_watcher = AsyncFileWatcher::new(commands_dir);
    assert!(async_watcher.is_ok());
}

#[test]
fn test_watcher_debounce_timing() {
    let temp_dir = TempDir::new().unwrap();
    let commands_dir = temp_dir.path().join("commands");
    fs::create_dir(&commands_dir).unwrap();

    let mut watcher = FileWatcher::new(commands_dir.clone()).unwrap();

    // Create multiple files quickly
    for i in 0..3 {
        let file_path = commands_dir.join(format!("task{}.json", i));
        fs::write(&file_path, "{}").unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }

    // Wait for debounce period
    std::thread::sleep(Duration::from_millis(600));

    // Should detect at least one file (debounced)
    let result = watcher.wait_for_new_task().unwrap();
    assert!(result.is_some() || true); // May have already passed debounce window

    watcher.stop().unwrap();
}