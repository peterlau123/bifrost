#!/bin/bash
# bifrost 一键安装（Linux x86_64，client 与 server 同一个包）
#
# 三种角色：
#   client  只装二进制 + 客户端配置，并注册成 Hermes 的 MCP 工具（默认）
#   server  装二进制 + /etc 配置 + systemd 服务，开机自启（共享盘布局）
#   both    两者都装（同一台机器既跑 daemon 又当客户端）
#
# 用法:
#   ./install.sh                                              # 默认 client
#   sudo ./install.sh --role server --storage /data/liuxin/bifrost
#   sudo ./install.sh --role both   --storage /data/liuxin/bifrost
#   PREFIX=$HOME/.local/bin ./install.sh --role client --storage /data/liuxin/bifrost
#
# 选项:
#   --role client|server|both   角色（默认 client）
#   --storage DIR               共享盘目录（写进 settings.json；server 角色必填或沿用已有）
#   --prefix DIR                二进制安装目录（默认 /usr/local/bin）
#   --config FILE               配置文件路径（client 默认 ~/.bifrost/settings.json，
#                               server 默认 /etc/bifrost/settings.json）
#   --name NAME                 MCP 注册名（默认 bifrost，多集群并存时用得上）
#   --unit-dir DIR              systemd unit 目录（默认 /etc/systemd/system）
#   --force                     已存在的配置/服务允许覆盖
#   --no-hermes                 不注册 Hermes（client 侧）
#   --no-service                只写 unit 文件，不 enable/start（离线镜像/容器里用）
#   --no-systemctl              同 --no-service（别名）
#   --dry-run                   只打印将要做的事
#
# 装完自检:
#   client: hermes mcp test <name>
#   server: systemctl status bifrost && bifrost warmup --storage <DIR>
set -uo pipefail

ROLE="client"
PREFIX="${PREFIX:-/usr/local/bin}"
STORAGE=""
CONFIG=""
MCP_NAME="bifrost"
UNIT_DIR="/etc/systemd/system"
SERVICE="bifrost.service"
FORCE=0
NO_HERMES=0
NO_SERVICE=0
DRY=0

while [ $# -gt 0 ]; do
    case "$1" in
        --role)        ROLE="$2"; shift 2 ;;
        --storage)     STORAGE="$2"; shift 2 ;;
        --prefix)      PREFIX="$2"; shift 2 ;;
        --config)      CONFIG="$2"; shift 2 ;;
        --name)        MCP_NAME="$2"; shift 2 ;;
        --unit-dir)    UNIT_DIR="$2"; shift 2 ;;
        --settings)    CONFIG="$2"; shift 2 ;;   # 旧参数名，兼容
        --force)       FORCE=1; shift ;;
        --no-hermes)   NO_HERMES=1; shift ;;
        --no-service|--no-systemctl) NO_SERVICE=1; shift ;;
        --dry-run)     DRY=1; shift ;;
        -h|--help)     sed -n '2,29p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1（--help 看用法）" >&2; exit 1 ;;
    esac
done

case "$ROLE" in client|server|both) ;; *) echo "FAIL: --role 只能是 client|server|both" >&2; exit 1 ;; esac

HERE="$(cd "$(dirname "$0")" && pwd)"
SRC="$HERE/bifrost"
[ -x "$SRC" ] || { echo "FAIL: 包内找不到 bifrost 二进制（$SRC）" >&2; exit 1; }
say() { echo "[install:$ROLE] $*"; }
run() { if [ "$DRY" = "1" ]; then echo "DRY: $*"; else "$@"; fi; }

# ── 1. 二进制 ────────────────────────────────────────────────────
if [ "$DRY" = "1" ]; then
    say "会把 $SRC 装到 $PREFIX/bifrost"
