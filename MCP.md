# Bifrost MCP Server — 通用接入指南

> Bifrost 内置 MCP (Model Context Protocol) server，通过 stdio 暴露 4 个工具。
> **任何支持 MCP 协议的 Agent 均可接入**：Hermes、OpenCode、Claude Code、Cline、Cursor、Codex、通用 Coding Agent 等。

## 快速开始（3 步）

### 1. 准备二进制（联网侧机器，即 client 所在机器）

```bash
cd /gpfs/gcsp/liuxin/bifrost
cargo build --release          # 产物: target/release/bifrost (3.0M)
```

### 2. 准备配置（推荐：用 BIFROST_CONFIG 环境变量，不依赖 ~/.bifrost）

MCP server 配置解析顺序：**`BIFROST_CONFIG` 环境变量 > `-c` 参数 > `~/.bifrost/settings.json` > 默认值**。

对第三方 Agent 最友好的方式是在 Agent 的 MCP 配置里直接带上环境变量（见下方各 Agent 示例），这样**不需要**在运行 Agent 的机器上准备 `~/.bifrost/settings.json`：

```json
{
  "shared_storage": "/gpfs/gcsp/liuxin/bifrost",   // ← 改成你的 GPFS 交换区
  "client": { "poll_interval": "2s", "heartbeat_timeout": "180s" },
  "daemon": { "task_timeout": "300s", "heartbeat_interval": "60s", "max_concurrent": 10 }
}
```

> **shared_storage 必须与 H20 上 daemon 用同一个目录**，否则任务无法送达。

### 3. 在你使用的 Agent 中注册 MCP server

所有 Agent 的配置本质上都是：**以 stdio 方式启动 `bifrost mcp-serve`**。以下是各 Agent 的具体写法。

---

## 各 Agent 接入示例

### 🤖 Hermes

```bash
hermes mcp add bifrost --command /gpfs/gcsp/liuxin/bifrost/target/release/bifrost --args mcp-serve
hermes mcp test bifrost       # 验证
```

### 🖥️ OpenCode

`opencode.json`（项目根目录或 `~/.config/opencode/`）：

```json
{
  "$schema": "https://opencode.ai/schema.json",
  "mcp": {
    "bifrost": {
      "type": "stdio",
      "command": "/gpfs/gcsp/liuxin/bifrost/target/release/bifrost",
      "args": ["mcp-serve"],
      "env": {
        "BIFROST_CONFIG": "/gpfs/gcsp/liuxin/bifrost_test/settings.json"
      }
    }
  }
}
```

### 🟣 Claude Code

```bash
claude mcp add bifrost --env BIFROST_CONFIG=/gpfs/gcsp/liuxin/bifrost_test/settings.json -- /gpfs/gcsp/liuxin/bifrost/target/release/bifrost mcp-serve
```

或项目级 `.mcp.json`：

```json
{
  "mcpServers": {
    "bifrost": {
      "command": "/gpfs/gcsp/liuxin/bifrost/target/release/bifrost",
      "args": ["mcp-serve"],
      "env": {
        "BIFROST_CONFIG": "/gpfs/gcsp/liuxin/bifrost_test/settings.json"
      }
    }
  }
}
```

### 📐 Cline (VS Code)

VS Code 设置 → Cline → MCP Servers → 添加：

```json
{
  "mcpServers": {
    "bifrost": {
      "command": "/gpfs/gcsp/liuxin/bifrost/target/release/bifrost",
      "args": ["mcp-serve"],
      "env": {
        "BIFROST_CONFIG": "/gpfs/gcsp/liuxin/bifrost_test/settings.json"
      }
    }
  }
}
```

### 🚀 Cursor

Cursor Settings → MCP → Add new global MCP server：

```json
{
  "mcpServers": {
    "bifrost": {
      "command": "/gpfs/gcsp/liuxin/bifrost/target/release/bifrost",
      "args": ["mcp-serve"],
      "env": {
        "BIFROST_CONFIG": "/gpfs/gcsp/liuxin/bifrost_test/settings.json"
      }
    }
  }
}
```

