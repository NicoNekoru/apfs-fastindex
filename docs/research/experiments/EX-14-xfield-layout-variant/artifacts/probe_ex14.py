#!/usr/bin/env python3
"""Run EX-14: APFS xfield layout variant oracle.

This is deliberately still a Python-first probe.  It reuses the EX-13 raw
FS-tree body reader, creates broader same-run fixtures, and records whether the
xfield layout choices are deterministic enough to encode in Rust.
"""

from __future__ import annotations

import datetime as _dt
import fcntl
import importlib.util
import json
import os
import platform
import plistlib
import shutil
import stat
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
F_FULLFSYNC = 51
INO_EXT_TYPE_NAME = 4
INO_EXT_TYPE_DSTREAM = 8
INO_EXT_TYPE_SPARSE_BYTES = 13
DREC_EXT_TYPE_SIBLING_ID = 1
FIXTURE_XATTR_PREFIX = "com.apfsfi."

ARTIFACT_DIR = Path(__file__).resolve().parent
EXPERIMENT_DIR = ARTIFACT_DIR.parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
REPO_ROOT = ARTIFACT_DIR.parents[4]
EX13_PROBE = (
    REPO_ROOT
    / "docs"
    / "research"
    / "experiments"
    / "EX-13-native-fs-record-body-oracle"
    / "artifacts"
    / "probe_ex13.py"
)
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"

GENERATED_DIR.mkdir(exist_ok=True)


class ProbeError(RuntimeError):
    def __init__(self, verdict: str, detail: str) -> None:
        super().__init__(detail)
        self.verdict = verdict
        self.detail = detail


def load_ex13() -> Any:
    spec = importlib.util.spec_from_file_location("probe_ex13", EX13_PROBE)
    if spec is None or spec.loader is None:
        raise ProbeError("not_executed", f"unable to load {EX13_PROBE}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["probe_ex13"] = module
    spec.loader.exec_module(module)
    return module


EX13 = load_ex13()


def run(cmd: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        cwd=str(cwd) if cwd else None,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_checked(cmd: list[str], cwd: Path | None = None) -> subprocess.CompletedProcess[str]:
    proc = run(cmd, cwd=cwd)
    if proc.returncode != 0:
        raise ProbeError(
            "command_failed",
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}",
        )
    return proc


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n"
    )


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


def detach_device(device: str) -> None:
    run(["hdiutil", "detach", device])


def attach_image(image_path: Path, mountpoint: Path) -> tuple[list[dict], str]:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)]
    )
    info = plistlib.loads(proc.stdout.encode("utf-8"))
    entities = info.get("system-entities", [])
    if not entities:
        raise ProbeError("attach_failed", "hdiutil attach returned no system entities")
    return entities, entities[0]["dev-entry"]


def attach_nomount_image(image_path: Path) -> tuple[list[dict], str, str]:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-nomount", "-readonly", str(image_path)]
    )
    info = plistlib.loads(proc.stdout.encode("utf-8"))
    entities = info.get("system-entities", [])
    if not entities:
        raise ProbeError("attach_failed", "hdiutil attach -nomount returned no entities")
    detach = entities[0]["dev-entry"]
    container = None
    for entity in entities:
        if entity.get("content-hint") == APFS_CONTAINER_HINT:
            container = entity.get("dev-entry")
            break
    if container is None:
        raise ProbeError("missing_apfs_container", "no APFS container entity found")
    if container.startswith("/dev/disk"):
        container = "/dev/rdisk" + container[len("/dev/disk") :]
    return entities, detach, container


def create_file(path: Path, payload: bytes | str) -> None:
    if isinstance(payload, bytes):
        path.write_bytes(payload)
    else:
        path.write_text(payload)
    full_sync(path)
    sync_directory(path.parent)
    settle()


def set_xattr(path: Path, name: str, value: str, symlink: bool = False) -> dict:
    return {
        "path": str(path.name),
        "name": name,
        "symlink": symlink,
        "returncode": None,
        "stderr": "",
        "skipped": "user xattrs are outside the EX-14 xfield-layout isolation pass",
    }


