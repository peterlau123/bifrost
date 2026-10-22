# vLLM 多版本评测 · 问题记录

> 机器：H200 × 8（驱动 595.45.04，Open Kernel Module，NVSwitch 在位，NVLink 26.562 GB/s）
> 评测对象：MiniMax-M2.7 / Qwen3.6-35B-A3B / Qwen3.8-Flash-Next / Qwen3.5-397B-A17B
> 镜像：`vllm/vllm-openai` v0.17.1 / v0.19.1 / v0.22.1 / v0.25.1 / v0.26.0 / v0.29.0 / v0.31.0
> 时间：2026-10-21 ~ 10-22

按「现象 → 原因 → 解决」记录，分为**平台/驱动**、**vLLM 版本自身**、**压测工具（vllm bench）**、**调度与运维**四类。最后列出仍未解决或需要决策的项。

---

## 一、平台 / 驱动层

### 1. NVLS 多播不可用，新版 vLLM 起不来

**现象**
- v0.29.0 / v0.31.0：`RuntimeError: NCCL error: unhandled cuda error`（发生在 `ncclCommInitRank`），引擎初始化直接失败。
- v0.25.1 / v0.26.0：`Failed: Cuda error /workspace/csrc/custom_all_reduce.cuh:455 'an illegal memory access was encountered'`
- 日志里伴有：`CUDASymmetricMemory.cu:595 Warning: CUDA driver error: the operation cannot be performed in the present state (function init_multicast_for_block)`

**原因**
NVLS（NVLink SHARP，交换机内归约 + 多播）需要驱动暴露 CUDA multicast object 接口。本机驱动 595.45.04 + NVSwitch 固件组合下 `cuMulticastCreate` 被拒（`init_multicast_for_block` 失败）。
NCCL 2.30（0.29/0.31 捆绑）默认启用 NVLS，于是初始化即失败；0.22 及更早的 NCCL 会静默降级到 ring/tree，所以不受影响。
物理链路是好的（`nvidia-smi nvlink` 显示 18 条 26.562 GB/s），缺的是交换机内归约这条软件路径。

**解决**
统一加环境变量 `NCCL_NVLS_ENABLE=0`（所有版本、所有模型一致，保证可比）。
真正的修法在基础设施层：升级驱动 / NVSwitch 固件到支持 SHARP 的版本。

---

### 2. vLLM 自研 custom all-reduce 内核非法访存

**现象**：v0.25.1 / v0.26.0 在 CUDA graph 捕获阶段报 `custom_all_reduce.cuh:455 'an illegal memory access was encountered'`，8 个 worker 全挂。

**原因**：vLLM 自带的 custom all-reduce kernel 在 Hopper 上走多播/多卡屏障路径，与本机驱动能力不匹配（与第 1 条同源）。

**解决**：统一加 `--disable-custom-all-reduce`，退回 NCCL all-reduce。
注意该参数在新版 `vllm serve --help`（精简模式）里**不显示**，要确认存在得 `grep -rn "disable-custom-all-reduce" .../vllm/engine/arg_utils.py`。

---

### 3. 0.25 / 0.26 的 CUDA graph 捕获必崩（曾误判为 DeepGEMM 问题）

**现象**
```
buf7 = torch.ops.vllm.dynamic_flashinfer_deepgemm_blockscale_gemm.default(...)
RuntimeError: CUDA driver error (.../deepgemm-src/csrc/jit/handle.hpp:154):
                700 (CUDA_ERROR_ILLEGAL_ADDRESS)
```
（Qwen3.5-397B 上表现为 `RuntimeError: CUDA error: CUBLAS_STATUS_EXECUTION_FAILED when calling cublasGemmEx`）

**原因**
不是 DeepGEMM 本身的问题。排除过程：

| 试验 | 结果 | 排除什么 |
|---|---|---|
| 默认（graph + DeepGEMM） | 崩在 fp8 线性 GEMM | — |
| `--moe-backend triton` | 仍崩 | 不是 MoE 路径 |
| `--linear-backend cutlass` | 走得更远（到 CUDA graph profiling）后 worker 死 | 不是某个 GEMM 实现 |
| **`--enforce-eager`（禁用 CUDA graph）** | **正常启动，32/32 成功** | 崩溃点在**图捕获 / 显存剖析**阶段 |

