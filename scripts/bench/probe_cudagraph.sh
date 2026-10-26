#!/bin/bash
# probe_cudagraph.sh —— 用不同编译/图捕获参数起 vLLM，判定"能不能过图捕获阶段"。
#
# 用法: probe_cudagraph.sh <image> <label> [额外的 vllm serve 参数...]
# 结果: /data/liuxin/bifrost/bench/probes/<label>.log + 一行判定打印到 stdout
set -uo pipefail
IMG="$1"; LABEL="$2"; shift 2
PORT=19002
NAME=probe-bench
OUT=/data/liuxin/bifrost/bench/probes
mkdir -p "$OUT"
LOG="$OUT/$LABEL.log"

ENV_ARGS=(-e NCCL_NVLS_ENABLE=0)
for kv in ${PROBE_ENV:-}; do ENV_ARGS+=(-e "$kv"); done

docker rm -f "$NAME" >/dev/null 2>&1
docker run -d --name "$NAME" --gpus all --ipc=host --shm-size 32g --ulimit memlock=-1 \
    -p "$PORT:8000" "${ENV_ARGS[@]}" \
    -v /data/liuxin/Ax/models:/models:ro "$IMG" \
    --model /models/MiniMax_MiniMax-M2.7 --served-model-name m2.7 \
    --tensor-parallel-size 4 \
    --trust-remote-code --tool-call-parser minimax_m2 --reasoning-parser minimax_m2 --enable-auto-tool-choice \
    --disable-custom-all-reduce --gpu-memory-utilization 0.95 \
    --max-num-seqs 256 --max-num-batched-tokens 8192 \
    "$@" > "$OUT/$LABEL.run.log" 2>&1

RESULT="TIMEOUT"
for i in $(seq 1 30); do
    sleep 10
    if curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1; then RESULT="READY@$((i*10))s"; break; fi
    if ! docker ps --format '{{.Names}}' | grep -qx "$NAME"; then RESULT="CONTAINER_EXIT@$((i*10))s"; break; fi
    if docker logs "$NAME" 2>&1 | grep -qE "CUDA error: an illegal memory access|CUDA_ERROR_ILLEGAL_ADDRESS"; then
        RESULT="CRASH_ILLEGAL_ADDR@$((i*10))s"; break
    fi
done

docker logs "$NAME" > "$LOG" 2>&1 || true
FIRST_ERR=$(grep -m1 -E "RuntimeError: CUDA driver error|CUDA error: an illegal memory access|ValueError:|RuntimeError:" "$LOG" 2>/dev/null | cut -c1-150)
FIRST_WARN=$(grep -m1 -E "Profiling CUDA graph memory|Capturing CUDA graphs" "$LOG" 2>/dev/null | cut -c1-110)
echo "[$LABEL] $RESULT"
echo "   阶段: ${FIRST_WARN:-（未到图阶段）}"
echo "   首个错误: ${FIRST_ERR:-（无）}"
docker rm -f "$NAME" >/dev/null 2>&1
