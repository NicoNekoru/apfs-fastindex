#!/usr/bin/env python3
"""EX-30: production fallback-walker baseline on `/Users/kai`.

Drives the same `apfs-fastindex-scan` binary the GUI ships
(`--format msgpack --threads 0`, no --cross-mounts), running
1 cold + 4 warm iterations. Captures wall time, output bytes,
and entry/aggregate counts from the msgpack header.

Writes `artifacts/generated/ex30_baseline_<date>.json` so
historical runs are preserved. The latest run is the baseline
that R4 (persistent cache) is measured against.

Usage (from repo root):
  python3 docs/research/experiments/EX-30-perf-baseline/\
artifacts/probe_ex30.py /Users/kai

Defaults to `/Users/kai` if no path argument given. Set
`SCAN_TARGET` env var to override.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ARTIFACT_DIR = Path(__file__).resolve().parent
GEN_DIR = ARTIFACT_DIR / "generated"
GEN_DIR.mkdir(parents=True, exist_ok=True)

REPO_ROOT = ARTIFACT_DIR.parents[4]
CLI = REPO_ROOT / "target" / "release" / "apfs-fastindex-scan"

DEFAULT_TARGET = "/Users/kai"
WARM_ITERATIONS = 4
INTER_ITER_SLEEP_S = 0.2


def _ensure_built() -> None:
    if not CLI.exists():
        print(f"Building release binary: cargo build -p apfs-fastindex --release --bin apfs-fastindex-scan", file=sys.stderr)
        rc = subprocess.run(
            ["cargo", "build", "-p", "apfs-fastindex", "--release", "--bin", "apfs-fastindex-scan"],
            cwd=REPO_ROOT,
        ).returncode
        if rc != 0:
            raise SystemExit(f"cargo build failed (rc={rc})")
    if not CLI.exists():
        raise SystemExit(f"CLI not found after build: {CLI}")


def _purge_caches() -> bool:
    """Drop the OS file-cache so the next read is cold. Requires
    sudo. If sudo isn't cached and we can't prompt, the cold
    iteration is effectively warm — record but flag in output."""
    try:
        rc = subprocess.run(
            ["sudo", "-n", "purge"],
            stderr=subprocess.DEVNULL,
            timeout=30,
        ).returncode
        return rc == 0
    except Exception:
        return False


def _run_once(target: str) -> dict:
    """One CLI invocation. Returns timing + output stats."""
    with tempfile.NamedTemporaryFile(suffix=".msgpack", delete=False) as tmp:
        out_path = tmp.name
    try:
        t0 = time.monotonic()
        # No --threads arg = the CLI's own default
        # (min(hw.physicalcpu, 4) per EX-25 verdict). That
        # matches what the GUI sends when its threads pref is
        # at the "auto" sentinel.
        #
        # Capture stdout (the msgpack output) to a file so we
        # measure the full production-shape cost — serialise +
        # write to disk, the same work the GUI's subprocess
        # wrapper does. Then parse the msgpack header to read
        # entry/aggregate counts.
        proc = subprocess.run(
            [
                str(CLI),
                "--format", "msgpack",
                target,
            ],
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            timeout=600,
        )
        wall_s = time.monotonic() - t0
        out_bytes = len(proc.stdout)
        stderr_text = proc.stderr.decode("utf-8", errors="replace")
        # Parse the msgpack to read entry / aggregate counts.
        entries = 0
        aggregates = 0
        if proc.stdout:
            try:
                import msgpack  # type: ignore[import-not-found]
                data = msgpack.unpackb(proc.stdout, raw=False, strict_map_key=False)
                po = data.get("parser_output", {})
                entries = len(po.get("entries", []))
                aggregates = len(po.get("aggregates", []))
            except ImportError:
                # msgpack not installed; fall back to lengths.
                pass
            except Exception as e:
                stderr_text += f"\n(msgpack parse failed: {e})"
        return {
            "wall_s": wall_s,
            "exit": proc.returncode,
            "stdout_bytes": out_bytes,
            "entries": entries,
            "aggregates": aggregates,
            "entries_per_sec": (entries / wall_s) if wall_s > 0 else 0.0,
            "stderr_tail": "\n".join(stderr_text.splitlines()[-5:]),
        }
    finally:
        try:
            os.unlink(out_path)
        except FileNotFoundError:
            pass


def _physical_cpu_count() -> int:
    try:
        return int(subprocess.check_output(["sysctl", "-n", "hw.physicalcpu"]).strip())
    except Exception:
        return os.cpu_count() or 1


def main() -> int:
    _ensure_built()

    target = os.environ.get("SCAN_TARGET", sys.argv[1] if len(sys.argv) > 1 else DEFAULT_TARGET)
    if not Path(target).is_dir():
        raise SystemExit(f"SCAN_TARGET not a directory: {target}")

    host = {
        "platform": platform.platform(),
        "kernel": platform.release(),
        "physical_cpus": _physical_cpu_count(),
        "python": platform.python_version(),
    }

    print(f"=== EX-30 baseline on {target} ===")
    print(f"Host: {host['platform']}, {host['physical_cpus']} physical cores")
    print(f"CLI:  {CLI}")

    print("\n[cold] purging caches…")
    purge_ok = _purge_caches()
    if not purge_ok:
        print("    sudo purge failed (no cached creds?); cold run is degenerate", file=sys.stderr)

    print("[cold] iteration 1…")
    cold = _run_once(target)
    print(f"  wall_s={cold['wall_s']:.2f}  entries={cold['entries']}  "
          f"entries/sec={cold['entries_per_sec']:.0f}  exit={cold['exit']}")
    if cold["exit"] != 0:
        print(f"  stderr tail: {cold['stderr_tail']}")
        raise SystemExit("cold iteration failed")

    warm_runs: list[dict] = []
    for i in range(1, WARM_ITERATIONS + 1):
        time.sleep(INTER_ITER_SLEEP_S)
        print(f"[warm] iteration {i}…")
        w = _run_once(target)
        print(f"  wall_s={w['wall_s']:.2f}  entries={w['entries']}  "
              f"entries/sec={w['entries_per_sec']:.0f}  exit={w['exit']}")
        if w["exit"] != 0:
            print(f"  stderr tail: {w['stderr_tail']}")
            raise SystemExit(f"warm iteration {i} failed")
        warm_runs.append(w)

    warm_walls = [w["wall_s"] for w in warm_runs]
    warm_eps = [w["entries_per_sec"] for w in warm_runs]
    warm_summary = {
        "wall_s_median": statistics.median(warm_walls),
        "wall_s_min": min(warm_walls),
        "wall_s_max": max(warm_walls),
        "wall_s_stdev": statistics.stdev(warm_walls) if len(warm_walls) > 1 else 0.0,
        "entries_per_sec_median": statistics.median(warm_eps),
        "iterations": len(warm_runs),
    }

    cold_over_warm = (cold["wall_s"] / warm_summary["wall_s_median"]) if warm_summary["wall_s_median"] > 0 else 0.0

    verdict = "production_baseline_recorded"
    if cold["entries"] == 0 or warm_summary["wall_s_median"] == 0:
        verdict = "harness_failure"

    record = {
        "experiment": "EX-30",
        "title": "Production fallback-walker performance baseline",
        "date": _dt.date.today().isoformat(),
        "host": host,
        "target": target,
        "cli": str(CLI),
        "verdict": verdict,
        "purge_succeeded": purge_ok,
        "cold": cold,
        "warm": {
            "runs": warm_runs,
            "summary": warm_summary,
        },
        "cold_over_warm_ratio": cold_over_warm,
    }

    out_file = GEN_DIR / f"ex30_baseline_{_dt.date.today().isoformat()}.json"
    out_file.write_text(json.dumps(record, indent=2))
    print(f"\n=== Summary ===")
    print(f"  cold wall:        {cold['wall_s']:.2f} s ({cold['entries_per_sec']:.0f} ent/s)")
    print(f"  warm median:      {warm_summary['wall_s_median']:.2f} s ({warm_summary['entries_per_sec_median']:.0f} ent/s)")
    print(f"  cold/warm:        {cold_over_warm:.2f}× (size of the cache lever)")
    print(f"  verdict:          {verdict}")
    print(f"  output:           {out_file}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
