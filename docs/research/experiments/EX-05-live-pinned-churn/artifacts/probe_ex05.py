#!/usr/bin/env python3
"""Run EX-05: live mounted image raw reads under write churn.

This probe intentionally distinguishes "can raw-read a mounted lab image" from
"can pin and replay one historical XID while writes continue." The current
go-apfs proof walker always resolves latest visible state, so a successful raw
walk during churn is not proof of pinned live-scan correctness.
"""

from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import struct
import subprocess
import tempfile
import threading
import time
from pathlib import Path


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
F_FULLFSYNC = 51
NX_MAGIC = 0x4253584E
ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
RAW_WALK_DIR = (
    ARTIFACT_DIR
    / "../../EX-03-pinned-state-raw-vs-oracle/artifacts/rawwalk"
).resolve()
GENERATED_DIR.mkdir(exist_ok=True)


def run(cmd: list[str], input_text: str | None = None, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        input=input_text,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_checked(cmd: list[str], input_text: str | None = None, cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    proc = run(cmd, input_text=input_text, cwd=cwd)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def attach_image(image_path: Path, mountpoint: Path) -> dict:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)]
    )
    return plistlib.loads(proc.stdout.encode("utf-8"))


def detach_device(dev_entry: str) -> None:
    run(["hdiutil", "detach", dev_entry])


def sync_file(path: Path) -> None:
    with path.open("ab") as handle:
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass


def sync_directory(path: Path) -> None:
    dirfd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)


def settle() -> None:
    run(["sync"])
    time.sleep(0.15)


def latest_checkpoint_info(raw_container_path: str) -> dict:
    with open(raw_container_path, "rb", buffering=0) as handle:
        block0 = handle.read(4096)
        block_size = struct.unpack_from("<I", block0, 0x24)[0]
        desc_blocks = struct.unpack_from("<I", block0, 0x68)[0]
        desc_base_raw = struct.unpack_from("<Q", block0, 0x70)[0]
        desc_base = desc_base_raw & ((1 << 63) - 1)
        non_contiguous = bool(desc_base_raw >> 63)

        candidates = []
        highest_xid = None
        for index in range(desc_blocks):
            handle.seek((desc_base + index) * block_size)
            block = handle.read(block_size)
            if len(block) < block_size:
                continue
            magic = struct.unpack_from("<I", block, 0x20)[0]
            if magic != NX_MAGIC:
                continue
            xid = struct.unpack_from("<Q", block, 0x10)[0]
            candidates.append(
                {
                    "descriptor_index": index,
                    "xid": xid,
                    "obj_type": struct.unpack_from("<I", block, 0x18)[0],
                }
            )
            highest_xid = xid if highest_xid is None or xid > highest_xid else highest_xid

    return {
        "block_size": block_size,
        "descriptor_blocks": desc_blocks,
        "descriptor_base": desc_base,
        "descriptor_base_non_contiguous": non_contiguous,
        "highest_xid": highest_xid,
        "candidate_count": len(candidates),
        "candidates": candidates,
    }


def snapshot_tree(root: Path) -> list[dict]:
    entries: list[dict] = []
    for current_root, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        rel_root = Path(current_root).relative_to(root)
        if str(rel_root).startswith(".fseventsd"):
            continue
        dirnames[:] = [name for name in dirnames if name != ".fseventsd"]
        current_stat = os.lstat(current_root)
        entries.append(
            {
                "type": "dir",
                "path": "." if str(rel_root) == "." else str(rel_root),
                "inode": current_stat.st_ino,
                "nlink": current_stat.st_nlink,
            }
        )
        for name in filenames:
            path = Path(current_root) / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            if path.is_symlink():
                entry_type = "symlink"
                symlink_target = os.readlink(path)
                logical_size = len(symlink_target)
            else:
                entry_type = "file"
                symlink_target = None
                logical_size = st.st_size
            entries.append(
                {
                    "type": entry_type,
                    "path": str(rel_path),
                    "inode": st.st_ino,
                    "nlink": st.st_nlink,
                    "logical_size": logical_size,
                    "symlink_target": symlink_target,
                }
            )
    return entries


