# 高级用法与配置

调优 Bifrost 和不常见工作流的参考文档。安装与基础提交/查询请看
[README](../../README.zh-CN.md)；本文覆盖完整配置项、存储语义、SSH 传输、任务调度、清理、
pytest 容器，以及各 Agent 的 MCP 注册方式。

> [English](advanced_usage.md) | 简体中文

## 配置

### 配置文件与优先级

`~/.bifrost/settings.json`（客户端）或 `/etc/bifrost/settings.json`（deb 安装的 daemon）：

```bash
bifrost server init --storage /mnt/gpfs/bifrost   # 生成 daemon 侧默认配置
```

```json
{
  "shared_storage": "/tmp/bifrost",
  "client": {},
  "daemon": {}
}
```

解析顺序：**`BIFROST_CONFIG` 环境变量 > `-c` 参数 > `~/.bifrost/settings.json` > 默认值**。

| 字段 | 客户端 | 服务端 | 说明 |
|-------|--------|--------|------|
| shared_storage | 是 | 是 | commands/results/logs 的根目录 |
| transport | 是 | -- | Bridge 类型：`"shared"`（默认）或 `"ssh"` |
| ssh | 是 | -- | SSH 配置（host/remote_dir/...），transport=ssh 时必填 |
| client.* | 是 | -- | 轮询间隔、心跳超时、等待超时（上限 12h） |
| daemon.* | -- | 是 | 并发、超时、重试、工作目录 |

`--storage DIR` 和 `--gpus LIST` 是全局参数，会覆盖 `shared_storage` 和 `daemon.gpu_pool`，
对所有子命令生效。

### `daemon.*` 详解

```json
{
  "shared_storage": "/data/liuxin/bifrost",
  "client": { "poll_interval": "5s" },
  "daemon": {
    "task_timeout": "1h30m",
    "max_concurrent": 8,
    "use_shell": true,
    "scan_interval": "1s",
    "stale_marker_timeout": "3h",
    "max_retries": 3,
    "gpu_pool": [0, 1, 2, 3, 4, 5, 6, 7],
    "working_dir": "/workspace/work"
  }
}
```

| 字段 | 默认值 | 说明 |
|-------|---------|------|
| task_timeout | 1h30m | 任务未指定超时时的默认值。**不再是上限**：任务自带 `timeout` 优先 |
| max_concurrent | 10 | 并发执行的任务数（1–100） |
| use_shell | true | 命令经 `sh -c` 执行，管道、重定向、`&&`、`$VAR`、`&` 都可用；设为 false 则直接 argv 执行。单任务可用 `shell: false` 覆盖（参数含字面量 `$` / `*` 时使用） |
| scan_interval | 1s | 除 inotify 之外的兜底扫描周期。空闲 daemon 不再忙轮询烧 CPU；扫描线程每秒写 `scan.json`（ticks/scanned_at），可判断兜底扫描是否还活着 |
| stale_marker_timeout | 2×task_timeout | 超过该时间的 `.processing` claim 标记会被回收。回收前会校验归属：标记记录 host/pid/task，仍由存活本地进程或本进程持有的任务会跳过。keepalive 使用 `min(60s, 值/3)` |
| max_retries | 不限 | 重试次数上限（未设置 = 不设上限）。重试本身按任务显式开启：`client submit --retries 2` 或 YAML `retries: 2`；不写只跑一次。只有非零退出才重试，超时和取消不重试。每次尝试单独记录为 `stdout.attemptN.log` |
| gpu_pool | **自动** | **未设置 = 自动使用本机全部 GPU**（通过 `nvidia-smi` 探测）。每个任务独占一张卡并注入 `CUDA_VISIBLE_DEVICES`（任务自己指定时只租用该 id，不覆盖其值）；并发上限变为 `min(max_concurrent, GPU 数)`。`[]` 关闭 GPU 分配，`[0,1]` 只用这两张 |
| cancel_poll | 1s | 运行中任务检查取消请求的频率。1s 在可取消性与元数据开销之间平衡；30s 意味着失控的 benchmark 会多烧半分钟 GPU |
| log_backups | 1 | 保留的轮转备份数（`.1`..`.N`，最多 9）。每路日志固定 64MB 轮转，磁盘占用上限为 `64MB × (log_backups+1)`；最新内容始终在 `stdout.log` |
| — | 固定 | 结果负载上限 64MB（16MB 头部上下文 + 48MB 尾部摘要）—— benchmark 摘要位于流尾部，因此尾部权重更高；完整日志在 `logs/<id>/` |

