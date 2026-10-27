# 构建与发布流水线（GitHub Actions）

**两条通道，分支即通道。**

| 分支 | 推送后发生什么 | 产物 | 有没有正式版本 |
|---|---|---|---|
| `main` | gates → e2e → 打包 | `bifrost-latest-x86_64-linux-musl.tar.gz` + `.sha256`、`bifrost_<版本>-1_amd64.deb` + `.sha256`，并滚动更新 `tag=latest` 的 Release（prerelease） | ❌ 不产正式版本 |
| `release/x.y.z` | gates → e2e → 校验版本 → 打包 | `bifrost-<版本>-x86_64-linux-musl.tar.gz` + `.deb` + 两个 `.sha256` | ✅ tag `v<x.y.z>` + Release |
| 其它分支 | `ci.yml` 跑门禁与 e2e（`.github/workflows/ci.yml`），`release.yml` 不触发 | — | — |

一句话：**代码合进 main 只是"最新快照"，要发正式版本就开 `release/x.y.z` 分支推上去。**

每个 Release 页面提供三种交付形式（仅 Linux x86_64）：

| 形式 | 适合 | 说明 |
|---|---|---|
| `tar.gz` | 任意 x86_64 Linux、无 root、气隙机器 | 解压后按包内 `README.md` 跑 `install.sh`；默认装到 `/usr/local/bin`，可用 `--prefix` 指定 |
| `.deb` | Debian / Ubuntu | `sudo dpkg -i bifrost_<版本>-1_amd64.deb`，之后用 `bifrost-setup` 选角色 |
| Source code | 需要审计或自行编译 | GitHub 自动生成 `Source code (zip)` / `Source code (tar.gz)`；需要 Rust 1.87+ |

## 什么时候会触发打包

`main` 分支只有**打包输入**发生变化时才发布 `latest` 快照：

- `src/**`、`Cargo.toml` / `Cargo.lock`、`build.rs`、`rust-toolchain*`、`.cargo/**`
- `scripts/**`（构建 / 打包 / 安装脚本）、`skills/**`（tar 包内的 `SKILL.md`）、`config/**`
- `LICENSE`

以下变更**不会**触发打包，只更新仓库：

- `docs/**`（含架构与高级用法文档）、`examples/**`、`tests/**`
- 根目录 `README.md` / `README.zh-CN.md`、`AGENTS.md`
- `.github/**`（CI 配置本身）

`release/x.y.z` 正式版必须在分支上包含版本号变更（`Cargo.toml`），否则同样不会触发。
需要强制发布时，在 Actions 里手动运行：`gh workflow run release.yml --ref <branch>`。

如果源码有改动但这次不想打包，在推送的 commit message（PR 合并时是合并 / squash 提交的 message）
里加上 `[skip release]`：gates/e2e 仍会运行，`dist` 打包与发布步骤会跳过。手动 dispatch 不受该
标记影响，始终执行完整发布。

## 发一个正式版本的完整动作

```bash
# 1. 版本号唯一来源：Cargo.toml 的 version，改成你要发的版本
sed -i 's/^version = "0.1.1"/version = "0.2.0"/' Cargo.toml

# 2. 开 release 分支（分支名必须和版本号一致，脚本会硬校验）
git switch -c release/0.2.0
git commit -am "chore(release): v0.2.0"
git push origin release/0.2.0
```

推上去之后 Actions 自动跑：门禁 → e2e → 打 tag `v0.2.0` → 建 Release → 上传四个文件。

## 一次性开通

GitHub Actions 不需要导入流水线，也不需要额外令牌：

- workflow 文件在 `.github/workflows/`，推上去即生效；
- 发布用仓库自带的 `GITHUB_TOKEN`，`release.yml` 已声明 `permissions: contents: write`；
- 如果仓库策略把 Actions 权限锁成只读，需要在 Settings → Actions → General 里允许
  workflow 写入（或者改成 `Read and write permissions`）。

## 流水线做了什么

