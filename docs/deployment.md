# Deployment Guide

**English** | [简体中文](deployment.zh-CN.md)

> **This guide covers two deployment models.**
>
> - **Shared-storage deployment** (H200 / NFS / GPFS) — what production actually uses today; it is the
>   last section: build in a container → install on the host, `install-systemd.sh`, `deploy-h200.sh`,
>   plus the measured numbers.
> - **Local private layout** (`/var/lib/bifrost` + `/etc/bifrost`, unit shipped in the repo root) — the
>   legacy layout this document grew from; nothing installs it any more (use `scripts/deploy/deploy-server.sh`).
>   Parts of this half also describe behaviour the current release does **not** implement: the Prometheus
>   `metrics:` block in settings.json, and `/usr/local/bin/bifrost-health-check.sh` (the repo ships
>   `scripts/validate/health-check.sh` and nothing installs it). Treat it as a template, not as shipped features.

This guide covers production deployment of Bifrost using systemd.

## Overview

Bifrost is designed for production deployment with:
- systemd service integration
- Health monitoring
- Log management
- Resource limits
- Security hardening

## Prerequisites

- Linux system with systemd (Ubuntu, Debian, CentOS, RHEL, etc.)
- Root access for installation
- Rust toolchain for building (optional, pre-built binary available)
- 2GB+ RAM recommended
- 10GB+ disk space for logs

## Quick Deployment

### Automated Installation

```bash
# Everything in one command (requires root)
sudo scripts/deploy/deploy-server.sh --storage /data/liuxin/bifrost
```

That script:
1. Gates the candidate binary (static link → `--version` → `warmup`)
2. Writes the server-side settings via `bifrost server init`
3. Installs to `/usr/local/bin/bifrost` (backing up `.old`)
4. Renders and restarts the systemd unit (`scripts/deploy/install-systemd.sh`)
5. Waits for a fresh heartbeat and runs one real task as a smoke test, rolling back on failure

Optional flags: `--binary --config --prefix --service-name --gpus --no-systemd --skip-build --skip-warmup --skip-smoke --force`.
The manual steps below remain as a reference for the layout this script automates.

### Manual Installation

Step-by-step manual deployment:

```bash
# 1. Build binary
cargo build --release

# 2. Install binary
sudo cp target/release/bifrost /usr/local/bin/
sudo chmod +x /usr/local/bin/bifrost

# 3. Create directories
sudo mkdir -p /var/lib/bifrost/{pending,results,completed,logs}

# 4. Create configuration
sudo mkdir -p /etc/bifrost
sudo tee /etc/bifrost/settings.json >/dev/null <<'JSON'
{
  "shared_storage": "/data/liuxin/bifrost",
  "client": { "poll_interval": "1s", "wait_timeout": "12h" },
  "daemon": {
    "task_timeout": "1h30m",
    "max_concurrent": 8,
    "use_shell": true,
    "scan_interval": "1s",
    "gpu_pool": [0, 1, 2, 3, 4, 5, 6, 7],
    "working_dir": "/data/liuxin/bifrost/work"
  }
}
JSON

# 4.1 Pre-flight self-check: every deployment, any filesystem (GPFS/NFS/local alike);
#     must run on the server side, exits 1 on blocking findings
sudo bifrost warmup --config /etc/bifrost/settings.json


# 5. Install systemd service
sudo cp bifrost.service /etc/systemd/system/
sudo systemctl daemon-reload

# 6. Enable and start
sudo systemctl enable bifrost
sudo systemctl start bifrost
```

## Configuration

### Server Configuration


Edit `~/.bifrost/settings.json`:


```json
{
  "shared_storage": "/var/lib/bifrost",
  "daemon": {
    "max_concurrent": 4,
    "task_timeout": "3600s"
  }
}
```

### systemd Service Configuration

Edit `/etc/systemd/system/bifrost.service`:

```ini
[Unit]
Description=Bifrost Server
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/bifrost server
Restart=on-failure
RestartSec=10

# Resource limits
MemoryMax=2G
TasksMax=100

[Install]
WantedBy=multi-user.target
```

After editing:
```bash
sudo systemctl daemon-reload
sudo systemctl restart bifrost
```

