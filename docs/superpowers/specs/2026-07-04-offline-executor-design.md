# offline-executor Design Document

**Date**: 2026-07-04
**Author**: Claude Code brainstorming session
**Status**: Approved for implementation

---

## Executive Summary

**offline-executor** is a Rust-based framework for executing commands on offline machines through shared storage communication. It enables networked machines (t_ascend) to submit pytest tests or other commands to network-isolated machines (t_h20) via shared filesystem, with structured JSON results returned for AI analysis.

### Key Design Decisions

1. **Single Binary Architecture**: One executable runs in client or daemon mode via CLI parameters
2. **Dual-Separated Directory Communication**: commands/ (client writes) and results/ (daemon writes)
3. **Rust High-Performance Implementation**: notify file events (500ms latency), tokio async execution, serde JSON
4. **Optional SQLite Index**: Client-side task history indexing, not used in core communication
5. **pytest Deep Integration**: Structured error extraction for Claude Code analysis

---

## 1. System Architecture

### 1.1 Single Binary Design

```
offline-executor (单一二进制)
├── client mode  (联网机器启动: offline-executor client ...)
└── daemon mode  (离线机器启动: offline-executor daemon ...)
```

**Architecture Diagram**:

```
联网机器 (t_ascend)          共享存储              离线机器 (t_h20)
┌──────────────┐         ┌────────────┐         ┌──────────────┐
│ offline-     │         │commands/   │         │ offline-     │
│ executor     │────────>│{task}.json │────────>│ executor     │
│ client mode  │         │            │         │ daemon mode  │
│              │         ├────────────┤         │              │
│ SQLite索引   │<────────│results/    │<────────│ subprocess   │
│ (可选)       │         │{result}.json│         │ pytest调用   │
│              │         ├────────────┤         │              │
└──────────────┘         │status/     │         └──────────────┘
                         │heartbeat.  │
                         └────────────┘
```

### 1.2 Communication Flow

**Data Flow Sequence**:

1. **Task Submission**: Client writes `commands/{timestamp}_{task_id}.json` → Daemon detects file event
2. **Task Execution**: Daemon calls pytest → Real-time writes `status/{task_id}.json`
3. **Result Return**: Daemon writes `results/{task_id}_result.json` → Client detects result
4. **Heartbeat Detection**: Daemon updates `heartbeat.json` every 60s → Client checks 180s timeout

**Status Transitions**:

```
PENDING (commands/) → RUNNING (status/) → COMPLETED (results/)
                    ↓
                  FAILED (retry or permanent failure)
                    ↓
                  TIMEOUT (task execution timeout)
```

### 1.3 Shared Storage Directory Structure

```
/shared/storage/
├── commands/              # Client writes, daemon reads
│   ├── 20260704103000_550e8400-e29b-41d4-a716-446655440000.json
│   └── 20260704103005_6ba7b810-9dad-11d1-80b4-00c04fd430c8.json
├── results/               # Daemon writes, client reads
│   ├── 550e8400-e29b-41d4-a716-446655440000_result.json
│   └── 6ba7b810-9dad-11d1-80b4-00c04fd430c8_result.json
├── status/                # Daemon writes progress updates
│   ├── 550e8400-e29b-41d4-a716-446655440000.json
│   └── 6ba7b810-9dad-11d1-80b4-00c04fd430c8.json
└── heartbeat.json         # Daemon heartbeat (every 60s)
```

---

## 2. Core Data Models

### 2.1 Task Definition (`commands/*.json`)

```json
{
  "task_id": "550e8400-e29b-41d4-a716-446655440000",
  "timestamp": "2026-07-04T10:30:00Z",
  "command": "pytest tests/test_inference.py -v --json-report",
  "task_type": "pytest",
  "priority": 0,
  "timeout": 300,
  "retry_count": 3,
  "env_vars": {
    "PYTHONPATH": "/path/to/project",
    "TEST_DATA_DIR": "/path/to/data"
  },
  "working_dir": "/path/to/workspace",
  "artifacts_expected": ["report.json", "coverage.xml"],
  "metadata": {
    "submitted_by": "claude-code",
    "purpose": "verify_inference_fix",
    "related_files": ["src/inference.rs:42"]
  }
}
```

