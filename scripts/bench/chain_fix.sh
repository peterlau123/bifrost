#!/bin/bash
# 2K/1K 跑完后：0.25.1/0.26.0 用上游 workaround（关 allreduce+rmsnorm 融合）补 64K 与 2K/1K 两组，
# 然后千问 397B graph 补 3 版。
set -uo pipefail
while systemctl is-active --quiet bench-m27-2k1k; do sleep 30; done
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key m2.7-nofusion --mode graph v0.26.0 v0.25.1
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key m2.7-2k1k-nofusion --mode graph v0.26.0 v0.25.1
bash /data/liuxin/Ax/bifrost/scripts/bench/vllm_matrix.sh --model-key qwen3.5-397b --mode graph v0.22.1 v0.19.1 v0.17.1