def try_create(path: Path, payload: str) -> dict:
    result = {"path": path.name, "created": False, "error": None}
    try:
        with path.open("x") as handle:
            handle.write(payload)
        full_sync(path)
        sync_directory(path.parent)
        settle()
        result["created"] = True
    except Exception as exc:  # pragma: no cover - experiment-only branch
        result["error"] = f"{type(exc).__name__}: {exc}"
    return result


def create_sparse(path: Path, size: int) -> None:
    with path.open("wb") as handle:
        handle.write(b"HEAD")
        handle.seek(size - 4)
        handle.write(b"TAIL")
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass
    sync_directory(path.parent)
    settle()


def entry_type(path: Path, st: os.stat_result) -> str:
    mode = st.st_mode
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "file"
    return f"other({stat.S_IFMT(mode):#x})"


def xattr_inventory(path: Path) -> dict[str, str]:
    out: dict[str, str] = {}
    list_proc = run(["xattr", "-s", str(path)])
    if list_proc.returncode != 0:
        return out
    for name in sorted(line.strip() for line in list_proc.stdout.splitlines() if line.strip()):
        value_proc = run(["xattr", "-p", "-x", "-s", name, str(path)])
        if value_proc.returncode != 0:
            continue
        out[name] = "".join(
            char for char in value_proc.stdout.lower() if char in "0123456789abcdef"
        )
    return out


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
        st = os.lstat(current_path)
        root_entry = {
            "type": "dir",
            "path": "." if str(rel_root) == "." else str(rel_root),
            "inode": st.st_ino,
            "nlink": st.st_nlink,
            "xattrs": xattr_inventory(current_path),
        }
        entries.append(root_entry)
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
                "xattrs": xattr_inventory(path),
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
    dirs = [entry for entry in entries if entry["type"] == "dir"]
    unique_inode_sizes: dict[int, int] = {}
    for entry in files:
        unique_inode_sizes.setdefault(entry["inode"], entry["logical_size"])
    return {
        "entry_count": len(entries),
        "dir_count": len(dirs),
        "file_count": len(files),
        "symlink_count": len(symlinks),
        "hard_link_paths": sorted(entry["path"] for entry in files if entry["nlink"] > 1),
        "sparse_candidates": sorted(
            entry["path"]
            for entry in files
            if entry.get("allocated_bytes", 0) < entry.get("logical_size", 0)
        ),
        "fixture_xattr_paths": sorted(
            entry["path"]
            for entry in entries
            if any(name.startswith(FIXTURE_XATTR_PREFIX) for name in entry["xattrs"])
        ),
        "unique_inode_logical_total": sum(unique_inode_sizes.values()),
    }


