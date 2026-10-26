# 构建与发布流水线（Gitee）

main 分支每次推送 → 质量门禁 → 编 musl 静态包 → 打 tag → 发 Gitee Release（带安装包+校验和）。

## 一次性的三步开通

1. **导入流水线**：仓库页 → 流水线（Gitee Go）→ 新建流水线 → 选「导入 .workflow 配置」，
   选 `release.yml`。之后构建逻辑全在这个文件 + `scripts/ci/*.sh` 里，不再点页面。
2. **加令牌**：流水线 → 参数设置 → 新增加密变量 `GITEE_TOKEN`
   （Gitee → 设置 → [私人令牌](https://gitee.com/profile/personal_access_tokens)，勾 `projects`）。
   它用来建 tag、建 Release、传附件——不配就发布不了，CI 会直接失败并提示这一条。
3. **删掉自动生成的那条流水线**：`.workflow/流水线-202610221113.yml` 是 Gitee 用模板生成的
   （Rust 1.69 + 部署到主机组），和本流程冲突，删掉避免两条流水线抢着跑。

## 流水线做了什么

```
gates 阶段（fmt / clippy -D warnings / cargo test --all）
  └─ 挂了就不进发布阶段
release 阶段（scripts/ci/release.sh）
  ├─ scripts/dist/pack.sh        musl 静态二进制 → bifrost-<版本>-x86_64-linux-musl.tar.gz + .sha256
  ├─ POST /repos/{repo}/tags     tag = v<Cargo.toml 版本>；该版本已发布过则用 v<版本>-b<构建号>
  └─ POST /repos/{repo}/releases + attach_files   上传 tar.gz 与 .sha256
```

- **版本号唯一来源**是 `Cargo.toml` 的 `version`。发新版 = 改这一行并合到 main。
- 同一个版本重复构建不会覆盖已发布内容，而是出 `v0.1.0-b12` 这种构建号 tag。
- 打包失败在 musl 上会自动退回 gnu 构建，包名与 `BUILD_INFO.txt` 里会标出链接方式。

## 安装包内容

`bifrost`（静态二进制）、`install.sh`（`--role client|server|both`）、`bootstrap.sh`、
`README.md`、`SKILL.md`、`settings.example.json`、`LICENSE`、`BUILD_INFO.txt`。

目标机不需要 Rust，也不需要匹配 glibc：

```bash
tar xzf bifrost-<版本>-x86_64-linux-musl.tar.gz && cd bifrost-<版本>-x86_64-linux-musl
./install.sh --role client --storage /data/liuxin/bifrost      # 客户端
sudo ./install.sh --role server --storage /data/liuxin/bifrost # 服务端（systemd）
```

一句话从 Release 装（客户端侧）：

```bash
scripts/dist/bootstrap.sh --from https://gitee.com/peterlau/bifrost/releases/download/<tag>/bifrost-<版本>-x86_64-linux-musl.tar.gz --storage /data/liuxin/bifrost
```

## 本机手动发布（CI 不可用时）

```bash
export GITEE_TOKEN=<令牌>
bash scripts/ci/release.sh                 # 构建 + 打包 + 打 tag + 发 Release
bash scripts/ci/release.sh --no-publish    # 只出包，不发版
bash scripts/dist/publish.sh --dist dist --tag v0.1.0 --dry-run   # 只看 API 调用
```

## 排障

| 症状 | 原因 / 处理 |
|---|---|
| `缺 GITEE_TOKEN` | 流水线参数里没加加密变量，或没勾 `projects` 权限 |
| tag 已存在报错 | 正常：版本已发布过时会自动改用 `v<版本>-b<构建号>` |
| `rustc` 版本太旧 | `scripts/ci/toolchain.sh` 会探测并装 stable（Gitee 模板给的是 1.69） |
| musl 目标装不上 | 脚本会退回 gnu 构建，包名带 `-gnu`，目标机需要 glibc ≥ 构建机 |
| fmt/clippy 挂 | 本地先跑 `cargo fmt && cargo clippy --all-targets -- -D warnings` |
| 构建慢/卡在下载 | 流水线参数加 `CARGO_REGISTRY_MIRROR=rsproxy`（脚本会切 cargo 镜像） |