## Service Management

### Start Service

```bash
sudo systemctl start bifrost
```

### Stop Service

```bash
sudo systemctl stop bifrost
```

### Restart Service

```bash
sudo systemctl restart bifrost
```

### Check Status

```bash
sudo systemctl status bifrost
```

Output shows:
- Service state (running/stopped)
- Recent logs
- Process ID
- Memory usage

### View Logs

```bash
# Live logs
journalctl -u bifrost -f

# Recent logs
journalctl -u bifrost --since "1 hour ago"

# Logs from specific time
journalctl -u bifrost --since "2026-07-04 10:00:00"
```

## Health Monitoring

### Health Check Script

```bash
# Full health check
/usr/local/bin/bifrost-health-check.sh full

# Heartbeat check
/usr/local/bin/bifrost-health-check.sh heartbeat

# Service check
/usr/local/bin/bifrost-health-check.sh startup
```

### Monitoring Integration

#### Cron-based Monitoring

Add to cron:
```bash
# Check health every 5 minutes
*/5 * * * * /usr/local/bin/bifrost-health-check.sh full || systemctl restart bifrost
```

#### Prometheus Integration

Expose metrics:
```yaml
# Add to settings.json
metrics:
  enabled: true
  port: 9090
  path: /metrics
```

Metrics endpoint: `http://localhost:9090/metrics`

#### Nagios Integration

Create check script:
```bash
#!/bin/bash
# /usr/lib/nagios/plugins/check_bifrost

/usr/local/bin/bifrost-health-check.sh full
EXIT_CODE=$?

if [ $EXIT_CODE -eq 0 ]; then
    echo "BIFROST OK - Service healthy"
    exit 0
else
    echo "BIFROST CRITICAL - Service unhealthy"
    exit 2
fi
```

## Resource Management

### Memory Limits

Set in systemd service:
```ini
MemoryMax=2G
MemoryHigh=1G
MemoryMin=512M
```

### CPU Limits

```ini
CPUQuota=80%
CPUShares=1024
```

### Disk Management

Clean old logs:
```bash
# Daily cleanup (add to cron)
0 2 * * * find /var/lib/bifrost/logs -mtime +30 -delete
```

Or systemd tmpfiles:
```bash
# /etc/tmpfiles.d/bifrost.conf
d /var/lib/bifrost/logs 0755 root root 30d
```

## Security Hardening

### systemd Security Options

Add to service file:
```ini
# Security hardening
NoNewPrivileges=true
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/bifrost

# Network isolation (if no network needed)
PrivateNetwork=yes

# User namespace isolation
PrivateUsers=yes

# Restrict system calls
SystemCallFilter=@basic @system-service
```

### File Permissions

```bash
# Set ownership
sudo chown -R root:root /var/lib/bifrost
sudo chmod -R 750 /var/lib/bifrost

# Configuration
sudo chmod 640 ~/.bifrost/settings.json
```

### SELinux/AppArmor

For SELinux systems:
```bash
# Create policy
sudo semanage fcontext -a -t bifrost_data_t "/var/lib/bifrost(/.*)?"
sudo restorecon -R /var/lib/bifrost
```

For AppArmor:
```bash
# Create profile
sudo aa-genprof /usr/local/bin/bifrost
```

## Backup and Recovery

### Backup Configuration

```bash
# Backup config
sudo tar -czf bifrost-config-backup.tar.gz ~/.bifrost/

# Backup data
sudo tar -czf bifrost-data-backup.tar.gz /var/lib/bifrost/
```

### Recovery Procedure

```bash
# Stop service
sudo systemctl stop bifrost

# Restore config
sudo tar -xzf bifrost-config-backup.tar.gz -C /

# Restore data
sudo tar -xzf bifrost-data-backup.tar.gz -C /

# Start service
sudo systemctl start bifrost
```

## Upgrading

### Upgrade Procedure

```bash
# 1. Stop service
sudo systemctl stop bifrost

# 2. Backup
sudo cp /usr/local/bin/bifrost /usr/local/bin/bifrost.bak

# 3. Build new version
cargo build --release

# 4. Install new binary
sudo cp target/release/bifrost /usr/local/bin/

# 5. Start service
sudo systemctl start bifrost

# 6. Verify
sudo systemctl status bifrost
bifrost-health-check.sh full
```

