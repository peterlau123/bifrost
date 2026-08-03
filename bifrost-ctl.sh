#!/bin/bash
# bifrost-ctl.sh - 跨机器控制 bifrost server (本机运行)
#
# 原理: 通过 GPFS 共享存储的控制文件 (bifrost_test/control.json) 给 H20 上的
# supervisor 发指令。supervisor 每 2s 轮询一次, 读到就执行。
# 不需要 SSH, 不需要知道 H20 上的 PID。
#
# 用法:
#   ./bifrost-ctl.sh restart    # 重启 H20 上的 bifrost server
#   ./bifrost-ctl.sh stop       # 关闭 server
#   ./bifrost-ctl.sh status     # 查询状态 (supervisor 会回写 status.json)

set -u

CTRL_DIR="/gpfs/gcsp/liuxin/bifrost_test"
CTRL_FILE="${CTRL_DIR}/control.json"
STATUS_FILE="${CTRL_DIR}/status.json"

action="${1:-}"
case "$action" in
    restart|stop|status)
        # 写入控制指令
        echo "{\"action\": \"$action\", \"requested_at\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$CTRL_FILE"
        echo "指令 [$action] 已写入 $CTRL_FILE, supervisor 将在 2s 内执行"
        # status 时等待回写
        if [[ "$action" == "status" ]]; then
            rm -f "$STATUS_FILE"
            sleep 3
            if [[ -f "$STATUS_FILE" ]]; then
                echo "=== H20 状态 ==="
                cat "$STATUS_FILE"
            else
                echo "未收到状态回写 (supervisor 可能未运行)"
            fi
        fi
        ;;
    *)
        echo "用法: $0 {restart|stop|status}"
        exit 1
        ;;
esac
