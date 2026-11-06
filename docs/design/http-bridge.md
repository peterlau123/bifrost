# HTTP Bridge 设计方案（feature/http-bridge）

> 设计稿（2026-11-03 评审通过，D1~D7 已锁定；**2026-11-06 按真实 Nexus tmp-raw 逐条实测复核：D3 重写，新增 D8/D9/D10**；同日落实两条用户新约束：**D11 顺序模式 + D12 双方契约**，lock 见 §9/§10；**同日二轮评审（Ax）：D9 改 write-ahead、D8 补分页与查询词约束、新增 D13/D14**——见 §9 与附录 A/B）。目标是在不重写 daemon 的前提下，让 bifrost 通过「裸 HTTP PUT/GET」对接内网制品/对象存储（第一个落地目标:**Nexus Raw Hosted Repo**，如 `tmp-raw`）。
>
> **评审链（2026-11-06 用户明确定）**：`feature/http-bridge @ 769bf82` 由 **@Ax-vLLM适配 review**，再到 **@刘鑫 终审**；两关都通过之后才允许开始 Day 1。当前状态：**Ax review 完成（二轮结论见 §9 的 D8/D9/D13/D14），待刘鑫终审**。本设计分支**不 merge 到任何 release 分支**（release/0.1.1 等）——它只承载设计文档，后续 merge 对象与时机由最终评审结论决定。

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
| G6 | **V1 顺序模式**（2026-11-06 新增，D11 决策） | client 一次只提交一个任务、阻塞等待该任务完成；fetcher 每轮最多拉 **1 个** task 落本地。不做并发提交/批量拉取。这样能省掉 manifest 合并与多 client 竞争协调，复杂度砍到最低 |

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

- **fetcher**：轮询 HTTP 存储的 `commands/` 前缀（listing 走 Nexus REST search，见 D8），新出现的 `.json` 任务文件 GET 下来写入本地 `shared_storage/commands/`。**Daemon 完全无感知**
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
    "repository": "tmp-raw",
    "auth": {
      "kind": "basic",
      "user": "tmp-raw",
      "password": "***"
    },
    "prefix": "team-lx/bifrost-http-bridge",
    "listing_query": "bifrost-http-bridge",
    "connect_timeout_secs": 10,
    "read_timeout_secs": 30,
    "poll_interval_secs": 10,
    "listing_style": "rest-search"
  },
  "client": { ... },
  "daemon": { ... }
}
```

> **`prefix` 是双方契约的根**：client 侧 daemon 侧必须使用**同一份 `~/.bifrost/settings.json`**（至少 `[http]` 段一致）。写成 `<owner>/<project>` 形式（例 `team-lx/bifrost-http-bridge`），稳定、不随时间戳变化。详见 §4.6 双方契约。

### 字段说明

| 字段 | 类型 | 必填 | 说明 |
|------|------|------|------|
| `transport` | `"http"` | ✅ | 新增枚举值；与 `"shared"`/`"ssh"` 并列 |
| `http.base_url` | `String` | ✅ | HTTP 存储的根，所有资源路径都是 `<base_url>/<prefix>/<subdir>/<filename>`。**必须 `https://`**——Basic Auth 明文，`http://` 在配置解析期直接报错 |
| `http.auth.kind` | `"basic"` / `"none"` | ✅ | 现在只支持 basic；不填=匿名（如内网开放 GET） |
| `http.auth.user`/`password` | `String` | kind=basic 必填 | Basic Auth 凭证 |
| `http.prefix` | `String` | **✅ 必填**（2026-11-06 改为必填，见 D12） | URL 中的隔离前缀，也是双方契约的根；推荐 `<owner>/<project>` 形式（例 `team-lx/bifrost-http-bridge`），稳定、与时间戳无关。client 与 daemon 的 `[http]` 段必须**完全一致**，否则 fetcher 拉不到任务、pusher 推错位置、client 拉不到结果 |
| `http.connect_timeout_secs` | `u64` | ❌ 默认 10 | TCP/TLS 建连超时 |
| `http.read_timeout_secs` | `u64` | ❌ 默认 30 | **inactivity** 读超时（相邻两次收到数据的最大间隔），**不是整个请求的总超时** |
| `http.poll_interval_secs` | `u64` | ❌ 默认 **10**（2026-11-06 从 5s 提高，见 D11） | fetcher 轮询间隔；client 查 status/result 的最小间隔。顺序模式下 5s 轮询无收益（同时只有 1 个任务在发生），10s 既省 Nexus 调用，又让对人延迟可感知 |
| `http.atomic_upload` | `bool` | ❌ 默认 false | 默认单次 PUT 已足够（Nexus Raw 每个 PUT 落一个独立 blob，写完才可见，文件级原子性天然有）。开 true 则先 PUT `<file>.tmp` 再 PUT `<file>` 两步法，**裸 HTTP 没有 rename**，这种「原子」也只是约定，留给真正需要的后端 opt-in |
| `http.repository` | `String` | listing_style=`rest-search` 必填 | Nexus 仓库名（如 `tmp-raw`）。search API 在 `/service/rest/v1/` 下，**从 `base_url` 推不出 repo 名**，必须单独给。**配置期校验一致性**：`base_url` 必须含 `/repository/<repository>/`，不一致直接报错——否则 search 查 A 库、PUT 进 B 库，任务永远拉不到，症状像「fetcher 没起」（D13） |
| `http.listing_query` | `String` | ❌ 默认取 `prefix` 最后一段 | 传给 Nexus search 的 `q`。**不要用整条路径**：`q` 是分词 OR 匹配，路径里的通用 token（`materials`/`commands`）会把无关对象全拉回来。默认取最独特的那一段（如 `session-20261103`），实测 112 条 → 8 条（附录 B） |
| `http.listing_style` | `"rest-search"` / `"html"` / `"none"` | ❌ 默认 `"rest-search"` | fetcher 怎么列目录：`rest-search` = Nexus REST search（实测可用）；`html` = GET 目录解析 `<a href>`（**Nexus raw 实测 404，只留给 nginx autoindex 类后端**）；未来 S3 兼容再加 `"s3"` / `"json"`。**`none` 不支持 client 侧 `query_status` 兜底**，到期即 `TaskLost`（D14） |

