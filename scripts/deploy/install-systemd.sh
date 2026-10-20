#!/bin/bash
# install-systemd.sh —— 把 bifrost daemon 装成 systemd 服务（按实际布局渲染 unit）。
#
# 为什么需要：仓库根的 bifrost.service 写死 WorkingDirectory=/var/lib/bifrost 和
# --config /etc/bifrost/settings.json（本地私有布局），套到共享盘布局上 unit 直接起不来；
# 而手起的 daemon 挂在用户 login session 的 scope 里，SSH 一断可能被 logind 清掉，
# 也没有 Restart= 兜底。本脚本按 --storage 渲染一份对得上的 unit。
#
# 用法: scripts/install-systemd.sh [--storage DIR] [--install PATH] [--dry-run]
# 幂等：unit 已存在且在跑时，重渲染 + daemon-reload + restart。
#
# 安全硬化项（ProtectSystem/NoNewPrivileges/PrivateTmp）**故意不加**：这台机器的
# 用途就是以 root 跑任意命令（benchmark 脚本要写 /tmp、/workspace、按需 mount），
# 硬化只会制造"昨天能跑的脚本今天 permission denied"。要收紧请针对具体任务做。
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
STORAGE="/data/liuxin/bifrost"
INSTALL="/usr/local/bin/bifrost"
CONFIG=""
SERVICE="bifrost.service"
UNIT_PATH="/etc/systemd/system/$SERVICE"
DRY_RUN=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage) STORAGE="$2"; shift 2 ;;
        --install) INSTALL="$2"; shift 2 ;;
        --config)  CONFIG="$2"; shift 2 ;;
        --dry-run) DRY_RUN=1; shift ;;
        -h|--help) sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

if [[ "$DRY_RUN" != "1" ]]; then
    [[ "$(id -u)" == "0" ]] || { echo "必须以 root 运行（--dry-run 可免）" >&2; exit 1; }
    [[ -x "$INSTALL" ]] || { echo "$INSTALL 不存在或不可执行" >&2; exit 1; }
    command -v systemctl >/dev/null || { echo "没有 systemctl，本机不是 systemd 系统" >&2; exit 1; }
fi

if [[ -n "$CONFIG" ]]; then
    EXEC_TARGET="--config $CONFIG"
else
    EXEC_TARGET="--storage $STORAGE"
fi

render_unit() {
    cat <<EOF
[Unit]
Description=Bifrost command execution daemon (shared storage: $STORAGE)
Documentation=https://github.com/peterlau123/bifrost
After=network.target

[Service]
# Type=notify：等 daemon 真正就绪（目录建好、时钟探过、扫描循环起来）才认为启动完成。
# --systemd 让它把 READY=1 + STATUS= 发到 NOTIFY_SOCKET（systemd 注入的环境变量），
# 卡死由 TimeoutStartSec 兜住。
Type=notify
NotifyAccess=main
TimeoutStartSec=60
User=root
# 任务默认 cwd 由此决定（daemon.working_dir 可覆盖）
WorkingDirectory=$STORAGE
ExecStart=$INSTALL server ${EXEC_TARGET} --systemd
# 上线自检：结果进 journal，不阻断启动（就绪之后跑）
ExecStartPost=-$INSTALL warmup ${EXEC_TARGET}

# SIGTERM → daemon 自己杀干净所有任务的进程组，再退出
KillSignal=SIGTERM
# mixed: 只给主进程发 SIGTERM，超时后才对整组 SIGKILL（默认 control-group 会先打任务）
KillMode=mixed
TimeoutStopSec=30

Restart=on-failure
RestartSec=5

# 2TB 内存 / 192 核的机器，别用模板里的 MemoryMax=8G 把 benchmark 打 OOM；
# TasksMax 也不能小：8 个并发 vLLM 任务各自的进程/线程数远超 100。
LimitNOFILE=65536
TasksMax=4096
OOMScoreAdjust=-500

StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
}

if [[ "$DRY_RUN" == "1" ]]; then
    echo "--- 将写入 $UNIT_PATH ---"
    render_unit
    exit 0
