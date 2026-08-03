#!/bin/bash
# bifrost-supervisor.sh - bifrost server 守护进程 (H20 侧, 长期稳定运行版)
#
# 常驻后台, 通过信号 + 共享存储控制文件管理 bifrost server 生命周期。
#
# 用法:
#   ./bifrost-supervisor.sh start        # 启动 supervisor (后台)
#   ./bifrost-supervisor.sh stop         # 停止 supervisor + server
#   ./bifrost-supervisor.sh restart      # 重启 supervisor (SIGHUP)
#   ./bifrost-supervisor.sh status       # 查看状态
#   ./bifrost-supervisor.sh attach       # 前台运行 (调试)
#
# 本地信号控制 (H20 上):
#   kill -HUP  <pid>   # 重启 bifrost server
#   kill -TERM <pid>   # 关闭 bifrost server 并退出 supervisor
#   kill -USR1 <pid>   # 打印状态
#
# 跨机器控制 (本机, 通过 GPFS 共享存储):
#   ./bifrost-ctl.sh restart|stop|status
#
# 健壮性特性:
#   - 单实例锁 (flock), 防重复启动
#   - server 崩溃自动拉起 (2s 健康检查 + 退避重试)
#   - 日志轮转 (超过 5MB 归档)
#   - 控制文件解析容错 (坏 JSON 忽略)
#   - nohup 后台运行, SSH 断开不影响
#   - 可通过 crontab @reboot 开机自启

set -u

# ── 路径 ────────────────────────────────────────────────────────────────────
BIFROST_DIR="/gpfs/gcsp/liuxin/bifrost"
BINARY="${BIFROST_DIR}/target/release/bifrost"
CONFIG="/gpfs/gcsp/liuxin/bifrost_test/settings.json"
STATE_DIR="/gpfs/gcsp/liuxin/bifrost_test"
LOG_FILE="${STATE_DIR}/server.log"
PID_FILE="${STATE_DIR}/supervisor.pid"
SERVER_PID_FILE="${STATE_DIR}/server.pid"
CONTROL_FILE="${STATE_DIR}/control.json"
STATUS_FILE="${STATE_DIR}/status.json"
LOCK_FILE="${STATE_DIR}/supervisor.lock"

# 日志轮转阈值 (字节)
LOG_MAX_BYTES=$((5 * 1024 * 1024))
# 启动失败退避: 连续失败次数, 指数退避上限 60s
MAX_BACKOFF=60

# ── 工具函数 ────────────────────────────────────────────────────────────────

log() { echo "[$(date '+%Y-%m-%d %H:%M:%S')] $*" >> "$LOG_FILE"; }

rotate_log() {
    # 日志超过阈值则归档 (保留最近 3 个)
    if [[ -f "$LOG_FILE" ]] && [[ $(stat -c%s "$LOG_FILE" 2>/dev/null || echo 0) -gt "$LOG_MAX_BYTES" ]]; then
        local i
        for i in 3 2 1; do
            [[ -f "${LOG_FILE}.$((i-1))" ]] && mv "${LOG_FILE}.$((i-1))" "${LOG_FILE}.$i" 2>/dev/null
        done
        mv "$LOG_FILE" "${LOG_FILE}.1" 2>/dev/null
        : > "$LOG_FILE"
        log "日志轮转: 归档为 server.log.1"
    fi
}

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
        # 等最多 5s 优雅退出
        for _ in $(seq 1 10); do
            kill -0 "$pid" 2>/dev/null || break
            sleep 0.5
        done
        if kill -0 "$pid" 2>/dev/null; then
            log "server still alive after 5s, SIGKILL"
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
    # 等就绪 (最多 5s)
    for _ in $(seq 1 10); do
        if kill -0 "$(cat "$SERVER_PID_FILE" 2>/dev/null)" 2>/dev/null; then
            # server 进程活着, 且 heartbeat 文件新鲜
            if [[ -f "${STATE_DIR}/heartbeat.json" ]]; then
                break
            fi
        fi
        sleep 0.5
    done
    if kill -0 "$(cat "$SERVER_PID_FILE" 2>/dev/null)" 2>/dev/null; then
        log "server ready pid=$(cat "$SERVER_PID_FILE")"
        return 0
    else
        log "server FAILED to start:"
        tail -10 "$LOG_FILE" >> "$LOG_FILE"
        return 1
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
    # 释放 flock (exec fd 关闭即释放)
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
        echo "{\"supervisor_pid\": $sup_pid, \"server_pid\": $srv_pid, \"server\": \"running\", \"time\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$STATUS_FILE"
    else
        echo "{\"supervisor_pid\": $sup_pid, \"server_pid\": null, \"server\": \"stopped\", \"time\": \"$(date -u +%Y-%m-%dT%H:%M:%SZ)\"}" > "$STATUS_FILE"
    fi
}

