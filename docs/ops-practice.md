# Bifrost 运维实践记录（APMM UT 场景）

> 记录 2026-08-03 在 APMM UT Workflow 接入 bifrost 过程中的问题、优化与运维方案。
> 场景：本机（npu-062）作为 Client，H20（infra-gpu-h20-022）作为离线 Daemon，
> 通过 GPFS 共享存储（`/gpfs/gcsp/liuxin/bifrost_test`）通信。

---

## 1. 接入背景

APMM 的 UT Workflow 远程执行原用 `tools/agent.py`（SSH Bastion 双跳隧道），
存在断连频繁、串行执行慢的问题。改为 bifrost（GPFS 文件交换）后：

- 无 SSH 隧道，无"连接"概念，天然抗断连
- daemon 并发 10 任务
- task_id 全生命周期追踪

---

## 2. 关键 Bug 修复记录

### 2.1 跨机器 spawn 失败（`No such file or directory`）

**症状**：提交任务后 daemon 报 `Failed to spawn process: No such file or directory (os error 2)`。

**根因**：`Task::new()` 的默认 `working_dir` 用 `std::env::current_dir()`，
序列化的是**提交端（本机）**路径（如 `/root/.hermes/...`）。发到 H20 后目录
不存在，executor 的 `cmd.current_dir(&task.working_dir)` 直接失败。

**修复**：
- `src/core/models.rs`：默认 `working_dir` 改为 `PathBuf::from(".")`（继承 daemon cwd）
- `src/daemon/executor.rs`：`working_dir == "."` 时不调 `cmd.current_dir()`
- `src/daemon/runner.rs`：启动时 `mkdir -p` daemon working_dir 并 `set_current_dir`

**排查要点**：
- MCP server 是**常驻进程**，改 binary 后必须 kill 旧 `mcp-serve` 进程，
  Hermes 才会用新 binary 重新拉起（本问题排查卡了 3 轮就因此）。
- 排查时在 executor 加 `eprintln!("[executor] spawning: ...")` 是最快定位手段。

### 2.2 CLI client 不读 BIFROST_CONFIG

**症状**：`remote_executor.py` 用 `bifrost client submit` 提交后任务一直 Pending，
daemon 永远不消费。

**根因**：`BIFROST_CONFIG` 环境变量只在 MCP server 模式（`handle_mcp_serve`）
被读取；CLI client（`handle_client_mode`）只读 `~/.bifrost/settings.json`。
本机 `~/.bifrost/settings.json` 残留着测试配置
（`shared_storage=/dev/shm/bifrost_e2e_robust_804_bo_6`），任务写到那里，
daemon 监控的却是 `bifrost_test`，永远消费不到。

**修复**：`src/main.rs` 的 `handle_client_mode` 同样按
`BIFROST_CONFIG env > ~/.bifrost/settings.json` 解析配置。

---

## 3. 耗时优化

### 3.1 目标

端到端耗时（submit → result）≈ server 执行任务时间，开销控制在很小。

### 3.2 优化项

| 优化 | 前 | 后 | 改动 |
|---|---|---|---|
| Client 轮询 | 每 2s spawn `bifrost client status` | 每 0.2s stat GPFS result 文件 | `tools/remote_executor.py`（APMM 侧）|
| Daemon fallback scan | 5s 固定 | 100ms | `FALLBACK_SCAN_INTERVAL` |
| Daemon 主循环 sleep | 500ms 写死 | 与 fallback 一致（100ms）| `runner.rs` select! |
| 消费延迟 | ~5s | ~400ms（fallback 100ms 版）| 上述组合 |

**效果**：端到端开销占比从 60%+ 降到 **4-6%**（submit 30ms + 消费 ~400ms + 发现 ~200ms）。

### 3.3 GPFS inotify 不可靠

