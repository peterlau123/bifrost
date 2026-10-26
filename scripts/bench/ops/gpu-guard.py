#!/usr/bin/env python3
"""外部占卡守卫：每 30s 扫一次 GPU，发现非本任务的进程就停掉所有 bench unit 并写告警。"""
import subprocess, time, os, datetime

ALERT = "/data/liuxin/bifrost/bench/GPU_ALERT.txt"
LOG = "/data/liuxin/bifrost/bench/gpu-guard.log"
OURS = {"bifrost-bench", "probe", "probe2", "probe3", "probe-bench"}
THRESH = 512  # MiB

def log(msg):
    with open(LOG, "a") as f:
        f.write(f"[{datetime.datetime.utcnow():%Y-%m-%dT%H:%M:%SZ}] {msg}\n")

def sh(cmd):
    return subprocess.run(cmd, shell=True, capture_output=True, text=True).stdout.strip()

def owner(pid):
    m = subprocess.run(["bash", "-c", f"grep -o 'docker-[0-9a-f]\\{{12\\}}' /proc/{pid}/cgroup 2>/dev/null | head -1"],
                       capture_output=True, text=True).stdout.strip()
    if not m:
        return f"非容器进程(pid={pid})"
    cid = m.replace("docker-", "")
    name = subprocess.run(["docker", "inspect", "-f", "{{.Name}}", cid], capture_output=True, text=True).stdout.strip()
    return name.lstrip("/") or f"未知容器({cid})"

def scan():
    out = subprocess.run(["nvidia-smi", "--query-compute-apps=pid,used_memory", "--format=csv,noheader"],
                         capture_output=True, text=True).stdout
    foreign = {}
    for line in out.splitlines():
        try:
            pid_s, mem_s = line.split(",")
            pid, mem = int(pid_s.strip()), int(mem_s.strip().split()[0])
        except ValueError:
            continue
        if mem < THRESH:
            continue
        who = owner(pid)
        if who not in OURS:
            foreign.setdefault(who, 0)
            foreign[who] += mem
    return foreign

while True:
    try:
        foreign = scan()
    except Exception as e:      # 采集失败不算占卡，但要留痕
        log(f"扫描异常: {e}")
        foreign = {}
    if foreign:
        detail = "、".join(f"{k}({v} MiB)" for k, v in foreign.items())
        units = sh("systemctl list-units --all --no-legend 'bench-*' | awk '{print $1}'").split()
        for u in units:
            subprocess.run(["systemctl", "stop", u], capture_output=True)
        log(f"⚠️ 外部占卡: {detail} → 已停止 {len(units)} 个 bench unit: {' '.join(units)}")
        with open(ALERT, "w") as f:
            f.write(f"{datetime.datetime.utcnow():%Y-%m-%dT%H:%M:%SZ}\t外部程序占卡: {detail}\t已停止 {len(units)} 个评测 unit\n")
        break
    time.sleep(30)
