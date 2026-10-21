#!/bin/bash
# vllm_matrix.sh —— 按从低到高的版本顺序，逐个调用 vllm_bench_one.sh。
# 每个版本一个镜像：load → serve → bench → 存结果 → rm/rmi（宿主空间只够一次一个）。
# 口径全部来自 bench.env，本脚本只负责"顺序"和"版本列表"。
#
# 用法:
#   scripts/bench/vllm_matrix.sh                       # 默认跑 bench.env 里 VERSIONS 的默认序列
#   scripts/bench/vllm_matrix.sh v0.22.1 v0.25.1       # 指定版本（按给定顺序）
#   scripts/bench/vllm_matrix.sh --from v0.22.1        # 从某版本开始跑默认序列
#
# 注意: 某个版本起不来只会记一行 unsupported/startup_failed，不会换并行策略重试 ——
#       换策略会让不同版本的数字不可比。
set -uo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CONFIG="${BENCH_ENV:-$HERE/bench.env}"
[[ -f "$CONFIG" ]] || { echo "找不到配置文件: $CONFIG" >&2; exit 1; }
# shellcheck disable=SC1090
source "$CONFIG"

# 默认从最高版本往下（用户指定顺序）。v0.19.1 / v0.17.1 低于 recipe 的 min_vllm_version=0.20.0，
# 仍按同一口径跑，起不来的话状态会明确记为 below_recipe_min / startup_failed。
DEFAULT_VERSIONS="v0.31.0 v0.29.0 v0.26.0 v0.25.1 v0.22.1 v0.19.1 v0.17.1"
FROM=""
ARGS=()
while [[ $# -gt 0 ]]; do
    case "$1" in
        --from) FROM="$2"; shift 2 ;;
        -h|--help) sed -n '2,16p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) ARGS+=("$1"); shift ;;
    esac
done

VERSIONS=("${ARGS[@]}")
if [[ ${#VERSIONS[@]} -eq 0 ]]; then
    read -r -a VERSIONS <<< "$DEFAULT_VERSIONS"
fi
if [[ -n "$FROM" ]]; then
    KEEP_FROM=0; TMP=()
    for v in "${VERSIONS[@]}"; do
        [[ "$v" == "$FROM" ]] && KEEP_FROM=1
        [[ $KEEP_FROM -eq 1 ]] && TMP+=("$v")
    done
    VERSIONS=("${TMP[@]}")
fi

echo "== 口径: $CONFIG"
echo "== 并行: $PARALLEL_NAME [$PARALLEL_ARGS]"
echo "== 负载: in=$IN_LEN out=$OUT_LEN prompts=$NUM_PROMPTS concurrency=$CONCURRENCY"
echo "== 版本: ${VERSIONS[*]}"
echo "== 结果: $OUTPUT_ROOT/summary.tsv"

for v in "${VERSIONS[@]}"; do
    # 每个版本开跑前再确认一次机器没被别人占（有人占就停下，不硬顶）
    "$HERE/vllm_bench_one.sh" --version "$v" --config "$CONFIG"
done

echo "===== 全部结束，汇总: $OUTPUT_ROOT/summary.tsv ====="
column -t -s$'\t' "$OUTPUT_ROOT/summary.tsv" 2>/dev/null || cat "$OUTPUT_ROOT/summary.tsv"
