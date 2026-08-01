#!/usr/bin/env python3
"""pytest 多卡并发模拟: 提交 N 个 pytest 任务, 每个指定不同 CUDA_VISIBLE_DEVICES,
验证 server 并行执行且各自环境变量正确隔离."""
import json, os, shutil, subprocess, sys, tempfile, time

BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")
STORAGE = os.environ.get("BIFROST_STORAGE", sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="bifrost_e2e_pytest_concurrent_"))

# 伪 pytest 脚本放存储区 (CI 无 /gpfs) - 注意必须等 rmtree 之后再写
fake_pytest = os.path.join(STORAGE, "fake_pytest.py")

# 1. 准备存储区
shutil.rmtree(STORAGE, ignore_errors=True)
for sub in ("commands", "results", "status", "logs", "artifacts"):
    os.makedirs(os.path.join(STORAGE, sub), exist_ok=True)
cfg = {"shared_storage": STORAGE,
       "client": {"poll_interval": "2s", "heartbeat_timeout": "180s"},
       "daemon": {"task_timeout": "300s", "heartbeat_interval": "2s", "max_concurrent": 10}}
open(os.path.join(STORAGE, "settings.json"), "w").write(json.dumps(cfg))
os.makedirs(os.path.expanduser("~/.bifrost"), exist_ok=True)
open(os.path.expanduser("~/.bifrost/settings.json"), "w").write(json.dumps(cfg))

with open(fake_pytest, "w") as fh:
    fh.write('''#!/usr/bin/env python3
import os, time, sys
# 模拟 pytest 用例: 打印当前 GPU, sleep 2s
print(f"GPU={os.environ.get('CUDA_VISIBLE_DEVICES', 'unset')} running-on-{os.getpid()}", flush=True)
time.sleep(2)
print("PASS", flush=True)
''')

# 2. 启动 server
server = subprocess.Popen([BINARY, "server", "-c", os.path.join(STORAGE, "settings.json")],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.2)

# 3. 提交 4 个"pytest 用例", 各指定不同 GPU (通过命令内 env 前缀, 模拟不同卡)
gpus = ["0", "1", "2", "3"]
task_ids = []
t0 = time.time()
for i, gpu in enumerate(gpus):
    cmd = f"sh -c 'CUDA_VISIBLE_DEVICES={gpu} python3 {fake_pytest}'"
    r = subprocess.run([BINARY, "client", "submit", "--command", cmd,
                        "--timeout", "60", "--working-dir", "/tmp"],
                       capture_output=True, text=True, timeout=30)
    for line in r.stdout.splitlines():
        if "Task ID:" in line:
            task_ids.append(line.split("Task ID:")[1].strip())
print(f"提交 4 个 pytest 任务完成: {time.time()-t0:.2f}s")

# 4. 等待全部完成, 收集 stdout
done = set()
stdouts = {}
while len(done) < len(task_ids) and time.time() - t0 < 60:
    for tid in task_ids:
        if tid in done:
            continue
        try:
            res = json.load(open(os.path.join(STORAGE, "results", f"{tid}_result.json")))
            done.add(tid)
            stdouts[tid] = res["output"]["stdout"]
        except Exception:
            pass
    time.sleep(0.5)

total = time.time() - t0
print(f"全部完成: 端到端 {total:.2f}s (串行预期 ~8s, 并行预期 ~2.5s)")

# 5. 校验: 每个任务看到不同的 GPU
gpu_seen = set()
for tid, out in stdouts.items():
    for line in out.splitlines():
        if line.startswith("GPU="):
            gpu_seen.add(line.split("GPU=")[1].split()[0])
            print(f"  {tid[:8]} → {line.strip()}")
ok = len(gpu_seen) == 4 and total < 5
print(f"\n结论: {'✅ 4 个 pytest 任务并行执行且 GPU 隔离正确' if ok else '❌ 有问题'}")
print(f"观察到 GPU: {sorted(gpu_seen)}")

server.terminate()
server.wait(timeout=5)
sys.exit(0 if ok else 1)
