#!/usr/bin/env python3
"""Run EX-04: expanded pinned raw-vs-oracle corpus for narrow v1."""

from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import stat
import struct
import subprocess
import tempfile
import time
from pathlib import Path


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
F_FULLFSYNC = 51
NX_MAGIC = 0x4253584E
ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
RAW_WALK_DIR = (
    ARTIFACT_DIR
    / ".."
    / ".."
    / "EX-03-pinned-state-raw-vs-oracle"
    / "artifacts"
    / "rawwalk"
).resolve()
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


def attach_nomount_image(image_path: Path) -> dict:
    proc = run_checked(["hdiutil", "attach", "-plist", "-nomount", str(image_path)])
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


def entry_type(path: Path, st: os.stat_result) -> str:
    mode = st.st_mode
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISFIFO(mode):
        return "other(DT_FIFO)"
    if stat.S_ISCHR(mode):
        return "other(DT_CHR)"
    if stat.S_ISBLK(mode):
        return "other(DT_BLK)"
    if stat.S_ISSOCK(mode):
        return "other(DT_SOCK)"
    return "other(DT_UNKNOWN)"


def snapshot_tree(root: Path) -> list[dict]:
    entries: list[dict] = []
    for current_root, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        rel_root = Path(current_root).relative_to(root)
        if str(rel_root).startswith(".fseventsd"):
            continue
        dirnames[:] = [name for name in dirnames if name != ".fseventsd"]
        current_path = Path(current_root)
        current_stat = os.lstat(current_path)
        entries.append(
            {
                "type": "dir",
                "path": "." if str(rel_root) == "." else str(rel_root),
                "inode": current_stat.st_ino,
                "nlink": current_stat.st_nlink,
            }
        )
        for name in filenames:
            path = current_path / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            kind = entry_type(path, st)
            entry = {
                "type": kind,
                "path": str(rel_path),
                "inode": st.st_ino,
                "nlink": st.st_nlink,
            }
            if kind == "symlink":
                target = os.readlink(path)
                entry["logical_size"] = len(target)
                entry["symlink_target"] = target
            elif kind == "file":
                entry["logical_size"] = st.st_size
                entry["allocated_bytes"] = st.st_blocks * 512
            entries.append(entry)
    return entries


def snapshot_summary(entries: list[dict]) -> dict:
    files = [entry for entry in entries if entry["type"] == "file"]
    symlinks = [entry for entry in entries if entry["type"] == "symlink"]
    others = [entry for entry in entries if entry["type"].startswith("other(")]
    unique_inode_sizes: dict[int, int] = {}
    for entry in files:
        unique_inode_sizes.setdefault(entry["inode"], entry["logical_size"])
    return {
        "entry_count": len(entries),
        "file_count": len(files),
        "symlink_count": len(symlinks),
        "other_count": len(others),
        "naive_logical_total": sum(entry["logical_size"] for entry in files),
        "unique_inode_logical_total": sum(unique_inode_sizes.values()),
        "hard_link_paths": sorted(entry["path"] for entry in files if entry["nlink"] > 1),
        "sparse_candidates": sorted(
            entry["path"]
            for entry in files
            if entry.get("allocated_bytes", 0) < entry.get("logical_size", 0)
        ),
    }


def normalize_oracle_entry(entry: dict) -> dict:
    normalized = {
        "type": entry["type"],
        "file_id": entry["inode"],
    }
    if entry["type"] in {"file", "symlink"}:
        normalized["logical_size"] = entry["logical_size"]
    if entry["type"] == "symlink":
        normalized["symlink_target"] = entry["symlink_target"]
    return normalized


def normalize_raw_entry(entry: dict) -> dict:
    normalized = {
        "type": entry["type"],
        "file_id": entry["file_id"],
    }
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
    mismatches = []
    for path in sorted(set(oracle_index) & set(raw_index)):
        if oracle_index[path] != raw_index[path]:
            mismatches.append(
                {
                    "path": path,
                    "oracle": oracle_index[path],
                    "raw": raw_index[path],
                }
            )

    return {
        "oracle_path_count": len(oracle_index),
        "raw_path_count": len(raw_index),
        "missing_paths": missing_paths,
        "unexpected_paths": unexpected_paths,
        "mismatch_count": len(mismatches),
        "mismatches": mismatches,
        "matched": not missing_paths and not unexpected_paths and not mismatches,
    }


