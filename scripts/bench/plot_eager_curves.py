#!/usr/bin/env python3
"""绘制 eager 模式下各模型的版本趋势折线图（每个模型、每个指标各一张）。

数据来源：docs/bench-results-2026-10-22_0130Z/RESULTS.md
    - MiniMax-M2.7 · eager      (TP4, --enforce-eager)
    - Qwen3.6-35B-A3B · eager   (TP4, --enforce-eager)

用法：
    python scripts/bench/plot_eager_curves.py [out_dir]
默认输出到 docs/bench-results-2026-10-22_0130Z/charts/。
"""
import os
import sys

import matplotlib

matplotlib.use("Agg")
import matplotlib.pyplot as plt

# 版本按语义升序，横轴顺序即此处顺序
VERSIONS = ["v0.17.1", "v0.19.1", "v0.22.1", "v0.25.1", "v0.26.0", "v0.29.0", "v0.31.0"]

DATA = {
    "MiniMax-M2.7 (TP4)": {
        "slug": "m2.7",
        "color": "#c0392b",
        # 输出 tok/s
        "output": [77.9, 79.7, 78.0, 94.8, 95.5, 91.3, 79.6],
        # TTFT 均 (s)
        "ttft": [7.60, 7.63, 7.23, 7.67, 7.03, 6.88, 6.88],
        # TPOT (ms)
        "tpot": [98.65, 96.38, 98.77, 80.31, 80.06, 83.99, 96.79],
    },
    "Qwen3.6-35B-A3B (TP4)": {
        "slug": "qwen3.6-35b",
        "color": "#1f6fb4",
        "output": [93.9, 105.4, 101.4, 98.7, 100.5, 101.5, 102.0],
        "ttft": [3.05, 3.07, 3.56, 2.95, 2.88, 2.60, 2.92],
        "tpot": [83.24, 73.88, 76.76, 79.25, 77.85, 77.32, 76.69],
    },
}

CHARTS = [
    ("output", "输出吞吐", "输出 tok/s（越高越好）", "01-output-throughput.png", "%.1f"),
    ("ttft", "Mean TTFT", "平均 TTFT（秒，越低越好）", "02-mean-ttft.png", "%.2f"),
    ("tpot", "Mean TPOT", "平均 TPOT（毫秒，越低越好）", "03-mean-tpot.png", "%.2f"),
]

XLABEL = "vLLM 版本（升序）"


def plot(model, ys, color, title, ylabel, filename, fmt, out_dir):
    fig, ax = plt.subplots(figsize=(8.2, 4.4), dpi=150)
    xs = range(len(VERSIONS))
    span = max(ys) - min(ys) or 1.0

    ax.plot(xs, ys, marker="o", markersize=5, linewidth=2, color=color)

    for x, y in zip(xs, ys):
        left = ys[x] - ys[x - 1] if x > 0 else None
        right = ys[x + 1] - ys[x] if x + 1 < len(ys) else None
        slopes = [s for s in (left, right) if s is not None]
        slope = sum(slopes) / len(slopes)
        # 局部极小点与首尾点标在无折线的一侧（陡升点标下方，陡降点标上方）
        is_min = left is not None and right is not None and left < 0 and right > 0
        if is_min:
            dx, dy = 0, -18
        elif x in (0, len(ys) - 1):
            dx, dy = 0, (-18 if slope > 0 else 9)   # 居中，避免文字被画布裁切
        else:
            dx, dy = (-12 if slope >= 0 else 12), 9
            if abs(slope) > 0.1 * span:   # 陡坡段拉近，防止文字被线段压住
                dx = int(dx / 2)
        ax.annotate(fmt % y, (x, y), textcoords="offset points", xytext=(dx, dy),
                    ha="center", fontsize=7.5, color=color)

    ax.set_title(f"{model} · eager 模式 · {title}", fontsize=12)
    ax.set_xlabel(XLABEL)
    ax.set_ylabel(ylabel)
    ax.set_xticks(list(xs))
    ax.set_xticklabels(VERSIONS)
    ax.grid(True, linestyle="--", alpha=0.45)
    ax.margins(x=0.04, y=0.14)

    fig.tight_layout()
    path = os.path.join(out_dir, filename)
    fig.savefig(path)
    plt.close(fig)
    return path


def main():
    here = os.path.dirname(os.path.abspath(__file__))
    root = os.path.abspath(os.path.join(here, "..", ".."))
    out_dir = sys.argv[1] if len(sys.argv) > 1 else os.path.join(
        root, "docs", "bench-results-2026-10-22_0130Z", "charts")
    os.makedirs(out_dir, exist_ok=True)

    plt.rcParams["font.sans-serif"] = ["Microsoft YaHei", "SimHei", "DejaVu Sans"]
    plt.rcParams["axes.unicode_minus"] = False

    for model, series in DATA.items():
        for metric, title, ylabel, filename, fmt in CHARTS:
            print("[ok]", plot(model, series[metric], series["color"], title, ylabel,
                               f"{series['slug']}-eager-{filename}", fmt, out_dir))


if __name__ == "__main__":
    main()
