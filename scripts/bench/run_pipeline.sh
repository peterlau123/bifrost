#!/bin/bash
# run_pipeline.sh —— 按 (模型, 模式) 串行跑评测，一个接一个，绝不同时。
#
# 用法:
#   scripts/bench/run_pipeline.sh qwen3.8-flash:graph qwen3.8-flash:eager \
#                                qwen3.6-35b:graph qwen3.6-35b:eager \
#                                qwen3.5-397b:graph qwen3.5-397b:eager
#   不带 spec 则跑默认序列（见下）
#
# 每个 (模型,模式) 结束后自动生成该目录的 REPORT.md（MD 格式），并在总日志里打一行汇总。
# 建议用 systemd-run 起，脱离任务调度器 cgroup：
#   systemd-run --unit=bench-pipeline --collect bash scripts/bench/run_pipeline.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${BENCH_ENV:-$HERE/bench.env}"
# shellcheck disable=SC1090
source "$CONFIG"

VERSIONS="v0.31.0 v0.29.0 v0.26.0 v0.25.1 v0.22.1 v0.19.1 v0.17.1"
SPECS=("$@")
[[ ${#SPECS[@]} -gt 0 ]] || SPECS=(
    qwen3.8-flash:graph qwen3.8-flash:eager
    qwen3.6-35b:graph   qwen3.6-35b:eager
    qwen3.5-397b:graph  qwen3.5-397b:eager
)

ROOT="$(cd "$HERE/../.." && pwd)"
BENCH_ROOT="$(dirname "$(grep -m1 '^OUTPUT_ROOT=' "$HERE/models/${SPECS[0]%%:*}.env" | cut -d= -f2-)")"
LOG="$BENCH_ROOT/pipeline.log"
mkdir -p "$BENCH_ROOT"
say() { echo "[$(date -u +%FT%TZ)] $*" | tee -a "$LOG"; }

say "===== pipeline 开始，共 ${#SPECS[@]} 个任务（串行）: ${SPECS[*]} ====="
for spec in "${SPECS[@]}"; do
    model="${spec%%:*}"; mode="${spec##*:}"
    say "----- $model / $mode 开始 -----"
    "$HERE/vllm_matrix.sh" --model-key "$model" --mode "$mode" $VERSIONS 2>&1 | tee -a "$LOG"
    dir="$(grep -m1 '^OUTPUT_ROOT=' "$HERE/models/$model.env" | cut -d= -f2-)-$mode"
    say "----- $model / $mode 结束，结果目录: $dir -----"
    python3 "$HERE/gen_report.py" "$dir" "$model · $mode 模式" 2>&1 | tee -a "$LOG"
    if [[ -f "$dir/summary.tsv" ]]; then
        column -t -s$'\t' "$dir/summary.tsv" 2>/dev/null | tee -a "$LOG" || cat "$dir/summary.tsv" | tee -a "$LOG"
    fi
done
say "===== pipeline 全部结束 ====="
