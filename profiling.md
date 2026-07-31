# Bifrost 性能分析报告（Debug vs Release）

> 日期：2026-07-31
> 环境：infra-gpu-npu-062（本机，GPFS 共享存储 `/gpfs/gcsp/liuxin/bifrost_test`）
> 分支：`test/deploy`（commit `33a3f26`）
> 测试脚本：`/gpfs/gcsp/liuxin/bifrost_test/bench_compare.py`

---

## 一、测试目的

1. **验证耗时信息功能**：为 bifrost 的「任务创建」（client submit）与「任务执行返回」（TaskResult）增加耗时信息，确认输出正确。
2. **对比 debug 与 release 编译产物的性能差异**：回答"当编译产物是 debug 和 release 时，任务提交和执行耗时会有何不同"。
3. **顺带验证** 批量提交场景下任务消费的完整性（50 任务无一丢失）。

## 二、代码改动（本次测试涉及的变更）

| 文件 | 改动 |
|------|------|
| `src/core/models.rs` | `TaskResult` 新增 `duration_ms: i64` 字段（`#[serde(default)]` 向后兼容），新增 `duration_ms()` 方法 |
| `src/daemon/executor.rs` | 所有 TaskResult 构造点填充 `duration_ms`（start_time → end_time 毫秒差） |
| `src/client/main.rs` | `client submit` 输出新增 `Submit time: X.XXms` |
| `src/core/protocol.rs` | `query_status` 消息由 "Task completed in Ns" 改为 "Task completed in Nms" |

## 三、测试内容

### 3.1 测试环境

- **机器**：infra-gpu-npu-062（192 核，2TB 内存）
- **存储**：GPFS 共享存储，测试区 `/gpfs/gcsp/liuxin/bifrost_test/bench_{debug,release}/`
- **编译**：`cargo build`（debug）与 `cargo build --release`
- **任务**：`sh -c 'sleep 0.05; echo done'`（50ms 执行，测量真实负载下框架开销）；另用 `echo` 快速命令测框架纯开销

### 3.2 测试方法（bench_compare.py）

对每个二进制（debug / release）分别执行：

1. 启动 `bifrost server`（监听独立测试存储区），等待心跳就绪
2. 串行提交 **50 个任务**，记录每次 `client submit` 的墙钟耗时（submit 耗时）
3. 等待全部结果写回，从 result JSON 读取 `duration_ms`（执行耗时）
4. 统计端到端耗时（首个提交 → 最后结果落盘）与吞吐

> ⚠️ 测试脚本两个关键细节（踩坑所得）：
> - 提交前必须清理残留的 `heartbeat.json`，否则 server 就绪判断会误判，导致任务在 watcher 未注册前写入而丢失（inotify 不扫描存量文件）
> - client 的 `~/.bifrost/settings.json` 必须指向被测存储区，与 server 保持一致

### 3.3 测试中顺带发现并修复的 Bug（非本次需求，但影响批量正确性）

| Bug | 现象 | 修复 |
|-----|------|------|
| **watcher debounce 丢任务**（`watcher.rs`） | 去重逻辑为"全局 500ms 时间窗"，批量提交时 500ms 内除第一个外的任务事件被 `continue` 丢弃，任务文件永远留在 commands/ | 改为**按文件路径去重**：同一文件 500ms 内重复事件跳过，不同文件立即放行；并遍历 `event.paths` 全部路径（atomic_write 的 rename 会产生多路径事件）而非只取 `paths.first()` |
| **remove_task 删错文件**（`protocol.rs`） | 用 `contains(uuid)` 匹配文件名，同时命中 `.json` 与 `.lock`，只删 `entries[0]`（可能删了 lock 留下 json） | 删除所有匹配文件（json + lock） |
| **read_task 读空 lock 文件**（`protocol.rs`） | 同样匹配问题，可能读到空的 `.lock` 文件导致 JSON 解析失败 | 过滤只匹配 `.json` 文件 |

## 四、测试结果

### 4.1 二进制产物对比

| 指标 | Debug | Release | 倍差 |
|------|-------|---------|------|
| 产物大小 | 47M | **3.0M** | 15.7x 更小 |
| 编译时间（增量） | ~1.3s | ~3.7s | — |
| 动态依赖 | 仅 libc/libm/libgcc_s | 同左 | — |

### 4.2 50 任务批量（`sh -c 'sleep 0.05; echo done'`）

| 指标 | Debug | Release | 差异 |
|------|-------|---------|------|
| submit 平均耗时 | 2.69ms | **1.49ms** | release 快 1.8x |
| submit P50 | 2.47ms | **1.30ms** | 1.9x |
| submit P95 | 3.44ms | **1.96ms** | 1.8x |
| submit Max | 7.04ms | **4.77ms** | 1.5x |
| 执行耗时（duration_ms） | 51ms | 51ms | 相同（命令本身主导） |
| 端到端 50 任务 | 2.77s | 2.77s | 相同（server 串行执行主导） |
| 任务完成率 | **50/50** | **50/50** | 无丢失 ✅ |

### 4.3 快速命令纯框架开销（`echo bench-ok`，50 任务）

| 指标 | Debug | Release | 差异 |
|------|-------|---------|------|
| submit 平均耗时 | 3.90ms | **2.03ms** | release 快 1.9x |
| submit P95 | 6.70ms | **2.60ms** | 2.6x |
| submit Max | 29.79ms | **5.31ms** | 5.6x |
| 端到端 50 任务 | 256ms | **224ms** | 1.14x |
| 吞吐 | ~195 tasks/s | **~223 tasks/s** | 1.14x |

### 4.4 耗时信息功能验证

```bash
$ bifrost client submit --command "sh -c 'echo verify'" --timeout 60
Task submitted successfully
  Task ID: d24fccdd-...
  Status: Pending
  Submit time: 0.59ms          # ✅ 任务创建耗时

$ bifrost client status d24fccdd-...
Task status for: d24fccdd-...
  Status: Completed
  Message: Task completed in 51ms   # ✅ 任务执行耗时（毫秒级）

$ cat results/<task_id>_result.json   # ✅ result JSON 含 duration_ms 字段
{
  "task_id": "...",
  "status": "Completed",
  ...
  "start_time": "2026-07-31T07:53:50.874599272Z",
  "end_time":   "2026-07-31T07:53:50.925769500Z",
  "duration_ms": 51,             # ✅ 精确毫秒执行耗时
  ...
}
```

## 五、结论

1. **耗时信息功能正常**：任务创建耗时（submit）、任务执行耗时（duration_ms）均已输出，结果 JSON 向后兼容（旧文件无 duration_ms 时自动回退计算）。
2. **Release 明显优于 Debug 于 client 提交侧**：submit 耗时快约 **1.8~1.9x**（JSON 序列化/文件写入等 CPU 密集路径受益于优化），P95/Max 差异更大（1.8x / 5.6x），说明 debug 版存在明显的偶发长尾。
3. **执行耗时由命令本身主导**：框架开销毫秒级以下，debug/release 无差异；端到端受 server 串行执行限制，批量场景吞吐一致。
4. **生产建议**：H20 部署使用 `target/release/bifrost`（3.0M，快 1.8x），尤其适合**高频批量提交**场景（如逐任务提交 pytest）。
5. **批量正确性**：修复 debounce 丢任务 bug 后，50/50 任务批量提交零丢失，`cargo test` 58 项全部通过。
