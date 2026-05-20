#!/usr/bin/env python3
"""EX-27: clone-dedup via the extent-reference tree on a same-run APFS fixture.

Python-direct probe. Reads both the fs-tree and the extent-reference tree
off a detached `.dmg`, joins file-extent records with their phys-ext
refcounts, computes per-inode deduplicated allocated bytes, and compares
against macOS's `du -A` oracle.

Bridge to the existing Rust scanner: uses `apfs-fastindex-scan` only to
obtain the selected checkpoint, block size, and the resolved
`root_tree_lookup.paddr` + `extentref_tree_lookup.paddr` (the latter was
added in this commit). Everything else is parsed directly off the raw
device.

Verdict slugs:
  - validated_clone_dedup           Hypothesis A + B hold across the fixture
  - validated_clone_dedup_with_divergence  parity within tolerance but some
                                    documented per-shape divergence
  - oracle_inconclusive_clone_dedup Hypothesis A or B fails materially
  - probe_blocked_no_extentref      Rust did not surface extentref_tree_lookup
                                    (volume OMAP didn't carry a mapping)
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
import struct
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

# B-tree node layout constants (matches EX-13 and crates/apfs-fastindex/src/btree.rs).
OBJ_HEADER_SIZE = 32
BTREE_INFO_SIZE = 40
BTNODE_ROOT = 0x0001
BTNODE_LEAF = 0x0002
BTNODE_FIXED_KV_SIZE = 0x0004
OBJ_ID_MASK = (1 << 60) - 1
OBJ_TYPE_SHIFT = 60

# FS-record raw_types.
RAW_TYPE_EXTENT_REFERENCE = 0x2
RAW_TYPE_INODE = 0x3
RAW_TYPE_FILE_EXTENT = 0x8
RAW_TYPE_DIR_REC = 0x9


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


# ---- little-endian readers ---------------------------------------------- #

def le_u16(data: bytes, offset: int) -> int:
    return struct.unpack_from("<H", data, offset)[0]


def le_u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def le_i32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<i", data, offset)[0]


def le_u64(data: bytes, offset: int) -> int:
    return struct.unpack_from("<Q", data, offset)[0]


# ---- fixture helpers ---------------------------------------------------- #

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


def write_file(path: Path, payload: bytes) -> None:
    path.write_bytes(payload)
    full_sync(path)
    sync_directory(path.parent)
    settle()


def clone_file(src: Path, dst: Path) -> None:
    proc = run(["cp", "-c", str(src), str(dst)])
    if proc.returncode != 0:
        raise ProbeError("fixture_build", f"cp -c failed: {proc.stderr}")
    sync_directory(dst.parent)
    settle()


def rewrite_middle(path: Path, offset: int, length: int) -> None:
    """Overwrite ``length`` bytes at ``offset`` with random data, breaking
    extent sharing on that region without truncating the file. Forces the
    kernel to allocate new physical extents for the rewritten range."""
    fresh = os.urandom(length)
    with path.open("rb+") as handle:
        handle.seek(offset)
        handle.write(fresh)
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

    # EX-22 baseline (regression protection — these rows should match
    # st_blocks * 512 unchanged).
    ordinary = root / "ordinary.txt"
    write_file(ordinary, b"Hello, EX-27 ordinary case.\n")
    operations.append({"step": "create ordinary.txt", "path": "ordinary.txt"})

    hard = root / "hard.txt"
    os.link(ordinary, hard)
    sync_directory(root)
    settle()
    operations.append({"step": "hard link hard.txt -> ordinary.txt", "path": "hard.txt"})

    symlink = root / "link.txt"
    os.symlink("ordinary.txt", symlink)
    sync_directory(root)
    settle()
    operations.append({"step": "symlink link.txt -> ordinary.txt", "path": "link.txt"})

    # Clone family A (small): 64 KiB cloned 4 times.
    family_a_dir = root / "family-a"
    family_a_dir.mkdir()
    sync_directory(root)
    payload_a = (b"AAA0123456789xyz" * (64 * 1024 // 16))[: 64 * 1024]
    src_a = family_a_dir / "src.bin"
    write_file(src_a, payload_a)
    operations.append({"step": "family-a src.bin (64 KiB)", "path": "family-a/src.bin"})
    for i in range(1, 5):
        clone_path = family_a_dir / f"clone-{i}.bin"
        clone_file(src_a, clone_path)
        operations.append({"step": f"family-a clone-{i}", "path": f"family-a/clone-{i}.bin"})

    # Clone family B (medium): 1 MiB cloned twice (3 instances).
    family_b_dir = root / "family-b"
    family_b_dir.mkdir()
    sync_directory(root)
    payload_b = os.urandom(1 * 1024 * 1024)
    src_b = family_b_dir / "src.bin"
    write_file(src_b, payload_b)
    operations.append({"step": "family-b src.bin (1 MiB)", "path": "family-b/src.bin"})
    for i in range(1, 3):
        clone_path = family_b_dir / f"clone-{i}.bin"
        clone_file(src_b, clone_path)
        operations.append({"step": f"family-b clone-{i}", "path": f"family-b/clone-{i}.bin"})

    # Clone family C (partial-share): 1 MiB cloned, then rewrite 256 KiB
    # in the middle of the clone.
    family_c_dir = root / "family-c"
    family_c_dir.mkdir()
    sync_directory(root)
    payload_c = os.urandom(1 * 1024 * 1024)
    src_c = family_c_dir / "src.bin"
    write_file(src_c, payload_c)
    operations.append({"step": "family-c src.bin (1 MiB)", "path": "family-c/src.bin"})
    clone_c = family_c_dir / "clone.bin"
    clone_file(src_c, clone_c)
    operations.append({"step": "family-c clone", "path": "family-c/clone.bin"})
    rewrite_middle(clone_c, offset=384 * 1024, length=256 * 1024)
    operations.append(
        {
            "step": "family-c clone middle 256 KiB rewritten at 384 KiB",
            "path": "family-c/clone.bin",
        }
    )

    return operations


# ---- POSIX oracle ------------------------------------------------------- #

def snapshot_oracle(root: Path) -> dict:
    """Capture st_blocks per inode + du -A per path."""
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
            }
        )
        for name in filenames:
            path = Path(current_root) / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            kind = (
                "dir" if stat.S_ISDIR(st.st_mode)
                else "symlink" if stat.S_ISLNK(st.st_mode)
                else "file" if stat.S_ISREG(st.st_mode)
                else f"other({stat.S_IFMT(st.st_mode):#x})"
            )
            entry: dict[str, Any] = {
                "type": kind,
                "path": str(rel_path),
                "inode": st.st_ino,
                "nlink": st.st_nlink,
                "st_size": st.st_size,
                "st_blocks": st.st_blocks,
                "st_blocks_x_512": st.st_blocks * 512,
            }
            if kind == "symlink":
                entry["symlink_target"] = os.readlink(path)
            entries.append(entry)

    # du -A on each file: returns 512-byte blocks deduplicated across
    # clones.  -P prevents following symlinks, -A reports deduplicated
    # allocated bytes (this is the key oracle).
    du_per_path: dict[str, int] = {}
    for entry in entries:
        if entry["type"] == "dir":
            continue
        full = root / entry["path"]
        proc = run(["du", "-A", "-P", str(full)])
        if proc.returncode != 0:
            continue
        first = proc.stdout.strip().split("\n")[0]
        blocks_str = first.split()[0] if first else "0"
        try:
            blocks = int(blocks_str)
        except ValueError:
            blocks = 0
        # du -A reports in 512-byte blocks by default on macOS.
        du_per_path[entry["path"]] = blocks * 512

    # du -A on each directory (deduplicated total of the subtree).
    du_per_dir: dict[str, int] = {}
    for entry in entries:
        if entry["type"] != "dir":
            continue
        full = root / entry["path"] if entry["path"] != "." else root
        proc = run(["du", "-A", "-s", "-P", str(full)])
        if proc.returncode != 0:
            continue
        first = proc.stdout.strip().split("\n")[0]
        blocks_str = first.split()[0] if first else "0"
        try:
            blocks = int(blocks_str)
        except ValueError:
            blocks = 0
        du_per_dir[entry["path"]] = blocks * 512

    return {
        "entries": entries,
        "du_per_path": du_per_path,
        "du_per_dir": du_per_dir,
    }


# ---- image lifecycle ---------------------------------------------------- #

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


# ---- B-tree node parsing (port from EX-13) ----------------------------- #

def read_block(handle: Any, paddr: int, block_size: int) -> bytes:
    handle.seek(paddr * block_size)
    block = handle.read(block_size)
    if len(block) != block_size:
        raise ProbeError(
            "short_read",
            f"short read at block {paddr}: expected {block_size}, got {len(block)}",
        )
    return block


def parse_btree_node(block: bytes, block_size: int) -> dict:
    flags = le_u16(block, 0x20)
    nkeys = le_u32(block, 0x24)
    toc_off_rel = le_u16(block, 0x28)
    toc_len = le_u16(block, 0x2A)
    data_offset = OBJ_HEADER_SIZE + 24
    toc_offset = data_offset + toc_off_rel
    key_area_offset = toc_offset + toc_len
    value_area_end = block_size - BTREE_INFO_SIZE if flags & BTNODE_ROOT else block_size
    if flags & BTNODE_FIXED_KV_SIZE:
        raise ProbeError(
            "unsupported_btree", "fs-tree / extentref-tree node uses fixed kv size"
        )
    return {
        "flags": flags,
        "level": le_u16(block, 0x22),
        "nkeys": nkeys,
        "toc_offset": toc_offset,
        "toc_len": toc_len,
        "key_area_offset": key_area_offset,
        "value_area_end": value_area_end,
        "is_leaf": bool(flags & BTNODE_LEAF),
        "is_root": bool(flags & BTNODE_ROOT),
    }


def node_entry(block: bytes, node: dict, index: int) -> tuple[bytes, bytes]:
    entry_off = node["toc_offset"] + 8 * index
    k_off = le_u16(block, entry_off)
    k_len = le_u16(block, entry_off + 2)
    v_off = le_u16(block, entry_off + 4)
    v_len = le_u16(block, entry_off + 6)
    key_start = node["key_area_offset"] + k_off
    value_start = node["value_area_end"] - v_off
    return (
        block[key_start : key_start + k_len],
        block[value_start : value_start + v_len],
    )


def walk_virtual_btree(
    handle: Any,
    root_paddr: int,
    block_size: int,
    omap_lookup: dict[int, int],
) -> list[tuple[bytes, bytes]]:
    """Walk a virtual B-tree, resolving internal-node child OIDs via the
    provided OMAP lookup. Returns a list of (key, value) leaf-record pairs."""
    leaves: list[tuple[bytes, bytes]] = []

    def visit(paddr: int) -> None:
        block = read_block(handle, paddr, block_size)
        node = parse_btree_node(block, block_size)
        if node["is_leaf"]:
            for index in range(node["nkeys"]):
                k, v = node_entry(block, node, index)
                leaves.append((k, v))
            return
        for index in range(node["nkeys"]):
            _, value = node_entry(block, node, index)
            if len(value) < 8:
                raise ProbeError(
                    "malformed_btree",
                    f"internal value at {paddr}/{index} shorter than child oid",
                )
            child_oid = le_u64(value, 0)
            if child_oid not in omap_lookup:
                raise ProbeError(
                    "omap_lookup_miss",
                    f"internal node at {paddr}/{index} references oid {child_oid} "
                    f"not present in supplied OMAP map; expand sample size in Rust",
                )
            visit(omap_lookup[child_oid])

    visit(root_paddr)
    return leaves


# ---- record decoders ---------------------------------------------------- #

def parse_key_header(key: bytes) -> tuple[int, int]:
    """Returns (obj_id, raw_type) from j_key_t."""
    word = le_u64(key, 0)
    return word & OBJ_ID_MASK, word >> OBJ_TYPE_SHIFT


def decode_file_extent_record(key: bytes, value: bytes) -> dict:
    """j_file_extent_key_t (16 bytes total): hdr (8) + logical_addr (8).
    j_file_extent_val_t (24 bytes): len_and_flags (8) + phys_block_num (8) +
    crypto_id (8). High 4 bits of len_and_flags are flags; low 60 bits are
    length in *bytes*.

    The key's `hdr.obj_id_and_type` low 60 bits is the **dstream_id**
    (a.k.a. private_id), not the inode obj_id. Clones share a single
    dstream_id; their inodes have `private_id` pointing at that dstream.
    """
    if len(key) < 16:
        raise ProbeError("malformed_file_extent", f"file_extent key too short: {len(key)}")
    if len(value) < 16:  # crypto_id is optional in v1 fixtures
        raise ProbeError("malformed_file_extent", f"file_extent value too short: {len(value)}")
    dstream_id, _ = parse_key_header(key)
    logical_addr = le_u64(key, 8)
    len_and_flags = le_u64(value, 0)
    length_bytes = len_and_flags & OBJ_ID_MASK
    flags = (len_and_flags >> OBJ_TYPE_SHIFT) & 0xF
    phys_block_num = le_u64(value, 8)
    crypto_id = le_u64(value, 16) if len(value) >= 24 else 0
    return {
        "dstream_id": dstream_id,
        "logical_addr": logical_addr,
        "length_bytes": length_bytes,
        "flags": flags,
        "phys_block_num": phys_block_num,
        "crypto_id": crypto_id,
    }


def decode_phys_ext_record(key: bytes, value: bytes) -> dict:
    """j_phys_ext_key_t (8 bytes total): hdr where low 60 bits are paddr_first.
    j_phys_ext_val_t (20 bytes): len_and_kind (8) + owning_obj_id (8) +
    refcnt (4). High 4 bits of len_and_kind are kind; low 60 bits are length
    in *blocks*.
    """
    if len(key) < 8:
        raise ProbeError("malformed_phys_ext", f"phys_ext key too short: {len(key)}")
    if len(value) < 20:
        raise ProbeError("malformed_phys_ext", f"phys_ext value too short: {len(value)}")
    paddr_first, raw_type = parse_key_header(key)
    if raw_type != RAW_TYPE_EXTENT_REFERENCE:
        raise ProbeError(
            "wrong_raw_type",
            f"phys_ext key has raw_type {raw_type}, expected {RAW_TYPE_EXTENT_REFERENCE}",
        )
    len_and_kind = le_u64(value, 0)
    length_blocks = len_and_kind & OBJ_ID_MASK
    kind = (len_and_kind >> OBJ_TYPE_SHIFT) & 0xF
    owning_obj_id = le_u64(value, 8)
    refcnt = le_i32(value, 16)
    return {
        "paddr_first": paddr_first,
        "length_blocks": length_blocks,
        "kind": kind,
        "owning_obj_id": owning_obj_id,
        "refcnt": refcnt,
    }


def decode_dir_rec_key(key: bytes) -> tuple[int, str]:
    """Parse a j_drec_hashed_key_t to recover the parent obj_id and the name.
    Used to build a parent-id -> child-id map for path attribution.
    """
    parent_obj_id, _ = parse_key_header(key)
    if len(key) >= 12:
        name_len = le_u32(key, 8) & 0x000003FF
        if name_len and 12 + name_len <= len(key):
            name_bytes = key[12 : 12 + name_len]
            return parent_obj_id, name_bytes.decode("utf-8", errors="replace").rstrip("\x00")
    name_len = le_u16(key, 8)
    name_bytes = key[10 : 10 + name_len]
    return parent_obj_id, name_bytes.decode("utf-8", errors="replace").rstrip("\x00")


def decode_dir_rec_value_file_id(value: bytes) -> int:
    """j_drec_val_t: file_id (8) + date_added (8) + flags (2) [+ xfields].
    Returns the file_id (the child inode's obj_id)."""
    return le_u64(value, 0)


# ---- OMAP map extraction ----------------------------------------------- #

def collect_omap_mappings(volume_omap_summary: dict | None) -> dict[int, int]:
    """Build oid -> paddr from the Rust scanner's volume_omap.sample_mappings.

    The Rust scanner samples up to 8 mappings; for a small fixture every
    mapping is likely surfaced, but if the volume OMAP has more than 8
    entries we may miss some.  The walk error message will say which one.
    """
    if not volume_omap_summary:
        return {}
    out: dict[int, int] = {}
    for sample in volume_omap_summary.get("sample_mappings", []):
        out[int(sample["oid"])] = int(sample["paddr"])
    return out


# ---- per-inode dedup math ----------------------------------------------- #

def split_file_extent_against_phys_exts(
    fe: dict,
    phys_exts: list[dict],
    block_size: int,
) -> list[dict]:
    """Walk a file_extent's physical byte range against the phys_ext records
    it overlaps. Each phys_ext record carries the refcnt for its sub-range.

    Returns a list of {paddr, length_bytes, refcnt, from_phys_ext_record}
    sub-extents that together cover the file_extent. APFS only stores
    phys_ext records for extents whose refcnt is non-trivial; any byte
    range not covered by a phys_ext record has implicit refcnt = 1.
    """
    fe_paddr_start = fe["phys_block_num"]
    fe_length_blocks = fe["length_bytes"] // block_size
    if fe["length_bytes"] % block_size != 0:
        # Sub-block residue; round up since extents are block-aligned in
        # practice. APFS file extents on a 4 KiB-block fixture are always
        # block-multiples.
        fe_length_blocks += 1
    fe_paddr_end = fe_paddr_start + fe_length_blocks  # exclusive

    sub_extents: list[dict] = []
    cursor = fe_paddr_start
    # Iterate phys_exts that overlap [fe_paddr_start, fe_paddr_end).
    relevant = sorted(
        (p for p in phys_exts
         if p["paddr_first"] < fe_paddr_end
         and p["paddr_first"] + p["length_blocks"] > fe_paddr_start),
        key=lambda p: p["paddr_first"],
    )
    for p in relevant:
        p_start = p["paddr_first"]
        p_end = p_start + p["length_blocks"]
        if p_start > cursor:
            # Gap between cursor and this phys_ext: implicit refcnt=1.
            sub_extents.append({
                "paddr": cursor,
                "length_bytes": (p_start - cursor) * block_size,
                "refcnt": 1,
                "from_phys_ext_record": False,
            })
            cursor = p_start
        overlap_start = max(cursor, p_start)
        overlap_end = min(fe_paddr_end, p_end)
        if overlap_end > overlap_start:
            sub_extents.append({
                "paddr": overlap_start,
                "length_bytes": (overlap_end - overlap_start) * block_size,
                "refcnt": max(1, p["refcnt"]),
                "from_phys_ext_record": True,
            })
            cursor = overlap_end
    if cursor < fe_paddr_end:
        sub_extents.append({
            "paddr": cursor,
            "length_bytes": (fe_paddr_end - cursor) * block_size,
            "refcnt": 1,
            "from_phys_ext_record": False,
        })
    return sub_extents


def compute_per_dstream_dedup(
    file_extents: list[dict],
    phys_exts: list[dict],
    block_size: int,
) -> dict[int, dict]:
    """Aggregate per-dstream dedup totals.

    Returns dstream_id -> {
        'raw_alloc_bytes':   Σ file_extent.length_bytes (un-deduped),
        'dedup_alloc_bytes': Σ (sub_extent.length_bytes / sub_extent.refcnt)
                             where each sub_extent comes from the file_extent
                             walked against the phys_ext tree,
        'extents': [{...sub_extents...}, ...],
    }
    """
    per_dstream: dict[int, dict] = {}
    for fe in file_extents:
        if fe["length_bytes"] == 0:
            continue
        sub_extents = split_file_extent_against_phys_exts(fe, phys_exts, block_size)
        dedup_share = 0
        for se in sub_extents:
            dedup_share += se["length_bytes"] // se["refcnt"]
        bucket = per_dstream.setdefault(
            fe["dstream_id"],
            {
                "raw_alloc_bytes": 0,
                "dedup_alloc_bytes": 0,
                "extents": [],
            },
        )
        bucket["raw_alloc_bytes"] += fe["length_bytes"]
        bucket["dedup_alloc_bytes"] += dedup_share
        bucket["extents"].extend(sub_extents)
    return per_dstream


def compute_per_inode(
    per_dstream: dict[int, dict],
    inode_private_id: dict[int, int],
    dstream_refcnt: dict[int, int],
) -> dict[int, dict]:
    """Attribute each dstream's deduped bytes across the inodes that
    reference it. dstream_refcnt[d] is the count of inodes sharing
    dstream d; each gets dstream_dedup / refcnt as its share."""
    per_inode: dict[int, dict] = {}
    for inode_id, private_id in inode_private_id.items():
        dstream = per_dstream.get(private_id)
        refcnt = max(1, dstream_refcnt.get(private_id, 1))
        if dstream is None:
            per_inode[inode_id] = {
                "private_id": private_id,
                "dstream_refcnt": refcnt,
                "dstream_dedup_total": 0,
                "dstream_raw_total": 0,
                "this_inode_dedup_share": 0,
                "extents": [],
            }
            continue
        per_inode[inode_id] = {
            "private_id": private_id,
            "dstream_refcnt": refcnt,
            "dstream_dedup_total": dstream["dedup_alloc_bytes"],
            "dstream_raw_total": dstream["raw_alloc_bytes"],
            "this_inode_dedup_share": dstream["dedup_alloc_bytes"] // refcnt,
            "extents": dstream["extents"],
        }
    return per_inode


# ---- path -> inode attribution via drec records ------------------------ #

def build_path_to_inode(records: list[dict]) -> dict[str, int]:
    """Walk drec records (raw_type 9) to reconstruct path -> inode obj_id."""
    children: dict[int, list[tuple[str, int]]] = {}
    for r in records:
        if r["family"] != "dir_rec":
            continue
        key = r["key"]
        if key.get("kind") != "named":
            continue
        name = key["name"]
        # Rust's emitted record has the parent obj_id as `object_id`.
        parent_id = r["object_id"]
        child_file_id = r["value"]["file_id"]
        children.setdefault(parent_id, []).append((name, child_file_id))

    APFS_ROOT_DIR_OID = 2
    path_to_inode: dict[str, int] = {}

    def walk(parent_id: int, parent_path: str) -> None:
        for name, child_id in sorted(children.get(parent_id, []), key=lambda p: p[0]):
            if name == ".fseventsd":
                continue
            path = name if parent_path == "" else f"{parent_path}/{name}"
            path_to_inode[path] = child_id
            walk(child_id, path)

    walk(APFS_ROOT_DIR_OID, "")
    return path_to_inode


# ---- driver ------------------------------------------------------------- #

def environment() -> dict:
    sw_vers = run(["sw_vers"])
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
        "du": shutil.which("du"),
        "sw_vers": sw_vers.stdout,
    }


