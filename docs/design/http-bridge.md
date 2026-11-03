# HTTP Bridge 设计方案（feature/http-bridge）

> 设计稿（2026-11-03 评审通过，D1~D7 已锁定，见 §9）。目标是在不重写 daemon 的前提下，让 bifrost 通过「裸 HTTP PUT/GET」对接内网制品/对象存储（第一个落地目标：**Nexus Raw Hosted Repo**，如 `tmp-raw`）。

---

## 0. 背景与定位

Bifrost 已有 2 种 transport：

- `shared`（Protocol，GPFS/NFS，目标机 daemon 直接用）
- `ssh`   （SshBridge，client 到不了 GPFS 时通过 ssh 操作目标机的目录）

本设计加第 3 种：

- `http` （HttpBridge，client 与目标机之间需要一个**中性 HTTP 中转存储**：client PUT 任务文件上去，目标机 daemon 端 **fetcher 进程** GET 下来放进本地 `commands/` 目录）

**与 SSH bridge 的根本区别**：SSH 是「client 直接读写目标机的目录语义」；HTTP 是「client 与一个独立第三方存储服务交互，目标机额外运行一个 fetcher 把任务从 HTTP 拉到本地目录」。Daemon **完全不改**——它仍然只听本地 Protocol。

**第一个落地场景**（用户 2026-11-03 提供的飞书文档指向）：内部 Nexus `tmp-raw` 仓库，`curl -u user:pass --upload-file <file> <url>` + `wget <url>`。这就是「裸 PUT/GET + Basic Auth」，是 HTTP bridge 的最低公分母。

---

## 1. 设计目标（按优先级）

| # | 目标 | 说明 |
|---|------|------|
| G1 | **daemon 零改动** | daemon 仍只听本地 `Protocol`。HTTP 只用来「转发文件到目标机的 commands/ 目录」 |
| G2 | **对称于 ssh bridge** | settings 加 `transport: "http"` + `[http]` 段；`create_bridge` 工厂加一条分支；命名/错误处理与 `SshSection` 对齐 |
| G3 | **不锁死后端** | settings schema 现在只描述「裸 PUT/GET」；未来的 S3 签名/MultiUpload 通过 `[http]` 加可选字段扩展，不需要新 transport |
| G4 | **能对接 Nexus tmp-raw** | URL 模板、Basic Auth、单次 PUT 上传三件套够通（两步 `.tmp` 法留 opt-in） |
| G5 | **服务目录 = 临时存储** | 服务器不保证稳定（文档原话「随时可能被删」），方案需容忍 **文件遗失**——结果丢失只影响 UI，不能让任务永远 Pending |

---

## 2. 总体架构

```
源机器 (联网)             HTTP 中继存储             目标机器 (离线)
┌──────────────┐        ┌──────────────┐        ┌─────────────────────┐
│ bifrost CLI  │  PUT   │  tmp-raw /   │  GET   │  fetcher (新进程)    │
│ client submit│───────▶│  OSS / MinIO │◀───────│  poll + download     │
│              │        │              │        │  ↓ 写本地 commands/  │
│ client status│  GET   │              │        │                      │
│              │◀───────│              │        │  daemon (不动)       │
│ get_result   │  GET   │              │        │  监控本地 commands/  │
│              │◀───────│              │        │  执行 → results/     │
│              │  PUT   │              │        │  pusher (新进程)     │
│              │◀───────│              │        │  从 results/ PUT 上去│
└──────────────┘        └──────────────┘        └─────────────────────┘
```

**目标机新增 2 个可选辅助进程**（脚本/小工具，不属于 core bifrost）：

- **fetcher**：轮询 HTTP 存储的 `commands/` 目录（Listing 接口），新出现的 `.json` 任务文件 GET 下来写入本地 `shared_storage/commands/`。**Daemon 完全无感知**
- **pusher**：监控本地 `shared_storage/results/`、`status/`，把新生成的文件 PUT 到 HTTP 存储对应路径

> ✅ **D2 已锁定**：fetcher/pusher **纳入 bifrost 主体**，作为 `bifrost fetcher` / `bifrost pusher` 子命令（**不是 daemon 模块**）。理由：复用 settings 解析 + transport 抽象 + 现有日志/重试；单一二进制部署；独立 shell 脚本会引入第二套配置读取和错误处理，长期维护更贵。保持 daemon 纯净，`subcommand` 承载。

---

## 3. Settings schema

```json
{
  "shared_storage": "/gpfs/gcsp/lx/bifrost_local",
  "transport": "http",
  "http": {
    "base_url": "https://nexus.zs.shaipower.online/repository/tmp-raw/uploads",
    "auth": {
      "kind": "basic",
      "user": "tmp-raw",
      "password": "<REDACTED>"
    },
    "prefix": "team-lx/session-20261103",
    "timeout_secs": 30,
    "poll_interval_secs": 5,
    "listing_style": "html"
  },
  "client": { ... },
  "daemon": { ... }
}
```

