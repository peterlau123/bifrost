#!/bin/bash
# 2K/1K 跑完后：① 0.25.1/0.26.0 用 -O1 补 64K 组 ② 同样 -O1 补 2K/1K 组 ③ 千问 397B graph 3 版
set -uo pipefail
while systemctl is-active --quiet bench-m27-2k1k; do sleep 30; done
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key m2.7-o1 --mode graph v0.26.0 v0.25.1
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key m2.7-2k1k-o1 --mode graph v0.26.0 v0.25.1
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key qwen3.5-397b --mode graph v0.22.1 v0.19.1 v0.17.1
