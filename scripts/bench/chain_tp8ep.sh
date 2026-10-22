#!/bin/bash
# 等前一轮 TP4 流水线结束，再跑 TP8+EP(eager) 那一轮
set -uo pipefail
while systemctl is-active --quiet bench-m27-recipe; do sleep 30; done
exec bash /data/liuxin/Ax/bifrost/scripts/bench/run_pipeline.sh m2.7-recipe-tp8ep:eager
