#!/usr/bin/env python3
"""Run EX-08 safe read-path support matrix cells.

This probe records access and validation facts without converting raw
readability into a support claim. It only exercises cells that are safe on the
current host: detached image control, mounted image control, and an
unprivileged startup raw-read attempt.
"""

from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time
from dataclasses import asdict
from pathlib import Path


ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
REPO_ROOT = ARTIFACT_DIR.parents[4]
SRC_DIR = REPO_ROOT / "src"
APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
F_FULLFSYNC = 51

GENERATED_DIR.mkdir(exist_ok=True)
sys.path.insert(0, str(SRC_DIR))

from apfs_fastindex.oracle_diff import compare_parser_output_to_oracle  # noqa: E402
from apfs_fastindex.parser import ParserSkeleton  # noqa: E402
from apfs_fastindex.poc_fixture import build_proof_fixture  # noqa: E402


def run(cmd: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=cwd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_checked(cmd: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    proc = run(cmd, cwd=cwd)
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
    proc = run_checked(["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)])
    return plistlib.loads(proc.stdout.encode("utf-8"))


def detach_device(dev_entry: str) -> None:
    run(["hdiutil", "detach", dev_entry])


def normalize_raw_device(device: str) -> str:
    if device.startswith("/dev/rdisk"):
        return device
    if device.startswith("/dev/disk"):
        return "/dev/r" + device.split("/dev/", 1)[1]
    return device


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


def settle() -> None:
    run(["sync"])
    time.sleep(0.15)


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


def diff_to_dict(diff: object) -> dict:
    return {
        "matched": diff.matched,
        "missing_paths": list(diff.missing_paths),
        "unexpected_paths": list(diff.unexpected_paths),
        "mismatches": [asdict(mismatch) for mismatch in diff.mismatches],
    }


def cell_template(source_id: str, source_class: str) -> dict:
    return {
        "source_id": source_id,
        "source_class": source_class,
        "requested_mode": "raw_single_volume_namespace_logical_size",
        "raw_readable": None,
        "checkpoint_discovery": None,
        "root_discovery": None,
        "raw_walk": None,
        "oracle_available": None,
        "comparison_matched": None,
        "support_verdict": "pending",
        "fallback_reason": None,
    }


def run_detached_image_control() -> dict:
    cell = cell_template("detached-unencrypted-dmg", "detached_image")
    parser = ParserSkeleton()
    with build_proof_fixture() as fixture:
        output = parser.parse(fixture.image_path)
        diff = compare_parser_output_to_oracle(output, fixture.oracle_path)
        cell.update(
            {
                "raw_readable": True,
                "checkpoint_discovery": True,
                "root_discovery": True,
                "raw_walk": True,
                "oracle_available": True,
                "comparison_matched": diff.matched,
                "support_verdict": "supported" if diff.matched else "fallback_required",
                "fallback_reason": None if diff.matched else "detached image control did not match oracle",
                "entry_count": len(output.entries),
                "aggregate_count": len(output.aggregates),
                "scan_state": asdict(output.scan_state),
                "oracle_diff": diff_to_dict(diff),
                "operations": list(fixture.operations),
            }
        )
    return cell


def create_mounted_control_fixture(root: Path) -> list[str]:
    operations = []
    src = root / "src"
    dst = root / "dst"
    src.mkdir()
    dst.mkdir()
    sync_directory(root)
    settle()
    operations.append("create src and dst directories")

    base = src / "base.txt"
    base.write_text("mounted control\n")
    full_sync(base)
    sync_directory(src)
    settle()
    operations.append("create src/base.txt")

    moved = dst / "moved.txt"
    base.rename(moved)
    sync_directory(src)
    sync_directory(dst)
    settle()
    operations.append("move src/base.txt -> dst/moved.txt")

    link = dst / "link.txt"
    os.symlink("moved.txt", link)
    sync_directory(dst)
    settle()
    operations.append("create symlink dst/link.txt -> moved.txt")
    return operations


def run_mounted_image_control() -> dict:
    cell = cell_template("mounted-unencrypted-dmg-quiescent", "mounted_image")
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex08-mounted-", dir="/tmp"))
    image_path = base / "ex08-mounted.dmg"
    mountpoint = base / "mnt"
    oracle_path = base / "oracle.json"
    mountpoint.mkdir()
    entities: list[dict] = []
    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "160m",
                "-fs",
                "APFS",
                "-volname",
                "EX08Mounted",
                "-nospotlight",
                str(image_path),
            ]
        )
        attach_info = attach_image(image_path, mountpoint)
        entities = attach_info["system-entities"]
        container_dev = next(
            entity["dev-entry"]
            for entity in entities
            if entity.get("content-hint") == APFS_CONTAINER_HINT
        )
        raw_container_path = normalize_raw_device(container_dev)
        operations = create_mounted_control_fixture(mountpoint)
        oracle_path.write_text(
            json.dumps({"entries": snapshot_tree(mountpoint)}, indent=2, sort_keys=True) + "\n"
        )

        parser = ParserSkeleton()
        output = parser.parse(raw_container_path)
        diff = compare_parser_output_to_oracle(output, oracle_path)
        cell.update(
            {
                "raw_readable": True,
                "checkpoint_discovery": True,
                "root_discovery": True,
                "raw_walk": True,
                "oracle_available": True,
                "comparison_matched": diff.matched,
                "support_verdict": "readable_not_supported",
                "fallback_reason": (
                    "mounted lab image was raw-readable and parsable, but current raw path "
                    "does not enforce selected-XID lookups for mounted sources"
                ),
                "entry_count": len(output.entries),
                "aggregate_count": len(output.aggregates),
                "scan_state": asdict(output.scan_state),
                "oracle_diff": diff_to_dict(diff),
                "operations": operations,
            }
        )
    except Exception as exc:  # pragma: no cover - experiment path
        cell.update(
            {
                "raw_readable": False,
                "checkpoint_discovery": False,
                "root_discovery": False,
                "raw_walk": False,
                "oracle_available": oracle_path.exists(),
                "comparison_matched": False,
                "support_verdict": "fallback_required",
                "fallback_reason": f"{type(exc).__name__}: {exc}",
            }
        )
    finally:
        if entities:
            detach_device(entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)
    return cell


def run_startup_unprivileged_read() -> dict:
    cell = cell_template("startup-container-unprivileged-raw-read", "startup_container")
    root_info = diskutil_info_plist("/")
    startup_container = root_info.get("APFSContainerReference")
    startup_raw_path = f"/dev/r{startup_container}" if startup_container else None
    cell["root_volume"] = {
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
    }
    cell["raw_container_path"] = startup_raw_path
    if not startup_raw_path:
        cell.update(
            {
                "raw_readable": False,
                "checkpoint_discovery": False,
                "root_discovery": False,
                "raw_walk": False,
                "oracle_available": False,
                "comparison_matched": None,
                "support_verdict": "fallback_required",
                "fallback_reason": "startup APFS container reference not found",
            }
        )
        return cell

    try:
        with open(startup_raw_path, "rb", buffering=0) as handle:
            data = handle.read(4096)
        cell.update(
            {
                "raw_readable": len(data) == 4096,
                "checkpoint_discovery": False,
                "root_discovery": False,
                "raw_walk": False,
                "oracle_available": False,
                "comparison_matched": None,
                "support_verdict": "fallback_required",
                "fallback_reason": (
                    "startup raw bytes were readable, but startup/System/Data semantics, "
                    "privilege, and oracle policy remain outside raw v1"
                ),
                "bytes_read": len(data),
            }
        )
    except Exception as exc:  # pragma: no cover - experiment path
        cell.update(
            {
                "raw_readable": False,
                "checkpoint_discovery": False,
                "root_discovery": False,
                "raw_walk": False,
                "oracle_available": False,
                "comparison_matched": None,
                "support_verdict": "blocked_privilege",
                "fallback_reason": f"{type(exc).__name__}: {exc}",
            }
        )
    return cell


def build_environment() -> dict:
    diskutil_apfs = run(["diskutil", "apfs", "list"])
    if diskutil_apfs.stdout:
        write_text("diskutil-apfs-list.txt", diskutil_apfs.stdout)
    return {
        "sw_vers": run_checked(["sw_vers"]).stdout,
        "uname": run_checked(["uname", "-a"]).stdout.strip(),
        "go_version": run(["go", "version"]).stdout.strip(),
        "diskutil_apfs_list_returncode": diskutil_apfs.returncode,
        "diskutil_apfs_list_stderr": diskutil_apfs.stderr,
        "probe_scope": (
            "safe cells only: detached image control, mounted image control, "
            "unprivileged startup raw-read attempt"
        ),
    }


def summarize(cells: list[dict]) -> dict:
    return {
        "cell_count": len(cells),
        "verdicts": {cell["source_id"]: cell["support_verdict"] for cell in cells},
        "raw_readable": {cell["source_id"]: cell["raw_readable"] for cell in cells},
        "comparison_matched": {cell["source_id"]: cell["comparison_matched"] for cell in cells},
        "ruled_out": [
            "raw-readable is not equivalent to supported",
            "mounted-image readability does not prove live raw support without selected-XID enforcement",
            "startup raw v1 support remains outside the allowlist",
        ],
    }


def main() -> None:
    environment = build_environment()
    write_json("environment.json", environment)

    cells = [
        run_detached_image_control(),
        run_mounted_image_control(),
        run_startup_unprivileged_read(),
    ]
    for cell in cells:
        write_json(f"{cell['source_id']}.json", cell)
    write_json("summary.json", summarize(cells))


if __name__ == "__main__":
    main()
