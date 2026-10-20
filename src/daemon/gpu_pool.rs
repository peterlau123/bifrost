// GPU pool: hand out one GPU device at a time to tasks.
//
// Each device is a single-permit semaphore, so at most one task per GPU runs
// concurrently. Allocation is round-robin to spread load; if every device is
// busy the caller waits for the next one in rotation.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Parse `nvidia-smi --query-gpu=index --format=csv,noheader` output.
fn parse_indices(stdout: &str) -> Vec<u32> {
    stdout
        .lines()
        .filter_map(|l| l.trim().parse::<u32>().ok())
        .collect()
}

/// 自动探测本机可用 GPU（返回 index 列表）。
///
/// 顺序：`--query-gpu=index` → 失败则数 `nvidia-smi -L` 的行数。
/// 没有 nvidia-smi / 无卡 / 命令失败 → 空列表（等价于不启用 GPU 池）。
pub fn detect() -> Vec<u32> {
    use std::process::Command;
    if let Ok(out) = Command::new("nvidia-smi")
        .args(["--query-gpu=index", "--format=csv,noheader"])
        .output()
    {
        if out.status.success() {
            let ids = parse_indices(&String::from_utf8_lossy(&out.stdout));
            if !ids.is_empty() {
                return ids;
            }
        }
    }
    if let Ok(out) = Command::new("nvidia-smi").arg("-L").output() {
        if out.status.success() {
            let n = String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter(|l| l.starts_with("GPU "))
                .count();
            return (0..n as u32).collect();
        }
    }
    Vec::new()
}

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// A held GPU slot. Dropping it returns the device to the pool.
pub struct GpuLease {
    pub gpu_id: u32,
    _permit: OwnedSemaphorePermit,
}

pub struct GpuPool {
    ids: Vec<u32>,
    slots: Vec<Arc<Semaphore>>,
    next: AtomicUsize,
}

impl GpuPool {
    pub fn new(ids: Vec<u32>) -> Self {
        let slots = ids.iter().map(|_| Arc::new(Semaphore::new(1))).collect();
        Self {
            ids,
            slots,
            next: AtomicUsize::new(0),
        }
    }

    /// True when the pool has at least one device.
    pub fn is_enabled(&self) -> bool {
        !self.ids.is_empty()
    }

    /// Acquire a specific device (for tasks that pin CUDA_VISIBLE_DEVICES).
    /// Returns None when the id is not part of the pool or is disabled.
    pub async fn acquire_specific(self: Arc<Self>, gpu_id: u32) -> Option<GpuLease> {
        let idx = self.ids.iter().position(|&g| g == gpu_id)?;
        let permit = self.slots[idx].clone().acquire_owned().await.ok()?;
        Some(GpuLease {
            gpu_id,
            _permit: permit,
        })
    }

    /// Acquire a GPU. Returns None when the pool is disabled.
    pub async fn acquire(self: Arc<Self>) -> Option<GpuLease> {
        if self.ids.is_empty() {
            return None;
        }
        let start = self.next.fetch_add(1, Ordering::Relaxed) % self.ids.len();

        // Fast path: any idle device, starting at the round-robin cursor.
        for offset in 0..self.ids.len() {
            let idx = (start + offset) % self.ids.len();
            if let Ok(permit) = self.slots[idx].clone().try_acquire_owned() {
                return Some(GpuLease {
                    gpu_id: self.ids[idx],
                    _permit: permit,
                });
            }
        }

        // All busy: wait for the next device in rotation.
        let idx = start % self.ids.len();
        let permit = self.slots[idx].clone().acquire_owned().await.ok()?;
        Some(GpuLease {
            gpu_id: self.ids[idx],
            _permit: permit,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_indices() {
        assert_eq!(parse_indices("0\n1\n2\n3\n"), vec![0, 1, 2, 3]);
        assert_eq!(parse_indices("0\n"), vec![0]);
        assert!(parse_indices("").is_empty());
        // 非数字行忽略，不 panic
        assert_eq!(parse_indices("0\nN/A\n2\n"), vec![0, 2]);
    }

    /// 无 nvidia-smi 的机器上探测结果必须为空（不能凭空造卡）。
    #[test]
    fn test_detect_without_gpu_is_empty() {
        if std::process::Command::new("nvidia-smi")
            .arg("-L")
            .output()
            .is_err()
        {
            assert!(detect().is_empty());
        }
    }

    #[test]
    fn test_disabled_pool() {
        let pool = GpuPool::new(vec![]);
        assert!(!pool.is_enabled());
    }

    #[tokio::test]
    async fn test_round_robin_and_exclusive() {
        let pool = Arc::new(GpuPool::new(vec![0, 1]));
        assert!(pool.is_enabled());

        let a = pool.clone().acquire().await.unwrap();
        let b = pool.clone().acquire().await.unwrap();
        assert_ne!(a.gpu_id, b.gpu_id, "两个租约必须落在不同 GPU 上");

        // Both devices busy: a third acquire must block until one is released.
        let p3 = pool.clone();
        let handle = tokio::spawn(async move { p3.acquire().await.map(|l| l.gpu_id) });
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(!handle.is_finished(), "设备全忙时应等待");

        drop(a);
        let got = tokio::time::timeout(std::time::Duration::from_secs(2), handle)
            .await
            .expect("释放后应立刻拿到设备")
            .unwrap();
        assert!(got.is_some());
    }
}