### 与 SSH section 的对称性

| 维度 | SshSection | HttpSection（新增） |
|---|---|---|
| 必填字段 | `host`, `remote_dir` | `base_url` |
| 鉴权 | 复用 ssh-agent/key | `auth.{kind,user,password}` |
| 隔离 | `remote_dir` 天然用户目录 | `prefix` 显式 |
| 超时 | `connect_timeout` | `connect_timeout_secs` + `read_timeout_secs`（inactivity） |
| 默认 transport | shared | shared |
| 未知 transport 行为 | serde 报错 | serde 报错 ✅ |

---

## 4. Bridge trait 实现细节

HTTP Bridge 与 SSH Bridge **完全对称**——都是「client 端在远端目录上读写文件」。所以：

- `src/core/http_bridge.rs` `pub struct HttpBridge`，复用 `Bridge` trait 全部 10 个方法
- `src/core/bridge.rs::create_bridge` 工厂加 `transport.is_http()` 分支
- daemon 侧**永远不选 HTTP bridge**，它只听本地 Protocol（HTTP 是 client 与 fetcher/pusher 之间的协议）

### 4.1 路径约定

`<base_url>/<prefix>/<subdir>/<filename>`，其中 `<subdir>` ∈ `{commands, results, status, cancel}`（都是 <1MB 的 JSON）。`artifacts/` **不走 HTTP 链路**，见 §7。

例：`https://nexus.../uploads/team-lx/session-20261103/commands/20261103_184512_xxx-yyy.json`

**前缀归一化（D13）**：所有拼接与过滤用的前缀都必须先归一成 `"<prefix>/"`（trim 首尾 `/` 后补一个 `/`）。`path.starts_with("team-lx/sess")` 会命中 `team-lx/session-evil/commands/...`——**跨前缀拉别人的任务下来执行是安全事故，不是性能问题**。URL 编码按**段**做（`prefix` 里的 `/` 是路径分隔符，不能整体 percent-encode），空格/`#`/`+` 按段编码。

### 4.2 `Bridge for HttpBridge` 各方法实现要点

| 方法 | 实现 | 关键约束 |
|---|---|---|
| `submit_task` | 单次 `PUT <base>/commands/<task_file>`（默认，`atomic_upload=false`）。开 `atomic_upload=true` 则 `PUT <task_file>.tmp` → `PUT <task_file>`（两次 PUT，第二次作 rename 替代） | Nexus Raw 单 PUT 落独立 blob、写完才可见，原子性天然有（见 D1）；PUT 新对象实测 **201**、覆盖同路径也 **201** → 判定一律 `status().is_success()`，别写 `== 200` |
| `query_status` | GET `<base>/results/<id>_result.json`；缺则 GET `<base>/status/<id>.json`；缺则 REST search 在 `<prefix>/commands/` 下查 `<id>`（D8）兜底 Pending | 与 SshBridge 同优先级；三级都查不到且 `wait_timeout` 已到期 → `TaskLost`（D10），message 里带上三级探测结果；`listing_style=none` 时没有第三级（D14） |
| `get_result` | GET + parse | **先看 `status == 200` 再读 body**——Nexus 的 404 页带约 1.5KB body 和 `Content-Length`，靠长度判存在会把「不存在」当成「存在但解析失败」；404 → `TaskNotFound`；其他 4xx/5xx → `ExecutionError` |
| `list_pending_tasks` | 循环 `GET /service/rest/v1/search/assets?repository=<repo>&q=<listing_query>&continuationToken=...`，**必须翻到 `continuationToken` 缺失**；每页取 `items[].path`，按归一化前缀 `starts_with` 过滤、丢掉 `*.tmp` 与非 `.json`（D8） | 仅 fetcher 用；client 不会调。`q` 是分词 OR 匹配且**每页上限 50、按相关度排序**——前缀对象可能落在第 2 页之后，不翻完就是静默丢任务。页数上限默认 20 页，超限抛 `ExecutionError` 并告警（防 `q` 退化时全库扫描） |
| `read_task` | 先 search 列出 `commands/` 前缀，再按 `<id>` 匹配对象 GET | 无 listing 时不能靠 HEAD 猜文件名：命令文件名带 `<ts>_` 前缀，光有 id 拼不出来 |
| `write_result` / `write_status` | PUT `<subdir>/<filename>` | 仅 pusher 用；client 不会调 |
| `remove_task` | `DELETE <base>/.../<file>` | 2xx / 404 → 视为已不存在（成功）；401/403 → `AuthError`；405 → 后端不支持删除，记 warning 后依赖 ledger（D3/D9） |
| `cancel_task` | PUT `<base>/cancel/<id>.json` | 与 SshBridge 一致；fetcher 拉到本地后**要 DELETE 远端 cancel 对象**（`Protocol::clear_cancel` 只清本地，否则远端永久堆积） |

