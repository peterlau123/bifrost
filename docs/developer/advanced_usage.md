# Advanced Usage and Configuration

Reference material for tuning Bifrost and for less common workflows. Start with the
[README](../../README.md) for installation and the basic submit/status/cancel loop; this document covers
the full settings surface, storage semantics, SSH transport, task scheduling, cleanup, pytest
containers, and per-agent MCP registration.

> [简体中文](advanced_usage.zh-CN.md) | English

## Configuration

### Settings file and precedence

`~/.bifrost/settings.json` (client) or `/etc/bifrost/settings.json` (daemon, when installed from the deb):

```bash
bifrost server init --storage /mnt/gpfs/bifrost   # generate daemon-side defaults
```

```json
{
  "shared_storage": "/tmp/bifrost",
  "client": {},
  "daemon": {}
}
```

Resolution order: **`BIFROST_CONFIG` env > `-c` flag > `~/.bifrost/settings.json` > defaults**.

| Field | Client | Server | Description |
|-------|--------|--------|-------------|
| shared_storage | Yes | Yes | Root directory for commands/results/logs |
| transport | Yes | -- | Bridge type: `"shared"` (default) or `"ssh"` |
| ssh | Yes | -- | SSH bridge config (host/remote_dir/...) — required when transport=ssh |
| client.* | Yes | -- | Poll interval, heartbeat timeout, wait timeout (12h cap) |
| daemon.* | -- | Yes | Concurrency, timeout, retry, working dir |

`--storage DIR` and `--gpus LIST` are global flags that override `shared_storage` and
`daemon.gpu_pool` for any subcommand.

### `daemon.*` in detail

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

| Field | Default | Description |
|-------|---------|-------------|
| task_timeout | 1h30m | Default timeout when a task does not specify one. **No longer a cap**: a task's own `timeout` wins |
| max_concurrent | 10 | Number of tasks executed concurrently (1–100) |
| use_shell | true | Commands run through `sh -c`, so pipes, redirection, `&&`, `$VAR`, `&` all work; set false to exec argv directly. A single task can override with `shell: false` (use it for tasks containing literal `$` / `*`) |
| scan_interval | 1s | Fallback scan period in addition to inotify. An idle daemon no longer busy-polls and burns CPU; the scan thread writes `scan.json` (ticks/scanned_at) every second, so you can tell whether the fallback scan is still alive |
| stale_marker_timeout | 2×task_timeout | A `.processing` claim marker older than this is reclaimed. Ownership is verified before reclaiming: the marker records host/pid/task, and tasks owned by a live local process or by this process are skipped. Keepalive uses `min(60s, value/3)` |
| max_retries | unlimited | Upper bound on retries (unset = no bound). Retrying itself is opt-in per task: `client submit --retries 2` or YAML `retries: 2`; without it a task runs once. Only non-zero exits are retried — timeouts and cancellations are not. Each attempt is logged separately as `stdout.attemptN.log` |
| gpu_pool | **auto** | **Unset = automatically use every GPU on the machine** (detected via `nvidia-smi`). Each task gets an exclusive card injected as `CUDA_VISIBLE_DEVICES` (if the task names one itself, that id is leased but the value is not overwritten); the concurrency cap becomes `min(max_concurrent, #GPUs)`. `[]` disables GPU assignment, `[0,1]` uses only those two |
| cancel_poll | 1s | How often a running task checks for a cancel request. 1s balances cancellability against metadata overhead; 30s would mean a runaway benchmark keeps burning the GPU for half a minute before it dies |
| log_backups | 1 | Number of rotated backups to keep (`.1`..`.N`, max 9). Each stream rotates at a fixed 64MB, so disk usage is bounded by `64MB × (log_backups+1)`; the newest content is always in `stdout.log` |
| — | fixed | Result payload cap 64MB (16MB head context + 48MB tail summary) — benchmark summaries sit at the tail of the stream, so the tail is weighted more heavily; full logs live in `logs/<id>/` |

**Timeout semantics:** `client submit --timeout 0` (the default) means "use the daemon's
`task_timeout`"; passing a concrete number makes the task's own value authoritative — the daemon no
longer clamps it.

**Client-side cap:** `client.wait_timeout` (default 12h) is the hard upper bound on how long the client
waits for a single job, so a dead daemon cannot block the client forever.

**Shell semantics:** `use_shell=true` is the default. Commands are executed as `sh -c "<command>"`, so
pipes, redirects, `&&`, `$VAR` and background `&` work out of the box. For a task whose arguments must
stay literal (for example a regex containing `*` or a literal `$`), disable the shell for that task:

