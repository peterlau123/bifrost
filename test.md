# Bifrost 测试报告

> **文档创建时间**：2026-07-31 16:30 (CST, UTC+8)
> **最后更新**：2026-07-31 16:30 (CST, UTC+8)
> **环境**：infra-gpu-npu-062（本机，GPFS 共享存储 `/gpfs/gcsp/liuxin/bifrost_test`）
> **分支**：`test/deploy`
> **相关 commit**：`33a3f26`（性能对比）、`07c9511`（timeout 修复）

---

## 📑 内容目录

- [第一部分：性能对比测试（Debug vs Release）](#第一部分性能对比测试debug-vs-release)
  - [1.1 测试目的](#11-测试目的)
  - [1.2 测试环境](#12-测试环境)
  - [1.3 测试方法](#13-测试方法)
  - [1.4 测试结果](#14-测试结果)
  - [1.5 结论](#15-结论)
- [第二部分：Timeout 专项测试](#第二部分timeout-专项测试)
  - [2.1 测试目的](#21-测试目的)
  - [2.2 测试方法](#22-测试方法)
  - [2.3 测试结果](#23-测试结果)
  - [2.4 发现并修复的问题](#24-发现并修复的问题)
  - [2.5 结论](#25-结论)
- [附录：所有测试中发现并修复的 Bug 汇总](#附录所有测试中发现并修复的-bug-汇总)

---

# 第一部分：性能对比测试（Debug vs Release）

## 1.1 测试目的

1. **验证耗时信息功能**：为 bifrost 的「任务创建」（client submit）与「任务执行返回」（TaskResult）增加耗时信息，确认输出正确。
2. **对比 debug 与 release 编译产物的性能差异**：回答"当编译产物是 debug 和 release 时，任务提交和执行耗时会有何不同"。
3. **顺带验证** 批量提交场景下任务消费的完整性（50 任务无一丢失）。

## 1.2 测试环境

- **机器**：infra-gpu-npu-062（192 核，2TB 内存）
- **存储**：GPFS 共享存储，测试区 `/gpfs/gcsp/liuxin/bifrost_test/bench_{debug,release}/`
- **编译**：`cargo build`（debug）与 `cargo build --release`
- **任务**：`sh -c 'sleep 0.05; echo done'`（50ms 执行，测量真实负载下框架开销）；另用 `echo` 快速命令测框架纯开销
- **测试脚本**：`/gpfs/gcsp/liuxin/bifrost_test/bench_compare.py`

### 涉及的功能代码改动

| 文件 | 改动 |
|------|------|
| `src/core/models.rs` | `TaskResult` 新增 `duration_ms: i64` 字段（`#[serde(default)]` 向后兼容），新增 `duration_ms()` 方法 |
| `src/daemon/executor.rs` | 所有 TaskResult 构造点填充 `duration_ms`（start_time → end_time 毫秒差） |
| `src/client/main.rs` | `client submit` 输出新增 `Submit time: X.XXms` |
| `src/core/protocol.rs` | `query_status` 消息由 "Task completed in Ns" 改为 "Task completed in Nms" |

## 1.3 测试方法

对每个二进制（debug / release）分别执行：

1. 启动 `bifrost server`（监听独立测试存储区），等待心跳就绪
2. 串行提交 **50 个任务**，记录每次 `client submit` 的墙钟耗时（submit 耗时）
3. 等待全部结果写回，从 result JSON 读取 `duration_ms`（执行耗时）
4. 统计端到端耗时（首个提交 → 最后结果落盘）与吞吐

> ⚠️ 测试脚本两个关键细节（踩坑所得）：
> - 提交前必须清理残留的 `heartbeat.json`，否则 server 就绪判断会误判，导致任务在 watcher 未注册前写入而丢失（inotify 不扫描存量文件）
> - client 的 `~/.bifrost/settings.json` 必须指向被测存储区，与 server 保持一致

## 1.4 测试结果

### 1.4.1 二进制产物对比

| 指标 | Debug | Release | 倍差 |
|------|-------|---------|------|
| 产物大小 | 47M | **3.0M** | 15.7x 更小 |
| 编译时间（增量） | ~1.3s | ~3.7s | — |
| 动态依赖 | 仅 libc/libm/libgcc_s | 同左 | — |

### 1.4.2 50 任务批量（`sh -c 'sleep 0.05; echo done'`）

| 指标 | Debug | Release | 差异 |
|------|-------|---------|------|
| submit 平均耗时 | 2.69ms | **1.49ms** | release 快 1.8x |
| submit P50 | 2.47ms | **1.30ms** | 1.9x |
| submit P95 | 3.44ms | **1.96ms** | 1.8x |
| submit Max | 7.04ms | **4.77ms** | 1.5x |
| 执行耗时（duration_ms） | 51ms | 51ms | 相同（命令本身主导） |
| 端到端 50 任务 | 2.77s | 2.77s | 相同（server 串行执行主导） |
| 任务完成率 | **50/50** | **50/50** | 无丢失 ✅ |

### 1.4.3 快速命令纯框架开销（`echo bench-ok`，50 任务）

| 指标 | Debug | Release | 差异 |
|------|-------|---------|------|
| submit 平均耗时 | 3.90ms | **2.03ms** | release 快 1.9x |
| submit P95 | 6.70ms | **2.60ms** | 2.6x |
| submit Max | 29.79ms | **5.31ms** | 5.6x |
| 端到端 50 任务 | 256ms | **224ms** | 1.14x |
| 吞吐 | ~195 tasks/s | **~223 tasks/s** | 1.14x |

### 1.4.4 耗时信息功能验证

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

## 1.5 结论

1. **耗时信息功能正常**：任务创建耗时（submit）、任务执行耗时（duration_ms）均已输出，结果 JSON 向后兼容（旧文件无 duration_ms 时自动回退计算）。
2. **Release 明显优于 Debug 于 client 提交侧**：submit 耗时快约 **1.8~1.9x**（JSON 序列化/文件写入等 CPU 密集路径受益于优化），P95/Max 差异更大（1.8x / 5.6x），说明 debug 版存在明显的偶发长尾。
3. **执行耗时由命令本身主导**：框架开销毫秒级以下，debug/release 无差异；端到端受 server 串行执行限制，批量场景吞吐一致。
4. **生产建议**：H20 部署使用 `target/release/bifrost`（3.0M，快 1.8x），尤其适合**高频批量提交**场景（如逐任务提交 pytest）。
5. **批量正确性**：修复 debounce 丢任务 bug 后，50/50 任务批量提交零丢失，`cargo test` 58 项全部通过。

---

# 第二部分：Timeout 专项测试

> 时间：2026-07-31 16:30 (CST, UTC+8)
> 测试脚本：`/gpfs/gcsp/liuxin/bifrost_test/test_timeout.py`
> 二进制：`./target/release/bifrost`

## 2.1 测试目的

对**带 timeout 的任务**进行专项测试，验证整个框架在超时场景下是否暴露问题，重点检查：

1. 超时状态是否正确返回（Timeout 状态 + 错误消息）
2. **超时后子进程是否泄漏**（tokio timeout 取消 future 不杀进程的经典坑）
3. 边界情况：任务在 timeout 内恰好完成
4. 长任务是否阻塞后续任务（并发性）
5. 超时后框架是否仍可用

## 2.2 测试方法

对每个用例：启动独立 server → `client submit` 提交任务 → 轮询结果文件 → 断言状态/耗时/进程残留。

| 用例 | 任务 | timeout | 预期 |
|------|------|---------|------|
| T1 | `sleep 30` | 2s | Timeout 状态，~2s 返回，错误消息含 "timed out" |
| T2 | （T1 后） | — | `sleep 30` 进程零残留（无泄漏） |
| T3 | `sleep 1` | 2s | Completed（边界内正常完成） |
| T4 | `sleep 10` + `echo fast-ok` 先后提交 | 5s | 长任务 Timeout，快任务**不被阻塞**（并发） |
| T5 | `echo after-timeout` | 5s | Completed（超时后框架可用） |

## 2.3 测试结果

| 用例 | 结果 | 数据 |
|------|------|------|
| T1 基础超时 | ✅ | status=Timeout，elapsed=2.1s，duration=2001ms |
| T1 错误消息 | ✅ | "Task timed out after 2 seconds" |
| T2 进程泄漏 | ✅ 修复后 | 残留 sleep 进程数=0 |
| T3 边界完成 | ✅ | status=Completed，elapsed=1.1s |
| T4 快任务不阻塞 | ✅ 修复后 | 快任务 0.7s 完成（**修复前 5.1s**） |
| T4 长任务超时 | ✅ | status=Timeout，elapsed=5.0s |
| T5 超时后可用 | ✅ | status=Completed，elapsed=0.1s |

**测试前后对比（修复前 vs 修复后）：**

| 指标 | 修复前 | 修复后 |
|------|--------|--------|
| `sleep 30` 超时后进程残留 | ❌ 泄漏（孤儿进程） | ✅ 0 残留 |
| `sh -c 'sleep 30'` 超时后孙进程残留 | ❌ 泄漏（sleep 变孤儿） | ✅ 0 残留 |
| 快任务被长任务阻塞 | ❌ 等 5.1s | ✅ 0.7s |

## 2.4 发现并修复的问题

### 🐛 Bug A：超时后子进程泄漏（严重）

- **现象**：`sleep 30` + timeout 2 → 任务已标记 Timeout，但 `sleep 30` 进程仍在后台运行
- **根因**：tokio `timeout()` 取消 wait future 时**不杀子进程**；`kill_on_drop` 只杀直接子进程，`sh -c 'sleep 30'` 的孙进程 sleep 变成孤儿继续跑
- **修复**（`07c9511`）：
  - `cmd.process_group(0)` —— 子进程放入独立进程组
  - 超时分支 `libc::kill(-pid, SIGKILL)` —— 杀掉**整个进程组**
  - 随后 `child.wait().await` 回收，避免僵尸进程
- **验证**：修复后 `sh -c 'sleep 30'` 超时，进程树零残留、零僵尸

### 🐛 Bug B：server 串行执行，长任务阻塞一切（严重）

- **现象**：先提交 `sleep 10`（timeout 5），再提交 `echo fast-ok` → echo 等了 **5.1s** 才执行
- **根因**：`daemon.max_concurrent` 配置在 README 和 settings 中都定义了（默认 10），但 runner **从未使用**——server 实际是严格串行处理
- **修复**（`07c9511`）：每个任务 `tokio::spawn` 独立执行 + `Semaphore(max_concurrent)` 限流（默认 10）
- **验证**：修复后 echo 0.7s 完成，长任务 5s 超时，互不阻塞

## 2.5 结论

1. **超时功能本身正确**：超时状态、错误消息、耗时记录均正常。
2. **超时进程泄漏是真实且隐蔽的生产风险**：孤儿进程会无限累积，吃满资源；`sh -c` 包裹的命令（bifrost 防注入设计的常见用法）会让问题更隐蔽。已通过进程组 kill 彻底解决。
3. **并发是刚需**：H20 上跑 vLLM 等长任务时，串行执行会阻塞整个队列；`max_concurrent` 现已真正生效。
4. 修复后 `cargo test` 58 项全部通过。

---

# 附录：所有测试中发现并修复的 Bug 汇总

| # | Bug | 涉及文件 | 现象 | 修复 commit |
|---|-----|---------|------|------------|
| 1 | **watcher debounce 丢任务** | `watcher.rs` | 全局 500ms 时间窗去重，批量提交时 500ms 内除第一个外全部丢弃，任务文件永久滞留 commands/ | `33a3f26` |
| 2 | **remove_task 删错文件** | `protocol.rs` | `contains(uuid)` 匹配同时命中 `.json` 与 `.lock`，只删 entries[0]（可能删 lock 留 json） | `33a3f26` |
| 3 | **read_task 读空 lock 文件** | `protocol.rs` | 同样匹配问题，读到空 `.lock` 文件导致 JSON 解析失败 | `33a3f26` |
| 4 | **超时后子进程泄漏** | `executor.rs` | tokio timeout 取消 future 不杀进程；`sh -c` 孙进程变孤儿 | `07c9511` |
| 5 | **server 串行执行阻塞** | `runner.rs` | `max_concurrent` 配置存在但从未生效，长任务阻塞后续所有任务 | `07c9511` |
| 6 | （测试脚本）pgrep 假阴性 | `test_timeout.py` | `pgrep -f "^sleep$"` 匹配不到带参数进程 `sleep 30`，漏报泄漏 | `07c9511` |

> 所有修复均已通过 `cargo test`（58 项）与端到端实测验证，并推送到 `test/deploy` 分支。
