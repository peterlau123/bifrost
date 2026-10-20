#!/bin/bash
# quickstart.sh —— 一份可跑通的 bifrost 用法样例（client 侧），照抄改改就能用。
#
# 跑完会依次演示：daemon 存活 → 单任务 → 结果字段 → GPU 分配 → 并发任务 → artifact 收集，
# 最后给 PASS/FAIL 汇总。任何一步失败都会打出原始输出，方便判断是环境问题还是用法问题。
#
# 用法:
#   scripts/examples/quickstart.sh --storage /data/liuxin/bifrost
#   scripts/examples/quickstart.sh --storage /data/liuxin/bifrost --binary ~/bin/bifrost
set -uo pipefail

STORAGE=""
BINARY="${BIFROST_BIN:-}"
PARALLEL=3

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage)  STORAGE="$2"; shift 2 ;;
        --binary)   BINARY="$2"; shift 2 ;;
        --parallel) PARALLEL="$2"; shift 2 ;;
        -h|--help)  sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done
[[ -n "$STORAGE" ]] || { echo "必选参数缺失: --storage <共享目录>" >&2; exit 1; }

# 解析二进制：--binary / $BIFROST_BIN > PATH 里的 bifrost > 仓库内产物。
# 解析不到就直接退出——否则后面每个 submit 都会静默返回空 task id，白等几十秒。
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
if [[ -z "$BINARY" ]]; then
    if command -v bifrost >/dev/null 2>&1; then
        BINARY="$(command -v bifrost)"
    elif [[ -x "$REPO/target/release/bifrost" ]]; then
        BINARY="$REPO/target/release/bifrost"
    elif [[ -x "$REPO/dist/bifrost-linux-x86_64" ]]; then
        BINARY="$REPO/dist/bifrost-linux-x86_64"
    else
        echo "找不到 bifrost 二进制：装一个（scripts/deploy/deploy-client.sh）或用 --binary 指定" >&2
        exit 1
    fi
fi
[[ -x "$BINARY" ]] || { echo "$BINARY 不可执行" >&2; exit 1; }
echo "binary: $BINARY"

pass=0; fail=0
chk() { if [[ "$2" == "0" ]]; then pass=$((pass+1)); printf '  \033[32mPASS\033[0m %s\n' "$1";
        else fail=$((fail+1)); printf '  \033[31mFAIL\033[0m %s\n' "$1"; fi; }

