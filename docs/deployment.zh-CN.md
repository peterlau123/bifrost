# 部署指南

[English](deployment.md) | **简体中文**

> **本文档覆盖两套部署模型。**
>
> - **共享盘部署**（H200 / NFS / GPFS）—— 当前生产实际在用的方式，位于文末：容器内构建 →
>   宿主安装、`install-systemd.sh`、`deploy-h200.sh`，以及各项实测数据。
> - **本地私有布局**（`/var/lib/bifrost` + `/etc/bifrost`，仓库根目录那份 unit）—— 这份文档早期的布局，
>   现在已没有脚本会装它（请用 `scripts/deploy/deploy-server.sh`）。前半部分还有若干内容当前版本**并未实现**：
>   settings.json 里的 Prometheus `metrics:` 配置段，以及 `/usr/local/bin/bifrost-health-check.sh`
>   （仓库里是 `scripts/validate/health-check.sh`，且没有脚本会安装它）。请把它们当模板看，别当既有功能。

本指南覆盖 Bifrost 的生产部署（systemd）。

## 概述

Bifrost 面向生产部署，具备：
- systemd 服务集成
- 健康监控
- 日志管理
- 资源限制
- 安全加固

## 前置条件

- 带 systemd 的 Linux 系统（Ubuntu、Debian、CentOS、RHEL 等）
- 安装需要 root 权限
- 编译需要 Rust 工具链（可选，也有预编译二进制）
- 建议 2GB 以上内存
- 建议 10GB 以上磁盘空间用于日志

## 快速部署

### 自动化安装

```bash
# 一条命令装完（需要 root）
sudo scripts/deploy/deploy-server.sh --storage /data/liuxin/bifrost
```

这个脚本会：
1. 对候选二进制做门槛校验（静态链接 → `--version` → `warmup`）
2. 用 `bifrost server init` 生成 server 侧配置
3. 安装到 `/usr/local/bin/bifrost`（先备份 `.old`）
4. 渲染并重启 systemd unit（`scripts/deploy/install-systemd.sh`）
5. 等心跳刷新并真跑一条任务做 smoke，失败自动回滚

可选参数：`--binary --config --prefix --service-name --gpus --no-systemd --skip-build --skip-warmup --skip-smoke --force`。
下面的手工步骤保留作参考，方便理解脚本到底做了什么。

### 手动安装

逐步手动部署：

```bash
# 1. 编译二进制
cargo build --release

# 2. 安装二进制
sudo cp target/release/bifrost /usr/local/bin/
sudo chmod +x /usr/local/bin/bifrost

# 3. 建立目录
sudo mkdir -p /var/lib/bifrost/{pending,results,completed,logs}

# 4. 生成配置
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

# 4.1 上线前自检：每次部署都跑，与文件系统无关（GPFS/NFS/本地目录一样）；
#     必须在 server 端跑，有阻断项时退出码 1
sudo bifrost warmup --config /etc/bifrost/settings.json


# 5. 安装 systemd 服务
sudo cp bifrost.service /etc/systemd/system/
sudo systemctl daemon-reload

# 6. 设为开机自启并启动
sudo systemctl enable bifrost
sudo systemctl start bifrost
```

## 配置

### 服务端配置

编辑 `~/.bifrost/settings.json`：

```json
{
  "shared_storage": "/var/lib/bifrost",
  "daemon": {
    "max_concurrent": 4,
    "task_timeout": "3600s"
  }
}
```

### systemd 服务配置

编辑 `/etc/systemd/system/bifrost.service`：

```ini
[Unit]
Description=Bifrost Server
After=network.target

[Service]
Type=simple
ExecStart=/usr/local/bin/bifrost server
Restart=on-failure
RestartSec=10

# 资源限制
MemoryMax=2G
TasksMax=100

[Install]
WantedBy=multi-user.target
```

改完执行：

```bash
sudo systemctl daemon-reload
sudo systemctl restart bifrost
```

## 服务管理

### 启动服务

```bash
sudo systemctl start bifrost
```

### 停止服务

```bash
sudo systemctl stop bifrost
```

### 重启服务

