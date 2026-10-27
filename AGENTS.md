# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, etc.) when working with this repository.

## Project Overview

**Bifrost** — 基于共享存储通信的离线机器命令执行框架。

### 起源与定位

Bifrost 是一个**通用框架**，用于在网络隔离的机器间下发并执行命令：

- **源机器（联网）**：下发命令、调度任务、接收结果
- **目标机器（离线）**：执行命令、返回结果、报告状态
- **Bridge**：传输抽象层，目前实现共享存储，可扩展 SSH 等

任何需要在网络隔离机器间下发并执行命令的场景，Bifrost 都可以胜任。

### 数据流

```
源机器 (联网)          共享存储              目标机器 (离线)
┌──────────────┐    ┌────────────┐    ┌──────────────┐
│ bifrost CLI  │───▶│ commands/  │───▶│   daemon     │
│ 下发命令      │    │ task.json  │    │  监控+执行    │
│              │◀───│ results/   │◀───│              │
│ 拉取结果      │    │ result.json│    │  写入结果     │
│              │    ├────────────┤    │              │
│ 查状态       │────│ status/    │◀───│  实时进度     │
│              │    ├────────────┤    │              │
│ 健康检查     │────│ heartbeat  │◀───│  心跳 60s    │
└──────────────┘    └────────────┘    └──────────────┘
```

### 关键特性

- **通用命令执行**：shell / pytest / 任意自定义命令
- **Bridge 传输抽象**：共享存储已实现，可扩展 SSH 等其他传输
- **双向目录通信**：commands/ 写入任务，results/ 返回结果
- **Rust 高性能**：notify 文件事件监控（500ms），tokio 异步执行
- **安全防护**：shell-words 防注入、路径遍历检测
- **生产级容错**：超时控制、失败重试、心跳检测、panic-safe RAII
- **GPU 感知调度**：nvidia-smi 监控 + round-robin 分配 + CUDA_VISIBLE_DEVICES 隔离
- **pytest 集成**：自动识别 pytest 命令，可解析 JSON 报告
- **批量与 Job**：YAML Job 顺序执行、JSON Manifest 批量提交、进度跟踪
- **Claude Code 友好**：结构化 JSON 输出，便于 AI 分析和后续调度

## 构建与部署

### 环境要求

- **Rust**: 1.87+ (2021 edition, CI 用 stable 工具链；`is_multiple_of` 需 1.87)
- **共享存储**: 源机器 和 目标机器 都能访问的 GPFS 共享目录（也支持 NFS/Lustre 等 POSIX 文件系统）

### 构建

```bash
cargo build --release
# 输出: target/release/bifrost (~8MB 单一二进制)
```

### 部署

```bash
# 源机器 (联网机器)
sudo cp target/release/bifrost /usr/local/bin/
bifrost server --init          # 生成 ~/.bifrost/settings.json

# 目标机器 (离线机器)
sudo cp target/release/bifrost /usr/local/bin/
bifrost server --init
sudo cp bifrost.service /etc/systemd/system/
sudo systemctl enable bifrost
sudo systemctl start bifrost
```

## CLI 命令速查

```bash
# 上线前自检（在 server/目标机上跑）
bifrost warmup                      # 或 bifrost --warmup

# 提交单条命令（自动识别 pytest/shell）
bifrost client submit --command "pytest tests/ -v" --timeout 600
bifrost client submit --command "..." --retries 2   # 重试需显式开启
bifrost client submit --command "..." --timeout 0   # 0 = 继承 daemon 默认
bifrost server --storage /data/liuxin/bifrost       # --storage 覆盖 shared_storage

# 提交 YAML Job（多步骤顺序执行）
bifrost client submit --job job.yaml

# 查询任务状态
bifrost client status <task-id>

# 取消任务
bifrost client cancel <task-id>

# 启动服务端守护进程
bifrost server
bifrost server --config server.json --systemd
```

## 项目结构