结论：0.25 / 0.26 的 CUDA graph 捕获路径在本机会非法访存；0.17/0.19/0.22/0.29/0.31 的捕获路径正常。报错长在 DeepGEMM 那行，是因为它是捕获期间第一个被调用的 kernel —— 是表象不是根因。

**解决**：`--enforce-eager`。代价是解码慢约 3 倍（TPOT 22 ms → 79 ms）。
为了全表同口径，最终**全平台统一 eager**，graph 模式的结果单独存档作为基线。

---

### 4. fp8 分块量化模型的纯 TP8 在数学上不成立

**现象**
```
ValueError: The output_size of gate's and up's weight = 192
            is not divisible by weight quantization block_n = 128
```

**原因**：MiniMax-M2.7 是 fp8 分块量化 MoE（`moe_intermediate_size=1536`，`weight_block_size=[128,128]`）。TP8 时每卡切到 `1536/8=192`，不是 128 的整数倍，GEMM kernel 直接拒绝。官方 recipe 的 `strategy_overrides` 也写明 pure TP8 不支持。

**解决**：该模型固定 TP4（1536/4=384 ✓）。备选是 TP8+EP 或 DP8+EP（专家维度切分，保持 1536 完整），但 EP 稳定性差（见第 1、2 条），最终未采用。

---

### 5. 752 GB 的模型 4 卡装不下

**现象**：Qwen3.5-397B-A17B 权重 752 GB（bf16），TP4 只有约 536 GB 可用显存，起不来。

**原因**：4 × 141 GB × 0.95 ≈ 536 GB < 752 GB。

**解决**：该模型固定 TP8（≈1072 GB）。
**这是唯一在模型之间不一致的参数**，同一模型内部 7 个版本仍完全一致；跨模型比较本身不是变量控制问题，但需要在报告里注明。

---

### 5b. 补充实测：TP8 + EP 在 eager 下是**可以跑**的

**现象**：M2.7 用 `--tensor-parallel-size 8 --enable-expert-parallel --enforce-eager` 启动，成功进入服务状态：

```
Available KV cache memory: 103.56 GiB
GPU KV cache size: 3,503,008 tokens, Maximum concurrency for 204,800 tokens per request: 17.10x
GET /health HTTP/1.1" 200 OK
```
8 卡各占 136.9 GB，容器稳定运行。

**原因分析**
- EP（专家并行）按专家维度切分，`moe_intermediate_size=1536` 在每个 rank 上保持完整，所以绕开了 fp8 `block_n=128` 的整除约束（第 4 条）。
- 之前 EP 失败的两次都不是 EP 本身：v0.19.1 是 `shm_broadcast` 死锁（版本缺陷），v0.25.1 是 CUDA graph 捕获崩溃（第 3 条）。
- 关掉 graph 之后，这两个坑都不触发，EP 可用。

**结论**
- 想用满 8 卡跑 M2.7：`TP8 + --enable-expert-parallel + --enforce-eager`，可行。
- 本次主表仍用 TP4（与已产出的 7 个版本数据保持同口径），EP 8 卡作为备选方案记录在此。

---

## 二、vLLM 版本自身

### 6. 老版本不认识新模型架构

**现象**
```
Value error, The checkpoint you are trying to load has model type `qwen4_exp`
but Transformers does not recognize this architecture.
```

**原因**：Qwen3.8-Flash-Next 的 `Qwen4ExpForConditionalGeneration` 只有 v0.29 / v0.31 的模型注册表里有；v0.17~v0.26 完全没有。同理 Qwen3.5/Qwen3.6 的 `Qwen3_5MoeForConditionalGeneration` 在全部 7 个版本里都有（这两个模型可以全版本跑）。

**解决**：无法解决，只能如实记录「架构不支持」。这类版本必须在报告里与「跑得慢」区分开。

---

### 7. 更低版本的 vLLM 未必更差（反直觉，但是实测）

