#!/bin/bash
# ① M2.7 2K/1K 补 0.19.1（上次 cudaErrorNotPermitted，重试）+ 0.25.1 复核
# ② 千问 3.6 / 397B 的 0.26.0/0.25.1 用 workaround 开 graph 补跑（TP4 / TP8）
set -uo pipefail
B=/data/liuxin/Ax/bifrost/scripts/bench
bash $B/vllm_matrix.sh --model-key m2.7-2k1k --mode graph v0.19.1 v0.25.1
bash $B/vllm_matrix.sh --model-key qwen3.6-35b-nofusion --mode graph v0.26.0 v0.25.1
bash $B/vllm_matrix.sh --model-key qwen3.5-397b-nofusion --mode graph v0.26.0 v0.25.1
