#!/bin/bash
# 等 64K graph 那轮跑完，再跑 2K/1K graph
set -uo pipefail
while systemctl is-active --quiet bench-m27-64k-graph; do sleep 30; done
exec bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key m2.7-2k1k --mode graph v0.31.0 v0.29.0 v0.22.1 v0.19.1 v0.17.1
