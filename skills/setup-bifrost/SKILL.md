---
name: setup-bifrost
description: "Use when installing, repairing, or registering bifrost on an offline/air-gapped Linux machine whose only external channel is shared storage. Automatically installs and registers the client-side MCP server; for the daemon side, presents the exact commands for the user to run."
version: 1.1.0
author: peterlau
license: MIT
platforms: [linux]
metadata:
  hermes:
    tags: [bifrost, mcp, install, offline, air-gapped, shared-storage]
---

# 安装 bifrost MCP

bifrost 是面向**离线 / 气隙机器**的命令执行框架：只要两台机器之间有一块可读写的共享存储
（GPFS / NFS / Lustre / 本地挂载目录均可），就可以把任务从联网侧送到离线侧执行。不要求特定
服务器型号，也不要求 GPU——GPU 只是一个可选能力。

MCP 侧是 **stdio 服务器**（`bifrost mcp-serve`），通过共享存储与 daemon 通信，**不需要网络、
不需要 Rust 工具链、不需要在目标机重编译**。发行包是 **Linux x86_64 静态 musl** 二进制
（约 7 MB，不依赖系统 glibc）。

## 角色与执行约定

先判断这次要装的是哪一侧：

| 角色 | 位置 | 本 skill 的行为 |
|---|---|---|
| **client / MCP 侧** | 当前这台可联网或可拿到发行包的机器 | **自动完成**：下载/解压、校验、安装到用户目录、写配置、注册 MCP、握手验证 |
| **server / daemon 侧** | 真正执行命令的离线机器 | **只给出命令**，由用户确认并执行 sudo；不要在用户未确认时改系统目录或 systemd |

如果用户只说“装 bifrost”而没有说明角色，先问一句：
**“这台机器是提交任务的客户端，还是执行任务的离线 daemon？”** 同时确认共享存储路径。

- client 侧：在拿到共享存储路径后直接执行第 2–4 步，不需要逐步征求确认。
- server 侧：输出第 5 步的命令和检查项，等用户执行完再帮忙验证。
- 不确定时按 client 处理（不会改系统文件，风险最低）。

## 1. 先探测，别急着装

```bash
uname -m                                   # 必须是 x86_64
command -v bifrost && bifrost --version
hermes mcp list 2>/dev/null | grep -i bifrost
ls -l ~/.bifrost/settings.json 2>/dev/null && cat ~/.bifrost/settings.json
```

| 现状 | 做法 |
|---|---|
| 二进制在、MCP 已注册但连不上 | 跳到第 6 步排障（多半是共享盘路径错） |
| 二进制在、MCP 没注册 | 跳到第 3 步注册 |
| 都没有 | 走第 2 步完整安装 |

如果 `uname -m` 不是 `x86_64`，停止安装：当前发行包只有 x86_64，**不要**硬装。告诉用户需要
对应架构的构建产物。

## 2. 自动安装 client 侧

### 2.1 找到共享存储路径

`shared_storage` 必须是**本机看到的挂载点**，不是别人机器上的路径。如果用户没给，先问；也可以
从现有配置或挂载表里找线索：

```bash
df -h | grep -Ei 'gpfs|nfs|lustre|/data|/workspace'
```

典型形态（仅作示例，实际以用户环境为准）：

| 场景 | 可能的路径 |
|---|---|
| 宿主机直接挂共享盘 | `/data/<team>/bifrost` |
| 容器内映射 | `/workspace/bifrost` |
| NFS / Lustre 挂载 | `/mnt/<cluster>/bifrost` |

确认目录可写，并看看 daemon 是否已经在写心跳：

```bash
test -w <storage> && echo "writable"
cat <storage>/heartbeat.json 2>/dev/null
```

### 2.2 取包、校验、安装（无需 root）

优先用共享盘/离线介质里已有的包；没有再从 GitHub Release 下载。**必须校验 sha256**：

```bash
cd /tmp
PKG=bifrost-latest-x86_64-linux-musl.tar.gz
BASE=https://github.com/peterlau123/bifrost/releases/download/latest
curl -fsSLO "$BASE/$PKG" && curl -fsSLO "$BASE/$PKG.sha256"
sha256sum -c "$PKG.sha256"
tar xzf "$PKG"
```

安装到用户目录（不污染系统、不需要 sudo）：

```bash
PREFIX="${PREFIX:-$HOME/.local/bin}"
./bifrost-latest-x86_64-linux-musl/install.sh \
  --role client \
  --prefix "$PREFIX" \
  --storage <storage>
```

`install.sh` 会：装二进制 → 写 `~/.bifrost/settings.json`（已存在则**不覆盖**，要改加 `--force`）→
跑一次 stdio 握手自检 → 若检测到 `hermes`，自动执行 `hermes mcp add`。

如果用户希望系统级安装（Debian/Ubuntu，需要 sudo），改用发布页的 `.deb`：

```bash
sudo dpkg -i bifrost_<版本>-1_amd64.deb
bifrost-setup client --storage <storage>     # 这一步不需要 sudo
```

## 3. 注册并“启动” MCP server

MCP server 是 stdio 进程，由 Agent 在需要时拉起，**不需要常驻后台**。把注册做对，就等于启动。

Hermes（`install.sh` 通常已经做了，这里确认一次）：