def build_variant_corpus(root: Path) -> list[dict]:
    operations: list[dict] = []
    dirs = {
        "src": root / "src",
        "dst": root / "dst",
        "names": root / "names",
        "sparse": root / "sparse",
    }
    for directory in dirs.values():
        directory.mkdir(parents=True, exist_ok=True)
    sync_directory(root)
    settle()
    operations.append({"step": "create src, dst, names, and sparse directories"})

    base = dirs["src"] / "base.txt"
    create_file(base, "alpha\n")
    operations.append({"step": "create src/base.txt"})

    renamed = dirs["src"] / "renamed.txt"
    base.rename(renamed)
    sync_directory(dirs["src"])
    settle()
    operations.append({"step": "rename src/base.txt -> src/renamed.txt"})

    moved = dirs["dst"] / "moved.txt"
    renamed.rename(moved)
    sync_directory(dirs["src"])
    sync_directory(dirs["dst"])
    settle()
    operations.append({"step": "move src/renamed.txt -> dst/moved.txt"})

    hard_peer = dirs["dst"] / "hard.txt"
    os.link(moved, hard_peer)
    sync_directory(dirs["dst"])
    settle()
    operations.append({"step": "create hard link dst/hard.txt"})

    sparse_one = dirs["sparse"] / "sparse-1m.bin"
    create_sparse(sparse_one, 1024 * 1024)
    sparse_two = dirs["sparse"] / "sparse-unaligned-name.bin"
    create_sparse(sparse_two, 1024 * 1024 + 123)
    operations.append(
        {
            "step": "create sparse variants",
            "paths": ["sparse/sparse-1m.bin", "sparse/sparse-unaligned-name.bin"],
        }
    )

    clone = dirs["dst"] / "clone.txt"
    clone_proc = run(["cp", "-c", str(moved), str(clone)])
    sync_directory(dirs["dst"])
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
    sync_directory(dirs["dst"])
    settle()
    operations.append({"step": "append to dst/moved.txt"})

    symlink = dirs["dst"] / "link.txt"
    os.symlink("moved.txt", symlink)
    sync_directory(dirs["dst"])
    settle()
    operations.append({"step": "create symlink dst/link.txt -> moved.txt"})

    for name, payload in [
        ("n1", "1\n"),
        ("name08ch", "eight\n"),
        ("name09chr", "nine\n"),
        ("name15charsxxx", "fifteen\n"),
        ("name17charsxxxxx", "seventeen\n"),
    ]:
        create_file(dirs["names"] / name, payload)
        operations.append({"step": f"create names/{name}"})

    operations.append(
        {
            "step": "skip optional user xattrs, unicode/case probes, and compression",
            "reason": "keep EX-14 isolated to xfield layout variants after the broad corpus hit a Rust checkpoint-context blocker",
        }
    )

    return operations


def run_rust_context(raw_container: str) -> dict:
    proc = run_checked(
        ["cargo", "run", "--quiet", "--bin", "apfs-fastindex-scan", "--", raw_container],
        cwd=RUST_CRATE_DIR,
    )
    return json.loads(proc.stdout)


def run_identitydump(raw_container: str) -> dict:
    try:
        return EX13.run_identitydump(raw_container)
    except Exception as exc:  # pragma: no cover - diagnostic observer
        return {"observer_error": f"{type(exc).__name__}: {exc}"}


def normalize_entry(entry: dict) -> dict:
    normalized = {
        "path": entry["path"],
        "type": entry["type"],
        "file_id": entry.get("file_id", entry.get("inode")),
    }
    if entry["type"] in {"file", "symlink"}:
        normalized["logical_size"] = entry.get("logical_size")
    if entry["type"] == "symlink":
        normalized["symlink_target"] = entry.get("symlink_target")
    return normalized


def compare_namespace(mounted: list[dict], native: list[dict]) -> dict:
    mounted_by_path = {entry["path"]: normalize_entry(entry) for entry in mounted}
    native_by_path = {entry["path"]: normalize_entry(entry) for entry in native}
    missing = sorted(set(mounted_by_path) - set(native_by_path))
    unexpected = sorted(set(native_by_path) - set(mounted_by_path))
    mismatches = []
    for path in sorted(set(mounted_by_path) & set(native_by_path)):
        expected = mounted_by_path[path]
        actual = native_by_path[path]
        if expected != actual:
            mismatches.append({"path": path, "expected": expected, "actual": actual})
    return {
        "matched": not missing and not unexpected and not mismatches,
        "missing_paths": missing,
        "unexpected_paths": unexpected,
        "mismatches": mismatches,
    }


def path_index(entries: list[dict]) -> dict[str, dict]:
    return {entry["path"]: entry for entry in entries}


def raw_path_to_inode(native_entries: list[dict]) -> dict[str, int]:
    return {entry["path"]: entry["file_id"] for entry in native_entries}


