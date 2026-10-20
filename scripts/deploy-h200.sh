#!/bin/bash
# deploy-h200.sh —— 在目标机（H200）上安装新的静态二进制并重启 daemon。
#
# 为什么需要它：H200 宿主没有 Rust 工具链，而开发容器里编出的 gnu 二进制要
# glibc >= 2.39，宿主是 2.35 起不来。所以部署链路固定为：
#   1) 容器里 cargo build --release --target x86_64-unknown-linux-musl --bin bifrost
#   2) 产物落在共享盘（宿主可见路径见 --binary）
#   3) 本脚本在宿主上以 root 安装并重启 daemon
#
# 本脚本会被 daemon 退出时的进程组清理波及，必须用 setsid 脱离再跑：
#   bifrost client submit --storage /data/liuxin/bifrost \
#     --command 'setsid nohup bash /data/liuxin/Ax/bifrost/scripts/deploy-h200.sh >/dev/null 2>&1 &'
#
# 用法:
#   scripts/deploy-h200.sh [--binary PATH] [--storage DIR] [--install PATH]
# 默认:
#   --binary  ${REPO}/target/x86_64-unknown-linux-musl/release/bifrost
#   --storage /data/liuxin/bifrost
#   --install /usr/local/bin/bifrost
# 退出码: 0 = 新 daemon 已就绪；1 = 失败（已尝试回滚到旧的备份二进制）。
set -uo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
BINARY="$REPO/target/x86_64-unknown-linux-musl/release/bifrost"
STORAGE="/data/liuxin/bifrost"
INSTALL="/usr/local/bin/bifrost"
LOG="/tmp/bifrost-deploy.log"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --binary)  BINARY="$2"; shift 2 ;;
        --storage) STORAGE="$2"; shift 2 ;;
        --install) INSTALL="$2"; shift 2 ;;
        --log)     LOG="$2"; shift 2 ;;
        -h|--help) sed -n '2,28p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

exec >>"$LOG" 2>&1
ts() { date -Is; }
PATTERN="bifrost server --storage $STORAGE"
echo "=== $(ts) deploy start (binary=$BINARY install=$INSTALL storage=$STORAGE)"

[[ "$(id -u)" == "0" ]] || { echo "FAIL: 必须以 root 运行（要写 $INSTALL）"; exit 1; }
[[ -x "$BINARY" ]] || { echo "FAIL: $BINARY 不存在或不可执行"; exit 1; }
"$BINARY" --version || { echo "FAIL: 新二进制在本机起不来（架构/glibc 不匹配？）"; exit 1; }
echo "new sha:   $(sha256sum "$BINARY")"

# 先备份再原子替换：运行中的 daemon 持有旧 inode，rename 不会触发 ETXTBSY。
cp -f "$INSTALL" "$INSTALL.old" 2>/dev/null || echo "WARN: 无法备份 $INSTALL"
install -m 0755 "$BINARY" "$INSTALL.new" || { echo "FAIL: 安装失败"; exit 1; }
mv -f "$INSTALL.new" "$INSTALL" || { echo "FAIL: 替换失败"; exit 1; }
echo "installed: $(sha256sum "$INSTALL")"

# 停止旧 daemon（SIGTERM 优雅退出，<1s；超时才 SIGKILL）
pkill -TERM -f "$PATTERN"
for _ in $(seq 1 40); do pgrep -f "$PATTERN" >/dev/null || break; sleep 0.5; done
if pgrep -f "$PATTERN" >/dev/null; then
    echo "WARN: 旧 daemon 20s 后仍在，SIGKILL"
    pkill -KILL -f "$PATTERN"; sleep 1
fi
echo "old daemon stopped at $(ts)"

start_daemon() {
    cd "$STORAGE" || return 1
    setsid nohup "$1" server --storage "$STORAGE" >>/tmp/bifrost-daemon.log 2>&1 &
}

start_daemon "$INSTALL"
for _ in $(seq 1 20); do pgrep -f "$PATTERN" >/dev/null && break; sleep 0.5; done

if pgrep -f "$PATTERN" >/dev/null; then
    echo "OK: daemon pid $(pgrep -f "$PATTERN" | tr '\n' ' ') at $(ts)"
    exit 0
fi

echo "FAIL: 新 daemon 没起来，回滚到 $INSTALL.old"
cp -f "$INSTALL.old" "$INSTALL"
start_daemon "$INSTALL"
sleep 3
if pgrep -f "$PATTERN" >/dev/null; then
    echo "ROLLED BACK: 旧二进制在跑"
else
    echo "CRITICAL: 没有 daemon 在跑，需要手动启动"
fi
exit 1
