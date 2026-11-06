# HTTP Bridge 设计方案（feature/http-bridge）

> 设计稿（2026-11-03 评审通过，D1~D7 已锁定；**2026-11-06 按真实 Nexus tmp-raw 逐条实测复核：D3 重写，新增 D8/D9/D10**；同日落实两条用户新约束：**D11 顺序模式 + D12 双方契约**，lock 见 §9/§10）。目标是在不重写 daemon 的前提下，让 bifrost 通过「裸 HTTP PUT/GET」对接内网制品/对象存储（第一个落地目标:**Nexus Raw Hosted Repo**，如 `tmp-raw`）。
>
> **评审链（2026-11-06 用户明确定）**：`feature/http-bridge @ 769bf82` 由 **@Ax-vLLM适配 review**，再到 **@刘鑫 终审**；两关都通过之后才允许开始 Day 1。当前状态：**Ax review 中**，刘鑫已先行 gate-open 让我（AI Engineer）开工 Day 1，但若 Ax 提出骨架级修改实施立即暂停回到 design 阶段。本设计分支**不 merge 到任何 release 分支**（release/0.1.1 等）——它只承载设计文档，后续 merge 对象与时机由最终评审结论决定。

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
| `http.repository` | `String` | listing_style=`rest-search` 必填 | Nexus 仓库名（如 `tmp-raw`）。search API 在 `/service/rest/v1/` 下，**从 `base_url` 推不出 repo 名**，必须单独给 |
| `http.listing_style` | `"rest-search"` / `"html"` / `"none"` | ❌ 默认 `"rest-search"` | fetcher 怎么列目录：`rest-search` = Nexus REST search（实测可用）；`html` = GET 目录解析 `<a href>`（**Nexus raw 实测 404，只留给 nginx autoindex 类后端**）；未来 S3 兼容再加 `"s3"` / `"json"` |

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

### 4.2 `Bridge for HttpBridge` 各方法实现要点

| 方法 | 实现 | 关键约束 |
|---|---|---|
| `submit_task` | 单次 `PUT <base>/commands/<task_file>`（默认，`atomic_upload=false`）。开 `atomic_upload=true` 则 `PUT <task_file>.tmp` → `PUT <task_file>`（两次 PUT，第二次作 rename 替代） | Nexus Raw 单 PUT 落独立 blob、写完才可见，原子性天然有（见 D1）；PUT 新对象实测 **201**、覆盖同路径也 **201** → 判定一律 `status().is_success()`，别写 `== 200` |
| `query_status` | GET `<base>/results/<id>_result.json`；缺则 GET `<base>/status/<id>.json`；缺则 REST search 在 `<prefix>/commands/` 下查 `<id>`（D8）兜底 Pending | 与 SshBridge 同优先级；三级都查不到且 `wait_timeout` 已到期 → `TaskLost`（D10） |
| `get_result` | GET + parse | **先看 `status == 200` 再读 body**——Nexus 的 404 页带 1601B body 和 `Content-Length`，靠长度判存在会把「不存在」当成「存在但解析失败」；404 → `TaskNotFound`；其他 4xx/5xx → `ExecutionError` |
| `list_pending_tasks` | Nexus REST search：`GET /service/rest/v1/search/assets?repository=<repo>&q=<prefix>/commands&continuationToken=...`，取 `items[].path`，**本地 `path.starts_with(prefix)` 二次过滤**（D8） | 仅 fetcher 用；client 不会调。`q` 是分词 OR 匹配，**不过滤会把别人的任务拉下来执行** |
| `read_task` | 先 search 列出 `commands/` 前缀，再按 `<id>` 匹配对象 GET | 无 listing 时不能靠 HEAD 猜文件名：命令文件名带 `<ts>_` 前缀，光有 id 拼不出来 |
| `write_result` / `write_status` | PUT `<subdir>/<filename>` | 仅 pusher 用；client 不会调 |
| `remove_task` | `DELETE <base>/.../<file>` | 2xx / 404 → 视为已不存在（成功）；401/403 → `AuthError`；405 → 后端不支持删除，记 warning 后依赖 ledger（D3/D9） |
| `cancel_task` | PUT `<base>/cancel/<id>.json` | 与 SshBridge 一致；fetcher 拉到本地后**要 DELETE 远端 cancel 对象**（`Protocol::clear_cancel` 只清本地，否则远端永久堆积） |