# ── 单实例锁 ────────────────────────────────────────────────────────────────
acquire_lock() {
    exec 9>"$LOCK_FILE"
    if ! flock -n 9; then
        echo "另一个 supervisor 已在运行 (lock: $LOCK_FILE)"
        exit 1
    fi
}

# ── 信号处理 ────────────────────────────────────────────────────────────────
trap 'do_restart'  HUP
trap 'do_stop'     TERM INT
trap 'do_status'   USR1

# ── 主流程 ───────────────────────────────────────────────────────────────────
case "${1:-}" in
    start)
        # 单实例锁检查 (防重复启动)
        exec 9>"$LOCK_FILE"
        if ! flock -n 9; then
            echo "supervisor already running (lock: $LOCK_FILE)"
            exit 1
        fi
        echo "starting bifrost-supervisor..."
        nohup "$0" attach >> "$LOG_FILE" 2>&1 &
        echo $! > "$PID_FILE"
        # 注意: flock 继承到子进程, 子进程 attach 时再 acquire 会失败
        # 所以 attach 模式不重新 acquire, 这里直接放行
        echo "supervisor pid: $(cat "$PID_FILE")  (log: $LOG_FILE)"
        echo "  重启 server (本机): ./bifrost-ctl.sh restart"
        echo "  关闭 (本机):        ./bifrost-ctl.sh stop"
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
        # 前台常驻循环: server 崩溃自动拉起 (退避重试) + 轮询共享存储控制文件
        log "== supervisor attach (pid=$$) =="
        start_server
        FAIL_COUNT=0
        rm -f "$CONTROL_FILE"
        while true; do
            rotate_log
            sleep 2

            # 1. server 崩溃自动拉起 (退避: 连续失败指数增长, 上限 60s)
            if ! kill -0 "$(server_pid 2>/dev/null)" 2>/dev/null; then
                FAIL_COUNT=$((FAIL_COUNT + 1))
                if start_server; then
                    FAIL_COUNT=0
                else
                    backoff=$((2 ** (FAIL_COUNT > 6 ? 6 : FAIL_COUNT)))
                    [[ $backoff -gt $MAX_BACKOFF ]] && backoff=$MAX_BACKOFF
                    log "启动失败 #$FAIL_COUNT, ${backoff}s 后重试"
                    sleep "$backoff"
                    continue
                fi
            else
                FAIL_COUNT=0
            fi

            # 2. 轮询共享存储控制文件 (跨机器控制: 本机写 control.json)
            if [[ -f "$CONTROL_FILE" ]]; then
                action=$(python3 -c "import json;print(json.load(open('$CONTROL_FILE')).get('action',''))" 2>/dev/null)
                case "$action" in
                    restart) log "control: restart"; do_restart ;;
                    stop)    log "control: stop"; do_stop ;;
                    status)  do_status ;;
                    *)       log "control: unknown action '$action', ignored" ;;
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
