#!/usr/bin/env python3
"""EX-24: drive the standalone microbench across three configurations.

Compiles `microbench.rs` with `rustc -O` and runs it 5x for each of:

  - drec_only         (getattrlistbulk, name+objtype+fileid+devid+error only)
  - current_walker    (drec_only + ATTR_FILE_TOTALSIZE + ATTR_FILE_ALLOCSIZE)
  - fts               (BSD fts_open + fts_read traversal)

Records per-run JSON, computes per-config medians, and verdicts:

  - validated_vnode_cost_load_bearing  (drec_only > 1.3x current_walker)
  - vnode_cost_marginal                (drec_only within 1.1x of current_walker)
  - fts_wins_singlethread              (fts > current_walker by >5%)
  - oracle_inconclusive                (entry counts diverge by >1%)

Output goes to `generated/`. No sudo; runs as the invoking user.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import shutil
import statistics
import subprocess
import sys
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
TARGET = "/Applications"
RUNS_PER_CONFIG = 5
CONFIGS = ["drec_only", "current_walker", "fts"]


def run(cmd: list[str], **kwargs) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        **kwargs,
    )


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n"
    )


def environment() -> dict:
    sw_vers = run(["sw_vers"])
    rustc = run(["rustc", "--version"])
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(ARTIFACT_DIR),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "rustc": rustc.stdout.strip(),
        "sw_vers": sw_vers.stdout,
        "target": TARGET,
        "runs_per_config": RUNS_PER_CONFIG,
    }


def compile_microbench() -> Path:
    source = ARTIFACT_DIR / "microbench.rs"
    binary = GENERATED_DIR / "microbench.bin"
    if binary.exists():
        binary.unlink()
    proc = run(["rustc", "-O", str(source), "-o", str(binary)])
    if proc.returncode != 0:
        raise SystemExit(
            f"rustc -O failed:\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return binary


def run_config(binary: Path, config: str) -> list[dict]:
    runs: list[dict] = []
    for i in range(RUNS_PER_CONFIG):
        proc = run([str(binary), config, TARGET])
        if proc.returncode != 0:
            raise SystemExit(
                f"microbench {config} run {i} failed:\n"
                f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
            )
        try:
            doc = json.loads(proc.stdout.strip())
        except json.JSONDecodeError as err:
            raise SystemExit(f"microbench {config} run {i} returned non-JSON: {err}")
        runs.append(doc)
    return runs


def summarise(label: str, runs: list[dict]) -> dict:
    walls = [r["wall_seconds"] for r in runs]
    users = [r["user_seconds"] for r in runs]
    syss = [r["sys_seconds"] for r in runs]
    eps = [r["entries_per_second"] for r in runs]
    return {
        "config": label,
        "entries": runs[0]["entries"],
        "run_count": len(runs),
        "wall_median": statistics.median(walls),
        "wall_min": min(walls),
        "wall_max": max(walls),
        "user_median": statistics.median(users),
        "sys_median": statistics.median(syss),
        "entries_per_second_median": statistics.median(eps),
    }


def verdict(summaries: dict[str, dict]) -> tuple[str, str]:
    drec = summaries["drec_only"]
    cur = summaries["current_walker"]
    fts = summaries["fts"]
    # Consistency check: entry counts within 1%
    counts = [drec["entries"], cur["entries"], fts["entries"]]
    max_count = max(counts)
    min_count = min(counts)
    if max_count > 0 and (max_count - min_count) / max_count > 0.01:
        return (
            "oracle_inconclusive",
            f"entry counts diverge: drec={drec['entries']}, "
            f"current={cur['entries']}, fts={fts['entries']} (>1% drift)",
        )

    drec_eps = drec["entries_per_second_median"]
    cur_eps = cur["entries_per_second_median"]
    fts_eps = fts["entries_per_second_median"]

    drec_vs_cur = drec_eps / cur_eps if cur_eps else 0.0
    fts_vs_cur = fts_eps / cur_eps if cur_eps else 0.0

    details = (
        f"drec_only={drec_eps:.0f} ent/s, current_walker={cur_eps:.0f} ent/s "
        f"(ratio {drec_vs_cur:.2f}x); "
        f"fts={fts_eps:.0f} ent/s (vs current {fts_vs_cur:.2f}x)"
    )

    if drec_vs_cur >= 1.3:
        return ("validated_vnode_cost_load_bearing", details)
    if fts_vs_cur >= 1.05:
        return ("fts_wins_singlethread", details)
    if 0.9 <= drec_vs_cur <= 1.1:
        return ("vnode_cost_marginal", details)
    return ("partial_signal", details)


def main() -> int:
    write_json("environment.json", environment())

    if not Path(TARGET).is_dir():
        raise SystemExit(f"target {TARGET} not found or not a directory")

    binary = compile_microbench()

    summaries: dict[str, dict] = {}
    for config in CONFIGS:
        runs = run_config(binary, config)
        write_json(f"ex24-{config}-runs.json", {"runs": runs})
        summaries[config] = summarise(config, runs)

    summary_slug, detail = verdict(summaries)
    write_json(
        "summary.json",
        {
            "status": "executed",
            "verdict": summary_slug,
            "verdict_detail": detail,
            "summaries": summaries,
            "target": TARGET,
            "runs_per_config": RUNS_PER_CONFIG,
        },
    )
    print(f"Verdict: {summary_slug}")
    print(detail)
    print("Per-config medians:")
    for cfg in CONFIGS:
        s = summaries[cfg]
        print(
            f"  {cfg:18s}  {s['entries']:>7} entries  "
            f"{s['wall_median']:.3f}s wall  "
            f"{s['user_median']:.3f}s user  "
            f"{s['sys_median']:.3f}s sys  "
            f"{s['entries_per_second_median']:>8.0f} ent/s"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