**超时语义：** `client submit --timeout 0`（默认）表示“使用 daemon 的 `task_timeout`”；传具体
数值时任务自身的值生效，daemon 不再截断。

**客户端上限：** `client.wait_timeout`（默认 12h）是客户端等待单个 job 的硬上限，避免 daemon
失联时客户端永久阻塞。

**Shell 语义：** `use_shell=true` 是默认值。命令以 `sh -c "<command>"` 执行，因此管道、重定向、
`&&`、`$VAR` 和后台 `&` 开箱可用。若某个任务的参数必须保持字面量（例如含 `*` 的正则或字面量
`$`），对该任务关闭 shell：

```json
{ "command": "grep -n 'a.*b' file.txt", "shell": false }
```

### GPU 调度

- `gpu_pool` 未设置 = 使用 `nvidia-smi` 探测到的全部 GPU。
- `[0,1]` = 只租用 0、1 号卡；`[]` = 完全关闭 GPU 分配。
- 每个任务通过 `CUDA_VISIBLE_DEVICES` 独占一张卡；任务自己设置该变量时仍会租用 id，但不覆盖值。
- 实际并发为 `min(max_concurrent, 租用 GPU 数)`。

### 时钟自检与兜底扫描

部分虚拟化环境（例如某些 VM/容器宿主）会冻结 `CLOCK_MONOTONIC`：`sleep()` 永不返回、线程永久
挂起，daemon 表现为“心跳正常但任务永不消费”。大多数现代宿主没有这个缺陷，因此**默认不让每台
机器承担这个成本**：

| scan_mode | 行为 |
|---|---|
| `auto`（默认） | 启动时花约 100ms 比较墙钟与单调钟；确认两者漂移后才切到 spin |
| `sleep` | 强制定时器驱动扫描（CPU 最低），用于确认健康的机器 |
| `spin` | 强制忙轮询（约 1 核），只用于确实有时钟问题的机器 |

自检结果会打印在启动日志：`clock: ok (sleep-driven scan)` 或 `WARNING: ... busy-poll`。如果运行
中仍怀疑卡死，使用外部看门狗 `scripts/ops/watchdog.sh`（读取 `heartbeat.json` / `scan.json` 的
年龄，超过阈值就重启）——它由 cron 或 systemd timer 运行，与 daemon 不共享故障域。

### SSH 传输层

当客户端与目标机器**没有共享文件系统**但可以通过 SSH 互通时，使用 SSH 传输：客户端通过 ssh
读写目标机的 `commands/ results/ status/ artifacts/`，目标机上的 daemon 继续通过本地协议消费
任务——两侧目录语义完全一致，daemon 无需修改。

```json
{
  "shared_storage": "/tmp/bifrost",            // daemon 仍用它作为本地路径
  "transport": "ssh",
  "ssh": {
    "host": "target-machine.example.com",      // 必填
    "user": "bifrost",                          // 可选，默认当前用户
    "remote_dir": "/gpfs/bifrost",              // 必填：目标机上充当 shared_storage 的目录
    "port": 22,                                 // 可选，默认 22
    "connect_timeout": "10s"                    // 可选，单连接超时（默认 10s）
  },
  "client": {},
  "daemon": {}
}
```