### Rolling Back

```bash
# If upgrade fails
sudo systemctl stop bifrost
sudo cp /usr/local/bin/bifrost.bak /usr/local/bin/bifrost
sudo systemctl start bifrost
```

## Multi-Instance Deployment

For multiple daemon instances:

```bash
# Create instance 1
sudo cp bifrost.service /etc/systemd/system/bifrost-1.service
sudo mkdir -p /var/lib/bifrost-1

# Create instance 2
sudo cp bifrost.service /etc/systemd/system/bifrost-2.service
sudo mkdir -p /var/lib/bifrost-2

# Configure each
bifrost server --init
# Edit ~/.bifrost/settings.json to set shared_storage: /var/lib/bifrost-1

# For second daemon:
bifrost server --init
# Edit ~/.bifrost/settings.json to set shared_storage: /var/lib/bifrost-2

# Start instances
sudo systemctl start bifrost-1
sudo systemctl start bifrost-2
```

## Troubleshooting

### Service Won't Start

Check logs:
```bash
journalctl -u bifrost -n 50
```

Common issues:
- Missing configuration: Create `~/.bifrost/settings.json`
- Permission denied: Check file permissions
- Port conflict: No network port needed for bifrost
- Resource limits: Increase MemoryMax

### Service Crashes

Check:
```bash
# System logs
journalctl -u bifrost --since "10 minutes ago"

# Application logs
ls -lh /var/lib/bifrost/logs/

# Disk space
df -h /var/lib/bifrost
```

### Tasks Not Executing

Check:
```bash
# Pending tasks
ls /var/lib/bifrost/pending/

# Daemon logs
journalctl -u bifrost | grep "Watcher"

# Configuration
cat ~/.bifrost/settings.json
```

### Health Check Fails

```bash
# Check heartbeat
cat /var/lib/bifrost/logs/heartbeat.json

# Check service
systemctl status bifrost

# Manual check
bifrost-health-check.sh heartbeat
```

## Best Practices

1. **Resource limits**: Always set MemoryMax and CPUQuota
2. **Log rotation**: Configure automatic log cleanup
3. **Health monitoring**: Set up automated health checks
4. **Backup**: Regular backups of configuration and data
5. **Security**: Use systemd hardening options
6. **Testing**: Test upgrades in staging environment
7. **Documentation**: Document custom configurations

## See Also

- [README.md](../README.md) - Project overview
- [Adapter Guide](adapter-guide.md) - Task adapters


## Shared-directory deployment (H200 / non-GPFS)

Bifrost only requires a POSIX directory both sides can read and write; GPFS is not mandatory.
On H200 the host's shared `/data` is enough (inside containers it is often mounted as `/workspace`).

```bash
# ---- H200 (server side) -----------------------------------------
cd /data/liuxin/Ax/bifrost            # the repo already lives on shared storage

# Option A: use the prebuilt static binary in the repo (recommended, no toolchain needed)
#   dist/bifrost-linux-x86_64 is musl-static; verified to run on a glibc 2.35 host
sudo cp dist/bifrost-linux-x86_64 /usr/local/bin/bifrost && sudo chmod +x /usr/local/bin/bifrost
# Option B: build on the host (unavailable when the host has no Rust toolchain - see the next section)
#   cargo build --release            # gnu artifact target/release/bifrost, only for glibc>=2.39 targets
#   scripts/build/find-binary.sh release   # let cargo report the real path
#   Note: on an older-glibc target the gnu artifact may still start (weak version only warns),
#   so do not rely on `--version` to decide

bifrost warmup --storage /data/liuxin/bifrost
#   ✓ shared storage is writable
#   ✓ filesystem: xfs
#   ✓ clock: ok (sleep-driven scanning is safe)
#   ✓ 8 GPU(s) visible via nvidia-smi
bifrost server --storage /data/liuxin/bifrost
```

### Building in the dev container → installing on the host

