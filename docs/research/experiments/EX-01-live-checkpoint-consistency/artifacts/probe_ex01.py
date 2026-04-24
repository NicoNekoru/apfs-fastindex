#!/usr/bin/env python3
"""Run EX-01: live checkpoint consistency and runtime boundary."""

from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import struct
import subprocess
import tempfile
import time
from pathlib import Path


NX_MAGIC = 0x4253584E
F_FULLFSYNC = 51
ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)


def run(cmd: list[str], input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_checked(cmd: list[str], input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    proc = run(cmd, input_text=input_text)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def write_text(name: str, text: str) -> None:
    (GENERATED_DIR / name).write_text(text)


def diskutil_info_plist(target: str) -> dict:
    proc = run_checked(["diskutil", "info", "-plist", target])
    return plistlib.loads(proc.stdout.encode("utf-8"))


def attach_image(image_path: Path, mountpoint: Path) -> dict:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)]
    )
    return plistlib.loads(proc.stdout.encode("utf-8"))


def detach_device(dev_entry: str) -> None:
    run(["hdiutil", "detach", dev_entry])


def latest_checkpoint_info(raw_container_path: str) -> dict:
    with open(raw_container_path, "rb", buffering=0) as f:
        block0 = f.read(4096)
        block_size = struct.unpack_from("<I", block0, 0x24)[0]
        desc_blocks = struct.unpack_from("<I", block0, 0x68)[0]
        desc_base_raw = struct.unpack_from("<Q", block0, 0x70)[0]
        non_contiguous = bool(desc_base_raw >> 63)
        desc_base = desc_base_raw & ((1 << 63) - 1)

        candidates = []
        highest_xid = None
        for index in range(desc_blocks):
            f.seek((desc_base + index) * block_size)
            block = f.read(block_size)
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
                logical_size = len(os.readlink(path))
            else:
                entry_type = "file"
                logical_size = st.st_size
            entries.append(
                {
                    "type": entry_type,
                    "path": str(rel_path),
                    "inode": st.st_ino,
                    "nlink": st.st_nlink,
                    "logical_size": logical_size,
                }
            )
    return entries


def full_sync(path: Path) -> None:
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


def run_live_image_probe() -> dict:
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex01-", dir="/tmp"))
    image_path = base / "ex01-live.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    entities: list[dict] = []

    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "128m",
                "-fs",
                "APFS",
                "-volname",
                "EX01Live",
                "-nospotlight",
                str(image_path),
            ]
        )

        attach_info = attach_image(image_path, mountpoint)
        entities = attach_info["system-entities"]
        mounted_volume = next(entity["dev-entry"] for entity in entities if entity.get("mount-point"))
        volume_info = diskutil_info_plist(mounted_volume)
        container_ref = volume_info["APFSContainerReference"]
        raw_container_path = "/dev/r" + container_ref

        operations = [
            "create alpha.txt with 1 MiB payload",
            "append 256 KiB to alpha.txt",
            "rename alpha.txt -> alpha-renamed.txt",
            "create directory nested",
            "move alpha-renamed.txt into nested/",
            "create beta.txt with 1 MiB payload",
            "append 256 KiB to beta.txt",
            "delete beta.txt",
        ]

        logs = []

        def record(step: str) -> None:
            logs.append(
                {
                    "step": step,
                    "timestamp": time.time(),
                    "checkpoint": latest_checkpoint_info(raw_container_path),
                    "tree_snapshot": snapshot_tree(mountpoint),
                }
            )

        record("initial")

        alpha = mountpoint / "alpha.txt"
        beta = mountpoint / "beta.txt"
        nested = mountpoint / "nested"

        alpha.write_bytes(os.urandom(1024 * 1024))
        full_sync(alpha)
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[0])

        with alpha.open("ab") as handle:
            handle.write(os.urandom(256 * 1024))
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[1])

        alpha_renamed = mountpoint / "alpha-renamed.txt"
        alpha.rename(alpha_renamed)
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[2])

        nested.mkdir()
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[3])

        moved_alpha = nested / "alpha-renamed.txt"
        alpha_renamed.rename(moved_alpha)
        sync_directory(mountpoint)
        sync_directory(nested)
        run(["sync"])
        time.sleep(0.2)
        record(operations[4])

        beta.write_bytes(os.urandom(1024 * 1024))
        full_sync(beta)
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[5])

        with beta.open("ab") as handle:
            handle.write(os.urandom(256 * 1024))
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[6])

        beta.unlink()
        sync_directory(mountpoint)
        run(["sync"])
        time.sleep(0.2)
        record(operations[7])

        return {
            "image_path": str(image_path),
            "mountpoint": str(mountpoint),
            "mounted_volume": mounted_volume,
            "container_ref": container_ref,
            "raw_container_path": raw_container_path,
            "operations": operations,
            "logs": logs,
        }
    finally:
        if entities:
            detach_device(entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)


def main() -> None:
    root_info = diskutil_info_plist("/")
    sw_vers = {
        line.split(":")[0].strip(): line.split(":", 1)[1].strip()
        for line in run_checked(["sw_vers"]).stdout.splitlines()
        if ":" in line
    }

    startup_container = root_info.get("APFSContainerReference")
    startup_raw_path = "/dev/r" + startup_container if startup_container else None
    startup_raw_access = {"path": startup_raw_path, "success": False}
    if startup_raw_path:
        try:
            with open(startup_raw_path, "rb", buffering=0) as handle:
                data = handle.read(4096)
            startup_raw_access["success"] = True
            startup_raw_access["bytes_read"] = len(data)
        except Exception as exc:  # pragma: no cover - experiment path
            startup_raw_access["error"] = f"{type(exc).__name__}: {exc}"

    write_json(
        "host-environment.json",
        {
            "sw_vers": sw_vers,
            "root_volume": {
                key: root_info.get(key)
                for key in [
                    "DeviceIdentifier",
                    "MountPoint",
                    "APFSContainerReference",
                    "APFSSnapshot",
                    "APFSSnapshotName",
                    "APFSVolumeGroupID",
                    "FileVault",
                    "Encryption",
                    "Sealed",
                    "Writable",
                ]
            },
            "startup_raw_access": startup_raw_access,
        },
    )
    write_text("startup-diskutil-apfs-list.txt", run_checked(["diskutil", "apfs", "list"]).stdout)

    live_probe = run_live_image_probe()
    write_json("live-image-probe.json", live_probe)

    summary = {
        "startup_raw_access": startup_raw_access,
        "initial_highest_xid": live_probe["logs"][0]["checkpoint"]["highest_xid"],
        "final_highest_xid": live_probe["logs"][-1]["checkpoint"]["highest_xid"],
        "unique_highest_xids": sorted(
            {
                entry["checkpoint"]["highest_xid"]
                for entry in live_probe["logs"]
                if entry["checkpoint"]["highest_xid"] is not None
            }
        ),
        "operation_count": len(live_probe["operations"]),
    }
    write_json("summary.json", summary)


if __name__ == "__main__":
    main()