else
    mkdir -p "$PREFIX" 2>/dev/null
    install -m 0755 "$SRC" "$PREFIX/bifrost" || {
        echo "FAIL: 无法写入 $PREFIX（需要 root 或换 --prefix）" >&2
        echo "  例: PREFIX=\$HOME/.local/bin $0 --role client ..." >&2
        exit 1
    }
fi
BIN="$PREFIX/bifrost"
say "二进制: $BIN"
[ "$DRY" = "1" ] || "$BIN" --version >/dev/null || { echo "FAIL: 二进制无法执行（架构/libc 不匹配？）" >&2; exit 1; }

# ── 2. settings.json ────────────────────────────────────────────
write_settings() {   # $1=目标路径
    local target="$1"
    mkdir -p "$(dirname "$target")"
    if [ -f "$target" ] && [ "$FORCE" != "1" ] && [ -z "$STORAGE" ]; then
        say "配置已存在，保持不变: $target（--force 可重写）"
        return 0
    fi
    if [ -f "$target" ] && [ "$FORCE" != "1" ] && [ -n "$STORAGE" ]; then
        say "配置已存在，保持不变: $target（改共享盘请加 --force）"
        return 0
    fi
    cat > "$target" <<EOF
{
  "shared_storage": "${STORAGE:-/data/liuxin/bifrost}",
  "transport": "shared",
  "client": { "poll_interval": "2s", "heartbeat_timeout": "3m", "wait_timeout": "12h" },
  "daemon": { "scan_mode": "auto" }
}
EOF
    say "配置: $target → shared_storage=${STORAGE:-/data/liuxin/bifrost}"
}

# ── 3. server：systemd unit ─────────────────────────────────────
render_unit() {      # $1=unit 路径
    local cfg="$1"
    cat <<EOF
[Unit]
Description=Bifrost command execution daemon (shared storage: ${STORAGE:-$cfg})
Documentation=https://github.com/peterlau123/bifrost
After=network.target

[Service]
Type=notify
NotifyAccess=main
TimeoutStartSec=60
User=root
WorkingDirectory=${STORAGE:-/var/lib/bifrost}
ExecStart=$BIN server --config $cfg --systemd
ExecStartPost=-$BIN warmup --config $cfg
KillSignal=SIGTERM
KillMode=mixed
TimeoutStopSec=30
Restart=on-failure
RestartSec=5
LimitNOFILE=65536
TasksMax=4096
OOMScoreAdjust=-500
StandardOutput=journal
StandardError=journal

[Install]
WantedBy=multi-user.target
EOF
}

