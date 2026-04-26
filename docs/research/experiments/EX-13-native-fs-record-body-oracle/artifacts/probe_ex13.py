#!/usr/bin/env python3
"""Run EX-13 as a Python-first raw FS-record body experiment.

The probe deliberately avoids adding new Rust parser behavior. It uses the
already-validated Rust scanner only to obtain the selected checkpoint and FS-tree
root context from EX-12's gate, then parses FS-tree record bodies directly in
Python and compares reconstructed namespace/logical-size rows to the mounted
POSIX oracle generated from the same fixture.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import plistlib
import shutil
import struct
import subprocess
import sys
from pathlib import Path
from typing import Any


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
OBJ_HEADER_SIZE = 32
BTREE_INFO_SIZE = 40
BTNODE_ROOT = 0x0001
BTNODE_LEAF = 0x0002
BTNODE_FIXED_KV_SIZE = 0x0004
OBJ_ID_MASK = (1 << 60) - 1
OBJ_TYPE_SHIFT = 60
J_DREC_LEN_MASK = 0x000003FF
DREC_TYPE_MASK = 0x000F
INODE_FIXED_SIZE = 0x5C
INODE_HAS_UNCOMPRESSED_SIZE = 0x00040000
INO_EXT_TYPE_NAME = 4
INO_EXT_TYPE_DSTREAM = 8
INO_EXT_TYPE_SPARSE_BYTES = 13
DREC_EXT_TYPE_SIBLING_ID = 1
XATTR_DATA_STREAM = 0x0001
XATTR_DATA_EMBEDDED = 0x0002
J_DSTREAM_SIZE = 40

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
REPO_ROOT = ARTIFACT_DIR.parents[4]
GENERATED_DIR.mkdir(exist_ok=True)
sys.path.insert(0, str(REPO_ROOT / "src"))

from apfs_fastindex.poc_fixture import build_proof_fixture  # noqa: E402

IDENTITYDUMP_DIR = (
    REPO_ROOT
    / "docs"
    / "research"
    / "experiments"
    / "EX-06-identity-tracking"
    / "artifacts"
    / "identitydump"
)
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"


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
        json.dumps(payload, indent=2, sort_keys=True) + "\n"
    )


def le_u16(data: bytes, offset: int) -> int:
    return struct.unpack_from("<H", data, offset)[0]


def le_i32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<i", data, offset)[0]


def le_u32(data: bytes, offset: int) -> int:
    return struct.unpack_from("<I", data, offset)[0]


def le_u64(data: bytes, offset: int) -> int:
    return struct.unpack_from("<Q", data, offset)[0]


def align8(value: int) -> int:
    return (value + 7) & ~7


def trim_nul(data: bytes) -> bytes:
    return data[:-1] if data.endswith(b"\x00") else data


def decode_string(data: bytes) -> tuple[str, str | None]:
    try:
        return trim_nul(data).decode("utf-8"), None
    except UnicodeDecodeError as err:
        return "", f"invalid utf-8: {err}"


def normalize_raw_device(device: str) -> str:
    if device.startswith("/dev/rdisk"):
        return device
    if device.startswith("/dev/disk"):
        return "/dev/rdisk" + device[len("/dev/disk") :]
    return device


def attach_nomount_image(image_path: Path) -> tuple[list[dict], str, str]:
    proc = run_checked(["hdiutil", "attach", "-plist", "-nomount", str(image_path)])
    info = plistlib.loads(proc.stdout.encode("utf-8"))
    entities = info.get("system-entities", [])
    if not entities:
        raise ProbeError("attach_failed", "hdiutil attach returned no system entities")
    detach_device = entities[0]["dev-entry"]
    container_device = None
    for entity in entities:
        if entity.get("content-hint") == APFS_CONTAINER_HINT:
            container_device = entity.get("dev-entry")
            break
    if not container_device:
        raise ProbeError("missing_apfs_container", "no APFS container from hdiutil")
    return entities, detach_device, normalize_raw_device(container_device)


def detach_device(device: str) -> None:
    run(["hdiutil", "detach", device])


def run_rust_context(raw_container: str) -> dict:
    proc = run_checked(
        ["cargo", "run", "--quiet", "--bin", "apfs-fastindex-scan", "--", raw_container],
        cwd=RUST_CRATE_DIR,
    )
    return json.loads(proc.stdout)


def run_identitydump(raw_container: str) -> dict:
    proc = run_checked(["go", "run", ".", "--device", raw_container], cwd=IDENTITYDUMP_DIR)
    return json.loads(proc.stdout)


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
        raise ProbeError("unsupported_record_body", "FS-tree node uses fixed kv size")
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


def parse_fs_tree(handle: Any, root_paddr: int, block_size: int) -> dict:
    records: list[dict] = []
    nodes: list[dict] = []

    def walk(paddr: int, is_root: bool) -> None:
        block = read_block(handle, paddr, block_size)
        node = parse_btree_node(block, block_size)
        nodes.append(
            {
                "paddr": paddr,
                "level": node["level"],
                "nkeys": node["nkeys"],
                "is_leaf": node["is_leaf"],
                "is_root": is_root,
            }
        )
        for index in range(node["nkeys"]):
            key, value = node_entry(block, node, index)
            if node["is_leaf"]:
                records.append(parse_record(paddr, index, key, value))
            else:
                if len(value) < 8:
                    raise ProbeError(
                        "malformed_record_body",
                        f"internal FS-tree value at {paddr}/{index} is shorter than child paddr",
                    )
                walk(le_u64(value, 0), False)

    walk(root_paddr, True)
    return {"nodes": nodes, "records": records}


def parse_record(node_paddr: int, entry_index: int, key: bytes, value: bytes) -> dict:
    if len(key) < 8:
        raise ProbeError("malformed_record_body", "FS record key shorter than j_key_t")
    key_word = le_u64(key, 0)
    object_id = key_word & OBJ_ID_MASK
    raw_type = key_word >> OBJ_TYPE_SHIFT
    notes: list[str] = []
    return {
        "node_paddr": node_paddr,
        "entry_index": entry_index,
        "object_id": object_id,
        "raw_type": raw_type,
        "family": family_name(raw_type),
        "key": parse_key(raw_type, key, notes),
        "value": parse_value(raw_type, value, notes),
        "key_len": len(key),
        "value_len": len(value),
        "validation_notes": notes,
    }


def family_name(raw_type: int) -> str:
    return {
        1: "snap_metadata",
        2: "extent_reference",
        3: "inode",
        4: "xattr",
        5: "sibling_link",
        6: "dstream_id",
        7: "crypto_state",
        8: "file_extent",
        9: "dir_rec",
        10: "dir_stats",
        11: "snap_name",
        12: "sibling_map",
        13: "file_info",
    }.get(raw_type, "unknown")


def parse_key(raw_type: int, key: bytes, notes: list[str]) -> dict:
    if raw_type == 9:
        if len(key) >= 12:
            name_len = le_u32(key, 8) & J_DREC_LEN_MASK
            if name_len and 12 + name_len <= len(key):
                name_bytes = key[12 : 12 + name_len]
                name, note = decode_string(name_bytes)
                if note:
                    notes.append(note)
                return {
                    "kind": "named",
                    "raw_key_form": "hashed",
                    "name_len": name_len,
                    "name": name,
                    "name_bytes_hex": name_bytes.hex(),
                }
        name_len = le_u16(key, 8)
        name_bytes = key[10 : 10 + name_len]
        name, note = decode_string(name_bytes)
        if note:
            notes.append(note)
        return {
            "kind": "named",
            "raw_key_form": "unhashed",
            "name_len": name_len,
            "name": name,
            "name_bytes_hex": name_bytes.hex(),
        }
    if raw_type == 4:
        name_len = le_u16(key, 8)
        name_bytes = key[10 : 10 + name_len]
        name, note = decode_string(name_bytes)
        if note:
            notes.append(note)
        return {
            "kind": "named",
            "raw_key_form": "xattr",
            "name_len": name_len,
            "name": name,
            "name_bytes_hex": name_bytes.hex(),
        }
    if raw_type == 5 and len(key) >= 16:
        return {"kind": "sibling_link", "sibling_id": le_u64(key, 8)}
    return {"kind": "plain"}


def parse_value(raw_type: int, value: bytes, notes: list[str]) -> dict:
    if raw_type == 3:
        return {"kind": "inode", **parse_inode(value)}
    if raw_type == 4:
        return {"kind": "xattr", **parse_xattr(value)}
    if raw_type == 5:
        return {"kind": "sibling_link", **parse_sibling_link(value, notes)}
    if raw_type == 6:
        return {"kind": "dstream_id", "refcnt": le_u32(value, 0) if len(value) >= 4 else None}
    if raw_type == 9:
        return {"kind": "dir_rec", **parse_dir_rec(value)}
    if raw_type == 12:
        return {"kind": "sibling_map", "file_id": le_u64(value, 0) if len(value) >= 8 else None}
    return {"kind": "unsupported"}


def parse_inode(value: bytes) -> dict:
    if len(value) < INODE_FIXED_SIZE:
        raise ProbeError("malformed_record_body", "inode value shorter than fixed body")
    internal_flags = le_u64(value, 0x30)
    xfields = parse_xfields(value[INODE_FIXED_SIZE:], INODE_FIXED_SIZE)
    dstream = next(
        (field["interpreted"]["value"] for field in xfields if field.get("interpreted", {}).get("kind") == "dstream"),
        None,
    )
    sparse_bytes = next(
        (field["interpreted"]["value"] for field in xfields if field["x_type"] == INO_EXT_TYPE_SPARSE_BYTES and field.get("interpreted", {}).get("kind") == "u64"),
        None,
    )
    inode_name = next(
        (field["interpreted"]["value"] for field in xfields if field["x_type"] == INO_EXT_TYPE_NAME and field.get("interpreted", {}).get("kind") == "utf8"),
        None,
    )
    return {
        "parent_id": le_u64(value, 0x00),
        "private_id": le_u64(value, 0x08),
        "internal_flags": internal_flags,
        "nchildren_or_nlink": le_i32(value, 0x38),
        "bsd_flags": le_u32(value, 0x44),
        "owner": le_u32(value, 0x48),
        "group": le_u32(value, 0x4C),
        "mode": le_u16(value, 0x50),
        "uncompressed_size": le_u64(value, 0x54),
        "has_uncompressed_size": bool(internal_flags & INODE_HAS_UNCOMPRESSED_SIZE),
        "xfields": xfields,
        "dstream": dstream,
        "sparse_bytes": sparse_bytes,
        "inode_name": inode_name,
    }


def parse_dir_rec(value: bytes) -> dict:
    if len(value) < 18:
        raise ProbeError("malformed_record_body", "directory record shorter than fixed body")
    flags = le_u16(value, 0x10)
    xfields = parse_xfields(value[18:], 18)
    sibling_id = next(
        (field["interpreted"]["value"] for field in xfields if field["x_type"] == DREC_EXT_TYPE_SIBLING_ID and field.get("interpreted", {}).get("kind") == "u64"),
        None,
    )
    return {
        "file_id": le_u64(value, 0),
        "date_added": le_u64(value, 8),
        "flags": flags,
        "entry_type": flags & DREC_TYPE_MASK,
        "sibling_id": sibling_id,
        "xfields": xfields,
    }


def parse_xattr(value: bytes) -> dict:
    if len(value) < 4:
        raise ProbeError("malformed_record_body", "xattr value shorter than fixed body")
    flags = le_u16(value, 0)
    xdata_len = le_u16(value, 2)
    is_embedded = bool(flags & XATTR_DATA_EMBEDDED)
    is_stream = bool(flags & XATTR_DATA_STREAM)
    payload = value[4 : 4 + xdata_len] if is_embedded else value[4:]
    payload_utf8, _ = decode_string(payload)
    stream_object_id = le_u64(payload, 0) if is_stream and len(payload) >= 8 else None
    dstream = parse_dstream(payload[8 : 8 + J_DSTREAM_SIZE]) if is_stream and len(payload) >= 48 else None
    return {
        "flags": flags,
        "xdata_len": xdata_len,
        "is_embedded": is_embedded,
        "is_stream": is_stream,
        "payload_utf8": payload_utf8 if payload_utf8 else None,
        "payload_hex": payload.hex(),
        "stream_object_id": stream_object_id,
        "dstream": dstream,
    }


def parse_sibling_link(value: bytes, notes: list[str]) -> dict:
    if len(value) < 10:
        raise ProbeError("malformed_record_body", "sibling link shorter than fixed body")
    name_len = le_u16(value, 8)
    name_bytes = value[10 : 10 + name_len]
    name, note = decode_string(name_bytes)
    if note:
        notes.append(note)
    return {
        "parent_id": le_u64(value, 0),
        "name_len": name_len,
        "name": name,
        "name_bytes_hex": name_bytes.hex(),
    }


def parse_xfields(value: bytes, base_offset: int) -> list[dict]:
    if not value:
        return []
    if len(value) < 4:
        raise ProbeError("malformed_record_body", "xfield blob shorter than header")
    count = le_u16(value, 0)
    used_data = le_u16(value, 2)
    fields_end = 4 + count * 4
    if fields_end > len(value) or used_data > len(value) - 4:
        raise ProbeError("malformed_record_body", "xfield metadata exceeds value")
    metadata = []
    for index in range(count):
        offset = 4 + index * 4
        metadata.append((value[offset], value[offset + 1], le_u16(value, offset + 2)))
    errors = []
    candidates = []
    layouts = (
        (
            "record_relative_start_record_relative_fields",
            align_relative(base_offset, fields_end),
            lambda cursor: align_relative(base_offset, cursor),
        ),
        (
            "unpacked_start_record_relative_fields",
            fields_end,
            lambda cursor: align_relative(base_offset, cursor),
        ),
        (
            "unpacked_start_blob_relative_fields",
            fields_end,
            align8,
        ),
        (
            "blob_relative_start_blob_relative_fields",
            align8(fields_end),
            align8,
        ),
    )
    for data_start_name, data_start, align_next in layouts:
        try:
            fields = parse_xfield_data(
                value, metadata, data_start, data_start_name, align_next
            )
            candidates.append((score_xfields(fields), fields))
        except ProbeError as err:
            errors.append(f"{data_start_name}: {err.detail}")
    if candidates:
        candidates.sort(key=lambda item: item[0], reverse=True)
        return candidates[0][1]
    raise ProbeError(
        "malformed_record_body",
        "xfield data exceeds value for all tested layouts: " + "; ".join(errors),
    )


def parse_xfield_data(
    value: bytes,
    metadata: list[tuple[int, int, int]],
    data_start: int,
    layout: str,
    align_next: Any,
) -> list[dict]:
    cursor = data_start
    fields = []
    for x_type, x_flags, x_size in metadata:
        data = value[cursor : cursor + x_size]
        if len(data) != x_size:
            raise ProbeError(
                "malformed_record_body",
                f"xfield data exceeds value at cursor={cursor} size={x_size} len={len(value)}",
            )
        fields.append(
            {
                "x_type": x_type,
                "x_flags": x_flags,
                "x_size": x_size,
                "layout": layout,
                "value_hex": data.hex(),
                "interpreted": interpret_xfield(x_type, data),
            }
        )
        cursor = align_next(cursor + x_size)
    return fields


def align_relative(base_offset: int, relative_offset: int) -> int:
    return align8(base_offset + relative_offset) - base_offset


def score_xfields(fields: list[dict]) -> int:
    score = 0
    for field in fields:
        interpreted = field.get("interpreted") or {}
        if interpreted.get("kind") == "utf8" and interpreted.get("value"):
            value = interpreted["value"]
            stripped_value = value.rstrip("\x00")
            if not stripped_value or "\x00" in stripped_value:
                score -= 5
            else:
                score += 5
        if interpreted.get("kind") == "dstream":
            value = interpreted["value"]
            if (
                value["size"] < (1 << 40)
                and value["alloced_size"] < (1 << 40)
                and value["total_bytes_written"] < (1 << 40)
            ):
                score += 10
            else:
                score -= 100
        if interpreted.get("kind") == "u64":
            if interpreted["value"] < (1 << 40):
                score += 2
            else:
                score -= 10
    return score


def interpret_xfield(x_type: int, data: bytes) -> dict | None:
    if x_type in {1, 5, 13, 16} and len(data) == 8:
        return {"kind": "u64", "value": le_u64(data, 0)}
    if x_type == INO_EXT_TYPE_NAME:
        value, note = decode_string(data)
        return {"kind": "utf8", "value": value, "note": note}
    if x_type == INO_EXT_TYPE_DSTREAM and len(data) >= J_DSTREAM_SIZE:
        return {"kind": "dstream", "value": parse_dstream(data[:J_DSTREAM_SIZE])}
    return None


def parse_dstream(data: bytes) -> dict:
    return {
        "size": le_u64(data, 0),
        "alloced_size": le_u64(data, 8),
        "default_crypto_id": le_u64(data, 16),
        "total_bytes_written": le_u64(data, 24),
        "total_bytes_read": le_u64(data, 32),
    }


def entry_type_name(entry_type: int) -> str:
    return {4: "dir", 8: "file", 10: "symlink"}.get(entry_type, f"other({entry_type})")


def reconstruct_entries(records: list[dict]) -> list[dict]:
    children_by_parent: dict[int, list[dict]] = {}
    inode_by_id: dict[int, dict] = {}
    xattrs_by_id: dict[int, list[dict]] = {}
    sibling_links: list[dict] = []
    sibling_maps: list[dict] = []

    for record in records:
        family = record["family"]
        if family == "dir_rec":
            parent_id = record["object_id"]
            children_by_parent.setdefault(parent_id, []).append(record)
        elif family == "inode":
            inode_by_id[record["object_id"]] = record
        elif family == "xattr":
            xattrs_by_id.setdefault(record["object_id"], []).append(record)
        elif family == "sibling_link":
            sibling_links.append(record)
        elif family == "sibling_map":
            sibling_maps.append(record)

    entries = [{"path": ".", "type": "dir", "file_id": 2}]

    def walk_dir(parent_id: int, parent_path: str) -> None:
        for record in sorted(
            children_by_parent.get(parent_id, []),
            key=lambda item: item["key"].get("name", ""),
        ):
            name = record["key"].get("name", "")
            if name == ".fseventsd":
                continue
            value = record["value"]
            file_id = value["file_id"]
            entry_type = entry_type_name(value["entry_type"])
            path = name if parent_path == "." else f"{parent_path}/{name}"
            out = {"path": path, "type": entry_type, "file_id": file_id}
            inode = inode_by_id.get(file_id, {}).get("value", {})
            dstream = inode.get("dstream")
            if entry_type in {"file", "symlink"} and dstream:
                out["logical_size"] = dstream["size"]
            if entry_type == "symlink":
                target = symlink_target(xattrs_by_id.get(file_id, []))
                if target is not None:
                    out["symlink_target"] = target
                    if not out.get("logical_size"):
                        out["logical_size"] = len(target)
            entries.append(out)
            if entry_type == "dir":
                walk_dir(file_id, path)

    walk_dir(2, ".")
    return sorted(entries, key=lambda item: item["path"])


def symlink_target(records: list[dict]) -> str | None:
    for record in records:
        if record["key"].get("name") != "com.apple.fs.symlink":
            continue
        payload = record["value"].get("payload_utf8")
        if payload is not None:
            return payload
    return None


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


def compare_entries(mounted: list[dict], native: list[dict]) -> dict:
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


def family_counts(records: list[dict]) -> dict[str, int]:
    counts: dict[str, int] = {}
    for record in records:
        counts[record["family"]] = counts.get(record["family"], 0) + 1
    return counts


def run_probe() -> dict:
    detach = None
    with build_proof_fixture() as fixture:
        mounted_oracle = json.loads(fixture.oracle_path.read_text())
        entities, detach, raw_container = attach_nomount_image(fixture.image_path)
        try:
            rust_context = run_rust_context(raw_container)
            identitydump = run_identitydump(raw_container)
            selected = rust_context["selected_checkpoint"]
            block_size = selected["container"]["block_size"]
            volume = selected["volumes"][0]
            root_paddr = volume["root_tree_lookup"]["paddr"]
            with open(raw_container, "rb", buffering=0) as handle:
                fs_tree = parse_fs_tree(handle, root_paddr, block_size)
            native_entries = reconstruct_entries(fs_tree["records"])
            comparison = compare_entries(mounted_oracle["entries"], native_entries)
            return {
                "source": {
                    "image_path": str(fixture.image_path),
                    "raw_container_path": raw_container,
                    "nomount_entities": entities,
                    "fixture_operations": list(fixture.operations),
                    "image_size_bytes": fixture.image_path.stat().st_size,
                },
                "mounted_oracle": mounted_oracle,
                "rust_context": rust_context,
                "go_apfs_observer": identitydump,
                "native_record_body_dump": {
                    "selected_xid": selected["xid"],
                    "block_size": block_size,
                    "volume_oid": volume["volume_oid"],
                    "root_tree_lookup": volume["root_tree_lookup"],
                    "nodes": fs_tree["nodes"],
                    "records": fs_tree["records"],
                    "family_counts": family_counts(fs_tree["records"]),
                    "reconstructed_entries": native_entries,
                },
                "comparison": comparison,
            }
        finally:
            if detach:
                detach_device(detach)


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "hdiutil_available": shutil.which("hdiutil") is not None,
        "cargo_available": shutil.which("cargo") is not None,
        "go_available": shutil.which("go") is not None,
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
    }


def main() -> int:
    write_json("environment.json", environment())
    try:
        result = run_probe()
        write_json("fixture-operations.json", result["source"]["fixture_operations"])
        write_json("mounted-posix-oracle.json", result["mounted_oracle"])
        write_json("native-record-body-dump.json", result["native_record_body_dump"])
        write_json("go-apfs-record-observer.json", result["go_apfs_observer"])
        write_json("comparison.json", result["comparison"])
        verdict = (
            "validated_native_record_body_contract"
            if result["comparison"]["matched"]
            else "body_field_mismatch"
        )
        summary = {
            "status": "executed",
            "verdict": verdict,
            "selected_xid": result["native_record_body_dump"]["selected_xid"],
            "block_size": result["native_record_body_dump"]["block_size"],
            "record_count": len(result["native_record_body_dump"]["records"]),
            "node_count": len(result["native_record_body_dump"]["nodes"]),
            "family_counts": result["native_record_body_dump"]["family_counts"],
            "reconstructed_entry_count": len(
                result["native_record_body_dump"]["reconstructed_entries"]
            ),
            "mounted_entry_count": len(result["mounted_oracle"]["entries"]),
            "comparison_matched": result["comparison"]["matched"],
            "missing_path_count": len(result["comparison"]["missing_paths"]),
            "unexpected_path_count": len(result["comparison"]["unexpected_paths"]),
            "mismatch_count": len(result["comparison"]["mismatches"]),
            "implementation_note": (
                "Record bodies were decoded by Python from raw FS-tree bytes. "
                "Rust was used only as the existing EX-12 context provider for "
                "selected checkpoint and root-tree paddr; no new Rust parser "
                "behavior was added."
            ),
        }
        write_json("summary.json", summary)
        return 0
    except ProbeError as err:
        summary = {
            "status": "executed",
            "verdict": err.verdict,
            "detail": err.detail,
        }
        write_json("summary.json", summary)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
