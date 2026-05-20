#!/usr/bin/env python3
"""EX-26: SR-019 sparse + decmpfs allocated-size precedence on a same-run APFS fixture.

Extends the EX-22 fixture with three sparse and three decmpfs variants,
and tests three hypotheses that would lift SR-019's two remaining
fail-closed branches:

  - Hypothesis A `sparse_alloc_minus_sparse_bytes`:
      for regular + dstream + INO_EXT_TYPE_SPARSE_BYTES,
      ``alloced_size - sparse_bytes == st_blocks * 512``.

  - Hypothesis B `decmpfs_fork_stored`:
      for decmpfs files whose compression_type byte indicates
      resource-fork storage (types 4-6), the file's primary dstream
      `alloced_size == st_blocks * 512`.

  - Hypothesis C `decmpfs_xattr_stored`:
      for decmpfs files whose compression_type byte indicates
      xattr-inline storage (types 7-9), the picked value is
      ``Some(0)`` (the compressed bytes live in the xattr; the data
      fork has no extents).

Verdict slugs emitted in ``summary.json``:

  - ``validated_sparse_and_decmpfs`` (Hyp A + B + C hold)
  - ``validated_sparse_only`` (Hyp A holds; B or C fails)
  - ``oracle_inconclusive_sparse`` (Hyp A fails)
  - ``oracle_inconclusive_overall`` (Rust did not publish selected_checkpoint)

Per-row breakdown lives in ``ex26-precedence-table.json``;
``ex26-rust-records.json`` and ``ex26-mounted-posix-oracle.json``
are kept for cross-reference.
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

DECMPFS_XATTR = "com.apple.decmpfs"
DECMPFS_MAGIC = b"fpmc"


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


def create_sparse_simple(path: Path, hole: int, head: bytes = b"HEAD", tail: bytes = b"TAIL") -> None:
    """Sparse file: HEAD at offset 0, TAIL at end-of-hole, hole in between."""
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


def create_sparse_chunked(path: Path, total: int, chunk: int, stride: int) -> None:
    """Sparse file alternating ``chunk`` bytes of data and ``stride - chunk`` hole until total bytes."""
    written = 0
    pattern = b"X" * chunk
    with path.open("wb") as handle:
        offset = 0
        while offset < total:
            handle.seek(offset)
            handle.write(pattern)
            written += chunk
            offset += stride
        # Make sure final size is `total`:
        handle.truncate(total)
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass
    sync_directory(path.parent)
    settle()


def build_fixture(root: Path, source_root: Path) -> list[dict]:
    """Build the EX-22 baseline + EX-26 extensions in ``root``.

    ``source_root`` is a mutable scratch directory outside the APFS image
    (used as ditto's source-of-truth for the compressed cases — ditto
    refuses to operate in-place).
    """
    operations: list[dict] = []

    # --- EX-22 baseline (kept for cross-reference; subset of EX-26 cases)

    plain = root / "ordinary.txt"
    create_file(plain, b"Hello, SR-019 ordinary case.\n")
    operations.append({"step": "create ordinary.txt", "path": "ordinary.txt"})

    sparse_size = 1024 * 1024 + 4321
    sparse = root / "sparse.bin"
    create_sparse_simple(sparse, sparse_size)
    operations.append(
        {"step": "create sparse.bin (HEAD/TAIL, ~1 MiB hole)", "path": "sparse.bin", "hole_size": sparse_size}
    )

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

    # --- EX-26 sparse extensions

    sparse_medium = root / "sparse-medium.bin"
    create_sparse_simple(sparse_medium, hole=10 * 1024 * 1024)  # 10 MiB
    operations.append({"step": "create sparse-medium.bin (~10 MiB hole)", "path": "sparse-medium.bin"})

    sparse_large = root / "sparse-large.bin"
    create_sparse_simple(sparse_large, hole=50 * 1024 * 1024)  # 50 MiB; small enough for 160m image
    operations.append({"step": "create sparse-large.bin (~50 MiB hole)", "path": "sparse-large.bin"})

    sparse_chunked = root / "sparse-chunked.bin"
    create_sparse_chunked(sparse_chunked, total=2 * 1024 * 1024, chunk=4096, stride=64 * 1024)
    operations.append(
        {"step": "create sparse-chunked.bin (4 KiB data every 64 KiB, 2 MiB total)", "path": "sparse-chunked.bin"}
    )

    # --- EX-26 decmpfs extensions

    # Variant 1: small highly-compressible JSON. ditto --hfsCompression should
    # produce a type-7-class xattr-stored shape (compressed bytes inline).
    compressible_payload = b"compressible " * 4096  # ~52 KiB
    src_compressible = source_root / "to_compress.txt"
    src_compressible.write_bytes(compressible_payload)
    full_sync(src_compressible)
    sync_directory(source_root)
    settle()
    compressed = root / "compressed.txt"
    proc = run(["ditto", "--hfsCompression", str(src_compressible), str(compressed)])
    if proc.returncode != 0:
        raise ProbeError("fixture_build", f"ditto small step failed:\n{proc.stderr}")
    sync_directory(root)
    settle()
    operations.append({"step": "ditto --hfsCompression -> compressed.txt", "path": "compressed.txt"})

    # Variant 2: larger compressible payload (~256 KiB). At this size ditto
    # tends to switch to type-4-class fork-stored shape (compressed bytes in
    # com.apple.ResourceFork xattr with its own dstream).
    big_payload = b"compressible " * (256 * 1024 // 13 + 1)
    big_payload = big_payload[: 256 * 1024]
    src_big = source_root / "big_to_compress.bin"
    src_big.write_bytes(big_payload)
    full_sync(src_big)
    sync_directory(source_root)
    settle()
    big_compressed = root / "compressed-big.bin"
    proc = run(["ditto", "--hfsCompression", str(src_big), str(big_compressed)])
    if proc.returncode != 0:
        raise ProbeError("fixture_build", f"ditto big step failed:\n{proc.stderr}")
    sync_directory(root)
    settle()
    operations.append(
        {"step": "ditto --hfsCompression -> compressed-big.bin (~256 KiB compressible)", "path": "compressed-big.bin"}
    )

    # Variant 3: random/incompressible payload of moderate size. ditto often
    # leaves these uncompressed but still sets the UF_COMPRESSED flag; this
    # variant tests the boundary where ditto chose not to compress.
    import os as _os
    incompressible_payload = _os.urandom(128 * 1024)
    src_random = source_root / "to_not_compress.bin"
    src_random.write_bytes(incompressible_payload)
    full_sync(src_random)
    sync_directory(source_root)
    settle()
    random_dest = root / "compressed-random.bin"
    proc = run(["ditto", "--hfsCompression", str(src_random), str(random_dest)])
    if proc.returncode != 0:
        raise ProbeError("fixture_build", f"ditto random step failed:\n{proc.stderr}")
    sync_directory(root)
    settle()
    operations.append(
        {"step": "ditto --hfsCompression -> compressed-random.bin (incompressible)", "path": "compressed-random.bin"}
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


# ---- decmpfs header helpers --------------------------------------------- #

def decmpfs_header(payload_hex: str) -> dict | None:
    """Parse a com.apple.decmpfs xattr payload.

    Layout (little-endian):
      bytes 0..4   magic 'fpmc'
      bytes 4..8   compression_type (u32)
      bytes 8..16  uncompressed_size (u64)
    """
    try:
        raw = bytes.fromhex(payload_hex)
    except ValueError:
        return None
    if len(raw) < 16:
        return None
    if raw[:4] != DECMPFS_MAGIC:
        return None
    return {
        "magic_ok": True,
        "compression_type": int.from_bytes(raw[4:8], "little"),
        "uncompressed_size": int.from_bytes(raw[8:16], "little"),
        "payload_len": len(raw),
    }


def decmpfs_storage_class(compression_type: int) -> str:
    """Classify the decmpfs compression_type byte by where the bytes live.

    Apple's published list (zlib/lzvn/lzfse):
      types 1,2  : reserved / uncompressed
      types 3,4  : zlib  — 3 = xattr-inline, 4 = resource fork
      types 7,8  : lzvn  — 7 = xattr-inline, 8 = resource fork
      types 11,12: lzfse — 11 = xattr-inline, 12 = resource fork
      type  5,6  : raw/dataless — uncompressed inline; treated as fork-stored here for the
                    purposes of EX-26 because the data fork is empty.

    Odd types live inline in the xattr; even types live in the resource fork.
    """
    if compression_type in (3, 7, 11):
        return "xattr_inline"
    if compression_type in (4, 8, 12):
        return "fork_stored"
    if compression_type in (1, 2, 5, 6):
        return "uncompressed_or_dataless"
    return f"unknown({compression_type})"


# ---- SR-019 precedence with EX-26 hypotheses ---------------------------- #

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
    if DECMPFS_XATTR in entry["xattrs"]:
        return "compressed"
    if masked == S_IFREG:
        return "regular"
    return f"other({masked:#x})"


def apply_ex26(entry: dict, kind: str) -> dict:
    """Apply SR-019 with EX-26 hypothesis lifts."""
    inode = entry.get("inode") or {}
    dstream = inode.get("dstream") or {}
    alloced_size = dstream.get("alloced_size")
    sparse_bytes = inode.get("sparse_bytes")

    candidates: dict[str, Any] = {
        "kind": kind,
        "j_dstream_alloced_size": alloced_size,
        "j_dstream_size": dstream.get("size"),
        "sparse_bytes": sparse_bytes,
        "has_decmpfs_xattr": DECMPFS_XATTR in entry["xattrs"],
        "has_dstream_xfield": bool(dstream),
    }

    if kind == "regular":
        if alloced_size is None:
            return {
                "kind": kind,
                "hypothesis": "no_dstream",
                "picked": "fail_closed",
                "picked_value": None,
                "candidates": candidates,
                "fail_closed_reason": "regular file without dstream xfield (unexpected: not decmpfs)",
            }
        if sparse_bytes is not None:
            # Hypothesis A
            picked = alloced_size - sparse_bytes
            return {
                "kind": kind,
                "hypothesis": "A_sparse_alloc_minus_sparse_bytes",
                "picked": "alloced_size_minus_sparse_bytes",
                "picked_value": picked,
                "candidates": candidates,
                "fail_closed_reason": None,
            }
        # non-sparse regular: same as EX-22's emit branch
        return {
            "kind": kind,
            "hypothesis": "ex22_dstream_alloced_size",
            "picked": "j_dstream_alloced_size",
            "picked_value": alloced_size,
            "candidates": candidates,
            "fail_closed_reason": None,
        }

    if kind == "compressed":
        # EX-26 finding (revised Hypothesis F, supersedes B/C/D):
        # For decmpfs files on macOS-produced fixtures, the compressed
        # bytes live in one of two xattrs:
        #   - com.apple.decmpfs.stream_dstream      (xattr-stored, stream-backed)
        #   - com.apple.ResourceFork.stream_dstream (fork-stored, stream-backed)
        # The inode has no primary dstream. The allocated bytes are the
        # sum of those xattr dstreams' alloced_size. Embedded (non-stream)
        # decmpfs xattrs carry the compressed bytes inline and contribute 0.
        decmpfs_xattr = entry["xattrs"].get(DECMPFS_XATTR) or {}
        rfork_xattr = entry["xattrs"].get("com.apple.ResourceFork") or {}
        decmpfs_inline_header = decmpfs_header(decmpfs_xattr.get("payload_hex", ""))
        candidates["decmpfs_xattr_flags"] = decmpfs_xattr.get("flags")
        candidates["decmpfs_xattr_stream_alloced"] = (
            (decmpfs_xattr.get("stream_dstream") or {}).get("alloced_size")
        )
        candidates["rfork_xattr_flags"] = rfork_xattr.get("flags")
        candidates["rfork_xattr_stream_alloced"] = (
            (rfork_xattr.get("stream_dstream") or {}).get("alloced_size")
        )
        candidates["decmpfs_inline_compression_type"] = (
            decmpfs_inline_header["compression_type"] if decmpfs_inline_header else None
        )
        candidates["decmpfs_inline_uncompressed_size"] = (
            decmpfs_inline_header["uncompressed_size"] if decmpfs_inline_header else None
        )

        decmpfs_share = candidates["decmpfs_xattr_stream_alloced"] or 0
        rfork_share = candidates["rfork_xattr_stream_alloced"] or 0
        primary_share = alloced_size or 0
        picked = primary_share + decmpfs_share + rfork_share
        return {
            "kind": kind,
            "hypothesis": "F_decmpfs_stream_dstream_sum",
            "picked": "primary_plus_decmpfs_plus_rfork_stream_alloced",
            "picked_value": picked,
            "candidates": candidates,
            "fail_closed_reason": None,
        }

    if kind in {"symlink", "dir"}:
        return {
            "kind": kind,
            "hypothesis": "zero",
            "picked": "zero",
            "picked_value": 0,
            "candidates": candidates,
            "fail_closed_reason": None,
        }

    return {
        "kind": kind,
        "hypothesis": "unknown_kind",
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
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex26-", dir="/tmp"))
    source_root = Path(tempfile.mkdtemp(prefix="apfsfi-ex26-src-", dir="/tmp"))
    image_path = base / "EX26CI.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_detach: str | None = None
    nomount_detach: str | None = None
    summary: dict[str, Any] = {
        "status": "executed",
        "verdict": "pending",
        "verdict_detail": "",
    }
    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "256m",
                "-fs",
                "APFS",
                "-volname",
                "EX26CI",
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_fixture(mountpoint, source_root)
        oracle = snapshot_oracle(mountpoint)
        write_json("ex26-fixture-operations.json", {"operations": operations})
        write_json("ex26-mounted-posix-oracle.json", {"entries": oracle})
        detach_device(mounted_detach)
        mounted_detach = None
        time.sleep(0.4)

        _, nomount_detach, raw_container = attach_nomount(image_path)
        rust_scan = run_rust_scan(raw_container)
        sel = rust_scan.get("selected_checkpoint")
        if not sel:
            raise ProbeError(
                "oracle_inconclusive_overall",
                "Rust did not publish selected_checkpoint; rerun EX-15 first",
            )
        volume = sel["volumes"][0]
        dump = volume.get("fs_record_dump") or {}
        records = dump.get("records") or []
        write_json(
            "ex26-rust-records.json",
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
        sparse_mismatches: list[dict] = []
        decmpfs_mismatches: list[dict] = []
        other_mismatches: list[dict] = []
        fail_closed_rows: list[dict] = []
        sparse_rows: list[dict] = []
        decmpfs_rows: list[dict] = []

        for oracle_entry in sorted(oracle_by_inode.values(), key=lambda e: e["path"]):
            inode_id = oracle_entry["inode"]
            bundle = index.get(inode_id)
            if bundle is None or bundle.get("inode") is None:
                other_mismatches.append(
                    {
                        "path": oracle_entry["path"],
                        "reason": f"no inode record found for inode {inode_id}",
                    }
                )
                continue
            kind = precedence_kind(bundle)
            result = apply_ex26(bundle, kind)
            row = {
                "path": oracle_entry["path"],
                "inode": inode_id,
                "mounted_st_size": oracle_entry["st_size"],
                "mounted_st_blocks": oracle_entry["st_blocks"],
                "mounted_st_blocks_x_512": oracle_entry["st_blocks_x_512"],
                "compressed_flag": oracle_entry.get("compressed_flag", False),
                "precedence_kind": kind,
                "hypothesis": result["hypothesis"],
                "picked": result["picked"],
                "picked_value": result["picked_value"],
                "candidates": result["candidates"],
                "fail_closed_reason": result["fail_closed_reason"],
            }
            is_sparse = result["candidates"].get("sparse_bytes") is not None
            is_decmpfs = kind == "compressed"
            if is_sparse:
                sparse_rows.append(row)
            if is_decmpfs:
                decmpfs_rows.append(row)
            if result["picked"] == "fail_closed":
                fail_closed_rows.append(row)
                rows.append(row)
                continue
            row["matches_oracle"] = result["picked_value"] == oracle_entry["st_blocks_x_512"]
            rows.append(row)
            if not row["matches_oracle"]:
                mismatch_entry = {
                    "path": oracle_entry["path"],
                    "expected_st_blocks_x_512": oracle_entry["st_blocks_x_512"],
                    "picked": result["picked"],
                    "picked_value": result["picked_value"],
                    "hypothesis": result["hypothesis"],
                    "candidates": result["candidates"],
                }
                if is_sparse:
                    sparse_mismatches.append(mismatch_entry)
                elif is_decmpfs:
                    decmpfs_mismatches.append(mismatch_entry)
                else:
                    other_mismatches.append(mismatch_entry)

        precedence = {
            "rows": rows,
            "sparse_rows": sparse_rows,
            "decmpfs_rows": decmpfs_rows,
            "sparse_mismatches": sparse_mismatches,
            "decmpfs_mismatches": decmpfs_mismatches,
            "other_mismatches": other_mismatches,
            "fail_closed_rows": fail_closed_rows,
            "sparse_validated": not sparse_mismatches and bool(sparse_rows),
            "decmpfs_validated": not decmpfs_mismatches and bool(decmpfs_rows),
            "row_count": len(rows),
            "sparse_row_count": len(sparse_rows),
            "decmpfs_row_count": len(decmpfs_rows),
            "fail_closed_count": len(fail_closed_rows),
        }
        write_json("ex26-precedence-table.json", precedence)

        if other_mismatches:
            verdict = "oracle_inconclusive_overall"
            detail = (
                f"{len(other_mismatches)} mismatches in non-sparse, non-decmpfs rows; "
                "EX-22 baseline must hold before EX-26 hypotheses can be evaluated."
            )
        elif precedence["sparse_validated"] and precedence["decmpfs_validated"]:
            verdict = "validated_sparse_and_decmpfs"
            detail = (
                f"Hypothesis A (alloced_size - sparse_bytes) holds across "
                f"{len(sparse_rows)} sparse rows; Hypothesis F (primary + "
                f"decmpfs.stream_dstream + ResourceFork.stream_dstream) holds "
                f"across {len(decmpfs_rows)} decmpfs rows. SR-019 can lift both branches."
            )
        elif precedence["sparse_validated"]:
            verdict = "validated_sparse_only"
            detail = (
                f"Hypothesis A holds across {len(sparse_rows)} sparse rows; "
                f"decmpfs branch (Hypothesis F) has {len(decmpfs_mismatches)} "
                f"mismatches out of {len(decmpfs_rows)} rows."
            )
        elif not sparse_rows:
            verdict = "oracle_inconclusive_sparse"
            detail = "no sparse rows in fixture — re-check fixture build"
        else:
            verdict = "oracle_inconclusive_sparse"
            detail = (
                f"Hypothesis A fails: {len(sparse_mismatches)}/"
                f"{len(sparse_rows)} sparse rows mismatch st_blocks*512."
            )

        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["sparse_row_count"] = len(sparse_rows)
        summary["sparse_mismatch_count"] = len(sparse_mismatches)
        summary["decmpfs_row_count"] = len(decmpfs_rows)
        summary["decmpfs_mismatch_count"] = len(decmpfs_mismatches)
        summary["other_mismatch_count"] = len(other_mismatches)
        summary["fail_closed_count"] = len(fail_closed_rows)
        write_json("summary.json", summary)
        return 0 if verdict in {"validated_sparse_and_decmpfs", "validated_sparse_only"} else 1
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
        shutil.rmtree(source_root, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
