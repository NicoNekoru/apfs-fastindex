#!/usr/bin/env python3
"""Run EX-03: required-record taxonomy for namespace and logical size."""

from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


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


def attach_image(image_path: Path, mountpoint: Path) -> dict:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)]
    )
    return plistlib.loads(proc.stdout.encode("utf-8"))


def detach_device(dev_entry: str) -> None:
    run(["hdiutil", "detach", dev_entry])


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
                    "allocated_bytes": st.st_blocks * 512,
                }
            )
    return entries


def snapshot_summary(entries: list[dict]) -> dict:
    files = [entry for entry in entries if entry["type"] == "file"]
    symlinks = [entry for entry in entries if entry["type"] == "symlink"]
    hard_link_paths = [entry["path"] for entry in files if entry["nlink"] > 1]
    unique_inode_sizes: dict[int, int] = {}
    for entry in files:
        unique_inode_sizes.setdefault(entry["inode"], entry["logical_size"])
    sparse_candidates = [
        entry["path"] for entry in files if entry["allocated_bytes"] < entry["logical_size"]
    ]
    return {
        "file_count": len(files),
        "symlink_count": len(symlinks),
        "naive_logical_total": sum(entry["logical_size"] for entry in files),
        "unique_inode_logical_total": sum(unique_inode_sizes.values()),
        "hard_link_paths": hard_link_paths,
        "sparse_candidates": sparse_candidates,
    }


def record(logs: list[dict], root: Path, step: str, extra: dict | None = None) -> None:
    entries = snapshot_tree(root)
    logs.append(
        {
            "step": step,
            "snapshot": entries,
            "summary": snapshot_summary(entries),
            "extra": extra or {},
        }
    )


def run_case(volume_label: str, fs_name: str) -> dict:
    base = Path(tempfile.mkdtemp(prefix=f"apfsfi-{volume_label.lower()}-", dir="/tmp"))
    image_path = base / f"{volume_label}.dmg"
    mountpoint = base / "mnt"
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
                fs_name,
                "-volname",
                volume_label,
                "-nospotlight",
                str(image_path),
            ]
        )
        attach_info = attach_image(image_path, mountpoint)
        entities = attach_info["system-entities"]

        logs: list[dict] = []
        src = mountpoint / "src"
        dst = mountpoint / "dst"

        record(logs, mountpoint, "initial")

        src.mkdir()
        dst.mkdir()
        sync_directory(mountpoint)
        settle()
        record(logs, mountpoint, "create src and dst directories")

        base_file = src / "base.txt"
        base_file.write_text("alpha\n")
        full_sync(base_file)
        sync_directory(src)
        settle()
        record(logs, mountpoint, "create src/base.txt")

        renamed = src / "renamed.txt"
        base_file.rename(renamed)
        sync_directory(src)
        settle()
        record(logs, mountpoint, "rename src/base.txt -> src/renamed.txt")

        moved = dst / "moved.txt"
        renamed.rename(moved)
        sync_directory(src)
        sync_directory(dst)
        settle()
        record(logs, mountpoint, "move src/renamed.txt -> dst/moved.txt")

        hard_link = dst / "hard.txt"
        os.link(moved, hard_link)
        sync_directory(dst)
        settle()
        record(logs, mountpoint, "create hard link dst/hard.txt")

        sparse = dst / "sparse.bin"
        with sparse.open("wb") as handle:
            handle.write(b"HEAD")
            handle.seek(1024 * 1024 - 4)
            handle.write(b"TAIL")
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(dst)
        settle()
        record(logs, mountpoint, "create sparse file dst/sparse.bin")

        clone = dst / "clone.txt"
        clone_proc = run(["cp", "-c", str(moved), str(clone)])
        sync_directory(dst)
        settle()
        record(
            logs,
            mountpoint,
            "clone dst/moved.txt -> dst/clone.txt",
            {
                "cp_returncode": clone_proc.returncode,
                "cp_stderr": clone_proc.stderr.strip(),
            },
        )

        with moved.open("a") as handle:
            handle.write("beta\n")
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(dst)
        settle()
        record(logs, mountpoint, "append to dst/moved.txt")

        symlink = dst / "link.txt"
        os.symlink("moved.txt", symlink)
        sync_directory(dst)
        settle()
        record(logs, mountpoint, "create symlink dst/link.txt")

        case_a = mountpoint / "CaseName.txt"
        case_b = mountpoint / "casename.txt"
        case_result = {"case_a_created": False, "case_b_created": False, "case_b_error": None}
        with case_a.open("x") as handle:
            handle.write("A\n")
        case_result["case_a_created"] = True
        try:
            with case_b.open("x") as handle:
                handle.write("B\n")
            case_result["case_b_created"] = True
        except Exception as exc:  # pragma: no cover - experiment path
            case_result["case_b_error"] = f"{type(exc).__name__}: {exc}"
        sync_directory(mountpoint)
        settle()
        record(logs, mountpoint, "case probe", case_result)

        return {
            "volume_label": volume_label,
            "fs_name": fs_name,
            "logs": logs,
        }
    finally:
        if entities:
            detach_device(entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)


def main() -> None:
    ci = run_case("EX03CI", "APFS")
    cs = run_case("EX03CS", "Case-sensitive APFS")

    summary = {
        "case_insensitive_case_probe": ci["logs"][-1]["extra"],
        "case_sensitive_case_probe": cs["logs"][-1]["extra"],
        "case_insensitive_clone_step": next(
            log for log in ci["logs"] if log["step"] == "clone dst/moved.txt -> dst/clone.txt"
        )["extra"],
        "case_sensitive_clone_step": next(
            log for log in cs["logs"] if log["step"] == "clone dst/moved.txt -> dst/clone.txt"
        )["extra"],
        "case_insensitive_hardlink_summary": next(
            log for log in ci["logs"] if log["step"] == "create hard link dst/hard.txt"
        )["summary"],
        "case_sensitive_hardlink_summary": next(
            log for log in cs["logs"] if log["step"] == "create hard link dst/hard.txt"
        )["summary"],
        "case_insensitive_sparse_summary": next(
            log for log in ci["logs"] if log["step"] == "create sparse file dst/sparse.bin"
        )["summary"],
        "case_sensitive_sparse_summary": next(
            log for log in cs["logs"] if log["step"] == "create sparse file dst/sparse.bin"
        )["summary"],
    }

    write_json("case-insensitive.json", ci)
    write_json("case-sensitive.json", cs)
    write_json("summary.json", summary)


if __name__ == "__main__":
    main()
