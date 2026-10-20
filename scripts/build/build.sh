#!/bin/bash
# build.sh —— 编译 bifrost，并解析出真实产物路径（不写死 target/release）。
#
# 为什么单独一个脚本：产物位置会被 CARGO_TARGET_DIR、[build] target-dir、--target、
# --profile 改掉，脚本里写死 `target/release/bifrost` 拿到的是旧二进制甚至不存在。
# 这里统一让 cargo 自己报告路径（--message-format=json），其它脚本一律调本脚本。
#
# 用法:
#   scripts/build/build.sh                      # release（本机 gnu 产物）
#   scripts/build/build.sh --static             # musl 静态产物（部署到宿主用这个）
#   scripts/build/build.sh --profile debug
#   scripts/build/build.sh --target aarch64-unknown-linux-gnu
#   scripts/build/build.sh --static --dist      # 顺带复制到 dist/bifrost-linux-x86_64
#   scripts/build/build.sh -- --features foo    # `--` 之后的参数原样透传给 cargo
#
# 输出: 最后一行是产物绝对路径（其余是进度信息，都打到 stderr）
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$REPO" || exit 1

PROFILE="release"
TARGET=""
DIST=0
CARGO_EXTRA=()
MUSL_TRIPLE="x86_64-unknown-linux-musl"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --profile)  PROFILE="$2"; shift 2 ;;
        --target)   TARGET="$2"; shift 2 ;;
        --static)   TARGET="$MUSL_TRIPLE"; PROFILE="release"; shift ;;
        --dist)     DIST=1; shift ;;
        -h|--help)  sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        --)         shift; CARGO_EXTRA=("$@"); break ;;
        *)          echo "未知参数: $1（透传 cargo 参数请放在 -- 之后）" >&2; exit 1 ;;
    esac
done

# 构建参数
BUILD_ARGS=(build --bin bifrost)
if [[ "$PROFILE" == "debug" || "$PROFILE" == "dev" ]]; then
    :
elif [[ "$PROFILE" == "release" ]]; then
    BUILD_ARGS+=(--release)
else
    BUILD_ARGS+=(--profile "$PROFILE")
fi
[[ -n "$TARGET" ]] && BUILD_ARGS+=(--target "$TARGET")
[[ ${#CARGO_EXTRA[@]} -gt 0 ]] && BUILD_ARGS+=("${CARGO_EXTRA[@]}")

echo "==> cargo ${BUILD_ARGS[*]}" >&2
BIN=$(cargo "${BUILD_ARGS[@]}" --message-format=json 2>/dev/null | python3 -c '
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
    t = m.get("target") or {}
    if m.get("reason") == "compiler-artifact" and t.get("name") == "bifrost" and m.get("executable"):
        out = m["executable"]
print(out)
')

if [[ -z "$BIN" || ! -x "$BIN" ]]; then
    echo "编译失败：cargo 没有产出 bifrost 二进制（profile=$PROFILE target=${TARGET:-host}）" >&2
    echo "提示：宿主没有 Rust 工具链时，用 --binary 指定已有产物，或在本机先编译" >&2
    exit 1
fi

if [[ "$DIST" == "1" ]]; then
    mkdir -p "$REPO/dist"
    if [[ "$TARGET" == "$MUSL_TRIPLE" || -z "$TARGET" ]]; then
        DEST="$REPO/dist/bifrost-linux-x86_64"
    else
        DEST="$REPO/dist/bifrost-linux-x86_64-glibc"
    fi
    cp -f "$BIN" "$DEST"
    echo "已复制到 $DEST" >&2
    echo "sha256: $(sha256sum "$DEST" | cut -d' ' -f1)" >&2
    echo "$DEST"
    exit 0
fi

echo "artifact: $BIN" >&2
echo "sha256:   $(sha256sum "$BIN" | cut -d' ' -f1)" >&2
echo "$BIN"
