#!/bin/bash
# 解析 cargo 实际产出的 bifrost 二进制路径。
#
# 存在的意义：CARGO_TARGET_DIR / [build] target-dir / --target-dir 都会让产物
# 不再落在 ./target/<profile>/，写死路径的脚本就会拿到旧二进制或直接报错。
# 这里让 cargo 自己报告路径，脚本一律调用本文件，不自己拼路径。
#
# 用法: scripts/find-binary.sh [release|debug|<profile>]
# 输出: 绝对路径；失败时非 0 退出并打印原因到 stderr。
set -euo pipefail

cd "$(dirname "$0")/.."

PROFILE="${1:-release}"
case "$PROFILE" in
  release)      BUILD_ARGS=(--release) ;;
  debug|dev)    BUILD_ARGS=() ;;
  *)            BUILD_ARGS=(--profile "$PROFILE") ;;
esac

BIN=$(cargo build --bin bifrost "${BUILD_ARGS[@]}" --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
out = ""
for line in sys.stdin:
    line = line.strip()
    if not line.startswith("{"):
        continue
    try:
        m = json.loads(line)
    except Exception:
        continue
    tgt = m.get("target") or {}
    if m.get("reason") == "compiler-artifact" and tgt.get("name") == "bifrost" and m.get("executable"):
        out = m["executable"]
print(out)
')

if [[ -z "$BIN" || ! -x "$BIN" ]]; then
  echo "cargo 未产出 bifrost 二进制（profile=${PROFILE}）" >&2
  exit 1
fi

echo "$BIN"