def normalize_oracle_entry(entry: dict) -> dict:
    normalized = {"type": entry["type"], "file_id": entry["inode"]}
    if entry["type"] in {"file", "symlink"}:
        normalized["logical_size"] = entry["logical_size"]
    if entry["type"] == "symlink":
        normalized["symlink_target"] = entry["symlink_target"]
    return normalized


def normalize_raw_entry(entry: dict) -> dict:
    normalized = {"type": entry["type"], "file_id": entry["file_id"]}
    if entry["type"] in {"file", "symlink"}:
        normalized["logical_size"] = entry.get("logical_size", 0)
    if entry["type"] == "symlink":
        normalized["symlink_target"] = entry.get("symlink_target")
    return normalized


def compare_entries(oracle_entries: list[dict], raw_entries: list[dict]) -> dict:
    oracle_index = {
        entry["path"]: normalize_oracle_entry(entry)
        for entry in oracle_entries
        if entry["path"] != "."
    }
    raw_index = {entry["path"]: normalize_raw_entry(entry) for entry in raw_entries}
    missing_paths = sorted(path for path in oracle_index if path not in raw_index)
    unexpected_paths = sorted(path for path in raw_index if path not in oracle_index)
    mismatches = [
        {"path": path, "oracle": oracle_index[path], "raw": raw_index[path]}
        for path in sorted(set(oracle_index) & set(raw_index))
        if oracle_index[path] != raw_index[path]
    ]
    return {
        "oracle_path_count": len(oracle_index),
        "raw_path_count": len(raw_index),
        "missing_paths": missing_paths[:50],
        "unexpected_paths": unexpected_paths[:50],
        "missing_count": len(missing_paths),
        "unexpected_count": len(unexpected_paths),
        "mismatch_count": len(mismatches),
        "mismatches": mismatches[:50],
        "matched": not missing_paths and not unexpected_paths and not mismatches,
    }


def build_seed(root: Path, count: int = 300) -> list[str]:
    operations = []
    stable = root / "stable"
    hot = root / "hot"
    stable.mkdir()
    hot.mkdir()
    sync_directory(root)
    for index in range(count):
        path = stable / f"seed-{index:04d}.txt"
        path.write_text(f"seed {index}\n")
        if index % 50 == 0:
            sync_file(path)
    sync_directory(stable)
    settle()
    operations.append(f"created {count} stable seed files")
    return operations


def run_rawwalk(raw_container_path: str) -> dict:
    proc = run_checked(
        ["go", "run", ".", "--device", raw_container_path],
        cwd=RAW_WALK_DIR,
    )
    return json.loads(proc.stdout)


def run_rawwalk_while_mutating(raw_container_path: str, root: Path, mutation_count: int) -> dict:
    mutations: list[dict] = []
    ready = threading.Event()

    def mutate() -> None:
        ready.wait(timeout=2)
        hot = root / "hot"
        for index in range(mutation_count):
            path = hot / f"churn-{index:04d}.txt"
            path.write_text(f"churn {index}\n")
            if index % 3 == 0:
                sync_file(path)
            if index % 5 == 0:
                sync_directory(hot)
                checkpoint = latest_checkpoint_info(raw_container_path)
                mutations.append(
                    {
                        "index": index,
                        "path": str(path.relative_to(root)),
                        "highest_xid": checkpoint["highest_xid"],
                    }
                )
            time.sleep(0.01)
        sync_directory(hot)
        settle()

    thread = threading.Thread(target=mutate, name="ex05-mutator")
    thread.start()
    start_checkpoint = latest_checkpoint_info(raw_container_path)
    ready.set()
    start = time.monotonic()
    error = None
    raw_walk = None
    try:
        raw_walk = run_rawwalk(raw_container_path)
    except Exception as exc:  # pragma: no cover - experiment path
        error = f"{type(exc).__name__}: {exc}"
    elapsed_seconds = time.monotonic() - start
    thread.join()
    end_checkpoint = latest_checkpoint_info(raw_container_path)
    return {
        "start_checkpoint": start_checkpoint,
        "end_checkpoint": end_checkpoint,
        "elapsed_seconds": elapsed_seconds,
        "mutations": mutations,
        "raw_walk": raw_walk,
        "error": error,
    }


