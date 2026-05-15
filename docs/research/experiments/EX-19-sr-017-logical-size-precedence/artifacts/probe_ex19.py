#!/usr/bin/env python3
"""EX-19: SR-017 logical-size precedence on a same-run APFS fixture.

Builds a fixture with ordinary, sparse, clone, hard-link, symlink, and
ditto --hfsCompression cases. Captures POSIX st_size as the per-entry
oracle. Runs the patched Rust scanner to get `FsRecordDump.records`.
For each inode, applies SR-017 step-by-step and asserts the picked
size equals `st_size`.
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

INODE_HAS_UNCOMPRESSED_SIZE = 0x0004_0000
UF_COMPRESSED = 0x20

DECMPFS_MAGIC = 0x636D7066  # 'fpmc' little-endian becomes 'cmpf' as int


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
    """Build the SR-017 corpus.

    Returns a list of operation records. Each entry that introduces an
    inode we care about also records the expected logical-size oracle
    later (via stat); we just record the path here.
    """
    operations: list[dict] = []

    plain = root / "ordinary.txt"
    create_file(plain, b"Hello, SR-017 ordinary case.\n")
    operations.append({"step": "create ordinary.txt", "path": "ordinary.txt"})

    # Sparse file: 1 MiB hole between head and tail markers.
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

    # Compressed: build a source file outside the image, then use
    # `ditto --hfsCompression` into the image so APFS materializes
    # decmpfs + UF_COMPRESSED.
    compressible_payload = b"compressible " * 4096  # 53 KiB of repeating data
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


# ---- SR-017 precedence -------------------------------------------------- #

def decode_decmpfs_header(payload_hex: str) -> dict | None:
    try:
        data = bytes.fromhex(payload_hex)
    except ValueError:
        return None
    if len(data) < 16:
        return None
    magic_le = int.from_bytes(data[0:4], "little")
    magic_be = int.from_bytes(data[0:4], "big")
    compression_type = int.from_bytes(data[4:8], "little")
    uncompressed_size = int.from_bytes(data[8:16], "little")
    return {
        "magic_le_hex": f"{magic_le:#010x}",
        "magic_be_hex": f"{magic_be:#010x}",
        "compression_type": compression_type,
        "uncompressed_size": uncompressed_size,
    }


def index_records(records: list[dict]) -> dict[int, dict]:
    """Group decoded FsRecordRow entries by object_id, collecting all xattrs
    and inode bodies and any dir_rec linking back to a name."""
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


def apply_sr017(entry: dict, kind: str) -> dict:
    """Apply SR-017 precedence to one indexed inode bundle.

    Returns:
      {
        "kind": kind,
        "picked": "step_name",
        "picked_value": int | None,
        "candidates": {... per-step value ...},
        "fail_closed_reason": Optional[str],
      }
    """
    inode = entry.get("inode") or {}
    internal_flags = inode.get("internal_flags") or 0
    has_uncompressed_size = bool(internal_flags & INODE_HAS_UNCOMPRESSED_SIZE)
    inode_uncompressed_size = inode.get("uncompressed_size")
    dstream = inode.get("dstream") or {}
    dstream_size = dstream.get("size")
    sparse_bytes = inode.get("sparse_bytes")

    decmpfs_xattr = entry["xattrs"].get("com.apple.decmpfs")
    decmpfs_header = decode_decmpfs_header(decmpfs_xattr.get("payload_hex", "")) if decmpfs_xattr else None
    symlink_xattr = entry["xattrs"].get("com.apple.fs.symlink")
    symlink_target_len = None
    if symlink_xattr and symlink_xattr.get("embedded"):
        text = symlink_xattr.get("payload_utf8") or ""
        text = text.rstrip("\x00")
        symlink_target_len = len(text.encode("utf-8"))

    candidates = {
        "internal_flags_hex": f"{internal_flags:#x}",
        "has_uncompressed_size": has_uncompressed_size,
        "inode_uncompressed_size": inode_uncompressed_size,
        "j_dstream_size": dstream_size,
        "j_dstream_alloced_size": dstream.get("alloced_size"),
        "sparse_bytes": sparse_bytes,
        "decmpfs_header": decmpfs_header,
        "symlink_target_len": symlink_target_len,
    }

    if kind == "symlink":
        if symlink_target_len is None:
            return {
                "kind": kind,
                "picked": "fail_closed",
                "picked_value": None,
                "candidates": candidates,
                "fail_closed_reason": "symlink missing com.apple.fs.symlink xattr",
            }
        return {
            "kind": kind,
            "picked": "symlink_target_len",
            "picked_value": symlink_target_len,
            "candidates": candidates,
            "fail_closed_reason": None,
        }

    # Compressed: prefer flag → inode.uncompressed_size; else decmpfs header.
    if decmpfs_xattr is not None:
        if has_uncompressed_size and inode_uncompressed_size is not None:
            return {
                "kind": kind,
                "picked": "inode_uncompressed_size",
                "picked_value": inode_uncompressed_size,
                "candidates": candidates,
                "fail_closed_reason": None,
            }
        if decmpfs_header is not None:
            return {
                "kind": kind,
                "picked": "decmpfs_header_uncompressed_size",
                "picked_value": decmpfs_header["uncompressed_size"],
                "candidates": candidates,
                "fail_closed_reason": None,
            }
        return {
            "kind": kind,
            "picked": "fail_closed",
            "picked_value": None,
            "candidates": candidates,
            "fail_closed_reason": "compressed inode without UNCOMPRESSED_SIZE flag or decmpfs header",
        }

    # Ordinary/sparse/clone/hard-link: dstream size.
    if dstream_size is not None:
        return {
            "kind": kind,
            "picked": "j_dstream_size",
            "picked_value": dstream_size,
            "candidates": candidates,
            "fail_closed_reason": None,
        }
    return {
        "kind": kind,
        "picked": "zero_fallback",
        "picked_value": 0,
        "candidates": candidates,
        "fail_closed_reason": None,
    }


def precedence_kind(entry: dict) -> str:
    inode = entry.get("inode") or {}
    mode = inode.get("mode") or 0
    if (mode & 0xF000) == 0xA000:
        return "symlink"
    if "com.apple.decmpfs" in entry["xattrs"]:
        return "compressed"
    return "regular"


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
        "compression_tool": shutil.which("compression_tool"),
        "sw_vers": sw_vers.stdout,
    }


# ---- driver -------------------------------------------------------------- #

def main() -> int:
    write_json("environment.json", environment())
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex19-", dir="/tmp"))
    image_path = base / "EX19CI.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_detach = None
    nomount_detach = None
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
                "EX19CI",
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_fixture(mountpoint)
        oracle = snapshot_oracle(mountpoint)
        write_json("ex19-fixture-operations.json", {"operations": operations})
        write_json("ex19-mounted-posix-oracle.json", {"entries": oracle})
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
        write_json("ex19-rust-records.json", records)

        index = index_records(records)
        # Map mounted entries (which carry inode -> path) to indexed bundles.
        oracle_by_inode = {entry["inode"]: entry for entry in oracle if entry["type"] != "dir"}
        rows: list[dict] = []
        mismatches: list[dict] = []
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
            result = apply_sr017(bundle, kind)
            row = {
                "path": oracle_entry["path"],
                "inode": inode_id,
                "mounted_st_size": oracle_entry["st_size"],
                "mounted_st_flags": oracle_entry["st_flags"],
                "compressed_flag": oracle_entry.get("compressed_flag", False),
                "precedence_kind": kind,
                "picked": result["picked"],
                "picked_value": result["picked_value"],
                "candidates": result["candidates"],
                "fail_closed_reason": result["fail_closed_reason"],
                "matches_st_size": result["picked_value"] == oracle_entry["st_size"],
            }
            rows.append(row)
            if not row["matches_st_size"]:
                mismatches.append(
                    {
                        "path": oracle_entry["path"],
                        "expected": oracle_entry["st_size"],
                        "picked": result["picked"],
                        "picked_value": result["picked_value"],
                        "candidates": result["candidates"],
                    }
                )

        precedence = {
            "rows": rows,
            "mismatches": mismatches,
            "matched": not mismatches,
            "row_count": len(rows),
        }
        write_json("ex19-precedence-table.json", precedence)

        if precedence["matched"]:
            verdict = "validated_sr_017_precedence"
            detail = (
                f"{len(rows)} entries: all SR-017 picks equal mounted st_size."
            )
        else:
            compression_only = all(m.get("picked") in ("fail_closed", "decmpfs_header_uncompressed_size",
                                                       "inode_uncompressed_size", "zero_fallback")
                                  and m.get("path", "").endswith("compressed.txt")
                                  for m in mismatches)
            if compression_only and len(mismatches) == 1:
                verdict = "validated_non_compressed_only"
                detail = (
                    "Non-compressed cases (ordinary/sparse/clone/hard/symlink) match SR-017; "
                    "compressed case mismatches and is scoped for EX-19b."
                )
            else:
                verdict = "precedence_gap"
                detail = f"{len(mismatches)} mismatches; see ex19-precedence-table.json"
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["row_count"] = len(rows)
        summary["mismatch_count"] = len(mismatches)
        write_json("summary.json", summary)
        return 0 if verdict in {"validated_sr_017_precedence"} else 1
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
