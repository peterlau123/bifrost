#!/bin/bash
# deploy-h200.sh —— 在目标机（H200）上安装新的静态二进制并重启 daemon。
#
# 为什么需要它：H200 宿主没有 Rust 工具链，二进制只能在开发容器里编；容器里
# `cargo build --release` 的 gnu 产物依赖构建机的 glibc（实测 GLIBC_2.39），
# 而宿主是 2.35 —— 能起来，但只是因为在被调用到的那条路径上没有真正缺符号
# （动态链接器对 "weak version" 只警告不报错）。跑长任务不能靠这个运气。
# 所以部署政策是：只装 musl 静态产物。
#
# 重启方式：有 systemd unit（scripts/install-systemd.sh 装的）就 `systemctl restart`，
# 否则退回 pkill + setsid 手动起。前者才带 Restart=on-failure 和开机自启。
# 注意：有 unit 时本脚本会先用 systemd-run 把自己搬进一个 transient unit ——
# 否则脚本作为任务跑在 daemon 的 cgroup 里，`systemctl restart` 会把它一起杀掉
# （setsid 挡不住 cgroup kill，只能挡 daemon 自己的进程组清理）。
#
# 部署契约（任何一步失败都不碰正在跑的服务）：
#   ① ldd 显示 "statically linked"                = 静态产物（默认强制，--allow-dynamic 放行）
#   ② 候选二进制 `--version` 退出 0 且无 GLIBC 警告 = 产物与本机 glibc 匹配
#   ③ 候选二进制 `warmup --storage` 退出 0          = 环境可上线（存储/fstype/时钟/GPU/卡数）
#   → 备份 + 原子替换 → 重启（systemctl 或 pkill+setsid）
#   → 心跳必须比本次启动更新 → 跑一条真实任务 smoke，过了才算成功
#   → 任一步失败：回滚 .old 并重启旧二进制
#
# 用法:
#   scripts/deploy-h200.sh [--binary PATH] [--storage DIR] [--install PATH]
#                          [--skip-warmup] [--skip-smoke] [--allow-dynamic]
#                          [--no-systemd] [--log PATH]
# 退出码: 0 = 新 daemon 已就绪且跑通一条任务；1 = 失败（未改动或已回滚）。
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BINARY="$REPO/target/x86_64-unknown-linux-musl/release/bifrost"
STORAGE="/data/liuxin/bifrost"
INSTALL="/usr/local/bin/bifrost"
SERVICE="bifrost.service"
LOG="/tmp/bifrost-deploy.log"
DO_WARMUP=1
DO_SMOKE=1
ALLOW_DYNAMIC=0
USE_SYSTEMD=1

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)  BINARY="$2"; shift 2 ;;
        --storage) STORAGE="$2"; shift 2 ;;
        --install) INSTALL="$2"; shift 2 ;;
        --log)     LOG="$2"; shift 2 ;;
        --skip-warmup)   DO_WARMUP=0; shift ;;
        --skip-smoke)    DO_SMOKE=0; shift ;;
        --allow-dynamic) ALLOW_DYNAMIC=1; shift ;;
        --no-systemd)    USE_SYSTEMD=0; shift ;;
        -h|--help) sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

have_unit()   { [[ "$USE_SYSTEMD" == "1" ]] && systemctl cat "$SERVICE" >/dev/null 2>&1; }
unit_active() { systemctl is-active --quiet "$SERVICE" 2>/dev/null; }

# ── 自我搬家：有 systemd unit 时脱离 daemon 的 cgroup ────────────────
if have_unit && [[ "${BIFROST_DEPLOY_IN_TRANSIENT:-}" != "1" ]] && command -v systemd-run >/dev/null; then
    TN="bifrost-deploy-$(date +%s)"
    if systemd-run --collect --unit="$TN" --property=Type=oneshot \
            --setenv=BIFROST_DEPLOY_IN_TRANSIENT=1 \
            /bin/bash "$0" "$@" >/dev/null 2>&1; then
        echo "已在 transient unit $TN 里执行（不在 daemon cgroup，restart 杀不到它）"
        echo "部署日志: $LOG ; 启动输出: journalctl -u $TN"
        exit 0
    fi
    echo "WARN: systemd-run 失败，退回进程内执行（restart 时可能被打断）"
fi

exec >>"$LOG" 2>&1
ts() { date -Is; }
PATTERN="bifrost server --storage $STORAGE"
echo "=== $(ts) deploy start (binary=$BINARY install=$INSTALL storage=$STORAGE unit=$(have_unit && echo "$SERVICE" || echo none))"

fail_early() { echo "FAIL: $*"; echo "FAIL: 未改动 $INSTALL，旧 daemon 继续服务"; exit 1; }

[[ "$(id -u)" == "0" ]] || fail_early "必须以 root 运行（要写 $INSTALL）"
[[ -x "$BINARY" ]] || fail_early "$BINARY 不存在或不可执行"
echo "new sha:   $(sha256sum "$BINARY")"

# ── 门槛 ①：静态链接（部署政策）─────────────────────────────────────
LDD_OUT="$(ldd "$BINARY" 2>&1)"
if grep -qi "statically linked" <<<"$LDD_OUT"; then
    echo "link:      statically linked (musl) OK"
elif [[ "$ALLOW_DYNAMIC" == "1" ]]; then
    echo "WARN: 动态链接产物（--allow-dynamic 放行）: $(head -2 <<<"$LDD_OUT" | tr '\n' ' ')"