def summarize(result: dict) -> dict:
    baseline = result["baseline"]
    live = result["live_scan"]
    final = result["final"]
    live_raw = live.get("raw_walk") or {"entries": []}
    live_vs_baseline = compare_entries(baseline["oracle"], live_raw["entries"]) if live_raw["entries"] else None
    live_vs_final = compare_entries(final["oracle"], live_raw["entries"]) if live_raw["entries"] else None
    unique_mutation_xids = sorted({item["highest_xid"] for item in live["mutations"]})
    return {
        "baseline_xid": baseline["checkpoint"]["highest_xid"],
        "live_start_xid": live["start_checkpoint"]["highest_xid"],
        "live_end_xid": live["end_checkpoint"]["highest_xid"],
        "final_xid": final["checkpoint"]["highest_xid"],
        "mutation_sample_xids": unique_mutation_xids,
        "raw_walk_error": live["error"],
        "raw_walk_elapsed_seconds": live["elapsed_seconds"],
        "raw_walk_entry_count": len(live_raw["entries"]),
        "baseline_entry_count": len(baseline["oracle"]),
        "final_entry_count": len(final["oracle"]),
        "live_raw_matches_baseline": live_vs_baseline["matched"] if live_vs_baseline else False,
        "live_raw_matches_final": live_vs_final["matched"] if live_vs_final else False,
        "live_vs_baseline": live_vs_baseline,
        "live_vs_final": live_vs_final,
        "interpretation": (
            "The mounted image was raw-readable during churn, but this run did not "
            "prove historical-XID pinning because the current raw walker resolves "
            "latest visible state. A supported live mode still needs a resolver "
            "that enforces the selected scan XID or a stable snapshot/API oracle."
        ),
    }


def run_experiment() -> dict:
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex05-", dir="/tmp"))
    image_path = base / "ex05-live-pinned-churn.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_entities: list[dict] = []
    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "256m",
                "-fs",
                "APFS",
                "-volname",
                "EX05Live",
                "-nospotlight",
                str(image_path),
            ]
        )
        attach_info = attach_image(image_path, mountpoint)
        mounted_entities = attach_info["system-entities"]
        container_dev = next(
            entity["dev-entry"]
            for entity in mounted_entities
            if entity.get("content-hint") == APFS_CONTAINER_HINT
        )
        raw_container_path = "/dev/r" + container_dev.split("/dev/")[1]
        operations = build_seed(mountpoint)
        baseline_oracle = snapshot_tree(mountpoint)
        baseline_checkpoint = latest_checkpoint_info(raw_container_path)
        live_scan = run_rawwalk_while_mutating(raw_container_path, mountpoint, mutation_count=80)
        final_oracle = snapshot_tree(mountpoint)
        final_checkpoint = latest_checkpoint_info(raw_container_path)
        return {
            "image_name": image_path.name,
            "raw_container_path": raw_container_path,
            "operations": operations,
            "baseline": {
                "checkpoint": baseline_checkpoint,
                "oracle": baseline_oracle,
            },
            "live_scan": live_scan,
            "final": {
                "checkpoint": final_checkpoint,
                "oracle": final_oracle,
            },
        }
    finally:
        if mounted_entities:
            detach_device(mounted_entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)


def main() -> None:
    environment = {
        "sw_vers": run_checked(["sw_vers"]).stdout,
        "go_version": run_checked(["go", "version"]).stdout.strip(),
        "raw_walk_module": {
            "path": str(RAW_WALK_DIR),
            "go_mod": (RAW_WALK_DIR / "go.mod").read_text(),
        },
        "important_limitation": (
            "The go-apfs proof walker resolves latest state; it is not a historical-XID "
            "pinned walker."
        ),
    }
    write_json("environment.json", environment)
    run_checked(["go", "mod", "tidy"], cwd=RAW_WALK_DIR)
    result = run_experiment()
    summary = summarize(result)
    write_json("baseline-oracle.json", result["baseline"]["oracle"])
    write_json("final-oracle.json", result["final"]["oracle"])
    write_json("live-raw-walk.json", result["live_scan"].get("raw_walk"))
    write_json("summary.json", summary)
    write_json("run.json", result)


if __name__ == "__main__":
    main()
