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
- [第三部分：Job 工作流专项测试（--job）](#第三部分job-工作流专项测试--job)
  - [3.1 测试目的](#31-测试目的)
  - [3.2 测试用例设计](#32-测试用例设计)
  - [3.3 测试结果](#33-测试结果)
  - [3.4 发现并修复的问题](#34-发现并修复的问题)
  - [3.5 测试用例归档（供端到端测试复用）](#35-测试用例归档供端到端测试复用)
- [第四部分：并发与多卡并行测试](#第四部分并发与多卡并行测试)
  - [4.1 测试目的](#41-测试目的)
  - [4.2 测试结果](#42-测试结果)
  - [4.3 结论](#43-结论)
- [第五部分：Server 健壮性审查与加固](#第五部分server-健壮性审查与加固)
  - [5.1 审查结论：1 个真 bug + 4 个健壮性缺口](#51-审查结论1-个真-bug--4-个健壮性缺口)
  - [5.2 修复方案](#52-修复方案)
  - [5.3 验证结果（test_robustness.py，S1-S4 全过）](#53-验证结果test_robustnesspys1-s4-全过)
  - [5.4 结论](#54-结论)
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

# 第三部分：Job 工作流专项测试（--job）

> 时间：2026-07-31 17:30 (CST, UTC+8)
> 测试脚本：`/gpfs/gcsp/liuxin/bifrost_test/test_job.py`
> YAML 用例：`/gpfs/gcsp/liuxin/bifrost_test/jobs/`（j1~j5）
> 二进制：`./target/release/bifrost`

## 3.1 测试目的

`--job` 是 YAML 多任务工作流提交入口（client 侧串行执行引擎，launcher.rs）。验证：

1. 基本 job 生命周期：提交 → 逐任务执行 → JobResult 汇总
2. 任务执行顺序（YAML 定义序）
3. job 内任务超时处理
4. `working_dir` / `env_vars` 等字段是否真正传递到任务
5. `ignore_failure` 语义（失败后是否继续）

## 3.2 测试用例设计

| 用例 | YAML | 任务 | 验证点 |
|------|------|------|--------|
| J1 | j1_basic.yaml | echo-ok / echo-fail(exit 3) / sleep-1 | JobResult 汇总、状态、stdout、exit_code、耗时 |
| J2 | j2_order.yaml | first→second→third 追加写文件 | 执行顺序严格按 YAML |
| J3 | j3_timeout.yaml | sleep 30 + timeout 2 | job 内任务超时 → Timeout 状态 |
| J4 | j4_env_wd.yaml | `sh -c 'pwd && echo $MY_VAR'`，wd=/tmp，MY_VAR=job-env-ok | working_dir / env_vars 传递 |
| J5 | j5_ignore_failure.yaml | will-fail(exit 1) → after-fail | 失败后 job 继续执行 |

## 3.3 测试结果

| 用例 | 结果 | 数据 |
|------|------|------|
| J1 结构 | ✅ | 3 任务全部进入 task_results |
| J1 状态汇总 | ✅ | job_status=CompletedWithFailures，completed=2/failed=1 |
| J1 各任务状态 | ✅ | [Completed, Failed, Completed] |
| J1 stdout 捕获 | ✅ | `job-task-ok` |
| J1 exit_code 捕获 | ✅ | 3 |
| J1 耗时记录 | ✅ | sleep-1 duration=1s |
| J2 执行顺序 | ✅ | `first\nsecond\nthird`（修复用例写法后） |
| J3 超时状态 | ✅ | Timeout, dur=2s |
| J3 超时消息 | ✅ | "Task timed out after 2 seconds" |
| J4 working_dir | ✅ | stdout=`/tmp`（修复前为仓库根目录） |
| J4 env_vars | ✅ | stdout=`job-env-ok`（修复前为空） |
| J5 失败后继续 | ✅ | 2 任务均执行：[will-fail=Failed, after-fail=Completed] |

## 3.4 发现并修复的问题

### 🐛 Bug D：launcher 静默忽略 JobTask 的 working_dir / env_vars（严重）

- **现象**：J4 中 YAML 配置 `working_dir: /tmp` 和 `env_vars: {MY_VAR: job-env-ok}`，但任务实际在**仓库根目录**执行、环境变量**为空**
- **根因**：`launcher.rs` 的 `launch_job` 构造 Task 时只设置了 `priority`/`timeout`/`retry_count`，**从未调用 `with_working_dir` / `with_env_var`**——YAML 中这两个字段被静默忽略，用户配置了但完全不生效
- **影响面**：所有依赖特定工作目录或环境变量的 job 任务（如 Python 虚拟环境、GPU 相关变量）都会在错误环境下运行
- **修复**（`ea4fea5`）：launcher 构造 Task 后应用 `with_working_dir` / `with_env_var`
- **回归测试**：新增 MockBridge 单测 `test_launch_job_passes_working_dir_and_env`，验证字段传递（59 项测试全过）

## 3.5 测试用例归档（供端到端测试复用）

以下用例设计可直接放入未来的端到端（E2E）测试套件，脚本与 YAML 均已保留：

```
/gpfs/gcsp/liuxin/bifrost_test/
├── test_timeout.py          # 第一部分 Timeout 测试 (T1-T5)
├── test_job.py              # 第三部分 Job 测试 (J1-J5)
├── test_concurrent.py       # 第四部分 并发测试 (N 任务并行)
├── test_pytest_concurrent.py# 第四部分 多卡 pytest 并行 (GPU 隔离)
└── jobs/                    # YAML 用例文件
    ├── j1_basic.yaml        # 基本: ok + fail(exit 3) + sleep
    ├── j2_order.yaml        # 顺序: 3 任务追加写文件
    ├── j3_timeout.yaml      # 超时: sleep 30 + timeout 2
    ├── j4_env_wd.yaml       # wd/env: working_dir + env_vars 传递
    └── j5_ignore_failure.yaml  # 失败继续: exit 1 → echo
```

**复用说明：**
- 脚本均以 `<binary> <storage> [n]` 方式参数化，E2E 中只需替换二进制路径和存储区
- J2 用例依赖绝对路径 `/gpfs/gcsp/liuxin/bifrost_test/job_order.txt`，E2E 时需参数化
- 用例断言已包含修复前失败/修复后通过的对照，可作回归测试
- 对应单测：`cargo test test_launch_job_passes_working_dir_and_env`（MockBridge，无需真实 server）

---

# 第五部分：Server 健壮性审查与加固

> 时间：2026-07-31 18:30 (CST, UTC+8)
> 审查对象：`src/daemon/runner.rs`、`watcher.rs`、`executor.rs`
> 验证脚本：`/gpfs/gcsp/liuxin/bifrost_test/test_robustness.py`
> 修复 commit：`21269c5`

## 5.1 审查结论：1 个真 bug + 4 个健壮性缺口

| # | 严重度 | 问题 | 后果 |
|---|--------|------|------|
| Bug E | 🔴 真 bug | stdout 截断 `&s[..1000]` UTF-8 边界 panic | 中文输出超 1000 字节时任务结果丢失 |
| 缺口 1 | 🔴 严重 | server 启动不消费存量任务 | 重启/提前提交的任务**永久丢失** |
| 缺口 2 | 🔴 严重 | watcher 出错 break 循环 | server **变僵尸**（活着但不消费） |
| 缺口 3 | 🟡 中 | 坏 JSON 静默丢弃 | client **永远 Pending** |
| 缺口 4 | 🟡 中 | executor 错误不写结果 | client **永远 Pending** |

## 5.2 修复方案

1. **Bug E**：`truncate_utf8()` 按字符边界安全截断（3 个新单测）
2. **缺口 1**：启动时 catch-up 扫描 + **每 5s 兜底扫描** commands/（inotify 快路径之外的第二通道，watcher 死了也能自愈）
3. **缺口 2**：watcher Err 时 continue 而非 break；init 失败每 2s 重试；主体包进 `spawn_blocking`（实测发现 thread::sleep 在 async worker 上会 panic，一并修复）
4. **缺口 3/4**：3 次读/解析重试 + 失败写 Failed 结果 → client 必得终态
5. **去重**：`commands/{name}.processing` 领取标记（`create_new` 原子）——inotify 与 scan 双通道不会重复执行同一任务

## 5.3 验证结果（test_robustness.py，S1-S4 全过）

| 场景 | 验证点 | 结果 |
|------|--------|------|
| S1 重启恢复 | server 启动前提交的任务被执行（旧版永久丢失） | ✅ |
| S2 快路径 | 正常运行期 inotify 毫秒级消费 | ✅ |
| S3 坏 JSON | 不可解析任务写 Failed 结果，非永远 Pending | ✅ |
| S4 防重 | 同 task_id 重复提交只执行一次 | ✅ |

## 5.4 结论

server 端现在具备**自愈能力**：watcher 失效 → 5s 兜底扫描接管；重启 → 存量任务补执行；解析/执行失败 → 必有终态结果。73 项单测全过。

---

# 第四部分：并发与多卡并行测试

> 时间：2026-07-31 17:50 (CST, UTC+8)
> 测试脚本：`/gpfs/gcsp/liuxin/bifrost_test/test_concurrent.py`、`test_pytest_concurrent.py`
> 二进制：`./target/release/bifrost`

## 4.1 测试目的

回答三个问题：
1. 能否同时提交多个独立任务？（client 侧）
2. server 能否并行执行？（daemon 侧，基于 07c9511 的 tokio::spawn + Semaphore 并发修复）
3. 多个 pytest 用例能否跑在 H20 不同 GPU 卡上并行执行？

## 4.2 测试结果

### 场景 A：4 个 sleep 3 任务

| 指标 | 结果 |
|------|------|
| 提交 4 任务耗时 | **0.01s**（一口气提交，无等待） |
| 端到端完成 | **3.54s**（串行预期 12s） |
| 各任务 duration | 3002/3005/3005/3005ms（完整执行） |
| 并行度 | **3.4x** |

### 场景 B：10 个 sleep 2 任务（压 max_concurrent=10 上限）

| 指标 | 结果 |
|------|------|
| 提交耗时 | **0.02s** |
| 端到端完成 | **2.57s**（串行预期 20s） |
| 并行度 | **7.8x** |

### 场景 C：4 个 pytest 用例跑 4 张卡（GPU 隔离验证）

4 个任务各指定 `CUDA_VISIBLE_DEVICES=0/1/2/3`，并行执行：

| 任务 | 看到的 GPU | 独立 PID |
|------|-----------|---------|
| 23dfdf15 | GPU=0 | 3200653 |
| ad15c54b | GPU=1 | 3200655 |
| 8166d3ea | GPU=2 | 3200654 |
| 475b2e0e | GPU=3 | 3200656 |

- 端到端 **3.01s**（串行预期 ~8s）✅
- 4 个任务并行运行、GPU 环境变量隔离正确 ✅

## 4.3 结论

1. **提交侧**：支持任意数量任务同时提交（毫秒级，写文件即完成）
2. **执行侧**：server 按 `daemon.max_concurrent`（默认 10）**并行执行**，互不阻塞；长任务不再卡住后续任务
3. **多卡场景**：bifrost 本身不分配 GPU——**任务命令自己指定** `CUDA_VISIBLE_DEVICES`（`sh -c 'CUDA_VISIBLE_DEVICES=0 pytest ...'` 或 job YAML 的 env_vars），daemon 并行拉起，天然支持"多个用例跑不同卡"
4. **并发上限**：`max_concurrent` 可调（H20 上建议 ≤ GPU 卡数，避免排队任务争卡）

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
| 7 | **launcher 忽略 wd/env** | `launcher.rs` | `--job` 任务的 working_dir / env_vars 被静默忽略，任务在错误环境下执行 | `ea4fea5` |
| 8 | **stdout 截断 UTF-8 panic** | `executor.rs` | `&s[..1000]` 切片落在多字节字符中间时 panic（中文超长输出），任务结果丢失 | `21269c5` |

> 所有修复均已通过 `cargo test`（73 项）与端到端实测验证，并推送到 `test/deploy` 分支。