### 🧩 通用 Coding Agent（任意支持 MCP 的客户端）

只要该 Agent 支持 **stdio MCP server**，配置就是一条命令 + 一个环境变量：

```
command: /gpfs/gcsp/liuxin/bifrost/target/release/bifrost
args:    ["mcp-serve"]
env:     BIFROST_CONFIG=/path/to/your/settings.json   (可选; 不设则读 ~/.bifrost/settings.json)
```

---

## 可用工具

| 工具 | 说明 | 必调时机 |
|------|------|---------|
| `bifrost_health` | 检查离线 daemon 心跳 | **提交前必调**：`alive=false` 时提交会丢任务 |
| `bifrost_submit` | 提交命令任务，返回 task_id | 参数: `command`(必填), `timeout`, `priority`, `working_dir` |
| `bifrost_status` | 查询任务状态 | `task_id` 参数；轮询直到终态 |
| `bifrost_result` | 拉取完整结果 | 终态后调用；含 stdout/stderr/exit_code/duration_ms |

### 标准调用序列

```
1. bifrost_health        → 确认 alive=true（daemon 在线）
2. bifrost_submit        → {"command": "sh -c 'echo hi'", "timeout": 60} → task_id
3. bifrost_status        → 轮询: Pending → Running → Completed/Failed/Timeout
4. bifrost_result        → 获取 stdout、退出码、耗时
```

### 重要使用规则

1. **复杂命令必须 `sh -c '...'` 包裹**：bifrost 防注入设计，命令不经 shell 解释。重定向 `>`、`&&`、`$VAR`、后台 `&` 都需要 `sh -c`。
2. **提交前检查 health**：daemon 未就绪时提交的任务不会被消费（inotify 只监控新文件）。
3. **任务终态**：`Completed`（exit 0）/ `Failed`（非 0 退出）/ `Timeout`（超时）。超时后进程组会被完整清理。
4. **并发**：daemon 按 `max_concurrent`（默认 10）并行执行，批量快速任务可直接连发。

---

## 验证方法（不依赖任何 Agent）

```bash
# 方式 1: Hermes 自带测试
hermes mcp test bifrost

# 方式 2: 手动 JSON-RPC 握手
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"inspector","version":"1.0"}}}' \
  | bifrost mcp-serve | head -3
# 期望输出包含 "capabilities":{"tools":{}}

# 方式 3: 完整 E2E 脚本（health→submit→status→result）
python3 /gpfs/gcsp/liuxin/bifrost_test/test_mcp_e2e.py
```

---

## 常见问题

| 问题 | 原因 | 解决 |
|------|------|------|
| 连接失败/Connection closed | shared_storage 路径无权限或不存在 | 检查 settings.json 的 shared_storage 是否可读写 |
| `alive: false` | daemon 未启动或心跳过期 | 在 H20 启动 `bifrost server -c <cfg>`，等 2s 再查 |
| 任务一直 Pending | daemon 未消费（inotify 不扫存量） | 提交前先 health 检查；确认 server 正在运行 |
| submit 报错 | 命令格式错误 | 复杂命令用 `sh -c '...'` 包裹 |
| 两个 Agent 同时用 | 无冲突（只读/写 GPFS 文件） | 无需额外配置，天然支持多客户端 |

---

## 架构说明

```
┌─────────────────────────────────────────┐
│ 任意 MCP Agent (Hermes/OpenCode/Claude…) │
│   └─ stdio MCP ── bifrost mcp-serve    │  ← 联网侧 (client 机器)
├─────────────────────────────────────────┤
│        GPFS 共享存储 (shared_storage)    │
│   commands/  results/  status/  logs/   │
├─────────────────────────────────────────┤
│        bifrost daemon (H20, 离线)        │
│   inotify 监控 → 并发执行 → 写回结果     │
└─────────────────────────────────────────┘
```

MCP server 是无状态的：每次调用直接读写 GPFS。多个 Agent 可同时连接（每个连接一个进程实例），互不干扰。