**Field Descriptions**:

| Field | Type | Description | Required |
|-------|------|-------------|----------|
| `task_id` | UUID v4 | Unique task identifier | ✅ |
| `timestamp` | ISO 8601 | Submission timestamp | ✅ |
| `command` | String | Shell command to execute | ✅ |
| `task_type` | Enum | pytest/shell/custom | ✅ |
| `priority` | u8 | 0=normal, 1=high, 2=urgent | ❌ (default: 0) |
| `timeout` | u64 | Execution timeout in seconds | ❌ (default: 300) |
| `retry_count` | u8 | Number of retries on failure | ❌ (default: 3) |
| `env_vars` | HashMap | Environment variables | ❌ |
| `working_dir` | PathBuf | Working directory | ❌ (default: current dir) |
| `artifacts_expected` | Vec<String> | Expected artifact files | ❌ |
| `metadata` | HashMap | Custom metadata for Claude Code | ❌ |

### 2.2 Execution Status (`status/{task_id}.json`)

```json
{
  "task_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "running",
  "progress": 45,
  "current_step": "Executing test_scheduler.py",
  "started_at": "2026-07-04T10:30:05Z",
  "elapsed_seconds": 120,
  "estimated_remaining": 180,
  "pid": 12345,
  "heartbeat": "2026-07-04T10:32:05Z"
}
```

**Update Frequency**: Every 10 seconds or when test file changes

### 2.3 Execution Result (`results/{task_id}_result.json`)

**核心数据模型（通用）**：

```json
{
  "task_id": "550e8400-e29b-41d4-a716-446655440000",
  "status": "completed",
  "return_code": 0,
  "stdout": "=== test session starts ===\ncollected 15 Items\n...",
  "stderr": "",
  "execution_time": "45s",

  "task_output": {
    "type": "pytest",
    "report_file": "logs/550e8400-e29b-41d4-a716-446655440000/pytest_report.json",
    "log_file": "logs/550e8400-e29b-41d4-a716-446655440000/execution.log",
    "artifacts": [
      {
        "type": "pytest_report",
        "path": "logs/550e8400-e29b-41d4-a716-446655440000/report.json",
        "size": 2048
      },
      {
        "type": "coverage",
        "path": "logs/550e8400-e29b-41d4-a716-446655440000/coverage.xml",
        "size": 5120
      }
    ]
  },

  "metadata": {
    "completed_at": "2026-07-04T10:30:50Z",
    "machine": "t_h20",
    "python_version": "3.10.5",
    "pytest_version": "7.2.0"
  }
}
```

**Field Descriptions**:

| Field | Type | Description | Required |
|-------|------|-------------|----------|
| `task_id` | UUID v4 | Unique task identifier | ✅ |
| `status` | Enum | completed/failed/timeout/cancelled | ✅ |
| `return_code` | i32 | Process exit code | ✅ |
| `stdout` | String | Brief stdout output (truncated) | ✅ |
| `stderr` | String | stderr messages | ❌ |
| `execution_time` | String | Total execution time | ✅ |
| `task_output` | Object | Task-specific output file references | ✅ |
| `task_output.type` | String | Task type (pytest/shell/custom) | ✅ |
| `task_output.report_file` | Path | Detailed report file path | ✅ |
| `task_output.log_file` | Path | Execution log file path | ✅ |
| `task_output.artifacts` | Array | Artifact file list | ❌ |
| `metadata` | HashMap | Execution metadata | ❌ |

**设计说明**：

- `stdout`字段只存储简要输出（如pytest的collect信息），详细输出存放在`log_file`中
- `task_output`字段存储任务专用输出文件的路径引用，实现核心模型与具体任务解耦
- 不同任务类型的详细报告格式不同，通过独立文件存储，核心模型保持通用性

### 2.4 Task-Specific Output Files

**pytest任务输出文件**（`logs/{task_id}/pytest_report.json`）：

