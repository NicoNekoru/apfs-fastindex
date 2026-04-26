#!/usr/bin/env python3
"""Run EX-11: checkpoint-map integrity and ephemeral-object validation."""

from __future__ import annotations

import hashlib
import json
import os
import plistlib
import platform
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import BinaryIO


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
NX_MAGIC = 0x4253584E
OBJECT_TYPE_MASK = 0x0000FFFF
OBJECT_TYPE_NX_SUPERBLOCK = 0x0001
OBJECT_TYPE_CHECKPOINT_MAP = 0x000C
OBJECT_TYPE_BTREE = 0x0002
OBJECT_TYPE_OMAP = 0x000B
OBJECT_TYPE_SPACEMAN = 0x0005
OBJ_PHYSICAL = 0x40000000
CHECKPOINT_MAP_LAST = 0x00000001
MAX_DESCRIPTOR_STEPS = 10_000
MAX_EPHEMERAL_BLOCKS = 16

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
REPO_ROOT = ARTIFACT_DIR.parents[4]
GENERATED_DIR.mkdir(exist_ok=True)
sys.path.insert(0, str(REPO_ROOT / "src"))

from apfs_fastindex.poc_fixture import build_proof_fixture  # noqa: E402


class ProbeError(RuntimeError):
    def __init__(self, verdict: str, detail: str) -> None:
        super().__init__(detail)
        self.verdict = verdict
        self.detail = detail


