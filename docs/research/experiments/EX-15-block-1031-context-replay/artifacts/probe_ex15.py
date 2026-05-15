#!/usr/bin/env python3
"""Run EX-15: block-1031 context replay.

This probe rebuilds the EX-14 fixture deterministically, dumps the offending
block(s), and replays SR-005 / SR-007 header validation in Python against every
NXSB candidate the descriptor area exposes. It also asks `fsck_apfs -n` and the
local `go-apfs identitydump` helper whether the image is internally consistent
from their point of view.

The probe writes one artifact per question (fixture, oracle, candidate replay,
failing-block dump, go-apfs, fsck, Rust) plus a `summary.json` that names which
of hypotheses (a)-(d) holds.
"""

from __future__ import annotations

import datetime as _dt
import fcntl
import hashlib
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
NX_MAGIC = 0x4253584E
OBJ_HEADER_SIZE = 32

OBJECT_TYPE_MASK = 0x0000FFFF
OBJECT_TYPE_FLAGS_MASK = 0xFFFF0000
OBJ_VIRTUAL = 0x00000000
OBJ_EPHEMERAL = 0x80000000
OBJ_PHYSICAL = 0x40000000
OBJ_NOHEADER = 0x20000000
OBJ_ENCRYPTED = 0x10000000

OBJECT_TYPE_NX_SUPERBLOCK = 0x0001
OBJECT_TYPE_BTREE = 0x0002
OBJECT_TYPE_BTREE_NODE = 0x0003
OBJECT_TYPE_OMAP = 0x000B
OBJECT_TYPE_CHECKPOINT_MAP = 0x000C
OBJECT_TYPE_FS = 0x000D
OBJECT_TYPE_FSTREE = 0x000E

CHECKPOINT_MAP_LAST = 0x1
CHECKPOINT_MAPPING_SIZE = 40
OMAP_KEY_SIZE = 16
OMAP_VAL_SIZE = 16
OMAP_INTERNAL_VAL_SIZE = 8

OMAP_VAL_DELETED = 0x1
OMAP_VAL_SAVED = 0x2
OMAP_VAL_ENCRYPTED = 0x4
OMAP_VAL_NOHEADER = 0x8
OMAP_VAL_CRYPTO_GENERATION = 0x10
OMAP_VAL_KNOWN_BITS = (
    OMAP_VAL_DELETED
    | OMAP_VAL_SAVED
    | OMAP_VAL_ENCRYPTED
    | OMAP_VAL_NOHEADER
    | OMAP_VAL_CRYPTO_GENERATION
)
OMAP_MANUALLY_MANAGED = 0x1

BTNODE_ROOT = 0x0001
BTNODE_LEAF = 0x0002
BTNODE_FIXED_KV_SIZE = 0x0004
BTREE_INFO_SIZE = 40

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
EXPERIMENT_DIR = ARTIFACT_DIR.parent
REPO_ROOT = ARTIFACT_DIR.parents[4]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"
IDENTITYDUMP_DIR = (
    REPO_ROOT
    / "docs"
    / "research"
    / "experiments"
    / "EX-06-identity-tracking"
    / "artifacts"
    / "identitydump"
)


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


# ---- low-level helpers --------------------------------------------------- #

def le_u16(data: bytes, offset: int) -> int:
    return int.from_bytes(data[offset : offset + 2], "little")


def le_u32(data: bytes, offset: int) -> int:
    return int.from_bytes(data[offset : offset + 4], "little")


def le_u64(data: bytes, offset: int) -> int:
    return int.from_bytes(data[offset : offset + 8], "little")


def apfs_fletcher64(block: bytes) -> int:
    """Match Rust's `apfs_fletcher64`: skip first 8 cksum bytes, treat the rest
    as little-endian u32 words, fletcher-64 over u32-modulo-(2^32 - 1)."""
    mod = 0xFFFFFFFF
    lower = 0
    upper = 0
    body = block[8:]
    if len(body) % 4 != 0:
        body = body + b"\x00" * (4 - (len(body) % 4))
    for offset in range(0, len(body), 4):
        word = int.from_bytes(body[offset : offset + 4], "little")
        lower += word
        upper += lower
    lower %= mod
    upper %= mod
    cksum_lower = mod - ((lower + upper) % mod)
    cksum_upper = mod - ((lower + cksum_lower) % mod)
    return (cksum_upper << 32) | cksum_lower


def checksum_matches(block: bytes) -> bool:
    stored = le_u64(block, 0)
    return stored == apfs_fletcher64(block)


