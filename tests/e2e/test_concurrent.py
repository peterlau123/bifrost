#!/usr/bin/env python3
"""并发验证: 同时提交 N 个独立任务, 测 server 是否并行执行.

如果并行: 4 个 sleep 3 任务总耗时 ≈ 3s (而非 12s 串行).
"""
import json, os, shutil, subprocess, sys, tempfile, time

BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")
STORAGE = os.environ.get("BIFROST_STORAGE", sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="bifrost_e2e_concurrent_"))
N = int(sys.argv[3]) if len(sys.argv) > 3 else 4
SLEEP = int(sys.argv[4]) if len(sys.argv) > 4 else 3

# 1. 准备存储区
shutil.rmtree(STORAGE, ignore_errors=True)
for sub in ("commands", "results", "status", "logs", "artifacts"):
    os.makedirs(os.path.join(STORAGE, sub), exist_ok=True)
cfg = {"shared_storage": STORAGE,
       "client": {"poll_interval": "2s", "heartbeat_timeout": "180s"},
       "daemon": {"task_timeout": "300s", "heartbeat_interval": "2s", "max_concurrent": 10}}
open(os.path.join(STORAGE, "settings.json"), "w").write(json.dumps(cfg))
open(os.path.expanduser("~/.bifrost/settings.json"), "w").write(json.dumps(cfg))

# 2. 启动 server
server = subprocess.Popen([BINARY, "server", "-c", os.path.join(STORAGE, "settings.json")],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.2)

t0 = time.time()
# 3. 并发提交 N 个任务 (不等待, 一口气全交)
task_ids = []
for i in range(N):
    r = subprocess.run([BINARY, "client", "submit", "--command",
                        f"sh -c 'sleep {SLEEP}; echo task-{i}-done'",
                        "--timeout", "60", "--working-dir", "/tmp"],
                       capture_output=True, text=True, timeout=30)
    for line in r.stdout.splitlines():
        if "Task ID:" in line:
            task_ids.append(line.split("Task ID:")[1].strip())
    assert task_ids[-1] if task_ids else True
submit_elapsed = time.time() - t0
print(f"提交 {len(task_ids)} 个任务完成: {submit_elapsed:.2f}s")

# 4. 轮询等待全部完成
done = set()
durations = {}
t_poll = time.time()
while len(done) < len(task_ids) and time.time() - t_poll < 60:
    for tid in task_ids:
        if tid in done:
            continue
        r = subprocess.run([BINARY, "client", "status", tid], capture_output=True, text=True, timeout=15)
        for line in r.stdout.splitlines():
            if "Status:" in line:
                st = line.split("Status:")[1].strip()
                if st in ("Completed", "Failed", "Timeout"):
                    # 读结果拿 duration_ms
                    try:
                        res = json.load(open(os.path.join(STORAGE, "results", f"{tid}_result.json")))
                        durations[tid] = res.get("duration_ms", -1)
                    except Exception:
                        durations[tid] = -1
                    done.add(tid)
    time.sleep(0.5)

total = time.time() - t0
print(f"全部完成: 端到端 {total:.2f}s (串行预期 ≈ {N*SLEEP}s, 并行预期 ≈ {SLEEP+1}s)")
print(f"各任务耗时: {sorted(durations.values())}ms")
ok = total < N * SLEEP * 0.6  # 明显快于串行
print(f"\n结论: {'✅ 并行执行' if ok else '❌ 疑似串行'}")
print(f"并行度 ≈ {N*SLEEP/total:.1f}x")

server.terminate()
server.wait(timeout=5)
sys.exit(0 if ok else 1)