def compare_fixture_xattrs(mounted: list[dict], records: list[dict], native_entries: list[dict]) -> dict:
    inode_by_path = raw_path_to_inode(native_entries)
    raw_xattrs_by_inode: dict[int, dict[str, str]] = {}
    for record in records:
        if record["family"] != "xattr":
            continue
        name = record["key"].get("name")
        if not name or not name.startswith(FIXTURE_XATTR_PREFIX):
            continue
        raw_xattrs_by_inode.setdefault(record["object_id"], {})[name] = (
            record["value"].get("payload_hex") or ""
        )

    expected_by_path = {}
    actual_by_path = {}
    for entry in mounted:
        expected = {
            name: value
            for name, value in entry.get("xattrs", {}).items()
            if name.startswith(FIXTURE_XATTR_PREFIX)
        }
        if expected:
            expected_by_path[entry["path"]] = expected
            actual_by_path[entry["path"]] = raw_xattrs_by_inode.get(
                inode_by_path.get(entry["path"], -1), {}
            )

    mismatches = []
    for path, expected in sorted(expected_by_path.items()):
        actual = actual_by_path.get(path, {})
        if expected != actual:
            mismatches.append({"path": path, "expected": expected, "actual": actual})
    return {
        "matched": not mismatches,
        "fixture_xattr_path_count": len(expected_by_path),
        "mismatches": mismatches,
    }


def candidate_signature(candidate: dict) -> str:
    return json.dumps(candidate.get("field_summaries", []), sort_keys=True)


def clean_text(value: str | None) -> str | None:
    if value is None:
        return None
    return value.rstrip("\x00")


def expected_names_by_inode(records: list[dict]) -> dict[int, set[str]]:
    names: dict[int, set[str]] = {}
    for record in records:
        if record["family"] == "dir_rec":
            name = clean_text(record["key"].get("name"))
            file_id = record["value"].get("file_id")
            if name and file_id is not None:
                names.setdefault(file_id, set()).add(name)
        elif record["family"] == "sibling_link":
            name = clean_text(record["value"].get("name"))
            if name:
                names.setdefault(record["object_id"], set()).add(name)
    return names


def expected_sizes_by_inode(mounted: list[dict]) -> dict[int, set[int]]:
    sizes: dict[int, set[int]] = {}
    for entry in mounted:
        if entry["type"] in {"file", "symlink"} and "logical_size" in entry:
            sizes.setdefault(entry["inode"], set()).add(entry["logical_size"])
    return sizes


def sparse_inodes(mounted: list[dict]) -> set[int]:
    return {
        entry["inode"]
        for entry in mounted
        if entry["type"] == "file"
        and entry.get("allocated_bytes", 0) < entry.get("logical_size", 0)
    }


def sibling_map(records: list[dict]) -> dict[int, int]:
    out = {}
    for record in records:
        if record["family"] == "sibling_map":
            out[record["object_id"]] = record["value"].get("file_id")
    return out


def candidate_constraints(
    record: dict,
    candidate: dict,
    names_by_inode: dict[int, set[str]],
    sizes_by_inode: dict[int, set[int]],
    sparse_ids: set[int],
    sibling_id_to_file_id: dict[int, int],
) -> tuple[bool, list[str], list[str]]:
    passed: list[str] = []
    failed: list[str] = []
    summaries = candidate.get("field_summaries", [])
    for field in summaries:
        x_type = field.get("x_type")
        if x_type == INO_EXT_TYPE_NAME and record["family"] == "inode":
            value = clean_text(field.get("value"))
            expected = names_by_inode.get(record["object_id"])
            if expected:
                if value in expected:
                    passed.append("inode_name_matches_dir_or_sibling_record")
                else:
                    failed.append(
                        f"inode_name {value!r} not in raw name set {sorted(expected)!r}"
                    )
        elif x_type == INO_EXT_TYPE_DSTREAM and record["family"] == "inode":
            expected_sizes = sizes_by_inode.get(record["object_id"])
            size = field.get("size")
            if expected_sizes:
                if size in expected_sizes:
                    passed.append("dstream_size_matches_mounted_logical_size")
                else:
                    failed.append(
                        f"dstream size {size!r} not in mounted sizes {sorted(expected_sizes)!r}"
                    )
        elif x_type == INO_EXT_TYPE_SPARSE_BYTES and record["family"] == "inode":
            value = field.get("value")
            expected_sizes = sizes_by_inode.get(record["object_id"], set())
            if record["object_id"] in sparse_ids and expected_sizes:
                max_size = max(expected_sizes)
                if isinstance(value, int) and 0 <= value <= max_size:
                    passed.append("sparse_bytes_plausible_for_sparse_oracle")
                else:
                    failed.append(f"sparse bytes {value!r} outside logical size {max_size}")
        elif x_type == DREC_EXT_TYPE_SIBLING_ID and record["family"] == "dir_rec":
            value = field.get("value")
            file_id = record["value"].get("file_id")
            if isinstance(value, int) and value in sibling_id_to_file_id:
                if sibling_id_to_file_id[value] == file_id:
                    passed.append("drec_sibling_id_maps_to_child_file_id")
                else:
                    failed.append(
                        f"sibling id {value} maps to {sibling_id_to_file_id[value]}, not {file_id}"
                    )
    return not failed, passed, failed


