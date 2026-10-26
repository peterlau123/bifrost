#!/bin/bash
# TP8 + EP + CUDA Graph 全版本矩阵（含 workaround），严格串行
set -uo pipefail
B=/data/liuxin/Ax/bifrost/scripts/bench
while systemctl is-active --quiet bench-last; do sleep 30; done
V="v0.31.0 v0.29.0 v0.26.0 v0.25.1 v0.22.1 v0.19.1 v0.17.1"
bash $B/vllm_matrix.sh --model-key m2.7-tp8ep-graph        --mode graph $V
bash $B/vllm_matrix.sh --model-key qwen3.8-flash-tp8ep-graph --mode graph $V
bash $B/vllm_matrix.sh --model-key qwen3.6-35b-tp8ep-graph   --mode graph $V
bash $B/vllm_matrix.sh --model-key qwen3.5-397b-tp8ep-graph  --mode graph $V