> **同步还是异步：用 `reqwest::blocking`（Day 2 前定死）**。`Bridge` trait 全是同步方法（`core/bridge.rs`），SshBridge 也是阻塞实现，按对称性选 blocking；**禁止在 tokio runtime 内调用**。HTTP bridge 只被 client CLI / fetcher / pusher 使用，daemon 永不选 http（G1），所以这个约束成立。

### 4.3 原子性策略（D1 已锁定）

**默认单次 PUT，不开 `.tmp` 两步法。**

依据（2026-11-03 评审结论）：

- Nexus Raw 每个 PUT 落一个独立 blob，**写完才对 client 可见**，文件级原子性天然有
- 「`.tmp` → `.json`」在 Raw 上根本不是 rename，只是**两个不同 key 的文件**，反而要多一步 delete
- status/results JSON 都很小，不存在「大文件传半截」的场景

`atomic_upload` 保留为 opt-in 开关，默认 false。开 true 才走 2-PUT 流程给需要的后端用（比如未来某个不支持单 PUT 原子的对象存储）。

### 4.4 Listing 协议（关键扩展点）

`listing_style` 是未来加 S3 后端的扩展口。**2026-11-06 实测推翻了「Nexus Raw 支持 GET 目录返回 HTML」这个前提**：目录里明明有对象，`GET <base>/commands/` 仍返回 **404**（`text/html`，1582B 错误页）。所以默认改成 REST search：

| style | 实现 | 适配 |
|---|---|---|
| `rest-search` | `GET /service/rest/v1/search/assets?repository=<repo>&q=<prefix>`，读 `items[].path`，`continuationToken` 分页 | Nexus Raw ✅ **默认**（实测匿读 200） |
| `html` | GET 目录，正则 `<a href="...">` 提取文件名 | ❌ Nexus raw 实测 404；只留给 nginx autoindex 类后端 |
| `none` | 不支持 listing → `list_pending_tasks` 报错（fetcher 不可用） | 只 client→server 单向场景 |
| （未来）`s3` | `<base>?list-type=2&prefix=...` XML 解析 | MinIO/AWS S3 |
| （未来）`oss-json` | OSS ListObjects JSON | oss://apmm |

用 REST search 当 listing 的三条硬约束（都实测过）：

1. **`q` 是分词 OR 匹配，必须本地再过滤**：`q=review-probe/hermes-http-bridge` 除了本前缀对象，还返回了一条无关的 `materials/.../cni_bin/bridge`。因此拿到 `items[].path` 后必须 `path.starts_with("<prefix>/commands/")`，否则会**把别人的任务拉到本地执行**（安全事故，不是性能问题）。
2. **对象写完才出现**：语义上比目录 listing 更安全，天然等价于「完整可见」——不会读到半截文件。
3. **repo 名推不出来**：search API 在 `/service/rest/v1/` 下，`base_url` 里只有 `/repository/<repo>/...` 路径，所以 schema 必须单列 `repository` 字段。

### 4.5 错误分类与重试

| 类别 | 处理 |
|---|---|
| 网络 / 5xx | 重试 3 次（指数退避 1s → 2s → 4s），抛 `ExecutionError` |
| 超时 | 拆两段：`connect_timeout_secs` 建连、`read_timeout_secs` inactivity。**别用一个总超时**——单流实测 26 MiB/s，30s 总超时等于给大对象判 780MB 死刑 |
| 2xx（含 201/204） | 一律成功（`status().is_success()`）。实测 PUT 新对象/覆盖都是 **201**、DELETE 是 **204** |
| 404 | 读操作 → `TaskNotFound`；DELETE → 已不存在（成功）；**判存在必须先看状态码**（404 页也带 body） |
| 401/403 | 立即抛 `AuthError`（settings 填错时早暴露）—— **新错误枚举值需要加进 `core/error.rs`** |
| 405 (Method Not Allowed) | 只在 DELETE 上有意义：后端真不支持删除 → 记 warning，幂等靠 ledger（D3/D9）。**不是「静默当成功」** |
| `TaskLost` | `wait_timeout` 到期、且 search 在 `results/status/commands` 三处都查不到 → 抛 `TaskLost`（新增变体，D10），别继续报 Pending |

