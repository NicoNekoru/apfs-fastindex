"""
Generate the performance figures used in chapter 12 of the manual.

Source:
  - docs/implementation/measurement-baseline.md (standing baseline)
  - r2c-fallback-perf micro-optimisation pass (2026-05-16)
  - r2c-syscall-perf-research parallel-walker pass (2026-05-16, EX-25)

Run:    python3 perf_chart.py
Output:
  - perf-throughput.pdf    standing baseline by backend
  - perf-cpu.pdf           CPU stack for the bulk vs std comparison
  - perf-fallback-pass.pdf before/after user-CPU for the micro-opt pass
  - perf-parallel.pdf      EX-25 parallel-walker scaling curve
"""

from __future__ import annotations

import pathlib

import matplotlib.pyplot as plt
import numpy as np

FIG_DIR = pathlib.Path(__file__).resolve().parent

# Standing baseline (pre-parallel; matches measurement-baseline.md).
# (target, backend, entries, wall_s, user_s, sys_s)
ROWS = [
    ("proof fixture",       "raw",             7,         0.23,  0.013, 0.015),
    ("apfs-fastindex repo", "fallback (std)",  9_124,     0.07,  0.015, 0.048),
    ("/Applications",       "fallback (std)",  163_667,   1.28,  0.410, 0.728),
    ("/Applications",       "fallback (bulk)", 163_667,   1.04,  0.347, 0.608),
    ("/Users",              "fallback (bulk)", 1_304_073, 26.65, 2.92,  6.62),
    ("/ (whole machine)",   "fallback (std)",  5_251_546, 129.6, 16.3,  47.8),
    ("/ (whole machine)",   "fallback (bulk)", 5_260_624, 108.7, 14.85, 29.80),
    # Post-fallback-perf, single-threaded
    ("/Applications",       "T=1 (post-perf)", 163_667,   0.816, 0.227, 0.515),
    # Post-parallel (this slice, T=4)
    ("/Applications",       "T=4 (parallel)",  163_667,   0.523, 0.256, 0.802),
]

BACKEND_COLORS = {
    "raw":             "#7f7f7f",
    "fallback (std)":  "#5b8def",
    "fallback (bulk)": "#2ecc71",
    "T=1 (post-perf)": "#f39c12",
    "T=4 (parallel)":  "#c0392b",
}


def plot_throughput() -> None:
    labels = [f"{target}\n[{backend}]" for target, backend, *_ in ROWS]
    throughput = [entries / wall for _, _, entries, wall, *_ in ROWS]
    colors = [BACKEND_COLORS[row[1]] for row in ROWS]

    fig, ax = plt.subplots(figsize=(7.0, 5.0))
    ypos = np.arange(len(labels))
    bars = ax.barh(ypos, throughput, color=colors, edgecolor="black", linewidth=0.4)

    ax.set_yticks(ypos)
    ax.set_yticklabels(labels, fontsize=8)
    ax.invert_yaxis()
    ax.set_xlabel("entries / second (higher is better)", fontsize=9)
    ax.set_title("Scan throughput across backends and targets", fontsize=10)
    ax.tick_params(axis="x", labelsize=8)
    ax.grid(axis="x", linestyle=":", color="#999", alpha=0.5)
    ax.set_axisbelow(True)

    for bar, value in zip(bars, throughput):
        ax.text(
            bar.get_width() + max(throughput) * 0.01,
            bar.get_y() + bar.get_height() / 2,
            f"{value:,.0f}",
            va="center",
            ha="left",
            fontsize=8,
        )
    ax.set_xlim(0, max(throughput) * 1.18)

    handles = [
        plt.Rectangle((0, 0), 1, 1, facecolor=color, edgecolor="black", linewidth=0.4)
        for color in BACKEND_COLORS.values()
    ]
    # Legend below the plot so it never overlaps a bar value label.
    ax.legend(handles, BACKEND_COLORS.keys(),
              loc="upper center", bbox_to_anchor=(0.5, -0.10),
              fontsize=7.5, frameon=False, ncol=5)

    fig.subplots_adjust(left=0.22, right=0.97, top=0.93, bottom=0.18)
    fig.savefig(FIG_DIR / "perf-throughput.pdf")
    plt.close(fig)