```json
{
  "total": 15,
  "passed": 13,
  "failed": 2,
  "skipped": 0,
  "duration": 45.2,
  "errors": [
    {
      "test_name": "test_inference_accuracy",
      "file": "tests/test_inference.py",
      "line": 42,
      "error_type": "AssertionError",
      "error_message": "Expected 0.95, got 0.87",
      "traceback": "AssertionError: ...\n  at test_inference.py:42"
    }
  ],
  "test_list": [
    {
      "name": "test_inference_accuracy",
      "outcome": "failed",
      "duration": 3.2
    },
    {
      "name": "test_scheduler_policy",
      "outcome": "passed",
      "duration": 2.1
    }
  ]
}
```

**shell任务输出文件**（`logs/{task_id}/shell_report.json`）：

```json
{
  "command": "python scripts/process_data.py",
  "exit_code": 0,
  "duration": 120.5,
  "output_summary": "Processed 1000 files successfully",
  "errors": []
}
```

**custom任务输出文件**（`logs/{task_id}/custom_report.json`）：

```json
{
  "custom_type": "model_inference",
  "metrics": {
    "accuracy": 0.95,
    "latency": 45.2,
    "throughput": 1000
  },
  "output_files": ["result.json", "metrics.csv"]
}
```

### 2.5 Log Directory Structure

```
/shared/storage/logs/
├── 550e8400-e29b-41d4-a716-446655440000/     # Task ID directory
│   ├── execution.log                          # Full stdout/stderr log
│   ├── pytest_report.json                     # pytest detailed report
│   ├── report.json                            # pytest-json-report output
│   ├── coverage.xml                           # Coverage report
│   └── artifacts/                             # Other artifacts
│       ├── test_output.txt
│       └── screenshots/
├── 6ba7b810-9dad-11d1-80b4-00c04fd430c8/
│   ├── execution.log
│   ├── shell_report.json
│   └── artifacts/
```

**日志清理策略**：

- 成功任务：保留7天后自动清理
- 失败任务：保留30天（便于Claude Code分析历史失败）
- 可手动清理：`offline-executor client cleanup --days 7`

### 2.4 Heartbeat (`heartbeat.json`)

```json
{
  "machine_id": "t_h20",
  "timestamp": "2026-07-04T10:32:00Z",
  "status": "healthy",
  "active_tasks": 2,
  "pending_tasks": 5,
  "completed_today": 15,
  "failed_today": 3,
  "cpu_usage": 45.2,
  "memory_usage": 62.8,
  "disk_available": "50GB"
}
```

**Update Frequency**: Every 60 seconds (client checks 180s timeout)

---

## 3. Rust Technology Stack

### 3.1 Project Structure (完全解耦合设计)

```
offline-executor/
├── src/
│   ├── main.rs              # Entry point with mode selection
│   ├── client/              # Client mode modules
│   │   ├── mod.rs
│   │   ├── submit.rs        # Task submission
│   │   ├── status.rs        # Status query
│   │   ├── results.rs       # Result retrieval
│   │   ├── history.rs       # SQLite history query
│   │   └── db.rs            # SQLite task index (optional)
│   ├── daemon/              # Daemon mode modules (无pytest专用代码)
│   │   ├── mod.rs
│   │   ├── watcher.rs       # File event monitor (notify)
│   │   ├── executor.rs      # Generic command executor (tokio)
│   │   ├── heartbeat.rs     # Heartbeat mechanism
│   │   ├── retry.rs         # Retry logic
│   │   └── logger.rs        # Log file management
│   ├── core/                # Shared core modules
│   │   ├── mod.rs
│   │   ├── models.rs        # Task, Result models (通用)
│   │   ├── protocol.rs      # File communication protocol
│   │   ├── lock.rs          # File lock (fs2)
│   │   ├── config.rs        # YAML config parsing
│   │   └── error.rs         # Error types
│   └── lib.rs               # Library root
├── adapters/                # 业务适配器（可选，不耦合到框架）
│   ├── pytest_adapter.yaml  # pytest命令模板配置
│   └── shell_adapter.yaml   # shell命令模板配置
├── Cargo.toml
├── config/
│   ├── client.yaml
│   └── daemon.yaml
├── tests/
│   ├── unit/                # 单元测试（每个模块）
│   ├── integration/         # 端到端测试（完整流程）
│   └── fixtures/            # 测试数据
├── scripts/
│   ├── systemd-setup.sh     # systemd配置脚本
│   ├── health-check.sh      # 健康检查
│   └── run-tests.sh         # 测试运行脚本
└── docs/
    ├── CLAUDE.md
    └── adapter-guide.md     # 适配器配置指南
```

