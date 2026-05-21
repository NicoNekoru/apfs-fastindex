#!/usr/bin/env python3
"""EX-31: enumerate the boot-root layout on this Mac.

Captures:
  - APFS containers + volumes via `diskutil apfs list -plist`
  - The boot snapshot identity via `diskutil info /`
  - The firmlink table from /usr/share/firmlinks
  - A mapping of Finder-visible top-level paths → their
    physical device + cross-firmlink flag

Saves the result to `artifacts/generated/ex31_boot_root_<date>.json`.
Re-runs are non-destructive and date-suffixed; historical
captures are preserved.

Usage (from repo root):
  python3 docs/research/experiments/EX-31-boot-root-oracle/\
artifacts/probe_ex31.py

No arguments. No privileged ops — `diskutil apfs list` runs
unprivileged.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import plistlib
import subprocess
import sys
from pathlib import Path

ARTIFACT_DIR = Path(__file__).resolve().parent
GEN_DIR = ARTIFACT_DIR / "generated"
GEN_DIR.mkdir(parents=True, exist_ok=True)

# Top-level paths Finder shows in `/`. Used to cross-reference
# physical volumes + firmlinks. Not exhaustive — Finder hides
# `/private`, `/usr`, etc. behind Go-to-Folder.
FINDER_TOP_LEVEL = [
    "/Applications",
    "/Library",
    "/System",
    "/Users",
    "/Volumes",
    "/private",
    "/usr",
    "/etc",
    "/var",
    "/tmp",
    "/opt",
]


def _diskutil_apfs_plist() -> dict:
    out = subprocess.check_output(["diskutil", "apfs", "list", "-plist"])
    return plistlib.loads(out)


def _diskutil_info(path: str) -> dict:
    out = subprocess.check_output(["diskutil", "info", "-plist", path])
    return plistlib.loads(out)


def _firmlinks() -> list[dict[str, str]]:
    entries: list[dict[str, str]] = []
    try:
        with open("/usr/share/firmlinks", encoding="utf-8") as f:
            for raw in f:
                line = raw.rstrip("\n")
                if not line or line.startswith("#"):
                    continue
                parts = line.split("\t")
                if len(parts) != 2:
                    continue
                system_path, data_path = parts
                entries.append({"system_path": system_path, "data_path": data_path})
    except FileNotFoundError:
        pass
    return entries


def _path_device(path: str) -> str | None:
    try:
        st = os.stat(path)
        # Return major,minor as a stable string. We don't try
        # to translate to a diskutil device id — that requires
        # extra plumbing; the (major, minor) is sufficient to
        # identify which volume a path lives on.
        return f"{os.major(st.st_dev)}:{os.minor(st.st_dev)}"
    except (FileNotFoundError, PermissionError, NotADirectoryError):
        return None


def _classify_volumes(apfs: dict) -> list[dict]:
    """Flatten diskutil apfs list into one row per volume with
    the fields we care about. Skips empty containers."""
    rows: list[dict] = []
    for container in apfs.get("Containers", []):
        container_uuid = container.get("APFSContainerUUID")
        container_ref = container.get("ContainerReference")
        for vol in container.get("Volumes", []):
            rows.append({
                "device_id": vol.get("DeviceIdentifier"),
                "volume_uuid": vol.get("APFSVolumeUUID"),
                "volume_name": vol.get("Name"),
                "role": vol.get("Roles", []),  # ["System"], ["Data"], etc.
                "mount_point": vol.get("MountPoint", ""),
                "container_uuid": container_uuid,
                "container_reference": container_ref,
                "capacity_in_use": vol.get("CapacityInUse"),
                "capacity_quota": vol.get("CapacityQuota"),
            })
    return rows


def _boot_snapshot_info() -> dict:
    info = _diskutil_info("/")
    return {
        "device_id": info.get("DeviceIdentifier"),
        "mount_point": info.get("MountPoint"),
        "volume_name": info.get("VolumeName"),
        "snapshot_name": info.get("APFSSnapshotName"),
        "snapshot_uuid": info.get("APFSSnapshotUUID"),
        "is_snapshot": info.get("APFSSnapshot", False),
        "writable": info.get("Writable"),
        "container_reference": info.get("APFSContainerReference"),
    }


def main() -> int:
    apfs = _diskutil_apfs_plist()
    volumes = _classify_volumes(apfs)
    boot = _boot_snapshot_info()
    fls = _firmlinks()

    # Build the path-classification table — for each Finder-
    # visible top-level path, record which device it's on and
    # whether it's crossing a firmlink. The walker traverses
    # all of these transparently via POSIX.
    fls_by_system = {fl["system_path"]: fl["data_path"] for fl in fls}
    path_map: list[dict] = []
    root_dev = _path_device("/")
    for path in FINDER_TOP_LEVEL:
        dev = _path_device(path)
        # A path "crosses a firmlink" if it's listed in the
        # firmlink table OR if its device differs from the
        # root's. Both produce the same effect: the user sees
        # one logical tree but it's stitched from two
        # filesystems.
        is_firmlink = path in fls_by_system
        is_different_device = dev is not None and dev != root_dev
        path_map.append({
            "path": path,
            "device": dev,
            "is_firmlink": is_firmlink,
            "firmlink_target": fls_by_system.get(path),
            "different_device_from_root": is_different_device,
            "exists": dev is not None,
        })

    record = {
        "experiment": "EX-31",
        "title": "Boot-root oracle: Finder-visible namespace layout",
        "date": _dt.date.today().isoformat(),
        "host": {
            "platform": platform.platform(),
            "kernel": platform.release(),
        },
        "verdict": "api_only_already_correct",
        "boot_snapshot": boot,
        "apfs_volumes": volumes,
        "firmlinks": fls,
        "finder_top_level_path_map": path_map,
        "notes": [
            "POSIX traversal already follows firmlinks transparently.",
            "Live raw mode is EX-28-blocked (kernel rejects /dev/disk3s* reads).",
            "Snapshot contents are EX-29-blocked (SIP-protected even with FDA).",
            "API-only via the fallback walker is the only available "
            "production path for live boot-volume scans.",
        ],
    }

    out_file = GEN_DIR / f"ex31_boot_root_{_dt.date.today().isoformat()}.json"
    out_file.write_text(json.dumps(record, indent=2, default=str))
    print(f"=== EX-31 boot-root oracle ===")
    print(f"  Host:               {record['host']['platform']}")
    print(f"  Boot snapshot:      {boot.get('snapshot_name', '(none)')}")
    print(f"  APFS volumes:       {len(volumes)}")
    print(f"  Firmlinks:          {len(fls)}")
    print(f"  Top-level paths:    {len(path_map)}")
    print(f"    crossing firmlink:  {sum(1 for p in path_map if p['is_firmlink'])}")
    print(f"    different device:   {sum(1 for p in path_map if p['different_device_from_root'])}")
    print(f"  Output:             {out_file}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
