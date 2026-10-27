#!/bin/bash
# dist-stage.sh —— 流水线第 3 阶段：工具链 → 按分支通道打包/发布
#
# 同样合成一个脚本的原因：Gitee Go 每条 command 是独立 shell，
# toolchain.sh 装的 rust 1.6x→stable 的 PATH 变化不会带到下一条命令，
# 结果 release.sh 里的 cargo 还是插件自带的 1.69（读不了 Cargo.lock v4）。
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
