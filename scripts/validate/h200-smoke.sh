#!/bin/bash
# h200-smoke.sh —— 在目标机（H200）上一条命令跑完 bifrost 的关键行为验证。
#
# 用法（在 H200 上）:
#   scripts/h200-smoke.sh                     # 临时目录做共享存储
#   scripts/h200-smoke.sh --storage /data/liuxin/bifrost-smoke
#   scripts/h200-smoke.sh --binary /usr/local/bin/bifrost   # 用现成二进制，不编译
#
# 检查项：warmup、shell 语法、显式 timeout、cancel、GPU 池注入、artifacts、
#         默认不重试 / 显式重试、9MB 输出（结果限长 + 日志完整）、scan 心跳。
# 退出码 0 = 全过。
set -uo pipefail

ROOT_DIR=$(cd "$(dirname "$0")/../.." && pwd)
STORAGE=""
BINARY=""

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage) STORAGE="$2"; shift 2 ;;
        --binary)  BINARY="$2"; shift 2 ;;
        -h|--help) sed -n '2,15p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done

pass=0; fail=0; failed_names=()
check() { # check <name> <0|1>
    if [[ "$2" == "0" ]]; then pass=$((pass+1)); printf "  \033[32mPASS\033[0m %s\n" "$1"
    else fail=$((fail+1)); failed_names+=("$1"); printf "  \033[31mFAIL\033[0m %s\n" "$1"; fi
}

if [[ -z "$BINARY" ]]; then
    echo "== 构建 =="
    BINARY=$(bash "$ROOT_DIR/scripts/find-binary.sh" release) || { echo "构建失败"; exit 1; }
fi
echo "binary: $BINARY"

WORK=$(mktemp -d "${TMPDIR:-/tmp}/bifrost-smoke.XXXXXX")
SHARED="${STORAGE:-$WORK/shared}"
mkdir -p "$SHARED" "$WORK/run"
CFG="$WORK/settings.json"

GPUS=$(nvidia-smi -L 2>/dev/null | grep -c '^GPU ' || true)
GPU_POOL="[]"
if [[ "${GPUS:-0}" -gt 0 ]]; then
    GPU_POOL=$(python3 -c "import json,sys;print(json.dumps(list(range(int(sys.argv[1])))))" "$GPUS")
fi

cat > "$CFG" <<JSON
{
  "shared_storage": "$SHARED",
  "client": { "poll_interval": "300ms" },
  "daemon": {
    "task_timeout": "60s",
    "max_concurrent": 8,
    "use_shell": true,
    "scan_interval": "200ms",
    "gpu_pool": $GPU_POOL,
    "working_dir": "$WORK/run"
  }
}
JSON
export BIFROST_CONFIG="$CFG"

echo "== warmup =="
"$BINARY" warmup || true

echo "== 启动 daemon =="
"$BINARY" server > "$WORK/daemon.log" 2>&1 &
DPID=$!
sleep 2
if ! grep -q "Server ready" "$WORK/daemon.log"; then
    echo "daemon 启动失败:"; cat "$WORK/daemon.log"; kill $DPID 2>/dev/null; exit 1
fi
grep -E "clock:|Server ready" "$WORK/daemon.log" | sed 's/^/  /'

submit() { "$BINARY" client submit "$@" 2>&1 | awk '/Task ID/{print $3}'; }
status_of() { "$BINARY" client status "${1:-none}" 2>/dev/null | awk '/Status:/{print $2}'; }
result_field() { python3 -c "import json;d=json.load(open('$SHARED/results/$1_result.json'));print(d$2)" 2>/dev/null; }

echo "== 1. shell 语法 =="
T=$(submit --command 'X=smoke; echo "$X" | tr a-z A-Z')
sleep 2
[[ "$(result_field "$T" "['output']['stdout'].strip()")" == "SMOKE" ]]; check "shell pipe+var" $?

echo "== 2. 显式 timeout 覆盖 daemon 默认 =="
T=$(submit --command 'sleep 5' --timeout 2)
sleep 4
[[ "$(status_of "$T")" == "Timeout" ]]; check "per-task timeout" $?

echo "== 3. cancel 运行中任务 =="
T=$(submit --command 'sleep 60' --timeout 120)
sleep 1
"$BINARY" client cancel "$T" >/dev/null 2>&1
sleep 2
[[ "$(status_of "$T")" == "Cancelled" ]]; check "cancel running task" $?

echo "== 4. GPU 池 =="
if [[ "$GPUS" -gt 0 ]]; then
    T=$(submit --command 'echo "gpu=$CUDA_VISIBLE_DEVICES"')
    sleep 2
    OUT=$(result_field "$T" "['output']['stdout'].strip()")
    [[ "$OUT" == gpu=* && "$OUT" != "gpu=" ]]; check "gpu_pool injects CUDA_VISIBLE_DEVICES ($OUT)" $?
else
    echo "  SKIP 无 GPU"
fi

echo "== 5. artifacts =="
cat > "$WORK/job.yaml" <<EOF
name: smoke-artifacts
tasks:
  - name: produce
    command: echo '{"score":1}' > out.json
    timeout: 30
    working_dir: $WORK/run
    artifacts: [out.json]
EOF
"$BINARY" client submit --job "$WORK/job.yaml" >/dev/null 2>&1
sleep 2
ls "$SHARED"/artifacts/*_out.json >/dev/null 2>&1; check "artifact collected" $?

echo "== 6. 默认不重试 =="
MARK="$WORK/attempts"
T=$(submit --command "echo x >> $MARK; exit 3")
sleep 2
[[ "$(wc -l < "$MARK" 2>/dev/null || echo 0)" == "1" ]]; check "no retry by default" $?

echo "== 7. --retries 2 =="
MARK2="$WORK/attempts2"
T=$(submit --command "n=\$(cat $MARK2 2>/dev/null || echo 0); n=\$((n+1)); echo \$n > $MARK2; test \$n -ge 3" --retries 2)
sleep 4
[[ "$(cat "$MARK2" 2>/dev/null)" == "3" ]]; check "--retries 2 → 3 attempts" $?

echo "== 8. 9MB 输出（结果 64MB 上限内不截断，日志完整） =="
T=$(submit --command "python3 -u -c \"import sys; sys.stdout.write('A'*9000000); sys.stdout.write('TAIL')\"" --timeout 120)
for _ in $(seq 1 30); do [[ "$(status_of "$T")" != "Pending" && "$(status_of "$T")" != "Running" ]] && break; sleep 1; done
PAYLOAD=$(result_field "$T" "['output']['stdout'].__len__()")
LOG=$(stat -c%s "$SHARED/logs/$T/stdout.log" 2>/dev/null || echo 0)
[[ "$PAYLOAD" == "9000004" && "$LOG" == "9000004" ]]; check "9MB payload+log intact (payload=$PAYLOAD log=$LOG)" $?

echo "== 9. scan 心跳 =="
sleep 2
[[ -f "$SHARED/scan.json" ]]; check "scan.json beacon" $?

kill $DPID 2>/dev/null
wait $DPID 2>/dev/null

echo
echo "================ RESULT ================"
echo "  passed: $pass    failed: $fail"
if [[ $fail -gt 0 ]]; then
    printf "  failed: %s\n" "${failed_names[*]}"
    echo "  daemon log: $WORK/daemon.log"
    echo "  shared dir: $SHARED"
    exit 1
fi
[[ -z "$STORAGE" ]] && rm -rf "$WORK"
echo "  all checks passed"
