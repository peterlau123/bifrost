# 🌉 Bifrost — Command Execution Framework for Offline Machines

![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)
![Rust Edition](https://img.shields.io/badge/Rust-2021%20Edition-dea584.svg)
![Platform](https://img.shields.io/badge/Platform-Linux-blue.svg)
![Tests](https://img.shields.io/badge/Tests-218%20passed-brightgreen.svg)
![MCP](https://img.shields.io/badge/MCP-5%20tools-8A2BE2.svg)

<p align="center">
  <img src="assets/bifrost-banner.svg" alt="Bifrost - the rainbow bridge connecting an online client and an offline daemon" width="720">
</p>

**English** | [简体中文](README.zh-CN.md)

Bifrost is a Rust framework for executing commands on offline / air-gapped machines through shared storage. A networked client machine submits tasks; an offline daemon machine executes them; results are synchronized back through the same portable storage.

## Table of Contents

- [Overview](#overview)
- [How It Works](#how-it-works)
- [Requirements](#requirements)
- [Installation](#installation)
  - [Debian / Ubuntu (.deb)](#debian--ubuntu-deb)
  - [Portable tarball](#portable-tarball)
  - [From source](#from-source)
  - [Client setup](#client-setup)
  - [Daemon setup](#daemon-setup)
  - [End-to-end verification](#end-to-end-verification)
- [Configuration](#configuration)
- [Basic Usage](#basic-usage)
- [Release and Update](#release-and-update)
- [MCP Server (Agent Integration)](#mcp-server-agent-integration)
- [Development](#development)
- [Documentation](#documentation)
- [Troubleshooting](#troubleshooting)
- [License](#license)
- [Status](#status)

---

## Overview

### Problem Solved

Traditional remote execution tools — SSH, RPC, REST APIs — do not work on air-gapped machines that have no network access. Bifrost uses **shared storage as the communication channel**:

- The **client** (networked machine) writes tasks into the `commands/` directory
- The **daemon** (offline machine) watches for new tasks and executes them
- **Results** are written back into the `results/` directory
- Synchronization happens over GPFS / NFS / Lustre or any other shared POSIX filesystem

### Key Features

- **Bridge abstraction** — transport-agnostic communication: shared storage (GPFS/NFS) **and SSH** are both implemented
- **Unified settings** — `~/.bifrost/settings.json`, initialized from the CLI
- **Air-gapped operation** — complete separation between client and server
- **Security hardened** — command injection prevention, path traversal protection
- **High performance** — inotify file watching plus a 1s fallback scan, tokio async execution, end-to-end overhead ~4–6%
- **Multiple task types** — shell commands, pytest runs, custom adapters
- **Parallel jobs** — `--parallel` submits all tasks concurrently (bounded by the daemon's `max_concurrent`)
- **Priority scheduling** — tasks are sorted by priority (0–255)
- **Timeout management** — configurable timeouts with optional retry
- **systemd integration** — production-ready server deployment
- **systemd supervision** — `Restart=on-failure`, readiness reported via sd_notify, starts on boot
- **MCP Server** — built in, so any MCP-capable agent (Hermes / OpenCode / Claude) can drive it directly
- **Health monitoring** — heartbeat-based server health checks

## How It Works

Bifrost exchanges JSON files through a POSIX directory that both machines can see:

1. The client writes a task to `commands/` (CLI, YAML job, or an MCP tool call).
2. The daemon watches that directory (`inotify` plus a fallback scan), claims the task, and executes it asynchronously.
3. The daemon writes `status/`, `logs/`, `artifacts/`, the final `results/` file, and a periodic `heartbeat.json`.

`shared` storage is the default transport; `ssh` is available when there is no shared filesystem.

See [docs/developer/architecture.md](docs/developer/architecture.md) for component diagrams, the full
task lifecycle, the shared-storage layout, design decisions, and the security model.

## Requirements

| Component | Requirement |
|---|---|
| **Platform** | Linux x86_64 — release binaries are statically linked musl |
| **Shared storage** | A POSIX directory readable and writable from both machines (GPFS / NFS / Lustre / local) |
| **Client** | The `bifrost` binary only — no Rust toolchain, no Python |
| **Daemon** | The same binary; `systemd` is recommended for production |
| **pytest tasks** | Python 3.10+, pytest 7.x and `pytest-json-report` on the daemon machine (optional) |

Building from source additionally requires Rust 1.87+.

## Installation

Only **Linux x86_64** is supported. The
[Releases page](https://github.com/peterlau123/bifrost/releases) offers three forms:

| Form | Use it when | What you get |
|---|---|---|
| `.deb` | Debian / Ubuntu | `dpkg -i`, `bifrost-setup`, systemd unit |
| `.tar.gz` | Any x86_64 Linux, no root, air-gapped | binary + `install.sh` (default prefix `/usr/local/bin`) |
| Source code | You want to audit or build yourself | GitHub-generated tag snapshot; needs Rust 1.87+ |

Every asset has a matching `.sha256`. `latest` is the rolling snapshot of `main`; `vX.Y.Z` tags are
official releases. Download examples:

```bash
BASE=https://github.com/peterlau123/bifrost/releases/download/latest
curl -fLO $BASE/bifrost-latest-x86_64-linux-musl.tar.gz
curl -fLO $BASE/bifrost-latest-x86_64-linux-musl.tar.gz.sha256
curl -fLO $BASE/bifrost_<version>-1_amd64.deb
curl -fLO $BASE/bifrost_<version>-1_amd64.deb.sha256
sha256sum -c *.sha256
```

### Debian / Ubuntu (.deb)

```bash
sudo dpkg -i bifrost_<version>-1_amd64.deb
bifrost --version

# client role: no sudo needed
bifrost-setup client --storage /data/liuxin/bifrost

# daemon role: sudo is required (writes /etc and registers systemd)
sudo bifrost-setup server --storage /data/liuxin/bifrost
```

The package installs `/usr/bin/bifrost`, `/usr/bin/bifrost-setup`, the systemd unit and
`/etc/default/bifrost`. The service is **not enabled by default**; `bifrost-setup server` enables and
starts it.

### Portable tarball

```bash
tar xzf bifrost-latest-x86_64-linux-musl.tar.gz
cd bifrost-latest-x86_64-linux-musl

# client (default binary prefix: /usr/local/bin)
./install.sh --role client --storage /data/liuxin/bifrost

# no root: choose your own prefix
PREFIX=$HOME/.local/bin ./install.sh --role client --storage /data/liuxin/bifrost

# daemon (writes /etc + systemd, so sudo)
sudo ./install.sh --role server --storage /data/liuxin/bifrost
```

Defaults and overrides:

| Item | Default | Override |
|---|---|---|
| Binary | `/usr/local/bin/bifrost` | `--prefix DIR` or `PREFIX=DIR` |
| Client config | `~/.bifrost/settings.json` | `--config FILE` |
| Daemon config | `/etc/bifrost/settings.json` | `--config FILE` |
| systemd unit | `/etc/systemd/system/bifrost.service` | `--unit-dir DIR` |

Run `./install.sh --help` for all options (`--no-hermes`, `--no-service`, `--dry-run`, …).

### From source

```bash
git clone https://github.com/peterlau123/bifrost.git
cd bifrost
cargo build --release

sudo install -m 0755 target/release/bifrost /usr/local/bin/bifrost

# or build the static musl artifact used by releases:
scripts/build/build.sh --static
```

### Client setup

`bifrost-setup client` (deb) or `install.sh --role client` (tarball) writes
`~/.bifrost/settings.json`. Manual equivalent:

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

Verify that the CLI works:

```bash
bifrost client submit --command "hostname" --timeout 30
bifrost client status <TASK_ID>
```

### Daemon setup

`bifrost-setup server` or `install.sh --role server` writes `/etc/bifrost/settings.json` and the
systemd unit. Manual equivalent:

```bash
sudo cp bifrost.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now bifrost
systemctl status bifrost
journalctl -u bifrost -f
```

Daemon settings (`/etc/bifrost/settings.json` or `~/.bifrost/settings.json`):

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

> **Important:** `shared_storage` must resolve to the same physical directory on the client and on
the daemon.

Run the pre-flight check on the daemon machine before starting it:

```bash
bifrost warmup --storage /mnt/gpfs/bifrost
```

For debugging, run the daemon in the foreground and stop it with `Ctrl+C`:

```bash
bifrost server --config ~/.bifrost/settings.json
```

The scripted path (`scripts/deploy/deploy-server.sh` / `deploy-client.sh`) does
install → restart → heartbeat → smoke test and rolls back on failure; see
[docs/deployment.md](docs/deployment.md).

### End-to-end verification

```bash
# daemon side: heartbeat exists and is fresh
cat /mnt/gpfs/bifrost/heartbeat.json | head -5

# client side
bifrost client submit \
  --command "sh -c 'echo \"bifrost deployment ok\"; nvidia-smi -L | head -3'" \
  --timeout 30
bifrost client status <TASK_ID>
# Expected: Completed, output contains "bifrost deployment ok"
```

Commands run through `sh -c` by default (`daemon.use_shell: true`); set `use_shell: false` or a
task's `shell: false` when arguments must stay literal.

## Configuration

Settings resolve in this order: **`BIFROST_CONFIG` env > `-c` flag > `~/.bifrost/settings.json` >
defaults**.

```json
{
  "shared_storage": "/mnt/gpfs/bifrost",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "180s" },
  "daemon": { "max_concurrent": 10, "task_timeout": "300s" }
}
```

`--storage DIR` overrides `shared_storage` for any subcommand; `--gpus LIST` overrides
`daemon.gpu_pool`.

The full reference — every `daemon.*` field, timeout/retry semantics, GPU leasing, clock/scan modes,
and the SSH transport — is in
[docs/developer/advanced_usage.md](docs/developer/advanced_usage.md).

## Basic Usage

### Shared storage and pre-flight check

Both machines must see the same POSIX directory. On the daemon machine, run the pre-flight check
before the first start:

```bash
bifrost warmup --storage /data/liuxin/bifrost
```

It verifies writability, clock behaviour, the existing heartbeat and GPU visibility; details in
[advanced_usage.md](docs/developer/advanced_usage.md#pre-flight-check-bifrost-warmup).

### Submit, inspect, cancel

```bash
# shell command (shell syntax works by default; daemon.use_shell=true)
bifrost client submit --command "pytest tests/unit/ -v" --timeout 600
bifrost client submit --command "python script.py --input data.csv" --priority 10
bifrost client submit --command "..." --retries 2

# multi-step YAML job
bifrost client submit --job examples/quick_start.yaml

# status / cancel
bifrost client status <TASK_ID>
bifrost client cancel <TASK_ID>
```

Tasks whose arguments contain literal `$` or `*` can disable the shell per task (`shell: false`); see
the advanced reference.

### Stopping the daemon

- `Ctrl+C` / `SIGTERM`: graceful shutdown within 500ms; a second `Ctrl+C` exits immediately.
- The daemon kills the process group of every running task, so no orphan keeps holding a GPU.

Advanced operations — cleanup, task types and priority, retries, pytest containers, SSH transport —
are documented in [docs/developer/advanced_usage.md](docs/developer/advanced_usage.md).

## Release and Update

One binary serves four roles — Client CLI, Daemon Server, MCP Server, and (optionally) systemd acting as the
supervisor. Updating it is mostly about knowing which of them keeps running:

| Role | Runs on | Needs a restart after a new build? |
|---|---|---|
| Client CLI | online machine | No — every invocation re-execs the binary |
| MCP Server | online machine (agent side) | **Yes** — kill the old `mcp-serve` process; the agent will not do it for you (Hermes: `pkill -f "bifrost mcp-serve"`) |
| Daemon Server | offline / GPU machine | **Yes** — `scripts/deploy/deploy-server.sh` does install → restart → verify in one shot |
| systemd unit | offline / GPU machine | No — `install-systemd.sh` renders it, and `deploy-server.sh` calls it |

```bash
# install the new release on the daemon
sudo dpkg -i bifrost_<version>-1_amd64.deb
sudo systemctl restart bifrost

# tarball / scripted path (install + restart + heartbeat + smoke, rollback on failure)
sudo scripts/deploy/deploy-server.sh --storage /data/liuxin/bifrost

# developers building from source
scripts/build/build.sh --static --dist
```

Release checklist:

```
1. ✅ Client CLI    - automatic (stateless)
2. ⚠️ MCP Server   - kill the old mcp-serve process
3. 🔄 Daemon       - scripts/deploy/deploy-server.sh --storage <dir>
4. ⏹️ systemd unit - handled by deploy-server.sh
```

## MCP Server (Agent Integration)

Bifrost ships a **built-in MCP (Model Context Protocol) server** that exposes task submission as structured tools over stdio. **Any MCP-capable agent** — Hermes, OpenCode, Claude Code, Cline, Cursor, Codex, or any generic coding agent — can connect and submit tasks to the offline daemon without writing file-exchange glue code.

The MCP server is **stateless**: every tool call reads and writes shared storage directly. Multiple agents can connect simultaneously (one process instance per connection) without conflicts.

> The full integration guide, including every agent's config sample, is in [MCP.md](MCP.md).

### Quick Start

**1. Install the binary** (on the online/client machine) — see [Installation](#installation).
From a release:

```bash
sudo dpkg -i bifrost_<version>-1_amd64.deb   # or unpack the tarball
```

Building from source works too: `cargo build --release` → `target/release/bifrost`.

**2. Prepare a settings file** — point `shared_storage` at the same exchange directory the offline daemon uses:

```json
{
  "shared_storage": "/gpfs/gcsp/liuxin/bifrost",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "180s" },
  "daemon": { "task_timeout": "300s", "heartbeat_interval": "60s", "max_concurrent": 10 }
}
```

**3. Register it in your agent** — the universal config is:

```
command: /path/to/bifrost
args:    ["mcp-serve"]
env:     BIFROST_CONFIG=/path/to/settings.json   (optional; falls back to ~/.bifrost/settings.json)
```

Config resolution order: **`BIFROST_CONFIG` env > `-c` flag > `~/.bifrost/settings.json` > defaults**.

### Agent registration

The universal config is `command: /path/to/bifrost`, `args: ["mcp-serve"]`, with an optional
`BIFROST_CONFIG` environment variable. Per-agent samples (Hermes, OpenCode, Claude Code, Cline/Cursor)
are in
[docs/developer/advanced_usage.md](docs/developer/advanced_usage.md#mcp-agent-configurations).

### MCP Tools

| Tool | Purpose | When to call |
|------|---------|-------------|
| `bifrost_health` | Check offline daemon heartbeat freshness | Recommended before submit: if `alive=false`, tasks stay queued until the daemon returns (the daemon performs a catch-up scan at startup) |
| `bifrost_submit` | Submit a command task, returns `task_id` | Args: `command` (required), `timeout`, `priority`, `working_dir` |
| `bifrost_status` | Query task status | Poll with `task_id` until terminal state |
| `bifrost_result` | Fetch full result | After terminal state: stdout/stderr/exit_code/duration_ms |

### Standard Call Sequence

```
1. bifrost_health        → confirm alive=true (daemon online)
2. bifrost_submit        → {"command": "echo hi", "timeout": 60} → task_id
3. bifrost_status        → poll: Pending → Running → Completed/Failed/Timeout
4. bifrost_result        → get stdout, exit code, duration
```

**Important rules:**

- Shell syntax works by default (`use_shell=true`); set `shell: false` for tasks whose arguments
  contain literal `$` or `*`
- Check `bifrost_health` before submitting — a not-ready daemon makes submits queue until it returns
- The daemon executes concurrently up to `max_concurrent` (default 10); burst submits are fine

## Development

### Running Tests

```bash
# All tests (218 cases: 130 lib + 10 bin + 77 integration + 1 doctest)
cargo test --all

# Unit tests only
cargo test --lib

# Integration tests — tests/unit 与 tests/integration 下的文件必须在 Cargo.toml 里用 [[test]] 声明，
# cargo 不会自动递归子目录（漏声明 = 静默不编译）
cargo test --test full_workflow_test
cargo test --test coverage_boost_test

# End-to-end (same suite the pipeline runs)
cargo build --release && python3 tests/e2e/run_all.py ./target/release/bifrost

# Coverage (requires tarpaulin)
cargo tarpaulin --out Html
```

### End-to-End Tests (CI-equivalent)

The full e2e suite lives in `tests/e2e/` (timeout, job, concurrent, multi-GPU pytest, robustness, MCP) and runs in CI (`.github/workflows/ci.yml`):

```bash
cargo build --release
python3 tests/e2e/run_all.py ./target/release/bifrost   # 6 suites, ~50s
```

CI also enforces `cargo fmt --check` and `cargo clippy --all-targets -- -D warnings` on every push.

See [docs/developer/architecture.md](docs/developer/architecture.md) for the source layout, component
diagrams, and internals.

## Documentation

- [Architecture and Internals](docs/developer/architecture.md) — components, task lifecycle, shared-storage layout, security model, source tree
- [Advanced Usage and Configuration](docs/developer/advanced_usage.md) — daemon tuning, GPU leasing, scan modes, SSH bridge, cleanup, pytest containers, MCP agent configs
- [Adapter Guide](docs/adapter-guide.md) — custom task adapters
- [Deployment Guide](docs/deployment.md) — production deployment, systemd, container-build → host-install flow
- [Operations Practice](docs/ops-practice.md) — real-world operational notes
- [MCP Integration Guide](MCP.md) — universal agent integration (OpenCode / Claude Code / Cline / Cursor / Hermes)
- [Test Report](test.md) — performance, timeout and job-workflow test reports

## Troubleshooting

### Daemon Not Detecting Tasks

1. Check shared storage path permissions
2. Verify the notify library works: `strace -e inotify_wait bifrost server`
3. Check that the `commands/` directory exists
4. Confirm both machines resolve `shared_storage` to the **same physical directory** (a wrong mount looks like "tasks never arrive")
5. Check `scan.json` freshness — the fallback scan writes it every second, and the daemon also performs a catch-up scan at startup

### Task Timeout

1. Increase the timeout when submitting the task
2. Check system resources on the daemon machine
3. Profile how long the command actually takes

### Heartbeat Timeout

1. Verify the daemon process is running: `systemctl status bifrost`
2. Check `heartbeat.json` permissions
3. Verify the shared storage is accessible

### Artifact Not Found

1. Verify the command actually produces the declared artifacts
2. Check the working directory
3. Verify `artifact_name` contains no path separators

## License

MIT License — see the [LICENSE](LICENSE) file.

## Status

Bifrost v0.1.1 is released. Core functionality is stable and tested. Planned enhancements:

- Web UI monitoring dashboard
- Distributed execution (multiple daemons)
- Task dependency graph (DAG scheduling)
- AI-based task scheduling optimisation