def parse_obj_phys(block: bytes, paddr: int) -> dict:
    object_type_raw = le_u32(block, 0x18)
    return {
        "paddr": paddr,
        "checksum_stored_hex": f"{le_u64(block, 0):#018x}",
        "checksum_computed_hex": f"{apfs_fletcher64(block):#018x}",
        "checksum_matches": checksum_matches(block),
        "oid": le_u64(block, 0x08),
        "xid": le_u64(block, 0x10),
        "object_type_raw_hex": f"{object_type_raw:#010x}",
        "object_type": object_type_raw & OBJECT_TYPE_MASK,
        "object_type_flags_hex": f"{object_type_raw & OBJECT_TYPE_FLAGS_MASK:#010x}",
        "object_subtype_hex": f"{le_u32(block, 0x1c):#010x}",
    }


# ---- fixture ------------------------------------------------------------- #

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


def create_file(path: Path, payload: bytes | str) -> None:
    if isinstance(payload, bytes):
        path.write_bytes(payload)
    else:
        path.write_text(payload)
    full_sync(path)
    sync_directory(path.parent)
    settle()


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


def build_variant_corpus(root: Path) -> list[dict]:
    """Mirror EX-14's build_variant_corpus so block-1031 is reproduced under the
    same operation sequence as the retained EX-14 fixture."""
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

    return operations


