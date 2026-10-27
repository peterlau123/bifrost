#!/bin/bash
# e2e.sh —— 跑端到端测试（流水线和本机同一个入口）
#
# 为什么要单独包一层：Gitee Go 的 build@rust 镜像不一定有 python3，
# 直接在流水线里写 python3 会在这一步报 "command not found" 而不是"e2e 挂了"，
# 难区分。这里先探测解释器，缺了就给明确的失败原因。
#
# 用法:
#   bash scripts/ci/e2e.sh                    # 默认 ./target/release/bifrost
#   bash scripts/ci/e2e.sh ./target/release/bifrost
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$ROOT" || exit 1
BIN="${1:-./target/release/bifrost}"

if [ ! -x "$BIN" ]; then
    echo "FAIL: 找不到可执行文件 $BIN（先 cargo build --release）" >&2
    exit 1
fi

PY="$(command -v python3 || command -v python || true)"
if [ -z "$PY" ]; then
    echo "FAIL: 构建镜像里没有 python3/python —— e2e 跑不了。" >&2
    echo "      流水线需要在 build@rust 插件镜像里提供 python3（或换带 python3 的镜像）" >&2
    exit 1
fi

echo "[e2e] 二进制: $BIN"
echo "[e2e] 解释器: $PY ($("$PY" -V 2>&1))"
exec "$PY" "$ROOT/tests/e2e/run_all.py" "$BIN"
