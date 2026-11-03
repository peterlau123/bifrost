# HTTP transport (`transport="http"`)

> 对应内部 Nexus raw hosted 仓库（`tmp-raw`），用于那些**与 GPFS 不通**的节点
> （例如 Flexible-Agent 海光机）作为 client/daemon 之间的离线文件中转。
>
> 设计决策与代码评审记录：见 `feat/cloud-storage` 分支 PR 讨论（2026-11-03，
> @LLM software architect × @刘鑫）。

## 1. 是什么

`transport=http` 时，bifrost **不实现新的 Bridge trait，也不替换 daemon**：
这两端继续用 `Protocol` 操作本地 `shared_storage`，但多了两个一步之遥的
**同步工具**把本地文件搬到远端 Nexus raw、把远端文件拉回本地。

```
┌──────────┐  Protocol ┌────────────┐  curl  ┌──────────────┐  curl  ┌──────────┐  Protocol ┌─────────┐
│  client  │─ commands→│  client 盘 │───────→│  Nexus raw   │←───────│ daemon 盘 │─────────→│ daemon  │
│          │←─results/ │  bifrost   │←───────│  tmp-raw     │───────→│  bifrost │←─────────│         │
│  submit  │   status  │  pusher    │        │              │        │  fetcher │  exec    │         │
└──────────┘           └────────────┘        └──────────────┘        └──────────┘          └─────────┘
```

## 2. 为什么不用 Nexus 的 listing API

Nexus raw hosted 仓库**只有 PUT/GET/DELETE，没有列目录能力**（tmp-raw 账号按
SRE 政策是 PUT/GET/DELETE 最小权限），所以 `Bridge::list_pending_tasks` 没法
表达成 HTTP。我们绕开它：client 每次 submit 时把一个任务文件 PUT 到
`commands/` 并追加到 `commands_manifest.txt`，daemon 侧的 fetcher 只按
manifest 拉，拉完从 manifest 剔除。

**含义**：同一时间窗 client 数 **必须为 1**（同一时间只能有一个 client 在提交，
否则 manifest 会被互相覆盖）。V1 先这样；要支持多 client 的话，方向是给每个
client 一个独立的 manifest 前缀（`manifest-<client_id>.txt`），这个先不做。

## 3. 怎么用

### 3.1 配置

`~/.bifrost/settings.json`（client 和 daemon 都用同一份模板）：

```json
{
  "shared_storage": "/local/path/on/this/machine",
  "transport": "http",
  "http": {
    "base_url": "https://nexus.zs.shaipower.online/repository/tmp-raw/uploads/bifrost",
    "poll_interval": "5s",
    "request_timeout": "30s",
    "atomic_upload": false
  }
}
```

| 字段 | 必填 | 默认 | 说明 |
|---|---|---|---|
| `base_url` | ✓ | — | Nexus raw 仓库前缀（**不要以 `/` 结尾**） |
| `user` | — | env `BIFROST_HTTP_USER` | HTTP basic auth 用户名 |
| `password` | — | env `BIFROST_HTTP_PASSWORD` | **严禁把真实密码写进 settings.json**；用 env |
| `poll_interval` | — | `5s` | daemon 侧轮询远端间隔 |
| `request_timeout` | — | `30s` | 单次 curl 超时 |
| `atomic_upload` | — | `false` | D1：Nexus PUT 本身原子，status/results JSON 小，单次 PUT 即可 |

### 3.2 起 sync

两端各跑一个 `bifrost sync`（fetch+push 双向），永远同步两边：

```bash
# client 侧（如 Flexible-Agent 海光机）
BIFROST_HTTP_USER=tmp-raw \
BIFROST_HTTP_PASSWORD=*** \
bifrost sync --storage /home/liuxin/bifrost-relay &

# daemon 侧（mx001/mx002）
BIFROST_HTTP_USER=tmp-raw \
BIFROST_HTTP_PASSWORD=*** \
bifrost sync --storage /data/liuxin/bifrost &

# daemon 照旧起（本地 Protocol + inotify）
bifrost server --storage /data/liuxin/bifrost
```

> 也可以拆开跑 `bifrost fetcher` / `bifrost pusher`，方便单独给一侧限频；
> 但 V1 推荐 `bifrost sync` —— 一条命令，两侧对称，不易配错。

### 3.3 任务流程

1. `bifrost client submit --command 'hostname'` → 本地 `commands/<ts>_<id>.json`
2. client 侧 `bifrost sync` 把它推到远端，并追加进 `commands_manifest.txt`
3. daemon 侧 `bifrost sync` 拉到本地 `commands/`
4. daemon 的 `bifrost server` 看到新任务 → 执行 → 写本地 `results/`、`status/`
5. daemon 侧 `bifrost sync` 推远端
6. client 侧 `bifrost sync` 拉回 `results/`、`status/`
7. `bifrost client status <id>` / `bifrost client result <id>` 照常工作

## 4. 限制与注意

- **单 client 假设**：见 §2。两条独立的 client 同时 `submit` 会把
  `commands_manifest.txt` 写飞，需要后面加 per-client manifest。
- **临时性**：tmp-raw 是 SRE 明确的"随用随删"通道，对象随时可能被外部清掉。
  bifrost 已经按"404 = 任务不在远端 = 已被清理"处理，但**不要把 Nexus 当作
  任务归档**；结果只在那保留到 client 下一次 sync 拉走。
- **没有 cancel relay**：V1 不支持 `bifrost client cancel` 跨 HTTP transport —
  cancel 文件还是写在 `cancel/` 子目录下，但 fetcher V1 只拉 `commands/`。
  需要的话把 `commands_manifest.txt` 复制一份改成 `cancel_manifest.txt` 就行，
  架构相同。
- **不要在 settings.json 写密码**：如果非写不可，至少 `chmod 600` 并且**别
  提交进 git**。推荐是唯一方式：env 变量。
- **本仓库的 GitHub Actions 用 ubuntu-latest** 自带 curl，无需额外装；
  离线环境 (`mx001`/`mx002`/海光机) 装 `curl` 即可，无其它依赖。

## 5. 验证过的场景

- `cargo build` / `cargo test`：**221 tests passed**（feat/cloud-storage@later）
- `./bifrost --help` / `./bifrost sync --help` 正常打印
- 无 `[http]` 段时 `bifrost sync --once` 报 `error: settings has no [http] section`
- 假 base_url 时 curl 错误被正确捕获并 Eprintln，进程不 crash

后续：在 mx001 上，与 Flexible-Agent 海光机做一次端到端 PING。