```bash
sudo systemctl restart bifrost
```

### 查看状态

```bash
sudo systemctl status bifrost
```

输出包含：
- 服务状态（running/stopped）
- 最近的日志
- 进程 ID
- 内存占用

### 查看日志

```bash
# 实时日志
journalctl -u bifrost -f

# 最近一小时
journalctl -u bifrost --since "1 hour ago"

# 指定起始时间
journalctl -u bifrost --since "2026-07-04 10:00:00"
```

## 健康监控

### 健康检查脚本

```bash
# 完整检查
/usr/local/bin/bifrost-health-check.sh full

# 心跳检查
/usr/local/bin/bifrost-health-check.sh heartbeat

# 服务检查
/usr/local/bin/bifrost-health-check.sh startup
```

### 接入监控系统

#### Cron 定时监控

加到 cron：

```bash
# 每 5 分钟检查一次，失败就重启
*/5 * * * * /usr/local/bin/bifrost-health-check.sh full || systemctl restart bifrost
```

#### Prometheus 集成

暴露指标：

```yaml
# 加到 settings.json
metrics:
  enabled: true
  port: 9090
  path: /metrics
```

指标地址：`http://localhost:9090/metrics`

#### Nagios 集成

创建检查脚本：

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

## 资源管理

### 内存限制

在 systemd 服务里设置：

```ini
MemoryMax=2G
MemoryHigh=1G
MemoryMin=512M
```

### CPU 限制

```ini
CPUQuota=80%
CPUShares=1024
```

### 磁盘管理

清理旧日志：

```bash
# 每天清理（加到 cron）
0 2 * * * find /var/lib/bifrost/logs -mtime +30 -delete
```

或用 systemd tmpfiles：

```bash
# /etc/tmpfiles.d/bifrost.conf
d /var/lib/bifrost/logs 0755 root root 30d
```

## 安全加固

### systemd 安全选项

加到服务文件：

```ini
# 安全加固
NoNewPrivileges=true
PrivateTmp=yes
ProtectSystem=strict
ProtectHome=yes
ReadWritePaths=/var/lib/bifrost

# 网络隔离（不需要网络时）
PrivateNetwork=yes

# 用户命名空间隔离
PrivateUsers=yes

# 限制系统调用
SystemCallFilter=@basic @system-service
```

### 文件权限

```bash
# 设置属主
sudo chown -R root:root /var/lib/bifrost
sudo chmod -R 750 /var/lib/bifrost

# 配置文件
sudo chmod 640 ~/.bifrost/settings.json
```

### SELinux / AppArmor

SELinux 系统：

```bash
# 创建策略
sudo semanage fcontext -a -t bifrost_data_t "/var/lib/bifrost(/.*)?"
sudo restorecon -R /var/lib/bifrost
```

AppArmor：

```bash
# 创建 profile
sudo aa-genprof /usr/local/bin/bifrost
```

## 备份与恢复

### 备份配置

```bash
# 备份配置
sudo tar -czf bifrost-config-backup.tar.gz ~/.bifrost/

# 备份数据
sudo tar -czf bifrost-data-backup.tar.gz /var/lib/bifrost/
```

### 恢复流程

```bash
# 停服务
sudo systemctl stop bifrost

# 恢复配置
sudo tar -xzf bifrost-config-backup.tar.gz -C /

# 恢复数据
sudo tar -xzf bifrost-data-backup.tar.gz -C /

# 启动服务
sudo systemctl start bifrost
```

## 升级

### 升级流程

```bash
# 1. 停服务
sudo systemctl stop bifrost

# 2. 备份
sudo cp /usr/local/bin/bifrost /usr/local/bin/bifrost.bak

# 3. 编译新版本
cargo build --release

# 4. 安装新二进制
sudo cp target/release/bifrost /usr/local/bin/

# 5. 启动服务
sudo systemctl start bifrost

# 6. 验证
sudo systemctl status bifrost
bifrost-health-check.sh full
```

### 回滚

```bash
# 升级失败时
sudo systemctl stop bifrost
sudo cp /usr/local/bin/bifrost.bak /usr/local/bin/bifrost
sudo systemctl start bifrost
```

