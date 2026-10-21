#!/bin/bash
# vllm_bench_one.sh —— 跑**一个** vLLM 镜像的评测。换版本只改 --image / --version，其余全部来自
# bench.env（模型、并行策略、显存/调度参数、功能参数、压测负载）。
#
# 用法:
#   # 镜像已在本地（只评测，不 load）
#   scripts/bench/vllm_bench_one.sh --image vllm/vllm-openai:v0.31.0
#
#   # 从 IMAGES_DIR 里的 tar load 进来再评测，跑完删镜像（宿主空间只够一次一个）
#   scripts/bench/vllm_bench_one.sh --version v0.31.0
#   scripts/bench/vllm_bench_one.sh --version v0.31.0 --keep-image
#
#   # 只看这次会用哪些参数、哪条 docker 命令（不碰 GPU / 不碰 docker）
#   scripts/bench/vllm_bench_one.sh --version v0.31.0 --dry-run
#   scripts/bench/vllm_bench_one.sh --print-config
#
# 结果: $OUTPUT_ROOT/<version>/{load.log,run.log,serve.log,bench.log,result.json}
#       并在 $OUTPUT_ROOT/summary.tsv 追加一行（同一张表累积所有版本，方便对比）
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${BENCH_ENV:-$HERE/bench.env}"
[[ -f "$CONFIG" ]] || { echo "找不到配置文件: $CONFIG" >&2; exit 1; }
# shellcheck disable=SC1090
source "$CONFIG"

IMAGE=""; VERSION=""; KEEP=0; DRY=0; PRINT_CFG=0
while [[ $# -gt 0 ]]; do
    case "$1" in
        --image) IMAGE="$2"; shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        --keep-image) KEEP=1; shift ;;
        --dry-run) DRY=1; shift ;;
        --print-config) PRINT_CFG=1; shift ;;
        --config) CONFIG="$2"; source "$CONFIG"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

# --version → --image（tar 名规则：vllm_vllm-openai-<ver>.tar）
[[ -n "$VERSION" && -z "$IMAGE" ]] && IMAGE="vllm/vllm-openai:$VERSION"
[[ -n "$VERSION" || -n "$IMAGE" ]] || { echo "需要 --version 或 --image" >&2; exit 1; }
[[ -n "$VERSION" ]] || VERSION="${IMAGE##*:}"
TAG="$IMAGE"
# 镜像名 → tar 名：vllm/vllm-openai:v0.31.0 → vllm_vllm-openai-v0.31.0.tar
REPO="${IMAGE%%:*}"; REPO="${REPO##*/}"      # vllm/vllm-openai → vllm-openai
TAR="$IMAGES_DIR/vllm_${REPO}-${IMAGE##*:}.tar"   # → vllm_vllm-openai-v0.31.0.tar

DIR="$OUTPUT_ROOT/$VERSION"
PROGRESS="$OUTPUT_ROOT/progress.log"
SUMMARY="$OUTPUT_ROOT/summary.tsv"

log() { echo "[$(date -u +%FT%TZ)] $*" | tee -a "$PROGRESS"; }
free_gb() { df -BG --output=avail / | tail -1 | tr -dc '0-9'; }
cleanup_container() { docker rm -f "$CONTAINER" >/dev/null 2>&1 || true; }

SERVE_CMD=(docker run -d --name "$CONTAINER" --gpus all --ipc=host --shm-size "$SHM_SIZE"
    -p "$PORT:8000" -v "$(dirname "$MODEL_HOST"):$MODEL_MOUNT:ro"
    "$TAG"
    --model "$MODEL_MOUNT/$(basename "$MODEL_HOST")" --served-model-name "$SERVED_NAME"
    $PARALLEL_ARGS $RECIPE_ARGS
    --gpu-memory-utilization "$GPU_MEM_UTIL"
    --max-num-seqs "$MAX_NUM_SEQS" --max-num-batched-tokens "$MAX_NUM_BATCHED_TOKENS")

BENCH_CMD=(docker exec "$CONTAINER" vllm bench serve
    --backend "$BACKEND" --model "$SERVED_NAME" --base-url "http://127.0.0.1:8000"
    --endpoint "$ENDPOINT" --dataset-name "$DATASET"
    --random-input-len "$IN_LEN" --random-output-len "$OUT_LEN"
    --num-prompts "$NUM_PROMPTS" --max-concurrency "$CONCURRENCY"
    --save-result --result-dir /tmp)

if [[ $PRINT_CFG -eq 1 ]]; then
    cat <<EOF
配置文件   : $CONFIG
镜像       : $TAG
tar        : $TAR
模型       : $MODEL_HOST → $MODEL_MOUNT/$(basename "$MODEL_HOST")（served as $SERVED_NAME）
并行策略   : $PARALLEL_NAME   [$PARALLEL_ARGS]
高性能参数 : gpu-mem-util=$GPU_MEM_UTIL max-num-seqs=$MAX_NUM_SEQS max-num-batched-tokens=$MAX_NUM_BATCHED_TOKENS max-model-len=auto
功能参数   : $RECIPE_ARGS
压测负载   : $BACKEND $ENDPOINT $DATASET in=$IN_LEN out=$OUT_LEN prompts=$NUM_PROMPTS concurrency=$CONCURRENCY
输出       : $DIR
EOF
    exit 0
fi

if [[ $DRY -eq 1 ]]; then
    echo "== 配置文件: $CONFIG"; echo "== 镜像: $TAG"; echo "== tar: $TAR"; echo "== 输出: $DIR"
    printf '== serve:\n%s\n' "$(printf '%q ' "${SERVE_CMD[@]}")"
    printf '== bench:\n%s\n' "$(printf '%q ' "${BENCH_CMD[@]}")"
    exit 0
