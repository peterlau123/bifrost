#!/bin/bash
# deploy-client.sh —— 在**联网机（client 侧）**上装好 bifrost CLI 并生成配置。
#
# 只做三件事：装二进制 → 生成 client 侧 settings.json → 报告 daemon 是否在线。
# 不需要 root（默认装到 /usr/local/bin，写不进去就提示用 sudo 或改 --prefix）。
#
# 用法:
#   scripts/deploy/deploy-client.sh --storage /data/liuxin/bifrost
#   scripts/deploy/deploy-client.sh --storage /data/liuxin/bifrost --prefix ~/bin
#   scripts/deploy/deploy-client.sh --storage /data/liuxin/bifrost --binary dist/bifrost-linux-x86_64
#
# 必选:
#   --storage DIR      共享目录（必须与 daemon 侧同一个物理目录）
# 可选:
#   --binary PATH      安装这个二进制（默认 scripts/build/build.sh 现编 release）
#   --config FILE      settings.json 路径（默认 ~/.bifrost/settings.json）
#   --prefix DIR       安装目录（默认 /usr/local/bin）
#   --skip-build       不编译，直接用 --binary 或已有产物
#   --force            覆盖已有 settings.json
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
STORAGE=""
BINARY=""
CONFIG=""
PREFIX="/usr/local/bin"
SKIP_BUILD=0
FORCE=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage)    STORAGE="$2"; shift 2 ;;
        --binary)     BINARY="$2"; shift 2 ;;
        --config)     CONFIG="$2"; shift 2 ;;
        --prefix)     PREFIX="$2"; shift 2 ;;
        --skip-build) SKIP_BUILD=1; shift ;;
        --force)      FORCE=1; shift ;;
        -h|--help)    sed -n '2,26p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$STORAGE" ]] || { echo "必选参数缺失: --storage <共享目录>" >&2; exit 1; }
INSTALL="$PREFIX/bifrost"
# ~ 展开（--prefix ~/bin 这种写法）
PREFIX="${PREFIX/#\~/$HOME}"; INSTALL="$PREFIX/bifrost"

# ── 1. 取二进制 ────────────────────────────────────────────────────
if [[ -z "$BINARY" ]]; then
    if [[ "$SKIP_BUILD" == "1" ]]; then
        BINARY="$REPO/target/release/bifrost"
    else
        BINARY="$(bash "$REPO/scripts/build/build.sh" 2>/dev/null)" || {
            echo "编译失败。没有 Rust 工具链时用 --binary 指定已有产物，例如：" >&2
            echo "  $0 --storage $STORAGE --binary $REPO/dist/bifrost-linux-x86_64" >&2
            exit 1
        }
    fi
fi
[[ -x "$BINARY" ]] || { echo "$BINARY 不存在或不可执行" >&2; exit 1; }
echo "candidate: $BINARY ($(sha256sum "$BINARY" | cut -c1-16))"

# 二进制能跑才装
"$BINARY" --version || { echo "候选二进制在本机起不来（架构/glibc 不匹配）" >&2; exit 1; }

# ── 2. 安装 ────────────────────────────────────────────────────────
if [[ ! -d "$PREFIX" ]]; then
    mkdir -p "$PREFIX" 2>/dev/null || true
fi
if [[ -w "$PREFIX" ]]; then
    install -m 0755 "$BINARY" "$INSTALL" || { echo "安装到 $INSTALL 失败" >&2; exit 1; }
else
    echo "$PREFIX 不可写。要么用 sudo 重跑，要么换 --prefix（例如 --prefix \$HOME/bin）" >&2
    exit 1
fi
echo "installed: $INSTALL"

# ── 3. 生成 client 侧配置 ──────────────────────────────────────────
INIT_ARGS=(client init --storage "$STORAGE")
[[ -n "$CONFIG" ]] && INIT_ARGS+=(--path "$CONFIG")
[[ "$FORCE" == "1" ]] && INIT_ARGS+=(--force)
"$INSTALL" "${INIT_ARGS[@]}" || exit 1

# ── 4. 报一下 daemon 是否在线（不阻断部署）──────────────────────────
HB="$STORAGE/heartbeat.json"
if [[ -f "$HB" ]]; then
    age=$(( $(date +%s) - $(stat -c %Y "$HB") ))
    if [[ "$age" -lt 180 ]]; then
        echo "daemon: online（心跳 ${age}s 前）"
    else
        echo "daemon: 心跳已 ${age}s 未更新 —— 目标机上的 daemon 可能没跑"
    fi
else
    echo "daemon: 还没有 heartbeat.json —— 先把 server 侧装起来（scripts/deploy/deploy-server.sh）"
fi

echo
echo "下一步："
echo "  $INSTALL client submit --storage $STORAGE --command 'hostname'"
echo "  完整样例：scripts/examples/quickstart.sh --storage $STORAGE"
