#!/bin/bash
# bifrost-supervisor.sh - bifrost server 守护进程 (H20 侧)
#
# 常驻后台, 通过信号控制 bifrost server 生命周期, 免去手动 kill/启动。
#
# 用法:
#   ./bifrost-supervisor.sh start        # 启动 supervisor (后台)
#   ./bifrost-supervisor.sh stop         # 停止 supervisor + server
#   ./bifrost-supervisor.sh restart      # 重启 supervisor (SIGHUP)
#   ./bifrost-supervisor.sh status       # 查看状态
#   ./bifrost-supervisor.sh attach       # 前台运行 (调试)
#
# 信号控制 (attach 模式):
#   kill -HUP  <pid>   # 重启 bifrost server (改代码编译后用这个!)
#   kill -TERM <pid>   # 关闭 bifrost server 并退出 supervisor
#   kill -USR1 <pid>   # 打印状态
#
# 常驻模式: start 后 supervisor 以 nohup 后台运行, PID 存到 <dir>/supervisor.pid。
# 即使 SSH 断开也继续运行。

set -u

# ── 路径 ────────────────────────────────────────────────────────────────────
BIFROST_DIR="/gpfs/gcsp/liuxin/bifrost"
BINARY="${BIFROST_DIR}/target/release/bifrost"
CONFIG="/gpfs/gcsp/liuxin/bifrost_test/settings.json"
STATE_DIR="/gpfs/gcsp/liuxin/bifrost_test"
LOG_FILE="${STATE_DIR}/server.log"
PID_FILE="${STATE_DIR}/supervisor.pid"
SERVER_PID_FILE="${STATE_DIR}/server.pid"

# ── 工具函数 ────────────────────────────────────────────────────────────────

log() { echo "[$(date '+%H:%M:%S')] $*" >> "$LOG_FILE"; }

server_pid() {
    # 用 pid 文件优先, 否则 pgrep
    if [[ -f "$SERVER_PID_FILE" ]]; then
        local pid
        pid=$(cat "$SERVER_PID_FILE" 2>/dev/null)
        if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
            echo "$pid"
            return
        fi
    fi
    pgrep -f "bifrost server -c" | head -1
}

stop_server() {
    local pid
    pid=$(server_pid)
    if [[ -n "$pid" ]]; then
        log "stop server pid=$pid"
        kill -TERM "$pid" 2>/dev/null
        sleep 1
        if kill -0 "$pid" 2>/dev/null; then
            log "server still alive, SIGKILL"
            kill -9 "$pid" 2>/dev/null
        fi
    fi
    rm -f "$SERVER_PID_FILE"
}

start_server() {
    # 确保 working_dir 存在
    local wd
    wd=$(python3 -c "import json;print(json.load(open('$CONFIG'))['daemon'].get('working_dir','/tmp/bifrost/work'))" 2>/dev/null || echo "/tmp/bifrost/work")
    mkdir -p "$wd"
    log "start server: $BINARY server -c $CONFIG"
    nohup "$BINARY" server -c "$CONFIG" >> "$LOG_FILE" 2>&1 &
    echo $! > "$SERVER_PID_FILE"
    # 等就绪 (heartbeat 文件出现/刷新)
    sleep 2
    if kill -0 "$(cat "$SERVER_PID_FILE")" 2>/dev/null; then
        log "server ready pid=$(cat "$SERVER_PID_FILE")"
    else
        log "server FAILED to start, tail log:"
        tail -5 "$LOG_FILE" >> "$LOG_FILE"
    fi
}

do_restart() {
    log "== restart requested =="
    stop_server
    start_server
}

do_stop() {
    log "== stop requested =="
    stop_server
    log "supervisor exiting"
    rm -f "$PID_FILE"
    exit 0
}

do_status() {
    local srv_pid sup_pid
    srv_pid=$(server_pid)
    sup_pid=$$
    echo "supervisor:  running (pid=$sup_pid)"
    if [[ -n "$srv_pid" ]]; then
        echo "bifrost server: running (pid=$srv_pid)"
        ps -o pid,lstart,cmd -p "$srv_pid" | tail -1
    else
        echo "bifrost server: STOPPED"
    fi
    echo "log: $LOG_FILE"
    # 回写到共享存储 (供本机 bifrost-ctl.sh status 读取)
    if [[ -n "$srv_pid" ]]; then
        echo "{\"supervisor_pid\": $sup_pid, \"server_pid\": $srv_pid, \"server\": \"running\", \"time\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "${STATE_DIR}/status.json"
    else
        echo "{\"supervisor_pid\": $sup_pid, \"server_pid\": null, \"server\": \"stopped\", \"time\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "${STATE_DIR}/status.json"
    fi
}

# ── 信号处理 ────────────────────────────────────────────────────────────────
trap 'do_restart'  HUP
trap 'do_stop'     TERM INT
trap 'do_status'   USR1

# ── 主流程 ───────────────────────────────────────────────────────────────────
case "${1:-}" in
    start)
        # 已有 supervisor? 拒绝重复启动
        if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
            echo "supervisor already running (pid=$(cat "$PID_FILE"))"
            exit 1
        fi
        echo "starting bifrost-supervisor..."
        nohup "$0" attach >> "$LOG_FILE" 2>&1 &
        echo $! > "$PID_FILE"
        echo "supervisor pid: $(cat "$PID_FILE")  (log: $LOG_FILE)"
        echo "  重启 server: kill -HUP  $(cat "$PID_FILE")"
        echo "  关闭:        kill -TERM $(cat "$PID_FILE")"
        ;;
    stop)
        if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
            kill -TERM "$(cat "$PID_FILE")"
            echo "stop signal sent to supervisor pid=$(cat "$PID_FILE")"
        else
            echo "no supervisor running, stopping server directly"
            pkill -f "bifrost server -c" 2>/dev/null
        fi
        ;;
    restart)
        if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
            kill -HUP "$(cat "$PID_FILE")"
            echo "restart signal sent to supervisor pid=$(cat "$PID_FILE")"
        else
            echo "no supervisor running; starting fresh"
            "$0" start
        fi
        ;;
    status)
        if [[ -f "$PID_FILE" ]] && kill -0 "$(cat "$PID_FILE")" 2>/dev/null; then
            kill -USR1 "$(cat "$PID_FILE")"
            sleep 1
        else
            echo "supervisor: not running"
        fi
        ;;
    attach)
        # 前台常驻循环: 保证 server 存活 (崩溃自动拉起) + 轮询共享存储控制文件
        log "== supervisor attach (pid=$$) =="
        start_server
        CONTROL_FILE="${STATE_DIR}/control.json"
        rm -f "$CONTROL_FILE"
        while true; do
            sleep 2
            # 1. server 崩溃自动拉起
            if ! kill -0 "$(server_pid 2>/dev/null)" 2>/dev/null; then
                log "server died, restarting..."
                start_server
            fi
            # 2. 轮询共享存储控制文件 (跨机器控制: 本机写 control.json)
            if [[ -f "$CONTROL_FILE" ]]; then
                action=$(python3 -c "import json;print(json.load(open('$CONTROL_FILE')).get('action',''))" 2>/dev/null)
                case "$action" in
                    restart) log "control: restart"; do_restart ;;
                    stop)    log "control: stop"; do_stop ;;
                    status)  do_status ;;
                esac
                rm -f "$CONTROL_FILE"
            fi
        done
        ;;
    *)
        echo "用法: $0 {start|stop|restart|status|attach}"
        exit 1
        ;;
esac
