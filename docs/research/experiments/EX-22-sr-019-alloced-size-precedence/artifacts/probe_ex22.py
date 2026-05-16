#!/usr/bin/env python3
"""EX-22: SR-019 allocated-size precedence on a same-run APFS fixture.

Builds the same fixture shape as EX-19 (ordinary, sparse, clone,
hard link, symlink, ditto --hfsCompression). Captures POSIX
``st_blocks * 512`` as the per-inode oracle and `st_size` for
context. Runs the existing Rust scanner to get
``FsRecordDump.records``. For each inode, applies SR-019
precedence:

  - regular file with a dstream xfield -> Some(dstream.alloced_size)
  - regular file without a dstream and a com.apple.decmpfs xattr ->
    fail_closed (None, row recorded in fail_closed_rows)
  - symlink -> 0
  - directory -> 0
  - anything else -> fail_closed

and asserts that the picked value equals ``st_blocks * 512`` for
rows the rule emits.

The probe also captures, for diagnostic purposes only:

  - file_extent (raw_type 0x8) count per inode from the FS-tree
    family histogram (Rust body decoding for this family is not in
    the v1 allowlist, so byte sums are out of scope here).
  - extent_reference (raw_type 0x2) count for the volume.

Verdict slugs emitted in ``summary.json``:

  - ``validated_sr_019_precedence`` if Hypothesis A holds.
  - ``partial_validated`` if at least one non-compressed row
    diverges from the oracle.
  - ``oracle_inconclusive`` if Rust did not publish
    ``selected_checkpoint`` (rerun EX-15-style upstream gate).

The fixture is identical in shape to EX-19's so the experimental
overhead is small and the oracle pairing is unambiguous.
"""

from __future__ import annotations

import datetime as _dt
import fcntl
import json
import os
import platform
import plistlib
import shutil
import stat
import subprocess
import tempfile
import time
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
REPO_ROOT = ARTIFACT_DIR.parents[4]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"

APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
F_FULLFSYNC = 51

UF_COMPRESSED = 0x20
S_IFMT = 0xF000
S_IFREG = 0x8000
S_IFLNK = 0xA000
S_IFDIR = 0x4000


class ProbeError(RuntimeError):
    def __init__(self, verdict: str, detail: str) -> None:
        super().__init__(detail)
        self.verdict = verdict
        self.detail = detail


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
        json.dumps(payload, indent=2, sort_keys=True, default=_json_default) + "\n"
    )


def _json_default(obj: Any) -> Any:
    if isinstance(obj, bytes):
        return obj.hex()
    raise TypeError(f"unserializable: {type(obj).__name__}")


# ---- fixture helpers ----------------------------------------------------- #

def full_sync(path: Path) -> None:
    with path.open("ab") as handle:
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass


def sync_directory(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def settle() -> None:
    run(["sync"])
    time.sleep(0.2)


def create_file(path: Path, payload: bytes | str) -> None:
    if isinstance(payload, bytes):
        path.write_bytes(payload)
    else:
        path.write_text(payload)
    full_sync(path)
    sync_directory(path.parent)
    settle()


def create_sparse(path: Path, hole: int, head: bytes = b"HEAD", tail: bytes = b"TAIL") -> None:
    with path.open("wb") as handle:
        handle.write(head)
        handle.seek(hole - len(tail))
        handle.write(tail)
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass
    sync_directory(path.parent)
    settle()


def build_fixture(root: Path) -> list[dict]:
    operations: list[dict] = []

    plain = root / "ordinary.txt"
    create_file(plain, b"Hello, SR-019 ordinary case.\n")
    operations.append({"step": "create ordinary.txt", "path": "ordinary.txt"})

    sparse_size = 1024 * 1024 + 4321
    sparse = root / "sparse.bin"
    create_sparse(sparse, sparse_size)
    operations.append({"step": "create sparse.bin", "path": "sparse.bin", "hole_size": sparse_size})

    clone = root / "clone.txt"
    proc = run(["cp", "-c", str(plain), str(clone)])
    if proc.returncode != 0:
        raise ProbeError("fixture_build", f"clone step failed:\n{proc.stderr}")
    sync_directory(root)
    settle()
    operations.append({"step": "clone ordinary.txt -> clone.txt", "path": "clone.txt"})

    hard = root / "hard.txt"
    os.link(plain, hard)
    sync_directory(root)
    settle()
    operations.append({"step": "hard link ordinary.txt <- hard.txt", "path": "hard.txt"})

    symlink = root / "link.txt"
    os.symlink("ordinary.txt", symlink)
    sync_directory(root)
    settle()
    operations.append({"step": "symlink link.txt -> ordinary.txt", "path": "link.txt"})

    compressible_payload = b"compressible " * 4096
    source_dir = root.parent / "src"
    source_dir.mkdir(exist_ok=True)
    source_file = source_dir / "to_compress.txt"
    source_file.write_bytes(compressible_payload)
    full_sync(source_file)
    sync_directory(source_dir)
    settle()
    compressed = root / "compressed.txt"
    proc = run(["ditto", "--hfsCompression", str(source_file), str(compressed)])
    if proc.returncode != 0:
        raise ProbeError(
            "fixture_build", f"ditto --hfsCompression step failed:\n{proc.stderr}"
        )
    sync_directory(root)
    settle()
    operations.append(
        {"step": "ditto --hfsCompression -> compressed.txt", "path": "compressed.txt"}
    )

    return operations


def entry_type(mode: int) -> str:
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "file"
    return f"other({stat.S_IFMT(mode):#x})"


def snapshot_oracle(root: Path) -> list[dict]:
    entries: list[dict] = []
    for current_root, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        if Path(current_root).name == ".fseventsd":
            continue
        dirnames[:] = [name for name in dirnames if name != ".fseventsd"]
        rel = Path(current_root).relative_to(root)
        st = os.lstat(current_root)
        entries.append(
            {
                "type": "dir",
                "path": "." if str(rel) == "." else str(rel),
                "inode": st.st_ino,
                "st_size": st.st_size,
                "st_blocks": st.st_blocks,
                "st_blocks_x_512": st.st_blocks * 512,
                "st_flags": st.st_flags,
                "compressed_flag": bool(st.st_flags & UF_COMPRESSED),
            }
        )
        for name in filenames:
            path = Path(current_root) / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            kind = entry_type(st.st_mode)
            entry: dict[str, Any] = {
                "type": kind,
                "path": str(rel_path),
                "inode": st.st_ino,
                "nlink": st.st_nlink,
                "st_size": st.st_size,
                "st_blocks": st.st_blocks,
                "st_blocks_x_512": st.st_blocks * 512,
                "st_flags": st.st_flags,
                "compressed_flag": bool(st.st_flags & UF_COMPRESSED),
            }
            if kind == "symlink":
                entry["symlink_target"] = os.readlink(path)
            entries.append(entry)
    return entries


# ---- image lifecycle ----------------------------------------------------- #

def attach_image(image_path: Path, mountpoint: Path) -> tuple[list[dict], str]:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)]
    )
    info = plistlib.loads(proc.stdout.encode("utf-8"))
    entities = info.get("system-entities", [])
    if not entities:
        raise ProbeError("attach_failed", "hdiutil attach returned no entities")
    return entities, entities[0]["dev-entry"]


def attach_nomount(image_path: Path) -> tuple[list[dict], str, str]:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-nomount", "-readonly", str(image_path)]
    )
    info = plistlib.loads(proc.stdout.encode("utf-8"))
    entities = info.get("system-entities", [])
    detach_dev = entities[0]["dev-entry"]
    container_dev = None
    for entity in entities:
        if entity.get("content-hint") == APFS_CONTAINER_HINT:
            container_dev = entity.get("dev-entry")
            break
    if container_dev is None:
        raise ProbeError("missing_apfs_container", "no APFS container after -nomount")
    if container_dev.startswith("/dev/disk"):
        container_dev = "/dev/rdisk" + container_dev[len("/dev/disk") :]
    return entities, detach_dev, container_dev


def detach_device(device: str) -> None:
    run(["hdiutil", "detach", device])


def run_rust_scan(raw_container: str) -> dict:
    proc = run_checked(
        ["cargo", "run", "--quiet", "--bin", "apfs-fastindex-scan", "--", raw_container],
        cwd=RUST_CRATE_DIR,
    )
    return json.loads(proc.stdout)


# ---- SR-019 precedence -------------------------------------------------- #

