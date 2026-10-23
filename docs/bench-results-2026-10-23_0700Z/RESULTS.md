# vLLM 版本性能评测汇总

**数据截止**：2026-10-23 07:00Z　**机器**：H200 ×8（TP4 用 4 卡 / TP8 用 8 卡）　**版本**：7 个，v0.31.0 → v0.17.1

> `⚙` = 该版本加了 workaround `--compilation-config {"pass_config":{"fuse_allreduce_rms":false}}`（0.25.1/0.26.0 不加会崩，见第 6 节）

## 速览

| 问题 | 答案 |
|---|---|
| 满 8 卡值不值？ | **值**。M2.7 从 TP4 的 0.13–0.14 req/s 提到 TP8+EP 的 0.16–0.20，TTFT 从 ~10.5 s 降到 ~7.0 s |
| 哪个版本最快？ | 轻负载（2K/1K）看 **v0.29.0**；重负载（64K）7 个版本基本打平，差异被 prefill 吃掉 |
| 有没有性能回退？ | 有 1 处：**v0.25.1 在 2K/1K 上 0.47 req/s（TTFT 5.3 s）**，比 v0.26.0 的 0.74/438 ms 差一个量级，两次复测一致 |
| 0.25/0.26 为什么崩？ | FlashInfer `mnnvl` 融合 allreduce 需要 NVSwitch 多播，本机没有；加 `fuse_allreduce_rms=false` 后 7 个组合全部正常（第 6 节） |
| 还缺什么？ | 千问三模型的 TP8+EP 全量矩阵（因外部占卡暂停）；M2.7 recipe 负载 5 个版本（第 7 节） |

## 目录