## 多实例部署

需要多个 daemon 实例时：

```bash
# 实例 1
sudo cp bifrost.service /etc/systemd/system/bifrost-1.service
sudo mkdir -p /var/lib/bifrost-1

# 实例 2
sudo cp bifrost.service /etc/systemd/system/bifrost-2.service
sudo mkdir -p /var/lib/bifrost-2

# 逐个配置
bifrost server --init
# 编辑 ~/.bifrost/settings.json，把 shared_storage 指向 /var/lib/bifrost-1

# 第二个 daemon：
bifrost server --init
# 编辑 ~/.bifrost/settings.json，把 shared_storage 指向 /var/lib/bifrost-2

# 启动实例
sudo systemctl start bifrost-1
sudo systemctl start bifrost-2
```

## 排障

### 服务起不来

看日志：

```bash
journalctl -u bifrost -n 50
```

常见原因：
- 缺配置：创建 `~/.bifrost/settings.json`
- 权限不足：检查文件权限
- 端口冲突：bifrost 不需要网络端口
- 资源限制：调大 MemoryMax

### 服务崩溃

检查：

```bash
# 系统日志
journalctl -u bifrost --since "10 minutes ago"

# 应用日志
ls -lh /var/lib/bifrost/logs/

# 磁盘空间
df -h /var/lib/bifrost
```

### 任务不执行

检查：

```bash
# 待处理任务
ls /var/lib/bifrost/pending/

# Daemon 日志
journalctl -u bifrost | grep "Watcher"

# 配置
cat ~/.bifrost/settings.json
```

### 健康检查失败

```bash
# 查心跳
cat /var/lib/bifrost/logs/heartbeat.json

# 查服务
systemctl status bifrost

# 手动检查
bifrost-health-check.sh heartbeat
```

## 最佳实践

1. **资源限制**：始终设置 MemoryMax 与 CPUQuota
2. **日志轮转**：配置自动清理
3. **健康监控**：接入自动化健康检查
4. **备份**：定期备份配置与数据
5. **安全**：启用 systemd 加固选项
6. **测试**：升级先在预发环境验证
7. **文档**：把自定义配置记录下来

## 另见

- [README.md](../README.zh-CN.md) —— 项目总览
- [Adapter 指南](adapter-guide.md) —— 任务 adapter

## 共享盘部署（H200 / 非 GPFS）

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
#   scripts/build/find-binary.sh release   # 让 cargo 报告真实路径
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
  --command 'setsid nohup bash /data/liuxin/Ax/bifrost/scripts/deploy/deploy-h200.sh >/dev/null 2>&1 &'
