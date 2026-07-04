# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

**offline-executor** - A Rust-based framework for executing commands on offline machines through shared storage communication. Designed for scenarios where network-isolated machines need to run pytest tests or other commands under the control of networked machines.

### Key Features

- **双向分离目录通信**：联网机器写入commands/目录，离线机器写入results/目录
- **Rust高性能实现**：文件事件监控（500ms延迟）、异步执行（tokio）、serde JSON解析
- **pytest深度集成**：支持批量测试、失败重试、覆盖率报告、JSON结果解析
- **Claude Code友好**：结构化JSON输出，便于AI分析和后续任务调度
- **生产级容错**：超时控制、崩溃恢复、失败重试、心跳检测

## System Architecture

```
联网机器 (t_ascend)          共享存储              离线机器 (t_h20)
┌──────────────┐         ┌────────────┐         ┌──────────────┐
│ CLI工具(Rust)│────────>│commands/   │────────>│守护进程(Rust)│
│              │         │{task}.json │         │              │
│ SQLite索引   │         ├────────────┤         │ subprocess   │
│ (可选)       │<────────│results/    │<────────│ pytest调用   │
│              │         │{result}.json│         │              │
└──────────────┘         ├────────────┤         └──────────────┘
                         │status/     │
                         │heartbeat.  │
                         └────────────┘
```

**数据流向**：
1. **任务提交**：CLI写入commands/{timestamp}_{task_id}.json → 守护进程检测新文件
2. **任务执行**：守护进程调用pytest → 实时写入status/{task_id}.json进度
3. **结果返回**：守护进程写入results/{task_id}_result.json → CLI检测结果
4. **心跳检测**：守护进程每60秒更新heartbeat.json → CLI判断离线机器状态

## Project Structure

```
offline-executor/
├── client/                 # 联网机器CLI工具
│   ├── src/
│   │   ├── main.rs        # CLI入口
│   │   ├── submit.rs      # 任务提交逻辑
│   │   ├── status.rs      # 状态查询
│   │   ├── results.rs     # 结果获取
│   │   └── db.rs          # SQLite索引管理（可选）
│   └── Cargo.toml
├── daemon/                 # 离线机器守护进程
│   ├── src/
│   │   ├── main.rs        # 守护进程入口
│   │   ├── watcher.rs     # 文件事件监控（notify库）
│   │   ├── executor.rs    # 命令执行引擎（tokio异步）
│   │   ├── pytest.rs      # pytest集成模块
│   │   └── heartbeat.rs   # 心跳检测
│   └── Cargo.toml
├── core/                   # 共享核心逻辑
│   ├── src/
│   │   ├── models.rs      # 数据模型（Task、Result等）
│   │   ├── protocol.rs    # 通信协议定义
│   │   ├── lock.rs        # 文件锁机制（fs2库）
│   │   └── config.rs      # 配置管理
│   └── Cargo.toml
├── tests/                  # 测试用例
│   ├── integration/       # 集成测试
│   └── fixtures/          # 测试数据
├── docs/                   # 文档
│   ├── design.md          # 设计文档
│   ├── usage.md           # 使用指南
│   └── pytest-integration.md # pytest集成说明
├── config/                 # 配置文件模板
│   ├── client.yaml        # CLI配置模板
│   └── daemon.yaml        # 守护进程配置模板
├── scripts/                # 部署脚本
│   ├── install.sh         # 安装脚本
│   ├── systemd-setup.sh   # systemd服务配置
│   └── health-check.sh    # 健康检查脚本
├── Cargo.toml             # Workspace根配置
├── CLAUDE.md              # 本文件
└── README.md              # 项目介绍
```

## Development Workflow

### 环境要求

- **Rust**: 1.70+ (2021 edition)
- **Python**: 3.10+ (pytest环境，离线机器需要)
- **pytest插件**: pytest-json-report（生成结构化结果）
- **共享存储**: 联网机器和离线机器都能访问的目录

### 构建命令

