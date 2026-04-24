from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import subprocess
import tempfile
import time
from contextlib import contextmanager
from dataclasses import dataclass
from pathlib import Path
from typing import Iterator


F_FULLFSYNC = 51


@dataclass(frozen=True)
class ProofFixture:
    image_path: Path
    oracle_path: Path
    operations: tuple[str, ...]


class FixtureBuildError(RuntimeError):
    pass


def _run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _run_checked(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    proc = _run(cmd)
    if proc.returncode != 0:
        raise FixtureBuildError(
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def _attach_image(image_path: Path, mountpoint: Path) -> dict:
    proc = _run_checked(["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)])
    return plistlib.loads(proc.stdout.encode("utf-8"))


def _detach(device: str) -> None:
    _run(["hdiutil", "detach", device])


def _full_sync(path: Path) -> None:
    with path.open("ab") as handle:
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass


def _sync_directory(path: Path) -> None:
    dirfd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)


def _settle() -> None:
    _run(["sync"])
    time.sleep(0.15)


def _snapshot_tree(root: Path) -> list[dict]:
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


@contextmanager
def build_proof_fixture() -> Iterator[ProofFixture]:
    base = Path(tempfile.mkdtemp(prefix="apfsfi-skeleton-", dir="/tmp"))
    image_path = base / "skeleton-proof.dmg"
    mountpoint = base / "mnt"
    oracle_path = base / "oracle.json"
    mountpoint.mkdir()
    entities: list[dict] = []

    try:
        _run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "160m",
                "-fs",
                "APFS",
                "-volname",
                "SKELPROOF",
                "-nospotlight",
                str(image_path),
            ]
        )
        attach_info = _attach_image(image_path, mountpoint)
        entities = attach_info["system-entities"]

        operations = []
        src = mountpoint / "src"
        dst = mountpoint / "dst"
        src.mkdir()
        dst.mkdir()
        _sync_directory(mountpoint)
        _settle()
        operations.append("create src and dst directories")

        base_file = src / "base.txt"
        base_file.write_text("alpha\n")
        _full_sync(base_file)
        _sync_directory(src)
        _settle()
        operations.append("create src/base.txt")

        renamed = src / "renamed.txt"
        base_file.rename(renamed)
        _sync_directory(src)
        _settle()
        operations.append("rename src/base.txt -> src/renamed.txt")

        moved = dst / "moved.txt"
        renamed.rename(moved)
        _sync_directory(src)
        _sync_directory(dst)
        _settle()
        operations.append("move src/renamed.txt -> dst/moved.txt")

        hard_link = dst / "hard.txt"
        os.link(moved, hard_link)
        _sync_directory(dst)
        _settle()
        operations.append("create hard link dst/hard.txt")

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
        _sync_directory(dst)
        _settle()
        operations.append("create sparse file dst/sparse.bin")

        clone = dst / "clone.txt"
        clone_proc = _run(["cp", "-c", str(moved), str(clone)])
        if clone_proc.returncode != 0:
            raise FixtureBuildError(f"clone step failed:\n{clone_proc.stderr}")
        _sync_directory(dst)
        _settle()
        operations.append("clone dst/moved.txt -> dst/clone.txt")

        with moved.open("a") as handle:
            handle.write("beta\n")
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        _sync_directory(dst)
        _settle()
        operations.append("append to dst/moved.txt")

        symlink = dst / "link.txt"
        os.symlink("moved.txt", symlink)
        _sync_directory(dst)
        _settle()
        operations.append("create symlink dst/link.txt -> moved.txt")

        oracle_entries = _snapshot_tree(mountpoint)
        oracle_path.write_text(json.dumps({"entries": oracle_entries}, indent=2, sort_keys=True) + "\n")

        if entities:
            _detach(entities[0]["dev-entry"])
            entities = []

        yield ProofFixture(
            image_path=image_path,
            oracle_path=oracle_path,
            operations=tuple(operations),
        )
    finally:
        if entities:
            _detach(entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)