else
    echo "link:      $(head -3 <<<"$LDD_OUT" | tr '\n' ' ')"
    fail_early "候选二进制不是静态链接 —— gnu 动态产物，依赖构建机 glibc。请用 target/x86_64-unknown-linux-musl 的产物"
fi

# ── 门槛 ②：产物能在本机执行，且无 glibc 版本告警 ──────────────────
VER_OUT="$("$BINARY" --version 2>&1)"; VER_RC=$?
echo "$VER_OUT" | sed 's/^/           /'
[[ $VER_RC -eq 0 ]] || fail_early "候选二进制在本机起不来（架构不匹配？）"
if grep -q "GLIBC_[0-9.]*.*not found\|version \`GLIBC" <<<"$VER_OUT"; then
    fail_early "候选二进制要求的 glibc 比本机新（见上面 weak version 告警）—— 用 musl 静态产物"
fi

# ── 门槛 ③：环境可上线（候选二进制自己探，不启动服务）──────────────────
if [[ "$DO_WARMUP" == "1" ]]; then
    echo "--- warmup (candidate binary)"
    "$BINARY" warmup --storage "$STORAGE" || fail_early "warmup 有阻断项（见上）→ 换二进制解决不了，先修环境"
else
    echo "--- warmup SKIPPED (--skip-warmup)"
fi

# ── 安装：备份 + 原子替换（运行中的 daemon 持有旧 inode，rename 不触发 ETXTBSY）──
cp -f "$INSTALL" "$INSTALL.old" 2>/dev/null || echo "WARN: 无法备份 $INSTALL"
install -m 0755 "$BINARY" "$INSTALL.new" || fail_early "安装失败"
mv -f "$INSTALL.new" "$INSTALL" || fail_early "替换失败"
echo "installed: $(sha256sum "$INSTALL")"

# ── 重启 ────────────────────────────────────────────────────────────
START_EPOCH=$(date +%s)
start_manual() {
    pkill -TERM -f "$PATTERN"
    for _ in $(seq 1 40); do pgrep -f "$PATTERN" >/dev/null || break; sleep 0.5; done
    if pgrep -f "$PATTERN" >/dev/null; then
        echo "WARN: 旧 daemon 20s 后仍在，SIGKILL"
        pkill -KILL -f "$PATTERN"; sleep 1
    fi
    echo "old daemon stopped at $(ts)"
    cd "$STORAGE" || return 1
    setsid nohup "$1" server --storage "$STORAGE" >>/tmp/bifrost-daemon.log 2>&1 &
}

if have_unit; then
    echo "--- restart via systemctl (unit $SERVICE)"
    systemctl restart "$SERVICE" || echo "WARN: systemctl restart 返回非 0"
else
    echo "--- restart manually (no systemd unit)"
    start_manual "$INSTALL"
fi

wait_heartbeat() {  # 心跳必须比本次启动时间新
    local hb="$STORAGE/heartbeat.json"
    for _ in $(seq 1 40); do
        [[ -f "$hb" ]] && [[ "$(stat -c %Y "$hb")" -ge "$START_EPOCH" ]] && return 0
        sleep 0.5
    done
    return 1
}

# 真实任务 smoke：证明新 daemon 不只是进程活着，而是能收任务、能执行、能写结果
smoke() {
    local out tid res
    out=$("$INSTALL" client submit --storage "$STORAGE" --command 'echo bifrost-smoke-ok' 2>&1) || {
        echo "FAIL: client submit 失败: $out"; return 1; }
    tid=$(printf '%s\n' "$out" | awk '/Task ID/{print $3}')
    [[ -n "$tid" ]] || { echo "FAIL: 解析不到 task id: $out"; return 1; }
    res="$STORAGE/results/${tid}_result.json"
    for _ in $(seq 1 60); do
        if [[ -f "$res" ]]; then
            if grep -q '"Completed"' "$res" && grep -q 'bifrost-smoke-ok' "$res"; then
                echo "OK: smoke task $tid completed"; return 0
            fi
            echo "FAIL: smoke task $tid 未成功: $(head -c 400 "$res")"; return 1
        fi
        sleep 0.5
    done
    echo "FAIL: smoke task $tid 30s 内无结果"; return 1
}

rollback() {
    echo "ROLLBACK: 回滚到 $INSTALL.old"
    cp -f "$INSTALL.old" "$INSTALL" 2>/dev/null
    START_EPOCH=$(date +%s)
    if have_unit; then
        systemctl restart "$SERVICE"
    else
        start_manual "$INSTALL"
    fi
    sleep 3
    if { have_unit && unit_active; } || pgrep -f "$PATTERN" >/dev/null; then
        echo "ROLLED BACK: 旧二进制在跑"
    else
        echo "CRITICAL: 没有 daemon 在跑，需要手动启动"
    fi
    exit 1
}

if ! wait_heartbeat; then
    echo "FAIL: 新 daemon 30s 内没有写出新鲜心跳"
    have_unit && journalctl -u "$SERVICE" -n 20 --no-pager
    rollback
fi
if have_unit; then
    echo "OK: $SERVICE active (MainPID $(systemctl show -p MainPID --value "$SERVICE")), heartbeat fresh at $(ts)"
else
    echo "OK: daemon pid $(pgrep -f "$PATTERN" | tr '\n' ' ') heartbeat fresh at $(ts)"
fi

if [[ "$DO_SMOKE" == "1" ]]; then
    smoke || rollback
else
    echo "--- smoke SKIPPED (--skip-smoke)"
fi

echo "=== $(ts) deploy OK"
exit 0
