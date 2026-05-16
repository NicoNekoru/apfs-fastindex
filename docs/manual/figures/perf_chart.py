"""
Generate the performance figures used in chapter 12 of the manual.

Source: docs/implementation/measurement-baseline.md
Run:    python3 perf_chart.py
Output: perf-throughput.pdf, perf-cpu.pdf in the same directory.
"""

from __future__ import annotations

import pathlib

import matplotlib.pyplot as plt
import numpy as np

FIG_DIR = pathlib.Path(__file__).resolve().parent

# (target, backend, entries, wall_s, user_s, sys_s)
ROWS = [
    ("proof fixture",       "raw",             7,         0.23,  0.013, 0.015),
    ("apfs-fastindex repo", "fallback (std)",  9_124,     0.07,  0.015, 0.048),
    ("/Applications",       "fallback (std)",  163_667,   1.28,  0.410, 0.728),
    ("/Applications",       "fallback (bulk)", 163_667,   1.04,  0.347, 0.608),
    ("/Users",              "fallback (bulk)", 1_304_073, 26.65, 2.92,  6.62),
    ("/ (whole machine)",   "fallback (std)",  5_251_546, 129.6, 16.3,  47.8),
    ("/ (whole machine)",   "fallback (bulk)", 5_260_624, 108.7, 14.85, 29.80),
]

BACKEND_COLORS = {
    "raw":             "#7f7f7f",
    "fallback (std)":  "#5b8def",
    "fallback (bulk)": "#2ecc71",
}


def plot_throughput() -> None:
    """Horizontal bar chart of entries/sec per measurement row."""
    labels = [f"{target}\n[{backend}]" for target, backend, *_ in ROWS]
    throughput = [entries / wall for _, _, entries, wall, *_ in ROWS]
    colors = [BACKEND_COLORS[row[1]] for row in ROWS]

    fig, ax = plt.subplots(figsize=(7.0, 4.0))
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

    # legend by backend
    handles = [
        plt.Rectangle((0, 0), 1, 1, color=color, edgecolor="black", linewidth=0.4)
        for color in BACKEND_COLORS.values()
    ]
    ax.legend(handles, BACKEND_COLORS.keys(), loc="lower right", fontsize=8, frameon=False)

    fig.tight_layout()
    fig.savefig(FIG_DIR / "perf-throughput.pdf")
    plt.close(fig)


def plot_cpu_breakdown() -> None:
    """Wall-vs-CPU comparison for the two whole-machine / scan rows."""
    targets = ["/Applications", "/Users", "/ whole-machine"]
    std_rows = {row[0]: row for row in ROWS if row[1] == "fallback (std)"}
    bulk_rows = {row[0]: row for row in ROWS if row[1] == "fallback (bulk)"}

    keys = [("/Applications", "/Applications"), ("/Users", "/Users"),
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

    std_user_bar = ax.bar(xpos - width / 2, user_std, width,
                          color="#5b8def", edgecolor="black", linewidth=0.4,
                          label="user CPU (std)")
    ax.bar(xpos - width / 2, sys_std, width, bottom=user_std,
           color="#1f4ea1", edgecolor="black", linewidth=0.4,
           label="sys CPU (std)")
    bulk_user_bar = ax.bar(xpos + width / 2, user_bulk, width,
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


if __name__ == "__main__":
    plot_throughput()
    plot_cpu_breakdown()
    print("wrote:", FIG_DIR / "perf-throughput.pdf")
    print("wrote:", FIG_DIR / "perf-cpu.pdf")