def index_records(records: list[dict]) -> dict[int, dict]:
    out: dict[int, dict] = {}
    for record in records:
        oid = record["object_id"]
        entry = out.setdefault(
            oid,
            {
                "object_id": oid,
                "inode": None,
                "xattrs": {},
                "sibling_links": [],
                "dstream_id": None,
            },
        )
        family = record["family"]
        value = record["value"]
        if family == "inode":
            entry["inode"] = value
        elif family == "xattr":
            name = record["key"].get("name") if record["key"].get("kind") == "named" else None
            if name:
                entry["xattrs"][name] = value
        elif family == "sibling_link":
            entry["sibling_links"].append(value)
        elif family == "dstream_id":
            entry["dstream_id"] = value
    return out


def precedence_kind(entry: dict) -> str:
    inode = entry.get("inode") or {}
    mode = inode.get("mode") or 0
    masked = mode & S_IFMT
    if masked == S_IFLNK:
        return "symlink"
    if masked == S_IFDIR:
        return "dir"
    if "com.apple.decmpfs" in entry["xattrs"]:
        return "compressed"
    if masked == S_IFREG:
        return "regular"
    return f"other({masked:#x})"


def apply_sr019(entry: dict, kind: str) -> dict:
    """Apply SR-019 precedence to one indexed inode bundle."""
    inode = entry.get("inode") or {}
    dstream = inode.get("dstream") or {}
    alloced_size = dstream.get("alloced_size")

    candidates = {
        "kind": kind,
        "j_dstream_alloced_size": alloced_size,
        "j_dstream_size": dstream.get("size"),
        "sparse_bytes": inode.get("sparse_bytes"),
        "has_decmpfs_xattr": "com.apple.decmpfs" in entry["xattrs"],
        "has_dstream_xfield": dstream != {},
    }

    if kind == "regular":
        if alloced_size is None:
            return {
                "kind": kind,
                "picked": "fail_closed",
                "picked_value": None,
                "candidates": candidates,
                "fail_closed_reason": "regular file without dstream xfield (unexpected: not decmpfs)",
            }
        return {
            "kind": kind,
            "picked": "j_dstream_alloced_size",
            "picked_value": alloced_size,
            "candidates": candidates,
            "fail_closed_reason": None,
        }

    if kind == "compressed":
        return {
            "kind": kind,
            "picked": "fail_closed",
            "picked_value": None,
            "candidates": candidates,
            "fail_closed_reason": (
                "regular file with com.apple.decmpfs xattr; SR-019 v1 does not"
                " emit allocated_size for decmpfs cases (no validated oracle)"
            ),
        }

    if kind in {"symlink", "dir"}:
        return {
            "kind": kind,
            "picked": "zero",
            "picked_value": 0,
            "candidates": candidates,
            "fail_closed_reason": None,
        }

    return {
        "kind": kind,
        "picked": "fail_closed",
        "picked_value": None,
        "candidates": candidates,
        "fail_closed_reason": f"SR-019 has no emission rule for kind={kind}",
    }


def environment() -> dict:
    sw_vers = run(["sw_vers"])
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
        "ditto": shutil.which("ditto"),
        "sw_vers": sw_vers.stdout,
    }


# ---- driver -------------------------------------------------------------- #

