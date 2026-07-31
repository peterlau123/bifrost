#!/usr/bin/env python3
"""Bifrost --job 专项测试: 验证 YAML job 工作流.

用例:
  J1 基本 job:     3 任务 (ok/fail/sleep) → JobResult 汇总
  J2 执行顺序:     3 任务顺序执行, 验证文件追加顺序
  J3 job 内超时:   sleep 30 + timeout 2 → JobTaskResult Timeout
  J4 env/wd 传递:  working_dir + env_vars 是否传到任务
  J5 ignore_failure: 失败任务后 job 是否继续
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time

BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")
STORAGE = os.environ.get("BIFROST_STORAGE", sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="bifrost_e2e_job_"))
JOBS = os.environ.get("BIFROST_JOBS", os.path.join(os.path.dirname(os.path.abspath(__file__)), "jobs"))
# J2 顺序文件放存储区, 通过环境变量传给 server 进程 (job YAML 里用 $BIFROST_ORDER_FILE)
ORDER_FILE = os.path.join(STORAGE, "job_order.txt")
os.environ["BIFROST_ORDER_FILE"] = ORDER_FILE
CLIENT_SETTINGS = os.environ.get("BIFROST_CLIENT_SETTINGS", os.path.expanduser("~/.bifrost/settings.json"))

results = []
def log(name, ok, detail=""):
    results.append((name, ok, detail))
    print(f"  {'✅' if ok else '❌'} {name}: {detail}")

def setup():
    shutil.rmtree(STORAGE, ignore_errors=True)
    for sub in ("commands", "results", "status", "logs", "artifacts"):
        os.makedirs(os.path.join(STORAGE, sub), exist_ok=True)
    cfg = {"shared_storage": STORAGE,
           "daemon": {"task_timeout": "300s", "heartbeat_interval": "60s", "max_concurrent": 10}}
    with open(os.path.join(STORAGE, "settings.json"), "w") as fh:
        json.dump(cfg, fh)
    with open(CLIENT_SETTINGS, "w") as fh:
        json.dump(cfg, fh)
    if os.path.exists(ORDER_FILE):
        os.remove(ORDER_FILE)

def start_server():
    proc = subprocess.Popen([BINARY, "server", "-c", os.path.join(STORAGE, "settings.json")],
                            stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
    time.sleep(1.0)
    return proc

def run_job(yaml_name, timeout_s=90):
    """运行 client submit --job, 返回 (exit_code, stdout_json, stderr_text)"""
    t0 = time.time()
    r = subprocess.run([BINARY, "client", "submit", "--job", os.path.join(JOBS, yaml_name)],
                       capture_output=True, text=True, timeout=timeout_s)
    return r.returncode, r.stdout, r.stderr, time.time() - t0

def main():
    setup()
    server = start_server()
    try:
        # ── J1: 基本 job ────────────────────────────────────────
        print("\n[J1] 基本 job (echo-ok / echo-fail / sleep-1)")
        rc, out, err, dt = run_job("j1_basic.yaml")
        try:
            jr = json.loads(out)
            names = [t["name"] for t in jr["task_results"]]
            statuses = [t["status"] for t in jr["task_results"]]
            log("J1 结构", set(names) == {"echo-ok", "echo-fail", "sleep-1"},
                f"tasks={names}")
            log("J1 状态汇总", jr["status"] == "CompletedWithFailures",
                f"job_status={jr['status']} completed={jr['completed_tasks']} failed={jr['failed_tasks']}")
            log("J1 各任务状态", statuses == ["Completed", "Failed", "Completed"],
                f"statuses={statuses}")
            ok_stdout = jr["task_results"][0]["stdout"].strip()
            log("J1 stdout 捕获", ok_stdout == "job-task-ok", f"stdout={ok_stdout!r}")
            fail_code = jr["task_results"][1]["exit_code"]
            log("J1 exit_code 捕获", fail_code == 3, f"exit_code={fail_code}")
            dur = jr["task_results"][2]["duration_secs"]
            log("J1 耗时记录", dur >= 1, f"sleep-1 duration={dur}s")
        except Exception as e:
            log("J1 解析", False, f"JSON 解析失败: {e} | stdout={out[:200]!r} | stderr={err[:200]!r}")

        # ── J2: 执行顺序 ────────────────────────────────────────
        print("\n[J2] 执行顺序 (first/second/third 追加)")
        rc, out, err, dt = run_job("j2_order.yaml")
        if os.path.exists(ORDER_FILE):
            with open(ORDER_FILE) as fh:
                order = fh.read().strip()
            log("J2 顺序", order == "first\nsecond\nthird", f"顺序={order!r}")
        else:
            log("J2 顺序", False, "job_order.txt 未生成")

        # ── J3: job 内超时 ──────────────────────────────────────
        print("\n[J3] job 内任务超时 (sleep 30 + timeout 2)")
        rc, out, err, dt = run_job("j3_timeout.yaml", timeout_s=60)
        try:
            jr = json.loads(out)
            t = jr["task_results"][0]
            log("J3 超时状态", t["status"] == "Timeout", f"status={t['status']} dur={t['duration_secs']}s")
            log("J3 超时消息", "timed out" in (t.get("error_message") or "").lower(),
                f"error={t.get('error_message')}")
        except Exception as e:
            log("J3 解析", False, f"JSON 解析失败: {e} | stdout={out[:200]!r}")

        # ── J4: env/wd 传递 ─────────────────────────────────────
        print("\n[J4] working_dir + env_vars 传递")
        rc, out, err, dt = run_job("j4_env_wd.yaml")
        try:
            jr = json.loads(out)
            t = jr["task_results"][0]
            stdout = t.get("stdout", "")
            # 预期: pwd=/tmp, $MY_VAR=job-env-ok
            ok_wd = "/tmp" in stdout
            ok_env = "job-env-ok" in stdout
            log("J4 working_dir", ok_wd, f"stdout={stdout.strip()!r}")
            log("J4 env_vars", ok_env, f"stdout={stdout.strip()!r}")
        except Exception as e:
            log("J4 解析", False, f"JSON 解析失败: {e} | stdout={out[:200]!r}")

        # ── J5: ignore_failure ──────────────────────────────────
        print("\n[J5] ignore_failure 语义")
        rc, out, err, dt = run_job("j5_ignore_failure.yaml")
        try:
            jr = json.loads(out)
            names = [t["name"] for t in jr["task_results"]]
            statuses = [t["status"] for t in jr["task_results"]]
            log("J5 失败后继续", len(jr["task_results"]) == 2 and "after-fail" in names,
                f"tasks={list(zip(names, statuses))}")
        except Exception as e:
            log("J5 解析", False, f"JSON 解析失败: {e} | stdout={out[:200]!r}")

        # ── server 日志 ─────────────────────────────────────────
        print("\n[server 日志]")
        server.terminate()
        try:
            server.wait(timeout=5)
        except subprocess.TimeoutExpired:
            server.kill()
        out_log = server.stdout.read() if server.stdout else ""
        for line in out_log.splitlines()[-15:]:
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
