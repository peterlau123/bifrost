#!/usr/bin/env python3
"""Bifrost 端到端测试统一入口.

按顺序运行全部 e2e 测试 (每个脚本自管理临时存储区, 互不干扰):

  1. test_timeout.py         超时/泄漏/串行阻塞 (T1-T5)
  2. test_job.py             --job YAML 工作流 (J1-J5)
  3. test_concurrent.py      并发执行 (N 任务并行)
  4. test_pytest_concurrent.py 多卡 pytest 并行 (GPU 隔离)
  5. test_robustness.py      server 健壮性 (S1-S4)
  6. test_mcp_e2e.py         MCP stdio 全链路 (health→submit→status→result)

用法:
  python3 tests/e2e/run_all.py [BINARY_PATH]
  环境变量: BIFROST_BIN 指定二进制 (默认 ./target/release/bifrost)
"""
import os
import subprocess
import sys
import time

HERE = os.path.dirname(os.path.abspath(__file__))
BINARY = os.environ.get("BIFROST_BIN", sys.argv[1] if len(sys.argv) > 1 else "./target/release/bifrost")

TESTS = [
    ("timeout", "test_timeout.py"),
    ("job", "test_job.py"),
    ("concurrent", "test_concurrent.py"),
    ("pytest-concurrent", "test_pytest_concurrent.py"),
    ("robustness", "test_robustness.py"),
    ("mcp-e2e", "test_mcp_e2e.py"),
]

def main():
    if not os.path.exists(BINARY):
        print(f"❌ binary not found: {BINARY} (build with: cargo build --release)")
        return 2

    results = []
    t0 = time.time()
    for name, script in TESTS:
        path = os.path.join(HERE, script)
        print(f"\n{'='*60}\n▶ {name}: {script}")
        t1 = time.time()
        try:
            r = subprocess.run(
                [sys.executable, path, BINARY],
                env={**os.environ, "BIFROST_BIN": BINARY},
                timeout=600,
            )
            ok = r.returncode == 0
        except subprocess.TimeoutExpired:
            print(f"  ❌ {name}: TIMEOUT (>600s)")
            ok = False
        dt = time.time() - t1
        results.append((name, ok, dt))
        print(f"  → {name}: {'✅ PASS' if ok else '❌ FAIL'} ({dt:.1f}s)")

    print(f"\n{'='*60}\n汇总:")
    total_ok = True
    for name, ok, dt in results:
        total_ok = total_ok and ok
        print(f"  {'✅' if ok else '❌'} {name:<18} {dt:5.1f}s")
    print(f"\n端到端测试: {'✅ 全部通过' if total_ok else '❌ 有失败'}  (总耗时 {time.time()-t0:.1f}s)")
    return 0 if total_ok else 1

if __name__ == "__main__":
    sys.exit(main())
