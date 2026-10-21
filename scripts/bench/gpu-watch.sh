#!/bin/bash
# gpu-watch.sh —— 盯着 GPU 有没有被"别人"占走。压测机是共享的，别人的 sglang 一 start serving
# 就会瞬间吃掉 8×132G，把我们的压测打断（症状还长得像 vLLM 自己的 free-memory 报错）。
#
# 判定：nvidia-smi 里每个 compute 进程，看它的 cgroup 属于哪个容器；
#       属于 $BENCH_CONTAINER（我们自己的压测容器）就放过，否则记一条 ALERT。
#
# 用法:
#   scripts/bench/gpu-watch.sh                      # 默认 20s 一轮，日志在 bench.env 的 OUTPUT_ROOT
#   BENCH_CONTAINER=bifrost-bench scripts/bench/gpu-watch.sh --interval 10
#   touch <OUTPUT_ROOT>/gpu-watch.stop              # 让它退出
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${BENCH_ENV:-$HERE/bench.env}"
# shellcheck disable=SC1090
source "$CONFIG"
BENCH_CONTAINER="${BENCH_CONTAINER:-bifrost-bench}"
INTERVAL=20
while [[ $# -gt 0 ]]; do
    case "$1" in
        --interval) INTERVAL="$2"; shift 2 ;;
        -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) shift ;;
    esac
done
LOG="$OUTPUT_ROOT/gpu-watch.log"
STOP="$OUTPUT_ROOT/gpu-watch.stop"
ALERT="$OUTPUT_ROOT/gpu-watch.alert"
mkdir -p "$OUTPUT_ROOT"
rm -f "$STOP" "$ALERT"
echo "[$(date -u +%FT%TZ)] watch start interval=${INTERVAL}s bench_container=$BENCH_CONTAINER" >> "$LOG"

while [[ ! -f "$STOP" ]]; do
    cid=$(docker inspect -f '{{.Id}}' "$BENCH_CONTAINER" 2>/dev/null | cut -c1-12)
    foreign=""
    # 只认真实占卡：用显存 >= 512MiB 且进程名非空的条目。
    # nvidia-smi 自己、dcgm/persistenced 之类的探针会瞬态出现在列表里但 used_memory=0、
    # 名字为空，之前把这些当"外部占用"造成过两次假警报。
    while IFS=', ' read -r pid pname mem; do
        [[ -z "${pid:-}" ]] && continue
        mem=${mem%% *}; mem=${mem%.*}
        [[ "${mem:-0}" =~ ^[0-9]+$ ]] || mem=0
        [[ "$mem" -lt 512 ]] && continue
        cg=$(grep -o 'docker-[0-9a-f]\{12\}' "/proc/$pid/cgroup" 2>/dev/null | head -1 | sed 's/docker-//')
        if [[ -z "$cid" || "$cg" != "$cid" ]]; then
            foreign+="pid=$pid container=${cg:-无} name=${pname:-?} mem=${mem}MiB; "
        fi
    done < <(nvidia-smi --query-compute-apps=pid,process_name,used_memory --format=csv,noheader,nounits 2>/dev/null)
    if [[ -n "$foreign" ]]; then
        {
            echo "[$(date -u +%FT%TZ)] ALERT 有外部进程占用 GPU: $foreign"
            echo "  当前 GPU: $(nvidia-smi --query-gpu=index,memory.used,utilization.gpu --format=csv,noheader | tr '\n' ' ')"
            echo "  运行中容器: $(docker ps --format '{{.Names}}' | tr '\n' ' ')"
        } >> "$LOG"
        echo "$(date -u +%FT%TZ) $foreign" > "$ALERT"
    fi
    sleep "$INTERVAL"
done
echo "[$(date -u +%FT%TZ)] watch stop" >> "$LOG"