- [0. 复现命令](#0-复现命令)
- [1. 测试口径（控制变量）](#1-测试口径控制变量)
- [2. MiniMax-M2.7](#2-minimax-m27)
- [3. Qwen3.6-35B-A3B](#3-qwen36-35b-a3b)
- [4. Qwen3.8-Flash-Next](#4-qwen38-flash-next)
- [5. Qwen3.5-397B-A17B](#5-qwen35-397b-a17b)
- [6. 工程性问题：0.25.1 / 0.26.0 开 CUDA Graph 必崩](#6-工程性问题0251--0260-开-cuda-graph-必崩)
- [7. 覆盖度与缺口](#7-覆盖度与缺口)

---

## 0. 复现命令

### 0.1 共用前缀

```bash
# 7 个镜像标签，逐个替换 <VER>
VER=v0.31.0                 # v0.31.0 v0.29.0 v0.26.0 v0.25.1 v0.22.1 v0.19.1 v0.17.1
IMG=vllm/vllm-openai:$VER

# docker 启动的公共部分（端口 19000 只是宿主映射，压测走容器内 8000）
DC="docker run -d --name bifrost-bench --gpus all --ipc=host --shm-size 32g \
    -p 19000:8000 -e NCCL_NVLS_ENABLE=0 --ulimit memlock=-1 \
    -v /data/liuxin/Ax/models:/models:ro"

# 所有模式共用的服务端参数
COMMON_SERVE="--gpu-memory-utilization 0.95 --max-num-seqs 256 --max-num-batched-tokens 8192"

# 所有模式共用的压测命令模板
BENCH_BASE="docker exec bifrost-bench vllm bench serve --backend vllm \
  --base-url http://127.0.0.1:8000 --endpoint /v1/completions --dataset-name random \
  --save-result --result-dir /tmp"
```

### 0.2 M2.7 · TP4 · 负载 A（64K→2K，并发 8）· graph

```bash
$DC $IMG \
  --model /models/MiniMax_MiniMax-M2.7 --served-model-name m2.7 \
  --tensor-parallel-size 4 \
  --trust-remote-code --tool-call-parser minimax_m2 --reasoning-parser minimax_m2 --enable-auto-tool-choice \
  --disable-custom-all-reduce $COMMON_SERVE

# 等 /health 返回 200 后压测
$BENCH_BASE --model m2.7 --tokenizer /models/MiniMax_MiniMax-M2.7 \
  --random-input-len 65536 --random-output-len 2048 --num-prompts 32 --max-concurrency 8
```

### 0.3 M2.7 · TP4 · 负载 A · eager

与 0.2 完全相同，只在 `--disable-custom-all-reduce` 后加一个开关：

```bash
  ... --disable-custom-all-reduce --enforce-eager $COMMON_SERVE
```

### 0.4 M2.7 · TP4 · 负载 A · ⚙ workaround（仅 v0.26.0 / v0.25.1 需要）

与 0.2 完全相同，只多一个参数（**不加则启动阶段崩溃**，见第 6 节）：

```bash
  ... --disable-custom-all-reduce \
      --compilation-config '{"pass_config":{"fuse_allreduce_rms":false}}' \
      $COMMON_SERVE
```

### 0.5 M2.7 · TP4 · 负载 B（2K→1K，并发 8）· graph

服务端与 0.2 **完全一致**，只改压测长度（这是控制变量的关键）：

```bash
$BENCH_BASE --model m2.7 --tokenizer /models/MiniMax_MiniMax-M2.7 \
  --random-input-len 2048 --random-output-len 1024 --num-prompts 32 --max-concurrency 8
```

### 0.6 M2.7 · TP8+EP · 负载 A · graph（8 卡满配）

```bash
$DC $IMG \
  --model /models/MiniMax_MiniMax-M2.7 --served-model-name m2.7 \
  --tensor-parallel-size 8 --enable-expert-parallel \
  --trust-remote-code --tool-call-parser minimax_m2 --reasoning-parser minimax_m2 --enable-auto-tool-choice \
  --disable-custom-all-reduce \
  --compilation-config '{"pass_config":{"fuse_allreduce_rms":false}}' $COMMON_SERVE

$BENCH_BASE --model m2.7 --tokenizer /models/MiniMax_MiniMax-M2.7 \
  --random-input-len 65536 --random-output-len 2048 --num-prompts 32 --max-concurrency 8
```

### 0.7 M2.7 · TP4 · 官方 recipe 负载（2K→1K，并发 10，100 prompts）

服务端同 0.2，压测参数不同：

```bash
$BENCH_BASE --model m2.7 --tokenizer /models/MiniMax_MiniMax-M2.7 \
  --random-input-len 2048 --random-output-len 1024 --num-prompts 100 --max-concurrency 10
```

### 0.8 Qwen3.6-35B-A3B

```bash
# TP4 · graph
$DC $IMG --model /models/Qwen_Qwen3.6-35B-A3B --served-model-name qwen3.6-35b \
  --tensor-parallel-size 4 --trust-remote-code --disable-custom-all-reduce $COMMON_SERVE

# TP4 · eager：同上再加 --enforce-eager

# TP8+EP · graph（本轮仅 v0.26.0 / v0.25.1 跑过，其余待补）
$DC $IMG --model /models/Qwen_Qwen3.6-35B-A3B --served-model-name qwen3.6-35b \
  --tensor-parallel-size 8 --enable-expert-parallel --trust-remote-code \
  --disable-custom-all-reduce \
  --compilation-config '{"pass_config":{"fuse_allreduce_rms":false}}' $COMMON_SERVE

$BENCH_BASE --model qwen3.6-35b --tokenizer /models/Qwen_Qwen3.6-35B-A3B \
  --random-input-len 65536 --random-output-len 2048 --num-prompts 32 --max-concurrency 8
```

### 0.9 Qwen3.8-Flash-Next

```bash
$DC $IMG --model /models/Qwen_Qwen3.8-Flash-Next --served-model-name qwen3.8-flash \
  --tensor-parallel-size 4 --trust-remote-code --disable-custom-all-reduce $COMMON_SERVE
# eager：再加 --enforce-eager

$BENCH_BASE --model qwen3.8-flash --tokenizer /models/Qwen_Qwen3.8-Flash-Next \
  --random-input-len 65536 --random-output-len 2048 --num-prompts 32 --max-concurrency 8
```

### 0.10 Qwen3.5-397B-A17B（bf16 752 GB，必须占满 8 卡）

```bash
# TP8 · graph
$DC $IMG --model /models/Qwen_Qwen3.5-397B-A17B --served-model-name qwen3.5-397b \
  --tensor-parallel-size 8 --trust-remote-code --disable-custom-all-reduce $COMMON_SERVE

# TP8+EP · graph（本轮仅 v0.26.0 / v0.25.1 跑过）
$DC $IMG --model /models/Qwen_Qwen3.5-397B-A17B --served-model-name qwen3.5-397b \
  --tensor-parallel-size 8 --enable-expert-parallel --trust-remote-code \
  --disable-custom-all-reduce \
  --compilation-config '{"pass_config":{"fuse_allreduce_rms":false}}' $COMMON_SERVE

$BENCH_BASE --model qwen3.5-397b --tokenizer /models/Qwen_Qwen3.5-397B-A17B \
  --random-input-len 65536 --random-output-len 2048 --num-prompts 32 --max-concurrency 8
```

### 0.11 用评测脚本一键复现（上面命令的实际出处）

```bash
cd /data/liuxin/Ax/bifrost

# 打印某个模式在指定版本下将要执行的完整命令（不启容器）
bash scripts/bench/vllm_bench_one.sh --model-key m2.7-tp8ep-graph --version v0.31.0 --dry-run

# 跑单个版本
bash scripts/bench/vllm_bench_one.sh --model-key m2.7-tp8ep-graph --version v0.31.0

# 跑一整轮矩阵（严格串行，带 flock 单实例锁）
bash scripts/bench/vllm_matrix.sh --model-key m2.7-tp8ep-graph --mode graph \
  v0.31.0 v0.29.0 v0.26.0 v0.25.1 v0.22.1 v0.19.1 v0.17.1
```

`--model-key` 对应 `scripts/bench/models/<key>.env`，模型路径、并行策略、负载、输出目录都在其中；`--mode graph|eager` 只切换是否加 `--enforce-eager`。

---

## 1. 测试口径（控制变量）

| 项 | 取值 |
|---|---|
| 负载 A（长上下文） | 输入 65536 / 输出 2048 / 32 prompts / 并发 8 |
| 负载 B（短上下文） | 输入 2048 / 输出 1024 / 32 prompts / 并发 8 |
| 并行 | TP4（4 卡）或 TP8+EP（8 卡满配，`--enable-expert-parallel`） |
| graph 模式 | 默认（CUDA graph + torch.compile） |
| eager 模式 | `--enforce-eager`（仅历史对照） |
| 公共参数 | `--gpu-memory-utilization 0.95 --max-num-seqs 256 --max-num-batched-tokens 8192 --disable-custom-all-reduce` |
| 环境变量 | `NCCL_NVLS_ENABLE=0`（本机无 NVSwitch 多播） |
| 压测工具 | `vllm bench serve`（容器内打容器内 8000），同版本同参数 |

同一模型跨版本的所有参数完全一致，只换镜像标签。

**本文各表速查**

| 表 | 模型 | 并行 | 负载 | 版本数 |
|---|---|---|---|---|
| 2.1 | M2.7 | TP4 | A：64K→2K，并发 8 | 7 ✅ |
| 2.2 | M2.7 | TP4 | B：2K→1K，并发 8 | 7 ✅ |
| 2.3 | M2.7 | TP8+EP | A：64K→2K，并发 8 | 7 ✅ |
| 2.4 | M2.7 | TP4 | recipe：2K→1K，并发 10 | 2 |
| 2.5 | M2.7 | TP4 | A（eager 对照） | 7 ✅ |
| 3.1 / 3.2 | Qwen3.6-35B | TP4 | A（graph / eager） | 7 ✅ / 7 ✅ |
| 4.1 / 4.2 | Qwen3.8-Flash | TP4 | A（graph / eager） | 2 / 2 |
| 5.1 | Qwen3.5-397B | TP8 | A（graph） | 7 ✅ |

---

## 2. MiniMax-M2.7

### 2.1 TP4 · 负载 A（64K→2K，并发 8）· graph

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.14 | 291 | 9603 | 10292 ms | 21967 ms | 22.4 ms | ✅ |
| v0.29.0 | 0.14 | 294 | 9701 | 10872 ms | 22273 ms | 21.9 ms | ✅ |
| v0.26.0 ⚙ | 0.14 | 280 | 9255 | 10261 ms | 22278 ms | 23.5 ms | ✅ |
| v0.25.1 ⚙ | 0.14 | 280 | 9234 | 10309 ms | 22276 ms | 23.5 ms | ✅ |
| v0.22.1 | 0.14 | 277 | 9134 | 10409 ms | 22621 ms | 23.8 ms | ✅ |
| v0.19.1 | 0.13 | 265 | 8756 | 10720 ms | 29316 ms | 24.9 ms | ✅ |
| v0.17.1 | 0.13 | 267 | 8795 | 10624 ms | 28931 ms | 24.8 ms | ✅ |

### 2.2 TP4 · 负载 B（2K→1K，并发 8）· graph

*v0.19.1 首次 warmup 偶发 cudaErrorNotPermitted，重试通过*

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.78 | 794 | 2382 | 449 ms | 1348 ms | 9.6 ms | ✅ |
| v0.29.0 | 0.81 | 831 | 2493 | 562 ms | 1465 ms | 9.1 ms | ✅ |
| v0.26.0 ⚙ | 0.74 | 758 | 2275 | 438 ms | 1300 ms | 10.1 ms | ✅ |
| v0.25.1 ⚙ | 0.47 | 486 | 1457 | 5304 ms | 19480 ms | 11.3 ms | ✅ |
| v0.22.1 | 0.69 | 705 | 2115 | 681 ms | 1921 ms | 10.7 ms | ✅ |
| v0.19.1 | 0.58 | 597 | 1792 | 1309 ms | 8564 ms | 12.1 ms | ✅ |
| v0.17.1 | 0.58 | 598 | 1794 | 1312 ms | 8145 ms | 12.1 ms | ✅ |

### 2.3 TP8+EP · 负载 A（64K→2K，并发 8）· graph

*8 卡满配；0.17.1/0.19.1 的 EP 老死锁未复现*

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.20 | 400 | 13184 | 7012 ms | 14026 ms | 16.6 ms | ✅ |
| v0.29.0 | 0.20 | 417 | 13753 | 7324 ms | 14130 ms | 15.6 ms | ✅ |
| v0.26.0 | 0.18 | 363 | 11989 | 6899 ms | 14355 ms | 18.6 ms | ✅ |
| v0.25.1 | 0.18 | 362 | 11956 | 6908 ms | 14339 ms | 18.7 ms | ✅ |
| v0.22.1 | 0.17 | 344 | 11338 | 6972 ms | 14589 ms | 19.8 ms | ✅ |
| v0.19.1 | 0.16 | 329 | 10850 | 7307 ms | 21428 ms | 20.7 ms | ✅ |
| v0.17.1 | 0.16 | 328 | 10814 | 6655 ms | 21000 ms | 21.1 ms | ✅ |

### 2.4 TP4 · 官方 recipe 负载（2K→1K，并发 10，100 prompts）· graph

*早期批次，仅跑通 2 版（外部占卡打断）*

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.88 | 902 | 2707 | 373 ms | 1544 ms | 10.7 ms | ✅ |
| v0.29.0 | 0.92 | 939 | 2816 | 502 ms | 1552 ms | 10.2 ms | ✅ |
| v0.26.0 | — | — | — | — | — | — | ❌ startup_failed |
| v0.25.1 | — | — | — | — | — | — | 未测 |
| v0.22.1 | — | — | — | — | — | — | 未测 |
| v0.19.1 | — | — | — | — | — | — | 未测 |
| v0.17.1 | — | — | — | — | — | — | 未测 |

### 2.5 历史对照 · TP4 · 负载 A · eager

*eager 关闭全部图与编译，吞吐仅为 graph 的 1/3～1/5*

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.04 | 80 | 2627 | 6877 ms | 23369 ms | 96.8 ms | ✅ |
| v0.29.0 | 0.04 | 91 | 3015 | 6877 ms | 22909 ms | 84.0 ms | ✅ |
| v0.26.0 | 0.05 | 95 | 3150 | 7028 ms | 22949 ms | 80.1 ms | ✅ |
| v0.25.1 | 0.05 | 95 | 3129 | 7670 ms | 22954 ms | 80.3 ms | ✅ |
| v0.22.1 | 0.04 | 78 | 2573 | 7225 ms | 24184 ms | 98.8 ms | ✅ |
| v0.19.1 | 0.04 | 80 | 2629 | 7630 ms | 30866 ms | 96.4 ms | ✅ |
| v0.17.1 | 0.04 | 78 | 2572 | 7600 ms | 30675 ms | 98.7 ms | ✅ |

**M2.7 小结**

- 负载 A（64K）下 7 个版本几乎无差异（0.13–0.14 req/s），**prefill 完全主导**，版本优化体现不出来。
- 负载 B（2K/1K）版本差异明显：**v0.29.0 最强**（0.81 req/s），v0.17.1/v0.19.1 最弱（0.58），差 40%。
- **TP8+EP 全面优于 TP4**：0.16–0.20 vs 0.13–0.14 req/s，且 TTFT 从 ~10.5 s 降到 ~7.0 s（少 33%）。
- v0.25.1 在负载 B 上有真实回退（0.47 req/s / TTFT 5.3 s，两次复测一致），是本轮唯一的性能异常点。

---

## 3. Qwen3.6-35B-A3B

### 3.1 TP4 · 负载 A · graph

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.42 | 868 | 28637 | 4127 ms | 7357 ms | 7.2 ms | ✅ |
| v0.29.0 | 0.43 | 880 | 29046 | 4183 ms | 7666 ms | 7.0 ms | ✅ |
| v0.26.0 ⚙ | 0.37 | 765 | 25259 | 4366 ms | 7996 ms | 8.3 ms | ✅ |
| v0.25.1 ⚙ | 0.39 | 809 | 26681 | 3713 ms | 6731 ms | 8.1 ms | ✅ |
| v0.22.1 | 0.35 | 720 | 23750 | 5007 ms | 9963 ms | 8.6 ms | ✅ |
| v0.19.1 | 0.33 | 676 | 22303 | 5003 ms | 15599 ms | 9.3 ms | ✅ |
| v0.17.1 | 0.33 | 673 | 22202 | 4568 ms | 15204 ms | 9.6 ms | ✅ |

### 3.2 TP4 · 负载 A · eager

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.05 | 102 | 3366 | 2923 ms | 10257 ms | 76.7 ms | ✅ |
| v0.29.0 | 0.05 | 101 | 3349 | 2597 ms | 8698 ms | 77.3 ms | ✅ |
| v0.26.0 | 0.05 | 100 | 3316 | 2881 ms | 10475 ms | 77.8 ms | ✅ |
| v0.25.1 | 0.05 | 99 | 3257 | 2950 ms | 10727 ms | 79.2 ms | ✅ |
| v0.22.1 | 0.05 | 101 | 3345 | 3557 ms | 13317 ms | 76.8 ms | ✅ |
| v0.19.1 | 0.05 | 105 | 3477 | 3075 ms | 16435 ms | 73.9 ms | ✅ |
| v0.17.1 | 0.05 | 94 | 3099 | 3048 ms | 16226 ms | 83.2 ms | ✅ |

**小结**：graph 组 v0.29.0 最高（0.43 req/s），0.17.1/0.19.1 与 0.22.1 落在 0.33–0.35；graph 比 eager 快约 8–9 倍。

---

## 4. Qwen3.8-Flash-Next

### 4.1 TP4 · 负载 A · graph

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.25 | 516 | 17018 | 7168 ms | 14728 ms | 12.0 ms | ✅ |
| v0.29.0 | 0.22 | 449 | 14824 | 8893 ms | 21455 ms | 13.4 ms | ✅ |
| v0.26.0 | — | — | — | — | — | — | ❌ startup_failed |
| v0.25.1 | — | — | — | — | — | — | ❌ startup_failed |
| v0.22.1 | — | — | — | — | — | — | ❌ startup_failed |
| v0.19.1 | — | — | — | — | — | — | ❌ startup_failed |
| v0.17.1 | — | — | — | — | — | — | ❌ startup_failed |

### 4.2 TP4 · 负载 A · eager

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.03 | 71 | 2348 | 5169 ms | 19820 ms | 109.4 ms | ✅ |
| v0.29.0 | 0.04 | 72 | 2392 | 5959 ms | 22876 ms | 107.1 ms | ✅ |
| v0.26.0 | — | — | — | — | — | — | ❌ startup_failed |
| v0.25.1 | — | — | — | — | — | — | ❌ startup_failed |
| v0.22.1 | — | — | — | — | — | — | ❌ startup_failed |
| v0.19.1 | — | — | — | — | — | — | ❌ startup_failed |
| v0.17.1 | — | — | — | — | — | — | ❌ startup_failed |

**小结**：只有 v0.29.0 / v0.31.0 认识该架构；v0.26.0 及更早 5 个版本加载即失败（`does not recognize architecture qwen4_exp`），与性能无关。graph 组 v0.31.0 明显优于 v0.29.0（0.25 vs 0.22 req/s，TTFT 7.2 s vs 8.9 s）。

---

## 5. Qwen3.5-397B-A17B

### 5.1 TP8 · 负载 A · graph

*bf16 权重 752 GB，必须 TP8 占满 8 卡*

| 版本 | req/s | 输出 tok/s | 总 tok/s | TTFT 均值 | TTFT p99 | TPOT | 状态 |
|---|---|---|---|---|---|---|---|
| v0.31.0 | 0.21 | 434 | 14338 | 7360 ms | 15062 ms | 14.8 ms | ✅ |
| v0.29.0 | 0.22 | 441 | 14549 | 7632 ms | 15250 ms | 14.4 ms | ✅ |
| v0.26.0 ⚙ | 0.19 | 394 | 13001 | 6677 ms | 14675 ms | 17.0 ms | ✅ |
| v0.25.1 ⚙ | 0.19 | 386 | 12734 | 6850 ms | 15219 ms | 17.3 ms | ✅ |
| v0.22.1 | 0.16 | 336 | 11083 | 9460 ms | 21669 ms | 19.1 ms | ✅ |
| v0.19.1 | 0.17 | 351 | 11580 | 7904 ms | 24575 ms | 18.8 ms | ✅ |
| v0.17.1 | 0.17 | 344 | 11356 | 7784 ms | 24058 ms | 19.4 ms | ✅ |

**小结**：v0.29.0 最高（0.22 req/s / TTFT 7.6 s），v0.22.1 最差（0.16 / 9.5 s）。0.26.0/0.25.1 加 workaround 后为 0.19，落在中游。

---

## 6. 工程性问题：0.25.1 / 0.26.0 开 CUDA Graph 必崩

| 项 | 内容 |
|---|---|
| 现象 | 启动到 `Profiling CUDA graph memory` 后 worker 全挂，`CUDA error: an illegal memory access` |
| 崩溃点漂移 | 默认崩在 DeepGEMM `fp8_gemm_nt`；关掉 DeepGEMM 后改崩 FlashAttention → 说明不是某个 kernel 算错 |
| 根因 | FlashInfer `mnnvl` 融合 allreduce 需要 NVSwitch 多播；本机 H200-NVL 无独立 NVSwitch，workspace 却仍创建成功，融合 kernel 在图捕获阶段越界（vllm#49311 / PR #48075，修复尚未合入） |
| 解法 | `--compilation-config {"pass_config":{"fuse_allreduce_rms":false}}`（退化为普通 NCCL allreduce + 原生 RMSNorm），实测 7 个模型/版本组合全部正常启动 |
| 反例 | `VLLM_ALLREDUCE_USE_SYMM_MEM=0`、`VLLM_USE_DEEP_GEMM=0`、`VLLM_USE_DEEP_GEMM_E8M0=0`、`VLLM_MEMORY_PROFILER_ESTIMATE_CUDAGRAPHS=0` 均无效 |
| 备选 | `-O1`（保留编译、关图捕获）也能起，但丢掉 decode 图收益；`--enforce-eager` 最差 |

---

## 7. 覆盖度与缺口

| 组合 | 状态 |
|---|---|
| M2.7 · TP4 · 负载 A/B · 7 版 | ✅ 全齐 |
| M2.7 · TP8+EP · 负载 A · 7 版 | ✅ 全齐 |
| M2.7 · eager · 负载 A · 7 版 | ✅ 全齐（历史） |
| M2.7 · recipe 负载 · 7 版 | ⚠️ 仅 0.31.0 / 0.29.0 |
| Qwen3.6 · TP4 · graph/eager · 7 版 | ✅ 全齐 |
| Qwen3.6 · TP8+EP · 7 版 | ⚠️ 仅 0.26.0 / 0.25.1（workaround 补跑） |
| Qwen3.8 · graph/eager | ⚠️ 仅 0.31.0 / 0.29.0（其余版本不支持架构） |
| Qwen3.5-397B · TP8 · graph · 7 版 | ✅ 全齐 |
| Qwen3.5-397B · TP8+EP · 7 版 | ⚠️ 仅 0.26.0 / 0.25.1 |
| 千问 TP8+EP 全量矩阵 | ⏸ 因外部容器 `cphd1020_nv` 占卡暂停，未开跑 |

---

*生成时间 2026-10-23 07:00Z · 原始数据 `/workspace/bifrost/bench/<数据集>/summary.tsv` + `result.json`*
