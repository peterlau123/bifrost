#!/bin/bash
# deploy-server.sh —— 在**目标机（离线/GPU 机）**上把 bifrost 装成 daemon：一条命令搞定。
#
# 做四件事：校验候选二进制 → 生成 server 侧配置 → 安装 + 重启（systemd 优先）→ 跑一条
# 真实任务验证。任一步失败都不留半成品：配置不覆盖、二进制可回滚。
#
# 用法:
#   scripts/deploy/deploy-server.sh --storage /data/liuxin/bifrost
#   scripts/deploy/deploy-server.sh --storage /data/liuxin/bifrost --binary dist/bifrost-linux-x86_64
#   scripts/deploy/deploy-server.sh --storage /srv/bf --no-systemd --gpus 0,1
#
# 必选:
#   --storage DIR         共享目录（client 与 daemon 必须指向同一物理目录）
# 可选:
#   --binary PATH         安装这个二进制（默认 scripts/build/build.sh --static 现编）
#   --config FILE         settings.json 路径（默认 /etc/bifrost/settings.json）
#   --prefix DIR          安装目录（默认 /usr/local/bin）
#   --service-name NAME   systemd unit 名（默认 bifrost）
#   --gpus LIST           写死 gpu_pool，如 0,1,2,3；不给 = 自动探测全部卡
#   --no-systemd          不起 systemd，用 setsid 手起（调试用）
#   --skip-build          不编译，直接用 --binary 或已有的 musl 产物
#   --skip-smoke          跳过最后的真实任务验证（不推荐）
#   --allow-dynamic       允许装动态链接产物（默认只收 musl 静态）
#   --force               覆盖已有 settings.json
#   --log FILE            部署日志（默认 /tmp/bifrost-deploy.log）
set -uo pipefail

ORIG_ARGS=("$@")   # 解析会 shift 掉全部参数，搬进 transient unit 时要把原参数传回去

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
STORAGE=""
BINARY=""
CONFIG="/etc/bifrost/settings.json"
PREFIX="/usr/local/bin"
SERVICE="bifrost"
GPUS=""
USE_SYSTEMD=1
SKIP_BUILD=0
SKIP_SMOKE=0
SKIP_WARMUP=0
ALLOW_DYNAMIC=0
FORCE=0
LOG="/tmp/bifrost-deploy.log"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage)      STORAGE="$2"; shift 2 ;;
        --binary)       BINARY="$2"; shift 2 ;;
        --config)       CONFIG="$2"; shift 2 ;;
        --prefix)       PREFIX="$2"; shift 2 ;;
        --service-name) SERVICE="$2"; shift 2 ;;
        --gpus)         GPUS="$2"; shift 2 ;;
        --no-systemd)   USE_SYSTEMD=0; shift ;;
        --skip-build)   SKIP_BUILD=1; shift ;;
        --skip-smoke)   SKIP_SMOKE=1; shift ;;
        --skip-warmup)  SKIP_WARMUP=1; shift ;;
        --allow-dynamic) ALLOW_DYNAMIC=1; shift ;;
        --force)        FORCE=1; shift ;;
        --log)          LOG="$2"; shift 2 ;;
        -h|--help)      sed -n '2,30p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

[[ -n "$STORAGE" ]] || { echo "必选参数缺失: --storage <共享目录>" >&2; exit 1; }
INSTALL="$PREFIX/bifrost"
PATTERN="bifrost server --storage $STORAGE"

exec >>"$LOG" 2>&1
ts() { date -Is; }
echo "=== $(ts) deploy-server start (storage=$STORAGE config=$CONFIG service=$SERVICE systemd=$USE_SYSTEMD)"

# 有 unit 时本脚本也在 daemon 的 cgroup 里，`systemctl restart` 会把自己一起杀掉；
# setsid 挡得住进程组清理，挡不住 cgroup kill，所以先搬进 transient unit。
have_unit() { [[ "$USE_SYSTEMD" == "1" ]] && systemctl cat "$SERVICE.service" >/dev/null 2>&1; }
echo "migration-check: unit=$(have_unit && echo yes || echo no) in_transient=${BIFROST_DEPLOY_IN_TRANSIENT:-0} systemd_run=$(command -v systemd-run || echo none)"
if have_unit && [[ "${BIFROST_DEPLOY_IN_TRANSIENT:-}" != "1" ]] && command -v systemd-run >/dev/null; then
    TN="bifrost-deploy-$(date +%s)"
    if systemd-run --collect --unit="$TN" --property=Type=oneshot \
            --setenv=BIFROST_DEPLOY_IN_TRANSIENT=1 \
            /bin/bash "$0" "${ORIG_ARGS[@]}" >/dev/null 2>&1; then
        echo "已在 transient unit $TN 内执行（不在 daemon cgroup，restart 杀不到它）"
        echo "日志: $LOG   启动输出: journalctl -u $TN"
        exit 0
    fi
    echo "WARN: systemd-run 失败，继续在当前进程执行（restart 时可能被打断）" >&2