> **同步还是异步：用 `reqwest::blocking`（Day 2 前定死）**。`Bridge` trait 全是同步方法（`core/bridge.rs`），SshBridge 也是阻塞实现，按对称性选 blocking；**禁止在 tokio runtime 内调用**。HTTP bridge 只被 client CLI / fetcher / pusher 使用，daemon 永不选 http（G1），所以这个约束成立。
>
> 已核对源码，该约束**可满足**：`src/main.rs` 的 `fn main()` 是同步函数，`bifrost client …` 走 `handle_client_mode`（全程无 runtime），tokio runtime 只出现在 `server`/`mcp-serve` 分支，而这两条永不使用 http transport。实现要求：fetcher/pusher 子命令**自己不要建 runtime**，也别把 blocking client 塞进 `rt.block_on`。可加 `debug_assert!(tokio::runtime::Handle::try_current().is_err())` 兜住误用。
>
> 依赖与超时 API：`Cargo.toml` **目前还没有 `reqwest`**，Day 2 需新增 `reqwest = { version = "0.12", features = ["blocking", "rustls-tls"], default-features = false }`。`read_timeout_secs`（inactivity）要用 `blocking::ClientBuilder::read_timeout`（0.12.4+ 才有），**不要**用 `Client::timeout()`——那是整请求总超时，会把大对象判死（附录 A：单流 26 MiB/s）。

### 4.3 原子性策略（D1 已锁定）

**默认单次 PUT，不开 `.tmp` 两步法。**

依据（2026-11-03 评审结论）：

- Nexus Raw 每个 PUT 落一个独立 blob，**写完才对 client 可见**，文件级原子性天然有
- 「`.tmp` → `.json`」在 Raw 上根本不是 rename，只是**两个不同 key 的文件**，反而要多一步 delete
- status/results JSON 都很小，不存在「大文件传半截」的场景

`atomic_upload` 保留为 opt-in 开关，默认 false。开 true 才走 2-PUT 流程给需要的后端用（比如未来某个不支持单 PUT 原子的对象存储）。

### 4.4 Listing 协议（关键扩展点）

`listing_style` 是未来加 S3 后端的扩展口。**2026-11-06 实测推翻了「Nexus Raw 支持 GET 目录返回 HTML」这个前提**：目录里明明有对象，`GET <base>/commands/` 仍返回 **404**（`text/html`，约 1.5KB 错误页）。所以默认改成 REST search：

| style | 实现 | 适配 |
|---|---|---|
| `rest-search` | `GET /service/rest/v1/search/assets?repository=<repo>&q=<listing_query>&continuationToken=...`，读 `items[].path`，**必须翻到 token 缺失**（50 条/页） | Nexus Raw ✅ **默认**（实测匿读 200） |
| `html` | GET 目录，正则 `<a href="...">` 提取文件名 | ❌ Nexus raw 实测 404；只留给 nginx autoindex 类后端 |
| `none` | 不支持 listing → `list_pending_tasks` 报错（fetcher 不可用），且 client 到期只能 `TaskLost`（D14） | 只 client→server 单向场景 |
| （未来）`s3` | `<base>?list-type=2&prefix=...` XML 解析 | MinIO/AWS S3 |
| （未来）`oss-json` | OSS ListObjects JSON | oss://apmm |

用 REST search 当 listing 的六条硬约束（1~3 一轮实测，4~6 二轮实测）：

1. **`q` 是分词 OR 匹配，必须本地再过滤**：`q=review-probe/hermes-http-bridge` 除了本前缀对象，还返回了一条无关的 `materials/.../cni_bin/bridge`。因此拿到 `items[].path` 后必须 `path.starts_with("<prefix>/commands/")`（**注意末尾 `/`，见 D13**），否则会**把别人的任务拉到本地执行**（安全事故，不是性能问题）。
2. **对象写完才出现**：语义上比目录 listing 更安全，天然等价于「完整可见」——不会读到半截文件。
3. **repo 名推不出来**：search API 在 `/service/rest/v1/` 下，`base_url` 里只有 `/repository/<repo>/...` 路径，所以 schema 必须单列 `repository` 字段；配置期校验 `base_url` 与它一致（D13）。
4. **必须翻完分页，否则静默丢任务**：`search/assets` 每页上限 **50 条、按相关度排序**，前缀对象完全可能落在第 2 页之后。实测 `q=materials/deploy-tools/os-init`（整条路径）= 3 页 112 条，其中只有 8 条属该前缀；`q=os-init`（唯一 token）= 1 页 8 条。所以：`q` 用最独特的 token（`listing_query`），并循环到 `continuationToken` 缺失，页数上限 20 页。
5. **过滤要丢 `*.tmp` 与非 `.json`**：`atomic_upload=true` 会在远端留下 `<file>.tmp` 对象，不丢就会被 fetcher 当任务拉下来。
6. **每轮日志打 `pages=<n> matched=<m>`**：否则「`q` 退化成 0 命中」和「确实没任务」在群里分不清（D7 不做 heartbeat 的补偿之一）。

