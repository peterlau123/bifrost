#!/bin/bash
# deploy-h200.sh —— 在目标机（H200）上安装新的静态二进制并重启 daemon。
#
# 为什么需要它：H200 宿主没有 Rust 工具链，二进制只能在开发容器里编；容器里
# `cargo build --release` 的 gnu 产物依赖构建机的 glibc（实测 GLIBC_2.39），
# 而宿主是 2.35 —— 能起来，但只是因为在被调用到的那条路径上没有真正缺符号
# （动态链接器对 "weak version" 只警告不报错）。跑长任务不能靠这个运气。
# 所以部署政策是：只装 musl 静态产物。
#
# 链路：
#   1) 容器里 cargo build --release --target x86_64-unknown-linux-musl --bin bifrost
#   2) 产物落在共享盘（宿主可见路径见 --binary）
#   3) 本脚本在宿主上以 root 安装、自检、重启
#
# 部署契约（任何一步失败都不碰正在跑的 daemon）：
#   ① ldd 显示 "statically linked"                 = 静态产物（默认强制，--allow-dynamic 放行）
#   ② 候选二进制 `--version` 退出 0 且无 GLIBC 警告   = 产物与本机 glibc 匹配
#   ③ 候选二进制 `warmup --storage` 退出 0           = 环境可上线（存储可写、缓存型 fs、时钟不冻结、GPU 可见）
#   → 备份 + 原子替换 → SIGTERM 旧 daemon → setsid 起新 daemon
#   → 心跳必须比本次启动更新 → 跑一条真实任务 smoke，过了才算成功
#   → 任一步失败：回滚 .old 并重启旧二进制
#
# 本脚本会被 daemon 退出时的进程组清理波及，必须用 setsid 脱离再跑：
#   bifrost client submit --storage /data/liuxin/bifrost \
#     --command 'setsid nohup bash /data/liuxin/Ax/bifrost/scripts/deploy-h200.sh >/dev/null 2>&1 &'
#
# 用法:
#   scripts/deploy-h200.sh [--binary PATH] [--storage DIR] [--install PATH]
#                          [--skip-warmup] [--skip-smoke] [--allow-dynamic] [--log PATH]
# 退出码: 0 = 新 daemon 已就绪且跑通一条任务；1 = 失败（未改动或已回滚）。
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BINARY="$REPO/target/x86_64-unknown-linux-musl/release/bifrost"
STORAGE="/data/liuxin/bifrost"
INSTALL="/usr/local/bin/bifrost"
LOG="/tmp/bifrost-deploy.log"
DO_WARMUP=1
DO_SMOKE=1
ALLOW_DYNAMIC=0

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)  BINARY="$2"; shift 2 ;;
        --storage) STORAGE="$2"; shift 2 ;;
        --install) INSTALL="$2"; shift 2 ;;
        --log)     LOG="$2"; shift 2 ;;
        --skip-warmup)   DO_WARMUP=0; shift ;;
        --skip-smoke)    DO_SMOKE=0; shift ;;
        --allow-dynamic) ALLOW_DYNAMIC=1; shift ;;
        -h|--help) sed -n '2,35p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

exec >>"$LOG" 2>&1
ts() { date -Is; }
PATTERN="bifrost server --storage $STORAGE"
echo "=== $(ts) deploy start (binary=$BINARY install=$INSTALL storage=$STORAGE)"

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
    fail_early "候选二进制不是静态链接 —— 这是 gnu 动态产物，依赖构建机 glibc（宿主可能更旧）。请用 target/x86_64-unknown-linux-musl 的产物"
fi

# ── 门槛 ②：产物能在本机执行，且无 glibc 版本告警 ──────────────────
VER_OUT="$("$BINARY" --version 2>&1)"; VER_RC=$?
echo "$VER_OUT" | sed 's/^/           /'
[[ $VER_RC -eq 0 ]] || fail_early "候选二进制在本机起不来（架构不匹配？）"
if grep -q "GLIBC_[0-9.]*.*not found\|version \`GLIBC" <<<"$VER_OUT"; then
    fail_early "候选二进制要求的 glibc 比本机新（见上面的 weak version 告警）—— 用 musl 静态产物，别赌运行时不会走到那条路径"
fi

# ── 门槛 ③：环境可上线（用候选二进制自己探，不启动服务）──────────────
if [[ "$DO_WARMUP" == "1" ]]; then
    echo "--- warmup (candidate binary)"
    if ! "$BINARY" warmup --storage "$STORAGE"; then
        fail_early "warmup 有阻断项（见上）→ 这里换二进制解决不了，先修环境"
    fi
else
    echo "--- warmup SKIPPED (--skip-warmup)"
fi

# ── 安装：备份 + 原子替换（运行中的 daemon 持有旧 inode，rename 不触发 ETXTBSY）──
cp -f "$INSTALL" "$INSTALL.old" 2>/dev/null || echo "WARN: 无法备份 $INSTALL"
install -m 0755 "$BINARY" "$INSTALL.new" || fail_early "安装失败"
mv -f "$INSTALL.new" "$INSTALL" || fail_early "替换失败"
echo "installed: $(sha256sum "$INSTALL")"

# ── 重启：SIGTERM 优雅退出（<1s），超时才 SIGKILL ────────────────────
pkill -TERM -f "$PATTERN"
for _ in $(seq 1 40); do pgrep -f "$PATTERN" >/dev/null || break; sleep 0.5; done
if pgrep -f "$PATTERN" >/dev/null; then
    echo "WARN: 旧 daemon 20s 后仍在，SIGKILL"
    pkill -KILL -f "$PATTERN"; sleep 1
fi
echo "old daemon stopped at $(ts)"

START_EPOCH=$(date +%s)
start_daemon() {
    cd "$STORAGE" || return 1
    setsid nohup "$1" server --storage "$STORAGE" >>/tmp/bifrost-daemon.log 2>&1 &
}

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
    pkill -TERM -f "$PATTERN" 2>/dev/null
    for _ in $(seq 1 20); do pgrep -f "$PATTERN" >/dev/null || break; sleep 0.5; done
    START_EPOCH=$(date +%s)
    start_daemon "$INSTALL"
    sleep 3
    if pgrep -f "$PATTERN" >/dev/null; then
        echo "ROLLED BACK: 旧二进制在跑 pid $(pgrep -f "$PATTERN" | tr '\n' ' ')"
    else
        echo "CRITICAL: 没有 daemon 在跑，需要手动启动"
    fi
    exit 1
}

start_daemon "$INSTALL"
if ! wait_heartbeat; then
    echo "FAIL: 新 daemon 30s 内没有写出新鲜心跳"
    rollback
fi
echo "OK: daemon pid $(pgrep -f "$PATTERN" | tr '\n' ' ') heartbeat fresh at $(ts)"

if [[ "$DO_SMOKE" == "1" ]]; then
    smoke || rollback
else
    echo "--- smoke SKIPPED (--skip-smoke)"
fi

echo "=== $(ts) deploy OK"
exit 0