```bash
# 构建所有组件
cargo build --release

# 构建CLI工具（联网机器）
cargo build --release -p offline-client

# 构建守护进程（离线机器）
cargo build --release -p offline-daemon

# 运行测试
cargo test --all

# 生成文档
cargo doc --open
```

### 开发阶段（4周计划）

| Phase | 时间 | 目标 |
|-------|------|------|
| **Phase 1** | Week 1-2 | 核心框架：CLI工具、守护进程、文件通信、心跳检测 |
| **Phase 2** | Week 3 | pytest集成：subprocess调用、JSON报告解析、批量测试 |
| **Phase 3** | Week 3 | 高级特性：失败重试、超时控制、实时进度、优先级队列 |
| **Phase 4** | Week 4 | Claude Code集成：JSON格式优化、文档、部署脚本 |

### 测试策略

```bash
# 单元测试（每个模块）
cargo test -p offline-core --lib
cargo test -p offline-client --lib
cargo test -p offline-daemon --lib

# 集成测试（模拟双机器环境）
cargo test --test integration_tests

# pytest功能测试（实际调用pytest）
cargo test --test pytest_integration
```

## Configuration

### CLI配置（client.yaml）

```yaml
shared_storage: "/path/to/shared"
database: "tasks.db"          # SQLite索引（可选）
poll_interval: 2s             # 结果轮询间隔
heartbeat_timeout: 180s       # 心跳超时阈值
```

### 守护进程配置（daemon.yaml）

```yaml
shared_storage: "/path/to/shared"
poll_interval: 500ms          # 文件监控延迟（基于事件）
task_timeout: 300s            # 任务执行超时
max_retries: 3                # 失败重试次数
heartbeat_interval: 60s       # 心跳更新频率
max_concurrent: 10            # 最大并发任务数
```

## Key Dependencies

### Rust核心库

```toml
[dependencies]
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.0", features = ["full"] }
notify = "5.0"                # 文件事件监控
fs2 = "0.4"                   # 文件锁
chrono = "0.4"                # 时间处理
uuid = { version = "1.0", features = ["v4"] }
clap = { version = "4.0", features = ["derive"] }  # CLI参数解析
rusqlite = { version = "0.28", optional = true }   # SQLite（可选）
subprocess = "0.2"            # 进程调用（守护进程）
```

### Python pytest插件（离线机器需要）

```bash
pip install pytest pytest-json-report pytest-cov
```

## Usage Examples

### 提交单个pytest任务

```bash
# 联网机器执行
offline-ctl submit --cmd "pytest tests/test_inference.py -v"

# 查询状态
offline-ctl status <task_id>

# 获取结果
offline-ctl results <task_id>

# 输出结构化JSON（Claude Code解析）
{
  "task_id": "abc123",
  "status": "completed",
  "return_code": 0,
  "stdout": "...",
  "stderr": "",
  "artifacts": ["report.json", "coverage.xml"],
  "pytest_summary": {
    "passed": 15,
    "failed": 2,
    "skipped": 3
  }
}
```

### 批量测试调度

```bash
# 批量提交测试文件列表
offline-ctl batch --file test_list.txt --retry 3 --timeout 300

# test_list.txt内容
tests/test_inference.py
tests/test_scheduler.py
tests/test_memory.py
```

### 实时进度监控

```bash
# 查询执行进度（守护进程写入status/目录）
offline-ctl progress <task_id>

# 输出
{
  "task_id": "abc123",
  "progress": 45,
  "current_file": "test_scheduler.py",
  "elapsed": "120s",
  "estimated_remaining": "180s"
}
```

## Integration with Claude Code

### Claude Code工作流程

```
Claude Code → offline-ctl submit → 离线机器执行 → Claude Code接收结果 → Claude Code分析失败 → Claude Code调度修复任务
```

**典型场景**：
1. Claude Code批量提交100个pytest测试
2. 离线机器并发执行10个测试
3. 3个测试失败，Claude Code解析错误堆栈
4. Claude Code自动生成修复代码
5. Claude Code重新提交失败的3个测试验证修复

### JSON输出格式（Claude Code解析）