Measured on the H200 host (Ubuntu 22.04, glibc 2.35) there is **no** cargo/rustc: `command -v cargo`
is empty, and `find / -maxdepth 4 -name cargo` finds nothing. Meanwhile the dev container (Debian 13,
glibc 2.41) produces a gnu artifact that references `GLIBC_2.39`.

This pitfall is sneakier than it looks: **the host does not fail immediately** — it prints a single
line, "weak version 'GLIBC_2.39' not found", and then starts and executes tasks as usual, because the
symbol in question is a *weak* reference and the dynamic linker only warns. So "`--version` returned 0"
proves nothing; it blows up the day some code path actually calls that symbol. The deployment policy is
therefore fixed: **install only musl-static artifacts**.

```
# ---- dev container (has a Rust toolchain) -------------------------
cd /workspace/Ax/bifrost
cargo build --release --target x86_64-unknown-linux-musl --bin bifrost
cp target/x86_64-unknown-linux-musl/release/bifrost dist/bifrost-linux-x86_64

# ---- host (root): install + restart in one command, auto-rollback on failure ----
# setsid is required: the daemon kills task process groups when it exits, and without
# detaching it would kill the deploy script itself
bifrost client submit --storage /data/liuxin/bifrost \
  --command 'setsid nohup bash /data/liuxin/Ax/bifrost/scripts/deploy/deploy-h200.sh >/dev/null 2>&1 &'
```

`scripts/deploy/deploy-h200.sh` has three gates, all evaluated **before touching `/usr/local/bin`**; any
failure exits immediately and the running daemon is untouched:

1. `ldd` must report `statically linked` (blocks gnu dynamic artifacts; `--allow-dynamic` bypasses it)
2. the candidate's `--version` exits 0 **and its output carries no glibc version warning** (`weak version` counts)
3. the candidate itself runs `warmup --storage <dir>` and exits 0 — which also validates "can this
   environment go live": storage writability, filesystem type (cache-coherent types warn that inotify is
   unreliable), a clock that does not spin, `nvidia-smi` GPU count, and whether `gpu_pool` matches reality

Only after the gates does it back up `.old` → `install` + `mv` atomically → restart (via
`systemctl restart` when a unit exists, which itself blocks until `READY=1`) → wait for a heartbeat
newer than this start (the fallback for `Type=simple` or no systemd) → run one real task through
`client submit` as a smoke test (passing it is what counts as success, otherwise it rolls back).
Logs: `/tmp/bifrost-deploy.log`, `/tmp/bifrost-daemon.log`.

The GPU pool auto-detects every card on the machine; restrict it with `--gpus 0,1`, disable it with
`--gpus ""` (or `"gpu_pool": []` in settings.json).

```bash
# ---- client side (same machine, or any machine that can see the directory) ----
bifrost client submit --storage /data/liuxin/bifrost --command 'nvidia-smi -L'
bifrost client status --storage /data/liuxin/bifrost <TASK_ID>
```

### Must the shared directory be specified at deploy time?

**Yes.** The default is `~/.bifrost/data` (a machine-local directory), so the client and the daemon
never see the same data and tasks stay Pending forever. Three ways to set it (highest priority first):

1. Command line: `bifrost server --storage /data/liuxin/bifrost` (a global flag, also available to the client)
2. Environment variable: `export BIFROST_CONFIG=/etc/bifrost/settings.json`
3. Config file: `shared_storage` in `~/.bifrost/settings.json` or in the file given to `--config`

Once set, the directories are created automatically: `commands/ results/ status/ artifacts/ logs/ cancel/`,
plus `heartbeat.json` (daemon liveness) and `scan.json` (fallback-scan liveness).

### Directory permissions

The daemon user and the client user must both be able to read and write the same directory. In container
setups the two uids often differ (e.g. uid 10000 versus root); the least effort:

```bash
sudo chown -R <daemon_uid>:<client_gid> /data/liuxin/bifrost
# or
sudo setfacl -R -m u:10000:rwX /data/liuxin/bifrost
sudo setfacl -R -d -m u:10000:rwX /data/liuxin/bifrost
```

### Run it as a systemd service (recommended over a hand-started daemon)

