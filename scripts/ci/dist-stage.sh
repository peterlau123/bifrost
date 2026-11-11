#!/bin/bash
# dist-stage.sh —— 流水线第 3 阶段：工具链 → 按分支通道打包/发布
#
# 合成一个脚本（同 gates.sh）：让 toolchain.sh 装的 stable + PATH 调整
# 在同一 shell 内对 release.sh 里的 cargo 生效，避免 PATH 不传递导致旧版 cargo。
#
# 用法: bash scripts/ci/dist-stage.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=never

echo "########## 1/2 工具链 ##########"
bash scripts/ci/toolchain.sh

echo "########## 2/2 打包与发布（按分支通道）##########"
bash scripts/ci/release.sh

echo "########## dist 阶段完成 ##########"
