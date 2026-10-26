#!/bin/bash
# bifrost MCP 一键安装 —— 免编译，直接装静态二进制并注册到 Hermes。
#
# 用法:
#   ./install.sh                              # 装到 /usr/local/bin，注册到 Hermes
#   ./install.sh --storage /data/liuxin/bifrost
#   PREFIX=$HOME/.local/bin ./install.sh      # 无 root 权限时
#   ./install.sh --no-hermes                  # 只装二进制 + settings，不碰 Hermes 配置
#   ./install.sh --name bifrost-h200          # 注册成另一个名字（多集群并存时）
#
# 装完自检:  hermes mcp test <name>
set -uo pipefail

PREFIX="${PREFIX:-/usr/local/bin}"
MCP_NAME="bifrost"
STORAGE=""
NO_HERMES=0
FORCE=0
SETTINGS="${BIFROST_CONFIG:-$HOME/.bifrost/settings.json}"

while [ $# -gt 0 ]; do
    case "$1" in
        --prefix)   PREFIX="$2"; shift 2 ;;
        --storage)  STORAGE="$2"; shift 2 ;;
        --name)     MCP_NAME="$2"; shift 2 ;;
        --settings) SETTINGS="$2"; shift 2 ;;
        --force)    FORCE=1; shift ;;
        --no-hermes) NO_HERMES=1; shift ;;
        -h|--help)  sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/bifrost"
[ -x "$SRC" ] || { echo "FAIL: 包内找不到 bifrost 二进制（$SRC）" >&2; exit 1; }

# ---- 1. 二进制 ----
mkdir -p "$PREFIX" 2>/dev/null
install -m 0755 "$SRC" "$PREFIX/bifrost" 2>/dev/null || {
    echo "FAIL: 无法写入 $PREFIX" >&2
    echo "  改用:  PREFIX=\$HOME/.local/bin $0" >&2
    exit 1
}
BIN="$PREFIX/bifrost"
echo "✓ 二进制: $BIN  ($("$BIN" --version 2>&1))"

# ---- 2. settings.json ----
if [ -f "$SETTINGS" ] && [ -n "$STORAGE" ] && [ "$FORCE" != "1" ]; then
    echo "! 配置已存在，未覆盖（加 --force 才会改写）: $SETTINGS"
    echo "  当前 shared_storage: $(grep -o '"shared_storage"[^,]*' "$SETTINGS" 2>/dev/null)"
    echo "  想改:  $0 --storage $STORAGE --force"
elif [ -n "$STORAGE" ] || [ ! -f "$SETTINGS" ]; then
    mkdir -p "$(dirname "$SETTINGS")"
    cat > "$SETTINGS" <<EOF
{
  "shared_storage": "${STORAGE:-/data/liuxin/bifrost}",
  "transport": "shared",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "3m", "wait_timeout": "12h" },
  "daemon": { "scan_mode": "auto" }
}
EOF
    echo "✓ 配置:   $SETTINGS  →  shared_storage=${STORAGE:-/data/liuxin/bifrost}"
else
    echo "✓ 配置:   $SETTINGS（已存在，未覆盖）"
fi

# ---- 3. MCP 自检（stdio 握手）----
REPLY=$(printf '%s\n' \
  '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"install.sh","version":"0"}}}' \
  | BIFROST_CONFIG="$SETTINGS" timeout 15 "$BIN" mcp-serve 2>/dev/null | head -1)
case "$REPLY" in
    *protocolVersion*) echo "✓ MCP 握手: 正常（stdio 应答已收到）" ;;
    *) echo "! MCP 握手: 未收到应答，手工排查: BIFROST_CONFIG=$SETTINGS $BIN mcp-serve" >&2 ;;
esac

# ---- 4. 注册到 Hermes ----
if [ "$NO_HERMES" = "1" ]; then
    echo "· 已跳过 Hermes 注册（--no-hermes）"
    echo "  手工注册: hermes mcp add $MCP_NAME --env BIFROST_CONFIG=$SETTINGS --command $BIN --args mcp-serve"
    exit 0
fi

HERMES_BIN="$(command -v hermes 2>/dev/null || true)"
if [ -z "$HERMES_BIN" ]; then
    for c in /opt/hermes/bin/hermes /opt/hermes/libexec/hermes "$HOME/.local/bin/hermes"; do
        [ -x "$c" ] && { HERMES_BIN="$c"; break; }
    done
fi

if [ -z "$HERMES_BIN" ]; then
    echo "! 未找到 hermes 命令，跳过注册。装好 Hermes 后执行:"
    echo "  hermes mcp add $MCP_NAME --env BIFROST_CONFIG=$SETTINGS --command $BIN --args mcp-serve"
    exit 0
fi

echo "→ 注册到 Hermes: $HERMES_BIN"
printf 'y\n' | "$HERMES_BIN" mcp add "$MCP_NAME" \
    --connect-timeout 30 \
    --env BIFROST_CONFIG="$SETTINGS" \
    --command "$BIN" \
    --args mcp-serve

echo
echo "下一步:"
echo "  hermes mcp list            # 确认 $MCP_NAME 已启用"
echo "  hermes mcp test $MCP_NAME  # 打印发现的工具"
echo "  新会话中工具名为 mcp_${MCP_NAME}_*（已有会话需 /reload-mcp）"
