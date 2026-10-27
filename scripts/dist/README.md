# bifrost 安装包（Linux x86_64）

一个包同时能装**客户端**（提交任务、当 Hermes 的 MCP 工具）和**服务端**（跑 daemon、执行任务）。
当前只支持 **Linux x86_64**；二进制是静态 musl 链接，目标机不需要 Rust 工具链，也不需要匹配 glibc。

发布页提供三种形式，按目标机的实际情况选一个：

| 形式 | 适合 | 安装方式 |
|---|---|---|
| `.deb` | Debian / Ubuntu | `sudo dpkg -i bifrost_<版本>-1_amd64.deb` |
| `.tar.gz` | 任意 x86_64 Linux、无 root、气隙机器 | 解压后跑 `install.sh`，默认可执行目录 `/usr/local/bin` |
| Source code | 需要审计或自行编译 | Release 页的 `Source code (zip/tar.gz)`，用 Rust 1.87+ 编译 |

## A. Debian/Ubuntu 包（dpkg / apt）

```bash
sudo dpkg -i bifrost_<版本>-1_amd64.deb      # 或 sudo apt install ./bifrost_<版本>-1_amd64.deb
bifrost --version
```

装包只放系统文件，**角色与共享盘路径单独配一次**：

```bash
# 客户端：普通用户即可，不需要 sudo
bifrost-setup client --storage /data/liuxin/bifrost

# 服务端：需要写 /etc 并注册 systemd，用 sudo
sudo bifrost-setup server --storage /data/liuxin/bifrost
```

包内已含：`/usr/bin/bifrost`、`/usr/bin/bifrost-setup`、`/lib/systemd/system/bifrost.service`、
`/etc/default/bifrost`（conffile，`BIFROST_SERVER=1` 时 postinst 自动起服务）。
服务默认**不启用**，所以装完当客户端用完全没问题；`bifrost-setup server` 会写
`/etc/bifrost/settings.json` 并 `systemctl enable --now bifrost`。

## B. tar.gz 包（任意 x86_64 Linux，不需要 dpkg）

```bash
tar xzf bifrost-<版本>-x86_64-linux-musl.tar.gz
cd bifrost-<版本>-x86_64-linux-musl
```

**客户端**（提交任务 + 注册成 Hermes MCP 工具）：

```bash
./install.sh --role client --storage /data/liuxin/bifrost
```

无 root 权限时指定安装目录（默认 `/usr/local/bin`）：

```bash
PREFIX=$HOME/.local/bin ./install.sh --role client --storage /data/liuxin/bifrost
# 或者
./install.sh --role client --prefix $HOME/.local/bin --storage /data/liuxin/bifrost
```

**服务端**（装 systemd 服务，开机自启）：

```bash
sudo ./install.sh --role server --storage /data/liuxin/bifrost
systemctl status bifrost
```

同一台机器两样都要：

```bash
sudo ./install.sh --role both --storage /data/liuxin/bifrost
```

默认安装位置：

| 内容 | 默认路径 | 可覆盖参数 |
|---|---|---|
| 二进制 | `/usr/local/bin/bifrost` | `--prefix DIR` / `PREFIX=DIR` |
| 客户端配置 | `~/.bifrost/settings.json` | `--config FILE` |
| 服务端配置 | `/etc/bifrost/settings.json` | `--config FILE` |
| systemd unit | `/etc/systemd/system/bifrost.service` | `--unit-dir DIR` |

## C. 源码（需要 Rust 工具链）

Release 页的 `Source code (zip)` / `Source code (tar.gz)` 是 GitHub 自动生成的标签快照。
目标机需要 Rust 1.87+（与仓库 `AGENTS.md` 一致）：

```bash
tar xzf bifrost-<tag>.tar.gz && cd bifrost-<tag>
cargo build --release
sudo install -m 0755 target/release/bifrost /usr/local/bin/bifrost
```

需要可移植的静态产物时用仓库脚本：

```bash
scripts/build/build.sh --static
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

## 下载

`<tag>` 用 `latest`（main 滚动快照）或正式版本号（如 `v0.1.0`）：

```bash
BASE=https://github.com/peterlau123/bifrost/releases/download/<tag>

# 二进制包（任意发行版）
curl -fLO $BASE/bifrost-<版本>-x86_64-linux-musl.tar.gz
curl -fLO $BASE/bifrost-<版本>-x86_64-linux-musl.tar.gz.sha256

# Debian/Ubuntu 包
curl -fLO $BASE/bifrost_<版本>-1_amd64.deb
curl -fLO $BASE/bifrost_<版本>-1_amd64.deb.sha256

sha256sum -c *.sha256
```

## 这个包是怎么来的

`scripts/dist/pack.sh` 编 musl 静态二进制并打包；GitHub Actions 的
`.github/workflows/release.yml` 在 `main` 发布 `latest` 快照、在 `release/x.y.z` 发布正式版本
（见仓库 `docs/RELEASE.md`）。包内 `BUILD_INFO.txt` 记录 commit / 目标三元组 / 链接方式 / 构建时间，
出问题时先看它。