> 对照接口：`/service/rest/v1/assets?repository=<repo>[&q=...]` 也能列，但**只有 10 条/页**（search 是 50），同样分词匹配，不适合做 listing 主路径；`/v1/components` 对 raw 仓库无用。

### 4.5 错误分类与重试

| 类别 | 处理 |
|---|---|
| 网络 / 5xx | 重试 3 次（指数退避 1s → 2s → 4s），抛 `ExecutionError` |
| 超时 | 拆两段：`connect_timeout_secs` 建连、`read_timeout_secs` inactivity。**别用一个总超时**——单流实测 26 MiB/s，30s 总超时等于给大对象判 780MB 死刑 |
| 2xx（含 201/204） | 一律成功（`status().is_success()`）。实测 PUT 新对象/覆盖都是 **201**、DELETE 是 **204** |
| 404 | 读操作 → `TaskNotFound`；DELETE → 已不存在（成功）；**判存在必须先看状态码**（404 页也带 body） |
| 401/403 | 立即抛 `AuthError`（settings 填错时早暴露）—— **新错误枚举值需要加进 `core/error.rs`** |
| 405 (Method Not Allowed) | 只在 DELETE 上有意义：后端真不支持删除 → 记 warning，幂等靠 ledger（D3/D9）。**不是「静默当成功」** |
| `TaskLost` | `wait_timeout` 到期、且 search 在 `results/status/commands` 三处都查不到 → 抛 `TaskLost`（新增变体，D10），别继续报 Pending。**message 必须带三级探测结果**（如 `found=[commands]`）：`commands` 仍在、results/status 没有 = 「没人消费/fetcher 没起」，与「真被清掉」是两种故障，否则现场会按「丢了」重提、把 fetcher 停摆掩盖掉 |

### 4.6 双方契约（2026-11-06 新增，D12）

部署 HTTP Bridge 时，**client 侧和 daemon 侧必须使用同一份 `[http]` 段配置**——这是双方唯一的契约。四件事必须同时成立：

| # | 双方都要知道 | 约定 |
|---|---|---|
| 1 | **client 往哪里 PUT 任务** | `<base_url>/<prefix>/commands/<ts>_<task_id>.json`（每次新增，不覆盖） |
| 2 | **client 从哪里拉结果 / 状态** | `<base_url>/<prefix>/results/<task_id>_result.json` 与 `<base_url>/<prefix>/status/<task_id>.json` |
| 3 | **fetcher 怎么发现任务** | REST search（`q=listing_query`，**翻完分页**）在 `<prefix>/commands/` 下查，本地 `path.starts_with("<prefix>/")` 二次过滤（D8/D13），拉到后写 `<shared_storage>/commands/` 同时 DELETE 远端 |
| 4 | **pusher 把结果推到哪里** | `<base_url>/<prefix>/results/` + `<base_url>/<prefix>/status/`（PUT 成功后删除本地文件） |

`cancel/<task_id>.json` 与上述对称：client PUT 到 `<base_url>/<prefix>/cancel/`，fetcher 拉到本地 `<shared_storage>/cancel/` 后 DELETE 远端。

**双方共享的 settings 模板**（README「HTTP Bridge」章节照抄这份并填上真实密码）：

```json
{
  "transport": "http",
  "shared_storage": "/var/bifrost/local",
  "http": {
    "base_url": "https://nexus.zs.shaipower.online/repository/tmp-raw/uploads",
    "repository": "tmp-raw",
    "auth": { "kind": "basic", "user": "tmp-raw", "password": "<REAL_PASSWORD>" },
    "prefix": "<owner>/<project>",
    "listing_query": "<project>",
    "connect_timeout_secs": 10,
    "read_timeout_secs": 30,
    "poll_interval_secs": 10,
    "listing_style": "rest-search"
  },
  "client": { ... },
  "daemon": { ... }
}
```

---

## 5. Fetcher / Pusher（目标机侧）

Fetcher/pusher 作为 bifrost 自身的 **`bifrost fetcher` / `bifrost pusher`** 子命令（D2 已锁定）——用户可以选择一台机器起 `bifrost server` + `bifrost fetcher` + `bifrost pusher` 三进程，或者一台机器起 `bifrost server`+`bifrost fetcher` 而 pusher 由外部脚本承担（灵活）。

### Fetcher 行为

```
loop {
    let remote = http.list_pending();          // 一次 search 拿全部 path（q=listing_query，翻完分页），
                                               // 本地按 subdir 分组：commands/ cancel/（D8）
    let seen   = ledger.load();                // <shared_storage>/.fetcher-state/<task_id>（D9）
    for f in remote.commands where task_id(f) not in seen {
        match ledger.claim(task_id(f)) {       // ① 先占位：tmp + rename + fsync(dir)
            Ok(true)  => {}                    //    占位成功 → 继续投递
            Ok(false) => continue,             //    已占位（旧轮次）→ 跳过
            Err(e)    => { log::error!("claim {f} failed: {e}"); continue; }
                                               //    claim 失败 = 本轮跳过 + 告警，绝不写 commands/
        }
        let content = http.get(f);             // ② 先判 200：404 页带 body
        atomic_write(local_commands_dir + f, content);
        match http.delete(f) {
            ok(2xx) | 404 => {}
            Err(e)        => log::error!("delete {f} failed: {e}"),   // ③ 不静默，靠 ledger 兜
        }
        break;                                 // ⭐ V1 顺序模式：每轮最多投递 1 个（D11）
    }
    for f in remote.cancel {                   // 同一轮 search 的结果，不额外发请求
        let Ok(body) = http.get(f) else { continue };
        atomic_write(local_cancel_dir + f, body);
        let _ = http.delete(f);                // 写本地成功才清远端；失败只记 error
    }
    log::info!("pages={} matched={}", ...);    // 首轮额外打 backend/repo/prefix/listing_style
    sleep(poll_interval_secs);
}
```