fi

fail_early() { echo "FAIL: $*"; echo "FAIL: 未改动 $INSTALL，旧 daemon 继续服务"; exit 1; }
# 已经替换过二进制之后的失败：旧版本在 $INSTALL.old，可用 deploy-server.sh --binary 回滚
fail_late() {
    echo "FAIL: $*"
    echo "FAIL: 二进制已替换，旧版本在 $INSTALL.old（cp 回去再 systemctl restart 即可回滚）"
    exit 1
}

[[ "$(id -u)" == "0" ]] || fail_early "必须以 root 运行（要写 $PREFIX 和 $CONFIG）"

# ── 1. 取候选二进制 ────────────────────────────────────────────────
if [[ -z "$BINARY" ]]; then
    CAND="$REPO/target/x86_64-unknown-linux-musl/release/bifrost"
    if [[ "$SKIP_BUILD" == "1" ]]; then
        BINARY="$CAND"
    else
        echo "--- 编译 musl 静态产物"
        BINARY="$(bash "$REPO/scripts/build/build.sh" --static 2>/dev/null)" || fail_early "编译失败；宿主无工具链请用 --binary 指定产物"
    fi
fi
[[ -x "$BINARY" ]] || fail_early "$BINARY 不存在或不可执行"
echo "candidate: $BINARY"
echo "sha256:    $(sha256sum "$BINARY" | cut -d' ' -f1)"

# ── 2. 门槛：静态链接 / 能执行 / 环境可上线 ─────────────────────────
LDD_OUT="$(ldd "$BINARY" 2>&1)"
if grep -qi "statically linked" <<<"$LDD_OUT"; then
    echo "link:      statically linked (musl) OK"
elif [[ "$ALLOW_DYNAMIC" == "1" ]]; then
    echo "WARN: 动态链接产物（--allow-dynamic 放行）: $(head -2 <<<"$LDD_OUT" | tr '\n' ' ')"
else
    fail_early "候选二进制不是静态链接（gnu 动态产物，依赖构建机 glibc）；用 --static 编译，或显式给 --allow-dynamic"
fi

VER_OUT="$("$BINARY" --version 2>&1)"; VER_RC=$?
echo "           $VER_OUT"
[[ $VER_RC -eq 0 ]] || fail_early "候选二进制在本机起不来（架构不匹配？）"
if grep -q "version \`GLIBC" <<<"$VER_OUT"; then
    fail_early "候选二进制要求的 glibc 比本机新（见 weak version 告警）—— 用 musl 静态产物"
fi

if [[ "$SKIP_WARMUP" == "1" ]]; then
    echo "--- warmup 已跳过（--skip-warmup）"
else
    echo "--- warmup（用候选二进制自己探环境）"
    "$BINARY" warmup --storage "$STORAGE" || fail_early "warmup 有阻断项（见上）→ 换二进制解决不了，先修环境"
fi

# ── 3. 生成 server 侧配置 ──────────────────────────────────────────
echo "--- server init -> $CONFIG"
INIT_ARGS=(server init --storage "$STORAGE" --path "$CONFIG")
[[ "$FORCE" == "1" ]] && INIT_ARGS+=(--force)
if ! "$BINARY" "${INIT_ARGS[@]}"; then
    fail_early "生成配置失败"
fi

# --gpus 写进配置（不传则保持「自动探测全部卡」）
if [[ -n "$GPUS" ]]; then
    python3 - "$CONFIG" "$GPUS" <<'PY' || fail_early "写入 gpu_pool 失败"