A hand-started `sudo bifrost server` lives in the user login session's scope (measured:
`/user.slice/user-1000.slice/session-1356.scope`): when SSH drops, logind may sweep it away, and there
is no `Restart=` to bring it back. Usage:

```bash
sudo scripts/deploy/install-systemd.sh --storage /data/liuxin/bifrost
#   OK: bifrost.service active (728908), heartbeat fresh
systemctl status bifrost          # active + enabled (starts on boot)
journalctl -u bifrost -f          # logs (including the warmup self-check on every start)
```

The `bifrost.service` in the repo root is a template for the **local private layout**
(`/var/lib/bifrost` + `/etc/bifrost`) and fails to start on a shared-storage layout (neither
WorkingDirectory nor `--config` exists there). Hence `install-systemd.sh` renders one for the actual
`--storage` value. A few deliberate choices in that unit:

- `Type=notify` + `NotifyAccess=main` + `TimeoutStartSec=60`, with `--systemd` in `ExecStart`:
  the daemon reports `READY=1` + `STATUS=...` when it is **truly ready** (directories created, clock
  probed, scan loop running, `Server ready` printed), so `systemctl restart` returning means "it can
  accept tasks now". Measured `restart_ms=563`, and at that instant `heartbeat_age=0s` with `StatusText`
  already reading `watching /data/liuxin/bifrost/commands (max_concurrent=8, ...)`.
  Compare `Type=simple`: returning only means "forked", and the caller has to poll the heartbeat to guess
  readiness. A hung start is caught by `TimeoutStartSec=60`; on stop the daemon sends `STOPPING=1`, so
  systemd does not record it as failed.
- `KillSignal=SIGTERM` + `KillMode=mixed`: SIGTERM goes only to the main process so the daemon can clean
  up every task's process group first; only on timeout is the whole group SIGKILLed (the default
  control-group mode would hit the tasks first).
- `Restart=on-failure` + `RestartSec=5`: measured — after `kill -9` of the main process systemd brings it
  back within 5s and `NRestarts` increments.
- `TasksMax=4096` and **no** `MemoryMax`: on a 2TB / 192-core machine the template's `MemoryMax=8G` would
  OOM-kill concurrent benchmarks.
- `ProtectSystem` / `NoNewPrivileges` / `PrivateTmp` are **not** enabled: this machine's whole purpose is
  running arbitrary commands as root; hardening only produces "the script that worked yesterday gets
  permission denied today".

With a unit in place, `deploy-h200.sh` restarts through `systemctl restart` instead of pkill+setsid. Note
that it first moves itself into a transient unit with `systemd-run`: otherwise the script runs as a task
inside the daemon's cgroup and `systemctl restart` kills the deploy script along with it — setsid
survives the daemon's process-group cleanup but not a cgroup kill (hit this in practice: even a
log-writing helper died).

Side effect: the deploy task itself may produce **no result file** (the daemon is restarted before the
result is written) — trust `/tmp/bifrost-deploy.log` and `journalctl -u bifrost-deploy-*` instead.

### Stop / restart

```bash
# With systemd: prefer systemctl (it honours the KillSignal / KillMode above)
systemctl restart bifrost         # to swap binaries use scripts/deploy/deploy-h200.sh (validation + rollback)
# Without systemd: Ctrl+C once for a graceful exit (<1s), again to force; or kill -TERM <pid> in the background
```

On exit it kills every running task's process group first, so a benchmark cannot survive as an orphan
still holding the GPUs.

### Runtime self-healing

```bash
# Optional: external watchdog (cron, every 2 minutes) restarts the daemon when the heartbeat
# or the scan beacon goes stale
*/2 * * * * /data/liuxin/Ax/bifrost/scripts/ops/watchdog.sh \
    --storage /data/liuxin/bifrost --restart 'systemctl restart bifrost' \
    >> /var/log/bifrost-watchdog.log 2>&1
```

With a systemd unit installed, `Restart=on-failure` is already the first layer of self-healing; the
external watchdog above is only a fallback for **systems without systemd** (or to additionally cover
"process alive but wedged"). Running both is harmless — just remember the watchdog performs the same
`systemctl restart`.