顺序模式说明（**V1 不做并发提交**，见 D11）：

- client 一次只能 `submit` **一个**任务；`bifrost client submit` 会阻塞直到结果回来（或 `wait_timeout` 到期抛 `TaskLost`）
- fetcher 每轮**最多拉 1 个**新任务落本地（投递完成后 `break`），把节奏对齐到 daemon 实际执行能力，也天然避免多任务并发时 daemon/Protocol 侧出现并发竞争
- 顺序模式不改变下面的幂等要求：占位（claim）仍然要先于投递，`break` 之前必须先 claim 成功——否则「崩在写文件前」会变成重复执行
- 需要并发？V2 再议——前提是先跑通顺序模式，再决定要不要 manifest 合并 + 多 client 竞争协调那套（见 §7 YAGNI 注）

幂等性：**唯一依据是 ledger，不是本地文件名**。daemon 执行完会 `Protocol::remove_task` 删掉 `commands/<ts>_<id>.json`，`spawn_task` 末尾又删 `.processing`——任务终态后本地 `commands/` 什么都不剩。靠「本地有没有同名文件」判断，下一轮就会把同一任务再拉一遍、**再执行一次**（删除一旦不生效就是每个 poll 周期一次）。

**记账顺序必须是 write-ahead（先 claim 再投递），这是二轮评审的 P0**：`atomic_write → ledger.mark` 之间任一失败/崩溃，下一轮远端对象还在、ledger 没有 → 重新拉、重新写 `commands/`，而 daemon 可能已执行过 → **同一任务执行两次**。反向代价是「claim 成功但写文件前崩溃 → 任务丢失」，因此恢复路径是：`--once` 启动时扫 `.fetcher-state/*`，既没有对应 `commands/` 文件也没有 `results/` 的条目重新投递（或告警）。丢任务可告警，重复执行是静默错误——只能选前者。

ledger 实现要求：① 一条记录一个文件（`.fetcher-state/<task_id>`），`tmp+rename` 落盘，记录损坏会退化成「没记录」；② **单实例锁** `.fetcher-state/.lock`（flock）——两个 fetcher 同时跑 = 两个都判「没记录」= 重复执行；③ 定期裁剪（与 `results/` 对齐，保留 N 天），否则无限增长；④ 目录名带点，Protocol 只扫 `commands/results/status/cancel/artifacts` 五个子目录，放这里不会被 daemon 误读。

fetcher 首轮必须打一行日志：`backend=nexus repo=<repo> prefix=<prefix> listing=rest-search first_round=<N>` —— 否则外场分不清「fetcher 没起」和「没任务」（D7 不做 heartbeat 的补偿）。拉到 `cancel/<id>.json` 写进本地后，同样 DELETE 远端对象。

### Pusher 行为

对称：扫本地 `results/`、`status/`，本地出现的文件 PUT 到远端对应路径，**PUT 成功后删除本地**。差分依赖本地记账（每次 PUT 成功后立即删本地即是最简单的 ledger，**不需要远端 manifest**——顺序模式下同一时刻只有一个 client 和一个结果在等待，不会出现并发 merge 场景）。只搬 `results/`、`status/`、`cancel/`，不搬 `artifacts/`。

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
| 错误类型 | 复用 `ExecutionError` / `TaskNotFound` | 新增 `AuthError`（401/403）+ `TaskLost`（D10 到期未找到） |
| Heartbeat | `heartbeat_age_secs()` 跑 remote shell | HTTP bridge **不支持** health（**D7 决策：不做**。fetcher/pusher 本身就是轮询循环，活性看日志时间戳 / pid 就够，heartbeat 文件只会让 fetcher 多一个角色要处理）。**补偿**：fetcher 首轮打 `backend/repo/prefix/listing_style + 首轮条数` 一行日志 |
| `.processing` marker | 保留不删 | 同（fetcher 拉到本地后由 Protocol 管理） |
| 列目录 | `ls + grep`（`ssh_bridge.rs:170/291`） | REST search 翻完分页 + `path.starts_with("<prefix>/")` 过滤（D8/D13） |
| 幂等依据 | 远端文件仍在（无消费删除） | fetcher 自持 ledger `.fetcher-state/<task_id>`，**write-ahead**（claim → 写本地 → delete，D9） |

---

## 7. YAGNI 删除清单（明确不做）