要求：客户端可以免密 `ssh <host>`（`BatchMode=yes`）。文件内容通过 ssh stdin 传输
（`cat >` 加原子 `mv`），路径经过 shell 转义。

## 高级用法

### 共享存储语义

Bifrost 只要求**两端都能读写的 POSIX 目录**，不绑定 GPFS；同一个目录在源机器上可以是网络
挂载，在目标机器上可以是本地路径。

```bash
# daemon 侧
bifrost warmup  --storage /data/liuxin/bifrost   # 存储 / 时钟 / GPU 自检
bifrost server  --storage /data/liuxin/bifrost   # GPU 池自动探测全部卡
# 限制卡：--gpus 0,3,5；关闭 GPU：--gpus "" 或 "gpu_pool": []

# 客户端
bifrost client submit --storage /data/liuxin/bifrost --command 'nvidia-smi -L'
```

- 首次运行会自动创建 `commands/ results/ status/ artifacts/ logs/ cancel/`，以及
  `heartbeat.json` / `scan.json`。
- 如果目录是多机共享的网络文件系统，inotify 可能丢事件——默认 1s 的兜底扫描会覆盖，因此不要求
  GPFS。
- 仓库的 `dist/` 可能有两份本机构建：`bifrost-linux-x86_64` 是 musl 静态版；`-glibc` 版本要求
  glibc ≥ 2.39。不确定目标系统时用静态版。

### 上线前自检：`bifrost warmup`

**每次部署的第一步，与文件系统无关**——GPFS、NFS、Lustre 或普通本地目录都要跑。在将要运行
daemon 的机器上、首次启动前执行：

```bash
bifrost warmup --storage /data/liuxin/bifrost   # 或: bifrost --warmup
```

它检查共享存储可写性与文件系统类型、执行时钟自检（`sleep()` 是否会挂起）、读取已有 daemon
心跳并报告 GPU 可见性。发现阻塞问题会以 1 退出，因此可以卡住部署脚本——`deploy-server.sh`
正是这么做的。systemd 单元每次启动也会执行（`ExecStartPost`）。

**为什么必须在服务端跑：** 每项检查都是执行端的属性——时钟冻结影响 daemon 的兜底扫描线程，
GPU 只存在于目标机，可写性取决于 daemon 用户的权限。在客户端跑只能证明“客户端视角下目录可
写”，当 daemon 因不同用户/不同机器启动失败时说明不了问题。

### 任务类型

| 类型 | 说明 | 适用场景 |
|------|-------------|----------|
| `shell` | Shell 命令执行 | 构建脚本、数据处理 |
| `pytest` | Python 测试执行 | 单元测试、集成测试 |
| `custom` | 适配器执行 | Docker、专用工具 |

命令第一个词是 `pytest` 时会自动识别为 pytest 任务。

### 任务优先级

优先级范围 0–255，数值越小越先执行。

| 范围 | 优先级 | 示例 |
|-------|----------|---------|
| 0-10 | 高 | 紧急修复、紧急测试 |
| 11-50 | 普通 | 常规测试、构建 |
| 51-100 | 低 | 后台任务、清理 |
| 101-255 | 最低 | 维护、日志 |

```bash
bifrost client submit --command "pytest tests/unit/ -v" --priority 10 --timeout 600
```

### 重试与超时

重试按任务显式开启，并受 `daemon.max_retries` 上限约束：

```bash
bifrost client submit --command "..." --retries 2   # 先跑一次，最多再重试两次
bifrost client submit --command "..." --timeout 0   # 0 = 使用 daemon.task_timeout
```

只有非零退出会重试；超时和取消是终态。每次尝试单独记录（`stdout.attemptN.log`）。

### YAML Job

多步骤顺序执行使用 YAML：

```bash
bifrost client submit --job examples/quick_start.yaml
```

`examples/` 下有 pytest、benchmark、smoke 等示例。

### 清理历史任务