```json
{ "command": "grep -n 'a.*b' file.txt", "shell": false }
```

### GPU scheduling

- Unset `gpu_pool` = use every GPU detected by `nvidia-smi`.
- `[0,1]` = lease only cards 0 and 1; `[]` = disable GPU assignment entirely.
- Each task gets an exclusive card through `CUDA_VISIBLE_DEVICES`; if the task sets the variable
  itself, the id is still leased but the value is not overwritten.
- Effective concurrency is `min(max_concurrent, number of leased GPUs)`.

### Clock self-check and fallback scanning

Some virtualised environments (for example certain VM/container hosts) freeze `CLOCK_MONOTONIC`:
`sleep()` deadlines never arrive, threads hang forever, and the daemon looks like "heartbeat fine but
tasks never consumed". Most modern hosts do not have this defect, so **the default does not make every
machine pay for it**:

| scan_mode | Behaviour |
|---|---|
| `auto` (default) | Spend ~100ms at startup comparing wall clock against the monotonic clock; switch to spin only when the two are confirmed to diverge |
| `sleep` | Force timer-driven scanning (lowest CPU), for machines known to be healthy |
| `spin` | Force busy-poll (~1 core), only for machines that really have a clock problem |

The self-check result is printed in the startup log: `clock: ok (sleep-driven scan)` or
`WARNING: ... busy-poll`. If you still suspect a stall at runtime, use the external watchdog
`scripts/ops/watchdog.sh` (reads the age of `heartbeat.json` / `scan.json` and restarts past a
threshold) — it runs from cron or a systemd timer, so it does not share a failure domain with the
daemon.

### SSH bridge

When the client and the target machine have **no shared filesystem** but are reachable over SSH, use
SSH as the transport: the client reads and writes the target's
`commands/ results/ status/ artifacts/` directories over ssh, and the daemon on the target keeps
consuming tasks through the local protocol — both sides keep identical directory semantics, so the
daemon needs no changes.

```json
{
  "shared_storage": "/tmp/bifrost",            // the daemon still uses this for its local paths
  "transport": "ssh",
  "ssh": {
    "host": "target-machine.example.com",      // required
    "user": "bifrost",                          // optional, defaults to the current user
    "remote_dir": "/gpfs/bifrost",              // required: directory acting as shared_storage on the target
    "port": 22,                                 // optional, default 22
    "connect_timeout": "10s"                    // optional, per-connection timeout (default 10s)
  },
  "client": {},
  "daemon": {}
}
```

Requirements: passwordless `ssh <host>` from the client (`BatchMode=yes`). File contents travel over
ssh stdin (`cat >` plus an atomic `mv`), and paths are shell-escaped.

## Advanced usage

### Shared storage semantics

Bifrost only requires **a POSIX directory both sides can read and write**; it is not tied to GPFS, and
the same directory may be a network mount on one machine and a local path on the other.

```bash
# daemon side
bifrost warmup  --storage /data/liuxin/bifrost   # storage / clock / GPU self-check
bifrost server  --storage /data/liuxin/bifrost   # GPU pool auto-detects every card
# limit cards with --gpus 0,3,5; disable with --gpus "" or "gpu_pool": []

# client side
bifrost client submit --storage /data/liuxin/bifrost --command 'nvidia-smi -L'
```

- On first run `commands/ results/ status/ artifacts/ logs/ cancel/` are created automatically,
  along with `heartbeat.json` / `scan.json`.
- If the directory is a network filesystem shared by several machines, inotify may drop events — the
  fallback scan (1s by default) covers it, so GPFS is not required.
- The repository's `dist/` directory may contain two local builds: `bifrost-linux-x86_64` is musl
  static, while the `-glibc` variant requires glibc ≥ 2.39. Use the static one when unsure.

### Pre-flight check: `bifrost warmup`

**Step one of every deployment, whatever the filesystem is** — GPFS, NFS, Lustre or a plain local
directory all go through it. Run it on the machine that will host the daemon, before the first start:

```bash
bifrost warmup --storage /data/liuxin/bifrost   # or: bifrost --warmup
```

It checks shared-storage writability and filesystem type, performs the clock self-check (whether
`sleep()` can hang), reads the existing daemon heartbeat and reports GPU visibility. It exits 1 on
blocking findings, so it can gate a deploy script — which is exactly what
`scripts/deploy/deploy-server.sh` does. The systemd unit also runs it on every start (`ExecStartPost`).