def raw_summary(entries: list[dict]) -> dict:
    files = [entry for entry in entries if entry["type"] == "file"]
    symlinks = [entry for entry in entries if entry["type"] == "symlink"]
    others = [entry for entry in entries if entry["type"].startswith("other(")]
    unique_inode_sizes: dict[int, int] = {}
    for entry in files:
        unique_inode_sizes.setdefault(entry["file_id"], entry.get("logical_size", 0))
    duplicate_ids = {
        entry["file_id"]
        for entry in files
        if sum(1 for other in files if other["file_id"] == entry["file_id"]) > 1
    }
    return {
        "entry_count": len(entries),
        "file_count": len(files),
        "symlink_count": len(symlinks),
        "other_count": len(others),
        "naive_logical_total": sum(entry.get("logical_size", 0) for entry in files),
        "unique_inode_logical_total": sum(unique_inode_sizes.values()),
        "hard_link_paths": sorted(entry["path"] for entry in files if entry["file_id"] in duplicate_ids),
    }


def run_rawwalk(raw_container_path: str) -> dict:
    proc = run_checked(["go", "run", ".", "--device", raw_container_path], input_text=None)
    return json.loads(proc.stdout)


def create_file(path: Path, contents: str) -> None:
    path.write_text(contents)
    sync_file(path)
    sync_directory(path.parent)
    settle()


def try_create(path: Path, contents: str) -> dict:
    result = {"path": str(path.name), "created": False, "error": None}
    try:
        with path.open("x") as handle:
            handle.write(contents)
        sync_file(path)
        sync_directory(path.parent)
        settle()
        result["created"] = True
    except Exception as exc:  # pragma: no cover - experiment path
        result["error"] = f"{type(exc).__name__}: {exc}"
    return result