- ❌ S3 签名（v4 / presigned URL）—— 未来加，进入 `listing_style="s3"` / `auth.kind="aws-sigv4"`
- ❌ Multipart upload（>100MB 分块）—— HTTP 链路只搬 `commands/results/status/cancel` 的 <1MB JSON；**`artifacts/` 不走这条链路**（`TaskResult.artifacts` 在 http 模式下不搬，要传日志/图片另走带宽通道；实测单流 26 MiB/s、4 并发 ≈111 MiB/s，是将来真要搬时的起点）
- ❌ TLS 客户端证书 / mTLS —— 内网 HTTP Basic 够
- ❌ 健康检查 (`heartbeat_age_secs`) —— **D7 决策：fetcher/pusher 活性看日志时间戳 / pid 就够**，不需要额外 heartbeat 文件
- ❌ 加密 —— 内网明文 Basic Auth + HTTPS 已够；要加密就走 VPN/mTLS
- ❌ HTTP redirect 跟随 —— Nexus 不跳；S3 确实可能跳，未来加
- ❌ **并发任务提交 / 批量 fetcher 拉取 / manifest 合并** —— **D11 决策：V1 顺序模式**，一次一个任务、fetcher 每轮最多 1 条。如果真要多任务并发，V2 再说，前提是顺序模式跑通且证明有需求

---

## 8. 测试计划（评审通过后写代码时同步落地）

### 单测（`src/core/http_bridge.rs` `#[cfg(test)]`）

复用 SSH bridge 的 `local: true` 测试模式——**起本地 mock HTTP server**（用 `axum` 或 `tiny_http` 在单测里 spawn），把 `base_url` 指向 `http://127.0.0.1:<port>`。**https 强制不给运行时放行分支**：生产构造器只认 `https://`，单测走 `#[cfg(test)] HttpBridge::new_unchecked(base_url)`（或注入 `Client`），否则生产路径多一个可被误用的绕过口。