def main() -> int:
    write_json("environment.json", environment())
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex22-", dir="/tmp"))
    image_path = base / "EX22CI.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_detach: str | None = None
    nomount_detach: str | None = None
    summary = {"status": "executed", "verdict": "pending", "verdict_detail": ""}
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
                "EX22CI",
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_fixture(mountpoint)
        oracle = snapshot_oracle(mountpoint)
        write_json("ex22-fixture-operations.json", {"operations": operations})
        write_json("ex22-mounted-posix-oracle.json", {"entries": oracle})
        detach_device(mounted_detach)
        mounted_detach = None
        time.sleep(0.4)

        _, nomount_detach, raw_container = attach_nomount(image_path)
        rust_scan = run_rust_scan(raw_container)
        sel = rust_scan.get("selected_checkpoint")
        if not sel:
            raise ProbeError(
                "oracle_inconclusive",
                "Rust did not publish selected_checkpoint; rerun EX-15 first",
            )
        volume = sel["volumes"][0]
        dump = volume.get("fs_record_dump") or {}
        records = dump.get("records") or []
        family_counts = {
            fc["raw_type"]: fc
            for fc in (dump.get("family_counts") or [])
        }
        write_json(
            "ex22-rust-records.json",
            {
                "records": records,
                "family_counts": dump.get("family_counts") or [],
                "leaf_record_count": dump.get("leaf_record_count"),
                "unsupported_record_count": dump.get("unsupported_record_count"),
            },
        )

        index = index_records(records)
        oracle_by_inode = {entry["inode"]: entry for entry in oracle if entry["type"] != "dir"}
        rows: list[dict] = []
        mismatches: list[dict] = []
        fail_closed_rows: list[dict] = []
        for oracle_entry in sorted(oracle_by_inode.values(), key=lambda e: e["path"]):
            inode_id = oracle_entry["inode"]
            bundle = index.get(inode_id)
            if bundle is None or bundle.get("inode") is None:
                mismatches.append(
                    {
                        "path": oracle_entry["path"],
                        "reason": f"no inode record found for inode {inode_id}",
                    }
                )
                continue
            kind = precedence_kind(bundle)
            result = apply_sr019(bundle, kind)
            row = {
                "path": oracle_entry["path"],
                "inode": inode_id,
                "mounted_st_size": oracle_entry["st_size"],
                "mounted_st_blocks": oracle_entry["st_blocks"],
                "mounted_st_blocks_x_512": oracle_entry["st_blocks_x_512"],
                "compressed_flag": oracle_entry.get("compressed_flag", False),
                "precedence_kind": kind,
                "picked": result["picked"],
                "picked_value": result["picked_value"],
                "candidates": result["candidates"],
                "fail_closed_reason": result["fail_closed_reason"],
            }
            if result["picked"] == "fail_closed":
                fail_closed_rows.append(row)
                rows.append(row)
                continue
            row["matches_oracle"] = result["picked_value"] == oracle_entry["st_blocks_x_512"]
            rows.append(row)
            if not row["matches_oracle"]:
                mismatches.append(
                    {
                        "path": oracle_entry["path"],
                        "expected_st_blocks_x_512": oracle_entry["st_blocks_x_512"],
                        "picked": result["picked"],
                        "picked_value": result["picked_value"],
                        "candidates": result["candidates"],
                    }
                )

        precedence = {
            "rows": rows,
            "mismatches": mismatches,
            "fail_closed_rows": fail_closed_rows,
            "matched": not mismatches,
            "row_count": len(rows),
            "fail_closed_count": len(fail_closed_rows),
            "family_counts_seen": [
                {
                    "raw_type": rt,
                    "name": fc["name"],
                    "count": fc["count"],
                    "in_v1_namespace_scope": fc["in_v1_namespace_scope"],
                }
                for rt, fc in sorted(family_counts.items())
            ],
        }
        write_json("ex22-precedence-table.json", precedence)

        if precedence["matched"]:
            verdict = "validated_sr_019_precedence"
            detail = (
                f"{len(rows)} entries: SR-019 picks match st_blocks*512 for "
                f"{len(rows) - len(fail_closed_rows)} emit-rows, "
                f"{len(fail_closed_rows)} rows recorded as fail_closed "
                f"(decmpfs / etc.) per SR-019 v1 precedence."
            )
        else:
            verdict = "partial_validated"
            detail = (
                f"{len(mismatches)} mismatches against st_blocks*512; "
                "see ex22-precedence-table.json for the case-class breakdown."
            )
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["row_count"] = len(rows)
        summary["mismatch_count"] = len(mismatches)
        summary["fail_closed_count"] = len(fail_closed_rows)
        write_json("summary.json", summary)
        return 0 if verdict == "validated_sr_019_precedence" else 1
    except ProbeError as err:
        summary["verdict"] = err.verdict
        summary["verdict_detail"] = err.detail
        write_json("summary.json", summary)
        return 1
    except Exception as err:
        summary["verdict"] = "probe_exception"
        summary["verdict_detail"] = f"{type(err).__name__}: {err}"
        write_json("summary.json", summary)
        return 1
    finally:
        if nomount_detach:
            detach_device(nomount_detach)
        if mounted_detach:
            detach_device(mounted_detach)
        shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
