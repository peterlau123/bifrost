# 待跑任务清单（截至 2026-10-22 01:30 UTC）

> 全部任务已停止：6 个 systemd unit 均 inactive、无残留进程、无 `bifrost-bench` 容器、8 卡 0 MiB。
> 下面是**尚未产出数据**的部分，以及逐条续跑命令。

## 一、MiniMax-M2.7 · recipe 负载（2048 in / 1024 out / 并发 10 / 100 prompts）

### 1. graph 模式（TP4）— 已完成 2/7，缺 4 版

| 版本 | 状态 |
|---|---|
| v0.31.0 | ✅ 0.88 req/s，TTFT 372.8 ms |
| v0.29.0 | ✅ 0.92 req/s，TTFT 502.1 ms |
| v0.26.0 | ❌ CUDA graph 捕获崩溃（版本缺陷，无法补） |
| v0.25.1 | ⏳ 被打断（预计同样崩，需跑一次确认） |
| v0.22.1 / v0.19.1 / v0.17.1 | ⏳ 被外部占卡打断，**可补跑** |

```bash
systemd-run --unit=bench-resume1 --collect \
  bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key m2.7-recipe --mode graph v0.25.1 v0.22.1 v0.19.1 v0.17.1
```

### 2. eager 模式（TP4）— 整轮 7 版未跑

```bash
systemd-run --unit=bench-resume2 --collect \
  bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key m2.7-recipe --mode eager
```

### 3. TP8 + EP + 关 CUDA graph（占满 8 卡）— 整轮 7 版未跑

前提：**8 张卡必须全空闲**（TP8 用满）。已验证该配置能起（KV 3.5M tokens、103.56 GiB/卡、`/health` 200）。

```bash
systemd-run --unit=bench-resume3 --collect \
  bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key m2.7-recipe-tp8ep --mode eager
```

预计 0.17 / 0.19 会撞 `shm_broadcast` 死锁（EP 在旧版的问题），如实记录即可。

---

## 二、Qwen3.5-397B-A17B（TP8，752 G，单版加载 6~20 分钟）

| 项 | 状态 |
|---|---|
| graph · v0.31.0 / v0.29.0 | ✅ 0.21 / 0.22 req/s |
| graph · v0.26.0 | ❌ `CUBLAS_STATUS_EXECUTION_FAILED`（与 graph 缺陷同源） |
| graph · v0.25.1 | ⏸ 按用户要求中止 |
| graph · v0.22.1 / v0.19.1 / v0.17.1 | ⏳ 未跑 |
| **eager · 全部 7 版** | ⏳ 未跑 |

```bash
# 补 graph 3 版（约 1.5 h）
systemd-run --unit=bench-resume4 --collect \
  bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key qwen3.5-397b --mode graph v0.22.1 v0.19.1 v0.17.1
# 补 eager 全 7 版（约 4~6 h）
systemd-run --unit=bench-resume5 --collect \
  bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key qwen3.5-397b --mode eager
```

---

## 三、无法补齐（技术上不可行，非排期问题）

| 项 | 原因 |
|---|---|
| Qwen3.6-35B · graph 的 v0.26.0 / v0.25.1 | 这两版 CUDA graph 捕获必崩 |
| Qwen3.8-Flash · v0.26.0 及更早 5 版（graph 与 eager 都是） | Transformers/vLLM 不认识 `qwen4_exp` 架构 |
| M2.7 · graph 的 v0.26.0 | 同上（graph 缺陷） |
| M2.7 · 纯 TP8（不带 EP） | fp8 block_n=128 整除约束，权重加载即失败 |

---

## 四、续跑前检查清单

1. `nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader` 必须为空（只认 ≥512 MiB 的进程）。
2. `docker ps -a --filter name=bifrost-bench -q | wc -l` 必须为 0。
3. `systemctl is-active bench-pipeline bench-m27-recipe …` 必须全是 inactive。
4. 起任务用 `systemd-run --unit=<name> --collect`，不要直接交给 bifrost 任务（daemon 重启会杀）。
5. **跑的过程中不要修改 `scripts/bench/*.sh`**（bash 按字节偏移读脚本，改了会让运行中的实例死亡）。
6. 每轮结束后用生成器刷新汇总：
   ```bash
   python3 /opt/data/scripts/gen_all_results.py     # 重写 RESULTS.md
   ```

## 五、当前数据完整性

| 数据集 | 完成度 |
|---|---|
| M2.7 · 64k 长上下文 · eager | 7/7 |
| M2.7 · 64k 长上下文 · graph | 7/7（0.25/0.26 为 eager 代跑，表内已标注） |
| M2.7 · recipe 负载 · graph | 2/7 + 1 崩溃 |
| M2.7 · recipe 负载 · eager | 0/7 |
| M2.7 · recipe 负载 · TP8+EP eager | 0/7 |
| Qwen3.6-35B · graph / eager | 5/7 / 7/7 |
| Qwen3.8-Flash · graph / eager | 2/7 / 2/7 |
| Qwen3.5-397B · graph / eager | 2/4 / 0/7 |
