#!/bin/bash
# gates.sh —— 质量门禁：环境诊断 → 工具链 → fmt → clippy → test
#
# 为什么合成一个脚本：
#   1. Gitee Go 的每条 command 是**独立 shell**，toolchain.sh 里 source ~/.cargo/env
#      设的 PATH 不会带到下一条命令 → 下一条 cargo 还是插件自带的 1.69（2026-10-27 build#3
#      就是这么红的：lock file v4 需要 cargo>=1.78）
#   2. 一个 shell 里 set -euo pipefail，任何一环失败立刻停，不会"上一条失败还继续跑"
#
# 用法: bash scripts/ci/gates.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT"

export PATH="$HOME/.cargo/bin:$PATH"
export CARGO_TERM_COLOR=never

echo "########## 0/4 环境诊断 ##########"
bash scripts/ci/diagnose.sh

echo "########## 1/4 工具链 ##########"
bash scripts/ci/toolchain.sh

echo "########## 2/4 cargo fmt --check ##########"
cargo fmt --check
echo "✓ fmt"

echo "########## 3/4 cargo clippy -D warnings ##########"
cargo clippy --all-targets -- -D warnings
echo "✓ clippy"

echo "########## 4/4 cargo test --all ##########"
cargo test --all
echo "✓ tests"

echo "########## 门禁全部通过 ##########"
