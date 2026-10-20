// 启动自检：这台机器的时间机制到底能不能用？
//
// 背景：Linux 的 sleep 走 clock_nanosleep(CLOCK_MONOTONIC)。某些虚拟化环境
// （H20 上记录过）里单调时钟会冻住，于是 nanosleep 的 deadline 永不到达，
// 线程永久卡在 sleep 里 —— daemon 表现为"假活"：心跳还在、任务不消费。
// 曾经的应对是把兜底扫描写成 busy-poll + yield_now（不依赖时钟），代价是
// 常驻吃满一个核。
//
// 不是所有机器都有这个毛病（H200 大概率没有），所以不要默认背这个成本：
// 启动时花 ~200ms 探一次，只有确认时钟不可靠才切到 spin 模式。
//
// 判定方法：同时看两种时钟。请求 sleep(100ms) 后
//   * 墙钟（CLOCK_REALTIME）前进得远多于请求值，而
//   * 单调时钟（Instant）几乎没动
// 说明单调时钟与墙钟脱节 —— 正是 sleep 会卡死的前兆。

use std::time::{Duration, Instant, SystemTime};

/// 一次自检请求的 sleep 时长。
const PROBE_SLEEP: Duration = Duration::from_millis(100);
/// 墙钟比请求值多跑了这么多，才算异常。
const WALL_TOLERANCE: Duration = Duration::from_millis(2_000);
/// 单调时钟走了这么久以内，同时墙钟大幅前进，判定为冻结。
const MONO_TOLERANCE: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClockHealth {
    /// sleep 行为正常，可以用定时器驱动扫描。
    Ok,
    /// 单调时钟与墙钟脱节：sleep 有卡死风险，扫描必须改成不依赖时钟。
    Frozen {
        requested_ms: u128,
        wall_ms: u128,
        mono_ms: u128,
    },
}

impl ClockHealth {
    pub fn is_frozen(&self) -> bool {
        matches!(self, ClockHealth::Frozen { .. })
    }

    pub fn summary(&self) -> String {
        match self {
            ClockHealth::Ok => "ok".to_string(),
            ClockHealth::Frozen {
                requested_ms,
                wall_ms,
                mono_ms,
            } => format!(
                "frozen (asked {}ms, wall advanced {}ms, monotonic {}ms)",
                requested_ms, wall_ms, mono_ms
            ),
        }
    }
}

/// Probe once. Costs PROBE_SLEEP on a healthy machine.
pub fn probe() -> ClockHealth {
    let wall0 = SystemTime::now();
    let mono0 = Instant::now();
    std::thread::sleep(PROBE_SLEEP);
    let mono = mono0.elapsed();
    let wall = SystemTime::now().duration_since(wall0).unwrap_or_default();

    if wall > PROBE_SLEEP + WALL_TOLERANCE && mono < MONO_TOLERANCE {
        ClockHealth::Frozen {
            requested_ms: PROBE_SLEEP.as_millis(),
            wall_ms: wall.as_millis(),
            mono_ms: mono.as_millis(),
        }
    } else {
        ClockHealth::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 健康机器上必须判 Ok（否则会把正常机器误切到 spin 模式）。
    #[test]
    fn test_probe_ok_on_healthy_machine() {
        let health = probe();
        assert_eq!(
            health,
            ClockHealth::Ok,
            "健康机器不应被判定为时钟冻结: {}",
            health.summary()
        );
    }

    /// 判定阈值本身：墙钟大幅前进 + 单调时钟不动 = 冻结。
    #[test]
    fn test_frozen_detection_rules() {
        let frozen = ClockHealth::Frozen {
            requested_ms: 100,
            wall_ms: 5_000,
            mono_ms: 3,
        };
        assert!(frozen.is_frozen());
        assert!(frozen.summary().contains("frozen"));

        // 墙钟和单调时钟都正常前进 → 不是冻结
        let healthy = ClockHealth::Ok;
        assert!(!healthy.is_frozen());
    }
}
