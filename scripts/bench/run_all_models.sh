#!/bin/bash
# run_all_models.sh —— 按顺序把多个模型的版本矩阵串起来跑（每个模型内部从高版本到低版本）。
#
# 用法:
#   scripts/bench/run_all_models.sh qwen3.6-35b qwen3.8-flash qwen3.5-397b
#   scripts/bench/run_all_models.sh                     # 默认按"小到大"顺序跑三个 Qwen
#
# 建议用 systemd-run 起，脱离任务调度器 cgroup（daemon 重启会杀掉自己 cgroup 里的任务）：
#   systemd-run --unit=bench-all --collect bash scripts/bench/run_all_models.sh
set -uo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${BENCH_ENV:-$HERE/bench.env}"
# shellcheck disable=SC1090
source "$CONFIG"

MODELS=("$@")
[[ ${#MODELS[@]} -gt 0 ]] || MODELS=(qwen3.6-35b qwen3.8-flash qwen3.5-397b)
VERSIONS="v0.31.0 v0.29.0 v0.26.0 v0.25.1 v0.22.1 v0.19.1 v0.17.1"

BENCH_ROOT="$(dirname "$(grep -m1 '^OUTPUT_ROOT=' "$HERE/models/${MODELS[0]}.env" | cut -d= -f2-)")"
LOG="$BENCH_ROOT/all-models.log"
mkdir -p "$BENCH_ROOT"
say() { echo "[$(date -u +%FT%TZ)] $*" | tee -a "$LOG"; }

say "===== 开始，模型顺序: ${MODELS[*]} ====="
for mk in "${MODELS[@]}"; do
    say "----- 模型 $mk 开始 -----"
    "$HERE/vllm_matrix.sh" --model-key "$mk" $VERSIONS 2>&1 | tee -a "$LOG"
    say "----- 模型 $mk 结束 -----"
    root=$(grep -m1 '^OUTPUT_ROOT=' "$HERE/models/$mk.env" | cut -d= -f2-)
    if [[ -f "$root/summary.tsv" ]]; then
        say "== $mk 汇总（$root/summary.tsv）:"
        column -t -s$'\t' "$root/summary.tsv" 2>/dev/null | tee -a "$LOG" || cat "$root/summary.tsv" | tee -a "$LOG"
    fi
done
say "===== 全部模型结束 ====="
