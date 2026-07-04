// File locking utilities using fs2
use fs2::FileExt;
use std::fs::File;
use std::path::Path;
use std::io;

use crate::core::error::{BifrostError, Result};

/// File lock wrapper for atomic file operations
pub struct FileLock {
    file: File,
    path: std::path::PathBuf,
}

impl FileLock {
    /// Create a new file handle for locking (doesn't acquire lock yet)
    pub fn new(path: &Path) -> io::Result<Self> {
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .open(path)?;

        Ok(Self {
            file,
            path: path.to_path_buf(),
        })
    }

    /// Factory method: Create file handle and acquire exclusive lock in one step
    pub fn exclusive(path: &Path) -> Result<Self> {
        let lock = Self::new(path)
            .map_err(BifrostError::IoError)?;
        lock.file.lock_exclusive()
            .map_err(BifrostError::LockError)?;
        Ok(lock)
    }

    /// Factory method: Create file handle and acquire shared lock in one step
    pub fn shared(path: &Path) -> Result<Self> {
        let lock = Self::new(path)
            .map_err(BifrostError::IoError)?;
        lock.file.lock_shared()
            .map_err(BifrostError::LockError)?;
        Ok(lock)
    }

    /// Acquire exclusive lock (for writing)
    pub fn exclusive(&self) -> Result<()> {
        self.file.lock_exclusive()
            .map_err(BifrostError::LockError)?;
        Ok(())
    }

    /// Acquire shared lock (for reading)
    pub fn shared(&self) -> Result<()> {
        self.file.lock_shared()
            .map_err(BifrostError::LockError)?;
        Ok(())
    }

    /// Release the lock
    pub fn unlock(&self) -> Result<()> {
        self.file.unlock()
            .map_err(BifrostError::LockError)?;
        Ok(())
    }

    /// Get the underlying file handle
    pub fn file(&self) -> &File {
        &self.file
    }

    /// Get the path being locked
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        // Lock is automatically released when file is closed
        // But we explicitly unlock for clarity
        if let Err(_) = self.file.unlock() {
            // Ignore unlock errors during drop
        }
    }
}

/// Helper function for atomic write with exclusive lock
pub fn atomic_write(path: &Path, content: &[u8]) -> Result<()> {
    let lock = FileLock::new(path)
        .map_err(BifrostError::IoError)?;

    lock.exclusive()?;

    // Use a temporary file for atomic write
    let temp_path = path.with_extension("tmp");
    std::fs::write(&temp_path, content)
        .map_err(BifrostError::IoError)?;

    // Rename is atomic on most filesystems
    std::fs::rename(&temp_path, path)
        .map_err(BifrostError::IoError)?;

    lock.unlock()?;

    Ok(())
}

/// Helper function for atomic read with shared lock
pub fn atomic_read(path: &Path) -> Result<Vec<u8>> {
    let lock = FileLock::new(path)
        .map_err(BifrostError::IoError)?;

    lock.shared()?;

    let content = std::fs::read(path)
        .map_err(BifrostError::IoError)?;

    lock.unlock()?;

    Ok(content)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_exclusive_lock() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test.lock");

        let lock = FileLock::new(&lock_path).unwrap();
        assert!(lock.exclusive().is_ok());
        assert!(lock.unlock().is_ok());
    }

    #[test]
    fn test_shared_lock() {
        let temp_dir = TempDir::new().unwrap();
        let lock_path = temp_dir.path().join("test_shared.lock");

        let lock = FileLock::new(&lock_path).unwrap();
        assert!(lock.shared().is_ok());
        assert!(lock.unlock().is_ok());
    }

    #[test]
    fn test_atomic_write_and_read() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("atomic_test.txt");

        let content = b"test content for atomic write";
        atomic_write(&file_path, content).unwrap();

        let read_content = atomic_read(&file_path).unwrap();
        assert_eq!(content.to_vec(), read_content);
    }
}