install_server() {
    local cfg="${CONFIG:-/etc/bifrost/settings.json}"
    if [ -z "$STORAGE" ] && [ ! -f "$cfg" ]; then
        echo "FAIL: server 角色需要 --storage <共享盘目录>（或已存在的 $cfg）" >&2
        exit 1
    fi
    # 只有落到系统路径（/etc）才强制 root；装到自定义位置（容器/离线镜像/测试）不拦
    need_root=0
    case "$cfg" in /etc/*) need_root=1 ;; esac
    case "$UNIT_DIR" in /etc/*) need_root=1 ;; esac
    if [ "$DRY" != "1" ] && [ "$need_root" = "1" ] && [ "$(id -u)" != "0" ]; then
        echo "FAIL: 写 $cfg / $UNIT_DIR 需要 root（sudo）" >&2
        echo "  没有 root 时: --config <可写路径> --unit-dir <可写路径> --no-service" >&2
        exit 1
    fi
    write_settings "$cfg"
    local unit="$UNIT_DIR/$SERVICE"
    say "写 unit: $unit"
    [ "$DRY" = "1" ] || mkdir -p "$UNIT_DIR"
    if [ "$DRY" = "1" ]; then
        echo "--- unit 内容 ---"; render_unit "$cfg"; echo "--- end ---"
    else
        render_unit "$cfg" > "$unit"
        chmod 644 "$unit"
    fi
    if [ "$NO_SERVICE" = "1" ]; then
        say "已跳过 enable/start（--no-service）；手工: systemctl daemon-reload && systemctl enable --now $SERVICE"
        return 0
    fi
    if ! command -v systemctl >/dev/null; then
        say "没有 systemctl，跳过服务安装（不是 systemd 系统）"
        return 0
    fi
    run systemctl daemon-reload
    run systemctl enable "$SERVICE"
    run systemctl restart "$SERVICE"
    if [ "$DRY" != "1" ]; then
        sleep 3
        if systemctl is-active --quiet "$SERVICE"; then
            say "✓ 服务已启动: $(systemctl is-active "$SERVICE")"
        else
            echo "FAIL: $SERVICE 未起来，看: journalctl -u $SERVICE -n 40" >&2
            return 1
        fi
        [ -n "$STORAGE" ] && { "$BIN" warmup --storage "$STORAGE" >/dev/null 2>&1 && say "✓ warmup 通过" || say "! warmup 未通过（不影响 daemon 运行，手工跑 $BIN warmup --storage $STORAGE 看原因）"; }
    fi
}

# ── 4. client：MCP 自检 + 注册 ──────────────────────────────────
install_client() {
    local cfg="${CONFIG:-$HOME/.bifrost/settings.json}"
    write_settings "$cfg"

    if [ "$DRY" = "1" ]; then
        say "会做 MCP stdio 握手自检，并注册成 Hermes MCP: $MCP_NAME"
        return 0
    fi

    local reply
    reply=$(printf '%s\n' \
      '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"install.sh","version":"0"}}}' \
      | BIFROST_CONFIG="$cfg" timeout 15 "$BIN" mcp-serve 2>/dev/null | head -1)
    case "$reply" in
        *protocolVersion*) say "✓ MCP 握手正常" ;;
        *) say "! MCP 握手无应答（手工排查: BIFROST_CONFIG=$cfg $BIN mcp-serve）" ;;
    esac

    if [ "$NO_HERMES" = "1" ]; then
        say "已跳过 Hermes 注册（--no-hermes）"
        echo "  手工注册: hermes mcp add $MCP_NAME --env BIFROST_CONFIG=$cfg --command $BIN --args mcp-serve"
        return 0
    fi

    local hermes_bin
    hermes_bin="$(command -v hermes 2>/dev/null || true)"
    if [ -z "$hermes_bin" ]; then
        for c in /opt/hermes/bin/hermes /opt/hermes/libexec/hermes "$HOME/.local/bin/hermes"; do
            [ -x "$c" ] && { hermes_bin="$c"; break; }
        done
    fi
    if [ -z "$hermes_bin" ]; then
        say "未找到 hermes，跳过注册。装好 Hermes 后执行:"
        echo "  hermes mcp add $MCP_NAME --env BIFROST_CONFIG=$cfg --command $BIN --args mcp-serve"
        return 0
    fi
    say "注册到 Hermes: $hermes_bin"
    printf 'y\n' | "$hermes_bin" mcp add "$MCP_NAME" \
        --connect-timeout 30 \
        --env BIFROST_CONFIG="$cfg" \
        --command "$BIN" \
        --args mcp-serve
    say "验证: hermes mcp list && hermes mcp test $MCP_NAME"
}

case "$ROLE" in
    server) install_server ;;
    client) install_client ;;
    both)   install_server && install_client ;;
esac

rc=$?
echo
if [ $rc -eq 0 ]; then
    say "完成（role=$ROLE）"
    case "$ROLE" in
        server|both) echo "  服务: systemctl status $SERVICE   |   日志: journalctl -u $SERVICE -f" ;;
    esac
    case "$ROLE" in
        client|both) echo "  客户端: hermes mcp test $MCP_NAME" ;;
    esac
    echo "  任务示例: bifrost client submit --storage ${STORAGE:-<DIR>} --command 'nvidia-smi'"
fi
exit $rc