### 字段说明

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `transport` | `"http"` | ✅ | 新增枚举值；与 `"shared"`/`"ssh"` 并列 |
| `http.base_url` | `String` | ✅ | HTTP 存储的根，所有资源路径都是 `<base_url>/<prefix>/<subdir>/<filename>` |
| `http.auth.kind` | `"basic"` / `"none"` | ✅ | 现在只支持 basic；不填=匿名（如内网开放 GET） |
| `http.auth.user`/`password` | `String` | kind=basic 必填 | Basic Auth 凭证 |
| `http.prefix` | `String` | ❌ | URL 中的隔离前缀（多用户/多任务共存于一个 repo 时用） |
| `http.timeout_secs` | `u64` | ❌ 默认 30 | 单次 PUT/GET 超时 |
| `http.poll_interval_secs` | `u64` | ❌ 默认 5 | fetcher 轮询间隔；client 查 status/result 的最小间隔 |
| `http.atomic_upload` | `bool` | ❌ 默认 false | 默认单次 PUT 已足够（Nexus Raw 每个 PUT 落一个独立 blob，写完才可见，文件级原子性天然有）。开 true 则先 PUT `<file>.tmp` 再 PUT `<file>` 两步法，**裸 HTTP 没有 rename**，这种「原子」也只是约定，留给真正需要的后端 opt-in |
| `http.listing_style` | `"html"` / `"none"` | ❌ 默认 `"html"` | fetcher 怎么列目录：Nexus Raw 支持 GET 目录返回 HTML；未来 S3 兼容要加 `"s3"` / `"json"` |

### 与 SSH section 的对称性

| 维度 | SshSection | HttpSection（新增） |
|---|---|---|
| 必填字段 | `host`, `remote_dir` | `base_url` |
| 鉴权 | 复用 ssh-agent/key | `auth.{kind,user,password}` |
| 隔离 | `remote_dir` 天然用户目录 | `prefix` 显式 |
| 超时 | `connect_timeout` | `timeout_secs` |
| 默认 transport | shared | shared |
| 未知 transport 行为 | serde 报错 | serde 报错 ✅ |

---

## 4. Bridge trait 实现细节

HTTP Bridge 与 SSH Bridge **完全对称**——都是「client 端在远端目录上读写文件」。所以：

- `src/core/http_bridge.rs` `pub struct HttpBridge`，复用 `Bridge` trait 全部 10 个方法
- `src/core/bridge.rs::create_bridge` 工厂加 `transport.is_http()` 分支
- daemon 侧**永远不选 HTTP bridge**，它只听本地 Protocol（HTTP 是 client 与 fetcher/pusher 之间的协议）

### 4.1 路径约定

`<base_url>/<prefix>/<subdir>/<filename>`，其中 `<subdir>` ∈ `{commands, results, status, artifacts, cancel}`。

例：`https://nexus.../uploads/team-lx/session-20261103/commands/20261103_184512_xxx-yyy.json`

### 4.2 `Bridge for HttpBridge` 各方法实现要点

| 方法 | 实现 | 关键约束 |
|---|---|---|
| `submit_task` | 单次 `PUT <base>/commands/<task_file>`（默认，`atomic_upload=false`）。开 `atomic_upload=true` 则 `PUT <task_file>.tmp` → `PUT <task_file>`（两次 PUT，第二次作 rename 替代） | Nexus Raw 单 PUT 落独立 blob、写完才可见，原子性天然有（见 D1）；G4 Nexus 支持 PUT 覆盖 |
| `query_status` | GET `<base>/results/<id>_result.json`；缺则 GET `<base>/status/<id>.json`；缺则 GET `<base>/commands/*<id>*.json`（需要 listing）兜底 Pending | 与 SshBridge 同优先级 |
| `get_result` | GET + parse | 404 → `TaskNotFound`；其他 4xx/5xx → `ExecutionError` |
| `list_pending_tasks` | GET `<base>/commands/`（HTML listing）正则提取 `*.json`；逐个 GET 解析 | 仅 fetcher 用；client 不会调 |
| `read_task` | GET `<base>/commands/<candidate>.json`（也是 listing 辅助） | 同 SshBridge 用 `ls + grep id` |
| `write_result` / `write_status` | PUT `<subdir>/<filename>` | 仅 pusher 用；client 不会调 |
| `remove_task` | HTTP `DELETE` | ⚠️ Nexus Raw 未必支持；遇到 405 就静默当作成功（见 D3） |
| `cancel_task` | PUT `<base>/cancel/<id>.json` | 与 SshBridge 一致；fetcher 也要把这个拉到本地 |

### 4.3 原子性策略（D1 已锁定）