```
bifrost/
├── Cargo.toml              # 包清单 (bifrost v0.1.0)
├── src/
│   ├── main.rs             # CLI 入口: client/server 双模式
│   ├── core/               # 共享核心
│   │   ├── models.rs       # Task, TaskResult, BatchProgress, JobDefinition
│   │   ├── bridge.rs      # Bridge 传输抽象 trait + TaskStatusResponse
│   │   ├── protocol.rs     # 共享存储 Bridge 实现 (4 目录)
│   │   ├── settings.rs     # ~/.bifrost/settings.json 配置
│   │   ├── error.rs        # BifrostError 枚举
│   │   ├── lock.rs         # fs2 文件锁 + atomic_write
│   │   ├── warmup.rs       # bifrost warmup 环境自检
│   │   ├── db.rs           # TODO: SQLite 集成（任务历史持久化）
│   │   ├── batch_tracker.rs# 批量进度跟踪
│   │   └── job.rs          # YAML Job 定义
│   ├── client/             # 源机器 端 CLI
│   │   ├── submit.rs       # 任务提交
│   │   ├── status.rs       # 状态查询
│   │   ├── results.rs      # 结果检索 + 路径遍历防护
│   │   └── launcher.rs     # 顺序 Job 执行器
│   └── daemon/             # 目标机器 端守护进程
│       ├── runner.rs       # 主循环
│       ├── watcher.rs      # notify 文件监控 (500ms 防抖)
│       ├── executor.rs     # tokio 命令执行 (超时/截断/隔离)
│       ├── heartbeat.rs    # 心跳检测
│       ├── logger.rs       # 日志管理
│       ├── gpu_monitor.rs  # nvidia-smi GPU 状态
│       ├── gpu_pool.rs     # 在用：每卡一个租约的 GPU 池
│       ├── clock_probe.rs  # 启动时钟自检（决定 scan_mode）
│       ├── gpu_scheduler.rs# 旧轮询调度（未接线，待删）
│       └── gpu_task_processor.rs # GPU 完整生命周期 (RAII)
├── tests/
│   ├── unit/               # 10 个单元测试
│   └── integration/        # 2 个集成测试
├── config/                 # 配置模板
├── examples/               # YAML Job 示例 (pytest, benchmark, smoke)
├── scripts/                # 部署脚本
│   ├── find-binary.sh      # 解析 cargo 真实产物路径（尊重 CARGO_TARGET_DIR）
│   ├── watchdog.sh         # 外部看门狗（心跳/扫描信标年龄 → 重启）
│   └── h200-smoke.sh       # 目标机一条命令跑完 9 项行为验证
├── docs/                   # adapter-guide.md, deployment.md, ops-practice.md
└── adapters/               # pytest_template.yaml
```

## 配置

### ~/.bifrost/settings.json

```json
{
  "shared_storage": "/mnt/gpfs/bifrost",
  "client": {
    "poll_interval": "2s",
    "heartbeat_timeout": "180s"
  },
  "daemon": {
    "poll_interval": "500ms",
    "task_timeout": "300s",
    "max_retries": 3,
    "heartbeat_interval": "60s",
    "max_concurrent": 10,
    "working_dir": "/tmp/bifrost/work"
  }
}
```

## 架构决策

### 为什么选择 Rust？

| 考量 | Rust | Python |
|------|------|--------|
| 监控延迟 | 500ms（事件驱动） | 2s（定时轮询） |
| 并发 | 无 GIL，100+ 任务 | GIL 限制 |
| 稳定性 | 无 GC 暂停 | GC 可能影响实时性 |
| 部署 | 单一二进制，零依赖 | 需 Python + 依赖包 |
| 离线环境 | 直接运行二进制 | 需预装环境 |

### 为什么选择文件通信而非数据库队列？

- **双向分离**：commands/ 和 results/ 由不同机器写入，天然解耦
- **零依赖**：核心通信无需数据库
- **易调试**：直接查看 JSON 文件内容
- **容错简单**：文件损坏只影响单个任务

### 为什么选择 notify 文件事件而非定时轮询？

- 500ms 响应（事件触发）vs 2s 响应（定时轮询）
- 空闲时零 CPU 占用
- 任务提交后立即检测

## 测试

```bash
cargo test --all                    # 全部测试（218 个用例）
cargo test --lib                    # 仅单元测试
cargo test --test full_workflow_test # 集成测试
cargo test --test coverage_boost_test # tests/unit 下的用例（需在 Cargo.toml 用 [[test]] 声明）
```

> 注意：`tests/unit/*.rs`、`tests/integration/*.rs` 这些**子目录里的测试不会被 cargo 自动发现**，
> 必须在 `Cargo.toml` 里逐个写 `[[test]] name/path`。历史上漏声明 → 77 个用例长期不编译也没人发现。
> 加新测试文件时记得补声明，否则等于没写。

CI（GitHub Actions）：`.github/workflows/ci.yml` 跑 `gates`（fmt/clippy/cargo test --all）→ `e2e`
（`tests/e2e/run_all.py` 6 个场景）；`.github/workflows/release.yml` 在 main 推送时发 `bifrost-latest`
快照、在 `release/x.y.z` 推送时发正式 Release（`gh release` + `scripts/ci/release.sh`）。详见 `docs/RELEASE.md`。

## 常见问题

1. **守护进程检测不到新任务** → 检查共享存储权限，确认 notify 正常工作
2. **命令执行失败** → 检查 目标机器 上相应环境（Python/pytest 等）
3. **心跳超时** → 检查 目标机器 守护进程是否运行，heartbeat.json 是否可读写
4. **文件锁冲突** → 确认共享存储支持文件锁（NFS 需 lockd 服务）
