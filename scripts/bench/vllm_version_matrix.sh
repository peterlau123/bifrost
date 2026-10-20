#!/bin/bash
# vllm_version_matrix.sh —— 逐个版本拉起 vLLM 服务，用同一套 8 卡高性能配置与同一套 workload 压测，
# 结果落盘后删镜像（宿主 / 空间只够一次一个）。
#
# 为什么需要"并行配置降级链"：MiniMax-M2.7 是 fp8 分块量化（block_size 128×128）的 MoE，
# moe_intermediate_size=1536。TP=8 时每卡切到 192，不是 128 的整数倍，旧版 vLLM 直接报
#   ValueError: output_size of gate's and up's weight = 192 is not divisible by block_n = 128
# 新版可能做了 padding 自动补齐。所以按下面顺序尝试，取第一个能起来的配置，
# 并把它记进 summary（跨版本比较时必须知道各自用的是哪一档）。
#
# 用法:
#   scripts/bench/vllm_version_matrix.sh --model /data/liuxin/Ax/models/<模型目录> \
#     --images-dir /data/liuxin/images --output /data/liuxin/bifrost/bench/m2.7 \
#     --versions v0.19.1,v0.22.1,v0.25.1,v0.26.0,v0.29.0,v0.31.0
set -uo pipefail

MODEL=""; SERVED="m2.7"; IMAGES_DIR=""; OUTPUT=""; VERSIONS=""
PORT=19000; CONTAINER="bifrost-bench"
GPU_MEM_UTIL="0.90"; MAX_LEN=32768; MAX_SEQS=256; MAX_BATCHED_TOKENS=16384
IN_LEN=4096; OUT_LEN=512; NUM_PROMPTS=200; CONCURRENCY=64
SERVE_TIMEOUT=2400
KEEP=0
WAIT_GPU_MIN=0     # GPU 被别的任务占用时，最多等多少分钟（0 = 直接中止）

# 并行配置降级链：都占满 8 卡，只是分法不同
CONFIG_NAMES=("TP8" "TP8+EP" "TP4+DP2" "TP4+DP2+EP" "TP2+DP4")
CONFIG_ARGS=(
    "--tensor-parallel-size 8"
    "--tensor-parallel-size 8 --enable-expert-parallel"
    "--tensor-parallel-size 4 --data-parallel-size 2"
    "--tensor-parallel-size 4 --data-parallel-size 2 --enable-expert-parallel"
    "--tensor-parallel-size 2 --data-parallel-size 4"
)

while [[ $# -gt 0 ]]; do
    case "$1" in
        --model) MODEL="$2"; shift 2 ;;
        --served-name) SERVED="$2"; shift 2 ;;
        --images-dir) IMAGES_DIR="$2"; shift 2 ;;
        --output) OUTPUT="$2"; shift 2 ;;
        --versions) VERSIONS="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        --in-len) IN_LEN="$2"; shift 2 ;;
        --out-len) OUT_LEN="$2"; shift 2 ;;
        --num-prompts) NUM_PROMPTS="$2"; shift 2 ;;
        --concurrency) CONCURRENCY="$2"; shift 2 ;;
        --serve-timeout) SERVE_TIMEOUT="$2"; shift 2 ;;
        --keep-image) KEEP=1; shift ;;
        --wait-gpu) WAIT_GPU_MIN="$2"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done
[[ -n "$MODEL" && -n "$IMAGES_DIR" && -n "$OUTPUT" && -n "$VERSIONS" ]] || {
    echo "必选: --model --images-dir --output --versions" >&2; exit 1; }

mkdir -p "$OUTPUT"
PROGRESS="$OUTPUT/progress.log"
SUMMARY="$OUTPUT/summary.tsv"
if [[ -f "$SUMMARY" ]] && ! head -1 "$SUMMARY" | grep -q config; then mv "$SUMMARY" "$SUMMARY.old"; fi
[[ -f "$SUMMARY" ]] || printf "version\tconfig\tstatus\tload_s\tstartup_s\tbench_s\treq_per_s\tttft_ms\tresult_json\n" > "$SUMMARY"