c() { "$BINARY" client "$@" --storage "$STORAGE"; }
submit() {
    local out tid
    out=$(c submit "$@" 2>&1) || { echo "提交失败: $out" >&2; return 1; }
    tid=$(printf '%s\n' "$out" | awk '/Task ID/{print $3}')
    [[ -n "$tid" ]] || { echo "拿不到 task id: $out" >&2; return 1; }
    printf '%s' "$tid"
}
status_of() { c status "${1:-none}" 2>/dev/null | awk '/Status:/{print $2}'; }
result_file() { echo "$STORAGE/results/${1}_result.json"; }
field() { python3 -c "
import json,sys
try: print(json.load(open('$(result_file "$1")'))$2)
except Exception as e: print('')
" 2>/dev/null; }

wait_done() {
    local tid="$1" max="${2:-90}" s
    for _ in $(seq 1 "$max"); do
        s=$(status_of "$tid")
        [[ -n "$s" && "$s" != "Pending" && "$s" != "Running" ]] && { echo "$s"; return; }
        sleep 1
    done
    echo "TIMEOUT-WAIT"
}

echo "== 0. daemon 存活"
HB="$STORAGE/heartbeat.json"
if [[ -f "$HB" ]]; then
    age=$(( $(date +%s) - $(stat -c %Y "$HB") ))
    echo "     heartbeat ${age}s ago"
    [[ "$age" -lt 180 ]]; chk "daemon 在线（心跳 ${age}s）" $?
else
    echo "     找不到 $HB"
    chk "heartbeat.json 存在" 1
    echo; echo "daemon 没起来，后面全都会失败；先跑 scripts/deploy/deploy-server.sh"
    exit 1
fi

echo
echo "== 1. 提交一个最简单的任务"
T=$(submit --command 'hostname; uname -r; nproc') || exit 1
S=$(wait_done "$T" 60)
echo "     status=$S"
field "$T" "['output']['stdout']" | sed 's/^/     /'
[[ "$S" == "Completed" ]]; chk "单任务执行完成" $?

echo
echo "== 2. 结果的字段长什么样"
python3 - "$(result_file "$T")" <<'PY'
import json, sys
d = json.load(open(sys.argv[1]))
for k in ("task_id", "status", "duration_ms", "retries_used", "artifacts", "error_message"):
    print(f"     {k:14} {d.get(k)}")
print(f"     {'exit_code':14} {d['output'].get('exit_code')}")
PY
[[ -n "$(field "$T" "['task_id']")" ]]; chk "结果载荷可解析（task_id/status/duration/output）" $?

echo
echo "== 3. 需要 shell 语法时显式包 sh -c（否则 && 和 | 不会生效）"
T2=$(submit --command "sh -c 'echo a && echo b | tr a-z A-Z'") || exit 1
S2=$(wait_done "$T2" 60)
OUT2=$(field "$T2" "['output']['stdout']")
echo "     $OUT2" | sed 's/^/     /'
[[ "$(printf '%s' "$OUT2" | tr -d '\n')" == "ab" ]] || [[ "$(printf '%s' "$OUT2" | tr -d '\n')" == "aB" ]]
chk "sh -c 包裹后管道/&& 生效" $?

echo
echo "== 4. GPU 分配（daemon 有卡时每个任务独占一张）"
T3=$(submit --command 'nvidia-smi --query-gpu=index,name --format=csv,noheader 2>/dev/null | head -1; echo "cuda=$CUDA_VISIBLE_DEVICES"') || exit 1
S3=$(wait_done "$T3" 60)
GPU_OUT=$(field "$T3" "['output']['stdout']")
echo "$GPU_OUT" | sed 's/^/     /'
if printf '%s' "$GPU_OUT" | grep -q 'cuda='; then
    chk "任务拿到 CUDA_VISIBLE_DEVICES" 0
else
    chk "任务拿到 CUDA_VISIBLE_DEVICES（CPU-only 机器可忽略）" 1
fi

echo
echo "== 5. 并发 $PARALLEL 个任务：各占不同卡"
TIDS=()
for _ in $(seq 1 "$PARALLEL"); do
    t=$(submit --command 'sleep 3; echo "cuda=$CUDA_VISIBLE_DEVICES"') || exit 1
    TIDS+=("$t")
done
sleep 2
ASSIGNED=""
for t in "${TIDS[@]}"; do
    wait_done "$t" 90 >/dev/null
    v=$(field "$t" "['output']['stdout']" | tr -d '\n')
    ASSIGNED+="${v#cuda=} "
done
echo "     分配结果: $ASSIGNED"
UNIQ=$(printf '%s' "$ASSIGNED" | tr ' ' '\n' | sed '/^$/d' | sort -u | wc -l)
[[ "$UNIQ" -ge 1 ]]; chk "并发任务各自拿到卡（$UNIQ 个不同 id）" $?

echo
echo "== 6. artifact 收集（任务产物会复制到 $STORAGE/artifacts/）"
JOB=$(mktemp /tmp/bifrost-quickstart.XXXXXX.yaml)
cat > "$JOB" <<EOF
name: quickstart-artifact
tasks:
  - name: produce
    command: echo '{"score":1}' > /tmp/quickstart_artifact.json
    timeout: 30
    artifacts: [/tmp/quickstart_artifact.json]
EOF
c submit --job "$JOB" >/dev/null 2>&1
rm -f "$JOB"
sleep 2
ls "$STORAGE"/artifacts/*quickstart_artifact.json >/dev/null 2>&1; chk "artifact 已复制到共享目录" $?

echo
echo "================ RESULT ================"
echo "  passed: $pass    failed: $fail"
[[ $fail -gt 0 ]] && exit 1
echo "  all checks passed"