**现象**：MiniMax-M2.7 的 v0.17.1 / v0.19.1 低于 recipe 声明的 `min_vllm_version: 0.20.0`，却都能正常跑（因为走了 TP4 而非 EP 路径）。

**原因**：`min_vllm_version` 是针对 recipe 推荐配置（含 EP 等特性）的最低要求，不是「低于此版本完全不能跑」。

**解决**：不做预判，全部按同一口径实跑，起不来的记状态，起得来的给数字。

---

## 三、压测工具（vllm bench serve）

### 8. bench 客户端要用 `--tokenizer` 指定路径，不能用 served name

**现象**
```
OSError: Can't load the configuration of 'm2.7'.
If you were trying to load it from 'https://huggingface.co/models', make sure ...
```

**原因**：`--model` 传的是 API 里的 served name（`m2.7`），bench 客户端会拿它去 HF 加载分词器；容器内没有网络也没有这个仓库。

**解决**：加 `--tokenizer /models/<模型目录>`。

---

### 9. 压测的 base-url 要用容器内视角

**现象**：100 个请求全部 `ConnectionRefusedError: [Errno 111] Connect call failed ('127.0.0.1', 19000)`，耗时 0.13 s，`completed=0 failed=100`。

**原因**：压测是 `docker exec` 进容器里执行的，容器内 vLLM 监听 8000；19000 只是宿主侧的端口映射，容器内不可达。

**解决**：健康检查用宿主侧 `127.0.0.1:$PORT`，压测用容器内 `127.0.0.1:8000`，两个 URL 分开定义。
（如果容器用 host 网络，两者都是 `$PORT`。）

---

### 10. 全部请求失败时 bench 仍写「漂亮的全 0 结果」并 exit 0

**现象**：`result.json` 里 `request_throughput=0.00`、`mean_ttft_ms=0.0`、`completed=0`、`failed=100`，脚本按 ok 记录了 —— 一张看起来正常、实际全是 0 的表。

**原因**：vllm bench 在全失败时只打一条 UserWarning，仍然写出结果文件并返回 0。

**解决**：解析时一并读 `completed` / `failed`，`completed == 0` 判为失败（状态记 `bench_all_failed`），不写进汇总。

---

## 四、调度 / 运维层

### 11. 任务调度器重启会杀掉长任务

**现象**：跑到一半的版本突然没有结果行、进程消失，留下一个容器继续占着 4 张卡。

**原因**：任务跑在 bifrost daemon 的 cgroup 里，`systemctl restart bifrost`（当时有人在测「服务重启耗时」，journal 里能看到 `t0=$(date +%s.%N); systemctl restart bifrost; ...` 的临时 unit）会连带杀掉所有子进程。

**解决**：长流水线改用 `systemd-run --unit=<name> --collect` 起，脱离 daemon 的 cgroup，谁重启 daemon 都不影响。
`--property=KillMode=process` 只杀主进程，所以停止时还需要按 PID 清理残留子进程。

---

### 12. 调度器宕机恢复会把「已取消」的任务重新拉起 → 两个矩阵并行

**现象**：progress.log 里出现两个版本同时加载（v0.19.1 与 v0.31.0），共用同一个容器名和端口，互相把对方的服务打掉；汇总表混进被污染的行。

**原因**：bifrost 的 stale claim 回收机制在 daemon 重启后按 claim marker 重新执行旧任务；`cancel` 没能阻止它再次被拉起。

**解决**
- 矩阵脚本头加单实例锁：`exec 9>"$OUTPUT_ROOT/.matrix.lock"; flock -n 9 || exit 1`。
- 发现并行迹象：`ps -eo pid,args | grep vllm_matrix` 应该只有一组（父 `sh -c` + 子 bash）。
- 纠正后要把被污染的行归档（`_invalid_racing.tsv`）而不是留在总表里。

---

### 13. 容器「在跑」不等于「没失败」，失败判定要快

**现象**：v0.26.0 的 worker 已经崩了（`illegal memory` ×16、`/health` 返回 000），但容器 `Up 23 minutes` 还在，脚本按容器存活继续等，要白等到 40 分钟超时。

**原因**：vLLM 的 API Server 进程与 EngineCore/worker 是分离的；worker 死了 API 容器不一定退出。