def resolve_xfield_layouts(mounted: list[dict], records: list[dict]) -> dict:
    names_by_inode = expected_names_by_inode(records)
    sizes_by_inode = expected_sizes_by_inode(mounted)
    sparse_ids = sparse_inodes(mounted)
    sibling_id_to_file_id = sibling_map(records)

    records_with_xfields = []
    selected_layout_counts: dict[str, int] = {}
    selected_order_counts: dict[str, int] = {}
    unresolved = []
    resolved_by_constraints = 0
    resolved_by_identical_values = 0

    for record in records:
        value = record.get("value", {})
        candidates = value.get("xfield_layout_candidates") or []
        if not candidates:
            continue
        selected = value.get("xfield_layout")
        if selected:
            selected_layout_counts[selected] = selected_layout_counts.get(selected, 0) + 1
        selected_candidate = next(
            (candidate for candidate in candidates if candidate.get("layout") == selected),
            candidates[0],
        )
        order = ",".join(
            str(field.get("x_type"))
            for field in selected_candidate.get("field_summaries", [])
        )
        selected_order_counts[order] = selected_order_counts.get(order, 0) + 1

        evaluated = []
        for candidate in candidates:
            ok, passed, failed = candidate_constraints(
                record,
                candidate,
                names_by_inode,
                sizes_by_inode,
                sparse_ids,
                sibling_id_to_file_id,
            )
            evaluated.append(
                {
                    "layout": candidate.get("layout"),
                    "score": candidate.get("score"),
                    "signature": candidate_signature(candidate),
                    "constraint_ok": ok,
                    "constraints_passed": passed,
                    "constraints_failed": failed,
                    "field_summaries": candidate.get("field_summaries", []),
                }
            )

        viable = [candidate for candidate in evaluated if candidate["constraint_ok"]]
        viable_signatures = {candidate["signature"] for candidate in viable}
        constrained = any(candidate["constraints_passed"] for candidate in viable)
        if len(viable_signatures) <= 1:
            if constrained:
                resolved_by_constraints += 1
            else:
                resolved_by_identical_values += 1
        else:
            unresolved.append(
                {
                    "object_id": record["object_id"],
                    "family": record["family"],
                    "node_paddr": record["node_paddr"],
                    "entry_index": record["entry_index"],
                    "selected_layout": selected,
                    "selected_score": value.get("xfield_layout_score"),
                    "viable_signature_count": len(viable_signatures),
                    "evaluated_candidates": evaluated,
                }
            )

        records_with_xfields.append(
            {
                "object_id": record["object_id"],
                "family": record["family"],
                "selected_layout": selected,
                "selected_order": order,
                "candidate_count": len(candidates),
                "viable_signature_count": len(viable_signatures),
                "has_constraints": constrained,
            }
        )

    return {
        "records_with_xfields": len(records_with_xfields),
        "selected_layout_counts": selected_layout_counts,
        "selected_xfield_order_counts": selected_order_counts,
        "resolved_by_constraints": resolved_by_constraints,
        "resolved_by_identical_values": resolved_by_identical_values,
        "unresolved_record_count": len(unresolved),
        "unresolved_records": unresolved,
        "records": records_with_xfields,
    }