def plot_cpu_breakdown() -> None:
    targets = ["/Applications", "/Users", "/ whole-machine"]
    std_rows = {row[0]: row for row in ROWS if row[1] == "fallback (std)"}
    bulk_rows = {row[0]: row for row in ROWS if row[1] == "fallback (bulk)"}

    keys = [("/Applications", "/Applications"),
            ("/Users", "/Users"),
            ("/ (whole machine)", "/ whole-machine")]

    user_std  = [std_rows[k[0]][4] if k[0] in std_rows else 0 for k in keys]
    sys_std   = [std_rows[k[0]][5] if k[0] in std_rows else 0 for k in keys]
    wall_std  = [std_rows[k[0]][3] if k[0] in std_rows else 0 for k in keys]
    user_bulk = [bulk_rows[k[0]][4] for k in keys]
    sys_bulk  = [bulk_rows[k[0]][5] for k in keys]
    wall_bulk = [bulk_rows[k[0]][3] for k in keys]

    fig, ax = plt.subplots(figsize=(7.0, 3.6))
    xpos = np.arange(len(targets))
    width = 0.36

    ax.bar(xpos - width / 2, user_std, width,
           color="#5b8def", edgecolor="black", linewidth=0.4,
           label="user CPU (std)")
    ax.bar(xpos - width / 2, sys_std, width, bottom=user_std,
           color="#1f4ea1", edgecolor="black", linewidth=0.4,
           label="sys CPU (std)")
    ax.bar(xpos + width / 2, user_bulk, width,
           color="#2ecc71", edgecolor="black", linewidth=0.4,
           label="user CPU (bulk)")
    ax.bar(xpos + width / 2, sys_bulk, width, bottom=user_bulk,
           color="#17703f", edgecolor="black", linewidth=0.4,
           label="sys CPU (bulk)")

    for i, (us, ss, ub, sb, ws, wb) in enumerate(
        zip(user_std, sys_std, user_bulk, sys_bulk, wall_std, wall_bulk)
    ):
        if ws > 0:
            ax.text(i - width / 2, us + ss + max(sys_std + sys_bulk) * 0.02,
                    f"wall {ws:.1f}s", ha="center", va="bottom", fontsize=7.5)
        ax.text(i + width / 2, ub + sb + max(sys_std + sys_bulk) * 0.02,
                f"wall {wb:.1f}s", ha="center", va="bottom", fontsize=7.5)

    ax.set_xticks(xpos)
    ax.set_xticklabels(targets, fontsize=9)
    ax.set_ylabel("CPU seconds", fontsize=9)
    ax.set_title("CPU time: std lstat vs getattrlistbulk", fontsize=10)
    ax.tick_params(axis="y", labelsize=8)
    ax.grid(axis="y", linestyle=":", color="#999", alpha=0.5)
    ax.set_axisbelow(True)
    ax.legend(loc="upper left", fontsize=8, frameon=False, ncol=2)

    fig.tight_layout()
    fig.savefig(FIG_DIR / "perf-cpu.pdf")
    plt.close(fig)


def plot_fallback_pass() -> None:
    """The r2c-fallback-perf micro-optimisation pass: user-CPU before/after."""
    # Before / after numbers from measurement-baseline.md
    targets = ["apfs-fastindex repo\n(9,124 entries)",
               "/Applications\n(163,667 entries)"]
    user_before = [21, 340]   # ms
    user_after  = [17, 227]
    sys_before  = [32, 535]
    sys_after   = [32, 515]
    thr_before  = [323_721, 172_283]
    thr_after   = [351_757, 200_512]

    fig, axes = plt.subplots(1, 2, figsize=(7.2, 3.2))

    # left: stacked bar of user+sys CPU before/after per target
    ax = axes[0]
    xpos = np.arange(len(targets))
    width = 0.36
    ax.bar(xpos - width / 2, user_before, width,
           color="#5b8def", edgecolor="black", linewidth=0.4, label="user (before)")
    ax.bar(xpos - width / 2, sys_before, width, bottom=user_before,
           color="#1f4ea1", edgecolor="black", linewidth=0.4, label="sys (before)")
    ax.bar(xpos + width / 2, user_after, width,
           color="#2ecc71", edgecolor="black", linewidth=0.4, label="user (after)")
    ax.bar(xpos + width / 2, sys_after, width, bottom=user_after,
           color="#17703f", edgecolor="black", linewidth=0.4, label="sys (after)")
    ax.set_xticks(xpos)
    ax.set_xticklabels(targets, fontsize=8)
    ax.set_ylabel("CPU (ms)", fontsize=9)
    ax.set_title("CPU before vs after micro-optimisation", fontsize=9)
    ax.tick_params(axis="y", labelsize=8)
    ax.grid(axis="y", linestyle=":", color="#999", alpha=0.5)
    ax.set_axisbelow(True)
    ax.legend(loc="upper left", fontsize=7, frameon=False, ncol=2)

    # right: throughput before/after
    ax = axes[1]
    xpos = np.arange(len(targets))
    width = 0.36
    bars_b = ax.bar(xpos - width / 2, thr_before, width,
                    color="#5b8def", edgecolor="black", linewidth=0.4, label="before")
    bars_a = ax.bar(xpos + width / 2, thr_after, width,
                    color="#2ecc71", edgecolor="black", linewidth=0.4, label="after")
    ax.set_xticks(xpos)
    ax.set_xticklabels(targets, fontsize=8)
    ax.set_ylabel("entries / second", fontsize=9)
    ax.set_title("Throughput before vs after", fontsize=9)
    ax.tick_params(axis="y", labelsize=8)
    ax.grid(axis="y", linestyle=":", color="#999", alpha=0.5)
    ax.set_axisbelow(True)
    ax.legend(loc="upper left", fontsize=8, frameon=False)
    for bar, value in zip(list(bars_b) + list(bars_a), thr_before + thr_after):
        ax.text(bar.get_x() + bar.get_width() / 2,
                bar.get_height() + max(thr_before + thr_after) * 0.02,
                f"{value/1000:,.0f}k", ha="center", va="bottom", fontsize=7)
    ax.set_ylim(0, max(thr_before + thr_after) * 1.18)

    fig.tight_layout()
    fig.savefig(FIG_DIR / "perf-fallback-pass.pdf")
    plt.close(fig)


