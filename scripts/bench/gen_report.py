#!/usr/bin/env python3
"""从 <root>/summary.tsv + 各版本 result.json 生成 MD 报告。"""
import json, os, sys

root = sys.argv[1]
title = sys.argv[2] if len(sys.argv) > 2 else os.path.basename(root)
summary = os.path.join(root, "summary.tsv")
if not os.path.exists(summary):
    print(f"[skip] 无 summary: {summary}"); raise SystemExit(0)

rows = [l.rstrip("\n").split("\t") for l in open(summary) if l.strip()]
header, body = rows[0], rows[1:]
metrics = {}
for r in body:
    if len(r) < 9 or r[2] != "ok":
        continue
    f = r[8]
    if os.path.exists(f):
        metrics[r[0]] = json.load(open(f))

out = [f"# {title}", "", "## 口径", "", "```",
       "输入 65536 / 输出 2048 / 32 prompts / 并发 8 · /v1/completions · random",
       "NCCL_NVLS_ENABLE=0 · --disable-custom-all-reduce · gpu-mem-util 0.95",
       "```", "", "## 结果", "",
       "| 版本 | 配置 | 状态 | req/s | 输出 tok/s | 总 tok/s | TTFT 均 | TTFT p99 | TPOT | 起服务 | 压测 |",
       "|---|---|---|---|---|---|---|---|---|---|---|"]
for r in body:
    ver, cfg, status = r[0], r[1], r[2]
    m = metrics.get(ver)
    if m:
        out.append("| %s | %s | %s | %.3f | %.1f | %.1f | %.2f s | %.2f s | %.2f ms | %s s | %s s |" % (
            ver, cfg, status, m.get("request_throughput", 0), m.get("output_throughput", 0),
            m.get("total_token_throughput", 0), m.get("mean_ttft_ms", 0) / 1000,
            m.get("p99_ttft_ms", 0) / 1000, m.get("mean_tpot_ms", 0),
            r[4] if len(r) > 4 else "-", r[5] if len(r) > 5 else "-"))
    else:
        out.append("| %s | %s | %s | — | — | — | — | — | — | %s | %s |" % (
            ver, cfg, status, r[4] if len(r) > 4 and r[4] else "-", r[5] if len(r) > 5 and r[5] else "-"))

fails = [r for r in body if len(r) > 2 and r[2] != "ok"]
if fails:
    out += ["", "## 未产出数据的版本", ""]
    for r in fails:
        out.append(f"- `{r[0]}` — {r[2]}")
        log = os.path.join(root, r[0], "serve.log")
        if os.path.exists(log):
            for line in open(log, errors="ignore"):
                if any(k in line for k in ("RuntimeError:", "ValueError:", "OSError:", "NotImplementedError:")):
                    out.append(f"  - `{line.strip()[:200]}`"); break

out += ["", "## 原始数据", "",
        "每个版本目录下：`result.json`（vLLM bench 输出）、`serve.log`、`bench.log`、`load.log`",
        "", f"汇总表：`{summary}`"]
open(os.path.join(root, "REPORT.md"), "w").write("\n".join(out) + "\n")
print(f"[ok] {os.path.join(root, 'REPORT.md')}")