def compact_layout_summary(summary: dict) -> dict:
    return {
        "records_with_xfields": summary["records_with_xfields"],
        "selected_layout_counts": summary["selected_layout_counts"],
        "selected_xfield_order_counts": summary["selected_xfield_order_counts"],
        "resolved_by_constraints": summary["resolved_by_constraints"],
        "resolved_by_identical_values": summary["resolved_by_identical_values"],
        "unresolved_record_count": summary["unresolved_record_count"],
    }


def run_case(volume_label: str, fs_name: str) -> dict:
    base = Path(tempfile.mkdtemp(prefix=f"apfsfi-ex14-{volume_label.lower()}-", dir="/tmp"))
    image_path = base / f"{volume_label}.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_detach = None
    nomount_detach = None

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
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_variant_corpus(mountpoint)
        mounted_entries = snapshot_tree(mountpoint)
        mounted_oracle = {
            "volume_label": volume_label,
            "fs_name": fs_name,
            "entries": mounted_entries,
            "summary": snapshot_summary(mounted_entries),
        }
        slug = volume_label.lower()
        write_json(f"{slug}-fixture-operations.json", operations)
        write_json(f"{slug}-mounted-posix-oracle.json", mounted_oracle)
        detach_device(mounted_detach)
        mounted_detach = None

        nomount_entities, nomount_detach, raw_container = attach_nomount_image(image_path)
        rust_context = run_rust_context(raw_container)
        write_json(f"{slug}-rust-context.json", rust_context)
        selected = rust_context.get("selected_checkpoint")
        if not selected:
            reason = (
                "Rust scanner did not return a usable selected_checkpoint for "
                f"{volume_label}; top-level keys={sorted(rust_context.keys())}"
            )
            write_json(
                f"{slug}-native-record-body-dump.json",
                {
                    "status": "not_available",
                    "reason": reason,
                    "rust_context_artifact": f"{slug}-rust-context.json",
                },
            )
            write_json(
                f"{slug}-xfield-layout-summary.json",
                {"status": "not_available", "reason": reason},
            )
            write_json(
                f"{slug}-comparison.json",
                {
                    "status": "not_run",
                    "reason": reason,
                    "namespace": None,
                    "fixture_xattrs": None,
                    "matched": False,
                },
            )
            raise ProbeError(
                "oracle_inconclusive",
                reason,
            )
        go_apfs_observer = run_identitydump(raw_container)
        block_size = selected["container"]["block_size"]
        volume = selected["volumes"][0]
        root_paddr = volume["root_tree_lookup"]["paddr"]
        with open(raw_container, "rb", buffering=0) as handle:
            fs_tree = EX13.parse_fs_tree(handle, root_paddr, block_size)

        native_entries = EX13.reconstruct_entries(fs_tree["records"])
        namespace_comparison = compare_namespace(mounted_entries, native_entries)
        xattr_comparison = compare_fixture_xattrs(
            mounted_entries, fs_tree["records"], native_entries
        )
        layout_summary = resolve_xfield_layouts(mounted_entries, fs_tree["records"])

        return {
            "volume_label": volume_label,
            "fs_name": fs_name,
            "source": {
                "image_path": str(image_path),
                "raw_container_path": raw_container,
                "nomount_entities": nomount_entities,
                "image_size_bytes": image_path.stat().st_size,
            },
            "operations": operations,
            "mounted_oracle": mounted_oracle,
            "rust_context": rust_context,
            "go_apfs_observer": go_apfs_observer,
            "native_record_body_dump": {
                "selected_xid": selected["xid"],
                "block_size": block_size,
                "volume_oid": volume["volume_oid"],
                "root_tree_lookup": volume["root_tree_lookup"],
                "nodes": fs_tree["nodes"],
                "records": fs_tree["records"],
                "family_counts": EX13.family_counts(fs_tree["records"]),
                "xfield_layout_summary": layout_summary,
                "reconstructed_entries": native_entries,
            },
            "comparison": {
                "namespace": namespace_comparison,
                "fixture_xattrs": xattr_comparison,
                "matched": namespace_comparison["matched"] and xattr_comparison["matched"],
            },
            "xfield_layout_summary": layout_summary,
        }
    finally:
        if nomount_detach:
            detach_device(nomount_detach)
        if mounted_detach:
            detach_device(mounted_detach)
        shutil.rmtree(base, ignore_errors=True)


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
        "go": shutil.which("go"),
        "xattr": shutil.which("xattr"),
        "ditto": shutil.which("ditto"),
        "sw_vers": run(["sw_vers"]).stdout,
    }


