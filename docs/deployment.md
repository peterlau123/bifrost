# Deployment Guide

This guide covers production deployment of Bifrost using systemd.

## Overview

Bifrost is designed for production deployment with:
- systemd service integration
- Health monitoring
- Log management
- Resource limits
- Security hardening

## Quick Start


## Prerequisites

- Linux system with systemd (Ubuntu, Debian, CentOS, RHEL, etc.)
- Root access for installation
- Rust toolchain for building (optional, pre-built binary available)
- 2GB+ RAM recommended
- 10GB+ disk space for logs

## Quick Deployment

### Automated Installation

```bash
# Build and install (requires root)
sudo ./scripts/systemd-setup.sh
```

This script:
1. Builds release binary
2. Installs to `/usr/local/bin/bifrost`
3. Creates configuration in `/etc/bifrost/`
4. Sets up data directories in `/var/lib/bifrost/`
5. Installs systemd service
6. Starts service

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

# 4.1 自检（必须在 server 端；有阻断项退出码 1）
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

bifrost 只要求两端可读写的 POSIX 目录，GPFS 不是必需项。H200 上用宿主机共享的
`/data` 即可（容器内常挂成 `/workspace`）。

```bash
# ── H200（server 端）──────────────────────────────────────────────
cd /data/liuxin/Ax/bifrost            # 仓库已在共享盘上

# 方式 A：直接用仓库里预编译好的静态二进制（推荐，免工具链）
#   dist/bifrost-linux-x86_64 是 musl 静态链接，glibc 2.35 的宿主实测可跑
sudo cp dist/bifrost-linux-x86_64 /usr/local/bin/bifrost && sudo chmod +x /usr/local/bin/bifrost
# 方式 B：本机编译（宿主没有 Rust 工具链时不可用，见下节）
#   cargo build --release            # gnu 产物 target/release/bifrost，只适合 glibc>=2.39 的目标机
#   scripts/find-binary.sh release   # 让 cargo 报告真实路径
#   ⚠ 目标机 glibc 更旧时，gnu 产物仍可能起来（weak version 只警告），别靠 --version 判断

bifrost warmup --storage /data/liuxin/bifrost
#   ✓ shared storage is writable
#   ✓ filesystem: xfs
#   ✓ clock: ok (sleep-driven scanning is safe)
#   ✓ 8 GPU(s) visible via nvidia-smi
bifrost server --storage /data/liuxin/bifrost
```

### 从开发容器构建 → 在宿主机安装（宿主无 Rust 工具链）

实测 H200 宿主（Ubuntu 22.04，glibc 2.35）**没有** cargo/rustc：`command -v cargo` 空，
`find / -maxdepth 4 -name cargo` 也空。而开发容器（Debian 13，glibc 2.41）里
`cargo build --release` 的 gnu 产物引用 `GLIBC_2.39`。

这个坑比看起来隐蔽：**宿主跑它不会立刻失败**，只打一行
"weak version 'GLIBC_2.39' not found"，然后照常启动、照常执行任务——因为被引用的是
weak 符号，动态链接器只警告不报错。所以「`--version` 返回 0」根本不能证明产物匹配，
要等某天走到真正用到该符号的代码路径才炸。部署政策因此固定为**只装 musl 静态产物**：

```
# ── 开发容器（有 Rust 工具链）────────────────────────────────────
cd /workspace/Ax/bifrost
cargo build --release --target x86_64-unknown-linux-musl --bin bifrost
cp target/x86_64-unknown-linux-musl/release/bifrost dist/bifrost-linux-x86_64

# ── 宿主（root）：安装 + 重启，一条命令，失败自动回滚 ──────────────
# setsid 必须加：daemon 退出时会清理任务进程组，不脱离会把部署脚本自己杀掉
bifrost client submit --storage /data/liuxin/bifrost \
  --command 'setsid nohup bash /data/liuxin/Ax/bifrost/scripts/deploy-h200.sh >/dev/null 2>&1 &'
```

`scripts/deploy-h200.sh` 的三道门槛，全部在**碰 `/usr/local/bin` 之前**跑，任一道
不过就直接退出、旧 daemon 不受影响：

1. `ldd` 必须显示 `statically linked`（拦住 gnu 动态产物；`--allow-dynamic` 可放行）
2. 候选二进制 `--version` 退出 0 **且输出里没有 glibc 版本告警**（`weak version` 也算）
3. 候选二进制 `warmup --storage <dir>` 退出 0 —— 顺带把"环境能不能上线"一起验了：
   存储可写、文件系统类型（缓存型会提示 inotify 不可靠）、时钟不自旋、
   `nvidia-smi` 可见卡数、`gpu_pool` 与实际卡数是否一致

过了门槛才备份 `.old` → `install` + `mv` 原子替换 → SIGTERM 旧 daemon（实测 1s 内退出）
→ `setsid` 起新 daemon → 等心跳比本次启动更新 → 用 `client submit` 真跑一条任务做
smoke（过了才算部署成功，否则自动回滚）。
日志：`/tmp/bifrost-deploy.log`、`/tmp/bifrost-daemon.log`。

GPU 池默认自动探测本机全部卡；要限制范围用 `--gpus 0,1`，要关闭用 `--gpus ""`
（或在 settings.json 写 `"gpu_pool": []`）。

```bash
# ── client 端（同机器或任何能访问该目录的机器）────────────────────
bifrost client submit --storage /data/liuxin/bifrost --command 'nvidia-smi -L'
bifrost client status --storage /data/liuxin/bifrost <TASK_ID>
```

### 部署时必须指定共享目录吗？

**是。** 默认值是 `~/.bifrost/data`（本机私有目录），client 和 daemon 看不到同一份
数据，任务永远 Pending。三种指定方式（优先级从高到低）：

1. 命令行：`bifrost server --storage /data/liuxin/bifrost`（全局参数，client 同样可用）
2. 环境变量：`export BIFROST_CONFIG=/etc/bifrost/settings.json`
3. 配置文件：`~/.bifrost/settings.json` 或 `--config` 指定的文件里的 `shared_storage`

指定后目录会自动创建：`commands/ results/ status/ artifacts/ logs/ cancel/`，
以及 `heartbeat.json`（daemon 存活）和 `scan.json`（兜底扫描存活）。

### 目录权限

daemon 用户与 client 用户都要能读写同一个目录。容器场景下两边 uid 可能不同
（例如 uid 10000 与 root），最省事的做法：

```bash
sudo chown -R <daemon_uid>:<client_gid> /data/liuxin/bifrost
# 或
sudo setfacl -R -m u:10000:rwX /data/liuxin/bifrost
sudo setfacl -R -d -m u:10000:rwX /data/liuxin/bifrost
```

### 停止 / 重启

```bash
# 前台：Ctrl+C 一次优雅退出（<1s），再按一次强制退出
# 后台：kill -TERM <pid> 或 systemctl stop bifrost
# 换二进制重启：scripts/deploy-h200.sh（备份 + 原子替换 + 起不来回滚）
```

退出时会先杀掉所有在跑任务的进程组，避免 benchmark 变成孤儿进程继续占用 GPU。

### 运行期自愈

```bash
# 可选：外部看门狗（cron 每 2 分钟），心跳或扫描信标过期就重启 daemon
*/2 * * * * /data/liuxin/Ax/bifrost/scripts/watchdog.sh \
    --storage /data/liuxin/bifrost --restart 'systemctl restart bifrost' \
    >> /var/log/bifrost-watchdog.log 2>&1
```
