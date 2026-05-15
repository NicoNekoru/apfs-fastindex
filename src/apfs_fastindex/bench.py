"""Measurement harness for apfs-fastindex-scan.

Runs the Rust binary against one source path multiple times and reports
wall-clock time, peak resident-set size, and entries/sec for the
chosen mode (raw or fallback). The script is intentionally narrow: it
exists to give every future performance claim a reproducible baseline
on a known fixture, not to be a full benchmark suite.

Usage:

    PYTHONPATH=src python3 -m apfs_fastindex.bench --proof-fixture
    PYTHONPATH=src python3 -m apfs_fastindex.bench --target /path/to/scan
    PYTHONPATH=src python3 -m apfs_fastindex.bench \\
        --target /path/to/scan --mode fallback --repeat 5

Output is one JSON document per run plus a final aggregate.
"""

from __future__ import annotations

import argparse
import contextlib
import json
import resource
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Iterator

REPO_ROOT = Path(__file__).resolve().parents[2]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"


def _build_binary() -> Path:
    """Ensure the release binary is built and return its path.

    Cargo writes binaries under the workspace's `target/release/`, not the
    crate-local `target/`. We resolve the path explicitly so the harness
    keeps working whether the crate is built standalone or as part of a
    larger workspace later.
    """
    subprocess.run(
        ["cargo", "build", "--release", "--quiet", "--bin", "apfs-fastindex-scan"],
        cwd=str(RUST_CRATE_DIR),
        check=True,
    )
    candidates = [
        REPO_ROOT / "target" / "release" / "apfs-fastindex-scan",
        RUST_CRATE_DIR / "target" / "release" / "apfs-fastindex-scan",
    ]
    for candidate in candidates:
        if candidate.exists():
            return candidate
    raise RuntimeError(
        f"apfs-fastindex-scan binary not found in any of: {[str(c) for c in candidates]}"
    )


def _run_once(binary: Path, target: str, mode: str) -> dict:
    """Run the binary once and return per-run metrics."""
    args = [str(binary)]
    if mode != "auto":
        args.extend(["--mode", mode])
    args.append(target)

    before = resource.getrusage(resource.RUSAGE_CHILDREN)
    wall_start = time.monotonic()
    proc = subprocess.run(args, capture_output=True, text=True, check=False)
    wall_seconds = time.monotonic() - wall_start
    after = resource.getrusage(resource.RUSAGE_CHILDREN)

    if proc.returncode != 0:
        raise RuntimeError(
            f"apfs-fastindex-scan failed (rc={proc.returncode})\n"
            f"stderr:\n{proc.stderr}\nstdout head:\n{proc.stdout[:1000]}"
        )

    try:
        doc = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        raise RuntimeError(f"failed to parse Rust JSON: {exc}\nstdout head:\n{proc.stdout[:1000]}")
    parser_output = doc.get("parser_output", {})
    entries = parser_output.get("entries", [])
    aggregates = parser_output.get("aggregates", [])

    # RUSAGE_CHILDREN gives cumulative max-RSS across all children, so we
    # peek at the after-before delta. On macOS ru_maxrss is in bytes; on
    # Linux it's in KB. We label units explicitly in the report.
    rss_delta_bytes = after.ru_maxrss - before.ru_maxrss
    cpu_user = after.ru_utime - before.ru_utime
    cpu_sys = after.ru_stime - before.ru_stime
    return {
        "wall_seconds": wall_seconds,
        "cpu_user_seconds": cpu_user,
        "cpu_sys_seconds": cpu_sys,
        "peak_child_rss_bytes_delta_macos_or_kb_linux": rss_delta_bytes,
        "entry_count": len(entries),
        "aggregate_count": len(aggregates),
        "stderr_head": proc.stderr[:500],
    }


def _summarize(per_run: list[dict]) -> dict:
    walls = [r["wall_seconds"] for r in per_run]
    user = [r["cpu_user_seconds"] for r in per_run]
    sysc = [r["cpu_sys_seconds"] for r in per_run]
    rss = [r["peak_child_rss_bytes_delta_macos_or_kb_linux"] for r in per_run]
    entries = per_run[-1]["entry_count"] if per_run else 0
    median_wall = statistics.median(walls)
    return {
        "runs": len(per_run),
        "entries": entries,
        "wall_seconds_min": min(walls),
        "wall_seconds_median": median_wall,
        "wall_seconds_max": max(walls),
        "wall_seconds_mean": statistics.fmean(walls),
        "cpu_user_seconds_median": statistics.median(user),
        "cpu_sys_seconds_median": statistics.median(sysc),
        "peak_child_rss_median": statistics.median(rss),
        "entries_per_second_median": entries / median_wall if median_wall > 0 else None,
    }


@contextlib.contextmanager
def _maybe_proof_fixture(use_fixture: bool, mode: str) -> Iterator[tuple[str, str]]:
    """Yield (target_path, mode) suitable for the requested combination.

    If `use_fixture` is true, the EX-13 proof fixture is built. Fallback mode
    against the fixture mounts the image and yields the mountpoint;
    raw mode yields the detached `.dmg` path.
    """
    from .poc_fixture import build_proof_fixture
    import plistlib

    with build_proof_fixture() as fixture:
        if mode == "raw" or mode == "auto":
            yield (str(fixture.image_path), "raw" if mode == "auto" else mode)
            return
        # Fallback mode: re-mount the image for the duration of the run.
        mountpoint = fixture.image_path.parent / "bench-mnt"
        mountpoint.mkdir(exist_ok=True)
        attach = subprocess.run(
            [
                "hdiutil",
                "attach",
                "-plist",
                "-mountpoint",
                str(mountpoint),
                str(fixture.image_path),
            ],
            capture_output=True,
            text=True,
            check=True,
        )
        info = plistlib.loads(attach.stdout.encode())
        detach_dev = info["system-entities"][0]["dev-entry"]
        try:
            yield (str(mountpoint), "fallback")
        finally:
            subprocess.run(["hdiutil", "detach", detach_dev], capture_output=True)


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--target",
        help="explicit source path; pairs with --mode raw|fallback|auto",
    )
    parser.add_argument(
        "--proof-fixture",
        action="store_true",
        help="build and use the EX-13 proof fixture instead of --target",
    )
    parser.add_argument(
        "--mode",
        default="auto",
        choices=("auto", "raw", "fallback"),
        help="mode forwarded to apfs-fastindex-scan (default auto)",
    )
    parser.add_argument(
        "--repeat",
        type=int,
        default=3,
        help="how many times to run the binary (default 3)",
    )
    args = parser.parse_args(argv)
    if not args.target and not args.proof_fixture:
        parser.error("either --target PATH or --proof-fixture is required")
    if args.target and args.proof_fixture:
        parser.error("--target and --proof-fixture are mutually exclusive")

    binary = _build_binary()

    if args.proof_fixture:
        ctx = _maybe_proof_fixture(True, args.mode)
    else:
        @contextlib.contextmanager
        def passthrough():
            yield (args.target, args.mode)

        ctx = passthrough()

    per_run: list[dict] = []
    with ctx as (target, mode):
        for _ in range(args.repeat):
            per_run.append(_run_once(binary, target, mode))
        report = {
            "target": target,
            "mode": mode,
            "per_run": per_run,
            "aggregate": _summarize(per_run),
        }
        print(json.dumps(report, indent=2, sort_keys=True))
    return 0


if __name__ == "__main__":
    sys.exit(main())
