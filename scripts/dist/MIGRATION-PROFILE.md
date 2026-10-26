# 用 hermes profile export / import 迁移（逐步操作）

> 生成时间 2026-10-26 09:45Z。本文件描述的是**你已经选定的路线**：`hermes profile export` → `import`。
> 它搬的是「agent 人设」：`config.yaml`、`SOUL.md`、`memories/`、`skills/`、`cron/`、`plugins/`。
> **不搬**：`.env`（API keys）、`auth.json`（OAuth/凭据池）、`state.db`（聊天历史）、`installs/`（pm 依赖环境）、`bin/`（二进制）。

## 阶段 0 · 旧机（本机，已完成）

产物都在宿主可读的 `/data/liuxin/hermes-migrate/`：

| 文件 | 大小 | 作用 |
|---|---|---|
| `hermes-profile-default-<ts>.tar.gz` | 1.3 MB | profile 本体（本次迁移的主角） |
| `bifrost-mcp-0.1.0-x86_64-linux-musl.tar.gz` | 2.4 MB | bifrost 二进制 —— **profile 包里没有，必须单独带** |

重新导一次（内容有更新时）：

```bash
hermes profile export default -o /data/liuxin/hermes-migrate/hermes-profile-default-$(date -u +%Y%m%d-%H%M%S).tar.gz
```

## 阶段 1 · 传到新机

```bash
scp /data/liuxin/hermes-migrate/hermes-profile-*.tar.gz \
    /data/liuxin/bifrost/dist/bifrost-mcp-0.1.0-x86_64-linux-musl.tar.gz \
    <user>@<newhost>:/tmp/
```

## 阶段 2 · 新机安装 Hermes

```bash
curl -fsSL https://hermes-agent.nousresearch.com/install.sh | bash
hermes doctor
```

## 阶段 3 · 导入 profile

**注意语法**：名字用 `--name` 传，不能写成第二个位置参数。

```bash
hermes profile import /tmp/hermes-profile-default-<ts>.tar.gz --name peter
hermes profile list
```

两条硬约束（已实测）：

- **不能导入成 `default`**：`Error: Cannot import as 'default' — that is the built-in root profile`
- **不会覆盖**同名已存在的 profile

导入后落在 `~/.hermes/profiles/peter/`，并自动生成一个 `~/.local/bin/peter` 包装脚本。

### 3b · 想直接用 default？

两种做法，选一个：

```bash
# 做法一（推荐）：切换默认 profile
hermes profile use peter

# 做法二：把内容并进 default
cp -a ~/.hermes/profiles/peter/{SOUL.md,config.yaml,memories,skills,cron,plugins} ~/.hermes/
```

## 阶段 4 · 装 bifrost 二进制（profile 包里没有）

profile 的 `config.yaml` 里写的是**绝对路径**：

```yaml
mcp_servers:
  bifrost:
    command: /opt/data/bin/bifrost
    env:
      BIFROST_CONFIG: /opt/data/home/.bifrost/settings.json
```

所以有两条分支：

```bash
# A. 新机 HERMES_HOME 也是 /opt/data（docker 同构）→ 路径天然对，直接装二进制
tar xzf bifrost-mcp-0.1.0-x86_64-linux-musl.tar.gz
cd bifrost-mcp-0.1.0-x86_64-linux-musl
./install.sh --no-hermes --storage <新机共享盘路径>
cp bifrost /opt/data/bin/bifrost

# B. 新机是原生安装（~/.hermes）→ 先装二进制，再重写配置里的路径
PREFIX=$HOME/.local/bin ./install.sh --no-hermes --storage <新机共享盘路径>
hermes mcp remove bifrost
hermes mcp add bifrost --connect-timeout 30 \
  --env BIFROST_CONFIG=$HOME/.bifrost/settings.json \
  --command $HOME/.local/bin/bifrost --args mcp-serve
```

## 阶段 5 · 重填凭据（profile 包不含）

本机需要重新提供的键（**只有键名，值不随包走**）：

```
DEEPSEEK_API_KEY / DEEPSEEK_BASE_URL / API_SERVER_KEY
FEISHU_APP_ID / FEISHU_APP_SECRET / FEISHU_DOMAIN
FEISHU_CONNECTION_MODE / FEISHU_GROUP_POLICY / FEISHU_ALLOW_ALL_USERS
FEISHU_HOME_CHANNEL / FEISHU_HOME_CHANNEL_THREAD_ID
```

用向导填最省事：

```bash
hermes setup                     # 模型 provider + key
hermes config set display.interface cli
```

飞书凭据写进 `~/.hermes/.env`（或 `$HERMES_HOME/.env`）：

```bash
FEISHU_APP_ID=cli_xxx
FEISHU_APP_SECRET=xxx
FEISHU_CONNECTION_MODE=websocket
FEISHU_HOME_CHANNEL=oc_xxx
```

> 频道配置本身（`config.yaml` 的 `feishu:` 段）是随包过来的，缺的只是密钥。

## 阶段 6 · 重启并验证

```bash
hermes gateway install && hermes gateway start
hermes gateway status            # 必须是 running，且日志里有 feishu 适配器上线
hermes mcp list | grep bifrost   # ✓ enabled
hermes mcp test bifrost          # 应发现 5 个工具
hermes doctor
```

再发一条飞书消息给机器人，确认能收到回复。

## 阶段 7 · 确认无误后停掉旧机

`cron/jobs.json` 会随包过去 —— **两台机器同时跑同一个定时任务会重复触发**。

```bash
hermes pause                     # 在旧机执行，停掉 cron 与投递
```

## 这次迁移会丢什么（提前知道）

| 会丢 | 影响 | 补救 |
|---|---|---|
| `.env` / `auth.json` | 模型、飞书、OAuth 全要重填 | 阶段 5 |
| `state.db` | 聊天历史、`session_search` 索引全没 | 见下方"可选：搬历史" |
| `installs/`（pm venv） | 飞书通道可能因缺 `lark-oapi` 起不来 | `python -c "import pm; pm.sync_venv(['mcp','feishu'], explicit=True)"`，然后重启 gateway |
| `bin/bifrost` | bifrost MCP 连不上 | 阶段 4 |

### 可选：搬历史会话

旧机（**先停 gateway**，避免写半截数据库）：

```bash
hermes gateway stop
sqlite3 ~/.hermes/state.db ".backup /tmp/state.db"
# profile 场景是 ~/.hermes/profiles/<name>/state.db
scp /tmp/state.db <user>@<newhost>:/tmp/
```

新机：

```bash
hermes gateway stop
cp /tmp/state.db ~/.hermes/state.db       # 或 profiles/<name>/state.db
rm -f ~/.hermes/state.db-wal ~/.hermes/state.db-shm
sqlite3 ~/.hermes/state.db "PRAGMA integrity_check;"
hermes gateway start
```

新旧版本跨代时这一步有风险（schema 变更），`integrity_check` 不过就别硬上。

## 一句话总结

profile 路线 = 人设搬家，**凭据和二进制要自己补**。
如果哪天想省掉阶段 4 和"补二进制"，就走整盘 `hermes-home-*.tar.gz`（含 `bin/bifrost`）；
想连历史和凭据一起搬，就用 `hermes backup` 的 zip + `hermes import`。