def run(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_checked(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    proc = run(cmd)
    if proc.returncode != 0:
        raise ProbeError(
            "command_failed",
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}",
        )
    return proc


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def le_u32(block: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from("<I", block, offset)[0]


def le_u64(block: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from("<Q", block, offset)[0]


def put_u32(block: bytearray, offset: int, value: int) -> None:
    struct.pack_into("<I", block, offset, value)


def put_u64(block: bytearray, offset: int, value: int) -> None:
    struct.pack_into("<Q", block, offset, value)


def apfs_fletcher64(block: bytes | bytearray) -> int:
    lower = 0
    upper = 0
    data = bytes(block)
    for start in range(8, len(data), 4):
        chunk = data[start : start + 4]
        if len(chunk) < 4:
            chunk = chunk + b"\x00" * (4 - len(chunk))
        lower += int.from_bytes(chunk, "little")
        upper += lower
        if ((start - 8) // 4 + 1) % 1024 == 0:
            lower %= 0xFFFFFFFF
            upper %= 0xFFFFFFFF
    lower %= 0xFFFFFFFF
    upper %= 0xFFFFFFFF
    checksum_lower = 0xFFFFFFFF - ((lower + upper) % 0xFFFFFFFF)
    checksum_upper = 0xFFFFFFFF - ((lower + checksum_lower) % 0xFFFFFFFF)
    return (checksum_upper << 32) | checksum_lower


def set_checksum(block: bytearray) -> None:
    put_u64(block, 0, 0)
    put_u64(block, 0, apfs_fletcher64(block))


def checksum_matches(block: bytes | bytearray) -> bool:
    return le_u64(block, 0) == apfs_fletcher64(block)


def type_name(raw_type: int) -> str:
    base = raw_type & OBJECT_TYPE_MASK
    names = {
        OBJECT_TYPE_NX_SUPERBLOCK: "OBJECT_TYPE_NX_SUPERBLOCK",
        OBJECT_TYPE_BTREE: "OBJECT_TYPE_BTREE",
        OBJECT_TYPE_SPACEMAN: "OBJECT_TYPE_SPACEMAN",
        OBJECT_TYPE_OMAP: "OBJECT_TYPE_OMAP",
        OBJECT_TYPE_CHECKPOINT_MAP: "OBJECT_TYPE_CHECKPOINT_MAP",
    }
    return names.get(base, f"OBJECT_TYPE_{base:#x}")


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
    detach_device = entities[0]["dev-entry"]
    container_device = None
    for entity in entities:
        if entity.get("content-hint") == APFS_CONTAINER_HINT:
            container_device = entity.get("dev-entry")
            break
    if not container_device:
        raise ProbeError("missing_apfs_container", "hdiutil attach did not expose an APFS container")
    return entities, detach_device, normalize_raw_device(container_device)


def detach_device(device: str) -> None:
    run(["hdiutil", "detach", device])


def read_block(handle: BinaryIO, block_address: int, block_size: int) -> bytes:
    handle.seek(block_address * block_size)
    block = handle.read(block_size)
    if len(block) != block_size:
        raise ProbeError(
            "short_read",
            f"short read for block {block_address}: expected {block_size}, got {len(block)}",
        )
    return block


def parse_nxsb(block: bytes, descriptor_index: int | None, block_address: int) -> dict | None:
    if le_u32(block, 0x20) != NX_MAGIC:
        return None
    raw_type = le_u32(block, 0x18)
    if raw_type & OBJECT_TYPE_MASK != OBJECT_TYPE_NX_SUPERBLOCK:
        raise ProbeError("malformed_checkpoint_map", f"NXSB magic with wrong type {raw_type:#x}")
    if not checksum_matches(block):
        raise ProbeError("malformed_checkpoint_map", f"NXSB checksum mismatch at block {block_address}")
    return {
        "descriptor_index": descriptor_index,
        "block_address": block_address,
        "oid": le_u64(block, 0x08),
        "xid": le_u64(block, 0x10),
        "object_type_raw": raw_type,
        "object_subtype": le_u32(block, 0x1C),
        "checksum": le_u64(block, 0),
        "block": block,
    }


def choose_checkpoint(handle: BinaryIO) -> tuple[dict, list[dict], dict]:
    block0_probe = read_block(handle, 0, 4096)
    block_size = le_u32(block0_probe, 0x24)
    block0 = block0_probe if block_size == 4096 else read_block(handle, 0, block_size)
    block0_info = parse_nxsb(block0, None, 0)
    if block0_info is None:
        raise ProbeError("invalid_block_zero", "block zero is not an NX superblock")

    desc_blocks_raw = le_u32(block0, 0x68)
    data_blocks_raw = le_u32(block0, 0x6C)
    desc_blocks = desc_blocks_raw & 0x7FFFFFFF
    data_blocks = data_blocks_raw & 0x7FFFFFFF
    desc_base_raw = le_u64(block0, 0x70)
    data_base_raw = le_u64(block0, 0x78)
    desc_base = desc_base_raw & ((1 << 63) - 1)
    data_base = data_base_raw & ((1 << 63) - 1)
    layout = {
        "block_size": block_size,
        "descriptor_blocks": desc_blocks,
        "descriptor_base": desc_base,
        "descriptor_base_non_contiguous": bool(desc_base_raw >> 63) or bool(desc_blocks_raw >> 31),
        "data_blocks": data_blocks,
        "data_base": data_base,
        "data_base_non_contiguous": bool(data_base_raw >> 63) or bool(data_blocks_raw >> 31),
    }
    if layout["descriptor_base_non_contiguous"]:
        raise ProbeError(
            "unsupported_non_contiguous_descriptors",
            "non-contiguous checkpoint descriptor layouts are outside EX-11 positive support",
        )

    candidates = []
    for index in range(desc_blocks):
        block_address = desc_base + index
        block = read_block(handle, block_address, block_size)
        candidate = parse_nxsb(block, index, block_address)
        if candidate:
            candidates.append(candidate)
    if not candidates:
        raise ProbeError("malformed_checkpoint_map", "no checksum-valid checkpoint superblock candidates")
    selected = max(candidates, key=lambda candidate: candidate["xid"])
    return selected, candidates, layout


def checkpoint_fields(nxsb: bytes) -> dict:
    desc_blocks_raw = le_u32(nxsb, 0x68)
    data_blocks_raw = le_u32(nxsb, 0x6C)
    desc_base_raw = le_u64(nxsb, 0x70)
    data_base_raw = le_u64(nxsb, 0x78)
    return {
        "descriptor_blocks": desc_blocks_raw & 0x7FFFFFFF,
        "data_blocks": data_blocks_raw & 0x7FFFFFFF,
        "descriptor_base": desc_base_raw & ((1 << 63) - 1),
        "data_base": data_base_raw & ((1 << 63) - 1),
        "descriptor_base_non_contiguous": bool(desc_base_raw >> 63) or bool(desc_blocks_raw >> 31),
        "data_base_non_contiguous": bool(data_base_raw >> 63) or bool(data_blocks_raw >> 31),
        "descriptor_next": le_u32(nxsb, 0x80),
        "data_next": le_u32(nxsb, 0x84),
        "descriptor_index": le_u32(nxsb, 0x88),
        "descriptor_len": le_u32(nxsb, 0x8C),
        "data_index": le_u32(nxsb, 0x90),
        "data_len": le_u32(nxsb, 0x94),
        "spaceman_oid": le_u64(nxsb, 0x98),
        "container_omap_oid": le_u64(nxsb, 0xA0),
    }


def parse_mapping(block: bytes, offset: int) -> dict:
    raw_type = le_u32(block, offset)
    return {
        "type_raw": raw_type,
        "type": type_name(raw_type),
        "subtype_raw": le_u32(block, offset + 4),
        "subtype": type_name(le_u32(block, offset + 4)),
        "size": le_u32(block, offset + 8),
        "fs_oid": le_u64(block, offset + 16),
        "oid": le_u64(block, offset + 24),
        "paddr": le_u64(block, offset + 32),
    }


def validate_ephemeral(handle: BinaryIO, mapping: dict, fields: dict, selected_xid: int, block_size: int) -> dict:
    size = mapping["size"]
    if size == 0 or size % block_size != 0:
        raise ProbeError("bad_ephemeral_object", f"invalid mapped object size {size} for oid {mapping['oid']}")
    block_count = size // block_size
    if block_count > MAX_EPHEMERAL_BLOCKS:
        raise ProbeError(
            "bad_ephemeral_object",
            f"mapped object oid {mapping['oid']} spans {block_count} blocks, above EX-11 limit",
        )
    data_base = fields["data_base"]
    data_blocks = fields["data_blocks"]
    if data_blocks == 0:
        raise ProbeError("bad_ephemeral_object", "checkpoint data area has zero blocks")

    object_bytes = bytearray()
    data_index = mapping["paddr"] - data_base
    if data_index < 0 or data_index >= data_blocks:
        raise ProbeError(
            "bad_ephemeral_object",
            f"mapped paddr {mapping['paddr']} is outside checkpoint data area",
        )
    read_blocks = []
    for _ in range(block_count):
        block_address = data_base + data_index
        object_bytes.extend(read_block(handle, block_address, block_size))
        read_blocks.append(block_address)
        data_index = (data_index + 1) % data_blocks

    object_data = bytes(object_bytes)
    if not checksum_matches(object_data):
        raise ProbeError("bad_ephemeral_object", f"bad checksum for mapped object oid {mapping['oid']}")
    raw_type = le_u32(object_data, 0x18)
    subtype = le_u32(object_data, 0x1C)
    object_xid = le_u64(object_data, 0x10)
    if raw_type != mapping["type_raw"] or subtype != mapping["subtype_raw"]:
        raise ProbeError(
            "bad_ephemeral_object",
            f"type/subtype mismatch for mapped object oid {mapping['oid']}",
        )
    if object_xid > selected_xid:
        raise ProbeError(
            "bad_ephemeral_object",
            f"mapped object oid {mapping['oid']} has xid {object_xid} newer than checkpoint {selected_xid}",
        )
    return {
        "oid": mapping["oid"],
        "type_raw": raw_type,
        "type": type_name(raw_type),
        "subtype_raw": subtype,
        "subtype": type_name(subtype),
        "size": size,
        "object_xid": object_xid,
        "paddr": mapping["paddr"],
        "read_blocks": read_blocks,
        "checksum": le_u64(object_data, 0),
        "sha256": hashlib.sha256(object_data).hexdigest(),
    }


def validate_checkpoint_map_chain(handle: BinaryIO, selected: dict, block_size: int) -> dict:
    fields = checkpoint_fields(selected["block"])
    if fields["descriptor_base_non_contiguous"]:
        raise ProbeError(
            "unsupported_non_contiguous_descriptors",
            "selected checkpoint uses non-contiguous descriptor layout",
        )
    if fields["descriptor_blocks"] == 0:
        raise ProbeError("malformed_checkpoint_map", "descriptor block count is zero")
    if fields["descriptor_index"] >= fields["descriptor_blocks"]:
        raise ProbeError("malformed_checkpoint_map", "checkpoint-map descriptor index is out of range")

    maps = []
    mappings = []
    ephemeral_objects = []
    current_index = fields["descriptor_index"]
    max_steps = min(MAX_DESCRIPTOR_STEPS, fields["descriptor_blocks"] + 1)
    for step in range(max_steps):
        block_address = fields["descriptor_base"] + current_index
        block = read_block(handle, block_address, block_size)
        raw_type = le_u32(block, 0x18)
        if raw_type & OBJECT_TYPE_MASK != OBJECT_TYPE_CHECKPOINT_MAP:
            raise ProbeError(
                "malformed_checkpoint_map",
                f"descriptor index {current_index} is not a checkpoint map: {raw_type:#x}",
            )
        if not checksum_matches(block):
            raise ProbeError("malformed_checkpoint_map", f"checkpoint-map checksum mismatch at index {current_index}")
        flags = le_u32(block, 0x20)
        count = le_u32(block, 0x24)
        max_count = (block_size - 0x28) // 40
        if count > max_count:
            raise ProbeError("malformed_checkpoint_map", f"cpm_count {count} exceeds max {max_count}")
        map_info = {
            "step": step,
            "descriptor_index": current_index,
            "block_address": block_address,
            "xid": le_u64(block, 0x10),
            "flags": flags,
            "is_last": bool(flags & CHECKPOINT_MAP_LAST),
            "count": count,
            "checksum": le_u64(block, 0),
        }
        maps.append(map_info)
        for entry_index in range(count):
            mapping = parse_mapping(block, 0x28 + entry_index * 40)
            mapping["map_descriptor_index"] = current_index
            mapping["map_entry_index"] = entry_index
            mappings.append(mapping)
            ephemeral_objects.append(validate_ephemeral(handle, mapping, fields, selected["xid"], block_size))
        if flags & CHECKPOINT_MAP_LAST:
            return {
                "verdict": "validated_checkpoint_context",
                "selected_checkpoint": {
                    "descriptor_index": selected["descriptor_index"],
                    "block_address": selected["block_address"],
                    "xid": selected["xid"],
                    "oid": selected["oid"],
                    "checksum": selected["checksum"],
                },
                "checkpoint_fields": fields,
                "checkpoint_maps": maps,
                "mappings": mappings,
                "ephemeral_objects": ephemeral_objects,
            }
        current_index = (current_index + 1) % fields["descriptor_blocks"]
    raise ProbeError("malformed_checkpoint_map", "checkpoint-map chain did not reach CHECKPOINT_MAP_LAST")


def run_positive_fixture() -> dict:
    detach = None
    with build_proof_fixture() as fixture:
        entities, detach, raw_container = attach_nomount_image(fixture.image_path)
        try:
            with open(raw_container, "rb", buffering=0) as handle:
                selected, candidates, layout = choose_checkpoint(handle)
                result = validate_checkpoint_map_chain(handle, selected, layout["block_size"])
            result["source"] = {
                "source_id": "generated-proof-fixture",
                "image_name": fixture.image_path.name,
                "raw_container_path": raw_container,
                "nomount_entities": entities,
                "fixture_operations": list(fixture.operations),
                "existing_proof_artifacts_reused": False,
                "existing_proof_artifacts_note": (
                    "EX-03/EX-04/EX-06/EX-07 preserve JSON oracles but no reusable image files"
                ),
            }
            result["candidate_count"] = len(candidates)
            result["checkpoint_candidates"] = [
                {
                    "descriptor_index": candidate["descriptor_index"],
                    "block_address": candidate["block_address"],
                    "xid": candidate["xid"],
                    "oid": candidate["oid"],
                    "object_type_raw": candidate["object_type_raw"],
                    "checksum": candidate["checksum"],
                }
                for candidate in candidates
            ]
            return result
        finally:
            if detach:
                detach_device(detach)


class SyntheticReader:
    def __init__(self, blocks: dict[int, bytes], block_size: int) -> None:
        self.blocks = blocks
        self.block_size = block_size

    def read_block(self, block_address: int) -> bytes:
        block = self.blocks.get(block_address)
        if block is None:
            raise ProbeError("short_read", f"synthetic short read for block {block_address}")
        return block


def synthetic_object(block_size: int, oid: int, xid: int, raw_type: int, subtype: int) -> bytes:
    block = bytearray(block_size)
    put_u64(block, 0x08, oid)
    put_u64(block, 0x10, xid)
    put_u32(block, 0x18, raw_type)
    put_u32(block, 0x1C, subtype)
    block[0x20:0x28] = b"SYNTHETC"
    set_checksum(block)
    return bytes(block)


def synthetic_map_block(
    block_size: int,
    flags: int,
    count: int,
    mappings: list[dict],
    checksum_valid: bool = True,
) -> bytes:
    block = bytearray(block_size)
    put_u64(block, 0x08, 99)
    put_u64(block, 0x10, 10)
    put_u32(block, 0x18, OBJ_PHYSICAL | OBJECT_TYPE_CHECKPOINT_MAP)
    put_u32(block, 0x1C, 0)
    put_u32(block, 0x20, flags)
    put_u32(block, 0x24, count)
    for index, mapping in enumerate(mappings):
        offset = 0x28 + index * 40
        put_u32(block, offset, mapping["type_raw"])
        put_u32(block, offset + 4, mapping.get("subtype_raw", 0))
        put_u32(block, offset + 8, mapping["size"])
        put_u64(block, offset + 16, mapping.get("fs_oid", 0))
        put_u64(block, offset + 24, mapping["oid"])
        put_u64(block, offset + 32, mapping["paddr"])
    set_checksum(block)
    if not checksum_valid:
        block[-1] ^= 0xFF
    return bytes(block)


def validate_synthetic_chain(case: dict) -> dict:
    block_size = 4096
    fields = {
        "descriptor_blocks": case.get("descriptor_blocks", 2),
        "data_blocks": case.get("data_blocks", 2),
        "descriptor_base": 10,
        "data_base": 20,
        "descriptor_base_non_contiguous": case.get("non_contiguous", False),
        "data_base_non_contiguous": False,
        "descriptor_index": 0,
        "descriptor_len": 1,
        "data_index": 0,
        "data_len": 1,
    }
    if fields["descriptor_base_non_contiguous"]:
        raise ProbeError("unsupported_non_contiguous_descriptors", "synthetic non-contiguous descriptor layout")
    object_block = synthetic_object(
        block_size,
        oid=7,
        xid=8,
        raw_type=OBJ_PHYSICAL | OBJECT_TYPE_OMAP,
        subtype=0,
    )
    if case.get("bad_ephemeral_checksum"):
        object_mutable = bytearray(object_block)
        object_mutable[-1] ^= 0xFF
        object_block = bytes(object_mutable)

    mapping = {
        "type_raw": OBJ_PHYSICAL | OBJECT_TYPE_OMAP,
        "subtype_raw": 0,
        "size": case.get("mapped_size", block_size),
        "fs_oid": 0,
        "oid": 7,
        "paddr": 20,
    }
    if case.get("missing_last"):
        flags = 0
    else:
        flags = CHECKPOINT_MAP_LAST
    map_count = case.get("map_count", 1)
    map_block = synthetic_map_block(block_size, flags, map_count, [mapping])
    blocks = {10: map_block, 20: object_block}
    reader = SyntheticReader(blocks, block_size)

    def synthetic_read(block_address: int) -> bytes:
        return reader.read_block(block_address)

    maps = []
    mappings = []
    ephemeral_objects = []
    current_index = fields["descriptor_index"]
    for step in range(min(MAX_DESCRIPTOR_STEPS, fields["descriptor_blocks"] + 1)):
        block = synthetic_read(fields["descriptor_base"] + current_index)
        if le_u32(block, 0x18) & OBJECT_TYPE_MASK != OBJECT_TYPE_CHECKPOINT_MAP:
            raise ProbeError("malformed_checkpoint_map", "synthetic descriptor is not checkpoint map")
        if not checksum_matches(block):
            raise ProbeError("malformed_checkpoint_map", "synthetic map checksum mismatch")
        count = le_u32(block, 0x24)
        max_count = (block_size - 0x28) // 40
        if count > max_count:
            raise ProbeError("malformed_checkpoint_map", f"synthetic cpm_count {count} exceeds max {max_count}")
        flags = le_u32(block, 0x20)
        maps.append({"descriptor_index": current_index, "flags": flags, "count": count})
        for entry_index in range(count):
            mapping = parse_mapping(block, 0x28 + entry_index * 40)
            mappings.append(mapping)
            size = mapping["size"]
            if size == 0 or size % block_size != 0:
                raise ProbeError("bad_ephemeral_object", f"synthetic invalid mapped object size {size}")
            object_data = synthetic_read(mapping["paddr"])
            if not checksum_matches(object_data):
                raise ProbeError("bad_ephemeral_object", "synthetic bad ephemeral checksum")
            ephemeral_objects.append({"oid": mapping["oid"], "size": size, "paddr": mapping["paddr"]})
        if flags & CHECKPOINT_MAP_LAST:
            return {
                "verdict": "validated_checkpoint_context",
                "checkpoint_maps": maps,
                "mappings": mappings,
                "ephemeral_objects": ephemeral_objects,
            }
        current_index = (current_index + 1) % fields["descriptor_blocks"]
    raise ProbeError("malformed_checkpoint_map", "synthetic missing CHECKPOINT_MAP_LAST")


def run_synthetic_case(case: dict) -> dict:
    try:
        result = validate_synthetic_chain(case)
        observed = result["verdict"]
        detail = "synthetic case unexpectedly validated" if case["expected"] != observed else "validated as expected"
    except ProbeError as err:
        observed = err.verdict
        detail = err.detail
    return {
        "case_id": case["case_id"],
        "expected_verdict": case["expected"],
        "observed_verdict": observed,
        "matched_expectation": observed == case["expected"],
        "detail": detail,
        "case": case,
    }


def inventory_existing_artifacts() -> dict:
    experiments = ["EX-03", "EX-04", "EX-06", "EX-07", "EX-10"]
    artifact_files = []
    image_files = []
    for experiment in experiments:
        for path in (REPO_ROOT / "docs" / "research" / "experiments").glob(f"{experiment}-*/artifacts/**/*"):
            if path.is_file() and path.suffix == ".json":
                artifact_files.append(str(path.relative_to(REPO_ROOT)))
            if path.is_file() and path.suffix.lower() in {".dmg", ".img", ".raw", ".bin"}:
                image_files.append(str(path.relative_to(REPO_ROOT)))
    return {
        "json_artifact_count": len(artifact_files),
        "image_artifact_count": len(image_files),
        "json_artifacts_sample": artifact_files[:20],
        "image_artifacts": image_files,
        "conclusion": (
            "Existing proof routes provide JSON oracles and stale raw device paths, "
            "but no reusable detached image files are stored in the repo."
        ),
    }


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "hdiutil_available": shutil.which("hdiutil") is not None,
    }


def main() -> int:
    write_json("environment.json", environment())
    write_json("artifact-inventory.json", inventory_existing_artifacts())

    positive = run_positive_fixture()
    write_json("generated-proof-fixture.json", positive)

    synthetic_cases = [
        {"case_id": "valid-single-map", "expected": "validated_checkpoint_context"},
        {
            "case_id": "missing-checkpoint-map-last",
            "expected": "malformed_checkpoint_map",
            "missing_last": True,
            "descriptor_blocks": 1,
        },
        {"case_id": "invalid-cpm-count", "expected": "malformed_checkpoint_map", "map_count": 102},
        {"case_id": "invalid-mapped-size-zero", "expected": "bad_ephemeral_object", "mapped_size": 0},
        {"case_id": "invalid-mapped-size-unaligned", "expected": "bad_ephemeral_object", "mapped_size": 128},
        {"case_id": "bad-ephemeral-checksum", "expected": "bad_ephemeral_object", "bad_ephemeral_checksum": True},
        {
            "case_id": "non-contiguous-descriptor-layout",
            "expected": "unsupported_non_contiguous_descriptors",
            "non_contiguous": True,
        },
    ]
    synthetic_results = [run_synthetic_case(case) for case in synthetic_cases]
    write_json("synthetic-malformed-cases.json", {"cases": synthetic_results})

    summary = {
        "status": "executed",
        "positive_verdict": positive["verdict"],
        "positive_source": "generated-proof-fixture",
        "checkpoint_map_count": len(positive["checkpoint_maps"]),
        "mapped_ephemeral_object_count": len(positive["ephemeral_objects"]),
        "synthetic_case_count": len(synthetic_results),
        "synthetic_all_matched": all(case["matched_expectation"] for case in synthetic_results),
        "validated_checkpoint_context_available": positive["verdict"] == "validated_checkpoint_context",
        "next_gate": "EX-12 OMAP lookup validation may start from this validated checkpoint context",
    }
    write_json("summary.json", summary)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