1. `submit_task` 默认单次 PUT（无 `.tmp` 中间态）；开 `atomic_upload=true` 时按 `.tmp → .json` 顺序两次 PUT
2. `submit_task` + `read_task` round-trip
3. `query_status` 三态分支（Completed/Running/Pending）
4. `query_status` NotFound
5. `get_result` 解析 + status string 归一化
6. `remove_task` 语义：204/200 → 成功、404 → 成功（已不存在）、401 → `AuthError`、405 → warning 且不阻塞
7. Basic Auth header 正确编码
8. URL 拼接：`prefix` 边界（含斜杠 / 尾斜杠 / 重复斜杠）；**`prefix` 为空 → 配置期报错**（D12 必填）
9. 网络失败重试计数（用 mock 强制 500）
10. Auth 失败（401）立即失败
11. 404 响应**带 body + `Content-Length`** 时不被误判成「解析失败」（断言看状态码；**别把具体字节数写成常量**——实测同一仓库不同路径的 404 body 是 1531B / 1540B / 1582B 不等）
12. 目录 GET 返回 404 时 `listing_style=html` 的失败路径（报错，不是静默空列表）
13. 「任务已被消费、远端对象再次出现」→ 断言 ledger 命中、**不重复写入 commands/**
14. `prefix` 含 `/`、空格、`#` 时的 URL 编码（按段编码，`/` 不整体转义）
15. REST search 返回越界路径（`q` 分词误命中）时被本地前缀过滤掉
16. **分页**：mock 返回 3 页（每页 50 条 + `continuationToken`），目标对象只在第 3 页 → 必须被列出；不翻页的实现此用例失败
17. **`*.tmp` 过滤**：`atomic_upload=true` 留下的 `<file>.tmp` 不进 `commands/`
18. **前缀边界**：`prefix="team-lx/sess"` 时 `team-lx/session-evil/commands/x.json` **不得**命中
19. **ledger write-ahead**：`atomic_write` 前注入失败 → 远端对象不被消费、下一轮仍能拉到；`atomic_write` 后 `delete` 失败 → 不重复写 `commands/`
20. **配置校验**：`base_url` 不含 `/repository/<repository>/` → 解析期报错；`http://` → 报错
21. **`listing_style=none` + 到期** → `TaskLost`（不是永远 Pending）

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

## 9. 评审决策（2026-11-03 锁定 D1~D7；2026-11-06 实测复核后重写 D3、新增 D8/D9/D10；同日二轮评审新增 D13/D14）

| # | 决策 | 结论 | 备注 |
|---|---|---|---|
| **D1** | `atomic_upload` 默认值 | **false**（单次 PUT） | Nexus Raw 每个 PUT 落独立 blob、写完才可见，文件级原子性天然有；`.tmp → .json` 在 Raw 上不是 rename，只是两个 key，反而多一步 delete |
| **D2** | fetcher/pusher 归属 | **进 bifrost 主体（Rust 子命令）** | 复用 settings 解析 + transport 抽象 + 日志/重试；单一二进制部署；shell 脚本会引入第二套配置读取和错误处理 |
| **D3** | `remove_task` 在 HTTP 上的行为 | **DELETE 照发，结果不影响任务成败**（2026-11-06 重写） | 实测 Nexus raw **支持** DELETE：删存在对象 **204**、再删 **404**、匿名 **401** → 「405 静默通过」的事实前提不成立。正确映射：2xx/404 → 视为已不存在；401/403 → `AuthError`；405 → 后端真不支持（记 warning）。**清场失败不是错误，幂等由 D9 的 ledger 保证** |
| **D4** | 命名 `transport: "http"` | **保持** | 描述机制而非后端；S3 也是 HTTP，将来 `listing_style: "s3"` 正好嵌在 http 之下 |
| **D5** | `[http]` 段预留 `auth.kind = "none"` | **预留** | 保持方案原默认（内网开放 GET 场景） |
| **D6** | `password` 明文 + 文件 600 | **保持** | 保持方案原默认；与 ssh `password` 一致，不进 ENV |
| **D7** | heartbeat/health | **不做** | fetcher/pusher 本身是轮询循环，活性看日志时间戳 / pid 就够；heartbeat 文件意味着第三个角色要处理它，YAGNI |
| **D8** | fetcher 怎么列目录 | **Nexus REST search + 本地 path 前缀过滤 + 翻完分页**（2026-11-06 新增，二轮补分页/查询词） | 实测 raw 仓库 `GET <dir>/` 返回 **404**（`text/html`，约 1.5KB 错误页，即使目录内有对象）→ `listing_style=html` 不可用，默认改 `rest-search`。接口 `GET /service/rest/v1/search/assets?repository=<repo>&q=<listing_query>` 匿读 **200**、**50 条/页 + `continuationToken`**、写完才可见。两条硬约束：① `q` 是分词 OR 匹配（实测把无关的 `materials/.../cni_bin/bridge` 一起返回）→ 必须本地 `path.starts_with("<prefix>/")` 过滤，否则会执行别人的任务；② 前缀对象可能落在第 2 页之后（实测整条路径作 q：3 页 112 条里只有 8 条属前缀）→ **必须翻到 token 缺失**，`q` 改用 `listing_query`（默认 prefix 最后一段，实测 8 条/1 页），页数上限 20 页，并丢 `*.tmp`/非 `.json` |
| **D9** | fetcher 幂等依据 | **自持 ledger，write-ahead 顺序**：`claim → 写本地 → DELETE 远端`（2026-11-06 新增，二轮改顺序） | daemon 消费完会删 `commands/<ts>_<id>.json`、`.processing` 也被删——本地「没有同名文件」不代表没执行过。靠文件名匹配会**每个 poll 周期重复执行一次**。二轮结论：`atomic_write → ledger.mark` 之间崩溃/磁盘满会**再执行一次**（远端对象还在、ledger 没记），所以必须**先 claim（tmp+rename+fsync）再投递**，claim 失败本轮跳过并告警；claim 后崩溃的恢复路径是 `--once` 启动时扫 `.fetcher-state/*` 与本地 `commands/`、`results/` 对账重投。另需单实例 flock、定期裁剪。DELETE 失败只记 error，不静默 |
| **D10** | G5 的到期语义 | **`wait_timeout` 到期抛 `TaskLost`**（2026-11-06 新增） | 「被运维清掉」和「还在排队」不再靠猜：到期时 search 查 `results/status/commands` 三处，都无 → `TaskLost`（新增错误变体，进 `core/error.rs`）；只有 search 自身失败才继续报 Pending。message 带三级探测结果——`commands` 仍在 = 「没人消费/fetcher 没起」，不是「丢了」 |
| **D11** | 并发模型 | **V1 顺序模式**：client 一次只提交一个任务并阻塞等待该结果；fetcher 每轮最多拉 1 个 task；`poll_interval_secs` 默认 10s（2026-11-06 新增，用户拍板） | 现阶段允许「一次提交、等执行结果、顺序执行」。**省掉多套机制**：不需要 manifest 合并、不需要多 client 竞争协调、fetcher 不需要并发下载、client 不需要任务队列。需要并发？V2 再议——前提是顺序模式已跑通 |
| **D12** | 双方契约 | **`prefix` 必填**，用 `<owner>/<project>` 稳定前缀；client/daemon 双方抄同一份 settings（2026-11-06 新增，用户拍板） | 上传点 `commands/`、结果点 `results/`/`status/`、发现点（fetcher 用 REST search 在 `<prefix>/commands/`）都由同一份 `[http]` 配置唯一决定；没有同一份配置，两端无法协作。详见 §4.6 |
| **D13** | 前缀与仓库一致性 | **前缀归一化为 `"<prefix>/"`；`base_url` 必须含 `/repository/<repository>/`**（2026-11-06 二轮新增） | `starts_with("team-lx/sess")` 会命中 `team-lx/session-evil/...` → 跨前缀拉别人的任务执行（安全事故）。过滤/拼接前 trim + 补尾 `/`；URL 编码按段做（`/` 是分隔符，不整体转义）。`repository` 与 `base_url` 不一致必须在配置期报错，否则 search 查 A 库、PUT 进 B 库，症状像「fetcher 没起」 |
| **D14** | `listing_style="none"` 的语义 | **没有第三级兜底 → 到期直接 `TaskLost`**（2026-11-06 二轮新增） | D10 的前提是「能 search」；`none` 时 `query_status` 只能查 results/status 两个对象，到期后若不抛 `TaskLost` 就永远 Pending，正好违背 G5。要么配置期拒绝 `none` + client 查询的组合，要么到期即 `TaskLost` |

---

## 10. 时序（评审通过后）

```
Day 1  settings schema（含 repository / listing_query / 两段超时 / **prefix 必填**（D12）/ base_url↔repository 一致性（D13））+ transport.is_http 分支 + `AuthError`/`TaskLost` 枚举 + 占位 HttpBridge（全部 Err）+ 框架单测
Day 2  HTTP client 封装（**reqwest::blocking**，见 §4.2 注）+ submit_task/read_task 实现 + mock server 单测
Day 3  query_status/get_result/write_*/remove_*（2xx 全谱 + 404 body 陷阱）+ 单测补全
Day 4  fetcher 子命令（REST search listing **翻页** + write-ahead ledger + **每轮只投递 1 个的顺序节流**，见 D8/D9/D11）+ 对接真 Nexus tmp-raw 拉通
Day 5  pusher 子命令（PUT 成功即删本地，无 manifest，见 D11）+ E2E host→Nexus→target→result 完整链路 + 文档/README/AGENTS.md 更新
Day 6  review-review，merge 到 main
```

总工作量 ~3-4 天（含测试与文档）。**D8/D9/D11/D12/D13/D14 必须在 Day 1 前落进 schema 和 fetcher 设计**——否则 Day 4 的 E2E 会卡在「fetcher 拉不到任务」或「prefix 不匹配两端对不上」上。

---

## 附录 A. 后端能力实测记录（2026-11-06，Nexus `tmp-raw`）

客户端本机 curl；目标 `https://nexus.zs.shaipower.online`（= 10.9.190.129，`/etc/hosts` 手配）；探测前缀 `review-probe/hermes-http-bridge/`（探测后已 DELETE，search 复核 0 残留）。