log() { echo "[$(date -u +%H:%M:%SZ)] $*" | tee -a "$PROGRESS"; }
free_gb() { df -BG --output=avail / | tail -1 | tr -dc '0-9'; }
cleanup_container() { docker rm -f "$CONTAINER" >/dev/null 2>&1 || true; }

wait_healthy() {   # 0=ready 2=container died 1=timeout
    local deadline=$(( $(date +%s) + SERVE_TIMEOUT ))
    while [[ $(date +%s) -lt $deadline ]]; do
        curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && return 0
        docker ps --format '{{.Names}}' | grep -qx "$CONTAINER" || return 2
        sleep 10
    done
    return 1
}

record() { printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$@" >> "$SUMMARY"; }


# 8 卡必须整机空闲，否则 gpu_memory_utilization=0.90 必然报 free memory 不足，
# 而那个报错长得像 vLLM 自己的问题，极易误判。
gpu_busy() { nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader 2>/dev/null; }

wait_gpu_free() {
    local deadline=$(( $(date +%s) + WAIT_GPU_MIN * 60 ))
    while :; do
        local busy; busy=$(gpu_busy)
        if [[ -z "$busy" ]]; then return 0; fi
        if [[ $WAIT_GPU_MIN -eq 0 ]]; then
            log "GPU 被占用，中止（用 --wait-gpu N 可等待）:"
            echo "$busy" | sort -u | sed 's/^/    /' | tee -a "$PROGRESS"
            return 1
        fi
        if [[ $(date +%s) -ge $deadline ]]; then
            log "等待 ${WAIT_GPU_MIN}min 后 GPU 仍被占用，中止"; return 1
        fi
        sleep 20
    done
}

for ver in ${VERSIONS//,/ }; do
    TAG="vllm/vllm-openai:$ver"
    TAR="$IMAGES_DIR/vllm_vllm-openai-$ver.tar"
    DIR="$OUTPUT/$ver"; mkdir -p "$DIR"
    log "===== $ver start (free $(free_gb)G) ====="

    if ! wait_gpu_free; then
        record "$ver" "-" gpu_busy "" "" "" "" "" ""
        break
    fi
    if [[ ! -f "$TAR" ]]; then
        log "$ver SKIP: 无 $TAR"; record "$ver" "-" missing_tar "" "" "" "" "" ""; continue
    fi
    need=$(( $(stat -c %s "$TAR") * 22 / 10000000000 + 1 ))
    if [[ $(free_gb) -lt $need ]]; then
        log "$ver SKIP: 空间不足（需 ~${need}G，现有 $(free_gb)G）"
        record "$ver" "-" no_space "" "" "" "" "" ""; continue
    fi

    cleanup_container
    t0=$(date +%s)
    docker load -i "$TAR" > "$DIR/load.log" 2>&1 || { log "$ver FAIL: load 失败"; record "$ver" "-" load_failed "" "" "" "" "" ""; docker rmi "$TAG" >/dev/null 2>&1; continue; }
    if grep -qi "error unpacking" "$DIR/load.log"; then
        log "$ver FAIL: 层解包错误（包损坏；docker load 仍返回 0）"
        record "$ver" "-" corrupt_tar "" "" "" "" "" ""; docker rmi "$TAG" >/dev/null 2>&1; continue
    fi
    load_s=$(( $(date +%s) - t0 ))
    log "$ver loaded in ${load_s}s (free $(free_gb)G)"

    # ── 按降级链找第一个能起来的并行配置 ───────────────────────────
    WORKED_CFG=""; WORKED_ARGS=""; startup_s=""
    for i in "${!CONFIG_NAMES[@]}"; do
        cfg="${CONFIG_NAMES[$i]}"; args="${CONFIG_ARGS[$i]}"
        cleanup_container
        t1=$(date +%s)
        # shellcheck disable=SC2086
        docker run -d --name "$CONTAINER" --gpus all --ipc=host --shm-size 32g \
            -p "$PORT:8000" -v "$(dirname "$MODEL"):/models:ro" \
            "$TAG" \
            --model "/models/$(basename "$MODEL")" --served-model-name "$SERVED" \
            $args \
            --gpu-memory-utilization "$GPU_MEM_UTIL" --max-model-len "$MAX_LEN" \
            --max-num-seqs "$MAX_SEQS" --max-num-batched-tokens "$MAX_BATCHED_TOKENS" \
            --enable-chunked-prefill --trust-remote-code \
            > "$DIR/run-$cfg.log" 2>&1
        wait_healthy; hc=$?
        if [[ $hc -eq 0 ]]; then
            WORKED_CFG="$cfg"; WORKED_ARGS="$args"; startup_s=$(( $(date +%s) - t1 ))
            log "$ver config=$cfg ready in ${startup_s}s"
            break
        fi
        docker logs "$CONTAINER" > "$DIR/fail-$cfg.log" 2>&1
        log "$ver config=$cfg 失败(hc=$hc): $(grep -m1 -E 'ValueError|RuntimeError|CUDA out of memory|assert' "$DIR/fail-$cfg.log" | cut -c1-140)"
        cleanup_container
    done

    if [[ -z "$WORKED_CFG" ]]; then
        log "$ver FAIL: 所有并行配置都起不来"
        record "$ver" "-" all_configs_failed "$load_s" "" "" "" "" ""
        docker rmi "$TAG" >/dev/null 2>&1; continue
    fi

    # ── 压测 ───────────────────────────────────────────────────────
    t2=$(date +%s)
    docker exec "$CONTAINER" vllm bench serve \
        --model "$SERVED" --base-url "http://127.0.0.1:8000" \
        --dataset-name random --random-input-len "$IN_LEN" --random-output-len "$OUT_LEN" \
        --num-prompts "$NUM_PROMPTS" --max-concurrency "$CONCURRENCY" \
        --save-result --result-dir /tmp \
        > "$DIR/bench.log" 2>&1
    bench_rc=$?
    bench_s=$(( $(date +%s) - t2 ))
    docker logs "$CONTAINER" > "$DIR/serve.log" 2>&1
    docker cp "$CONTAINER:/tmp/." "$DIR/bench_raw" >/dev/null 2>&1 || true
    RES=$(ls -t "$DIR"/bench_raw/*.json 2>/dev/null | head -1)
    [[ -n "$RES" ]] && cp "$RES" "$DIR/result.json"

    if [[ $bench_rc -ne 0 || -z "$RES" ]]; then
        log "$ver FAIL: bench 失败 rc=$bench_rc"
        record "$ver" "$WORKED_CFG" bench_failed "$load_s" "$startup_s" "$bench_s" "" "" ""
    else
        read -r tput ttft < <(python3 - "$RES" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
req = d.get("request_throughput") or d.get("requests_per_second") or 0
ttft = d.get("mean_ttft_ms") or d.get("median_ttft_ms") or 0
print(f"{req:.2f} {ttft:.1f}")
PY
)
        log "$ver OK [cfg=$WORKED_CFG]: ${tput} req/s, TTFT ${ttft}ms"
        record "$ver" "$WORKED_CFG" ok "$load_s" "$startup_s" "$bench_s" "$tput" "$ttft" "$DIR/result.json"
    fi

    cleanup_container
    if [[ "$KEEP" != "1" ]]; then
        docker rmi "$TAG" >/dev/null 2>&1 && log "$ver image removed (free $(free_gb)G)"
    fi
done

log "===== 结束，汇总: $SUMMARY ====="
column -t -s$'\t' "$SUMMARY" 2>/dev/null || cat "$SUMMARY"