**默认单次 PUT，不开 `.tmp` 两步法。**

依据（2026-11-03 评审结论）：

- Nexus Raw 每个 PUT 落一个独立 blob，**写完才对 client 可见**，文件级原子性天然有
- 「`.tmp` → `.json`」在 Raw 上根本不是 rename，只是**两个不同 key 的文件**，反而要多一步 delete
- status/results JSON 都很小，不存在「大文件传半截」的场景

`atomic_upload` 保留为 opt-in 开关，默认 false。开 true 才走 2-PUT 流程给需要的后端用（比如未来某个不支持单 PUT 原子的对象存储）。

### 4.4 Listing 协议（关键扩展点）

`listing_style` 是未来加 S3 后端的扩展口：

| style | 实现 | 适配 |
|---|---|---|
| `html` | GET 目录，正则 `<a href="...">` 提取文件名（Nexus Raw 默认） | Nexus tmp-raw ✅ |
| `none` | 不支持 listing → `list_pending_tasks` 报错（fetcher 不可用） | 只 client→server 单向场景 |
| （未来）`s3` | `<base>?list-type=2&prefix=...` XML 解析 | MinIO/AWS S3 |
| （未来）`oss-json` | OSS ListObjects JSON | oss://apmm |

### 4.5 错误分类与重试

| 类别 | 处理 |
|---|---|
| 网络 / 5xx | 重试 3 次（指数退避 1s → 2s → 4s），抛 `ExecutionError` |
| 404 | `TaskNotFound` 或 None（具体看方法） |
| 401/403 | 立即抛 `AuthError`（settings 填错时早暴露）—— **新错误枚举值需要加进 `core/error.rs`** |
| 405 (Method Not Allowed) | DELETE 时静默通过（Nexus tmp-raw 实测可能不允许 DELETE——任务文件被 fetcher 拉走后**不主动清理**，文档「随时可能被删除」正好接受这个语义，见 D3） |

---

## 5. Fetcher / Pusher（目标机侧）

Fetcher/pusher 作为 bifrost 自身的 **`bifrost fetcher` / `bifrost pusher`** 子命令（D2 已锁定）——用户可以选择一台机器起 `bifrost server` + `bifrost fetcher` + `bifrost pusher` 三进程，或者一台机器起 `bifrost server`+`bifrost fetcher` 而 pusher 由外部脚本承担（灵活）。

### Fetcher 行为

```
loop {
    let remote_files = http.list("<base>/commands/");        // 同 §4.2 list_pending_tasks 定义（仅 fetcher 用）
    let local_files = scan_local("commands/");                // 已有任务的 id 集合
    for f in remote_files where f not in local_files {
        let content = http.get(f);
        atomic_write(local_commands_dir + f, content);
        http.delete(f)?;        // 拿到就删；405 按成功处理（见 §4.5/D3），其他错误重试
    }
    sleep(poll_interval_secs);
}
```

幂等性：靠**本地文件名匹配**（task_id 子串匹配，与 Protocol 同规则），重复拉取不会产生新任务。

### Pusher 行为

对称：扫本地 `results/`、`status/`，本地有远端没有的，PUT 上去。

### 部署形态

```json
// 目标机的 ~/.bifrost/settings.json
{
  "shared_storage": "/var/bifrost/local",
  "transport": "shared",              // daemon 永远 shared/Protocol
  "http": { ... },                    // 但 fetcher/pusher 需要同一份 http 配置
  "daemon": { ... }
}
```

→ Fetcher/pusher 是**和 daemon 用同一个 settings 但与 daemon 分离**的两个进程：两者不读 `transport` 字段（daemon 读 `transport: "shared"` → Protocol，fetcher/pusher 只关心 `[http]` 段），所以目标机三进程同机并存并不冲突。配置文件第二个用途——这个与 ssh bridge 的 settings 用法略有不同，README/docs 要写清楚。

---

## 6. 对称性核对表（与 ssh bridge 对齐）

| 维度 | Ssh Bridge | Http Bridge（本设计） |
|---|---|---|
| 配置文件 | `[ssh]` 段 | `[http]` 段 |
| 工厂分支 | `transport.is_ssh()` → SshBridge | `transport.is_http()` → HttpBridge |
| daemon 是否用 | ❌ 只 client | ❌ 只 client（fetcher 才用） |
| 文件原子写 | tmp + mv（远程） | **单次 PUT**（默认；如需 tmp+rename 语义开 `atomic_upload`，见 D1） |
| 错误类型 | 复用 `ExecutionError` / `TaskNotFound` | 新增 `AuthError`（401/403） |
| Heartbeat | `heartbeat_age_secs()` 跑 remote shell | HTTP bridge **不支持** health（**D7 决策：不做**。fetcher/pusher 本身就是轮询循环，活性看日志时间戳 / pid 就够，heartbeat 文件只会让 fetcher 多一个角色要处理） |
| `.processing` marker | 保留不删 | 同（fetcher 拉到本地后由 Protocol 管理） |

