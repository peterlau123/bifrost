#!/bin/bash
# gates.sh —— 质量门禁：环境诊断 → 工具链 → fmt → clippy → test
#
# 为什么合成一个脚本：
#   把 4 步放在一个 shell 里，toolchain.sh 里 source ~/.cargo/env 设的 PATH、
#   rustup 装的 stable、CARGO_TERM_COLOR 等环境变量都能带到后续步骤，
#   set -euo pipefail 也让任何一环失败立刻停，不会"上一步失败还继续跑"。
#   （这一约束源自历史教训：分离的 steps 曾各自起新 shell，PATH 不传递，
#   导致后续命令仍用旧版 cargo 读不了 Cargo.lock v4 而失败。）
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
