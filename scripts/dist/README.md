# bifrost 安装包（Linux x86_64，client / server 通用）

一个包同时能装**客户端**（提交任务、当 Hermes 的 MCP 工具）和**服务端**（跑 daemon、执行任务）。
包内是静态 musl 二进制，目标机**不需要 Rust 工具链、不需要 glibc 匹配**，任何 x86_64 Linux 直接跑。

## 装（二选一）

```bash
tar xzf bifrost-<版本>-x86_64-linux-musl.tar.gz
cd bifrost-<版本>-x86_64-linux-musl
```

**客户端**（提交任务 + 注册成 Hermes MCP 工具）：

```bash
./install.sh --role client --storage /data/liuxin/bifrost
```

无 root 权限时：`PREFIX=$HOME/.local/bin ./install.sh --role client --storage ...`

**服务端**（装 systemd 服务，开机自启，daemon 执行任务）：

```bash
sudo ./install.sh --role server --storage /data/liuxin/bifrost
systemctl status bifrost
```

同一台机器两样都要：

```bash
sudo ./install.sh --role both --storage /data/liuxin/bifrost
```

## install.sh 都做了什么

| 角色 | 二进制 | 配置 | 服务 / 工具 |
|---|---|---|---|
| client | `/usr/local/bin/bifrost` | `~/.bifrost/settings.json` | MCP stdio 自检 + `hermes mcp add` |
| server | `/usr/local/bin/bifrost` | `/etc/bifrost/settings.json` | 渲染 `/etc/systemd/system/bifrost.service`、enable + start、`warmup` 自检 |
| both | 同上 | 同上两份 | 两者都做 |

常用参数：`--storage DIR`（共享盘）、`--prefix DIR`（装到别处）、`--config FILE`、`--name NAME`
（MCP 注册名）、`--force`（覆盖已有配置）、`--no-hermes`、`--no-service`（只写 unit 不启动，
容器/离线镜像里用）、`--dry-run`。

## 关键前提：共享盘

bifrost 通过**共享存储**通信（client 写 `commands/`，server 写 `results/`），两端必须看到同一个目录：

| 机器 | `shared_storage` 应填 |
|---|---|
| GPU 宿主 | `/data/liuxin/bifrost` |
| 容器内 | `/workspace/bifrost`（同一块盘的另一挂载点） |
| 无共享盘 | 用 `transport: ssh` + SSH 配置（见 `docs/deployment.zh-CN.md`） |

## 手工版（不想用脚本）

```bash
install -m 0755 bifrost /usr/local/bin/bifrost
# 客户端
mkdir -p ~/.bifrost && cp settings.example.json ~/.bifrost/settings.json   # 改 shared_storage
hermes mcp add bifrost --env BIFROST_CONFIG=$HOME/.bifrost/settings.json \
  --command /usr/local/bin/bifrost --args mcp-serve
# 服务端（共享盘布局）
bifrost server --storage /data/liuxin/bifrost
```

## 验证

```bash
bifrost --version
bifrost warmup --storage /data/liuxin/bifrost          # 存储/时钟/daemon/GPU 四项自检
bifrost client submit --storage /data/liuxin/bifrost --command 'nvidia-smi'
hermes mcp test bifrost                                 # 客户端侧
```

## 这个包是怎么来的

`scripts/dist/pack.sh` 编 musl 静态二进制并打包；CI 在 main 分支上自动构建、打 tag、
发 Gitee Release（见仓库 `docs/RELEASE.md`）。包内 `BUILD_INFO.txt` 记录 commit / 目标三元组 /
链接方式 / 构建时间，出问题时先看它。

下载：

```bash
BASE=https://gitee.com/peterlau/bifrost/releases/download/<tag>
curl -fLO $BASE/bifrost-<版本>-x86_64-linux-musl.tar.gz
curl -fLO $BASE/bifrost-<版本>-x86_64-linux-musl.tar.gz.sha256
sha256sum -c bifrost-<版本>-x86_64-linux-musl.tar.gz.sha256
```