长期运行会累积 `commands/ results/ status/ logs/ artifacts/`。`clean` 会删除**已完成任务**
（有 result 文件）中早于 N 天的文件：

```bash
# 预览（安全，不删除）
bifrost client clean --older-than 7 --dry-run

# 实际删除 7 天前完成的任务文件
bifrost client clean --older-than 7

# 指定存储目录
bifrost client clean --storage /gpfs/gcsp/liuxin/bifrost_test --older-than 30
```

**安全保证：**

- 只处理**有 result 文件**（终态）的任务——pending/running 永远不会被选中
- `heartbeat.json` 和 `settings.json` **永不删除**
- `--dry-run` 先预览；`--older-than` 默认 7 天

### 容器内跑 pytest

在离线机器的容器里跑 pytest：

1. **创建 Docker 适配器**：

```yaml
# /etc/bifrost/adapters/docker_pytest.yaml
name: docker_pytest
command_template: "docker run --rm -v {working_dir}:/app -w /app python:3.10-slim pytest {path} --json-report --json-report-file=report.json"
timeout: 600
artifacts:
  - report.json
```

2. **用适配器提交任务**：

```bash
bifrost client submit \
  --command "docker run --rm -v /workspace/myproject:/app -w /app python:3.10-slim pytest tests/unit/ --json-report --json-report-file=report.json" \
  --working-dir /workspace/myproject \
  --timeout 600
```

### pytest 依赖

在离线机器（或容器内）安装：

```bash
pip install pytest pytest-json-report pytest-cov
```

## MCP Agent 配置

通用配置是 `command: /path/to/bifrost`、`args: ["mcp-serve"]`，可选 `BIFROST_CONFIG` 环境变量。
配置解析顺序：**`BIFROST_CONFIG` 环境变量 > `-c` 参数 > `~/.bifrost/settings.json` > 默认值**。

### Hermes

```bash
hermes mcp add bifrost --command /path/to/bifrost --args mcp-serve
hermes mcp test bifrost
```

### OpenCode — `opencode.json`

```json
{
  "mcp": {
    "bifrost": {
      "type": "stdio",
      "command": "/path/to/bifrost",
      "args": ["mcp-serve"],
      "env": { "BIFROST_CONFIG": "/path/to/settings.json" }
    }
  }
}
```

### Claude Code

```bash
claude mcp add bifrost --env BIFROST_CONFIG=/path/to/settings.json -- /path/to/bifrost mcp-serve
```

或项目级 `.mcp.json`：

```json
{
  "mcpServers": {
    "bifrost": {
      "command": "/path/to/bifrost",
      "args": ["mcp-serve"],
      "env": { "BIFROST_CONFIG": "/path/to/settings.json" }
    }
  }
}
```

### Cline / Cursor / 通用 Agent

使用同样的 `mcpServers` JSON 结构，包含 `command` + `args` + `env`。详见
[MCP.md](MCP.md)。

### MCP 验证

```bash
# 通过 Hermes
hermes mcp test bifrost

# 手工 JSON-RPC 握手（与 Agent 无关）
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"inspector","version":"1.0"}}}' \
  | bifrost mcp-serve | head -3
```

### MCP 排障

| 问题 | 原因 | 处理 |
|---------|-------|-----|
| 添加时连接关闭 | `shared_storage` 路径不可写/不存在 | 修正 settings.json 中的 `shared_storage` |
| `alive: false` | daemon 未运行或心跳过期 | 在目标机启动 `bifrost server -c <cfg>`，等 2s |
| 任务一直 Pending | daemon 未运行，或两端存储路径不一致 | 检查 health，确认 `shared_storage` 是同一物理目录，再看 `scan.json` |
| 提交报错 | 命令格式问题 | 检查引号；默认 `use_shell=true`，shell 语法可用 |
| 多 Agent 同时用 | ——（基于文件，无冲突） | 无需额外配置 |