---

## 7. YAGNI 删除清单（明确不做）

- ❌ S3 签名（v4 / presigned URL）—— 未来加，进入 `listing_style="s3"` / `auth.kind="aws-sigv4"`
- ❌ Multipart upload（>100MB 分块）—— >1MB 的任务文件很少见，要的话传公网 storage 而不是 HTTP
- ❌ TLS 客户端证书 / mTLS —— 内网 HTTP Basic 够
- ❌ 健康检查 (`heartbeat_age_secs`) —— **D7 决策：fetcher/pusher 活性看日志时间戳 / pid 就够**，不需要额外 heartbeat 文件
- ❌ 加密 —— 内网明文 Basic Auth + HTTPS 已够；要加密就走 VPN/mTLS
- ❌ HTTP redirect 跟随 —— Nexus 不跳；S3 确实可能跳，未来加

---

## 8. 测试计划（评审通过后写代码时同步落地）

### 单测（`src/core/http_bridge.rs` `#[cfg(test)]`）

复用 SSH bridge 的 `local: true` 测试模式——**起本地 mock HTTP server**（用 `axum` 或 `tiny_http` 在单测里 spawn），把 `base_url` 指向 `http://127.0.0.1:<port>`。覆盖：

1. `submit_task` 默认单次 PUT（无 `.tmp` 中间态）；开 `atomic_upload=true` 时按 `.tmp → .json` 顺序两次 PUT
2. `submit_task` + `read_task` round-trip
3. `query_status` 三态分支（Completed/Running/Pending）
4. `query_status` NotFound
5. `get_result` 解析 + status string 归一化
6. `remove_task` 405 静默通过
7. Basic Auth header 正确编码
8. URL 拼接：`prefix` 边界（空 / 有 / 含斜杠）
9. 网络失败重试计数（用 mock 强制 500）
10. Auth 失败（401）立即失败

### E2E

对接真 Nexus tmp-raw：

- `bifrost client submit --command "hostname"`（走 HTTP bridge 上传）
- 模拟 fetcher（手工运行一次 `--once`）拉下来 → daemon 拿到执行 → pusher 推结果回去
- `bifrost client status <id>` 看 Completed
- 验证 settings.schema 的 serde round-trip

### 文档更新

- README 「SSH Bridge」章节后新增「HTTP Bridge」章节
- `docs/deployment.md` 加 HTTP 中继部署形态
- `AGENTS.md` 数据流图加 HTTP transport 支路

---

## 9. 评审决策（2026-11-03 锁定）

| # | 决策 | 结论 | 备注 |
|---|---|---|---|
| **D1** | `atomic_upload` 默认值 | **false**（单次 PUT） | Nexus Raw 每个 PUT 落独立 blob、写完才可见，文件级原子性天然有；`.tmp → .json` 在 Raw 上不是 rename，只是两个 key，反而多一步 delete |
| **D2** | fetcher/pusher 归属 | **进 bifrost 主体（Rust 子命令）** | 复用 settings 解析 + transport 抽象 + 日志/重试；单一二进制部署；shell 脚本会引入第二套配置读取和错误处理 |
| **D3** | `remove_task` 在 HTTP 上的行为 | **405 静默通过** | 保持方案原默认；Nexus Raw 不保证稳定，不把「不可删」当错误 |
| **D4** | 命名 `transport: "http"` | **保持** | 描述机制而非后端；S3 也是 HTTP，将来 `listing_style: "s3"` 正好嵌在 http 之下 |
| **D5** | `[http]` 段预留 `auth.kind = "none"` | **预留** | 保持方案原默认（内网开放 GET 场景） |
| **D6** | `password` 明文 + 文件 600 | **保持** | 保持方案原默认；与 ssh `password` 一致，不进 ENV |
| **D7** | heartbeat/health | **不做** | fetcher/pusher 本身是轮询循环，活性看日志时间戳 / pid 就够；heartbeat 文件意味着第三个角色要处理它，YAGNI |

---

## 10. 时序（评审通过后）

```
Day 1  实现 settings schema + transport.is_http 分支 + 占位 HttpBridge（全部 Err）+ 框架单测
Day 2  HTTP client 封装（用 reqwest，blocking 或 async 待选）+ submit_task/read_task 实现 + mock server 单测
Day 3  query_status/get_result/write_*/remove_* + 单测补全
Day 4  fetcher 子命令 + 一次性 manual test 对接真 Nexus tmp-raw
Day 5  pusher 子命令 + E2E host→Nexus→target→result 完整链路 + 文档/README/AGENTS.md 更新
Day 6  review-review，merge 到 main
```

总工作量 ~3-4 天（含测试与文档）。
