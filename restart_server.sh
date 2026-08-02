#!/bin/bash
# restart_server.sh - 重启 bifrost server (H20 侧)
#
# 用法:
#   ./restart_server.sh                 # 用默认配置重启
#   ./restart_server.sh -c <config>     # 指定配置重启
#   ./restart_server.sh -b <binary>     # 指定 binary 路径
#   ./restart_server.sh --log           # 重启后 tail 日志
#
# 说明:
#   - kill 旧 server (SIGKILL, 因为 Ctrl+C 处理在旧版可能不干净)
#   - 启动新 server, 日志追加到 bifrost_test/server.log
#   - 等待 2s 验证 heartbeat 文件刷新, 确认启动成功

set -euo pipefail

# ── 默认路径 ────────────────────────────────────────────────────────────────
BIFROST_DIR="/gpfs/gcsp/liuxin/bifrost"
BINARY="${BIFROST_DIR}/target/release/bifrost"
CONFIG="/gpfs/gcsp/liuxin/bifrost_test/settings.json"
LOG_FILE="/gpfs/gcsp/liuxin/bifrost_test/server.log"
TAIL_LOG=0

# ── 解析参数 ────────────────────────────────────────────────────────────────
while [[ $# -gt 0 ]]; do
    case "$1" in
        -b|--binary) BINARY="$2"; shift 2 ;;
        -c|--config) CONFIG="$2"; shift 2 ;;
        --log)       TAIL_LOG=1; shift ;;
        -h|--help)
            echo "用法: $0 [-b <binary>] [-c <config>] [--log]"
            exit 0 ;;
        *) echo "未知参数: $1"; exit 1 ;;
    esac
done

# ── 校验 ────────────────────────────────────────────────────────────────────
if [[ ! -x "$BINARY" ]]; then
    echo "✗ binary 不存在或不可执行: $BINARY"
    echo "  先编译: cd $BIFROST_DIR && cargo build --release"
    exit 1
fi
if [[ ! -f "$CONFIG" ]]; then
    echo "✗ 配置不存在: $CONFIG"
    exit 1
fi

echo "== bifrost server 重启 =="
echo "  binary: $BINARY"
echo "  config: $CONFIG"

# ── kill 旧 server ──────────────────────────────────────────────────────────
OLD_PIDS=$(pgrep -f "bifrost server" || true)
if [[ -n "$OLD_PIDS" ]]; then
    echo "  kill 旧 server: $OLD_PIDS"
    kill -9 $OLD_PIDS 2>/dev/null || true
    sleep 1
else
    echo "  无旧 server 进程"
fi

# ── 启动新 server ───────────────────────────────────────────────────────────
mkdir -p "$(dirname "$LOG_FILE")"
nohup "$BINARY" server -c "$CONFIG" >> "$LOG_FILE" 2>&1 &
NEW_PID=$!
echo "  已启动, pid: $NEW_PID, 日志: $LOG_FILE"

# ── 验证心跳 ────────────────────────────────────────────────────────────────
echo "  等待 server 就绪..."
sleep 2
if kill -0 "$NEW_PID" 2>/dev/null; then
    echo "  ✓ server 运行中 (pid $NEW_PID)"
else
    echo "  ✗ server 启动失败! 最后 20 行日志:"
    tail -20 "$LOG_FILE"
    exit 1
fi

# ── 可选: tail 日志 ─────────────────────────────────────────────────────────
if [[ "$TAIL_LOG" == "1" ]]; then
    echo ""
    echo "== 日志 (Ctrl+C 退出 tail, 不影响 server) =="
    tail -f "$LOG_FILE"
fi

echo "== 完成 =="