import json, sys
path, ids = sys.argv[1], sys.argv[2]
d = json.load(open(path))
d.setdefault("daemon", {})["gpu_pool"] = [int(x) for x in ids.split(",") if x.strip() != ""]
json.dump(d, open(path, "w"), indent=2, ensure_ascii=False)
print(f"gpu_pool = {d['daemon']['gpu_pool']}")
PY
fi

# ── 4. 安装 + 重启 ────────────────────────────────────────────────
cp -f "$INSTALL" "$INSTALL.old" 2>/dev/null && echo "已备份旧二进制到 $INSTALL.old"
install -m 0755 "$BINARY" "$INSTALL.new" || fail_early "安装失败"
mv -f "$INSTALL.new" "$INSTALL" || fail_early "替换失败"
echo "installed: $(sha256sum "$INSTALL" | cut -d' ' -f1)"

START_EPOCH=$(date +%s)
if [[ "$USE_SYSTEMD" == "1" ]]; then
    echo "--- systemd：渲染 unit 并重启"
    BIFROST_INSTALL_IN_TRANSIENT=1 \
        bash "$REPO/scripts/deploy/install-systemd.sh" \
            --storage "$STORAGE" --install "$INSTALL" --config "$CONFIG" \
        || fail_late "install-systemd.sh 失败（看 journalctl -u $SERVICE）"
else
    echo "--- 手动重启（--no-systemd）"
    pkill -TERM -f "$PATTERN"
    for _ in $(seq 1 40); do pgrep -f "$PATTERN" >/dev/null || break; sleep 0.5; done
    pgrep -f "$PATTERN" >/dev/null && { pkill -KILL -f "$PATTERN"; sleep 1; }
    ( cd "$STORAGE" && setsid nohup "$INSTALL" server --storage "$STORAGE" >>/tmp/bifrost-daemon.log 2>&1 & )
fi

# ── 5. 验证：心跳必须比这次启动新 ──────────────────────────────────
HB="$STORAGE/heartbeat.json"
ok=0
for _ in $(seq 1 60); do
    if [[ -f "$HB" ]] && [[ "$(stat -c %Y "$HB")" -ge "$START_EPOCH" ]]; then ok=1; break; fi
    sleep 0.5
done
if [[ "$ok" != "1" ]]; then
    echo "FAIL: 30s 内没有新鲜心跳"
    [[ "$USE_SYSTEMD" == "1" ]] && journalctl -u "$SERVICE" -n 20 --no-pager
    if [[ -f "$INSTALL.old" ]]; then
        echo "ROLLBACK: 回滚到 $INSTALL.old 并重启"
        cp -f "$INSTALL.old" "$INSTALL"
        if [[ "$USE_SYSTEMD" == "1" ]]; then
            systemctl restart "$SERVICE"
        else
            ( cd "$STORAGE" && setsid nohup "$INSTALL" server --storage "$STORAGE" >>/tmp/bifrost-daemon.log 2>&1 & )
        fi
    fi
    exit 1
fi
echo "OK: 心跳新鲜（$(date -Is)）"

# ── 6. 真实任务 smoke ─────────────────────────────────────────────
if [[ "$SKIP_SMOKE" == "1" ]]; then
    echo "--- smoke 已跳过（--skip-smoke）"
else
    OUT=$("$INSTALL" client submit --storage "$STORAGE" --command 'echo bifrost-smoke-ok' 2>&1) || fail_late "client submit 失败: $OUT"
    TID=$(printf '%s\n' "$OUT" | awk '/Task ID/{print $3}')
    [[ -n "$TID" ]] || fail_late "解析不到 task id: $OUT"
    RES="$STORAGE/results/${TID}_result.json"
    for _ in $(seq 1 60); do
        if [[ -f "$RES" ]]; then
            if grep -q '"Completed"' "$RES" && grep -q 'bifrost-smoke-ok' "$RES"; then
                echo "OK: smoke task $TID completed"
                echo "=== $(ts) deploy-server OK"
                echo
                echo "下一步："
                echo "  client 机器上安装：scripts/deploy/deploy-client.sh --storage $STORAGE"
                echo "  systemd 状态：     systemctl status $SERVICE"
                echo "  日志：             journalctl -u $SERVICE -f"
                exit 0
            fi
            fail_late "smoke task $TID 未成功: $(head -c 300 "$RES")"
        fi
        sleep 0.5
    done
    fail_late "smoke task $TID 30s 内无结果"
fi
echo "=== $(ts) deploy-server OK（未跑 smoke）"
