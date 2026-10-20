#!/bin/bash
# bifrost-watchdog.sh —— 外部看门狗（可选组件）
#
# 用途：daemon 进程还在、但扫描/心跳停摆时把它拉起来。触发来源有两类：
#   1) heartbeat.json 太旧（daemon 整体卡死 / 被 OOM）
#   2) scan.json 太旧（兜底扫描线程卡死，例如宿主时钟冻结导致 sleep 不返回）
#
# 成本：一次运行只做 2 次 stat + 1 次比较，毫秒级；按 cron 每 2–5 分钟跑一次，
# 平均 CPU 占用可以忽略（远小于一次 `ps`）。
#
# 为什么放在外部而不是 daemon 里：daemon 内部任何自查也依赖同一套时钟/事件
# 机制；进程内自查在"时钟冻死"这种故障下同样会停。外部进程由 kernel 的 cron/
# systemd timer 驱动，是唯一不共享故障域的观察点。
#
# 用法:
#   bifrost-watchdog.sh                      # 用环境变量/默认值
#   bifrost-watchdog.sh --storage /data/x/bifrost --restart "systemctl restart bifrost"
#   bifrost-watchdog.sh --dry-run            # 只报告不重启
#
# cron 示例（每 2 分钟）:
#   */2 * * * * /opt/bifrost/scripts/watchdog.sh --storage /data/liuxin/bifrost \
#       >> /var/log/bifrost-watchdog.log 2>&1
#
# systemd timer 示例见 docs/ops-practice.md。
set -uo pipefail

STORAGE="${BIFROST_STORAGE:-$HOME/.bifrost/data}"
HEARTBEAT_MAX=180   # 秒；心跳默认 60s 一次
SCAN_MAX=300        # 秒；扫描默认 1s 一次，5 分钟没动基本可以判定卡死
RESTART_CMD="${BIFROST_RESTART_CMD:-}"
DRY_RUN=0

usage() {
    sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

# 解析 "180s" / "5m" / "2h" 为秒
parse_dur() {
    local v="$1"
    case "$v" in
        *s|*S) echo "${v%[sS]}" ;;
        *m|*M) echo $(( ${v%[mM]} * 60 )) ;;
        *h|*H) echo $(( ${v%[hH]} * 3600 )) ;;
        *[0-9]) echo "$v" ;;
        *) echo "无法解析时长: $v" >&2; exit 1 ;;
    esac
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage)        STORAGE="$2"; shift 2 ;;
        --heartbeat-max)  HEARTBEAT_MAX=$(parse_dur "$2"); shift 2 ;;
        --scan-max)       SCAN_MAX=$(parse_dur "$2"); shift 2 ;;
        --restart)        RESTART_CMD="$2"; shift 2 ;;
        --dry-run)        DRY_RUN=1; shift ;;
        -h|--help)        usage ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

now=$(date +%s)
problems=()

age_of() {
    local f="$1"
    [[ -f "$f" ]] || { echo ""; return; }
    local m
    m=$(stat -c %Y "$f" 2>/dev/null) || { echo ""; return; }
    echo $(( now - m ))
}

hb_age=$(age_of "$STORAGE/heartbeat.json")
scan_age=$(age_of "$STORAGE/scan.json")

if [[ -n "$hb_age" && "$hb_age" -gt "$HEARTBEAT_MAX" ]]; then
    problems+=("heartbeat stale ${hb_age}s > ${HEARTBEAT_MAX}s")
fi
if [[ -n "$scan_age" && "$scan_age" -gt "$SCAN_MAX" ]]; then
    problems+=("scan stale ${scan_age}s > ${SCAN_MAX}s")
fi

if [[ ${#problems[@]} -eq 0 ]]; then
    echo "[watchdog $(date -Is)] ok (heartbeat=${hb_age:-n/a}s scan=${scan_age:-n/a}s)"
    exit 0
fi

echo "[watchdog $(date -Is)] UNHEALTHY: ${problems[*]}"

if [[ $DRY_RUN -eq 1 ]]; then
    echo "[watchdog] dry-run: would run: ${RESTART_CMD:-<auto>}"
    exit 3
fi

if [[ -z "$RESTART_CMD" ]]; then
    if command -v systemctl >/dev/null 2>&1 && systemctl list-unit-files 2>/dev/null | grep -q '^bifrost.service'; then
        RESTART_CMD="systemctl restart bifrost"
    elif [[ -x "$(dirname "$0")/../bifrost-ctl.sh" ]]; then
        RESTART_CMD="$(cd "$(dirname "$0")/.." && pwd)/bifrost-ctl.sh restart"
    else
        echo "[watchdog] 无可用重启方式，请用 --restart 指定" >&2
        exit 1
    fi
fi

echo "[watchdog] restarting: $RESTART_CMD"
if bash -c "$RESTART_CMD"; then
    echo "[watchdog] restart OK"
    exit 0
else
    echo "[watchdog] restart FAILED" >&2
    exit 2
fi
