#!/usr/bin/env python3
"""EX-29: local-snapshot extent-set contribution.

Python-direct probe that characterizes the host's snapshot state and
the oracles available for snapshot accounting on a stock Apple silicon
macOS host.

After EX-28 closed with verdict `live_raw_blocked_by_kernel`, the
original EX-29 plan's raw-extent-set diff path is ruled out: a
`mount_apfs -s` snapshot device node is gated by the same kernel
storage-security policy that returns EPERM on the live data
partition. This probe therefore enumerates the available oracles
and emits one of:

- ``validated_snapshot_enumeration``: the host has at least one
  user-visible TM local snapshot (excluding sealed-system OS-update
  snapshots per SR-020). The probe emits the count + names; the
  reclaimable byte total is left unclaimed (no public read-only
  oracle).
- ``blocked_no_user_snapshots``: the host has no user-visible
  TM local snapshots — only sealed-system OS-update snapshots, or
  none at all. Same shape EX-23 found on this host class.
- ``probe_exception``: tmutil / diskutil unavailable, or output
  parsing failed.

The probe is read-only and unprivileged.
"""

from __future__ import annotations

import datetime as _dt
import json
import platform
import re
import shutil
import subprocess
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
REPO_ROOT = ARTIFACT_DIR.parents[4]

# Mount points to enumerate. `tmutil listlocalsnapshots /` is the
# canonical macOS query for boot-volume snapshots; the data volume
# at `/System/Volumes/Data` is the same APFS volume from a different
# mount-point angle and is included so probe output matches what a
# user might invoke directly.
MOUNT_POINTS = ("/", "/System/Volumes/Data")

# SR-020 exclusion: sealed-system OS-update snapshots are not
# user-reclaimable. They live on the read-only system volume and
# can't be deleted by tmutil; their bytes aren't user-visible.
SEALED_SYSTEM_SNAPSHOT_PATTERN = re.compile(r"^com\.apple\.os\.update-")


class ProbeError(RuntimeError):
    def __init__(self, verdict: str, detail: str) -> None:
        super().__init__(detail)
        self.verdict = verdict
        self.detail = detail


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
    raise TypeError(f"unserializable: {type(obj).__name__}")


# ---- tmutil parsing ----------------------------------------------------- #

def parse_tmutil_listlocalsnapshots(stdout: str) -> list[str]:
    """`tmutil listlocalsnapshots <mount>` output shape:

        Snapshots for disk <mount>:
        com.apple.TimeMachine.2026-05-20-100000.local
        com.apple.TimeMachine.2026-05-20-110000.local

    or, when none exist, just the header line and nothing after.
    Returns the list of snapshot names in document order.
    """
    snapshots: list[str] = []
    for line in stdout.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        if stripped.startswith("Snapshots for disk"):
            continue
        # Skip any indented "NOTE" lines that may appear; snapshot
        # names never start with "NOTE:".
        if stripped.startswith("NOTE:"):
            continue
        snapshots.append(stripped)
    return snapshots


def parse_diskutil_listsnapshots(stdout: str) -> list[dict]:
    """`diskutil apfs listSnapshots <mount>` output shape:

        Snapshot for disk3s1s1 (1 found)
        |
        +-- 3E1AC922-F4EC-433E-B4D0-...
            Name:        com.apple.os.update-...
            XID:         2293965
            Purgeable:   No
            NOTE:        ...

    Each `+--` block introduces one snapshot. Returns a list of
    {uuid, name, xid, purgeable, notes} dicts in document order.
    """
    snapshots: list[dict] = []
    current: dict | None = None
    for line in stdout.splitlines():
        stripped = line.strip()
        if stripped.startswith("+--"):
            if current is not None:
                snapshots.append(current)
            uuid = stripped[3:].strip()
            current = {"uuid": uuid, "name": None, "xid": None, "purgeable": None, "notes": []}
            continue
        if current is None:
            continue
        if stripped.startswith("Name:"):
            current["name"] = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("XID:"):
            xid_text = stripped.split(":", 1)[1].strip()
            try:
                current["xid"] = int(xid_text)
            except ValueError:
                current["xid"] = None
        elif stripped.startswith("Purgeable:"):
            current["purgeable"] = stripped.split(":", 1)[1].strip()
        elif stripped.startswith("NOTE:"):
            current["notes"].append(stripped.split(":", 1)[1].strip())
    if current is not None:
        snapshots.append(current)
    return snapshots


def is_user_visible_snapshot(name: str) -> bool:
    """SR-020 user-visibility filter. Sealed-system OS-update
    snapshots are excluded from any reclaimable accounting because
    the user can't delete them and their bytes aren't user-visible.
    """
    if SEALED_SYSTEM_SNAPSHOT_PATTERN.match(name):
        return False
    return True


# ---- enumeration -------------------------------------------------------- #