GPFS 对 `atomic_write`（tmp 文件 + rename）的 inotify 事件传递不稳定，
rename 事件经常丢失，任务靠 fallback scan 兜底。因此：
- fallback scan 间隔要足够小（100ms，readdir 开销可忽略）
- 不要依赖 inotify 作为唯一消费路径

---

## 4. Supervisor 守护进程

### 4.1 为什么需要

- H20 是离线节点，SSH 上去操作麻烦
- 本机无法直接 `kill` H20 上的进程（信号不能跨机器）
- server 崩溃需要自动恢复
- 改代码编译后需要一条命令重启 server

### 4.2 架构

```
本机 (npu-062)                           H20 (h20-022)
┌──────────────────────┐                ┌──────────────────────┐
│ bifrost-ctl.sh       │                │ bifrost-supervisor.sh│
│   restart|stop|status│ ── control.json─▶  每 2s 轮询 + 执行   │
│        │             │◀─ status.json ──│        │             │
│        └─────────────│                 │  ┌─────┴─────┐       │
│                      │                 │  │ bifrost server │  │
└──────────────────────┘                 │  │ (setsid 独立组) │  │
                                         │  └─────────────┘    │
                                         └──────────────────────┘
```

**跨机器控制**：本机写 `control.json`（GPFS），H20 supervisor 每 2s 轮询，
读到指令执行。`status` 指令回写 `status.json` 供本机读取。

### 4.3 脚本

| 脚本 | 位置 | 用途 |
|---|---|---|
| `bifrost-supervisor.sh` | H20 | 守护进程：server 生命周期管理 |
| `bifrost-ctl.sh` | 本机 | 跨机器控制（restart/stop/status）|
| `install-supervisor-cron.sh` | H20 | crontab @reboot 开机自启 |
| `restart_server.sh` | H20 | 轻量版一键重启（无 supervisor 时用）|

### 4.4 使用方式

```bash
# H20 一次性安装
./bifrost-supervisor.sh start
./install-supervisor-cron.sh        # 可选: 开机自启

# 本机日常操作 (无需 SSH)
./bifrost-ctl.sh restart    # 改代码编译后重启 server
./bifrost-ctl.sh status     # 查状态
./bifrost-ctl.sh stop       # 关闭
```

### 4.5 健壮性特性

| 特性 | 机制 |
|---|---|
| 长期常驻 | nohup 后台，SSH 断开不影响 |
| 崩溃自愈 | 2s 健康检查 + 指数退避重试（上限 60s）|
| 单实例锁 | flock 防重复启动 |
| 日志轮转 | 5MB 自动归档，保留 3 份 |
| 控制容错 | 坏 JSON/未知指令忽略 |
| 优雅停止 | TERM 等 5s，超时 SIGKILL |
| 开机自启 | crontab @reboot |

### 4.6 孤儿进程防护（杀不死问题）

**风险**：executor 用 `process_group(0)` 让任务子进程与 daemon 同进程组，
daemon 被 kill 时若不清进程组，正在跑的 pytest 会孤儿化继续占 GPU。

**3 层防护**：
1. **setsid 启动 daemon** → daemon 是独立进程组 leader（pgid==pid），
   `kill -TERM -- -pid` 能杀整个进程组
2. **stop_server 双保险** → TERM 进程组 → 5s 后 SIGKILL 进程组 → pkill 残留 docker exec
3. **启动时清理** → attach 启动前 pkill 上次残留的孤儿任务进程

---

## 5. 性能验证数据

| 测试 | e2e | daemon 执行 | 开销占比 |
|---|---|---|---|
| test_merge_attn_states | 11.5s | 10.9s | 5% |
| test_reshape_and_cache | 11.8s | 11.4s | 4% |
| test_merge_kernel | 11.5s | 10.9s | 5% |
| test_linear_decode_forward_triton | 25.7s | 24.8s | 4% |
| test_mha_attn_platform | 10.9s | 10.3s | 6% |

同一测试连跑 3 次：11.5 / 11.9 / 11.5s（稳定）。

---

*Last updated: 2026-08-03*