def plot_parallel_scaling() -> None:
    """EX-25 parallel walker microbench: throughput and sys-CPU vs T."""
    threads = np.array([1, 2, 4, 8, 14])
    entries_per_sec = np.array([314_380, 517_437, 776_196, 609_748, 432_866])
    sys_total = np.array([0.508, 0.616, 0.819, 2.076, 4.717])
    sys_per_t = sys_total / threads
    speedup = entries_per_sec / entries_per_sec[0]

    fig, ax1 = plt.subplots(figsize=(7.2, 4.0))

    # Left axis: throughput
    color1 = "#2ecc71"
    ax1.plot(threads, entries_per_sec / 1000, marker="o", color=color1,
             linewidth=2.0, markersize=7, label="throughput (k entries/s)")
    ax1.set_xlabel("thread count T", fontsize=10)
    ax1.set_ylabel("throughput  (k entries / sec)", fontsize=10, color=color1)
    ax1.tick_params(axis="y", labelcolor=color1, labelsize=8)
    ax1.tick_params(axis="x", labelsize=9)
    ax1.set_xticks(threads)
    ax1.grid(axis="y", linestyle=":", color="#999", alpha=0.5)
    ax1.set_axisbelow(True)
    ax1.set_ylim(0, max(entries_per_sec / 1000) * 1.25)

    # Annotate T=4 as the optimum
    ax1.annotate("T=4 optimum\n2.47x of T=1",
                 xy=(4, entries_per_sec[2] / 1000),
                 xytext=(5.5, entries_per_sec[2] / 1000 + 80),
                 fontsize=8.5, ha="left",
                 arrowprops=dict(arrowstyle="->", color="#444", lw=0.8))

    # Annotate the contention regression for T=8 and T=14
    ax1.annotate("contention\nregresses",
                 xy=(14, entries_per_sec[4] / 1000),
                 xytext=(11, entries_per_sec[4] / 1000 - 130),
                 fontsize=8.5, ha="center", color="#c0392b",
                 arrowprops=dict(arrowstyle="->", color="#c0392b", lw=0.8))

    # Right axis: sys-CPU per thread
    ax2 = ax1.twinx()
    color2 = "#c0392b"
    ax2.plot(threads, sys_per_t, marker="s", color=color2,
             linewidth=1.8, markersize=6, linestyle="--",
             label="sys CPU per thread (s)")
    ax2.plot(threads, sys_total, marker="^", color="#8e44ad",
             linewidth=1.4, markersize=6, linestyle=":",
             label="total sys CPU (s)")
    ax2.set_ylabel("CPU seconds", fontsize=10, color=color2)
    ax2.tick_params(axis="y", labelcolor=color2, labelsize=8)
    ax2.set_ylim(0, max(sys_total) * 1.15)

    # Combined legend
    lines1, labels1 = ax1.get_legend_handles_labels()
    lines2, labels2 = ax2.get_legend_handles_labels()
    ax1.legend(lines1 + lines2, labels1 + labels2,
               loc="upper left", fontsize=8, frameon=False)
    ax1.set_title("EX-25 parallel walker on /Applications (~164k entries)",
                  fontsize=10)

    fig.tight_layout()
    fig.savefig(FIG_DIR / "perf-parallel.pdf")
    plt.close(fig)


if __name__ == "__main__":
    plot_throughput()
    plot_cpu_breakdown()
    plot_fallback_pass()
    plot_parallel_scaling()
    for name in ("perf-throughput.pdf", "perf-cpu.pdf",
                 "perf-fallback-pass.pdf", "perf-parallel.pdf"):
        print("wrote:", FIG_DIR / name)