def summary_for_cases(cases: list[dict]) -> dict:
    all_comparisons_match = all(case["comparison"]["matched"] for case in cases)
    unresolved_total = sum(
        case["xfield_layout_summary"]["unresolved_record_count"] for case in cases
    )
    verdict = (
        "validated_deterministic_xfield_layout"
        if all_comparisons_match and unresolved_total == 0
        else "xfield_layout_unsettled"
    )
    return {
        "status": "executed",
        "verdict": verdict,
        "case_count": len(cases),
        "all_comparisons_match": all_comparisons_match,
        "unresolved_xfield_record_count": unresolved_total,
        "cases": [
            {
                "volume_label": case["volume_label"],
                "fs_name": case["fs_name"],
                "selected_xid": case["native_record_body_dump"]["selected_xid"],
                "record_count": len(case["native_record_body_dump"]["records"]),
                "family_counts": case["native_record_body_dump"]["family_counts"],
                "mounted_summary": case["mounted_oracle"]["summary"],
                "comparison": {
                    "matched": case["comparison"]["matched"],
                    "namespace_matched": case["comparison"]["namespace"]["matched"],
                    "fixture_xattrs_matched": case["comparison"]["fixture_xattrs"]["matched"],
                    "namespace_mismatch_count": len(
                        case["comparison"]["namespace"]["mismatches"]
                    ),
                    "fixture_xattr_mismatch_count": len(
                        case["comparison"]["fixture_xattrs"]["mismatches"]
                    ),
                },
                "xfield_layout_summary": compact_layout_summary(
                    case["xfield_layout_summary"]
                ),
            }
            for case in cases
        ],
        "implementation_note": (
            "EX-14 is a Python-first parser gate. Rust was used only for the "
            "validated EX-12 checkpoint/root context; no Rust FS-record body "
            "decoder or product namespace output was added by this probe."
        ),
    }


def main() -> int:
    write_json("environment.json", environment())
    try:
        cases = [
            run_case("EX14CI", "APFS"),
            run_case("EX14CS", "Case-sensitive APFS"),
        ]
        for case in cases:
            slug = case["volume_label"].lower()
            write_json(f"{slug}-fixture-operations.json", case["operations"])
            write_json(f"{slug}-mounted-posix-oracle.json", case["mounted_oracle"])
            write_json(
                f"{slug}-native-record-body-dump.json",
                case["native_record_body_dump"],
            )
            write_json(
                f"{slug}-xfield-layout-summary.json",
                case["xfield_layout_summary"],
            )
            write_json(f"{slug}-go-apfs-record-observer.json", case["go_apfs_observer"])
            write_json(f"{slug}-comparison.json", case["comparison"])

        combined_layout = {
            case["volume_label"]: case["xfield_layout_summary"] for case in cases
        }
        write_json("xfield-layout-summary.json", combined_layout)
        write_json("comparison.json", {case["volume_label"]: case["comparison"] for case in cases})
        summary = summary_for_cases(cases)
        write_json("summary.json", summary)
        return 0 if summary["verdict"] == "validated_deterministic_xfield_layout" else 1
    except ProbeError as err:
        write_json(
            "summary.json",
            {"status": "executed", "verdict": err.verdict, "detail": err.detail},
        )
        return 1
    except Exception as err:  # pragma: no cover - experiment failure artifact
        write_json(
            "summary.json",
            {
                "status": "executed",
                "verdict": "probe_exception",
                "detail": f"{type(err).__name__}: {err}",
            },
        )
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
