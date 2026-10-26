# bifrost MCP 便携包

把 bifrost 作为 Hermes 的 MCP 工具搬到另一台机器，**不需要 Rust 工具链、不需要重新编译**。
包内是 x86_64 **静态 musl** 二进制（6.8 MB，不依赖 glibc），任何 x86_64 Linux 直接可用。

## 一条命令装好

```bash
tar xzf bifrost-mcp-0.1.0-x86_64-linux-musl.tar.gz
cd bifrost-mcp-0.1.0-x86_64-linux-musl
./install.sh --storage /data/liuxin/bifrost
```

它做三件事：装二进制到 `/usr/local/bin/bifrost`、写 `~/.bifrost/settings.json`、
`hermes mcp add bifrost ...` 注册工具并自动发现。

无 root 权限时：

```bash
PREFIX=$HOME/.local/bin ./install.sh --storage /data/liuxin/bifrost
```

## 手工版（不想用脚本）

```bash
install -m 0755 bifrost /usr/local/bin/bifrost
mkdir -p ~/.bifrost && cp settings.example.json ~/.bifrost/settings.json   # 改 shared_storage
hermes mcp add bifrost --env BIFROST_CONFIG=$HOME/.bifrost/settings.json \
  --command /usr/local/bin/bifrost --args mcp-serve
hermes mcp test bifrost
```

## 关键前提：共享盘

bifrost 的 MCP 侧**只通过共享存储与 daemon 通信**（`mcp-serve` 是 stdio，没有 HTTP 传输）。
所以新机器必须能访问同一个共享目录：

| 机器 | `shared_storage` 应填 |
|---|---|
| H200 宿主 | `/data/liuxin/bifrost` |
| 装了 NFS 的其它机器 | 该机器上的挂载点（如 `/mnt/liuxin/bifrost`） |

daemon 不必在新机器上跑 —— 只要共享盘可见、且 H200 上的 `bifrost.service` 活着，新机器就能提交任务。

## 验证

```bash
hermes mcp list                 # bifrost / stdio / N tools / ✓ enabled
hermes mcp test bifrost         # 打印工具清单与连接耗时
```

新会话里工具名是 `mcp_bifrost_*`；已开着的会话要 `/reload-mcp`。

## 多集群并存

同一个 Hermes 里接两个 bifrost（比如 H200 和另一台 H100）：

```bash
./install.sh --name bifrost-h200 --storage /data/liuxin/bifrost --prefix /opt/bifrost-h200/bin
./install.sh --name bifrost-h100 --storage /data/other/h100/bifrost --prefix /opt/bifrost-h100/bin
```

工具会分别注册成 `mcp_bifrost_h200_*` 和 `mcp_bifrost_h100_*`。

## 包内容

```
bifrost                       静态 musl 二进制，bifrost 0.1.0
settings.example.json         配置模板
install.sh                    一键安装 + Hermes 注册
README.md                     本文件
```

## 升级

换新包后重跑 `./install.sh`。若本机已有 `~/.bifrost/settings.json`，脚本不会覆盖它；
需要改共享盘路径时显式传 `--storage`。