def entry_type(path: Path, st: os.stat_result) -> str:
    mode = st.st_mode
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "file"
    return f"other({stat.S_IFMT(mode):#x})"


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
        entries.append(
            {
                "type": "dir",
                "path": "." if str(rel_root) == "." else str(rel_root),
                "inode": st.st_ino,
                "nlink": st.st_nlink,
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
    files = [e for e in entries if e["type"] == "file"]
    symlinks = [e for e in entries if e["type"] == "symlink"]
    dirs = [e for e in entries if e["type"] == "dir"]
    unique = {e["inode"]: e["logical_size"] for e in files}
    return {
        "entry_count": len(entries),
        "dir_count": len(dirs),
        "file_count": len(files),
        "symlink_count": len(symlinks),
        "hard_link_paths": sorted(e["path"] for e in files if e["nlink"] > 1),
        "sparse_candidates": sorted(
            e["path"]
            for e in files
            if e.get("allocated_bytes", 0) < e.get("logical_size", 0)
        ),
        "unique_inode_logical_total": sum(unique.values()),
    }


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
    if not entities:
        raise ProbeError("attach_failed", "hdiutil attach -nomount returned no entities")
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


# ---- candidate replay ---------------------------------------------------- #

def read_block(handle: Any, paddr: int, block_size: int) -> bytes:
    handle.seek(paddr * block_size)
    block = handle.read(block_size)
    if len(block) != block_size:
        raise ProbeError(
            "short_read",
            f"short read at block {paddr}: expected {block_size}, got {len(block)}",
        )
    return block


def parse_nxsb_block0(block: bytes) -> dict:
    return {
        "header": parse_obj_phys(block, 0),
        "nx_magic_hex": f"{le_u32(block, 0x20):#010x}",
        "block_size": le_u32(block, 0x24),
        "block_count": le_u64(block, 0x28),
        "features_hex": f"{le_u64(block, 0x30):#x}",
        "incompat_features_hex": f"{le_u64(block, 0x40):#x}",
        "next_xid": le_u64(block, 0x60),
        "xp_desc_blocks": le_u32(block, 0x68),
        "xp_data_blocks": le_u32(block, 0x6c),
        "xp_desc_base_raw_hex": f"{le_u64(block, 0x70):#x}",
        "xp_data_base_raw_hex": f"{le_u64(block, 0x78):#x}",
        "xp_desc_next": le_u32(block, 0x80),
        "xp_data_next": le_u32(block, 0x84),
        "xp_desc_index": le_u32(block, 0x88),
        "xp_desc_len": le_u32(block, 0x8c),
        "xp_data_index": le_u32(block, 0x90),
        "xp_data_len": le_u32(block, 0x94),
        "spaceman_oid": le_u64(block, 0x98),
        "omap_oid": le_u64(block, 0xa0),
        "reaper_oid": le_u64(block, 0xa8),
        "max_file_systems": le_u32(block, 0xb4),
    }


def parse_nxsb_at(block: bytes, paddr: int) -> dict:
    info = parse_nxsb_block0(block)
    info["header"]["paddr"] = paddr
    return info


def descriptor_summary(handle: Any, block_size: int, block0: dict) -> dict:
    desc_base = int(block0["xp_desc_base_raw_hex"], 16) & ~(1 << 63)
    desc_blocks = block0["xp_desc_blocks"]
    candidates: list[dict] = []
    skipped: list[dict] = []
    for index in range(desc_blocks):
        paddr = desc_base + index
        block = read_block(handle, paddr, block_size)
        nx_magic_here = le_u32(block, 0x20) == NX_MAGIC
        if not nx_magic_here:
            continue
        header = parse_obj_phys(block, paddr)
        if header["object_type"] != OBJECT_TYPE_NX_SUPERBLOCK:
            skipped.append({"paddr": paddr, "reason": "non-NX-superblock object type", "header": header})
            continue
        if not header["checksum_matches"]:
            skipped.append({"paddr": paddr, "reason": "NXSB checksum mismatch", "header": header})
            continue
        nxsb = parse_nxsb_at(block, paddr)
        candidates.append({"descriptor_index": index, "paddr": paddr, "header": header, "fields": nxsb})
    return {"descriptor_base": desc_base, "descriptor_blocks": desc_blocks, "candidates": candidates, "skipped": skipped}


# ---- header validation (replays SR-005/SR-007 in Python) ----------------- #

def validate_object_header(
    block: bytes,
    paddr: int,
    *,
    expected_type: int,
    storage: str,  # "virtual" | "ephemeral" | "physical" | "any"
    max_xid: int | None = None,
    require_oid_eq_paddr: bool = False,
) -> dict:
    header = parse_obj_phys(block, paddr)
    errors: list[str] = []
    if not header["checksum_matches"]:
        errors.append(
            f"checksum mismatch at block {paddr}: stored {header['checksum_stored_hex']} computed {header['checksum_computed_hex']}"
        )
    if header["object_type"] != expected_type:
        errors.append(
            f"block {paddr} has object type {header['object_type']:#06x}, expected {expected_type:#06x}"
        )
    flags = int(header["object_type_flags_hex"], 16)
    is_physical = (flags & OBJ_PHYSICAL) != 0
    is_ephemeral = (flags & OBJ_EPHEMERAL) != 0
    is_encrypted = (flags & OBJ_ENCRYPTED) != 0
    is_noheader = (flags & OBJ_NOHEADER) != 0
    if storage == "virtual":
        if is_physical or is_ephemeral:
            errors.append(f"block {paddr} has storage flags {flags:#010x}, expected virtual")
    elif storage == "physical":
        if not is_physical:
            errors.append(f"block {paddr} has storage flags {flags:#010x}, expected physical")
    elif storage == "ephemeral":
        if not is_ephemeral:
            errors.append(f"block {paddr} has storage flags {flags:#010x}, expected ephemeral")
    # "any" -> no check
    if is_encrypted:
        errors.append(f"block {paddr} has OBJ_ENCRYPTED set; encrypted objects unsupported")
    if is_noheader:
        errors.append(f"block {paddr} has OBJ_NOHEADER set; zero-header objects unsupported")
    if require_oid_eq_paddr and header["oid"] != paddr:
        errors.append(
            f"physical object at block {paddr} has o_oid={header['oid']} (expected o_oid==paddr)"
        )
    if max_xid is not None and header["xid"] > max_xid:
        errors.append(
            f"object at block {paddr} has o_xid={header['xid']} newer than scan state {max_xid}"
        )
    return {"header": header, "errors": errors, "ok": not errors}


# ---- container OMAP and B-tree walk replay ------------------------------- #

def parse_omap_phys(block: bytes, paddr: int) -> dict:
    return {
        "paddr": paddr,
        "flags": le_u32(block, 0x20),
        "snapshot_count": le_u32(block, 0x24),
        "tree_type_raw_hex": f"{le_u32(block, 0x28):#010x}",
        "snapshot_tree_type_raw_hex": f"{le_u32(block, 0x2c):#010x}",
        "tree_oid": le_u64(block, 0x30),
        "snapshot_tree_oid": le_u64(block, 0x38),
        "most_recent_snap": le_u64(block, 0x40),
    }


def parse_btree_node(block: bytes, block_size: int) -> dict:
    flags = le_u16(block, 0x20)
    level = le_u16(block, 0x22)
    nkeys = le_u32(block, 0x24)
    is_root = (flags & BTNODE_ROOT) != 0
    is_leaf = (flags & BTNODE_LEAF) != 0
    fixed = (flags & BTNODE_FIXED_KV_SIZE) != 0
    toc_off_rel = le_u16(block, 0x28)
    toc_len = le_u16(block, 0x2A)
    data_offset = OBJ_HEADER_SIZE + 24  # past btree_node_phys preamble
    toc_offset = data_offset + toc_off_rel
    key_area_offset = toc_offset + toc_len
    value_area_end = block_size - BTREE_INFO_SIZE if is_root else block_size
    return {
        "flags_hex": f"{flags:#06x}",
        "is_root": is_root,
        "is_leaf": is_leaf,
        "fixed_kv_size": fixed,
        "level": level,
        "nkeys": nkeys,
        "toc_offset": toc_offset,
        "toc_len": toc_len,
        "key_area_offset": key_area_offset,
        "value_area_end": value_area_end,
    }


def fixed_entry(block: bytes, node: dict, index: int, key_size: int, val_size: int) -> tuple[bytes, bytes]:
    entry_off = node["toc_offset"] + 4 * index
    k_off = le_u16(block, entry_off)
    v_off = le_u16(block, entry_off + 2)
    key_start = node["key_area_offset"] + k_off
    value_start = node["value_area_end"] - v_off
    return (
        block[key_start : key_start + key_size],
        block[value_start : value_start + val_size],
    )


def walk_omap_tree(
    handle: Any,
    block_size: int,
    root_paddr: int,
    max_xid: int,
) -> dict:
    """Walk the container OMAP as physical-object B-tree. Record every visited
    node's validation status; stop at the first failure but keep the trail."""
    visit: list[dict] = []
    failure: dict | None = None
    leaf_mappings: list[dict] = []

    def visit_node(paddr: int, is_root_call: bool) -> None:
        nonlocal failure
        try:
            block = read_block(handle, paddr, block_size)
        except ProbeError as exc:
            failure = {"paddr": paddr, "reason": exc.detail, "stage": "read_block"}
            return
        validation = validate_object_header(
            block,
            paddr,
            expected_type=OBJECT_TYPE_BTREE if is_root_call else OBJECT_TYPE_BTREE_NODE,
            storage="physical",
            max_xid=max_xid,
            require_oid_eq_paddr=True,
        )
        node_info = parse_btree_node(block, block_size) if validation["ok"] else None
        record = {
            "paddr": paddr,
            "is_root_call": is_root_call,
            "validation": validation,
            "node": node_info,
        }
        visit.append(record)
        if not validation["ok"]:
            failure = {"paddr": paddr, "reason": validation["errors"][0], "stage": "validate"}
            return
        node = node_info
        if not node["fixed_kv_size"]:
            failure = {"paddr": paddr, "reason": "OMAP node not fixed-kv-size", "stage": "node_shape"}
            return
        if node["is_leaf"]:
            for index in range(node["nkeys"]):
                key, value = fixed_entry(block, node, index, OMAP_KEY_SIZE, OMAP_VAL_SIZE)
                leaf_mappings.append(
                    {
                        "node_paddr": paddr,
                        "index": index,
                        "oid": le_u64(key, 0),
                        "xid": le_u64(key, 8),
                        "flags_hex": f"{le_u32(value, 0):#010x}",
                        "size": le_u32(value, 4),
                        "paddr": le_u64(value, 8),
                    }
                )
            return
        for index in range(node["nkeys"]):
            _, child_value = fixed_entry(
                block, node, index, OMAP_KEY_SIZE, OMAP_INTERNAL_VAL_SIZE
            )
            child_paddr = le_u64(child_value, 0)
            visit_node(child_paddr, False)
            if failure is not None:
                return

    visit_node(root_paddr, True)
    return {"visited_nodes": visit, "leaf_mappings": leaf_mappings, "failure": failure}


def replay_candidate(
    handle: Any,
    block_size: int,
    candidate: dict,
) -> dict:
    """Replay container-level context for one NXSB candidate."""
    paddr = candidate["paddr"]
    xid = candidate["header"]["xid"]
    nxsb = candidate["fields"]
    failure_blocks: list[int] = []

    # Step A: walk checkpoint descriptor ring like Rust does.
    walk_notes: list[dict] = []
    desc_base = int(nxsb["xp_desc_base_raw_hex"], 16) & ~(1 << 63)
    desc_blocks = nxsb["xp_desc_blocks"]
    xp_desc_index = nxsb["xp_desc_index"]
    xp_desc_len = nxsb["xp_desc_len"]
    descriptor_ring_ok = True
    for offset in range(xp_desc_len):
        position = (xp_desc_index + offset) % desc_blocks
        block_addr = desc_base + position
        block = read_block(handle, block_addr, block_size)
        is_trailing = offset == xp_desc_len - 1
        if is_trailing:
            v = validate_object_header(
                block,
                block_addr,
                expected_type=OBJECT_TYPE_NX_SUPERBLOCK,
                storage="any",
                max_xid=None,
                require_oid_eq_paddr=False,
            )
            walk_notes.append({"stage": "ring_trailing_nxsb", "paddr": block_addr, "validation": v})
            if not v["ok"]:
                descriptor_ring_ok = False
                failure_blocks.append(block_addr)
                break
            if block_addr != paddr:
                descriptor_ring_ok = False
                walk_notes.append(
                    {
                        "stage": "ring_trailing_mismatch",
                        "paddr": block_addr,
                        "selected_paddr": paddr,
                    }
                )
                break
            continue
        v = validate_object_header(
            block,
            block_addr,
            expected_type=OBJECT_TYPE_CHECKPOINT_MAP,
            storage="physical",
            max_xid=None,
            require_oid_eq_paddr=True,
        )
        walk_notes.append({"stage": "ring_checkpoint_map", "paddr": block_addr, "validation": v})
        if not v["ok"]:
            descriptor_ring_ok = False
            failure_blocks.append(block_addr)
            break
        if v["header"]["xid"] != xid:
            descriptor_ring_ok = False
            walk_notes.append({"stage": "ring_xid_mismatch", "paddr": block_addr, "xid": v["header"]["xid"]})
            break

    if not descriptor_ring_ok:
        return {
            "paddr": paddr,
            "xid": xid,
            "stage_failed": "checkpoint_ring",
            "walk_notes": walk_notes,
            "failure_blocks": failure_blocks,
        }

    # Step B: open container OMAP-phys.
    omap_paddr = nxsb["omap_oid"]
    block = read_block(handle, omap_paddr, block_size)
    omap_v = validate_object_header(
        block,
        omap_paddr,
        expected_type=OBJECT_TYPE_OMAP,
        storage="physical",
        max_xid=None,
        require_oid_eq_paddr=True,
    )
    if not omap_v["ok"]:
        failure_blocks.append(omap_paddr)
        return {
            "paddr": paddr,
            "xid": xid,
            "stage_failed": "omap_phys",
            "walk_notes": walk_notes,
            "omap_phys_validation": omap_v,
            "omap_phys_paddr": omap_paddr,
            "failure_blocks": failure_blocks,
        }
    omap_phys = parse_omap_phys(block, omap_paddr)
    tree_root_paddr = omap_phys["tree_oid"]

    # Step C: walk container OMAP B-tree.
    omap_walk = walk_omap_tree(handle, block_size, tree_root_paddr, max_xid=xid)
    if omap_walk["failure"]:
        failure_blocks.append(omap_walk["failure"]["paddr"])
        return {
            "paddr": paddr,
            "xid": xid,
            "stage_failed": "omap_walk",
            "walk_notes": walk_notes,
            "omap_phys": omap_phys,
            "omap_walk": omap_walk,
            "failure_blocks": failure_blocks,
        }

    # Step D: for each volume OID in nxsb, look up via OMAP, decode the volume
    # superblock, walk the volume OMAP, and validate the FS-tree root header.
    volume_results: list[dict] = []
    for fs_oid in collect_fs_oids(handle, block_size, paddr):
        entry = omap_lower_bound(omap_walk["leaf_mappings"], fs_oid, xid)
        if entry is None:
            volume_results.append({"fs_oid": fs_oid, "status": "missing_from_omap"})
            continue
        vol_block = read_block(handle, entry["paddr"], block_size)
        v = validate_object_header(
            vol_block,
            entry["paddr"],
            expected_type=OBJECT_TYPE_FS,
            storage="virtual",
            max_xid=xid,
            require_oid_eq_paddr=False,
        )
        if not v["ok"]:
            volume_results.append(
                {
                    "fs_oid": fs_oid,
                    "omap_entry": entry,
                    "header_validation": v,
                }
            )
            failure_blocks.append(entry["paddr"])
            continue
        volume_fields = parse_volume_superblock(vol_block, entry["paddr"])
        # Volume OMAP open
        vol_omap_paddr = volume_fields["omap_oid"]
        vol_omap_block = read_block(handle, vol_omap_paddr, block_size)
        vo_v = validate_object_header(
            vol_omap_block,
            vol_omap_paddr,
            expected_type=OBJECT_TYPE_OMAP,
            storage="physical",
            max_xid=None,
            require_oid_eq_paddr=True,
        )
        if not vo_v["ok"]:
            volume_results.append(
                {
                    "fs_oid": fs_oid,
                    "omap_entry": entry,
                    "header_validation": v,
                    "volume_fields": volume_fields,
                    "stage_failed": "volume_omap_phys",
                    "vol_omap_phys_validation": vo_v,
                    "vol_omap_paddr": vol_omap_paddr,
                }
            )
            failure_blocks.append(vol_omap_paddr)
            continue
        vol_omap_phys = parse_omap_phys(vol_omap_block, vol_omap_paddr)
        # Walk volume OMAP
        vol_omap_walk = walk_omap_tree(handle, block_size, vol_omap_phys["tree_oid"], max_xid=xid)
        if vol_omap_walk["failure"]:
            failure_blocks.append(vol_omap_walk["failure"]["paddr"])
            volume_results.append(
                {
                    "fs_oid": fs_oid,
                    "omap_entry": entry,
                    "header_validation": v,
                    "volume_fields": volume_fields,
                    "vol_omap_phys": vol_omap_phys,
                    "stage_failed": "volume_omap_walk",
                    "vol_omap_walk_failure": vol_omap_walk["failure"],
                    "vol_omap_visited_nodes": len(vol_omap_walk["visited_nodes"]),
                    "vol_omap_leaf_count": len(vol_omap_walk["leaf_mappings"]),
                }
            )
            continue
        # FS-tree root lookup
        root_lookup = omap_lower_bound(
            vol_omap_walk["leaf_mappings"], volume_fields["root_tree_oid"], xid
        )
        if root_lookup is None:
            volume_results.append(
                {
                    "fs_oid": fs_oid,
                    "omap_entry": entry,
                    "header_validation": v,
                    "volume_fields": volume_fields,
                    "vol_omap_phys": vol_omap_phys,
                    "vol_omap_visited_nodes": len(vol_omap_walk["visited_nodes"]),
                    "vol_omap_leaf_count": len(vol_omap_walk["leaf_mappings"]),
                    "stage_failed": "fs_root_missing",
                }
            )
            continue
        fs_root_block = read_block(handle, root_lookup["paddr"], block_size)
        fs_root_v = validate_object_header(
            fs_root_block,
            root_lookup["paddr"],
            expected_type=OBJECT_TYPE_BTREE,
            storage="virtual",
            max_xid=xid,
            require_oid_eq_paddr=False,
        )
        if not fs_root_v["ok"]:
            failure_blocks.append(root_lookup["paddr"])
        volume_results.append(
            {
                "fs_oid": fs_oid,
                "omap_entry": entry,
                "header_validation": v,
                "volume_fields": volume_fields,
                "vol_omap_phys": vol_omap_phys,
                "vol_omap_visited_nodes": len(vol_omap_walk["visited_nodes"]),
                "vol_omap_leaf_count": len(vol_omap_walk["leaf_mappings"]),
                "vol_omap_sample": vol_omap_walk["leaf_mappings"][:24],
                "fs_root_lookup": root_lookup,
                "fs_root_validation": fs_root_v,
                "stage_failed": None if fs_root_v["ok"] else "fs_root_validation",
            }
        )

    return {
        "paddr": paddr,
        "xid": xid,
        "stage_failed": None
        if all(vr.get("stage_failed") in (None,) or vr.get("status") == "missing_from_omap" for vr in volume_results)
        else next(
            (vr["stage_failed"] for vr in volume_results if vr.get("stage_failed") not in (None,)),
            "unknown",
        ),
        "walk_notes": walk_notes,
        "omap_phys": omap_phys,
        "omap_walk_leaf_count": len(omap_walk["leaf_mappings"]),
        "omap_walk_visited_nodes": len(omap_walk["visited_nodes"]),
        "omap_leaf_sample": omap_walk["leaf_mappings"][:24],
        "volumes": volume_results,
        "failure_blocks": failure_blocks,
    }


def parse_volume_superblock(block: bytes, paddr: int) -> dict:
    """Decode the fields apfs-fastindex uses from `apfs_superblock_t`.

    Offsets mirror `apfs_fastindex::volume::decode_volume_summary`."""
    return {
        "paddr": paddr,
        "magic_hex": f"{le_u32(block, 0x20):#010x}",
        "fs_index": le_u32(block, 0x24),
        "features_raw": le_u64(block, 0x28),
        "ro_compat_features_raw": le_u64(block, 0x30),
        "incompat_features_raw": le_u64(block, 0x38),
        "fs_flags_raw": le_u64(block, 0x108),
        "root_tree_type_raw_hex": f"{le_u32(block, 0x74):#010x}",
        "extentref_tree_type_raw_hex": f"{le_u32(block, 0x78):#010x}",
        "snap_meta_tree_type_raw_hex": f"{le_u32(block, 0x7c):#010x}",
        "omap_oid": le_u64(block, 0x80),
        "root_tree_oid": le_u64(block, 0x88),
        "extentref_tree_oid": le_u64(block, 0x90),
        "snap_meta_tree_oid": le_u64(block, 0x98),
    }


def collect_fs_oids(handle: Any, block_size: int, nxsb_paddr: int) -> list[int]:
    block = read_block(handle, nxsb_paddr, block_size)
    fs_oid_array_offset = 0xB8
    fs_oid_count = 100
    oids: list[int] = []
    for index in range(fs_oid_count):
        oid = le_u64(block, fs_oid_array_offset + 8 * index)
        if oid != 0:
            oids.append(oid)
    return oids


def omap_lower_bound(mappings: list[dict], oid: int, max_xid: int) -> dict | None:
    best = None
    for entry in mappings:
        if (entry["oid"], entry["xid"]) > (oid, max_xid):
            continue
        if best is None or (entry["oid"], entry["xid"]) > (best["oid"], best["xid"]):
            best = entry
    if best is None:
        return None
    if best["oid"] != oid:
        return None
    flags = int(best["flags_hex"], 16)
    if flags & OMAP_VAL_DELETED:
        return None
    return best


# ---- block dump --------------------------------------------------------- #

def dump_block(raw_path: str, block_size: int, paddr: int) -> dict:
    with open(raw_path, "rb", buffering=0) as handle:
        block = read_block(handle, paddr, block_size)
    header = parse_obj_phys(block, paddr)
    return {
        "paddr": paddr,
        "block_size": block_size,
        "sha256": hashlib.sha256(block).hexdigest(),
        "first_64_bytes_hex": block[:64].hex(),
        "header": header,
        "nx_magic_at_0x20_hex": f"{le_u32(block, 0x20):#010x}",
        "is_nxsb_magic": le_u32(block, 0x20) == NX_MAGIC,
    }


# ---- oracle/tool calls --------------------------------------------------- #

def run_fsck(raw_container: str) -> dict:
    proc = run(["fsck_apfs", "-n", raw_container])
    return {
        "returncode": proc.returncode,
        "stdout": proc.stdout,
        "stderr": proc.stderr,
    }


def run_identitydump(raw_container: str) -> dict:
    proc = run(["go", "run", ".", "--device", raw_container], cwd=IDENTITYDUMP_DIR)
    if proc.returncode != 0:
        return {
            "ok": False,
            "returncode": proc.returncode,
            "stderr": proc.stderr,
            "stdout_head": proc.stdout[:2000],
        }
    try:
        payload = json.loads(proc.stdout)
    except json.JSONDecodeError as exc:
        return {
            "ok": False,
            "returncode": proc.returncode,
            "error": f"json decode: {exc}",
            "stdout_head": proc.stdout[:2000],
        }
    return {
        "ok": True,
        "returncode": 0,
        "device": payload.get("device"),
        "volume": payload.get("volume"),
        "root_tree": payload.get("root_tree"),
        "node_count": len(payload.get("nodes", [])),
        "entry_count": len(payload.get("entries", [])),
    }


def run_rust_context(raw_container: str) -> dict:
    proc = run(
        ["cargo", "run", "--quiet", "--bin", "apfs-fastindex-scan", "--", raw_container],
        cwd=RUST_CRATE_DIR,
    )
    if proc.returncode != 0:
        return {
            "ok": False,
            "returncode": proc.returncode,
            "stderr": proc.stderr,
            "stdout_head": proc.stdout[:2000],
        }
    try:
        return {"ok": True, "payload": json.loads(proc.stdout)}
    except json.JSONDecodeError as exc:
        return {
            "ok": False,
            "returncode": proc.returncode,
            "error": f"json decode: {exc}",
            "stdout_head": proc.stdout[:2000],
        }


# ---- environment manifest ------------------------------------------------ #

def environment() -> dict:
    sw_vers = run(["sw_vers"])
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
        "go": shutil.which("go"),
        "fsck_apfs": shutil.which("fsck_apfs"),
        "sw_vers": sw_vers.stdout,
    }


# ---- driver -------------------------------------------------------------- #

def main() -> int:
    write_json("environment.json", environment())
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex15-", dir="/tmp"))
    image_path = base / "EX15CI.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_detach = None
    nomount_detach = None
    summary = {
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
                "160m",
                "-fs",
                "APFS",
                "-volname",
                "EX15CI",
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_variant_corpus(mountpoint)
        mounted_entries = snapshot_tree(mountpoint)
        write_json(
            "ex15-fixture-operations.json",
            {"image_path": str(image_path), "operations": operations},
        )
        write_json(
            "ex15-mounted-posix-oracle.json",
            {
                "volume_label": "EX15CI",
                "entries": mounted_entries,
                "summary": snapshot_summary(mounted_entries),
            },
        )
        detach_device(mounted_detach)
        mounted_detach = None
        settle()

        _, nomount_detach, raw_container = attach_nomount(image_path)
        # Give macOS a beat before going raw.
        time.sleep(0.5)

        fsck_result = run_fsck(raw_container)
        write_json("ex15-fsck.json", fsck_result)

        go_apfs = run_identitydump(raw_container)
        write_json("ex15-go-apfs.json", go_apfs)

        rust_context = run_rust_context(raw_container)
        write_json("ex15-rust-context.json", rust_context)

        # Replay candidates.
        with open(raw_container, "rb", buffering=0) as handle:
            block0 = read_block(handle, 0, 4096)
            block_size = le_u32(block0, 0x24)
            if block_size != 4096:
                block0 = read_block(handle, 0, block_size)
            block0_info = parse_nxsb_block0(block0)
            descriptor = descriptor_summary(handle, block_size, block0_info)
            replays: list[dict] = []
            # Try every candidate in descending xid order.
            for cand in sorted(descriptor["candidates"], key=lambda c: c["header"]["xid"], reverse=True):
                replay = replay_candidate(handle, block_size, cand)
                replays.append(replay)

            # Dump every block flagged as failing, plus block 1031 unconditionally
            # since it's the EX-14 signature.
            failing_blocks: set[int] = set()
            for replay in replays:
                for fb in replay.get("failure_blocks", []):
                    failing_blocks.add(fb)
            failing_blocks.add(1031)
            dumps = []
            for fb in sorted(failing_blocks):
                try:
                    dumps.append(dump_block(raw_container, block_size, fb))
                except ProbeError as exc:
                    dumps.append({"paddr": fb, "error": exc.detail})

        write_json(
            "ex15-candidate-replay.json",
            {
                "block_size": block_size,
                "block0": block0_info,
                "descriptor": descriptor,
                "replays": replays,
            },
        )
        write_json("ex15-failing-blocks.json", {"blocks": dumps})

        # Decide verdict.
        ok_replays = [r for r in replays if r["stage_failed"] is None]
        any_xid = any(r["stage_failed"] == "checkpoint_ring" for r in replays)
        rust_ok_payload = rust_context.get("payload") if rust_context.get("ok") else None
        rust_selected = (
            rust_ok_payload.get("selected_checkpoint") if rust_ok_payload else None
        )

        if rust_selected is not None and ok_replays:
            verdict = "rust_now_selects_context"
            detail = (
                "Rust returned a selected_checkpoint; deterministic rebuild does not reproduce "
                "the EX-14 blocker. Record run-to-run flakiness signature and move to EX-16."
            )
        elif ok_replays and rust_selected is None:
            # Python found at least one working checkpoint; Rust did not.
            verdict = "stale_checkpoint_selection_or_fallback_gap"
            detail = (
                f"Python replay found {len(ok_replays)} self-consistent checkpoint(s) at xids "
                + ", ".join(str(r["xid"]) for r in ok_replays)
                + "; Rust still aborts. Hypothesis (a) candidate: add a recorded-fallback "
                "rule that retries the next-highest checkpoint after a typed failure."
            )
        elif not ok_replays and fsck_result["returncode"] == 0:
            verdict = "image_clean_but_no_python_match"
            detail = (
                "fsck_apfs -n reports the image as clean, but Python's strict SR-005/SR-007 "
                "replay rejected every NXSB candidate. Hypothesis (b) or (c) — current "
                "Rust+Python rule set is too strict for what Apple's tooling treats as valid."
            )
        elif fsck_result["returncode"] != 0:
            verdict = "malformed_source_signature"
            detail = (
                "fsck_apfs -n is non-zero and Python found no self-consistent checkpoint. "
                "Record the signature in SR-016's fail-closed register."
            )
        else:
            verdict = "inconclusive"
            detail = (
                "No Python replay produced a clean checkpoint; need a deeper probe into "
                "block-1031 role and surrounding state."
            )
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["candidate_xids"] = [c["header"]["xid"] for c in descriptor["candidates"]]
        summary["ok_replay_xids"] = [r["xid"] for r in ok_replays]
        summary["rust_returned_selected_checkpoint"] = rust_selected is not None
        summary["fsck_returncode"] = fsck_result["returncode"]
        summary["go_apfs_ok"] = bool(go_apfs.get("ok"))
        write_json("summary.json", summary)
        return 0 if verdict in {"rust_now_selects_context"} else 1
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
        retained = GENERATED_DIR / "retained-image.dmg"
        try:
            if image_path.exists() and not retained.exists():
                shutil.copy2(image_path, retained)
                summary["retained_image"] = str(retained)
                write_json("summary.json", summary)
        except Exception:
            pass
        shutil.rmtree(base, ignore_errors=True)


if __name__ == "__main__":
    raise SystemExit(main())
