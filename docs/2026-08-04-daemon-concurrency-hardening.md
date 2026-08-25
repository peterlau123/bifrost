# Daemon 并发能力优化与高负载排障（2026-08-04）

> 场景：APMM UT Workflow Phase 2 timeout 重试期间，bifrost daemon 在高并发
> （外部 4-8 并发 × 内部并行）下反复"卡死"：心跳停更、任务堆积、结果丢失。
> 本文记录 root cause、修复（blocking pool + 独立心跳线程）与排障经验。
> 接续 [ops-practice.md](ops-practice.md)（2026-08-03 接入记录）。

---

## 1. 现象

APMM 重试 727 个 timeout batch（每个 batch 内部并行跑 8 个 pytest 测试）时：

- daemon 心跳文件停止更新（`heartbeat.json` mtime 不动）
- `commands/` 堆积大量 `.processing` / `.json` 任务文件（50-100+）
- 新任务报 `cannot parse task file (after 3 retries)`
- 重启 daemon 后短暂恢复，处理几个任务又卡死，**循环往复**

---

## 2. Root Cause

### 2.1 同步阻塞 syscall 占满 tokio async worker

`runner.rs` 主循环与任务处理在 tokio worker 上直接做 4 处**阻塞式文件 I/O**，
GPFS 网络文件系统上每次 ~4ms（flock + write + rename），高并发下多个任务
同时卡在 I/O 上，心跳与任务 future 抢不到调度：

| 位置 | 同步调用 | 频率 |
|------|----------|------|
| `runner.rs` fallback scan | `std::fs::read_dir` | 每 100ms 全量 |
| `spawn_task` | `OpenOptions::create_new`（claim marker） | 每任务 |
| `read_task_retry` | `std::fs::read_to_string` | 每任务 |
| `process_one` 收尾 | `atomic_write`（result/status/remove） | 每任务 ×3 |

### 2.2 心跳放在 tokio::spawn 里被饿死

`run_server` 里心跳循环是 `tokio::spawn(async { ... fs::write ... })`——
GPFS 同步写在 async worker 上，高并发时心跳任务迟迟得不到调度 →
`heartbeat.json` 停更，被健康检查误判为"daemon 死了"。

### 2.3 （排障过程发现）多 daemon 实例并存

8-03 启动的旧 daemon（pid 1427217）在 supervisor 死亡后**变成孤儿进程继续运行**，
与后来重启的新 daemon **竞争同一个 `commands/` 目录**：两个 daemon 同时消费
同一任务文件 → `cannot parse task file`、任务被重复 claim、心跳/结果混乱。

---

## 3. 修复

### 3.1 同步 I/O 全部移到 blocking pool

`src/daemon/runner.rs`（commit `3add3b5`）：

- fallback scan 的 `scan_pending` 包进 `tokio::task::spawn_blocking`
- claim marker 的 `create_new` 包进 `spawn_blocking`（`.is_ok()` 判定）
- `read_task_retry` 的 `fs::read_to_string` 包进 `spawn_blocking`
- `process_one` 的 `write_result` / `write_status` / `remove_task` 用新增的
  `blocking_write<F, R>` helper 包进 `spawn_blocking`（`p: &Arc<Protocol>`）

### 3.2 心跳改为独立 std::thread

心跳循环从 `tokio::spawn` 改为 `std::thread::spawn` + `std::thread::sleep`，
与任务线程池完全隔离，GPFS 写再慢也不影响心跳按时上报。

### 3.3 daemon 侧 task_timeout 放宽

`settings.json`：`task_timeout: 300s → 900s`。原因：timeout batch 的测试
本身要 400-600s，daemon 默认 300s 硬上限（`executor.rs` 里
`effective_timeout = min(task.timeout, default_timeout)`）会把测试提前杀掉，
导致重试必失败。900s 覆盖 600s 测试 + 缓冲。

### 3.4 运维：杀干净旧 daemon，单实例运行

```
ssh infra-gpu-h20-022 "ps -eo pid,lstart,cmd | grep 'bifrost server' | grep -v grep"
# 发现双实例 → kill -9 旧 pid（8-03 的孤儿）
```

---

## 4. 验证

- `cargo test`：74 passed（66 lib + 7 integration + 1 doc）
- `cargo clippy --all-targets`：0 warning
- `cargo fmt --check`：通过
- 运行时：心跳每 60s 稳定更新；任务 0.3-0.5s 正常完成；APMM 全量重试
  2 并发下稳定运行（每 batch 5-10 分钟真实执行）

---

## 5. 排障经验（重要）

### 5.1 H20 时钟比本机慢 ~5 分钟

`server.log` / `heartbeat.json` 里的时间戳是 **H20 本地时钟**，比网关节点
（本机）慢约 5 分钟。**判断 daemon 是否存活不能看绝对时间戳**——要用
`ssh H20 "date"` 对时，或看任务是否实际完成（0.3s 实测）而非心跳 mtime。
本次大量"daemon 卡死"误判都是时钟差造成。

### 5.2 supervisor 恢复

- supervisor 死亡后 `control.json` 残留、`supervisor.pid` 丢失、restart 指令无人消费
- 恢复：SSH 直连 H20，`bash /gpfs/gcsp/liuxin/bifrost/bifrost-supervisor.sh start`
- ⚠️ 脚本必须有执行权限：`chmod +x bifrost-supervisor.sh`（`nohup "$0" attach`
  需要 x 权限直接 exec；无权限时报 `nohup: failed to run command ... Permission denied`）

### 5.3 daemon 健康检查 checklist

```bash
# 1. 进程数（必须恰好 1 个）
ssh H20 "ps -eo pid,lstart,cmd | grep 'bifrost server' | grep -v grep"
# 2. 心跳新鲜度（对时后看 age）
python3 -c "import json,time;from pathlib import Path
hb=json.loads(Path('/gpfs/gcsp/liuxin/bifrost_test/heartbeat.json').read_text())
print(hb['timestamp'], hb.get('active_tasks'))"
# 3. 实际任务往返（最可靠）
REMOTE_BACKEND=bifrost python3 -c "from tools.remote_executor import run_remote
print(run_remote('echo OK && hostname', timeout=30))"
```

---

## 6. 相关文件

- `src/daemon/runner.rs` — blocking pool + 心跳独立线程（`3add3b5`）
- `src/daemon/executor.rs` — effective_timeout = min(task.timeout, default) 逻辑
- `bifrost-supervisor.sh` / `bifrost-ctl.sh` — supervisor 生命周期管理
- `bifrost_test/settings.json` — task_timeout=900s、max_concurrent=10

*更新时间: 2026-08-04*
