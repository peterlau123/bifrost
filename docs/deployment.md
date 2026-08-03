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