```
ci.yml（PR / feature 分支推送 / release.yml 复用）
① gates    cargo fmt --check / cargo clippy -D warnings / cargo test --all
             └─ 挂了就停，不进 e2e
② e2e      cargo build --release + tests/e2e/run_all.py
             6 个场景：timeout / job YAML / concurrent / 多卡 pytest 并发 /
                       robustness / MCP stdio 全链路（全是模拟，不需要真 GPU）
release.yml（main / release/** 推送，或手动 dispatch）
③ dist     scripts/ci/release.sh（通道由 BIFROST_BRANCH 判定）
             main          → pack.sh --label latest --no-deb → 滚动更新 tag=latest
             release/x.y.z → 校验 Cargo.toml 版本 == x.y.z → pack.sh（含 .deb）
                             → gh release create v<版本> + 上传附件
```

构建逻辑都在 `scripts/ci/*.sh` 与 `scripts/dist/*.sh` 里，workflow 只负责触发和注入环境变量
（`BIFROST_PUBLISHER=github`、`BIFROST_BRANCH`、`BIFROST_REF`）。

## 四条硬规则（都在脚本里，不靠人记）

| 规则 | 行为 | 在哪 |
|---|---|---|
| **正式版本只能从 `release/x.y.z` 出** | 其它分支 `release.sh` 直接退出非 0，除非显式 `--force-branch` | `scripts/ci/release.sh` |
| **分支名 == Cargo.toml 版本** | `release/0.3.0` 里如果版本还是 0.2.0 → 直接失败，防止打错版本 | 同上 |
| **同版本不可重发** | 已有该 tag 的 Release → 失败并提示升版本；**不再自动出 `-b<构建号>`** | 同上 |
| **musl 静态构建失败即失败** | 不再静默退回 gnu；确需退回要显式 `--allow-fallback` | 同上 |

> 正式版本的语义是干净的：一个 `vX.Y.Z` 只有一个 Release，覆盖是不可能的。

## 正式版本产物

每次正式发布上传四个文件：

| 文件 | 用途 |
|---|---|
| `bifrost_<版本>-1_amd64.deb` | Debian/Ubuntu：`apt install ./xxx.deb` 或 `dpkg -i` |
| `bifrost-<版本>-x86_64-linux-musl.tar.gz` | 任意 x86_64 Linux 的便携包（含 install.sh） |
| 两个 `.sha256` | 校验和 |

另外 GitHub 会为每个 Release 自动附带 `Source code (zip)` / `Source code (tar.gz)`，源码编译安装用。

`.deb` 里是 `/usr/bin/bifrost` + `/usr/bin/bifrost-setup` + systemd unit（默认不启用）+
`/etc/default/bifrost`（conffile）。装完用 `bifrost-setup client|server --storage <DIR>` 定角色，
因为 apt 安装无法带参数，而共享盘路径每台机器不同。

tar.gz 里是 `bifrost`（静态二进制）、`install.sh`（`--role client|server|both`）、
`bootstrap.sh`、`README.md`、`SKILL.md`、`settings.example.json`、`LICENSE`、`BUILD_INFO.txt`。

目标机不需要 Rust，也不需要匹配 glibc：

```bash
# Debian/Ubuntu
sudo dpkg -i bifrost_<版本>-1_amd64.deb
bifrost-setup client --storage /data/liuxin/bifrost            # 客户端
sudo bifrost-setup server --storage /data/liuxin/bifrost       # 服务端（systemd）

# 其它发行版
tar xzf bifrost-<版本>-x86_64-linux-musl.tar.gz && cd bifrost-<版本>-x86_64-linux-musl
./install.sh --role client --storage /data/liuxin/bifrost      # 客户端
sudo ./install.sh --role server --storage /data/liuxin/bifrost # 服务端（systemd）
```

## 快照（main 的 latest）

`main` 每次推送都会把 `tag=latest` 的滚动 Release 覆盖更新，所以有稳定的下载地址：

```bash
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz.sha256
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost_<版本>-1_amd64.deb
https://github.com/peterlau123/bifrost/releases/download/latest/bifrost_<版本>-1_amd64.deb.sha256
```

快照同样带 `.deb`；版本号不变时文件名不变，每次构建覆盖更新（换版本号后旧文件会被清理）。