**解决**：判定时不能只看容器存活，要看 `/health` + 日志里的致命错误；确认没救就 `docker rm -f` 让它快速失败，跳到下一版（省下 30+ 分钟/版）。

---

### 14. 共享目录权限：root 建的目录，容器内 uid 10000 写不进去

**现象**：`touch commands/.probe → Permission denied`，client 提交不了任务；结果目录里 root 建的文件普通用户改不了。

**原因**：daemon 以 root 运行并创建目录（755/644），client 在容器里是 uid 10000。

**解决**：`sudo chmod -R a+rwX /data/liuxin/bifrost`（保留 root 属主，双方都可写）。
需要 root 才能改的文件，用一条 bifrost 任务（root 身份）去改，而不是在容器里硬试。

---

### 15. 镜像存储：单个镜像落盘约 2× tar，根分区装不下

**现象**：`/` 只剩 25~40 G，而单个 vLLM 镜像 load 后占 19~48 G（tar 的 2.0~2.2 倍，containerd 同时保留压缩 blob 与解包快照）；7 个镜像全展开约 290 G。

**解决（分析）**
- 一次只装一个镜像：load → 验证 → 跑 → `docker rm` + `docker rmi`。
- 空间要查 **daemon 自己的 data-root**：`df -BG --output=avail "$(docker info --format '{{.DockerRootDir}}')"`，查 `/` 会在 docker root 被迁走后给出完全错误的结论。

**解决（绕行，最终未采用）**
`--data-root` 挪不动 containerd 的 content store（新版 Docker 的镜像层由 containerd 解包，仍写 `/var/lib/containerd`）。真要换地方得另起 `containerd --root <新路径> --address <新 socket>` + `dockerd --containerd <该 socket>`，两者都用 `systemd-run` 起。
另外 overlay2/overlayfs 不能跑在 NFS 上，要放 NFS 只能 `truncate` 镜像文件 + `mkfs.ext4` + loop 挂载。

**最终解决**：运维把 docker root 迁到 `/nvme/docker`（13 T 可用），上述绕行全部拆除。

---

### 16. `docker load` 对损坏的层仍返回 0

**现象**：v0.17.1 的 tar 包 `docker load` 返回 0，但日志里有 `Error unpacking image ... failed to extract layer sha256:4abac35…: unexpected EOF`，镜像不可用（`docker create` 失败）。

**原因**：损坏的是内层 layer，load 只解到一半就报错，退出码却是 0。

**解决**
- load 后强制 `docker create`（或 `docker image inspect` + 实跑）验证，不能只看退出码。
- 重新拉包：本次用容器内的 registry 拉取脚本从 `docker.m.daocloud.io` 重新下载并生成 docker-archive tar（20.8 GB，6.3 分钟），再在 H200 上 `docker load` + `docker create` 验证通过。

---

### 17. 宿主 DNS 解析失败导致 `docker pull` 不可用

**现象**：`docker pull docker.m.daocloud.io/...` 报 `dial tcp: lookup docker.m.daocloud.io on 127.0.0.53:53: server misbehaving`。

**原因**：H200 宿主机的 systemd-resolved 解析异常（容器内 DNS 正常）。

**解决**：改在容器侧用 registry API 拉取（多线程分块 + 拼 docker-archive tar），产物直接在共享目录里，宿主 `docker load` 即可。

---

### 18. 临时目录被清理，工具脚本丢失

**现象**：拉取脚本 `pull_image.py` 第二天再跑时报 `No such file or directory`。

**原因**：它放在 `cache/scratch` 下，该目录对闲置 24 小时的文件做清理。

**解决**：复用技能目录里的同名副本（`skills/mlops/ml-artifact-staging/scripts/pull_image.py`）；长期要用的脚本不要放 scratch。

---

### 19. 压测进行中原地编辑脚本 → 正在跑的 bash 读错位而死

**现象**：v0.31.0 的 eager 压测跑到一半突然没有结果行、进程消失，留下一个孤儿容器占着 4 张卡；随后 6 个版本全被判 `gpu_busy` 跳过（汇总表一排 gpu_busy）。

