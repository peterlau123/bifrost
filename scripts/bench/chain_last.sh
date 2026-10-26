#!/bin/bash
# 等 bench-next 结束，用 workaround 口径重测 0.25.1 的 2K/1K（上次 0.47/5363 异常，再按同口径确认）
set -uo pipefail
while systemctl is-active --quiet bench-next; do sleep 30; done
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key m2.7-2k1k-nofusion --mode graph v0.25.1