### 4.6 双方契约（2026-11-06 新增，D12）

部署 HTTP Bridge 时，**client 侧和 daemon 侧必须使用同一份 `[http]` 段配置**——这是双方唯一的契约。四件事必须同时成立：

| # | 双方都要知道 | 约定 |
|---|---|---|
| 1 | **client 往哪里 PUT 任务** | `<base_url>/<prefix>/commands/<ts>_<task_id>.json`（每次新增，不覆盖） |
| 2 | **client 从哪里拉结果 / 状态** | `<base_url>/<prefix>/results/<task_id>_result.json` 与 `<base_url>/<prefix>/status/<task_id>.json` |
| 3 | **fetcher 怎么发现任务** | REST search 在 `<prefix>/commands/` 下查，本地 `path.starts_with(prefix)` 二次过滤（D8），拉到后写 `<shared_storage>/commands/` 同时 DELETE 远端 |
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
    let remote = http.list_pending();          // REST search + path 前缀过滤（§4.2 list_pending_tasks，D8）
    let seen   = ledger.load();                // <shared_storage>/.fetcher-state/<task_id>（D9）
    let first  = remote.iter().find(|f| !seen.contains(task_id(f)));
    if let Some(f) = first {                   // ⭐ V1 顺序模式：每轮最多拿 1 个（D11）
        let content = http.get(f);             // 先判 200：404 页带 body
        atomic_write(local_commands_dir + f, content);
        ledger.mark(task_id(f));               // 先落 ledger，再动远端
        match http.delete(f) {
            ok(2xx) | 404 => {}
            Err(e)        => log::error!("delete {f} failed: {e}"),   // 不静默，靠 ledger 兜
        }
    }
    sleep(poll_interval_secs);
}
```

顺序模式说明（**V1 不做并发提交**，见 D11）：

- client 一次只能 `submit` **一个**任务；`bifrost client submit` 会阻塞直到结果回来（或 `wait_timeout` 到期抛 `TaskLost`）
- fetcher 每轮**最多拉 1 个**新任务落本地（`first` + `find`），把节奏对齐到 daemon 实际执行能力，也天然避免多任务并发时 daemon/Protocol 侧出现并发竞争
- 需要并发？V2 再议——前提是先跑通顺序模式，再决定要不要 manifest 合并 + 多 client 竞争协调那套（见 §7 YAGNI 注）

幂等性：**唯一依据是 ledger，不是本地文件名**。daemon 执行完会 `Protocol::remove_task` 删掉 `commands/<ts>_<id>.json`，`spawn_task` 末尾又删 `.processing`——任务终态后本地 `commands/` 什么都不剩。靠「本地有没有同名文件」判断，下一轮就会把同一任务再拉一遍、**再执行一次**（删除一旦不生效就是每个 poll 周期一次）。ledger 与远端对象做差，删除失败只影响清场、不影响幂等。

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
| 列目录 | `ls + grep`（`ssh_bridge.rs:170/291`） | REST search + `path.starts_with` 过滤（D8） |
| 幂等依据 | 远端文件仍在（无消费删除） | fetcher 自持 ledger `.fetcher-state/<task_id>`（D9） |

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

复用 SSH bridge 的 `local: true` 测试模式——**起本地 mock HTTP server**（用 `axum` 或 `tiny_http` 在单测里 spawn），把 `base_url` 指向 `http://127.0.0.1:<port>`（https 强制校验对 loopback 放行，仅单测用）。覆盖：