fi

mkdir -p "$DIR"
if [[ -f "$SUMMARY" ]] && ! head -1 "$SUMMARY" | grep -q config; then mv "$SUMMARY" "$SUMMARY.old"; fi
[[ -f "$SUMMARY" ]] || printf "version\tconfig\tstatus\tload_s\tstartup_s\tbench_s\treq_per_s\tttft_ms\tresult_json\n" > "$SUMMARY"

record() { printf "%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\t%s\n" "$@" >> "$SUMMARY"; }
wait_healthy() {   # 0=ready 2=container died 1=timeout
    local deadline=$(( $(date +%s) + SERVE_TIMEOUT ))
    while [[ $(date +%s) -lt $deadline ]]; do
        curl -sf "http://127.0.0.1:$PORT/health" >/dev/null 2>&1 && return 0
        docker ps --format '{{.Names}}' | grep -qx "$CONTAINER" || return 2
        sleep 10
    done
    return 1
}

log "===== $TAG start (free $(free_gb)G, config=$PARALLEL_NAME) ====="

# 8 卡被占时直说，不要让它伪装成 vLLM 的报错
busy=$(nvidia-smi --query-compute-apps=pid,process_name --format=csv,noheader 2>/dev/null)
if [[ -n "$busy" ]]; then
    log "$TAG ABORT: GPU 被占用，先腾卡:"
    echo "$busy" | sort -u | sed 's/^/    /' | tee -a "$PROGRESS"
    record "$VERSION" "$PARALLEL_NAME" gpu_busy "" "" "" "" "" ""; exit 1
fi

# ── 需要时先 load（tar 不在就直接用本地镜像）───────────────────────────
load_s=""
if [[ -f "$TAR" ]]; then
    need=$(( $(stat -c %s "$TAR") * 22 / 10000000000 + 1 ))
    if [[ $(free_gb) -lt $need ]]; then
        log "$TAG ABORT: 空间不足（需 ~${need}G，现有 $(free_gb)G）"
        record "$VERSION" "$PARALLEL_NAME" no_space "" "" "" "" "" ""; exit 1
    fi
    t0=$(date +%s)
    docker load -i "$TAR" > "$DIR/load.log" 2>&1 || {
        log "$TAG FAIL: docker load 失败（见 $DIR/load.log）"
        record "$VERSION" "$PARALLEL_NAME" load_failed "" "" "" "" "" ""; exit 1; }
    if grep -qi "error unpacking" "$DIR/load.log"; then
        log "$TAG FAIL: 层解包错误（tar 损坏；docker load 仍返回 0）"
        docker rmi "$TAG" >/dev/null 2>&1
        record "$VERSION" "$PARALLEL_NAME" corrupt_tar "" "" "" "" "" ""; exit 1
    fi
    load_s=$(( $(date +%s) - t0 ))
    log "$TAG loaded in ${load_s}s (free $(free_gb)G)"
else
    log "$TAG 无 tar（$TAR），假设镜像已在本地"
fi

# ── 起服务（固定并行配置，不做降级：跨版本必须同口径）──────────────────
cleanup_container
t1=$(date +%s)
"${SERVE_CMD[@]}" > "$DIR/run.log" 2>&1
wait_healthy; hc=$?
startup_s=""
if [[ $hc -eq 0 ]]; then
    startup_s=$(( $(date +%s) - t1 ))
    log "$TAG ready in ${startup_s}s"
else
    docker logs "$CONTAINER" > "$DIR/serve.log" 2>&1
    log "$TAG FAIL: 服务未就绪(hc=$hc): $(grep -m1 -E 'ValueError|RuntimeError|CUDA out of memory|not divisible|assert' "$DIR/serve.log" | cut -c1-160)"
    cleanup_container
    [[ "$KEEP" != "1" ]] && docker rmi "$TAG" >/dev/null 2>&1
    record "$VERSION" "$PARALLEL_NAME" startup_failed "$load_s" "" "" "" "" ""; exit 1
fi

# ── 压测（固定负载）────────────────────────────────────────────────────
t2=$(date +%s)
"${BENCH_CMD[@]}" > "$DIR/bench.log" 2>&1
bench_rc=$?
bench_s=$(( $(date +%s) - t2 ))
docker logs "$CONTAINER" > "$DIR/serve.log" 2>&1
docker cp "$CONTAINER:/tmp/." "$DIR/bench_raw" >/dev/null 2>&1 || true
RES=$(ls -t "$DIR"/bench_raw/*.json 2>/dev/null | head -1)
[[ -n "$RES" ]] && cp "$RES" "$DIR/result.json"

if [[ $bench_rc -ne 0 || -z "$RES" ]]; then
    log "$TAG FAIL: bench 失败 rc=$bench_rc"
    record "$VERSION" "$PARALLEL_NAME" bench_failed "$load_s" "$startup_s" "$bench_s" "" "" ""
else
    read -r tput ttft < <(python3 - "$RES" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
req = d.get("request_throughput") or d.get("requests_per_second") or 0
ttft = d.get("mean_ttft_ms") or d.get("median_ttft_ms") or 0
print(f"{req:.2f} {ttft:.1f}")
PY
)
    log "$TAG OK: ${tput} req/s, TTFT ${ttft}ms"
    record "$VERSION" "$PARALLEL_NAME" ok "$load_s" "$startup_s" "$bench_s" "$tput" "$ttft" "$DIR/result.json"
fi

cleanup_container
if [[ "$KEEP" != "1" ]]; then
    docker rmi "$TAG" >/dev/null 2>&1 && log "$TAG image removed (free $(free_gb)G)"
fi
log "===== $TAG done ====="