```json
{
  "task_id": "uuid",
  "command": "pytest tests/test_x.py",
  "status": "completed|failed|timeout",
  "return_code": 0,
  "stdout": "full output",
  "stderr": "error messages",
  "artifacts": [
    {"type": "pytest_report", "path": "report.json"},
    {"type": "coverage", "path": "coverage.xml"}
  ],
  "pytest_summary": {
    "passed": 10,
    "failed": 2,
    "skipped": 1,
    "errors": [
      {"test": "test_func", "error": "AssertionError...", "file": "tests/test_x.py:42"}
    ]
  },
  "execution_time": "45s",
  "timestamp": "2026-07-04T10:30:00Z"
}
```

## Deployment

### systemd守护进程配置（离线机器）

```bash
# 安装守护进程二进制
sudo cp target/release/offline-daemon /usr/local/bin/

# 配置systemd服务
sudo scripts/systemd-setup.sh

# 启动服务
sudo systemctl start offline-executor
sudo systemctl enable offline-executor

# 查看状态
sudo systemctl status offline-executor
```

### 健康检查

```bash
# 检查守护进程状态
scripts/health-check.sh

# 输出
{
  "daemon_running": true,
  "heartbeat_age": "30s",
  "pending_tasks": 5,
  "active_tasks": 2,
  "completed_today": 15
}
```

## Troubleshooting

### 常见问题

1. **守护进程无法检测新任务**
   - 检查共享存储路径权限
   - 确认notify库正常工作（Linux: inotify，Windows: ReadDirectoryChangesW）

2. **pytest调用失败**
   - 认离线机器已安装pytest和pytest-json-report
   - 检查Python环境路径配置

3. **心跳超时**
   - 检查离线机器守护进程是否正常运行
   - 确认heartbeat.json文件权限（双方都可读）

4. **文件锁冲突**
   - 使用fs2库的file_lock确保原子性
   - 确认共享存储支持文件锁机制

## Related Projects

This project is part of the workspace containing:
- **vllm**: vLLM inference engine
- **sglang**: SGLang serving framework
- **transformers**: HuggingFace transformers
- **vllm-adapt-agent**: MACA hardware adaptation

See workspace root `CLAUDE.md` for details.

## Architecture Decisions

### 为什么选择Rust而非Python？

| 考量 | Rust优势 | Python劣势 |
|------|---------|-----------|
| **性能** | 500ms轮询延迟（基于文件事件） | 2秒轮询延迟（定时轮询） |
| **并发** | 无GIL限制，可同时执行100+任务 | GIL限制，并发任务数受限 |
| **稳定性** | 无GC暂停，长期运行稳定 | GC暂停可能影响实时性 |
| **部署** | 单一二进制，零依赖 | 需要Python环境和依赖包 |
| **离线环境** | 无需安装Rust，直接运行二进制 | 需要预先安装Python和pytest插件 |

### 为什么选择文件轮询而非SQLite队列？

- **单向分离目录约束**：commands/和results/分别由不同机器写入
- **零依赖通信**：核心通信无需SQLite，仅作为可选索引
- **易于调试**：直接查看JSON文件内容，无需数据库工具
- **容错简单**：文件损坏只影响单个任务，不影响全局队列

### 为什么选择notify文件事件监控而非定时轮询？

- **性能提升4倍**：500ms响应（事件触发）vs 2秒响应（定时轮询）
- **资源消耗低**：仅在文件变化时唤醒，空闲时零CPU占用
- **实时性强**：任务提交后立即检测，框架自身延迟最小化

## Future Extensions

- **Web UI监控面板**：实时显示任务队列、执行进度、历史统计
- **分布式执行**：多个离线机器协同执行，负载均衡
- **任务依赖图**：支持任务之间的依赖关系（DAG调度）
- **AI调度优化**：Claude Code根据历史数据优化测试顺序
- **缓存机制**：相同命令的结果缓存，避免重复执行

## References

- Design Document: `docs/design.md`
- Usage Guide: `docs/usage.md`
- pytest Integration: `docs/pytest-integration.md`
- API Documentation: `cargo doc --open`