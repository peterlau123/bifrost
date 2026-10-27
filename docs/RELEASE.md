# 构建与发布流水线（Gitee）

**两条通道，分支即通道。**

| 分支 | 推送后发生什么 | 产物 | 有没有正式版本 |
|---|---|---|---|
| `main` | 门禁 → e2e → 打包 | `bifrost-latest-x86_64-linux-musl.tar.gz` + `.sha256`，并覆盖更新 `tag=latest` 的滚动 Release（prerelease） | ❌ 不产正式版本 |
| `release/x.y.z` | 门禁 → e2e → 校验版本 → 打包 | `bifrost-<版本>-x86_64-linux-musl.tar.gz` + `.deb` + 两个 `.sha256` | ✅ tag `v<x.y.z>` + Release |
| 其它分支 | 不触发流水线；`release.sh` 本身也会拒绝 | — | — |

一句话：**代码合进 main 只是"最新快照"，要发正式版本就开 `release/x.y.z` 分支推上去。**

## 发一个正式版本的完整动作

```bash
# 1. 版本号唯一来源：Cargo.toml 的 version，改成你要发的版本
sed -i 's/^version = "0.1.0"/version = "0.2.0"/' Cargo.toml

# 2. 开 release 分支（分支名必须和版本号一致，流水线会硬校验）
git checkout -b release/0.2.0
git commit -am "chore(release): v0.2.0"
git push origin release/0.2.0
```

推上去之后流水线自动跑：门禁 → e2e → 打 tag `v0.2.0` → 建 Release → 上传四个文件。

## 一次性的三步开通

1. **导入流水线**：仓库页 → 流水线（Gitee Go）→ 新建流水线 → 选「导入 .workflow 配置」，
   选 `release.yml`。之后构建逻辑全在这个文件 + `scripts/ci/*.sh` 里，不再点页面。
