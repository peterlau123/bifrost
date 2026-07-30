# AGENTS.md

This file provides guidance to AI coding agents (Claude Code, etc.) when working with this repository.

## Project Overview

**Bifrost** — 基于共享存储通信的离线机器命令执行框架。

### 起源与定位

Bifrost 起源于 vLLM/SGLang 在华为昇腾（MACA）硬件上的 pytest 测试场景，但它不局限于 pytest，而是一个**通用框架**：

- **Ascend 机器（t_ascend，联网）**：下发命令、调度任务、接收结果
- **H20 机器（t_h20，离线）**：执行命令、返回结果、报告状态
- **共享存储**：双向目录通信，无需 SSH / RPC / REST API

任何需要在网络隔离机器间下发并执行命令的场景，Bifrost 都可以胜任。

### 数据流

```
Ascend (联网)          共享存储              H20 (离线)
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
- **双向目录通信**：commands/ 写入任务，results/ 返回结果
- **Rust 高性能**：notify 文件事件监控（500ms），tokio 异步执行
- **安全防护**：shell-words 防注入、路径遍历检测
- **生产级容错**：超时控制、失败重试、心跳检测、panic-safe RAII
- **GPU 感知调度**：nvidia-smi 监控 + round-robin 分配 + CUDA_VISIBLE_DEVICES 隔离
- **pytest 集成**：自动添加 --json-report，解析 JSON 报告，存储到 SQLite
- **批量与 Job**：YAML Job 顺序执行、JSON Manifest 批量提交、进度跟踪
- **SQLite 历史**：可选的任务/输出/环境变量/产物/pytest 结果持久化
- **Claude Code 友好**：结构化 JSON 输出，便于 AI 分析和后续调度

## 构建与部署

### 环境要求

- **Rust**: 1.70+ (2021 edition)
- **共享存储**: Ascend 和 H20 都能访问的目录（USB/NFS/rsync）

### 构建

```bash
cargo build --release
# 输出: target/release/bifrost (~8MB 单一二进制)
```

### 部署

```bash
# Ascend (联网机器)
sudo cp target/release/bifrost /usr/local/bin/
bifrost client init          # 生成 ~/.bifrost/settings.json

# H20 (离线机器)
sudo cp target/release/bifrost /usr/local/bin/
bifrost daemon --init
sudo ./scripts/systemd-setup.sh
sudo systemctl enable bifrost
sudo systemctl start bifrost
```

## CLI 命令速查

```bash
# 提交单条命令（自动识别 pytest/shell）
bifrost client submit --command "pytest tests/ -v" --timeout 600

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
│   │   ├── protocol.rs     # 文件通信协议 (4 目录)
│   │   ├── settings.rs     # ~/.bifrost/settings.json 配置
│   │   ├── error.rs        # BifrostError 枚举
│   │   ├── lock.rs         # fs2 文件锁 + atomic_write
│   │   ├── db.rs           # TODO: SQLite 集成（任务历史持久化）
│   │   ├── batch_tracker.rs# 批量进度跟踪
│   │   └── job.rs          # YAML Job 定义
│   ├── client/             # Ascend 端 CLI
│   │   ├── submit.rs       # 任务提交
│   │   ├── status.rs       # 状态查询
│   │   ├── results.rs      # 结果检索 + 路径遍历防护
│   │   ├── pytest.rs       # pytest 命令构建 + 报告解析
│   │   └── launcher.rs     # 顺序 Job 执行器
│   └── daemon/             # H20 端守护进程
│       ├── runner.rs       # 主循环
│       ├── watcher.rs      # notify 文件监控 (500ms 防抖)
│       ├── executor.rs     # tokio 命令执行 (超时/截断/隔离)
│       ├── heartbeat.rs    # 心跳检测
│       ├── logger.rs       # 日志管理
│       ├── gpu_monitor.rs  # nvidia-smi GPU 状态
│       ├── gpu_scheduler.rs# 轮询 GPU 调度
│       └── gpu_task_processor.rs # GPU 完整生命周期 (RAII)
├── tests/
│   ├── unit/               # 10 个单元测试
│   └── integration/        # 2 个集成测试
├── config/                 # 配置模板
├── examples/               # YAML Job 示例 (pytest, benchmark, smoke)
├── scripts/                # 部署脚本
├── docs/                   # ADAPTER_GUIDE.md, DEPLOYMENT.md
└── adapters/               # pytest_template.yaml
```

## 配置

### ~/.bifrost/settings.json

```json
{
  "shared_storage": "/mnt/shared",
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
- **零依赖**：核心通信无需 SQLite
- **易调试**：直接查看 JSON 文件内容
- **容错简单**：文件损坏只影响单个任务

### 为什么选择 notify 文件事件而非定时轮询？

- 500ms 响应（事件触发）vs 2s 响应（定时轮询）
- 空闲时零 CPU 占用
- 任务提交后立即检测

## 测试

```bash
cargo test --all                    # 全部测试
cargo test --lib                    # 仅单元测试
cargo test --test full_workflow_test # 集成测试
```

## 常见问题

1. **守护进程检测不到新任务** → 检查共享存储权限，确认 notify 正常工作
2. **命令执行失败** → 检查 H20 上相应环境（Python/pytest 等）
3. **心跳超时** → 检查 H20 守护进程是否运行，heartbeat.json 是否可读写
4. **文件锁冲突** → 确认共享存储支持文件锁（NFS 需 lockd 服务）
