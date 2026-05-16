#!/usr/bin/env python3
"""EX-23: snapshot shape parity for the existing fallback walker.

Best-effort probe per SR-020. Never runs sudo. The probe:

  1. Enumerates the snapshot inventory of every reachable APFS
     volume via `tmutil listlocalsnapshots <mount>` and
     `diskutil apfs listSnapshots -plist <volume-device>`. Skips
     sealed-system OS-update snapshots
     (`com.apple.os.update-*`) per SR-020.
  2. Scans `mount(8)` output for any line that mentions a
     snapshot in its options string. macOS reports
     `mount_apfs` snapshot mounts with the snapshot name in the
     mount-options text.
  3. For every pre-mounted snapshot whose live mountpoint is
     also user-readable, runs the Rust fallback scanner
     (`apfs-fastindex-scan --mode fallback`) against both paths
     and diffs the `(entry_kind, file_id, logical_size,
     symlink_target)` tuple per path on the intersection of
     paths that exist in both walks.
  4. Writes a verdict slug to `summary.json`:

       - `validated_snapshot_shape_parity` (all unchanged-path
         tuples matched on at least one pair).
       - `shape_divergence` (at least one mismatch).
       - `blocked_no_mounted_user_snapshot` (snapshots exist on
         the host but none is already mounted at a user-readable
         path; a `sudo mount_apfs -s ...` reproducer is saved per
         snapshot).
       - `blocked_no_snapshots_at_all` (no snapshots at all on
         any reachable APFS volume).
       - `oracle_inconclusive` (an unrecoverable error before
         the diff step).

The Rust crate's --mode is the existing `fallback` source class
and is unchanged by this probe.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import plistlib
import shutil
import subprocess
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
REPO_ROOT = ARTIFACT_DIR.parents[4]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"

SEALED_SYSTEM_SNAPSHOT_PREFIX = "com.apple.os.update-"


def run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(
        json.dumps(payload, indent=2, sort_keys=True, default=_json_default) + "\n"
    )


def _json_default(obj: Any) -> Any:
    if isinstance(obj, bytes):
        return obj.hex()
    if isinstance(obj, Path):
        return str(obj)
    raise TypeError(f"unserializable: {type(obj).__name__}")


# ---- enumeration helpers ----------------------------------------------- #

def list_apfs_mounts() -> list[dict]:
    """Return `{device, mountpoint, fs_type, options}` for every APFS
    mount the kernel currently exposes."""
    proc = run(["mount"])
    out: list[dict] = []
    for line in proc.stdout.splitlines():
        # `mount` output: "<device> on <mountpoint> (<fs>, <opts>)"
        if " on " not in line or " (" not in line or not line.endswith(")"):
            continue
        try:
            device, rest = line.split(" on ", 1)
            mountpoint, paren = rest.rsplit(" (", 1)
        except ValueError:
            continue
        details = paren[:-1]
        parts = [p.strip() for p in details.split(",")]
        if not parts:
            continue
        fs_type = parts[0]
        options = parts[1:]
        if fs_type.lower() != "apfs":
            continue
        out.append(
            {
                "device": device.strip(),
                "mountpoint": mountpoint.strip(),
                "fs_type": fs_type,
                "options": options,
                "snapshot_in_options": any("snapshot" in opt.lower() for opt in options),
            }
        )
    return out


def list_tmutil_snapshots(mount: str) -> list[str]:
    proc = run(["tmutil", "listlocalsnapshots", mount])
    if proc.returncode != 0:
        return []
    names: list[str] = []
    for line in proc.stdout.splitlines():
        line = line.strip()
        if not line or line.startswith("Snapshots for disk"):
            continue
        names.append(line)
    return names


def list_diskutil_snapshots(device: str) -> list[dict]:
    proc = run(["diskutil", "apfs", "listSnapshots", "-plist", device])
    if proc.returncode != 0:
        return []
    try:
        doc = plistlib.loads(proc.stdout.encode("utf-8"))
    except plistlib.InvalidFileException:
        return []
    snaps: list[dict] = []
    for container in doc.get("Snapshots", []):
        snaps.append(
            {
                "uuid": container.get("SnapshotUUID"),
                "name": container.get("SnapshotName"),
                "xid": container.get("SnapshotXID"),
                "purgeable": container.get("Purgeable"),
            }
        )
    if not snaps:
        for vol in doc.get("Containers", []) or []:
            for v in vol.get("Volumes", []) or []:
                for snap in v.get("Snapshots", []) or []:
                    snaps.append(
                        {
                            "uuid": snap.get("SnapshotUUID"),
                            "name": snap.get("SnapshotName"),
                            "xid": snap.get("SnapshotXID"),
                            "purgeable": snap.get("Purgeable"),
                        }
                    )
    return snaps


def is_sealed_system_snapshot(name: str | None) -> bool:
    return bool(name) and name.startswith(SEALED_SYSTEM_SNAPSHOT_PREFIX)


# ---- Rust scanner invocation ------------------------------------------- #

def run_fallback_scan(path: str) -> dict | None:
    """Returns the parsed JSON, or None if the scanner did not return 0
    (we want the probe to keep going even if one mount is unreadable)."""
    proc = run(
        [
            "cargo",
            "run",
            "--quiet",
            "--manifest-path",
            str(RUST_CRATE_DIR / "Cargo.toml"),
            "--bin",
            "apfs-fastindex-scan",
            "--",
            "--mode",
            "fallback",
            path,
        ]
    )
    if proc.returncode != 0:
        return None
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        return None


def entries_by_path(doc: dict) -> dict[str, dict]:
    out: dict[str, dict] = {}
    for entry in doc.get("parser_output", {}).get("entries", []) or []:
        out[entry["path"]] = entry
    return out


def diff_shape(live: dict, snapshot: dict) -> dict:
    """Diff on the intersection of entry.path; unchanged-tuple is
    `(entry_kind, file_id, logical_size, symlink_target)`."""
    live_paths = set(live.keys())
    snap_paths = set(snapshot.keys())
    only_live = sorted(live_paths - snap_paths)
    only_snap = sorted(snap_paths - live_paths)
    intersect = sorted(live_paths & snap_paths)
    mismatches: list[dict] = []
    for path in intersect:
        a, b = live[path], snapshot[path]
        live_tuple = (
            a.get("entry_kind"),
            a.get("file_id"),
            a.get("logical_size"),
            a.get("symlink_target"),
        )
        snap_tuple = (
            b.get("entry_kind"),
            b.get("file_id"),
            b.get("logical_size"),
            b.get("symlink_target"),
        )
        if live_tuple != snap_tuple:
            mismatches.append(
                {
                    "path": path,
                    "live": {
                        "entry_kind": a.get("entry_kind"),
                        "file_id": a.get("file_id"),
                        "logical_size": a.get("logical_size"),
                        "symlink_target": a.get("symlink_target"),
                        "allocated_size_diagnostic": a.get("allocated_size"),
                    },
                    "snapshot": {
                        "entry_kind": b.get("entry_kind"),
                        "file_id": b.get("file_id"),
                        "logical_size": b.get("logical_size"),
                        "symlink_target": b.get("symlink_target"),
                        "allocated_size_diagnostic": b.get("allocated_size"),
                    },
                }
            )
    return {
        "intersection_count": len(intersect),
        "only_live_count": len(only_live),
        "only_snapshot_count": len(only_snap),
        "mismatch_count": len(mismatches),
        "only_live": only_live[:50],
        "only_snapshot": only_snap[:50],
        "mismatches": mismatches[:50],
        "matched": not mismatches,
    }


# ---- environment ------------------------------------------------------- #

def environment() -> dict:
    sw_vers = run(["sw_vers"])
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "uid": os.getuid(),
        "tmutil": shutil.which("tmutil"),
        "diskutil": shutil.which("diskutil"),
        "mount": shutil.which("mount"),
        "cargo": shutil.which("cargo"),
        "sw_vers": sw_vers.stdout,
    }


def reproducer(snapshot_name: str, live_mount: str) -> str:
    safe_tmp = "/tmp/apfsfi-ex23-snapshot-mount"
    return (
        f"# To unblock this probe, mount the snapshot read-only "
        f"yourself (sudo) and rerun:\n"
        f"sudo mkdir -p {safe_tmp} && "
        f"sudo mount_apfs -s {snapshot_name} "
        f"{live_mount} {safe_tmp}"
    )


# ---- driver ------------------------------------------------------------ #

def main() -> int:
    write_json("environment.json", environment())

    summary: dict[str, Any] = {
        "status": "executed",
        "verdict": "pending",
        "verdict_detail": "",
    }

    try:
        apfs_mounts = list_apfs_mounts()
        write_json("ex23-mount-table.json", {"apfs_mounts": apfs_mounts})

        inventory: list[dict] = []
        for mount in apfs_mounts:
            tm_snapshots = list_tmutil_snapshots(mount["mountpoint"])
            diskutil_snapshots = list_diskutil_snapshots(mount["device"])
            # Union names from both sources.
            tm_names = {n for n in tm_snapshots if not is_sealed_system_snapshot(n)}
            for snap in diskutil_snapshots:
                name = snap.get("name")
                if not is_sealed_system_snapshot(name):
                    tm_names.add(name or "")
            tm_names.discard("")
            inventory.append(
                {
                    "device": mount["device"],
                    "mountpoint": mount["mountpoint"],
                    "tmutil_snapshots": tm_snapshots,
                    "diskutil_snapshots": diskutil_snapshots,
                    "user_visible_snapshot_names": sorted(tm_names),
                    "skipped_sealed_system_snapshots": [
                        s.get("name")
                        for s in diskutil_snapshots
                        if is_sealed_system_snapshot(s.get("name"))
                    ],
                }
            )
        write_json("ex23-snapshot-inventory.json", {"inventory": inventory})

        total_user_visible = sum(
            len(entry["user_visible_snapshot_names"]) for entry in inventory
        )
        if total_user_visible == 0:
            summary["verdict"] = "blocked_no_snapshots_at_all"
            summary["verdict_detail"] = (
                "No user-visible APFS snapshots found on any reachable APFS volume "
                "(sealed-system OS-update snapshots are excluded per SR-020). "
                "EX-23 is correctly blocked on a clean dev workstation; rerun after "
                "taking a Time Machine local snapshot (e.g. `tmutil localsnapshot`)."
            )
            summary["inventory_path"] = "artifacts/generated/ex23-snapshot-inventory.json"
            write_json("summary.json", summary)
            return 0

        # Find mounted snapshots. macOS reports them in `mount(8)` with
        # the snapshot name appearing in the options text.
        mounted_snapshot_candidates: list[dict] = []
        for mount in apfs_mounts:
            if not mount["snapshot_in_options"]:
                continue
            # Match this mount against the inventory.
            for entry in inventory:
                for snap_name in entry["user_visible_snapshot_names"]:
                    if snap_name in " ".join(mount["options"]):
                        mounted_snapshot_candidates.append(
                            {
                                "live_mountpoint": entry["mountpoint"],
                                "snapshot_mountpoint": mount["mountpoint"],
                                "snapshot_name": snap_name,
                                "snapshot_device": mount["device"],
                            }
                        )

        if not mounted_snapshot_candidates:
            reproducers = []
            for entry in inventory:
                for snap_name in entry["user_visible_snapshot_names"]:
                    reproducers.append(
                        {
                            "snapshot": snap_name,
                            "live_mountpoint": entry["mountpoint"],
                            "command": reproducer(snap_name, entry["mountpoint"]),
                        }
                    )
            summary["verdict"] = "blocked_no_mounted_user_snapshot"
            summary["verdict_detail"] = (
                f"Found {total_user_visible} user-visible APFS snapshot(s) across "
                f"{len(inventory)} mounted APFS volumes, but none is currently "
                "mounted at a user-readable path. SR-020 records that "
                "`mount_apfs -s` requires sudo; the probe never escalates "
                "privileges. See reproducers for the exact sudo commands."
            )
            summary["reproducers"] = reproducers[:25]
            summary["inventory_path"] = "artifacts/generated/ex23-snapshot-inventory.json"
            write_json("summary.json", summary)
            return 0

        # We have at least one (live, snapshot) pair to diff.
        per_pair: list[dict] = []
        any_mismatch = False
        any_success = False
        for cand in mounted_snapshot_candidates:
            live = run_fallback_scan(cand["live_mountpoint"])
            snap = run_fallback_scan(cand["snapshot_mountpoint"])
            if live is None or snap is None:
                per_pair.append(
                    {
                        "live_mountpoint": cand["live_mountpoint"],
                        "snapshot_mountpoint": cand["snapshot_mountpoint"],
                        "snapshot_name": cand["snapshot_name"],
                        "verdict": "scan_failed",
                        "live_ok": live is not None,
                        "snapshot_ok": snap is not None,
                    }
                )
                continue
            diff = diff_shape(entries_by_path(live), entries_by_path(snap))
            any_success = True
            if not diff["matched"]:
                any_mismatch = True
            per_pair.append(
                {
                    "live_mountpoint": cand["live_mountpoint"],
                    "snapshot_mountpoint": cand["snapshot_mountpoint"],
                    "snapshot_name": cand["snapshot_name"],
                    "verdict": "matched" if diff["matched"] else "shape_divergence",
                    "diff": diff,
                }
            )
        write_json("ex23-shape-diff.json", {"pairs": per_pair})

        if not any_success:
            summary["verdict"] = "oracle_inconclusive"
            summary["verdict_detail"] = (
                "Found mounted snapshot pair(s) but the Rust fallback scan failed "
                "on every pair; see ex23-shape-diff.json."
            )
        elif any_mismatch:
            summary["verdict"] = "shape_divergence"
            summary["verdict_detail"] = (
                "At least one (live, snapshot) pair diverged on an unchanged path; "
                "see ex23-shape-diff.json for the per-path mismatch."
            )
        else:
            summary["verdict"] = "validated_snapshot_shape_parity"
            summary["verdict_detail"] = (
                f"All {len(per_pair)} (live, snapshot) pair(s) matched on the "
                "intersection of unchanged paths."
            )
        summary["pair_count"] = len(per_pair)
        write_json("summary.json", summary)
        return 0 if summary["verdict"] in {
            "validated_snapshot_shape_parity",
            "blocked_no_mounted_user_snapshot",
            "blocked_no_snapshots_at_all",
        } else 1
    except Exception as err:  # noqa: BLE001 (probe wants the trace verbatim)
        summary["verdict"] = "oracle_inconclusive"
        summary["verdict_detail"] = f"{type(err).__name__}: {err}"
        write_json("summary.json", summary)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
