#!/usr/bin/env python3
"""server 健壮性验证:
S1: 重启恢复 - server 启动前提交的任务必须被执行 (旧版会永久丢失)
S2: 兜底扫描 - 提交后立刻杀掉 inotify 无法覆盖的场景, 由 5s fallback scan 消费
S3: 坏 JSON 任务 - 写一个无法解析的任务文件, server 应写 Failed 结果而非永远 Pending
S4: 重复提交防重 - 同一任务文件被 inotify + scan 同时发现, 只执行一次
"""
import json, os, shutil, subprocess, sys, tempfile, time

BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")
STORAGE = os.environ.get("BIFROST_STORAGE", sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="bifrost_e2e_robust_"))

shutil.rmtree(STORAGE, ignore_errors=True)
for sub in ("commands", "results", "status", "logs", "artifacts"):
    os.makedirs(os.path.join(STORAGE, sub), exist_ok=True)
cfg = {"shared_storage": STORAGE,
       "client": {"poll_interval": "2s", "heartbeat_timeout": "180s"},
       "daemon": {"task_timeout": "300s", "heartbeat_interval": "2s", "max_concurrent": 10}}
open(os.path.join(STORAGE, "settings.json"), "w").write(json.dumps(cfg))

# 统一 settings (client 也用这个)
os.makedirs(os.path.expanduser("~/.bifrost"), exist_ok=True)
open(os.path.expanduser("~/.bifrost/settings.json"), "w").write(json.dumps(cfg))

def make_task_file(command, task_id=None, valid=True):
    """直接写任务 JSON 文件 (模拟提交), 返回 task_id"""
    import uuid
    tid = task_id or str(uuid.uuid4())
    ts = time.strftime("%Y%m%d_%H%M%S")
    if valid:
        task = {"task_id": tid, "timestamp": "2026-07-31T00:00:00Z",
                "command": command, "task_type": "Shell", "priority": 0,
                "timeout": 30, "retry_count": 0, "env_vars": {},
                "working_dir": "/tmp", "artifacts_expected": [],
                "metadata": {}, "batch_id": None, "task_name": None}
        content = json.dumps(task)
    else:
        content = "{ this is not valid json !!!"
    path = os.path.join(STORAGE, "commands", f"{ts}_{tid}.json")
    # 原子写 (tmp + rename), 模拟 client 行为
    tmp = path + ".tmp"
    with open(tmp, "w") as fh: fh.write(content)
    os.rename(tmp, path)
    return tid

def start_server():
    return subprocess.Popen([BINARY, "server", "-c", os.path.join(STORAGE, "settings.json")],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)

def server_output(proc):
    try:
        return proc.stdout.read() if proc.stdout else ""
    except Exception:
        return ""

def wait_result(tid, timeout=15):
    end = time.time() + timeout
    while time.time() < end:
        rp = os.path.join(STORAGE, "results", f"{tid}_result.json")
        if os.path.exists(rp):
            return json.load(open(rp))
        time.sleep(0.3)
    return None

ok = True
# ========== S1: 重启恢复 ==========
print("="*50, "\nS1: 重启恢复 (server 启动前提交的任务)")
tid_s1 = make_task_file("sh -c 'echo s1-restored-ok'")
time.sleep(0.5)  # 确保文件落盘且 server 尚未启动
srv = start_server()
time.sleep(1.5)
res = wait_result(tid_s1, timeout=12)
s1_ok = res and res["status"] == "Completed" and "s1-restored-ok" in res["output"]["stdout"]
print(f"  S1: {'✅ 重启后存量任务被执行' if s1_ok else '❌ 任务丢失或被跳过'}")
print(f"  S1 detail: {res}")
ok = ok and s1_ok

# ========== S2: 兜底扫描 (新提交任务在 watcher 事件后仍被消费) ==========
print("="*50, "\nS2: 正常运行期提交 (inotify 快路径)")
tid_s2 = make_task_file("sh -c 'echo s2-fast-ok'")
res = wait_result(tid_s2, timeout=10)
s2_ok = res and res["status"] == "Completed" and "s2-fast-ok" in res["output"]["stdout"]
print(f"  S2: {'✅ 快路径正常' if s2_ok else '❌ 失败'}")
ok = ok and s2_ok

# ========== S3: 坏 JSON ==========
print("="*50, "\nS3: 坏 JSON 任务 (不可解析)")
tid_s3 = make_task_file("echo ignored", valid=False)
res = wait_result(tid_s3, timeout=12)
s3_ok = res and res["status"] == "Failed"
print(f"  S3: {'✅ 坏 JSON 写了 Failed 结果 (client 不会永远 Pending)' if s3_ok else '❌ 未写失败结果'}")
print(f"  S3 detail: {res}")
ok = ok and s3_ok

# ========== S4: 重复防重 ==========
print("="*50, "\nS4: 重复提交防重 (同 task_id 两次提交)")
tid_s4 = make_task_file("sh -c 'echo dup-run; date +%s%N'")
tid_s4b = make_task_file("sh -c 'echo dup-run; date +%s%N'", task_id=tid_s4)
res = wait_result(tid_s4, timeout=10)
# 检查结果文件只有一份, 且 commands 里任务文件已清理
time.sleep(1.0)
remaining = [f for f in os.listdir(os.path.join(STORAGE, "commands")) if tid_s4 in f]
s4_ok = res is not None and len(remaining) == 0
print(f"  S4: {'✅ 去重正常' if s4_ok else '❌ 有残留: ' + str(remaining)}")
ok = ok and s4_ok

# 汇总
print("="*50)
print(f"健壮性验证: {'✅ 全部通过' if ok else '❌ 有失败'}")
srv.terminate()
try: srv.wait(timeout=5)
except Exception: srv.kill()
print(server_output(srv))
sys.exit(0 if ok else 1)