`latest` 每次构建都会移到当前 `main` 提交（所以 Release 页的 Source code 包也是最新的），
**精确身份看包内 `BUILD_INFO.txt`**：

```
package:  bifrost-latest-x86_64-linux-musl
version:  0.2.0
label:    latest
commit:   <short sha>
branch:   main
link:     static
built_at: 2026-10-27T...
```

一句话装最新快照（气隙/客户机房）：

```bash
scripts/dist/bootstrap.sh \
  --from https://github.com/peterlau123/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz \
  --storage /data/liuxin/bifrost
```

正式版本则把 URL 里的 `latest` 换成 `v<x.y.z>`，包名也换成 `bifrost-<版本>-...`。

## 本机手动发布（CI 不可用时）

```bash
# GitHub（gh 已登录，或用 GH_TOKEN）
export BIFROST_PUBLISHER=github
export BIFROST_BRANCH=main            # detached HEAD / 手动发版时显式指定
export GITHUB_REPOSITORY=peterlau123/bifrost

# 小演一遍：只看会做什么，不动远端
bash scripts/ci/release.sh --dry-run

# 出 latest 快照包并发布
bash scripts/ci/release.sh --snapshot

# 只出正式包、不发 Release
BIFROST_BRANCH=release/0.2.0 bash scripts/ci/release.sh --release --no-publish

# 正式发版（需在 release/x.y.z 上，版本必须一致）
BIFROST_BRANCH=release/0.2.0 bash scripts/ci/release.sh --release

# 应急：版本号和分支对不上也要发（会跳过分支/版本校验，慎用）
bash scripts/ci/release.sh --release --force-branch --tag v0.2.0
```

`scripts/dist/publish-github.sh` 支持 `--dry-run`；需要 Gitee 发布时把
`BIFROST_PUBLISHER` 留空（默认 `gitee`）并设置 `GITEE_TOKEN`，走
`scripts/dist/publish.sh` / `publish-latest.sh`。

## 怎么确认流水线真的跑了

推完 main / release 分支后，别只看提交有没有上去，要确认**发布物**：

```bash
gh run list --workflow release.yml          # 最近几次运行
gh release view latest                      # 快照
gh release view v0.2.0                      # 正式版本
```

- `✓` → 流水线跑通了
- main 构建成功但没有 `latest` → 先看 Actions 日志里的 `publish-github.sh` 输出
  （按设计，快照发布失败只告警、不拖红 main）
- `FAIL: gh 未登录` / `Resource not accessible by integration` → `permissions` 或仓库
  Actions 写权限没开

## 排障

| 症状 | 原因 / 处理 |
|---|---|
| `当前分支 'xxx' 不允许发版` | 通道规则：main 出快照、release/x.y.z 出正式版；别从 feature 分支发 |
| `分支与 Cargo.toml 版本不一致` | 把 Cargo.toml 的 version 改成和 `release/x.y.z` 一致 |
| `vX.Y.Z 已经发布过` | 正式版本不可覆盖：升 Cargo.toml 版本，开新 release 分支 |
| `Resource not accessible by integration` | `release.yml` 的 `permissions: contents: write` 被仓库策略覆盖 |
| `musl 静态构建失败` | 默认失败是故意的（避免静默出 glibc 包）；确认要退回就加 `--allow-fallback` |
| `rustc` 版本太旧 | `scripts/ci/toolchain.sh` 会探测并装 stable（本地旧工具链时） |
| `latest` Release 更新失败 | 只告警不拖红 main（快照发布是 best-effort），制品仍在 Actions artifact 里 |
| main 推了但没有 `latest` | 看 Actions 日志，再跑 `gh release view latest` |
| e2e 报缺少 python3 | GitHub 的 `ubuntu-latest` 自带 python3；自建 runner 需自行安装 |
| fmt/clippy 挂 | 本地先跑 `cargo fmt && cargo clippy --all-targets -- -D warnings` |
| e2e 挂 | 本地 `cargo build --release && python3 tests/e2e/run_all.py`，失败项会打印场景名 |
| 构建慢/卡在下载 | 仓库 Actions secret 加 `CARGO_REGISTRY_MIRROR=rsproxy` 不会自动生效，需要在 workflow 里设同名 env |
