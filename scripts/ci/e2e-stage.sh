#!/bin/bash
# e2e-stage.sh —— 流水线第 2 阶段：工具链 → release 构建 → 6 个 e2e 场景
#
# 合成一个脚本（同 gates.sh）：让 toolchain.sh 装的 stable + PATH 调整
# 在同一 shell 内对后续步骤生效，避免分隔 steps 各自起新 shell 导致状态不传递。
#
# 用法: bash scripts/ci/e2e-stage.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=never

echo "########## 1/3 工具链 ##########"
bash scripts/ci/toolchain.sh

echo "########## 2/3 cargo build --release ##########"
cargo build --release
echo "✓ release 构建完成"

echo "########## 3/3 端到端（6 个场景）##########"
bash scripts/ci/e2e.sh ./target/release/bifrost

echo "########## e2e 阶段完成 ##########"