```

`scripts/deploy/deploy-h200.sh` 的三道门槛，全部在**碰 `/usr/local/bin` 之前**跑，任一道
不过就直接退出、旧 daemon 不受影响：

1. `ldd` 必须显示 `statically linked`（拦住 gnu 动态产物；`--allow-dynamic` 可放行）
2. 候选二进制 `--version` 退出 0 **且输出里没有 glibc 版本告警**（`weak version` 也算）
3. 候选二进制 `warmup --storage <dir>` 退出 0 —— 顺带把"环境能不能上线"一起验了：
   存储可写、文件系统类型（缓存型会提示 inotify 不可靠）、时钟不自旋、
   `nvidia-smi` 可见卡数、`gpu_pool` 与实际卡数是否一致

过了门槛才备份 `.old` → `install` + `mv` 原子替换 → 重启（有 unit 走
`systemctl restart`，它自己会阻塞到 `READY=1`）→ 等心跳比本次启动更新（`Type=simple`
或无 systemd 时的兜底）→ 用 `client submit` 真跑一条任务做 smoke（过了才算部署成功，
否则自动回滚）。
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

### 装成 systemd 服务（推荐，别用手起的 daemon）

手起的 `sudo bifrost server` 挂在用户 login session 的 scope 里（实测
`/user.slice/user-1000.slice/session-1356.scope`）：SSH 一断，logind 可能顺手清掉它，
而且没有 `Restart=` 兜底。用例：

```bash
sudo scripts/deploy/install-systemd.sh --storage /data/liuxin/bifrost
#   OK: bifrost.service active (728908), heartbeat fresh
systemctl status bifrost          # active + enabled（开机自启）
journalctl -u bifrost -f          # 日志（含每次启动的 warmup 自检）
```

仓库根的 `bifrost.service` 是**本地私有布局**（`/var/lib/bifrost` + `/etc/bifrost`）的
模板，直接套共享盘布局会启动失败（WorkingDirectory 和 `--config` 都不存在）。所以用
`install-systemd.sh` 按 `--storage` 现场渲染，unit 里几个刻意的选择：

- `Type=notify` + `NotifyAccess=main` + `TimeoutStartSec=60`，`ExecStart` 带 `--systemd`：
  daemon 在**真正就绪**时（目录建好、时钟探过、扫描循环起来、`Server ready` 打完）
  发 `READY=1` + `STATUS=...`，所以 `systemctl restart` 返回 = 已经能收任务。
  实测 `restart_ms=563`，返回那一刻 `heartbeat_age=0s`、`StatusText` 已是
  `watching /data/liuxin/bifrost/commands (max_concurrent=8, ...)`。
  对比 `Type=simple`：返回只代表 fork 成功，调用方得自己轮询心跳猜就绪。
  启动卡死由 `TimeoutStartSec=60` 兜住；停止时 daemon 发 `STOPPING=1`，
  systemd 不会把它记成 failed。
- `KillSignal=SIGTERM` + `KillMode=mixed`：只给主进程发 SIGTERM，让 daemon 自己先杀
  干净所有任务的进程组，超时才整组 SIGKILL（默认 control-group 会先打任务）。
- `Restart=on-failure` + `RestartSec=5`：实测 `kill -9` 主进程后 5s 由 systemd 拉起来，
  `NRestarts` 递增。
- `TasksMax=4096`、**不设** `MemoryMax`：2TB 内存 / 192 核的机器，模板里的
  `MemoryMax=8G` 会把并发 benchmark 打成 OOM。
- **不启用** `ProtectSystem` / `NoNewPrivileges` / `PrivateTmp`：这台机器的用途就是以
  root 跑任意命令，硬化只会制造"昨天能跑的脚本今天 permission denied"。

有 unit 之后 `deploy-h200.sh` 会走 `systemctl restart`（而不是 pkill+setsid）。注意它
会先用 `systemd-run` 把自己搬进一个 transient unit：否则脚本作为任务跑在 daemon 的
cgroup 里，`systemctl restart` 会把部署脚本自己一起杀掉 —— setsid 挡得住 daemon 的
进程组清理，挡不住 cgroup kill（这个坑实测撞过：连写日志的 helper 都没活下来）。

副作用：部署任务本身可能**没有结果文件**（写结果前 daemon 已被重启），以
`/tmp/bifrost-deploy.log` 和 `journalctl -u bifrost-deploy-*` 为准。

### 停止 / 重启

```bash
# 有 systemd：优先用 systemctl（会走上面的 KillSignal / KillMode）
systemctl restart bifrost         # 换二进制请用 scripts/deploy/deploy-h200.sh（含校验与回滚）
# 无 systemd：前台 Ctrl+C 一次优雅退出（<1s），再按一次强制退出；后台 kill -TERM <pid>
```

退出时会先杀掉所有在跑任务的进程组，避免 benchmark 变成孤儿进程继续占用 GPU。

### 运行期自愈

```bash
# 可选：外部看门狗（cron 每 2 分钟），心跳或扫描信标过期就重启 daemon
*/2 * * * * /data/liuxin/Ax/bifrost/scripts/ops/watchdog.sh \
    --storage /data/liuxin/bifrost --restart 'systemctl restart bifrost' \
    >> /var/log/bifrost-watchdog.log 2>&1
```

装了 systemd unit 之后 `Restart=on-failure` 已经是第一层自愈，下面的外部看门狗只作为
**没有 systemd 时**的兜底（或想额外覆盖"进程活着但卡死"的场景）；两者同时开不会打架，
但要记得看门狗也是 `systemctl restart` 同一套动作。
