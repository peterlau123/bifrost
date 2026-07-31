#!/usr/bin/env python3
"""MCP stdio 全链路测试: health → submit → status → result.
直接向 bifrost mcp-serve 发送 JSON-RPC 消息 (模拟 Hermes MCP 客户端)."""
import json
import os
import subprocess
import sys
import tempfile
import time

BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")
STORAGE = os.environ.get("BIFROST_STORAGE", sys.argv[2] if len(sys.argv) > 2 else tempfile.mkdtemp(prefix="bifrost_e2e_mcp_"))
for sub in ("commands", "results", "status", "logs", "artifacts"):
    os.makedirs(os.path.join(STORAGE, sub), exist_ok=True)
cfg = {"shared_storage": STORAGE,
       "daemon": {"task_timeout": "300s", "heartbeat_interval": "2s", "max_concurrent": 10}}
CONFIG = os.path.join(STORAGE, "settings.json")
with open(CONFIG, "w") as fh:
    json.dump(cfg, fh)

proc = subprocess.Popen([BINARY, "mcp-serve", "-c", CONFIG],
                        stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                        stderr=subprocess.PIPE, text=True)

# MCP 工具读写的任务需要 daemon 执行: 自启动一个 server (同一存储区)
server = subprocess.Popen([BINARY, "server", "-c", CONFIG],
                          stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
time.sleep(1.2)

def rpc(proc, msg, timeout=15):
    proc.stdin.write(json.dumps(msg) + "\n")
    proc.stdin.flush()
    # 读一行响应 (MCP stdio 是逐行 JSON)
    import select
    deadline = time.time() + timeout
    line = ""
    while time.time() < deadline:
        r, _, _ = select.select([proc.stdout], [], [], 0.5)
        if r:
            line = proc.stdout.readline()
            if line:
                return json.loads(line)
    raise TimeoutError(f"no response to {msg['method']}")

try:
    # 1. initialize
    resp = rpc(proc, {"jsonrpc": "2.0", "id": 1, "method": "initialize",
                      "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                                 "clientInfo": {"name": "e2e-test", "version": "1.0"}}})
    print("1. initialize:", "OK" if "result" in resp else resp)
    # 2. initialized notification
    proc.stdin.write(json.dumps({"jsonrpc": "2.0", "method": "notifications/initialized"}) + "\n")
    proc.stdin.flush()

    # 3. tools/list
    resp = rpc(proc, {"jsonrpc": "2.0", "id": 2, "method": "tools/list"})
    tools = [t["name"] for t in resp["result"]["tools"]]
    print("2. tools/list:", tools)

    # 4. call bifrost_health
    resp = rpc(proc, {"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                      "params": {"name": "bifrost_health", "arguments": {}}})
    health = resp["result"]["content"][0]["text"]
    print("3. health:", health)

    # 5. call bifrost_submit
    resp = rpc(proc, {"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                      "params": {"name": "bifrost_submit",
                                 "arguments": {"command": "sh -c 'echo mcp-e2e-ok; sleep 1; echo done'",
                                               "timeout": 30}}})
    submit_out = json.loads(resp["result"]["content"][0]["text"])
    tid = submit_out["task_id"]
    print("4. submit:", submit_out)

    # 6. 等任务完成, 轮询 status
    for i in range(15):
        time.sleep(0.5)
        resp = rpc(proc, {"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                          "params": {"name": "bifrost_status", "arguments": {"task_id": tid}}})
        st = json.loads(resp["result"]["content"][0]["text"])
        if i in (0, 1, 2) or st["status"] not in ("Pending", "Running"):
            print(f"5. status[{i}]:", st["status"], st.get("message"))
        if st["status"] in ("Completed", "Failed", "Timeout"):
            break

    # 7. call bifrost_result
    resp = rpc(proc, {"jsonrpc": "2.0", "id": 6, "method": "tools/call",
                      "params": {"name": "bifrost_result", "arguments": {"task_id": tid}}})
    result = json.loads(resp["result"]["content"][0]["text"])
    print("6. result: status=%s exit=%s dur=%sms stdout=%r" % (
        result["status"], result["exit_code"], result["duration_ms"], result["stdout"]))

    # 8. 错误路径: 非法 task_id
    resp = rpc(proc, {"jsonrpc": "2.0", "id": 7, "method": "tools/call",
                      "params": {"name": "bifrost_status", "arguments": {"task_id": "bad-id"}}})
    print("7. 非法UUID:", "OK (结构化错误)" if "isError" in resp["result"] or resp["result"].get("isError") else resp)

    ok = (tools == ["bifrost_health", "bifrost_result", "bifrost_status", "bifrost_submit"]
          and health and submit_out.get("status") == "Pending"
          and result.get("status") == "Completed" and "mcp-e2e-ok" in result.get("stdout", ""))
    print("\n========== MCP E2E: %s ==========" % ("✅ 全部通过" if ok else "❌ 有失败"))
    sys.exit(0 if ok else 1)
finally:
    proc.terminate()
    try:
        proc.wait(timeout=3)
    except subprocess.TimeoutExpired:
        proc.kill()
    server.terminate()
    try:
        server.wait(timeout=5)
    except subprocess.TimeoutExpired:
        server.kill()