```bash
# 已注册就跳过；需要重注册时先 hermes mcp remove bifrost
hermes mcp list 2>/dev/null | grep -q 'bifrost' || \
hermes mcp add bifrost \
  --connect-timeout 30 \
  --env BIFROST_CONFIG=$HOME/.bifrost/settings.json \
  --command "$PREFIX/bifrost" \
  --args mcp-serve
```

其它 Agent 用同一组参数：`command=<绝对路径>/bifrost`、`args=["mcp-serve"]`、
`env.BIFROST_CONFIG=~/.bifrost/settings.json`。

OpenCode（`opencode.json`）：

```json
{
  "mcp": {
    "bifrost": {
      "type": "stdio",
      "command": "/home/<user>/.local/bin/bifrost",
      "args": ["mcp-serve"],
      "env": { "BIFROST_CONFIG": "/home/<user>/.bifrost/settings.json" }
    }
  }
}
```

Claude Code：

```bash
claude mcp add bifrost \
  --env BIFROST_CONFIG=$HOME/.bifrost/settings.json \
  -- "$PREFIX/bifrost" mcp-serve
```

Cline / Cursor / 通用 Agent（`mcpServers`）：

```json
{
  "mcpServers": {
    "bifrost": {
      "command": "/home/<user>/.local/bin/bifrost",
      "args": ["mcp-serve"],
      "env": { "BIFROST_CONFIG": "/home/<user>/.bifrost/settings.json" }
    }
  }
}
```

> 已开着的会话需要 `/reload-mcp` 或新开会话；工具名通常是 `mcp_bifrost_*`。

## 4. 验证（必须做，别只看 list）

```bash
hermes mcp test bifrost                        # 连接耗时 + 工具清单（应有 5 个）
hermes mcp list | grep bifrost                 # ✓ enabled

# 与 Agent 无关的握手自检
echo '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"setup","version":"1"}}}' \
  | "$PREFIX/bifrost" mcp-serve | head -3

# 如果 daemon 已经在线，端到端跑一条
"$PREFIX/bifrost" client submit --storage <storage> --command 'hostname' --timeout 60
```

`mcp test` 通过、工具数正确、`bifrost_health` 返回 `alive: true`，即 client 侧完成。

## 5. server / daemon 侧：交给用户执行

不要自动执行这些命令。把下面与用户环境匹配的一组打印出来，并说明需要 sudo、需要共享存储路径：

### 推荐：deb 安装（Debian / Ubuntu）

```bash
sudo dpkg -i bifrost_<版本>-1_amd64.deb
sudo bifrost-setup server --storage <storage>
systemctl status bifrost
journalctl -u bifrost -f
```

### 便携包 / 其它发行版

```bash
sudo ./install.sh --role server --storage <storage>
```

### 手工方式（已有二进制）

```bash
sudo cp bifrost /usr/local/bin/bifrost
bifrost warmup --storage <storage>                    # 上线前自检
sudo bifrost server --config /etc/bifrost/settings.json   # 或交给 systemd
```

### 安装后检查

```bash
ls -l <storage>/heartbeat.json                # 存在且 60s 内更新
cat <storage>/heartbeat.json
bifrost warmup --storage <storage>
```

要点：

- 共享盘属主/权限要让 **daemon 用户**和 **client 用户**都能读写 `commands/ results/ status/ logs/`
- 如果两端看到的是同一块盘的不同挂载点，`shared_storage` 各自填**本机挂载点**即可
- server 侧不需要联网，也不需要 Rust

## 6. 排障对照

| 现象 | 原因 | 处理 |
|---|---|---|
| `mcp test` 很快失败，报 command not found | 注册时用了相对路径或错路径 | 用绝对路径重注册：`hermes mcp remove bifrost` 后重加 |
| 连上但 `bifrost_submit` 一直 Pending | 共享盘路径错 / daemon 挂了 / 两端不是同一块盘 | 查 `<storage>/heartbeat.json` 的 mtime；在 daemon 机器 `systemctl status bifrost`；检查 `scan.json` |
| `Permission denied` 写 `commands/` | 共享目录属主是 root，client 是普通用户 | 让管理员 `chmod -R a+rwX <storage>` 或 `chown` 给运行用户 |
| 二进制跑不起来（`cannot execute`） | 机器不是 x86_64 | 使用对应架构的构建产物，**不要**硬装 x86_64 |
| 工具列表里没有 `submit` | 连接超时太短 | 重注册时加 `--connect-timeout 30` |
| Agent 看不到新工具 | 当前会话没重载 | `/reload-mcp` 或新开会话 |

## 不要做的事

- **不要在目标机 `cargo build`** —— 发行包就是为免编译准备的；除非用户明确要求改代码并验证
- **不要自动执行 server 侧 sudo 安装** —— 打印命令让用户执行；systemd 和 `/etc` 的改动必须由用户确认
- **不要覆盖已有的 `settings.json`** —— 里面可能是另一套集群的路径；用 `--force` 前先备份
- **不要把 GitHub 当唯一源** —— 内网/气隙环境用共享盘或离线介质；Gitee 镜像可作备选
- **不要动 `/etc/bifrost/settings.json`**（daemon 侧配置），那是 server 的，与 MCP 客户端无关
