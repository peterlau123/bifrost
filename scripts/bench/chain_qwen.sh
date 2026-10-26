#!/bin/bash
# 等 M2.7 2K/1K 跑完，再跑千问 graph（397B 补齐 3 个版本，TP8）
set -uo pipefail
while systemctl is-active --quiet bench-m27-2k1k; do sleep 30; done
exec bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh \
    --model-key qwen3.5-397b --mode graph v0.22.1 v0.19.1 v0.17.1
