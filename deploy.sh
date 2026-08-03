#!/bin/bash
# deploy.sh - bifrost 一键发布部署工具 (本机运行)
#
# 一条命令完成: 编译 release -> 更新 MCP server -> 重启 H20 daemon
# 全程无需 SSH, 通过 supervisor 的 GPFS control.json 远程控制。
#
# 用法:
#   ./deploy.sh              # 完整发布: build + MCP 更新 + daemon 重启
#   ./deploy.sh --no-build   # 跳过编译 (binary 已是最新)
#   ./deploy.sh --no-mcp     # 不重启 MCP (MCP 没改代码时)
#   ./deploy.sh --no-daemon  # 不重启 H20 daemon (只发 client/MCP)
#   ./deploy.sh --check      # 只检查部署状态, 不执行
#   ./deploy.sh --help

set -euo pipefail

# ── 路径 ────────────────────────────────────────────────────────────────────
BIFROST_DIR="/gpfs/gcsp/liuxin/bifrost"
BINARY="${BIFROST_DIR}/target/release/bifrost"
CTL="${BIFROST_DIR}/bifrost-ctl.sh"

# 默认全做
DO_BUILD=1
DO_MCP=1
DO_DAEMON=1
CHECK_ONLY=0

# ── 参数解析 ────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        --no-build)  DO_BUILD=0; shift ;;
        --no-mcp)    DO_MCP=0; shift ;;
        --no-daemon) DO_DAEMON=0; shift ;;
        --check)     CHECK_ONLY=1; shift ;;
        --help|-h)
            echo "用法: $0 [--no-build] [--no-mcp] [--no-daemon] [--check]"
            echo "  默认: 编译 + 更新 MCP + 重启 daemon 全做"
            exit 0 ;;
        *) echo "未知参数: $1"; exit 1 ;;
    esac
done

# ── 工具函数 ────────────────────────────────────────────────────────────────
section() { echo ""; echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"; echo "  $1"; echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"; }
ok()   { echo "  ✅ $1"; }
warn() { echo "  ⚠️  $1"; }
fail() { echo "  ❌ $1"; }

# ── 检查模式 ────────────────────────────────────────────────────────────────
if [[ "$CHECK_ONLY" == "1" ]]; then
    section "🔍 部署状态检查"
    echo "  本机:"
    echo "    binary: $BINARY"
    if [[ -x "$BINARY" ]]; then
        local_ver=$(stat -c "%y" "$BINARY" 2>/dev/null | cut -d. -f1)
        ok "binary 存在 (编译于 $local_ver)"
    else
        fail "binary 不存在, 先 cargo build --release"
    fi
    echo "  本机 MCP server:"
    mcp_pid=$(pgrep -f "bifrost mcp-serve" | head -1 || true)
    if [[ -n "$mcp_pid" ]]; then
        mcp_start=$(ps -o lstart= -p "$mcp_pid" 2>/dev/null)
        warn "MCP 常驻进程 pid=$mcp_pid (启动于 $mcp_start) - 若 binary 新于它需重启"
    else
        warn "MCP 未运行 (首次调用时自动拉起)"
    fi
    echo "  H20 daemon (通过 supervisor):"
    "$CTL" status 2>/dev/null || echo "    ⚠️ supervisor 未响应"
    exit 0
fi

# ── 1. 编译 ─────────────────────────────────────────────────────────────────
if [[ "$DO_BUILD" == "1" ]]; then
    section "🔨 1/3 编译 release"
    (cd "$BIFROST_DIR" && cargo build --release 2>&1 | tail -3)
    if [[ ! -x "$BINARY" ]]; then
        fail "编译失败, 无 binary 产物"
        exit 1
    fi
    ver=$(stat -c "%y" "$BINARY" | cut -d. -f1)
    ok "编译完成: $BINARY (${ver})"
else
    section "🔨 1/3 跳过编译 (--no-build)"
fi

# ── 2. 更新 MCP server ──────────────────────────────────────────────────────
if [[ "$DO_MCP" == "1" ]]; then
    section "🔁 2/3 更新本机 MCP server"
    mcp_pid=$(pgrep -f "bifrost mcp-serve" | head -1 || true)
    if [[ -n "$mcp_pid" ]]; then
        mcp_start=$(stat -c "%y" "/proc/$mcp_pid/exe" 2>/dev/null || echo "unknown")
        bin_ver=$(stat -c "%y" "$BINARY")
        if [[ "$mcp_start" < "$bin_ver" ]] 2>/dev/null; then
            warn "MCP 进程 ($mcp_pid) 早于新 binary, kill 重启"
            pkill -9 -f "bifrost mcp-serve" 2>/dev/null || true
            sleep 1
            ok "MCP 已 kill, 下次调用自动拉起新版"
        else
            ok "MCP 进程已是最新, 无需重启"
        fi
    else
        ok "MCP 未运行, 无需处理 (首次调用自动拉起)"
    fi
else
    section "🔁 2/3 跳过 MCP 更新 (--no-mcp)"
fi

# ── 3. 重启 H20 daemon ──────────────────────────────────────────────────────
if [[ "$DO_DAEMON" == "1" ]]; then
    section "🔄 3/3 重启 H20 daemon"
    if [[ -x "$CTL" ]]; then
        "$CTL" restart
        sleep 3
        # 验证
        "$CTL" status
    else
        fail "bifrost-ctl.sh 不存在: $CTL"
        exit 1
    fi
else
    section "🔄 3/3 跳过 daemon 重启 (--no-daemon)"
fi

# ── 完成 ────────────────────────────────────────────────────────────────────
section "🎉 部署完成"
echo "  二进制: $BINARY"
echo "  下一版本发布: 再次运行 ./deploy.sh 即可"
echo ""
echo "  MCP/daemon 均已更新到最新 binary。"
