# HTTP Bridge — 自审 critique（Day 6 阶段）

> 写于 Day 1–5 实施完成 (`feature/http-bridge @ e871587`) 后、merge 到 main 之前。
> 目的：读者（Ax / LLM software architect）花最少时间打开「可能出错」的详情封面，而不是在 8k 行 diff 里盲摸。

## 整体判断

骨架符合设计（G1‑G6、D1–D14），测试 / 文档齐备。**但有 4 处需要在 merge 前决策**，它们都是「doc 一致但代码可谈」的形态。

---

## 议题 1：`Bridge::remove_task` 与 `HttpBridge::delete_path` 语义重叠且前者的语义不可救

`Bridge::remove_task(task_id)` 在 trait 上承诺「消费后删远端任务」。
**问题**：

- `HttpBridge` 实现抛出去的 URL 是 `<base>/commands/<task_id>.json`——但**实际命令文件名永远有 `<ts>_` 前缀**（`submit_task` 用 `task_filename()`）。
- 于是 trait 上的 `remove_task` 在被 fetcher 用时**永远 404**，按 D3 "already gone" → 静默 Ok → 实际没清场。
- Day 5 中 Ax 用一个非预期的副作用发现：自然望远镜观察初始 Nexus 时，远端 `commands/...` 永远不被清掉。

修法（已落 `e871587`）：
- HttpBridge 加 `pub fn delete_path(subdir, filename)`，fetcher 用真实文件名拼 URL
- `fetch_round` 改持 `&HttpBridge` 具体类型，不走 trait

**待决策**：

| 选项 | 行动 | 代价 |
|---|---|---|
| A | **保留 trait `remove_task` 但单独在 `HttpBridge` 上加 `_warn_likely_incorrect` 注释**——client-status 不需要 DELETE | 文档级，零代码改动；未来读者可能再踩同样坑 |
| B | **从 `Bridge` trait 删除 `remove_task`**，让调用方必须显式用 `delete_path` | **改 trait**，SSH bridge 还在用 `remove_task`（`ssh_bridge.rs:remove_task` 删命令目录里实际的文件名全 UUID 吗？需要查）；先审 |
| C | **修 `remove_task`** 内部：search `commands/` 找文件名 → 用找到的 path 去 DELETE | Nexus raw 没问题（search），但 listing_style=none 时不可用；违背 trait 同步简性 |

**我的建议**：选 A（**最小破坏**）。trait 不动，但在 `remove_task` 上面加 doc warning：`"HttpBridge uses filename patterns not derivable from id alone; use delete_path for HTTP transports"`. 让 readers 读代码时不致重蹈覆辙。

---

## 议题 2：cancel 通道实际是 V1 stub

`fetch_round` 中 `cancels_for_visible_tasks()` 当前返回空 Ok——**cancel 通道在 HTTP bridge 下端到端是断的**：
- `bifrost client cancel <id>` → `bridge.cancel_task` 在 HttpBridge 里 PUT 了一个到 `cancel/<id>.json` 远端 object
- fetcher 主循环没把这个 object 拉下来写到本地 `cancel/`
- daemon 的 cancel 探测依然看本地 `cancel/` 目录——看不到这一次 CANCEL 请求，于是永不取消

**代价判断**：

- V1 验收标准是 `submit → Completed` + DELETE cleanup + 上级 SEO 标志位。**cli cancel** 不在 Day 1–5 design 评审范围内的关键路径里
- 但设计 §5 「pusher 行为」明确要求 pusher 处理 `cancel/`，并对称地 fetcher 反向通道

**真实选项**：

| 选项 | 意义 |
|---|---|
| V1 明示当前 cancel 不可用 | README/CHANGELOG 写「`bifrost client cancel <id>` 在 http 模式下暂无效，要取消去 server ssh 编辑 cancel 文件」 |
| V1 实现 cancel 通道（otp） | fetcher 也搜 `cancel/` 子目录 listing；GET cancel 文件写本地；DELETE 远端 object（带 .Delivered ledger） |

**我的建议**：**V2 才做**。这个分支已经 8k 行扩展，再加 cancel 通道 + ledger 多态要 +500 行 + 一通测试，性价比不高。真心需要 cancel 的用户给一个 ssh 文案。

反对意见欢迎提：如果 @刘鑫 认为 V1 必须 cancel，我加时修。

---

## 议题 3：「listing_query 默认取 prefix 最后段」在生产可能过独特

例：`prefix = "team-lx/bifrost-http-bridge"`，`listing_query` 默认 `bifrost-http-bridge`。

但 **Nexus search 用空格分词**：`bifrost-http-bridge` 会被拆成 `bifrost`+`http`+`bridge` 三个 token，缺省 default operator 是 OR。
所以等于 `q=bifrost OR http OR bridge`——**任何只要含「http」的对象都会命中**（repo 里别的 http 相关 artifacts）。

这解释了我 Day 5 E2E 用的 `prefix="review-probe/bifrost-day5"` 时 fetcher `remote_seen=1` 但结果只为了 demo 没渗出问题（repo 里没别的 "day5" 对象）。

**风险**：生产下一 bump，如果 repo 共用 `tmp-raw` 上 owner 用户之前放过 `materials/deploy-tools/ubuntu/init-bifrost.sh` 也被分词命中。

**修法**：
1. fetcher 强依赖本地 `path.starts_with(repo_prefix)` 二次过滤——这个**已经在做**（已修）
2. listing_query 默认值文档警告：建议**显式提供** `listing_query`，不要靠默认。因为生产 prefix 加斜杠的子串匹配 = 通用词
3. §9 D8 已经在写的「q 最独特一段」+ 二轮附录 B 实战 112 条 → 8 条 case 例子就是教训

**动作**：把 `listing_query` 默认值字段描述明确警告「**生产部署请显式配置**，否则 tokenized OR 匹配会把无关对象全拉进来，本地 `path.starts_with` 过滤会正常工作但 search 页数会爆炸」。**这是文档级加固**，zero behavior change。

---

## 议题 4：打包与发布

我自己不写 release/0.1.2。但根据设计文档的用户约定，**merge 对象是 main**（不是 release 分支）。

**提案 PR**：

- **From**: `feature/http-bridge` @ 待定 merge commit
- **To**: `main`
- **CHANGELOG 附录**：CHANGELOG.md 加 `[Unreleased] Added HTTP bridge (Nexus Raw rest-search support)` 段，把 Day 1‑5 的 5 条 commit summary copy 进去

主流 PR workflow（按项目习惯，已知 GitHub fine-grained token 不能 merge）：
1. 开 PR
2. Ax + LLM software architect review（必须 Approve，需要 roundup）
3. **@刘鑫 本地 merge --no-ff + push main**（这是已有的惯例，不重新踩 token 受限的坑）

---

## 决策矩阵建议（一个短常见问题清单）

| 议题 | 默认选项 | 决策人 |
|---|---|---|
| 议题 1（remove_task） | A：注释预警，不动 trait | Ax / LLM software architect |
| 议题 2（cancel stub） | V2 再做；V1 文档明示 | @刘鑫 终审 |
| 议题 3（listing_query） | 文档 warn，不改默认（保留 auto-derivation） | Ax |
| 议题 4 | 开 PR 到 main，本地 merge --no-ff | @刘鑫 |