fi

# ── 自我搬家：unit 在跑时，本脚本也在 daemon 的 cgroup 里 ─────────────
# `systemctl restart` 会杀掉整个 cgroup，把脚本自己一起干掉（setsid 挡不住 cgroup
# kill）。所以先用 systemd-run 搬进一个 transient unit，再动手。
if [[ "${BIFROST_INSTALL_IN_TRANSIENT:-}" != "1" ]] \
   && systemctl is-active --quiet "$SERVICE" 2>/dev/null \
   && command -v systemd-run >/dev/null; then
    TN="bifrost-install-$(date +%s)"
    if systemd-run --collect --unit="$TN" --property=Type=oneshot \
            --setenv=BIFROST_INSTALL_IN_TRANSIENT=1 \
            /bin/bash "$0" "$@" >/dev/null 2>&1; then
        echo "已在 transient unit $TN 里执行（不在 daemon cgroup）"
        echo "输出: journalctl -u $TN"
        exit 0
    fi
    echo "WARN: systemd-run 失败，继续在当前进程里执行（restart 时可能被打断）"
fi

# 1) 停掉**手起**的 daemon，避免同一个 storage 上跑两个 daemon（任务会被执行两次）。
#    unit 已经在跑时跳过：那个进程是 systemd 的，杀掉会被 Restart= 立刻拉起来，
#    再叠加下面的 systemctl restart 等于白重启一次。
PATTERN="bifrost server --storage $STORAGE"
PATTERN_CFG="bifrost server --config $CONFIG"
if systemctl is-active --quiet "$SERVICE" 2>/dev/null; then
    echo "unit $SERVICE 已在跑，跳过 pkill（交给 systemctl restart）"
elif pgrep -f "$PATTERN" >/dev/null; then
    echo "停止手起的 daemon: pid $(pgrep -f "$PATTERN" | tr '\n' ' ')"
    pkill -TERM -f "$PATTERN"
    for _ in $(seq 1 40); do pgrep -f "$PATTERN" >/dev/null || break; sleep 0.5; done
    pgrep -f "$PATTERN" >/dev/null && { echo "仍在跑，SIGKILL"; pkill -KILL -f "$PATTERN"; sleep 1; }
fi

# 2) 渲染并安装 unit
render_unit > "$UNIT_PATH"
chmod 644 "$UNIT_PATH"
systemctl daemon-reload

# 3) 开机自启 + 现在启动（已 active 时相当于 restart）
systemctl enable "$SERVICE" >/dev/null 2>&1
systemctl restart "$SERVICE"

# 4) 验证：进程活着 + 心跳新鲜 + 任务能跑
START_EPOCH=$(date +%s)
HB="$STORAGE/heartbeat.json"
if ! systemctl is-active --quiet "$SERVICE"; then
    echo "FAIL: $SERVICE 未启动"
    journalctl -u "$SERVICE" -n 20 --no-pager
    exit 1
fi
for _ in $(seq 1 40); do
    [[ -f "$HB" ]] && [[ "$(stat -c %Y "$HB")" -ge "$START_EPOCH" ]] && break
    sleep 0.5
done
if [[ ! -f "$HB" ]] || [[ "$(stat -c %Y "$HB")" -lt "$START_EPOCH" ]]; then
    echo "FAIL: 心跳没刷新，daemon 可能卡在启动"
    journalctl -u "$SERVICE" -n 30 --no-pager
    exit 1
fi

echo "OK: $SERVICE active ($(systemctl show -p MainPID --value "$SERVICE")), heartbeat fresh"
echo "    type=$(systemctl show -p Type --value "$SERVICE") status=\"$(systemctl show -p StatusText --value "$SERVICE")\""
echo "    （StatusText 就是 daemon 上报的 STATUS=，能显示出来说明 READY=1 走通了）"
echo "    unit:    $UNIT_PATH"
echo "    logs:    journalctl -u $SERVICE -f"
echo "    restart: systemctl restart $SERVICE"
