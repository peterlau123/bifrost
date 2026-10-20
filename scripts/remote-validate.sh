#!/bin/bash
# remote-validate.sh —— 对**已经在运行**的 daemon 做端到端验证（client 侧执行）。
#
# 与 h200-smoke.sh 的区别：那个会自己起 daemon；这个只驱动已存在的 daemon，
# 用来做"另一台机器远程提交"的真机验证。
#
# 用法: scripts/remote-validate.sh --storage /data/liuxin/bifrost [--binary /usr/local/bin/bifrost]
set -uo pipefail

STORAGE=""
BINARY="${BIFROST_BIN:-bifrost}"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --storage) STORAGE="$2"; shift 2 ;;
        --binary)  BINARY="$2"; shift 2 ;;
        -h|--help) sed -n '2,10p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) echo "未知参数: $1" >&2; exit 1 ;;
    esac
done
[[ -z "$STORAGE" ]] && { echo "必须给 --storage" >&2; exit 1; }

pass=0; fail=0; failed=()
chk() { if [[ "$2" == "0" ]]; then pass=$((pass+1)); printf "  \033[32mPASS\033[0m %s\n" "$1";
        else fail=$((fail+1)); failed+=("$1"); printf "  \033[31mFAIL\033[0m %s\n" "$1"; fi }

c() { "$BINARY" client "$@" --storage "$STORAGE"; }
submit() { c submit "$@" 2>&1 | awk '/Task ID/{print $3}'; }
status_of() { c status "${1:-none}" 2>/dev/null | awk '/Status:/{print $2}'; }
wait_done() { # tid, max_secs
    for _ in $(seq 1 "${2:-60}"); do
        s=$(status_of "$1")
        [[ "$s" != "Pending" && "$s" != "Running" && -n "$s" ]] && { echo "$s"; return; }
        sleep 1
    done
    echo "TIMEOUT-WAIT"
}
res() { python3 -c "import json,sys;d=json.load(open('$STORAGE/results/$1_result.json'));print(d$2)" 2>/dev/null; }

echo "== 0. daemon 存活 =="
HB="$STORAGE/heartbeat.json"
if [[ -f "$HB" ]]; then
    age=$(( $(date +%s) - $(stat -c %Y "$HB") ))
    echo "  heartbeat ${age}s ago"
    [[ "$age" -lt 180 ]]; chk "daemon heartbeat fresh (${age}s)" $?
else
    chk "heartbeat.json exists" 1
fi

echo "== 1. 基本执行 =="
T=$(submit --command 'hostname; uname -r; nproc')
wait_done "$T" 30 >/dev/null
OUT=$(res "$T" "['output']['stdout']")
echo "$OUT" | sed 's/^/    /' | head -3
[[ -n "$OUT" ]]; chk "remote command runs" $?

echo "== 2. shell 语法（管道/变量/重定向） =="
T=$(submit --command 'X=ok; echo "$X" | tr a-z A-Z > /tmp/bifrost_shell_test; cat /tmp/bifrost_shell_test')
wait_done "$T" 30 >/dev/null
[[ "$(res "$T" "['output']['stdout'].strip()")" == "OK" ]]; chk "shell pipe+redirect" $?

echo "== 3. GPU 可见性 =="
T=$(submit --command 'nvidia-smi --query-gpu=index,name --format=csv,noheader')
wait_done "$T" 30 >/dev/null
GPUS=$(res "$T" "['output']['stdout']")
echo "$GPUS" | head -8 | sed 's/^/    /'
NGPU=$(echo "$GPUS" | grep -c 'GPU\|NVIDIA' || true)
[[ "$NGPU" -gt 0 ]]; chk "nvidia-smi sees $NGPU GPU(s)" $?

echo "== 4. GPU 池：8 个并发任务应拿到不同卡 =="
TIDS=()
for i in $(seq 1 "$NGPU"); do
    TIDS+=("$(submit --command 'sleep 3; echo "cuda=$CUDA_VISIBLE_DEVICES"')")
done
sleep 2
ASSIGNED=""
for t in "${TIDS[@]}"; do
    wait_done "$t" 60 >/dev/null
    v=$(res "$t" "['output']['stdout'].strip()")
    ASSIGNED+="${v#cuda=} "
done
UNIQ=$(echo "$ASSIGNED" | tr ' ' '\n' | sed '/^$/d' | sort -u | wc -l)
echo "    assigned: $ASSIGNED"
[[ "$UNIQ" -eq "$NGPU" ]]; chk "每个并发任务独占一张卡（$UNIQ/$NGPU 唯一）" $?

echo "== 5. 显式 timeout 覆盖 daemon 默认 =="
T=$(submit --command 'sleep 30' --timeout 3)
s=$(wait_done "$T" 60); [[ "$s" == "Timeout" ]]; chk "per-task timeout -> Timeout" $?

echo "== 6. cancel 运行中任务 =="
T=$(submit --command 'sleep 120' --timeout 180)
sleep 2
c cancel "$T" >/dev/null 2>&1
s=$(wait_done "$T" 30); [[ "$s" == "Cancelled" ]]; chk "cancel -> Cancelled (${s})" $?

echo "== 7. artifacts 收集 =="
JOB=$(mktemp /tmp/bifrost-job.XXXXXX.yaml)
cat > "$JOB" <<EOF
name: validate-artifacts
tasks:
  - name: produce
    command: echo '{"score":1}' > /tmp/bifrost_artifact.json
    timeout: 30
    artifacts: [/tmp/bifrost_artifact.json]
EOF
c submit --job "$JOB" >/dev/null 2>&1
rm -f "$JOB"
sleep 2
ls "$STORAGE"/artifacts/*bifrost_artifact.json >/dev/null 2>&1; chk "artifact copied to shared artifacts/" $?

echo "== 8. 默认不重试 / 显式重试 =="
M=/tmp/bifrost_attempts_$$
T=$(submit --command "echo x >> $M; exit 3")
wait_done "$T" 30 >/dev/null
[[ "$(wc -l < "$M" 2>/dev/null || echo 0)" == "1" ]]; chk "no retry by default" $?
M2=/tmp/bifrost_attempts2_$$
T=$(submit --command "n=\$(cat $M2 2>/dev/null || echo 0); n=\$((n+1)); echo \$n > $M2; test \$n -ge 3" --retries 2)
wait_done "$T" 60 >/dev/null
[[ "$(cat "$M2" 2>/dev/null)" == "3" ]]; chk "--retries 2 -> 3 attempts" $?

echo
echo "================ RESULT ================"
echo "  passed: $pass    failed: $fail"
[[ $fail -gt 0 ]] && printf "  failed: %s\n" "${failed[*]}" && exit 1
echo "  all checks passed"