**关键设计原则**：

✅ **框架零业务耦合**：daemon模块不包含pytest.rs，只有通用executor.rs
✅ **适配器外部化**：pytest/shell配置通过YAML模板实现，框架只负责执行
✅ **通用执行引擎**：executor通过subprocess执行任意命令，不解析业务输出

### 3.2 Cargo.toml Configuration

```toml
[package]
name = "offline-executor"
version = "0.1.0"
edition = "2021"

[dependencies]
# Core dependencies
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tokio = { version = "1.0", features = ["full"] }
chrono = { version = "0.4", features = ["serde"] }
uuid = { version = "1.0", features = ["v4", "serde"] }
thiserror = "1.0"

# File system
notify = "5.0"                # File event monitoring
fs2 = "0.4"                   # File locking

# CLI
clap = { version = "4.0", features = ["derive"] }
serde_yaml = "0.9"            # Config parsing

# Optional SQLite
rusqlite = { version = "0.28", optional = true }

[features]
default = ["sqlite"]
sqlite = ["rusqlite"]

[[bin]]
name = "offline-executor"
path = "src/main.rs"
```

### 3.3 Key Rust Libraries

| Library | Purpose | Performance Impact |
|---------|---------|-------------------|
| **tokio** | Async runtime | 100+ concurrent tasks, no GIL |
| **notify** | File event monitor | 500ms latency (vs 2s polling) |
| **serde/serde_json** | JSON serialization | 10x faster than Python |
| **fs2** | File locking | Atomic writes, avoid conflicts |
| **clap** | CLI parser | Compile-time validation |
| **thiserror** | Error handling | Zero runtime cost |
| **rusqlite** | SQLite (optional) | History query optimization |

---

## 4. CLI Design

### 4.1 Client Mode Commands

```bash
# Task submission
offline-executor client submit \
  --cmd "pytest tests/test_inference.py -v" \
  --type pytest \
  --timeout 300 \
  --retry 3

# Status query
offline-executor client status --task-id abc123

# Result retrieval
offline-executor client results --task-id abc123

# Batch testing
offline-executor client batch \
  --file test_list.txt \
  --retry 3 \
  --timeout 300

# Progress monitoring
offline-executor client progress --task-id abc123

# History query (SQLite)
offline-executor client history \
  --days 3 \
  --status failed
```

### 4.2 Daemon Mode Commands

```bash
# Start daemon
offline-executor daemon --config config/daemon.yaml

# Start as systemd service
offline-executor daemon --systemd
```

---

## 5. Business Adapter Design (业务解耦合)

### 5.1 Adapter配置文件（外部化）

**pytest适配器**（`adapters/pytest_adapter.yaml`）：

```yaml
adapter_name: "pytest"
command_template:
  base: "pytest {test_path}"
  flags:
    - "--json-report"
    - "--json-report-file={log_dir}/report.json"
    - "--cov={cov_target}"
    - "--cov-report=xml:{log_dir}/coverage.xml"
    - "--tb=short"

output_files:
  report: "{log_dir}/report.json"  # pytest-json-report输出
  coverage: "{log_dir}/coverage.xml"
  log: "{log_dir}/execution.log"

result_parser:
  type: "json"
  file: "{log_dir}/report.json"
  summary_fields:
    - "summary.total"
    - "summary.passed"
    - "summary.failed"
    - "summary.skipped"
```

**shell适配器**（`adapters/shell_adapter.yaml`）：

```yaml
adapter_name: "shell"
command_template:
  base: "{command}"
  flags: []

output_files:
  log: "{log_dir}/execution.log"

result_parser:
  type: "stdout"  # 从stdout提取结果摘要
```

### 5.2 框架执行流程（通用）