def build_corpus(root: Path) -> list[dict]:
    operations: list[dict] = []
    src = root / "src"
    dst = root / "dst"
    nested = root / "nested" / "branch" / "leaf"
    unicode_dir = root / "unicode"
    special_dir = root / "special"
    for directory in (src, dst, nested, unicode_dir, special_dir):
        directory.mkdir(parents=True, exist_ok=True)
    sync_directory(root)
    settle()
    operations.append({"step": "create base directories"})

    base_file = src / "base.txt"
    create_file(base_file, "alpha\n")
    operations.append({"step": "create src/base.txt"})

    renamed = src / "renamed.txt"
    base_file.rename(renamed)
    sync_directory(src)
    settle()
    operations.append({"step": "rename src/base.txt -> src/renamed.txt"})

    moved = dst / "moved.txt"
    renamed.rename(moved)
    sync_directory(src)
    sync_directory(dst)
    settle()
    operations.append({"step": "move src/renamed.txt -> dst/moved.txt"})

    hard_link = nested / "hard-across-dir.txt"
    os.link(moved, hard_link)
    sync_directory(nested)
    settle()
    operations.append({"step": "create hard link nested/branch/leaf/hard-across-dir.txt"})

    sparse = dst / "sparse-2m.bin"
    with sparse.open("wb") as handle:
        handle.write(b"HEAD")
        handle.seek(2 * 1024 * 1024 - 4)
        handle.write(b"TAIL")
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass
    sync_directory(dst)
    settle()
    operations.append({"step": "create sparse file dst/sparse-2m.bin"})

    clone = dst / "clone.txt"
    clone_proc = run(["cp", "-c", str(moved), str(clone)])
    sync_directory(dst)
    settle()
    operations.append(
        {
            "step": "clone dst/moved.txt -> dst/clone.txt",
            "returncode": clone_proc.returncode,
            "stderr": clone_proc.stderr.strip(),
        }
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
    operations.append({"step": "append to dst/moved.txt after clone"})

    symlink = dst / "link.txt"
    os.symlink("../nested/branch/leaf/hard-across-dir.txt", symlink)
    sync_directory(dst)
    settle()
    operations.append({"step": "create symlink dst/link.txt"})

    unicode_results = [
        try_create(unicode_dir / "cafe-é.txt", "precomposed\n"),
        try_create(unicode_dir / "cafe-e\u0301.txt", "decomposed\n"),
        try_create(unicode_dir / "東京.txt", "tokyo\n"),
    ]
    operations.append({"step": "unicode name probe", "results": unicode_results})

    case_results = [
        try_create(root / "CaseName.txt", "A\n"),
        try_create(root / "casename.txt", "B\n"),
    ]
    operations.append({"step": "case collision probe", "results": case_results})

    fifo = special_dir / "queue.fifo"
    fifo_result = {"created": False, "error": None}
    try:
        os.mkfifo(fifo)
        sync_directory(special_dir)
        settle()
        fifo_result["created"] = True
    except Exception as exc:  # pragma: no cover - experiment path
        fifo_result["error"] = f"{type(exc).__name__}: {exc}"
    operations.append({"step": "special file probe", "fifo": fifo_result})

    compress_src = dst / "compress-source.txt"
    create_file(compress_src, "compressible data\n" * 512)
    compressed = dst / "compressed-candidate.txt"
    ditto_proc = run(["ditto", "--hfsCompression", str(compress_src), str(compressed)])
    if compressed.exists():
        sync_file(compressed)
    sync_directory(dst)
    settle()
    operations.append(
        {
            "step": "compressed candidate via ditto --hfsCompression",
            "returncode": ditto_proc.returncode,
            "stderr": ditto_proc.stderr.strip(),
            "created": compressed.exists(),
        }
    )

    return operations


def run_case(volume_label: str, fs_name: str) -> dict:
    base = Path(tempfile.mkdtemp(prefix=f"apfsfi-ex04-{volume_label.lower()}-", dir="/tmp"))
    image_path = base / f"{volume_label}.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_entities: list[dict] = []
    nomount_entities: list[dict] = []

    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "256m",
                "-fs",
                fs_name,
                "-volname",
                volume_label,
                "-nospotlight",
                str(image_path),
            ]
        )

        attach_info = attach_image(image_path, mountpoint)
        mounted_entities = attach_info["system-entities"]
        operations = build_corpus(mountpoint)
        oracle_entries = snapshot_tree(mountpoint)
        oracle = {
            "entries": oracle_entries,
            "summary": snapshot_summary(oracle_entries),
        }

        detach_device(mounted_entities[0]["dev-entry"])
        mounted_entities = []

        nomount_info = attach_nomount_image(image_path)
        nomount_entities = nomount_info["system-entities"]
        container_dev = next(
            entity["dev-entry"]
            for entity in nomount_entities
            if entity.get("content-hint") == APFS_CONTAINER_HINT
        )
        raw_container_path = "/dev/r" + container_dev.split("/dev/")[1]

        pinned_state = latest_checkpoint_info(raw_container_path)
        raw_result = run_rawwalk(raw_container_path)
        comparison = compare_entries(oracle_entries, raw_result["entries"])

        return {
            "volume_label": volume_label,
            "fs_name": fs_name,
            "image_name": image_path.name,
            "operations": operations,
            "mounted_oracle": oracle,
            "pinned_state": pinned_state,
            "raw_walk": raw_result,
            "raw_summary": raw_summary(raw_result["entries"]),
            "comparison": comparison,
            "nomount_entities": nomount_entities,
        }
    finally:
        if nomount_entities:
            detach_device(nomount_entities[0]["dev-entry"])
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
    }
    write_json("environment.json", environment)

    run_checked(["go", "mod", "tidy"], input_text=None)

    cases = [
        run_case("EX04CI", "APFS"),
        run_case("EX04CS", "Case-sensitive APFS"),
    ]

    combined = {
        "case_count": len(cases),
        "all_matched": all(case["comparison"]["matched"] for case in cases),
        "cases": [
            {
                "volume_label": case["volume_label"],
                "fs_name": case["fs_name"],
                "matched": case["comparison"]["matched"],
                "pinned_highest_xid": case["pinned_state"]["highest_xid"],
                "oracle_summary": case["mounted_oracle"]["summary"],
                "raw_summary": case["raw_summary"],
                "missing_paths": case["comparison"]["missing_paths"],
                "unexpected_paths": case["comparison"]["unexpected_paths"],
                "mismatch_count": case["comparison"]["mismatch_count"],
                "mismatches": case["comparison"]["mismatches"],
            }
            for case in cases
        ],
    }

    for case in cases:
        slug = case["volume_label"].lower()
        write_json(f"{slug}-oracle.json", case["mounted_oracle"])
        write_json(f"{slug}-pinned-state.json", case["pinned_state"])
        write_json(f"{slug}-raw-walk.json", case["raw_walk"])
        write_json(f"{slug}-comparison.json", case["comparison"])
        write_json(f"{slug}-run.json", case)
    write_json("summary.json", combined)


if __name__ == "__main__":
    os.chdir(RAW_WALK_DIR)
    main()
