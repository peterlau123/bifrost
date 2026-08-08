// GPU monitor for checking GPU status via nvidia-smi or simulation mode
use std::process::Command;

/// GPU monitor for checking GPU idle status
#[derive(Clone)]
pub struct GpuMonitor {
    gpu_pool: Vec<u32>,
    simulate_mode: bool,
}

impl GpuMonitor {
    /// Create a new GPU monitor
    ///
    /// # Arguments
    /// * `gpu_pool` - List of GPU IDs to monitor
    /// * `simulate_mode` - If true, always returns idle (for testing)
    pub fn new(gpu_pool: Vec<u32>, simulate_mode: bool) -> Self {
        Self {
            gpu_pool,
            simulate_mode,
        }
    }

    /// Check if a GPU is idle
    ///
    /// In simulation mode, always returns true.
    /// In real mode, queries nvidia-smi to check GPU utilization.
    /// Check if a GPU is idle (async: nvidia-smi 移到 spawn_blocking,
    /// 避免同步阻塞占死 tokio runtime worker — 2026-08-08 假活根因修复)
    pub async fn is_gpu_idle(&self, gpu_id: u32) -> bool {
        // Validate GPU is in pool
        if !self.gpu_pool.contains(&gpu_id) {
            return false;
        }

        if self.simulate_mode {
            return true;
        }

        // Query nvidia-smi for GPU utilization (spawn_blocking 避免阻塞 runtime)
        let gpu_id = gpu_id;
        tokio::task::spawn_blocking(move || {
            let output = Command::new("nvidia-smi")
                .args([
                    "--query-gpu=utilization.gpu",
                    "--format=csv,noheader,nounits",
                    "-i",
                    &gpu_id.to_string(),
                ])
                .output();
            match output {
                Ok(output) if output.status.success() => {
                    let stdout = String::from_utf8_lossy(&output.stdout);
                    match stdout.trim().parse::<u32>() {
                        Ok(utilization) => utilization < 10,
                        Err(_) => false, // Failed to parse, assume busy
                    }
                }
                _ => false, // nvidia-smi failed, assume busy
            }
        })
        .await
        .unwrap_or(false)
    }

    /// Query nvidia-smi to check GPU utilization
    ///
    /// A GPU is considered idle if utilization is below 10%
    fn check_gpu_utilization(&self, gpu_id: u32) -> bool {
        let output = Command::new("nvidia-smi")
            .args([
                "--query-gpu=utilization.gpu",
                "--format=csv,noheader,nounits",
                "-i",
                &gpu_id.to_string(),
            ])
            .output();

        match output {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                match stdout.trim().parse::<u32>() {
                    Ok(utilization) => utilization < 10,
                    Err(_) => false, // Failed to parse, assume busy
                }
            }
            _ => {
                // nvidia-smi failed, assume GPU is busy
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_gpu_idle_simulation_mode() {
        let gpu_pool = vec![0, 1, 2];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);

        // In simulation mode, all GPUs should be idle
        for gpu_id in gpu_pool {
            let rt = tokio::runtime::Runtime::new().unwrap();
            assert!(rt.block_on(monitor.is_gpu_idle(gpu_id)));
        }
    }

    #[test]
    fn test_is_gpu_idle_invalid_gpu() {
        let gpu_pool = vec![0, 1];
        let monitor = GpuMonitor::new(gpu_pool.clone(), false);

        // GPU not in pool should not be idle
        let rt = tokio::runtime::Runtime::new().unwrap();
        assert!(!rt.block_on(monitor.is_gpu_idle(99)));
    }
}