| 探测 | 结果 | 结论 |
|---|---|---|
| `GET <base>/commands/`（目录内有对象） | **404**，`text/html`，body ≈1.5KB | 目录 listing 不存在（D8）；**body 大小随路径变化（实测 1531 / 1540 / 1582B），不要写成断言常量** |
| `GET /service/rest/v1/search/assets?repository=tmp-raw&q=<prefix>` | **200**，匿读可用；返回本前缀对象 + 1 条无关 `materials/.../cni_bin/bridge` | 可当 listing，但必须本地前缀过滤 |
| `PUT` 新对象（含认证） | **201** | 2xx 全谱判定 |
| `PUT` 覆盖同路径 | **201**（不是 204） | 同上 |
| 匿名 `GET` 已存在对象 / 匿名 `PUT` / 匿名 `DELETE` | 200 / **401** / **401** | 读可匿名，写必须账号 |
| `DELETE` 存在对象 / 再删一次 / 匿名删 | **204** / **404** / 401 | D3 前提推翻 |
| `HEAD` 存在对象 | 200 + 正确 `content-length` | 可做大小校验 |
| `HEAD` 不存在对象 | 404 + `content-length: 1601`（该路径实测，尺寸随路径变化） | 404 也带 body，判存在先看状态码 |
| `Range: bytes=0-3` | **206** | 断点续传可用 |
| 单流吞吐 / 4 并发 PUT | 26 MiB/s / ≈111 MiB/s | 只有大对象/artifacts 才需要并发 |

认证事实核对：示例里的 `user: "tmp-raw"` 是**正确的账号名**（匿名 PUT 401、带它 201），不存在「拿 repo 名当账号」的问题。

---

## 附录 B. 二轮评审补充探测（2026-11-06，全部只读/匿名）

目的：验证第一轮修法落进设计后是否**仍然**成立，重点查 listing 的边界条件。探测前缀沿用 `materials/deploy-tools/os-init`（仓库里已存在、含 8 个对象）作为「有已知对象的目录」样本。

| 探测 | 结果 | 结论 |
|---|---|---|
| `GET /service/rest/v1/search/assets?repository=tmp-raw`（无 q） | 200，50 条/页 + `continuationToken`（6 页取到 300 条仍未穷尽） | 全库列举不可行，必须靠 `q` 收窄 |
| `?q=materials/deploy-tools/os-init`（整条路径） | 3 页共 **112** 条，其中 `starts_with("materials/deploy-tools/os-init/")` 仅 **8** 条 | 通用 token（`materials`）把无关对象全拉进来 → 白翻页 |
| `?q=os-init`（唯一 token） | 1 页 **8** 条，全部命中 | `listing_query` 取最独特段的有效性依据 |
| 目标对象是否总在第 1 页 | 否（上述 112 条场景里目标分散在 3 页） | **不翻完分页 = 静默丢任务**（D8 第 4 条） |
| `GET /service/rest/v1/assets?repository=tmp-raw[&q=...]` | 200，但**只有 10 条/页** | 可列，比 search 差 5 倍，不做主路径 |
| `GET /service/rest/v1/components?repository=tmp-raw` | 200，13.8KB/页 | 对 raw 仓库无额外价值 |
| `GET /repository/tmp-raw/uploads/`（目录） | **404**，1540B | 复核第一轮结论（D8 前提成立） |
| `GET /repository/tmp-raw/uploads/nope-404-probe` | **404**，1531B，带 `content-length` | 404 也带 body，判存在先看状态码 |
| `GET /repository/tmp-raw/uploads/bundles/B200-PREP-20261105.md` | 200，9895B，`text/x-web-markdown` | 匿名读正常 |

二轮改动的设计点：D9 改 write-ahead（P0）、D8 补「翻完分页 + `listing_query` + 丢 `*.tmp` + pages/matched 日志」（P0）、新增 D13（前缀归一化 + repository/base_url 一致性）、D14（`listing_style=none` 到期即 `TaskLost`）、D10 的 message 带三级探测结果；测试计划相应补 16~21 条。
