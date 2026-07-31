#!/usr/bin/env python3
"""Bifrost timeout 专项测试: 验证超时处理 + 子进程泄漏 + 串行阻塞.

用例:
  T1 基础超时:     sleep 10 + timeout 2 → 期望 Timeout 状态, ~2s 返回
  T2 进程泄漏:     T1 超时后检查 sleep 进程是否残留 (关键!)
  T3 边界完成:     sleep 1 + timeout 2 → 期望 Completed
  T4 串行阻塞:     先提交 sleep 10 + timeout 5, 再提交 echo 快任务
                    → 测快任务是否被长任务阻塞
  T5 超时后框架可用: 超时任务后继续提交新任务, 确认框架没卡死
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")
STORAGE = os.environ.get("BIFROST_STORAGE", sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="bifrost_e2e_timeout_"))
CLIENT_SETTINGS = os.environ.get("BIFROST_CLIENT_SETTINGS", os.path.expanduser("~/.bifrost/settings.json"))

results = []

def log(name, ok, detail=""):
    results.append((name, ok, detail))
    mark = "✅" if ok else "❌"
    print(f"  {mark} {name}: {detail}")

def setup():
    shutil.rmtree(STORAGE, ignore_errors=True)
    for sub in ("commands", "results", "status", "logs", "artifacts"):
        os.makedirs(os.path.join(STORAGE, sub), exist_ok=True)
    cfg = {"shared_storage": STORAGE,
           "daemon": {"task_timeout": "300s", "heartbeat_interval": "60s"}}
    with open(os.path.join(STORAGE, "settings.json"), "w") as fh:
        json.dump(cfg, fh)
    with open(CLIENT_SETTINGS, "w") as fh:
        json.dump(cfg, fh)

def start_server():
    proc = subprocess.Popen([BINARY, "server", "-c", os.path.join(STORAGE, "settings.json")],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    time.sleep(1.0)  # 等 watcher 注册
    return proc

def submit(cmd, timeout, wd="/tmp"):
    r = subprocess.run([BINARY, "client", "submit", "--command", cmd,
                        "--timeout", str(timeout), "--working-dir", wd],
                       capture_output=True, text=True, timeout=15)
    if r.returncode != 0:
        raise RuntimeError(f"submit failed: {r.stderr}")
    for line in r.stdout.splitlines():
        if "Task ID" in line:
            return line.split("Task ID:")[-1].strip()
    raise RuntimeError(f"no task id: {r.stdout}")

def wait_result(tid, timeout_s=20):
    rf = os.path.join(STORAGE, "results", f"{tid}_result.json")
    deadline = time.time() + timeout_s
    while time.time() < deadline:
        if os.path.exists(rf):
            with open(rf) as fh:
                return json.load(fh)
        time.sleep(0.1)
    return None

def check_status(tid):
    r = subprocess.run([BINARY, "client", "status", tid],
                       capture_output=True, text=True, timeout=15)
    return r.stdout

def count_sleep_procs():
    r = subprocess.run(["pgrep", "-f", "sleep 30"], capture_output=True, text=True)
    return len([l for l in r.stdout.splitlines() if l.strip()])

def main():
    setup()
    server = start_server()
    try:
        # ── T1+T2: 基础超时 + 进程泄漏 ──────────────────────────
        print("\n[T1/T2] sleep 30 + timeout 2 (超时 + 进程泄漏检测)")
        t0 = time.time()
        tid = submit("sleep 30", 2)
        r = wait_result(tid)
        elapsed = time.time() - t0
        if r is None:
            log("T1 结果写回", False, "20s 内无结果")
        else:
            ok = r["status"] == "Timeout" and 1.5 < elapsed < 8
            log("T1 超时状态", ok, f"status={r['status']} elapsed={elapsed:.1f}s dur={r.get('duration_ms')}ms")
            log("T1 错误消息", "timed out" in (r.get("error_message") or ""), r.get("error_message") or "")
        # 进程泄漏: sleep 30 超时后应立即消失 (tokio timeout 取消 future 不杀子进程?)
        time.sleep(0.5)
        n = count_sleep_procs()
        log("T2 进程泄漏", n == 0, f"残留 sleep 进程数={n}")

        # ── T3: 边界完成 ────────────────────────────────────────
        print("\n[T3] sleep 1 + timeout 2 (边界内完成)")
        t0 = time.time()
        tid = submit("sleep 1", 2)
        r = wait_result(tid)
        elapsed = time.time() - t0
        if r is None:
            log("T3 结果写回", False, "20s 内无结果")
        else:
            ok = r["status"] == "Completed" and elapsed < 3
            log("T3 边界完成", ok, f"status={r['status']} elapsed={elapsed:.1f}s")

        # ── T4: 串行阻塞 ────────────────────────────────────────
        print("\n[T4] 长任务阻塞检测: sleep 10+timeout 5 先提交, echo 后提交")
        t0 = time.time()
        tid_long = submit("sleep 10", 5)
        time.sleep(0.5)  # 确保长任务先被消费
        tid_fast = submit("echo fast-ok", 5)
        t_fast_submit = time.time() - t0
        r_fast = wait_result(tid_fast, timeout_s=15)
        t_fast_done = time.time() - t0
        r_long = wait_result(tid_long, timeout_s=15)
        t_long_done = time.time() - t0
        if r_fast is None:
            log("T4 快任务完成", False, "15s 内无结果 (可能被长任务阻塞)")
        else:
            # 快任务应该立刻完成, 除非被长任务串行阻塞
            ok = r_fast["status"] == "Completed" and t_fast_done < 5
            log("T4 快任务阻塞", ok,
                f"status={r_fast['status']} 提交后{t_fast_done:.1f}s完成 (长任务{t_long_done:.1f}s完成)")
            log("T4 长任务超时", r_long["status"] == "Timeout" if r_long else False,
                f"status={r_long['status'] if r_long else 'None'}")

        # ── T5: 超时后框架可用性 ────────────────────────────────
        print("\n[T5] 超时后框架可用性")
        time.sleep(1.0)
        t0 = time.time()
        tid = submit("echo after-timeout", 5)
        r = wait_result(tid, timeout_s=10)
        elapsed = time.time() - t0
        ok = r is not None and r["status"] == "Completed"
        log("T5 超时后提交", ok, f"status={r['status'] if r else 'None'} elapsed={elapsed:.1f}s")

        # ── 检查 server 日志是否有异常 ──────────────────────────
        print("\n[server 日志]")
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
        out = server.stdout.read() if server.stdout else ""
        for line in out.splitlines()[-15:]:
            print(f"  {line}")
    finally:
        if server.poll() is None:
            server.terminate()
            try:
                server.wait(timeout=5)
            except subprocess.TimeoutExpired:
                server.kill()

    print("\n========== 汇总 ==========")
    fails = [r for r in results if not r[1]]
    for name, ok, detail in results:
        print(f"  {'✅' if ok else '❌'} {name}")
    print(f"\n通过 {len(results)-len(fails)}/{len(results)}")
    return 1 if fails else 0

if __name__ == "__main__":
    sys.exit(main())
