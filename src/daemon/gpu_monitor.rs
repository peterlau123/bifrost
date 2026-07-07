// GPU monitor for checking GPU status via nvidia-smi or simulation mode
use std::process::Command;

/// GPU monitor for checking GPU idle status
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
    pub fn is_gpu_idle(&mut self, gpu_id: u32) -> bool {
        // Validate GPU is in pool
        if !self.gpu_pool.contains(&gpu_id) {
            return false;
        }

        if self.simulate_mode {
            return true;
        }

        // Query nvidia-smi for GPU utilization
        self.check_gpu_utilization(gpu_id)
    }

    /// Get the list of GPUs being monitored
    pub fn get_gpu_pool(&self) -> &[u32] {
        &self.gpu_pool
    }

    /// Check if running in simulation mode
    pub fn is_simulation_mode(&self) -> bool {
        self.simulate_mode
    }

    /// Query nvidia-smi to check GPU utilization
    ///
    /// A GPU is considered idle if utilization is below 10%
    fn check_gpu_utilization(&self, gpu_id: u32) -> bool {
        let output = Command::new("nvidia-smi")
            .args(&[
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

    /// Get all idle GPUs from the pool
    pub fn get_idle_gpus(&mut self) -> Vec<u32> {
        self.gpu_pool
            .iter()
            .copied()
            .filter(|&gpu_id| self.is_gpu_idle(gpu_id))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gpu_monitor_new() {
        let gpu_pool = vec![0, 1, 2];
        let monitor = GpuMonitor::new(gpu_pool.clone(), true);

        assert_eq!(monitor.get_gpu_pool(), &[0, 1, 2]);
        assert!(monitor.is_simulation_mode());
    }

    #[test]
    fn test_is_gpu_idle_simulation_mode() {
        let gpu_pool = vec![0, 1, 2];
        let mut monitor = GpuMonitor::new(gpu_pool.clone(), true);

        // In simulation mode, all GPUs should be idle
        for gpu_id in gpu_pool {
            assert!(monitor.is_gpu_idle(gpu_id));
        }
    }

    #[test]
    fn test_is_gpu_idle_invalid_gpu() {
        let gpu_pool = vec![0, 1];
        let mut monitor = GpuMonitor::new(gpu_pool.clone(), false);

        // GPU not in pool should not be idle
        assert!(!monitor.is_gpu_idle(99));
    }

    #[test]
    fn test_get_idle_gpus_simulation_mode() {
        let gpu_pool = vec![0, 1, 2];
        let mut monitor = GpuMonitor::new(gpu_pool.clone(), true);

        let idle_gpus = monitor.get_idle_gpus();
        assert_eq!(idle_gpus, vec![0, 1, 2]);
    }
}