```rust
// daemon/src/executor.rs - 通用执行引擎（无业务逻辑）
pub async fn execute_task(&self, task: Task) -> Result<TaskResult> {
    // 1. 创建日志目录
    let log_dir = self.config.shared_storage
        .join("logs")
        .join(task.task_id.to_string());
    fs::create_dir_all(&log_dir)?;

    // 2. 执行命令（无业务解析）
    let output = timeout(
        Duration::from_secs(task.timeout),
        Command::new("sh")
            .arg("-c")
            .arg(&task.command)
            .envs(&task.env_vars)
            .current_dir(&task.working_dir)
            .output(),
    ).await?;

    // 3. 写入日志文件（通用）
    fs::write(log_dir.join("execution.log"), &output.stdout)?;

    // 4. 返回结果路径引用
    Ok(TaskResult {
        task_id: task.task_id,
        status: if output.status.success() { TaskStatus::Completed } else { TaskStatus::Failed },
        return_code: output.status.code().unwrap_or(-1),
        stdout: truncate_output(&output.stdout, 1000),  // 仅保留前1000字符
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
        execution_time: duration,
        task_output: TaskOutput {
            type: task.task_type.to_string(),
            report_file: log_dir.join("report.json"),  // 路径引用
            log_file: log_dir.join("execution.log"),
            artifacts: scan_artifacts(&log_dir)?,
        },
        metadata: HashMap::new(),
    })
}
```

**关键特性**：

- ✅ 框架不解析pytest输出，只执行命令并返回路径
- ✅ 业务逻辑通过适配器配置实现（YAML模板）
- ✅ Claude Code读取`task_output.report_file`路径获取详细结果
- ✅ 框架100%通用，适配器可选配置

### 5.3 Claude Code使用流程

```bash
# 1. 框架提交任务（无业务逻辑）
offline-executor client submit --cmd "pytest tests/test_inference.py -v"

# 2. 框架返回结果（包含日志路径）
offline-executor client results --task-id abc123
# 输出: task_output.report_file = "logs/abc123/report.json"

# 3. Claude Code读取业务报告（自己解析）
cat /shared/storage/logs/abc123/report.json
# Claude Code解析pytest-json-report格式，提取错误信息

# 4. Claude Code生成修复代码，重新提交验证
```

---

## 6. Error Handling & Recovery

### 6.1 Retry Mechanism

- Failed tasks automatically retry up to `retry_count` times
- Priority increases for retries (priority + 1)
- Permanent failure after exhausting retries

### 6.2 Timeout Control

- Task execution timeout: `timeout` seconds (default: 300s)
- Heartbeat timeout: 180 seconds (client-side check)
- File lock timeout: 30 seconds

---

## 7. Deployment

### 7.1 Build

```bash
cargo build --release
# Output: target/release/offline-executor (~8MB)
```

### 7.2 systemd Service

```bash
# Setup script
sudo cp target/release/offline-executor /usr/local/bin/
sudo systemctl enable offline-executor
sudo systemctl start offline-executor
```

---

## 8. Performance Targets

| Metric | Target | Implementation |
|--------|--------|----------------|
| **File event detection** | 500ms | notify library |
| **JSON serialization** | 1ms | serde |
| **Framework overhead** | <600ms | Exclude task execution |
| **Max concurrent tasks** | 100+ | tokio async |
| **Memory per daemon** | <10MB | Rust zero-allocation |

---

## 9. Development Plan (4 Weeks)

### Week 1-2: Core Framework
- Single binary with client/daemon mode
- File communication protocol
- File locking mechanism
- Heartbeat mechanism

### Week 3: pytest Integration
- pytest command generation
- JSON report parsing
- Batch testing
- Retry mechanism

### Week 3: Advanced Features
- Timeout control
- Priority queue
- SQLite history indexing
- systemd service

### Week 4: Claude Code Integration
- Structured JSON output
- Documentation
- Deployment scripts
- Integration tests

---

## 10. Architecture Decision Records

### ADR-001: Single Binary

**Decision**: Single binary with `client`/`daemon` subcommands

**Consequences**:
- ✅ Deployment simpler
- ✅ Code sharing natural
- ❌ Binary size ~8MB

### ADR-002: File Events

**Decision**: notify library (500ms) instead of polling (2s)

**Consequences**:
- ✅ 4x faster response
- ✅ Zero CPU when idle

### ADR-003: Optional SQLite

**Decision**: SQLite only for client-side optional history indexing

**Consequences**:
- ✅ Zero dependency for core functionality
- ✅ JSON files for communication

---

**Document Status**: ✅ Approved for implementation
**Next Step**: Invoke writing-plans skill to create implementation plan