1. `submit_task` 默认单次 PUT（无 `.tmp` 中间态）；开 `atomic_upload=true` 时按 `.tmp → .json` 顺序两次 PUT
2. `submit_task` + `read_task` round-trip
3. `query_status` 三态分支（Completed/Running/Pending）
4. `query_status` NotFound
5. `get_result` 解析 + status string 归一化
6. `remove_task` 语义：204/200 → 成功、404 → 成功（已不存在）、401 → `AuthError`、405 → warning 且不阻塞
7. Basic Auth header 正确编码
8. URL 拼接：`prefix` 边界（空 / 有 / 含斜杠）
9. 网络失败重试计数（用 mock 强制 500）
10. Auth 失败（401）立即失败
11. 404 响应**带 body + `Content-Length`** 时不被误判成「解析失败」
12. 目录 GET 返回 404 时 `listing_style=html` 的失败路径（报错，不是静默空列表）
13. 「任务已被消费、远端对象再次出现」→ 断言 ledger 命中、**不重复写入 commands/**
14. `prefix` 含 `/`、空格、`#` 时的 URL 编码
15. REST search 返回越界路径（`q` 分词误命中）时被本地前缀过滤掉

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

## 9. 评审决策（2026-11-03 锁定 D1~D7；2026-11-06 实测复核后重写 D3、新增 D8~D12）

| # | 决策 | 结论 | 备注 |
|---|---|---|---|
| **D1** | `atomic_upload` 默认值 | **false**（单次 PUT） | Nexus Raw 每个 PUT 落独立 blob、写完才可见，文件级原子性天然有；`.tmp → .json` 在 Raw 上不是 rename，只是两个 key，反而多一步 delete |
| **D2** | fetcher/pusher 归属 | **进 bifrost 主体（Rust 子命令）** | 复用 settings 解析 + transport 抽象 + 日志/重试；单一二进制部署；shell 脚本会引入第二套配置读取和错误处理 |
| **D3** | `remove_task` 在 HTTP 上的行为 | **DELETE 照发，结果不影响任务成败**（2026-11-06 重写） | 实测 Nexus raw **支持** DELETE：删存在对象 **204**、再删 **404**、匿名 **401** → 「405 静默通过」的事实前提不成立。正确映射：2xx/404 → 视为已不存在；401/403 → `AuthError`；405 → 后端真不支持（记 warning）。**清场失败不是错误，幂等由 D9 的 ledger 保证** |
| **D4** | 命名 `transport: "http"` | **保持** | 描述机制而非后端；S3 也是 HTTP，将来 `listing_style: "s3"` 正好嵌在 http 之下 |
| **D5** | `[http]` 段预留 `auth.kind = "none"` | **预留** | 保持方案原默认（内网开放 GET 场景） |
| **D6** | `password` 明文 + 文件 600 | **保持** | 保持方案原默认；与 ssh `password` 一致，不进 ENV |
| **D7** | heartbeat/health | **不做** | fetcher/pusher 本身是轮询循环，活性看日志时间戳 / pid 就够；heartbeat 文件意味着第三个角色要处理它，YAGNI |
| **D8** | fetcher 怎么列目录 | **Nexus REST search + 本地 path 前缀过滤**（2026-11-06 新增） | 实测 raw 仓库 `GET <dir>/` 返回 **404**（`text/html`，1582B 错误页，即使目录内有对象）→ `listing_style=html` 不可用，默认改 `rest-search`。接口 `GET /service/rest/v1/search/assets?repository=<repo>&q=<prefix>` 匿读 **200**、`continuationToken` 分页、写完才可见；但 **`q` 是分词 OR 匹配**（实测把无关的 `materials/.../cni_bin/bridge` 一起返回）→ 必须本地 `path.starts_with(prefix)` 二次过滤，否则会执行别人的任务。schema 相应新增 `repository` 字段 |
| **D9** | fetcher 幂等依据 | **自持 ledger**（`<shared_storage>/.fetcher-state/<task_id>`），删除失败不静默（2026-11-06 新增） | daemon 消费完会删 `commands/<ts>_<id>.json`、`.processing` 也被删——本地「没有同名文件」不代表没执行过。靠文件名匹配会**每个 poll 周期重复执行一次**。顺序：写本地 → 记 ledger → DELETE 远端；DELETE 失败只记 error log，不许静默 |
| **D10** | G5 的到期语义 | **`wait_timeout` 到期抛 `TaskLost`**（2026-11-06 新增） | 「被运维清掉」和「还在排队」不再靠猜：到期时 search 查 `results/status/commands` 三处，都无 → `TaskLost`（新增错误变体，进 `core/error.rs`）；只有 search 自身失败才继续报 Pending |
| **D11** | 并发模型 | **V1 顺序模式**：client 一次只提交一个任务并阻塞等待该结果；fetcher 每轮最多拉 1 个 task；`poll_interval_secs` 默认 10s（2026-11-06 新增，用户拍板） | 现阶段允许「一次提交、等执行结果、顺序执行」。**省掉多套机制**：不需要 manifest 合并、不需要多 client 竞争协调、fetcher 不需要并发下载、client 不需要任务队列。需要并发？V2 再议——前提是顺序模式已跑通 |
| **D12** | 双方契约 | **`prefix` 必填**，用 `<owner>/<project>` 稳定前缀；client/daemon 双方抄同一份 settings（2026-11-06 新增，用户拍板） | 上传点 `commands/`、结果点 `results/`/`status/`、发现点（fetcher 用 REST search 在 `<prefix>/commands/`）都由同一份 `[http]` 配置唯一决定；没有同一份配置，两端无法协作。详见 §4.6 |

---

## 10. 时序（评审通过后）

```
Day 1  settings schema（含 repository / 两段超时 / **prefix 必填** 校验，见 D12）+ transport.is_http 分支 + `AuthError`/`TaskLost` 枚举 + 占位 HttpBridge（全部 Err）+ 框架单测
Day 2  HTTP client 封装（**reqwest::blocking**，见 §4.2 注）+ submit_task/read_task 实现 + mock server 单测
Day 3  query_status/get_result/write_*/remove_*（2xx 全谱 + 404 body 陷阱）+ 单测补全
Day 4  fetcher 子命令（REST search listing + ledger + **首轮只拉 1 个的顺序节流**，见 D11）+ 对接真 Nexus tmp-raw 拉通
Day 5  pusher 子命令（PUT 成功即删本地，无 manifest，见 D11）+ E2E host→Nexus→target→result 完整链路 + 文档/README/AGENTS.md 更新
Day 6  review-review，merge 到 main
```

总工作量 ~3-4 天（含测试与文档）。**D8/D9/D11/D12 必须在 Day 1 前落进 schema 和 fetcher 设计**——否则 Day 4 的 E2E 会卡在「fetcher 拉不到任务」或「prefix 不匹配两端对不上」上。

---

## 附录 A. 后端能力实测记录（2026-11-06，Nexus `tmp-raw`）

客户端本机 curl；目标 `https://nexus.zs.shaipower.online`（= 10.9.190.129，`/etc/hosts` 手配）；探测前缀 `review-probe/hermes-http-bridge/`（探测后已 DELETE，search 复核 0 残留）。