**Why it has to run on the server side:** every check is a property of the executing side — a frozen
clock affects the daemon's fallback scan thread, the GPUs only exist on the target machine, and
writability depends on the daemon user's permissions. Running it on the client only proves "the shared
directory is writable from the client's point of view", which tells you nothing when the daemon fails
to start (different user, different machine).

### Task types

| Type | Description | Use Case |
|------|-------------|----------|
| `shell` | Shell command execution | Build scripts, data processing |
| `pytest` | Python test execution | Unit tests, integration tests |
| `custom` | Adapter-based execution | Docker, specialised tools |

Commands whose first word is `pytest` are treated as pytest tasks automatically.

### Task priority

Priority ranges from 0 to 255; lower numbers run first.

| Range | Priority | Example |
|-------|----------|---------|
| 0-10 | High | Critical fixes, urgent tests |
| 11-50 | Normal | Regular tests, builds |
| 51-100 | Low | Background tasks, cleanup |
| 101-255 | Lowest | Maintenance, logs |

```bash
bifrost client submit --command "pytest tests/unit/ -v" --priority 10 --timeout 600
```

### Retries and timeouts

Retries are opt-in per task and capped by `daemon.max_retries`:

```bash
bifrost client submit --command "..." --retries 2   # run once, then retry up to twice
bifrost client submit --command "..." --timeout 0   # 0 = use daemon.task_timeout
```

Only non-zero exits are retried; timeouts and cancellations are final. Each attempt is logged
separately (`stdout.attemptN.log`).

### YAML jobs

Multi-step sequential jobs use a YAML file:

```bash
bifrost client submit --job examples/quick_start.yaml
```

See `examples/` for pytest, benchmark and smoke-test job definitions.

### Clean up old tasks

Long-running deployments accumulate files in
`commands/ results/ status/ logs/ artifacts/`. `clean` removes all files of **finished tasks** (those
with a result file) older than N days:

```bash
# Preview what would be removed (safe, no deletion)
bifrost client clean --older-than 7 --dry-run

# Actually remove finished tasks' files older than 7 days
bifrost client clean --older-than 7

# Target a specific storage directory
bifrost client clean --storage /gpfs/gcsp/liuxin/bifrost_test --older-than 30
```

**Safety guarantees:**

- Only tasks **with a result file** (terminal state) are touched — pending/running tasks are never candidates
- `heartbeat.json` and `settings.json` are **never** removed
- `--dry-run` previews before any deletion; `--older-than` defaults to 7 days

### Pytest in a container

To run pytest in a container on the offline machine:

1. **Create a Docker adapter**:

```yaml
# /etc/bifrost/adapters/docker_pytest.yaml
name: docker_pytest
command_template: "docker run --rm -v {working_dir}:/app -w /app python:3.10-slim pytest {path} --json-report --json-report-file=report.json"
timeout: 600
artifacts:
  - report.json
```

2. **Submit a task with the adapter**:

```bash
bifrost client submit \
  --command "docker run --rm -v /workspace/myproject:/app -w /app python:3.10-slim pytest tests/unit/ --json-report --json-report-file=report.json" \
  --working-dir /workspace/myproject \
  --timeout 600
```

### Pytest prerequisites

Install on the offline machine (or inside the container):

```bash
pip install pytest pytest-json-report pytest-cov
```

## MCP agent configurations

The universal registration is `command: /path/to/bifrost`, `args: ["mcp-serve"]`, and an optional
`BIFROST_CONFIG` environment variable. Config resolution order is
**`BIFROST_CONFIG` env > `-c` flag > `~/.bifrost/settings.json` > defaults**.

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

or project-level `.mcp.json`:

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

### Cline / Cursor / generic agents

Use the same `mcpServers` JSON shape with `command` + `args` + `env`. See [MCP.md](MCP.md) for
screenshot-level detail.

### MCP verification

```bash
# Via Hermes
hermes mcp test bifrost

# Manual JSON-RPC handshake (agent-agnostic)
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"inspector","version":"1.0"}}}' \
  | bifrost mcp-serve | head -3
```

### MCP troubleshooting

| Problem | Cause | Fix |
|---------|-------|-----|
| Connection closed on add | shared_storage path not writable/missing | Fix `shared_storage` in settings.json |
| `alive: false` | daemon not running or heartbeat stale | Start `bifrost server -c <cfg>` on the target machine, wait 2s |
| Task stuck Pending | daemon not running, or the two machines point at different storage | Check health, confirm `shared_storage` is the same physical directory, then check `scan.json` |
| submit error | command format issue | Check quoting; shell syntax works by default (`use_shell=true`) |
| Multi-agent usage | — (no conflict, file-based) | No extra config needed |
