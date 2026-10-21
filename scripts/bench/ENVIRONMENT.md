# H200 评测环境限制（实测记录）

本机：H200 × 8，驱动 **595.45.04**（Open Kernel Module），NVLink 26.562 GB/s × 18 links，NVSwitch 在位。
以下限制均**实测确认**，不是推测；每条都给了绕行方式和代价。

## 1. NVLS 多播不可用（全局关闭）

**现象**：新版 vLLM（捆绑 NCCL 2.30.x）启动即失败，两种表象：

```
# 0.29 / 0.31
RuntimeError: NCCL error: unhandled cuda error        ← ncclCommInitRank
# 日志里伴随
CUDASymmetricMemory.cu:595 Warning: CUDA driver error: the operation cannot be
performed in the present state (function init_multicast_for_block)

# 0.25 / 0.26
Failed: Cuda error /workspace/csrc/custom_all_reduce.cuh:455
        'an illegal memory access was encountered'
```

**根因**：NVLS（NVLink SHARP，交换机内归约 + 多播）需要驱动暴露 CUDA multicast object
接口，本机驱动/NVSwitch 固件组合下 `cuMulticastCreate` 被拒（`init_multicast_for_block`
失败）。NCCL 2.30 默认启用 NVLS，于是初始化直接失败；旧版 NCCL（0.22 及更早）静默降级到
ring/tree，所以不受影响。

**处置**：**全平台统一关闭**，所有版本、所有模型一致：

```bash
-e NCCL_NVLS_ENABLE=0
--disable-custom-all-reduce        # vLLM 自研 AR kernel 同样依赖多播路径
```

**代价**：all-reduce 走 GPU SM 而不是交换机，大 TP 下有几个百分点的性能损失。
因为对所有版本统一施加，版本间对比仍然有效。

> 真正的修法在基础设施层：升级驱动 / NVSwitch 固件到支持 SHARP 的版本。当前不可行。

## 2. 纯 TP8 在 fp8 分块量化模型上不成立

`MiniMax-M2.7`：`moe_intermediate_size=1536`、fp8 `weight_block_size=[128,128]`，
TP8 时每卡切到 `1536/8=192`，不是 128 的整数倍：

```
ValueError: The output_size of gate's and up's weight = 192
            is not divisible by weight quantization block_n = 128
```

官方 recipe 也写明 pure TP8 不支持，要求 TP4 或 TP+EP。**处置**：该模型固定 TP4。

## 3. DeepGEMM 与当前驱动不兼容（0.25 / 0.26）

```
RuntimeError: CUDA driver error (.../deepgemm-src/csrc/.../jit/handle.hpp:154)
torch.AcceleratorError: CUDA error: an illegal memory access was encountered
```

关掉 custom all-reduce 后仍崩，说明是 GEMM kernel 本身。0.17/0.19/0.22/0.29/0.31 均正常。
要拿这两版数字只能换 MoE backend（`--moe-backend triton`），属偏离统一口径，需单独成表。

## 4. 长任务必须脱离任务调度器 cgroup

有人在用 bifrost 做"服务重启耗时"测量（`systemctl restart bifrost` 前后打时间戳）。
任务跑在 daemon 的 cgroup 里，重启会连带杀掉跑到一半的压测。

**处置**：长矩阵用 `systemd-run --unit=bench-matrix --collect ...` 起，脱离该 cgroup。
另：daemon 宕机恢复会按 stale claim 重新拉起旧任务，两个矩阵并存会抢同一容器名/端口，
矩阵脚本已加 `flock` 单实例锁。

## 5. 模型清单与并行策略

| 模型 | 权重 | 精度 | 架构 | TP | 说明 |
|---|---|---|---|---|---|
| MiniMax-M2.7 | 215 G | fp8 分块 | MiniMaxM2ForCausalLM | 4 | TP8 不可用（见 §2） |
| Qwen3.6-35B-A3B | 67 G | bf16 | Qwen3_5MoeForConditionalGeneration | 4 | 全部版本可加载 |
| Qwen3.8-Flash-Next | 336 G | bf16 | Qwen4ExpForConditionalGeneration | 4 | 仅 0.29 / 0.31 可加载 |
| Qwen3.5-397B-A17B | 752 G | bf16 | Qwen3_5MoeForConditionalGeneration | 8 | 必须占满 8 卡（4 卡装不下） |

## 6. 压测口径（所有模型、所有版本一致）

```
输入 65536 tokens / 输出 2048 tokens / 32 prompts / 并发 8
backend vllm，endpoint /v1/completions，dataset random，request_rate=inf
gpu-memory-utilization 0.95，max-num-seqs 256，max-num-batched-tokens 8192
```