| 探测 | 结果 | 结论 |
|---|---|---|
| `GET <base>/commands/`（目录内有对象） | **404**，`text/html`，body 1582B | 目录 listing 不存在（D8） |
| `GET /service/rest/v1/search/assets?repository=tmp-raw&q=<prefix>` | **200**，匿读可用；返回本前缀对象 + 1 条无关 `materials/.../cni_bin/bridge` | 可当 listing，但必须本地前缀过滤 |
| `PUT` 新对象（含认证） | **201** | 2xx 全谱判定 |
| `PUT` 覆盖同路径 | **201**（不是 204） | 同上 |
| 匿名 `GET` 已存在对象 / 匿名 `PUT` / 匿名 `DELETE` | 200 / **401** / **401** | 读可匿名，写必须账号 |
| `DELETE` 存在对象 / 再删一次 / 匿名删 | **204** / **404** / 401 | D3 前提推翻 |
| `HEAD` 存在对象 | 200 + 正确 `content-length` | 可做大小校验 |
| `HEAD` 不存在对象 | 404 + `content-length: 1601` | 404 也带 body，判存在先看状态码 |
| `Range: bytes=0-3` | **206** | 断点续传可用 |
| 单流吞吐 / 4 并发 PUT | 26 MiB/s / ≈111 MiB/s | 只有大对象/artifacts 才需要并发 |

认证事实核对：示例里的 `user: "tmp-raw"` 是**正确的账号名**（匿名 PUT 401、带它 201），不存在「拿 repo 名当账号」的问题。