2. **加令牌**：流水线 → 参数设置 → 新增加密变量 `GITEE_TOKEN`
   （Gitee → 设置 → [私人令牌](https://gitee.com/profile/personal_access_tokens)，勾 `projects`）。
   它用来建 tag、建 Release、传附件——不配就发布不了，正式通道会直接失败并提示这一条。
3. **删掉自动生成的那条流水线**：`.workflow/流水线-202610221113.yml` 是 Gitee 用模板生成的
   （Rust 1.69 + 部署到主机组），和本流程冲突，删掉避免两条流水线抢着跑。

## 流水线做了什么

```
① gates    cargo fmt --check / cargo clippy -D warnings / cargo test --all
             └─ 挂了就停，不进后面两阶段
② e2e      cargo build --release + tests/e2e/run_all.py
             6 个场景：timeout / job YAML / concurrent / 多卡 pytest 并发 /
                       robustness / MCP stdio 全链路（全是模拟，不需要真 GPU）
③ dist     scripts/ci/release.sh（按 GITEE_BRANCH 判定通道）
             main          → pack.sh --label latest --no-deb → 更新 tag=latest 滚动 Release
             release/x.y.z → 校验 Cargo.toml 版本 == x.y.z → pack.sh（含 .deb）
                             → POST /tags（tag=v<版本>）→ POST /releases + attach_files
```

## 四条硬规则（都在脚本里，不靠人记）

| 规则 | 行为 | 在哪 |
|---|---|---|
| **正式版本只能从 `release/x.y.z` 出** | 其它分支 `release.sh` 直接退出非 0，除非显式 `--force-branch` | `scripts/ci/release.sh` |
| **分支名 == Cargo.toml 版本** | `release/0.3.0` 里如果版本还是 0.2.0 → 直接失败，防止打错版本 | 同上 |
| **同版本不可重发** | 已有该 tag 的 Release → 失败并提示升版本；**不再自动出 `-b<构建号>`** | 同上 |
| **musl 静态构建失败即失败** | 不再静默退回 gnu；确需退回要显式 `--allow-fallback` | 同上 |

> 以前的实现会在版本重复时自动发 `v0.1.0-b12`，导致 Release 页变成 nightly 流水，且 `-bN` 也没标 prerelease。
> 现在正式版本的语义是干净的：一个 `vX.Y.Z` 只有一个 Release，覆盖是不可能的。

## 正式版本产物

每次正式发布上传四个文件：

| 文件 | 用途 |
|---|---|
| `bifrost_<版本>-1_amd64.deb` | Debian/Ubuntu：`apt install ./xxx.deb` 或 `dpkg -i` |
| `bifrost-<版本>-x86_64-linux-musl.tar.gz` | 任意 x86_64 Linux 的便携包（含 install.sh） |
| 两个 `.sha256` | 校验和 |

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

## 快照包（main 的 latest）

`main` 每次推送都会把 `tag=latest` 的滚动 Release 覆盖更新，所以有一个稳定下载地址：

```bash
https://gitee.com/peterlau/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz
https://gitee.com/peterlau/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz.sha256
```

`latest` 是滚动标签（不指向固定 commit），**真实身份看包内 `BUILD_INFO.txt`**：

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
  --from https://gitee.com/peterlau/bifrost/releases/download/latest/bifrost-latest-x86_64-linux-musl.tar.gz \
  --storage /data/liuxin/bifrost
```

正式版本则把 URL 里的 `latest` 换成 `v<x.y.z>`，包名也换成 `bifrost-<版本>-...`。

## 本机手动发布（CI 不可用时）

```bash
export GITEE_TOKEN=<令牌>

# 小演一遍：只看会做什么，不动远端
bash scripts/ci/release.sh --dry-run

# 出 latest 快照包（需在 main 上）
bash scripts/ci/release.sh --snapshot --no-publish

# 正式发版（需在 release/x.y.z 上，版本必须一致）
bash scripts/ci/release.sh --release

# 只出正式包、不发 Release
bash scripts/ci/release.sh --release --no-publish

# 应急：版本号和分支对不上也要发（会跳过分支/版本校验，慎用）
bash scripts/ci/release.sh --release --force-branch --tag v0.2.0
```

`scripts/dist/publish.sh` / `publish-latest.sh` 都支持 `--dry-run`，可以先看 API 调用再真跑。

## 怎么确认流水线真的跑了

推完 main / release 分支后，别只看提交有没有上去，要确认**发布物**：

```bash
bash scripts/ci/verify-release.sh          # 查 latest 快照
bash scripts/ci/verify-release.sh v0.2.0   # 查某个正式 tag
```

- `✅ 该发布就是这个 HEAD 的产物` → 流水线跑通了
- `❌ 没有 tag=latest 的 Release` → 两种可能：**流水线没激活**，或**没配 `GITEE_TOKEN`**
  （按设计，快照发布失败只告警、不拖红 main，所以"构建成功但没有 latest"是正常现象）
- `⚠️ 发布记录的是 xxx，本地 HEAD 是 yyy` → 发布落后（`latest` 是滚动标签，正文里的 commit
  只在创建时写入，最终身份以包内 `BUILD_INFO.txt` 为准）

彻底没反应时去 Gitee → 仓库 → 流水线，看有没有对应 commit 的运行记录；一条都没有就是没导入流水线。

## 排障

| 症状 | 原因 / 处理 |
|---|---|
| `当前分支 'xxx' 不允许发版` | 通道规则：main 出快照、release/x.y.z 出正式版；别从 feature 分支发 |
| `分支与 Cargo.toml 版本不一致` | 把 Cargo.toml 的 version 改成和 `release/x.y.z` 一致 |
| `vX.Y.Z 已经发布过` | 正式版本不可覆盖：升 Cargo.toml 版本，开新 release 分支 |
| `缺 GITEE_TOKEN` | 流水线参数里没加加密变量，或没勾 `projects` 权限 |
| `musl 静态构建失败` | 默认失败是故意的（避免静默出 glibc 包）；确认要退回就加 `--allow-fallback` |
| `rustc` 版本太旧 | `scripts/ci/toolchain.sh` 会探测并装 stable（Gitee 模板给的是 1.69） |
| `latest` Release 更新失败 | 只告警不拖红 main（快照发布是 best-effort），制品仍在流水线 artifact 里 |
| main 推了但没有 `latest` | 先跑 `bash scripts/ci/verify-release.sh`；再查流水线页面有没有该 commit 的运行记录 |
| e2e 阶段报"镜像里没有 python3" | Gitee Go 的 `build@rust` 镜像缺 python 解释器，换镜像或在该阶段前装 python3 |
| fmt/clippy 挂 | 本地先跑 `cargo fmt && cargo clippy --all-targets -- -D warnings` |
| e2e 挂 | 本地 `cargo build --release && python3 tests/e2e/run_all.py`，失败项会打印场景名 |
| 构建慢/卡在下载 | 流水线参数加 `CARGO_REGISTRY_MIRROR=rsproxy`（脚本会切 cargo 镜像） |