def main() -> int:
    write_json("environment.json", environment())
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex27-", dir="/tmp"))
    image_path = base / "EX27CI.dmg"
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
                "hdiutil", "create", "-size", "1g", "-fs", "APFS",
                "-volname", "EX27CI", "-nospotlight", str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_fixture(mountpoint)
        oracle = snapshot_oracle(mountpoint)
        write_json("ex27-fixture-operations.json", {"operations": operations})
        write_json("ex27-mounted-posix-oracle.json", oracle)
        detach_device(mounted_detach)
        mounted_detach = None
        time.sleep(0.4)

        _, nomount_detach, raw_container = attach_nomount(image_path)
        rust_scan = run_rust_scan(raw_container)
        write_json("ex27-rust-scan.json", rust_scan)
        sel = rust_scan.get("selected_checkpoint")
        if not sel:
            raise ProbeError(
                "oracle_inconclusive_overall",
                "Rust did not publish selected_checkpoint",
            )
        block_size = int(sel["container"]["block_size"])
        volume = sel["volumes"][0]
        root_tree_lookup = volume.get("root_tree_lookup")
        if not root_tree_lookup:
            raise ProbeError(
                "oracle_inconclusive_overall",
                "Rust scanner did not surface root_tree_lookup",
            )
        # The extent-reference tree's storage type is in
        # extentref_tree_type_raw. High byte 0x40 = OBJECT_TYPE_PHYSICAL
        # (the OID is a paddr); high byte 0x00 = OBJECT_TYPE_VIRTUAL
        # (needs OMAP lookup). hdiutil-created APFS images on macOS
        # use OBJECT_TYPE_PHYSICAL for this tree; live volumes may
        # vary. The Rust scanner's extentref_tree_lookup is populated
        # only for the virtual case.
        summary_v = volume["summary"]
        extentref_type_raw = int(summary_v["extentref_tree_type_raw"])
        extentref_storage_class = (extentref_type_raw >> 24) & 0xFF
        extentref_oid = int(summary_v["extentref_tree_oid"])
        if extentref_storage_class == 0x40:  # OBJECT_TYPE_PHYSICAL
            extentref_root_paddr = extentref_oid
        elif extentref_storage_class == 0x00:  # OBJECT_TYPE_VIRTUAL
            ert = volume.get("extentref_tree_lookup")
            if not ert:
                raise ProbeError(
                    "probe_blocked_no_extentref",
                    f"virtual extentref tree at oid {extentref_oid} did not resolve "
                    "via volume OMAP; Rust scanner surfaced no extentref_tree_lookup",
                )
            extentref_root_paddr = int(ert["paddr"])
        else:
            raise ProbeError(
                "probe_blocked_no_extentref",
                f"unsupported extentref tree storage class {extentref_storage_class:#x}",
            )
        omap_lookup = collect_omap_mappings(volume.get("volume_omap"))

        # Use the Rust scanner's drec records for path attribution
        # (already decoded; no need to re-parse in Python).
        fs_record_dump = volume.get("fs_record_dump") or {}
        rust_records = fs_record_dump.get("records") or []
        path_to_inode = build_path_to_inode(rust_records)

        # Walk both trees directly.
        with open(raw_container, "rb", buffering=0) as handle:
            try:
                fs_leaves = walk_virtual_btree(
                    handle, int(root_tree_lookup["paddr"]), block_size, omap_lookup
                )
            except ProbeError as err:
                # Fall back to single-leaf-root walking (no OMAP needed
                # when the tree fits in one leaf).
                if err.verdict == "omap_lookup_miss":
                    raise ProbeError(
                        "probe_blocked_omap_overflow",
                        f"fs-tree internal node references oid outside the 8-mapping OMAP "
                        f"sample; the Rust volume_omap.sample_mappings is too narrow for this "
                        f"fixture. Detail: {err.detail}",
                    ) from err
                raise
            try:
                er_leaves = walk_virtual_btree(
                    handle, extentref_root_paddr, block_size, omap_lookup
                )
            except ProbeError as err:
                if err.verdict == "omap_lookup_miss":
                    raise ProbeError(
                        "probe_blocked_omap_overflow",
                        f"extentref-tree internal node references oid outside the OMAP "
                        f"sample. Detail: {err.detail}",
                    ) from err
                raise

        file_extents: list[dict] = []
        for key, value in fs_leaves:
            if len(key) < 8:
                continue
            _, raw_type = parse_key_header(key)
            if raw_type == RAW_TYPE_FILE_EXTENT:
                file_extents.append(decode_file_extent_record(key, value))
        phys_exts: list[dict] = []
        for key, value in er_leaves:
            phys_exts.append(decode_phys_ext_record(key, value))

        write_json(
            "ex27-file-extents.json",
            {"count": len(file_extents), "extents": file_extents},
        )
        write_json(
            "ex27-phys-exts.json",
            {"count": len(phys_exts), "records": phys_exts},
        )

        # Build inode_obj_id → private_id and dstream_refcnt maps from the
        # Rust scanner output (already decoded; no need to re-parse).
        inode_private_id: dict[int, int] = {}
        dstream_refcnt: dict[int, int] = {}
        S_IFREG = 0o100_000
        S_IFMT = 0o170_000
        for r in rust_records:
            if r["family"] == "inode":
                v = r["value"]
                # Only regular files have an associated dstream.
                if (int(v["mode"]) & S_IFMT) == S_IFREG:
                    inode_private_id[int(r["object_id"])] = int(v["private_id"])
            elif r["family"] == "dstream_id":
                refcnt = r["value"].get("refcnt")
                if refcnt is not None:
                    dstream_refcnt[int(r["object_id"])] = int(refcnt)

        per_dstream = compute_per_dstream_dedup(file_extents, phys_exts, block_size)
        per_inode = compute_per_inode(per_dstream, inode_private_id, dstream_refcnt)

        # Ground-truth volume dedup: sum every phys_ext record's length once
        # (each phys_ext is one shared physical extent). This is the
        # authoritative oracle — `du -A` on macOS reports apparent (logical)
        # size and doesn't dedup, so it's not usable as a per-path oracle for
        # clone-aware allocation. The per-path comparison below records the
        # gap as expected (computed_dedup < oracle_du_minus_A for clones).
        phys_ext_total_bytes = sum(p["length_blocks"] for p in phys_exts) * block_size

        # Compare against du -A per path. For clones, `du -A` is the apparent
        # size (un-deduped), and our computed dedup is necessarily smaller.
        # The validation is per-row: computed should match
        # (dstream.refcnt > 1) ? (dstream_dedup / refcnt) : (dstream_dedup).
        rows: list[dict] = []
        for path, oracle_bytes in sorted(oracle["du_per_path"].items()):
            inode_id = path_to_inode.get(path)
            if inode_id is None:
                rows.append(
                    {
                        "path": path,
                        "oracle_du_minus_A": oracle_bytes,
                        "inode_obj_id": None,
                        "computed_dedup_share": None,
                        "note": "no fs-tree drec mapping; cannot attribute extents",
                    }
                )
                continue
            bucket = per_inode.get(inode_id) or {}
            row = {
                "path": path,
                "oracle_du_minus_A": oracle_bytes,
                "inode_obj_id": inode_id,
                "private_id": bucket.get("private_id"),
                "dstream_refcnt": bucket.get("dstream_refcnt"),
                "dstream_raw_total": bucket.get("dstream_raw_total"),
                "dstream_dedup_total": bucket.get("dstream_dedup_total"),
                "computed_dedup_share": bucket.get("this_inode_dedup_share", 0),
            }
            rows.append(row)

        # Per-dstream sanity row: dstream_dedup_total summed once across
        # all unique dstreams should equal Σ phys_ext.length_bytes.
        unique_dstreams_dedup_sum = sum(d["dedup_alloc_bytes"] for d in per_dstream.values())
        unique_dstreams_raw_sum = sum(d["raw_alloc_bytes"] for d in per_dstream.values())
        # Σ per-inode share should also equal unique_dstreams_dedup_sum.
        per_inode_share_sum = sum(b["this_inode_dedup_share"] for b in per_inode.values())

        precedence = {
            "rows": rows,
            "block_size": block_size,
            "file_extent_count": len(file_extents),
            "phys_ext_count": len(phys_exts),
            "per_dstream_count": len(per_dstream),
            "per_inode_count": len(per_inode),
            "phys_ext_total_bytes": phys_ext_total_bytes,
            "unique_dstreams_raw_sum": unique_dstreams_raw_sum,
            "unique_dstreams_dedup_sum": unique_dstreams_dedup_sum,
            "per_inode_share_sum": per_inode_share_sum,
            "oracle_dir_sum_du_minus_A": oracle["du_per_dir"].get(".", 0),
        }
        write_json("ex27-precedence-table.json", precedence)

        # Validation: the authoritative invariant is
        # Σ_dstream dedup_total == Σ_phys_ext length_bytes, because:
        #   - Each phys_ext at refcnt R is referenced by R file_extents.
        #   - Each file_extent owner contributes length_bytes / R to its
        #     dstream's dedup total.
        #   - Summed over all file_extents, this collapses to length_bytes
        #     once per phys_ext.
        # Per-inode shares should also sum to the same number (each
        # dstream's dedup divided across its inodes, summed back).
        verdict = "pending"
        detail = ""
        # Integer-division rounding: each clone-shared dstream loses
        # up to (refcnt - 1) bytes when its dedup total isn't evenly
        # divisible. Worst-case residue is bounded by Σ over dstreams
        # of (refcnt - 1) ≤ count_of_shared_dstreams * max_refcnt.
        rounding_residue = phys_ext_total_bytes - per_inode_share_sum
        sharing_dstreams = sum(
            1 for d, refcnt in dstream_refcnt.items()
            if refcnt > 1 and d in per_dstream
        )
        if unique_dstreams_dedup_sum == phys_ext_total_bytes and rounding_residue == 0:
            verdict = "validated_clone_dedup"
            detail = (
                f"Σ dstream dedup ({unique_dstreams_dedup_sum}) = "
                f"Σ phys_ext bytes ({phys_ext_total_bytes}) = "
                f"Σ per-inode share ({per_inode_share_sum}) exactly. "
                f"{len(per_dstream)} unique dstreams across {len(per_inode)} inodes."
            )
        elif (
            unique_dstreams_dedup_sum == phys_ext_total_bytes
            and rounding_residue >= 0
            and rounding_residue < sharing_dstreams * 64  # generous bound
        ):
            verdict = "validated_clone_dedup"
            detail = (
                f"Σ dstream dedup ({unique_dstreams_dedup_sum}) = "
                f"Σ phys_ext bytes ({phys_ext_total_bytes}); "
                f"Σ per-inode share ({per_inode_share_sum}) within "
                f"{rounding_residue} bytes of the phys_ext total — bounded "
                f"by integer-division rounding across {sharing_dstreams} "
                f"clone-shared dstreams (Σ refcnt-1 = {rounding_residue})."
            )
        elif unique_dstreams_dedup_sum == phys_ext_total_bytes:
            verdict = "validated_clone_dedup_with_divergence"
            detail = (
                f"dstream dedup totals match phys_ext bytes ({phys_ext_total_bytes}), "
                f"but per-inode share sum ({per_inode_share_sum}) diverges by "
                f"{rounding_residue} — exceeds the expected rounding bound."
            )
        else:
            verdict = "oracle_inconclusive_clone_dedup"
            detail = (
                f"Σ dstream dedup ({unique_dstreams_dedup_sum}) ≠ "
                f"Σ phys_ext bytes ({phys_ext_total_bytes}); "
                f"phys_ext walk or file_extent decode is wrong. "
                f"Σ undedup raw = {unique_dstreams_raw_sum}."
            )
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["row_count"] = len(rows)
        summary["phys_ext_total_bytes"] = phys_ext_total_bytes
        summary["unique_dstreams_dedup_sum"] = unique_dstreams_dedup_sum
        summary["per_inode_share_sum"] = per_inode_share_sum
        summary["unique_dstreams_raw_sum"] = unique_dstreams_raw_sum
        write_json("summary.json", summary)
        return 0 if verdict == "validated_clone_dedup" else 1
    except ProbeError as err:
        summary["verdict"] = err.verdict
        summary["verdict_detail"] = err.detail
        write_json("summary.json", summary)
        return 1
    except Exception as err:
        import traceback
        summary["verdict"] = "probe_exception"
        summary["verdict_detail"] = f"{type(err).__name__}: {err}"
        summary["traceback"] = traceback.format_exc()
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
