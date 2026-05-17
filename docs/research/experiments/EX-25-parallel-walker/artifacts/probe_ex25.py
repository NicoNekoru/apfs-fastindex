#!/usr/bin/env python3
"""EX-25: sweep T ∈ {1, 2, 4, 8, num_cpus} on /Applications and
verdict the scaling envelope.

Compiles parallel_microbench.rs with rustc -O and runs it 5x for
each thread count, sleeping 200 ms between runs.

Verdict ladder (per the EX-25 README):
  T=4 entries/sec >= 2.0x T=1 AND T=4 sys/thread <= 1.2x T=1 sys
       -> validated_parallel_scaling                   (default 4)
  T=2 entries/sec >= 1.4x T=1 AND no higher T improves
       -> partial_scaling_then_plateau                  (default 2)
  max-T speedup <= 1.1x T=1
       -> single_thread_optimal                         (no change)
  T=num_cpus < T=1
       -> pathological_contention                       (no change + warn)
  else -> partial_signal
"""

from __future__ import annotations

import datetime as _dt
import json
import platform
import statistics
import subprocess
import time
from pathlib import Path

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
TARGET = "/Applications"
RUNS_PER_T = 5
SLEEP_BETWEEN_RUNS = 0.2


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


def sysctl_int(name: str) -> int | None:
    proc = run(["sysctl", "-n", name])
    if proc.returncode != 0:
        return None
    try:
        return int(proc.stdout.strip())
    except ValueError:
        return None


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
        "runs_per_thread_count": RUNS_PER_T,
        "hw_physicalcpu": sysctl_int("hw.physicalcpu"),
        "hw_logicalcpu": sysctl_int("hw.logicalcpu"),
        "hw_perflevel0_physicalcpu": sysctl_int("hw.perflevel0.physicalcpu"),
        "hw_perflevel1_physicalcpu": sysctl_int("hw.perflevel1.physicalcpu"),
    }


def compile_microbench() -> Path:
    source = ARTIFACT_DIR / "parallel_microbench.rs"
    binary = GENERATED_DIR / "parallel_microbench.bin"
    if binary.exists():
        binary.unlink()
    proc = run(["rustc", "-O", str(source), "-o", str(binary)])
    if proc.returncode != 0:
        raise SystemExit(
            f"rustc -O failed:\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return binary


def run_t(binary: Path, t: int) -> list[dict]:
    runs: list[dict] = []
    for i in range(RUNS_PER_T):
        proc = run([str(binary), str(t), TARGET])
        if proc.returncode != 0:
            raise SystemExit(
                f"parallel_microbench T={t} run {i} failed:\n"
                f"stdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
            )
        try:
            doc = json.loads(proc.stdout.strip())
        except json.JSONDecodeError as err:
            raise SystemExit(
                f"parallel_microbench T={t} run {i} returned non-JSON: {err}"
            )
        runs.append(doc)
        time.sleep(SLEEP_BETWEEN_RUNS)
    return runs


def summarise(t: int, runs: list[dict]) -> dict:
    walls = [r["wall_seconds"] for r in runs]
    users = [r["user_seconds"] for r in runs]
    syss = [r["sys_seconds"] for r in runs]
    sys_per = [r["sys_per_thread"] for r in runs]
    eps = [r["entries_per_second"] for r in runs]
    return {
        "threads": t,
        "entries": runs[0]["entries"],
        "run_count": len(runs),
        "wall_median": statistics.median(walls),
        "wall_min": min(walls),
        "wall_max": max(walls),
        "user_median": statistics.median(users),
        "sys_median": statistics.median(syss),
        "sys_per_thread_median": statistics.median(sys_per),
        "entries_per_second_median": statistics.median(eps),
    }


def verdict(summaries: list[dict]) -> tuple[str, str]:
    by_t = {s["threads"]: s for s in summaries}
    t1 = by_t.get(1)
    if t1 is None:
        return ("oracle_inconclusive", "no T=1 baseline")

    # Consistency check: entry counts within 1% across all T.
    counts = [s["entries"] for s in summaries]
    if counts and max(counts) > 0:
        spread = (max(counts) - min(counts)) / max(counts)
        if spread > 0.01:
            return (
                "oracle_inconclusive",
                f"entry counts diverge by {spread:.2%} across T",
            )

    eps_1 = t1["entries_per_second_median"]
    sys_1 = t1["sys_median"]

    def speedup(t: int) -> float:
        s = by_t.get(t)
        if not s or eps_1 <= 0:
            return 0.0
        return s["entries_per_second_median"] / eps_1

    def sys_per_thread_ratio(t: int) -> float:
        s = by_t.get(t)
        if not s or sys_1 <= 0:
            return 0.0
        # sys_per_thread is total sys / T; ratio against single-thread sys.
        return s["sys_per_thread_median"] / sys_1

    speedups = {t: speedup(t) for t in sorted(by_t)}
    sys_ratios = {t: sys_per_thread_ratio(t) for t in sorted(by_t)}
    max_t = max(by_t)
    max_speedup = max(speedups.values())

    details = (
        "speedups: "
        + ", ".join(f"T={t}:{v:.2f}x" for t, v in speedups.items())
        + " | sys/T ratios: "
        + ", ".join(f"T={t}:{v:.2f}x" for t, v in sys_ratios.items())
    )

    # pathological_contention: T=num_cpus catastrophically worse
    if speedups.get(max_t, 1.0) < 0.9:
        return ("pathological_contention", details)

    # validated_parallel_scaling: T=4 >= 2.0x with sys/T discipline
    if speedups.get(4, 0.0) >= 2.0 and sys_ratios.get(4, 999.0) <= 1.2:
        return ("validated_parallel_scaling", details)

    # partial_scaling_then_plateau: T=2 >= 1.4x and no higher T pays
    if speedups.get(2, 0.0) >= 1.4 and speedups.get(4, 0.0) < speedups.get(2, 0.0) * 1.1:
        return ("partial_scaling_then_plateau", details)

    if max_speedup <= 1.1:
        return ("single_thread_optimal", details)

    return ("partial_signal", details)


def main() -> int:
    write_json("environment.json", environment())
    if not Path(TARGET).is_dir():
        raise SystemExit(f"target {TARGET} not found or not a directory")

    binary = compile_microbench()

    logical_cpu = sysctl_int("hw.logicalcpu") or 8
    physical_cpu = sysctl_int("hw.physicalcpu") or logical_cpu

    t_sweep = sorted({1, 2, 4, 8, physical_cpu, logical_cpu})

    summaries: list[dict] = []
    for t in t_sweep:
        runs = run_t(binary, t)
        write_json(f"ex25-t{t:02d}-runs.json", {"runs": runs})
        summaries.append(summarise(t, runs))

    v_slug, detail = verdict(summaries)
    write_json(
        "summary.json",
        {
            "status": "executed",
            "verdict": v_slug,
            "verdict_detail": detail,
            "summaries": summaries,
            "target": TARGET,
            "t_sweep": t_sweep,
            "runs_per_t": RUNS_PER_T,
            "physical_cpu": physical_cpu,
            "logical_cpu": logical_cpu,
        },
    )
    print(f"Verdict: {v_slug}")
    print(detail)
    print("Per-T medians:")
    for s in summaries:
        print(
            f"  T={s['threads']:>2}  entries={s['entries']:>7}  "
            f"wall={s['wall_median']:.3f}s  "
            f"user={s['user_median']:.3f}s  "
            f"sys={s['sys_median']:.3f}s  "
            f"sys/T={s['sys_per_thread_median']:.3f}s  "
            f"{s['entries_per_second_median']:>8.0f} ent/s"
        )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
