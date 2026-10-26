---
name: setup-bifrost
description: "Use when making bifrost MCP available on this machine. Puts the shipped binary on PATH and verifies the shared-storage config."
version: 1.0.0
author: peterlau
license: MIT
platforms: [linux]
metadata:
  hermes:
    tags: [bifrost, mcp, install, offline-node]
---

# 让 bifrost MCP 可用（distribution 版）

这个 profile 已经把 `mcp_servers.bifrost` 配好了（`command: bifrost`，走 PATH）。
**唯一缺的是二进制**：它就在本 skill 目录里，`scripts/bifrost`（静态 musl，约 7 MB，不依赖 glibc）。

## 步骤

### 1. 找到 skill 目录

```bash
SD="${HERMES_HOME:-$HOME/.hermes}/profiles/bifrost-agent/skills/setup-bifrost"
ls -l "$SD/scripts/bifrost"
```

如果 profile 名不同，用 `hermes profile list` 查实际路径。

### 2. 装到 PATH（不需要 root）

```bash
mkdir -p ~/.local/bin
install -m 0755 "$SD/scripts/bifrost" ~/.local/bin/bifrost
~/.local/bin/bifrost --version          # 应打印 bifrost 0.1.0
case ":$PATH:" in *":$HOME/.local/bin:"*) ;; *) echo '需要把 ~/.local/bin 加进 PATH';; esac
```

想装到系统目录就用 `/usr/local/bin`（需要 root）。

### 3. 配置共享存储

```bash
ls -l ~/.bifrost/settings.json 2>/dev/null || mkdir -p ~/.bifrost
```

没有就建一份，`shared_storage` 填**本机看到的**挂载点：

```bash
cat > ~/.bifrost/settings.json <<'JSON'
{
  "shared_storage": "/data/liuxin/bifrost",
  "transport": "shared",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "3m", "wait_timeout": "12h" },
  "daemon": { "scan_mode": "auto" }
}
JSON
```

| 机器 | `shared_storage` |
|---|---|
| H200 宿主 | `/data/liuxin/bifrost` |
| 容器内（映射 /workspace） | `/workspace/bifrost` |
| 其它挂 NFS 的机器 | 各自的挂载点 |

已有 `settings.json` **不要覆盖**，里面可能指向另一套集群；要改就手工编辑。

### 4. 验证

```bash
hermes mcp test bifrost        # 应发现 5 个工具
bifrost client submit --storage <storage> --command 'nvidia-smi -L' --timeout 60
```

`mcp_servers` 里的 `BIFROST_CONFIG` 为空时会回落到 `$HOME/.bifrost/settings.json`。
若填了别的路径，确保它存在，否则连接会失败。

### 5. 排障

| 现象 | 原因 | 处理 |
|---|---|---|
| `mcp test` 报 command not found | `bifrost` 不在 MCP 子进程的 PATH 里 | 装到 `/usr/local/bin`，或把 `~/.local/bin` 加进 PATH 后重启会话 |
| 连上但 `bifrost_submit` 一直 Pending | 共享盘路径错 / daemon 没跑 | 查 `<storage>/heartbeat.json` 的 mtime；宿主 `systemctl status bifrost` |
| 写 `commands/` 报 Permission denied | 共享目录属主是 root | `chmod -R a+rwX <storage>` |
| `cannot execute` | 机器不是 x86_64 | 需要 aarch64 构建，别硬装 |

## 不要做的事

- 不要在目标机 `cargo build` —— 二进制就在包里
- 不要覆盖别人的 `~/.bifrost/settings.json`
- 不要改 `/etc/bifrost/settings.json`（那是 daemon 侧，不关客户端的事）
