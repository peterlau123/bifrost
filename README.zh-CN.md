# 🌉 Bifrost —— 离线机器命令执行框架

![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)
![Rust Edition](https://img.shields.io/badge/Rust-2021%20Edition-dea584.svg)
![Platform](https://img.shields.io/badge/Platform-Linux-blue.svg)
![Tests](https://img.shields.io/badge/Tests-218%20passed-brightgreen.svg)
![MCP](https://img.shields.io/badge/MCP-5%20tools-8A2BE2.svg)

<p align="center">
  <img src="assets/bifrost-banner.svg" alt="Bifrost - 连接联网客户端与离线 daemon 的彩虹桥" width="720">
</p>

[English](README.md) | **简体中文**

Bifrost 是一个 Rust 框架：通过共享存储，在离线 / 气隙机器上执行命令。联网的客户端机器提交任务，
离线的 daemon 机器执行任务，结果通过同一块可移植存储同步回来。

## 目录

- [概览](#概览)
- [工作原理](#工作原理)
- [环境要求](#环境要求)
- [安装](#安装)
- [配置](#配置)
- [基础用法](#基础用法)
- [发布与更新](#发布与更新)
- [MCP Server（Agent 接入）](#mcp-serveragent-接入)
- [开发](#开发)
- [文档](#文档)
- [排障](#排障)
- [License](#license)
- [状态](#状态)

---

## 概览

### 解决什么问题

传统远程执行工具——SSH、RPC、REST API——在完全没有网络的隔离机器上无法使用。Bifrost 用
**共享存储作为通信通道**：

- **客户端**（联网机器）把任务写入 `commands/` 目录
- **daemon**（离线机器）监控新任务并执行
- **结果**写回 `results/` 目录
- 同步依赖 GPFS / NFS / Lustre 或任意共享 POSIX 文件系统

### 核心特性

- **Bridge 抽象** —— 传输无关：共享存储（GPFS/NFS）与 **SSH** 都已实现
- **统一配置** —— `~/.bifrost/settings.json`，由 CLI 初始化
- **气隙运行** —— 客户端与服务端完全隔离
- **安全加固** —— 防路径穿越，shell 模式可控
- **高性能** —— inotify 文件监控 + 1s 兜底扫描，tokio 异步执行，端到端开销约 4–6%
- **多种任务类型** —— shell 命令、pytest、自定义适配器
- **并行任务** —— `--parallel` 并发提交，受 daemon 的 `max_concurrent` 限制
- **优先级调度** —— 任务按优先级（0–255）排序
- **超时管理** —— 可配置超时与可选重试
- **systemd 集成** —— 生产级服务部署
- **systemd 监管** —— `Restart=on-failure`，通过 sd_notify 上报就绪，开机自启
- **MCP Server** —— 内置，Hermes / OpenCode / Claude 等任意 MCP Agent 可直接驱动
- **健康监控** —— 基于心跳的服务端健康检查

## 工作原理

Bifrost 通过两端都能看到的 POSIX 目录交换 JSON 文件：

1. 客户端把任务写入 `commands/`（CLI、YAML Job 或 MCP 工具调用）。
2. daemon 监控该目录（`inotify` + 兜底扫描），认领任务并异步执行。
3. daemon 写入 `status/`、`logs/`、`artifacts/`、最终 `results/` 文件，并周期性更新 `heartbeat.json`。

默认传输是 `shared`（共享存储）；没有共享文件系统时可以使用 `ssh`。

组件图、完整任务生命周期、共享存储布局、设计取舍与安全模型见
[docs/developer/architecture.zh-CN.md](docs/developer/architecture.zh-CN.md)。

## 环境要求

| 组件 | 要求 |
|---|---|
| **平台** | Linux x86_64——发布二进制为静态 musl 链接 |
| **共享存储** | 两端都能读写的 POSIX 目录（GPFS / NFS / Lustre / 本地目录） |
| **客户端** | 只需要 `bifrost` 二进制——不需要 Rust 工具链，不需要 Python |
| **daemon** | 同一个二进制；生产环境建议 systemd |
| **pytest 任务** | 目标机需要 Python 3.10+、pytest 7.x 与 `pytest-json-report`（可选） |

从源码编译额外需要 Rust 1.87+。

## 安装

仅支持 **Linux x86_64**。[Releases 页面](https://github.com/peterlau123/bifrost/releases)
提供三种形式：

| 形式 | 适用场景 | 内容 |
|---|---|---|
| `.deb` | Debian / Ubuntu | `dpkg -i`、`bifrost-setup`、systemd unit |
| `.tar.gz` | 任意 x86_64 Linux、无 root、气隙机器 | 二进制 + `install.sh`（默认前缀 `/usr/local/bin`） |
| Source code | 需要审计或自行编译 | GitHub 自动生成的 tag 快照；需要 Rust 1.87+ |

每个产物都有对应的 `.sha256`。`latest` 是 `main` 的滚动快照，`vX.Y.Z` 是正式版本。下载示例：

```bash
BASE=https://github.com/peterlau123/bifrost/releases/download/latest
curl -fLO $BASE/bifrost-latest-x86_64-linux-musl.tar.gz
curl -fLO $BASE/bifrost-latest-x86_64-linux-musl.tar.gz.sha256
curl -fLO $BASE/bifrost_<版本>-1_amd64.deb
curl -fLO $BASE/bifrost_<版本>-1_amd64.deb.sha256
sha256sum -c *.sha256
```

### Debian / Ubuntu（.deb）

```bash
sudo dpkg -i bifrost_<版本>-1_amd64.deb
bifrost --version

# 客户端：不需要 sudo
bifrost-setup client --storage /data/liuxin/bifrost

# 服务端：需要 sudo（写 /etc 并注册 systemd）
sudo bifrost-setup server --storage /data/liuxin/bifrost
```

包内安装 `/usr/bin/bifrost`、`/usr/bin/bifrost-setup`、systemd unit 和 `/etc/default/bifrost`。
服务**默认不启用**；`bifrost-setup server` 会 enable 并启动它。

### 便携 tar.gz

```bash
tar xzf bifrost-latest-x86_64-linux-musl.tar.gz
cd bifrost-latest-x86_64-linux-musl

# 客户端（默认二进制前缀：/usr/local/bin）
./install.sh --role client --storage /data/liuxin/bifrost

# 无 root：自定义安装目录
PREFIX=$HOME/.local/bin ./install.sh --role client --storage /data/liuxin/bifrost

# 服务端（写 /etc + systemd，需要 sudo）
sudo ./install.sh --role server --storage /data/liuxin/bifrost
```

默认路径与覆盖方式：

| 内容 | 默认值 | 覆盖参数 |
|---|---|---|
| 二进制 | `/usr/local/bin/bifrost` | `--prefix DIR` 或 `PREFIX=DIR` |
| 客户端配置 | `~/.bifrost/settings.json` | `--config FILE` |
| daemon 配置 | `/etc/bifrost/settings.json` | `--config FILE` |
| systemd unit | `/etc/systemd/system/bifrost.service` | `--unit-dir DIR` |

完整参数见 `./install.sh --help`（`--no-hermes`、`--no-service`、`--dry-run` 等）。

### 从源码安装

```bash
git clone https://github.com/peterlau123/bifrost.git
cd bifrost
cargo build --release

sudo install -m 0755 target/release/bifrost /usr/local/bin/bifrost

# 或构建发布用的静态 musl 产物：
scripts/build/build.sh --static
```

### 客户端配置

`bifrost-setup client`（deb）或 `install.sh --role client`（tar.gz）会写
`~/.bifrost/settings.json`。手工等价操作：

```bash
bifrost client init --storage /mnt/gpfs/bifrost
```

```json
{
  "shared_storage": "/mnt/gpfs/bifrost",
  "client": {
    "poll_interval": "2s",
    "heartbeat_timeout": "180s"
  }
}
```

验证 CLI：

```bash
bifrost client submit --command "hostname" --timeout 30
bifrost client status <TASK_ID>
```

### daemon 配置

`bifrost-setup server` 或 `install.sh --role server` 会写 `/etc/bifrost/settings.json` 和 systemd
unit。手工等价操作：

```bash
sudo cp bifrost.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now bifrost
systemctl status bifrost
journalctl -u bifrost -f
```

daemon 配置（`/etc/bifrost/settings.json` 或 `~/.bifrost/settings.json`）：

```json
{
  "shared_storage": "/mnt/gpfs/bifrost",
  "daemon": {
    "task_timeout": "300s",
    "max_retries": 3,
    "heartbeat_interval": "60s",
    "max_concurrent": 10,
    "working_dir": "/tmp/bifrost/work"
  }
}
```

> **重要：** `shared_storage` 在客户端与 daemon 上必须解析到**同一个物理目录**。

启动前在 daemon 机器上执行自检：

```bash
bifrost warmup --storage /mnt/gpfs/bifrost
```

调试时可以前台运行，`Ctrl+C` 停止：

```bash
bifrost server --config ~/.bifrost/settings.json
```

脚本化路径（`scripts/deploy/deploy-server.sh` / `deploy-client.sh`）会依次执行
安装 → 重启 → 等心跳 → 冒烟任务，失败则回滚；详见 [docs/deployment.zh-CN.md](docs/deployment.zh-CN.md)。

### 端到端验证

```bash
# daemon 侧：心跳存在且新鲜
cat /mnt/gpfs/bifrost/heartbeat.json | head -5

# 客户端
bifrost client submit \
  --command "sh -c 'echo \"bifrost deployment ok\"; nvidia-smi -L | head -3'" \
  --timeout 30
bifrost client status <TASK_ID>
# 预期：Completed，输出包含 "bifrost deployment ok"
```

命令默认经 `sh -c` 执行（`daemon.use_shell: true`）；需要参数保持字面量时，设置
`use_shell: false` 或任务的 `shell: false`。

## 配置

配置解析顺序：**`BIFROST_CONFIG` 环境变量 > `-c` 参数 > `~/.bifrost/settings.json` > 默认值**。

```json
{
  "shared_storage": "/mnt/gpfs/bifrost",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "180s" },
  "daemon": { "max_concurrent": 10, "task_timeout": "300s" }
}
```

`--storage DIR` 会覆盖任意子命令的 `shared_storage`；`--gpus LIST` 覆盖 `daemon.gpu_pool`。

完整参考——所有 `daemon.*` 字段、超时/重试语义、GPU 租用、时钟/扫描模式、SSH 传输——见
[docs/developer/advanced_usage.zh-CN.md](docs/developer/advanced_usage.zh-CN.md)。

## 基础用法

### 共享存储与上线自检

两端必须看到同一个 POSIX 目录。在 daemon 机器上、首次启动前执行自检：

```bash
bifrost warmup --storage /data/liuxin/bifrost
```

它会检查可写性、时钟行为、已有心跳和 GPU 可见性；详见
[advanced_usage.zh-CN.md](docs/developer/advanced_usage.zh-CN.md)。

### 提交、查询、取消

```bash
# shell 命令（默认 use_shell=true，shell 语法可用）
bifrost client submit --command "pytest tests/unit/ -v" --timeout 600
bifrost client submit --command "python script.py --input data.csv" --priority 10
bifrost client submit --command "..." --retries 2

# 多步骤 YAML Job
bifrost client submit --job examples/quick_start.yaml

# 状态 / 取消
bifrost client status <TASK_ID>
bifrost client cancel <TASK_ID>
```

参数包含字面量 `$` 或 `*` 的任务可以按任务关闭 shell（`shell: false`）；见高级文档。

### 停止 daemon

- `Ctrl+C` / `SIGTERM`：优雅退出，500ms 内结束；再按一次 `Ctrl+C` 立即退出。
- daemon 会杀掉所有运行任务的进程组，不会留下仍占用 GPU 的孤儿进程。

更多操作——清理、任务类型与优先级、重试、pytest 容器、SSH 传输——见
[docs/developer/advanced_usage.zh-CN.md](docs/developer/advanced_usage.zh-CN.md)。

## 发布与更新

一个二进制承担四种角色——Client CLI、Daemon Server、MCP Server，以及可选的 systemd 监管。
更新时主要关心哪些角色还在运行：

| 角色 | 运行位置 | 更新后需要重启？ |
|---|---|---|
| Client CLI | 联网机器 | 否——每次调用都会重新执行二进制 |
| MCP Server | 联网机器（Agent 侧） | **是**——杀掉旧的 `mcp-serve` 进程；Agent 不会帮你杀（Hermes：`pkill -f "bifrost mcp-serve"`） |
| Daemon Server | 离线 / GPU 机器 | **是**——`scripts/deploy/deploy-server.sh` 一次完成安装 → 重启 → 校验 |
| systemd unit | 离线 / GPU 机器 | 否——`install-systemd.sh` 负责渲染，`deploy-server.sh` 会调用它 |

```bash
# 在 daemon 上安装新版本
sudo dpkg -i bifrost_<版本>-1_amd64.deb
sudo systemctl restart bifrost

# tar.gz / 脚本路径（安装 + 重启 + 心跳 + 冒烟，失败回滚）
sudo scripts/deploy/deploy-server.sh --storage /data/liuxin/bifrost

# 开发者从源码构建
scripts/build/build.sh --static --dist
```

发布检查清单：

```
1. ✅ Client CLI    - 自动（无状态）
2. ⚠️ MCP Server   - 杀掉旧的 mcp-serve 进程
3. 🔄 Daemon       - scripts/deploy/deploy-server.sh --storage <dir>
4. ⏹️ systemd unit - deploy-server.sh 已处理
```

## MCP Server（Agent 接入）

Bifrost 内置 **MCP（Model Context Protocol）Server**，通过 stdio 把任务提交暴露为结构化工具。
**任意支持 MCP 的 Agent**——Hermes、OpenCode、Claude Code、Cline、Cursor、Codex 或通用编码
Agent——都可以直接连接并提交任务，无需自己写文件交换胶水代码。

MCP Server 是**无状态**的：每次工具调用直接读写共享存储。多个 Agent 可以同时连接（每个连接
一个进程实例）而互不冲突。

> 完整集成指南与各 Agent 配置示例见 [MCP.md](MCP.md)。

### 快速开始

**1. 安装二进制**（联网 / 客户端机器）——见[安装](#安装)。从发布包安装：

```bash
sudo dpkg -i bifrost_<版本>-1_amd64.deb   # 或解压 tar 包
```

也可以从源码编译：`cargo build --release` → `target/release/bifrost`。

**2. 准备配置文件**——把 `shared_storage` 指向离线 daemon 使用的同一个交换目录：

```json
{
  "shared_storage": "/gpfs/gcsp/liuxin/bifrost",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "180s" },
  "daemon": { "task_timeout": "300s", "heartbeat_interval": "60s", "max_concurrent": 10 }
}
```

**3. 在 Agent 中注册**——通用配置：

```
command: /path/to/bifrost
args:    ["mcp-serve"]
env:     BIFROST_CONFIG=/path/to/settings.json   （可选；否则回退到 ~/.bifrost/settings.json）
```

配置解析顺序：**`BIFROST_CONFIG` 环境变量 > `-c` 参数 > `~/.bifrost/settings.json` > 默认值**。

### Agent 注册

通用配置是 `command: /path/to/bifrost`、`args: ["mcp-serve"]`，可选 `BIFROST_CONFIG` 环境变量。
各 Agent 示例（Hermes、OpenCode、Claude Code、Cline/Cursor）见
[docs/developer/advanced_usage.zh-CN.md](docs/developer/advanced_usage.zh-CN.md)。

### MCP 工具

| 工具 | 用途 | 调用时机 |
|------|---------|-------------|
| `bifrost_health` | 检查离线 daemon 心跳是否新鲜 | 建议提交前调用：`alive=false` 时任务会排队，daemon 恢复后消费（启动时会做 catch-up 扫描） |
| `bifrost_submit` | 提交命令任务，返回 `task_id` | 参数：`command`（必填）、`timeout`、`priority`、`working_dir` |
| `bifrost_status` | 查询任务状态 | 用 `task_id` 轮询到终态 |
| `bifrost_result` | 获取完整结果 | 终态后获取 stdout/stderr/exit_code/duration_ms |

### 标准调用顺序

```
1. bifrost_health        → 确认 alive=true（daemon 在线）
2. bifrost_submit        → {"command": "echo hi", "timeout": 60} → task_id
3. bifrost_status        → 轮询：Pending → Running → Completed/Failed/Timeout
4. bifrost_result        → 取 stdout、退出码、耗时
```

**重要规则：**

- 默认 shell 语法可用（`use_shell=true`）；参数含字面量 `$` 或 `*` 的任务设置 `shell: false`
- 提交前检查 `bifrost_health`——daemon 未就绪时任务会排队，等它恢复
- daemon 按 `max_concurrent`（默认 10）并发执行；突发提交没有问题

## 开发

### 跑测试

```bash
# 全部测试（218 个用例：130 lib + 10 bin + 77 integration + 1 doctest）
cargo test --all

# 仅单元测试
cargo test --lib

# 集成测试——tests/unit 与 tests/integration 下的文件必须在 Cargo.toml 里用 [[test]] 声明，
# cargo 不会自动递归子目录（漏声明 = 静默不编译）
cargo test --test full_workflow_test
cargo test --test coverage_boost_test

# 端到端（与流水线同一套）
cargo build --release && python3 tests/e2e/run_all.py ./target/release/bifrost

# 覆盖率（需要 tarpaulin）
cargo tarpaulin --out Html
```

### 端到端测试（等价 CI）

完整 e2e 套件在 `tests/e2e/`（timeout、job、concurrent、多卡 pytest、robustness、MCP），并在
CI（`.github/workflows/ci.yml`）中运行：

```bash
cargo build --release
python3 tests/e2e/run_all.py ./target/release/bifrost   # 6 个场景，约 50s
```

CI 还会在每次推送时执行 `cargo fmt --check` 与 `cargo clippy --all-targets -- -D warnings`。

源码结构与内部实现见 [docs/developer/architecture.zh-CN.md](docs/developer/architecture.zh-CN.md)。

## 文档

- [架构与内部实现](docs/developer/architecture.zh-CN.md) —— 组件、任务生命周期、共享存储布局、安全模型、源码树
- [高级用法与配置](docs/developer/advanced_usage.zh-CN.md) —— daemon 调优、GPU 租用、scan 模式、SSH 传输、清理、pytest 容器、MCP Agent 配置
- [适配器指南](docs/adapter-guide.md) —— 自定义任务适配器
- [部署指南](docs/deployment.zh-CN.md) —— 生产部署、systemd、容器构建 → 宿主安装
- [运维实践](docs/ops-practice.md) —— 实际运维记录
- [MCP 集成指南](MCP.md) —— 通用 Agent 接入
- [测试报告](test.md) —— 性能、超时与 Job 工作流测试报告

## 排障

### Daemon 不消费任务

1. 检查共享存储路径权限
2. 验证 notify 库工作正常：`strace -e inotify_wait bifrost server`
3. 检查 `commands/` 目录是否存在
4. 确认两端把 `shared_storage` 解析到**同一个物理目录**（挂载错误看起来就像“任务永远不到”）
5. 检查 `scan.json` 是否新鲜——兜底扫描每秒更新，daemon 启动时还会做一次 catch-up 扫描

### 任务超时

1. 提交任务时增大 timeout
2. 检查 daemon 机器系统资源
3. 分析命令实际耗时

### 心跳超时

1. 确认 daemon 进程在运行：`systemctl status bifrost`
2. 检查 `heartbeat.json` 权限
3. 确认共享存储可访问

### 找不到 artifact

1. 确认命令确实产出了声明的 artifact
2. 检查工作目录
3. 确认 `artifact_name` 不含路径分隔符

## License

MIT License —— 见 [LICENSE](LICENSE)。

## 状态

Bifrost v0.1.1 已发布。核心功能稳定并有测试覆盖。后续计划：

- Web UI 监控面板
- 分布式执行（多 daemon）
- 任务依赖图（DAG 调度）
- 基于 AI 的任务调度优化
