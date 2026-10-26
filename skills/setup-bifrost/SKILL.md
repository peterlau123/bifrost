---
name: setup-bifrost
description: "Use when installing or repairing bifrost MCP on a machine. Downloads the static binary, registers it with Hermes, verifies tools."
version: 1.0.0
author: peterlau
license: MIT
platforms: [linux]
metadata:
  hermes:
    tags: [bifrost, mcp, install, offline-node, gpu]
---

# 安装 bifrost MCP

bifrost 是 H200/H100 离线节点上的命令执行框架。它的 MCP 侧是 **stdio 服务器**（`bifrost mcp-serve`），
通过共享存储与 daemon 通信，**不需要网络、不需要 Rust 工具链、不需要在目标机重编译**。

发行包是 `x86_64` **静态 musl** 二进制（约 7 MB，不依赖系统 glibc）。

## 什么时候用这个 skill

- 用户说"在这台机器装 bifrost / 接上 H200 / 让我能远程跑命令"
- `hermes mcp list` 里没有 `bifrost`，或 `hermes mcp test bifrost` 失败
- 迁移到新机器后要恢复 bifrost 工具
- 不要用它来**开发** bifrost —— 那是另一个仓库的工作

## 步骤

### 1. 先探测，别急着装

```bash
command -v bifrost && bifrost --version
hermes mcp list | grep -i bifrost
ls -l ~/.bifrost/settings.json 2>/dev/null && cat ~/.bifrost/settings.json
```

三种情况分开处理：

| 现状 | 做法 |
|---|---|
| 二进制在、MCP 已注册但连不上 | 跳到第 4 步排障（多半是共享盘路径错） |
| 二进制在、MCP 没注册 | 只跑 `hermes mcp add`（第 3 步 B） |
| 都没有 | 走完整安装（第 2、3 步） |

### 2. 取包并安装

优先用发布包（**必须校验 sha256**，不要裸 `curl | bash`）：

```bash
cd /tmp
PKG=bifrost-mcp-0.1.0-x86_64-linux-musl.tar.gz
BASE=https://gitee.com/peterlau/bifrost/releases/download/v0.1.0     # GitHub 不通时用 Gitee
curl -fsSLO "$BASE/$PKG" && curl -fsSLO "$BASE/$PKG.sha256"
sha256sum -c "$PKG.sha256"

tar xzf "$PKG"
./bifrost-mcp-0.1.0-x86_64-linux-musl/install.sh --storage <共享盘路径>
```

机器上已有包（共享盘/离线介质）就直接用本地路径，省掉下载：

```bash
./install.sh --storage /data/liuxin/bifrost
PREFIX=$HOME/.local/bin ./install.sh --storage /data/liuxin/bifrost    # 无 root
```

`install.sh` 会：装二进制 → 写 `~/.bifrost/settings.json`（已存在则**不覆盖**，要改加 `--force`）→
跑一次 stdio 握手自检 → `hermes mcp add` 并发现工具。

### 3. 共享盘路径必须对

这是最常见的失败原因。`shared_storage` 要填**本机看到的**那个挂载点，不是别人机器上的路径：

| 机器 | 填什么 |
|---|---|
| H200 宿主 | `/data/liuxin/bifrost` |
| 容器内（映射 /workspace） | `/workspace/bifrost` |
| 其它挂 NFS 的机器 | 各自的挂载点 |

查一下再填：

```bash
ls -d /data/liuxin/bifrost /workspace/bifrost 2>/dev/null
cat <storage>/heartbeat.json        # 有新鲜心跳 = daemon 活着
```

手工注册版（install.sh 已经做了，仅供参考）：

```bash
hermes mcp add bifrost \
  --env BIFROST_CONFIG=$HOME/.bifrost/settings.json \
  --command /usr/local/bin/bifrost --args mcp-serve
```

### 4. 验证（必须做，别只看 list）

```bash
hermes mcp test bifrost                        # 连接耗时 + 工具清单（应有 5 个）
hermes mcp list | grep bifrost                 # ✓ enabled
bifrost client submit --storage <storage> --command 'nvidia-smi -L' --timeout 60
```

工具在当前会话里叫 `mcp_bifrost_*`；已开着的会话要 `/reload-mcp` 或新开会话。

### 5. 排障对照

| 现象 | 原因 | 处理 |
|---|---|---|
| `mcp test` 500ms 内失败，报 command not found | 路径写死或用相对路径 | 用绝对路径重注册，`hermes mcp remove bifrost` 后重加 |
| 连上但 `bifrost_submit` 一直 Pending / 超时 | 共享盘路径错 / daemon 挂了 | 查 `<storage>/heartbeat.json` 的 mtime；在宿主 `systemctl status bifrost` |
| `Permission denied` 写 `commands/` | 共享目录属主是 root，client 是普通用户 | `chmod -R a+rwX <storage>` 或 `chown` 给运行用户 |
| 二进制跑不起来（`cannot execute`） | 机器不是 x86_64（如 GB200 aarch64） | 需要 aarch64 包，**不要**硬装 x86_64 |
| 工具列表里没有 `submit` | 连接超时太短 | 重注册时加 `--connect-timeout 30` |

## 不要做的事

- **不要在目标机 `cargo build`** —— 发行包就是为免编译准备的；除非用户明确要求改代码并验证
- **不要覆盖已有的 `settings.json`** —— 里面可能是另一套集群的路径；用 `--force` 前先备份
- **不要把 GitHub 当唯一源** —— 这台容器的 github.com 直连不通，用 Gitee 或共享盘
- **不要动 `/etc/bifrost/settings.json`**（daemon 侧配置），那是 server 的，与 MCP 客户端无关