def enumerate_mount_point(mount: str) -> dict:
    """For one mount point, return the tmutil snapshot list, the
    diskutil snapshot list, and a per-name SR-020 filter result."""
    tm = run(["tmutil", "listlocalsnapshots", mount])
    du = run(["diskutil", "apfs", "listSnapshots", mount])
    tm_snapshots = (
        parse_tmutil_listlocalsnapshots(tm.stdout)
        if tm.returncode == 0
        else []
    )
    du_snapshots = (
        parse_diskutil_listsnapshots(du.stdout)
        if du.returncode == 0
        else []
    )

    filtered_tm = [
        {"name": name, "user_visible": is_user_visible_snapshot(name)}
        for name in tm_snapshots
    ]
    filtered_du = [
        {**snap, "user_visible": is_user_visible_snapshot(snap["name"] or "")}
        for snap in du_snapshots
    ]

    return {
        "mount_point": mount,
        "tmutil_returncode": tm.returncode,
        "tmutil_stdout": tm.stdout,
        "tmutil_stderr": tm.stderr,
        "diskutil_returncode": du.returncode,
        "diskutil_stdout": du.stdout,
        "diskutil_stderr": du.stderr,
        "tmutil_snapshots": filtered_tm,
        "diskutil_snapshots": filtered_du,
        "user_visible_tmutil_count": sum(1 for s in filtered_tm if s["user_visible"]),
        "user_visible_diskutil_count": sum(1 for s in filtered_du if s["user_visible"]),
    }


def environment() -> dict:
    sw_vers = run(["sw_vers"])
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "tmutil": shutil.which("tmutil"),
        "diskutil": shutil.which("diskutil"),
        "sw_vers": sw_vers.stdout,
    }


def main() -> int:
    write_json("environment.json", environment())
    summary: dict[str, Any] = {
        "status": "executed",
        "verdict": "pending",
        "verdict_detail": "",
    }
    try:
        per_mount = [enumerate_mount_point(mp) for mp in MOUNT_POINTS]
        write_json("ex29-mount-enumeration.json", {"mount_points": per_mount})

        all_tmutil = [
            entry
            for record in per_mount
            for entry in record["tmutil_snapshots"]
        ]
        all_diskutil = [
            entry
            for record in per_mount
            for entry in record["diskutil_snapshots"]
        ]
        user_visible_tmutil = [entry for entry in all_tmutil if entry["user_visible"]]
        user_visible_diskutil = [entry for entry in all_diskutil if entry["user_visible"]]
        sealed_excluded = [
            entry["name"] for entry in all_diskutil if not entry["user_visible"]
        ]

        snapshot_table = {
            "tmutil_total": len(all_tmutil),
            "diskutil_total": len(all_diskutil),
            "user_visible_tmutil": len(user_visible_tmutil),
            "user_visible_diskutil": len(user_visible_diskutil),
            "sealed_system_excluded": sealed_excluded,
            "user_visible_tmutil_names": [s["name"] for s in user_visible_tmutil],
            "user_visible_diskutil_names": [
                s["name"] for s in user_visible_diskutil if s.get("name")
            ],
        }
        write_json("ex29-snapshot-table.json", snapshot_table)

        if user_visible_tmutil or user_visible_diskutil:
            verdict = "validated_snapshot_enumeration"
            detail = (
                f"Host has {len(user_visible_tmutil)} user-visible TM local "
                f"snapshot(s) and {len(user_visible_diskutil)} diskutil-visible "
                f"snapshot(s) after the SR-020 sealed-system filter. "
                f"Snapshot enumeration is the deliverable; reclaimable bytes "
                f"are unclaimed (no public read-only oracle on macOS)."
            )
        else:
            verdict = "blocked_no_user_snapshots"
            detail = (
                f"No user-visible TM local snapshots on any of {MOUNT_POINTS}. "
                f"Diskutil reports {len(all_diskutil)} sealed-system snapshot(s) "
                f"(SR-020 excluded): {sealed_excluded}. Same shape EX-23 found on "
                f"this host class; EX-29 enumeration is structurally correct but "
                f"has nothing to surface."
            )

        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["user_visible_tmutil_count"] = len(user_visible_tmutil)
        summary["user_visible_diskutil_count"] = len(user_visible_diskutil)
        summary["sealed_system_excluded_count"] = len(sealed_excluded)
        write_json("summary.json", summary)
        return 0 if verdict.startswith("validated") or verdict.startswith("blocked_no") else 1
    except ProbeError as err:
        summary["verdict"] = err.verdict
        summary["verdict_detail"] = err.detail
        write_json("summary.json", summary)
        return 1
    except Exception as err:
        import traceback
        summary["verdict"] = "probe_exception"
        summary["verdict_detail"] = f"{type(err).__name__}: {err}"
        summary["traceback"] = traceback.format_exc()
        write_json("summary.json", summary)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