**原因**：bash 按字节偏移增量读取脚本文件。我在流水线运行时修改了 `vllm_bench_one.sh` / `vllm_matrix.sh`，正在执行的实例读到错位的字节，中途死亡。
（这是**我自己的操作失误**，不是环境问题。）

**解决**
- 原则：要改脚本，先停流水线、清容器，改完再起。
- 守卫顺序也要修：先 `docker rm -f` 自家容器再判定是否被占用，否则会把自己上一轮的残留判成「外部占用」。

---

### 20. GPU 占用判定被瞬态探针误报

**现象**：`gpu-watch` 报「有外部进程占用 GPU」，但 `nvidia-smi` 显示 8 卡全是 0 MiB，进程名为空。

**原因**：`nvidia-smi` / `dcgm` / `nvidia-persistenced` 这类探针会瞬态出现在 compute-apps 列表里，`used_memory=0`，进程名读不到。

**解决**：判定条件收紧为「显存 ≥ 512 MiB 且进程名非空」；并且在自家压测容器不存在时不做判定（否则任何进程都会被算成外部）。

---

### 21. `pkill -f` 自杀：匹配到当前 shell 自己

**现象**：清理残留进程的任务本身返回 Failed、无输出；原因是它把自己那条 `sh -c "... pkill -f vllm_bench_one.sh ..."` 命令行也匹配上了，把自己杀了。

**原因**：`pkill -f` 匹配整条命令行，而当前 shell 的命令行里就包含这个模式。

**解决**：按 PID 精确 kill；或者把模式拆开写（`pkill -f "vllm_bench""_one.sh"`）；更稳的是先 `ps` 拿 PID 再 kill。

---

### 22. 别人的 sglang 容器抢占 8 卡（外部干扰）

**现象**：压测启动报 `Free memory on device cuda:5 (7.93/139.8 GiB) is less than desired GPU memory utilization`，看起来像 vLLM 自己的显存配置问题。

**原因**：`glm52-d-0522`（sglang，GLM-5.2，TP8）占满 8 卡各约 132 GB；且它被别人手动 `docker start` 过两次，甚至在我压测期间。

**解决**
- 这条报错要先 `nvidia-smi` 看占用，不要先怀疑 vLLM。
- 共享机上「容器 Up 但 0 MiB」的服务随时可能真正 start serving 并瞬间吃掉 8×132G；长任务前后都要看 `docker ps`。
- 加 `gpu-watch`（15 s 一轮，按 cgroup 判定归属）做实时监视，一旦真实外部占用就暂停评测并通知。

---

### 23. 397B 的起服务耗时被 NFS 冷/热缓存放大

**现象**：同一个模型，v0.31.0 起服务 1207 s，v0.29.0 只有 352 s。

**原因**：752 GB 权重从 NFS 冷读 vs 页缓存已热，与版本优劣无关。

**解决**：报告里把「起服务耗时」与「性能」分开列，并注明该指标不可跨版本直接比较；必要时先做一次预热读。

---

## 仍未解决 / 需要决策

| # | 事项 | 现状 |
|---|---|---|
| 1 | NVLS 多播不可用 | 已绕过（`NCCL_NVLS_ENABLE=0`）；根治需升级驱动/NVSwitch 固件 |
| 2 | 0.25/0.26 的 CUDA graph 捕获崩溃 | 已绕过（`--enforce-eager`）；属上游版本缺陷，可向 vLLM 报 issue（附本机驱动版本与复现命令） |
| 3 | Qwen3.8-Flash 只有 0.29/0.31 支持 | 无法解决，除非换用更新的 vLLM 镜像 |
| 4 | Qwen3.5-397B 的 graph 3 个版本 + eager 7 个版本未跑 | 时间成本过高（单版 6~20 分钟加载），按要求暂停；补齐约需 1.5 h + 4~6 h |
| 5 | 全平台 uniform eager 模式 | 当前所有数字都是 eager 口径；若要「生产口径」，需要能开 graph 的版本单独再跑一组 |
| 6 | 共享机外部抢占 | 依赖人工协调 + gpu-watch 监视；没有硬性锁卡机制 |
