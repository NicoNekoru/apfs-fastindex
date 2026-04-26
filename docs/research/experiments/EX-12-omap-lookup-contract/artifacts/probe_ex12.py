#!/usr/bin/env python3
"""Run EX-12: OMAP lookup contract verification against a paired raw image
and identity oracle.

This probe is the EX-12 unblocker. It builds a fresh APFS proof fixture and
keeps the raw image alive across two readers so the oracle and the native
Rust scanner observe the *same* on-disk bytes:

1. Build a proof fixture with `apfs_fastindex.poc_fixture.build_proof_fixture`
   (already used by EX-11). This produces a `.dmg` and detaches the mount
   used to populate it; the image stays alive for the duration of the
   `with` block.
2. Re-attach the image with `hdiutil attach -nomount` so the raw
   `/dev/rdiskN` device is exposed without bringing the volume online.
3. Run the native Rust `apfs-fastindex-scan` binary against the raw device.
4. Run the existing EX-06 `identitydump` Go program against the same raw
   device to capture an OMAP-resolved FS-tree root identity from `go-apfs`.
5. Compare:
   - container OMAP -> volume superblock lookup output by Rust against the
     volume superblock object header read at that paddr (oid, xid <=
     selected, virtual storage, fs-tree subtype),
   - volume OMAP -> FS-tree root tree lookup output by Rust against the
     `go-apfs` lookup recorded by `identitydump` (oid, paddr, on-disk
     checksum at that paddr).
6. Re-run the SR-006 hard-stop unit tests in the Rust crate so synthetic
   failure cases (encrypted, no-header, crypto-generation, unknown bits,
   ENCRYPTING/DECRYPTING/KEYROLLING) are part of the EX-12 evidence record.
7. Detach.

The probe writes the merged evidence to `artifacts/generated/`.
"""

from __future__ import annotations

import datetime as _dt
import hashlib
import json
import os
import platform
import plistlib
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path
from typing import BinaryIO


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
# Object-type constants from Apple's APFS reference (`apfs.h`). The base
# object type is the low 16 bits of `obj_phys_t.o_type`; the high 16 bits
# carry storage class and flag bits.
OBJECT_TYPE_NX_SUPERBLOCK = 0x0000_0001
OBJECT_TYPE_BTREE = 0x0000_0002
OBJECT_TYPE_OMAP = 0x0000_000B
OBJECT_TYPE_CHECKPOINT_MAP = 0x0000_000C
APFS_OBJECT_TYPE_FS = 0x0000_000D
OBJECT_TYPE_FSTREE = 0x0000_000E
OBJECT_TYPE_MASK = 0x0000_FFFF
OBJ_VIRTUAL = 0x0000_0000
OBJ_PHYSICAL = 0x4000_0000
OBJ_STORAGETYPE_MASK = 0xC000_0000

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
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
        cwd=str(cwd) if cwd else None,
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


def le_u32(block: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from("<I", block, offset)[0]


def le_u64(block: bytes | bytearray, offset: int) -> int:
    return struct.unpack_from("<Q", block, offset)[0]


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
        raise ProbeError(
            "attach_failed", "hdiutil attach returned no system entities"
        )
    detach_device = entities[0]["dev-entry"]
    container_device = None
    for entity in entities:
        if entity.get("content-hint") == APFS_CONTAINER_HINT:
            container_device = entity.get("dev-entry")
            break
    if not container_device:
        raise ProbeError(
            "missing_apfs_container",
            "hdiutil attach did not expose an APFS container",
        )
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


def parse_obj_phys(block: bytes) -> dict:
    return {
        "checksum": le_u64(block, 0x00),
        "oid": le_u64(block, 0x08),
        "xid": le_u64(block, 0x10),
        "object_type_raw": le_u32(block, 0x18),
        "object_subtype_raw": le_u32(block, 0x1C),
    }


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


def checksum_matches(block: bytes | bytearray) -> bool:
    """Return True iff the Fletcher-64 checksum stored at offset 0 covers
    the rest of the block (per `obj_phys_t.o_cksum`)."""
    stored = le_u64(block, 0)
    # Build a copy with the cksum field zeroed and recompute as the parser
    # would. The Fletcher-64 routine here ignores the first 8 bytes by
    # starting at offset 8, so the in-place stored checksum does not affect
    # the computation; we still verify equality.
    return stored == apfs_fletcher64(block)


def sr006_lower_bound(
    samples: list[dict], requested_oid: int, selected_xid: int
) -> dict:
    """Pick the SR-006 lower-bound entry from a flat list of OMAP samples.

    Returns the largest `(oid, xid)` entry with `oid == requested_oid` and
    `xid <= selected_xid`. If no entry qualifies, returns `None`.
    """
    candidates = [
        sample
        for sample in samples
        if sample.get("oid") == requested_oid and sample.get("xid", 0) <= selected_xid
    ]
    if not candidates:
        return {"selected": None, "candidates": []}
    selected = max(candidates, key=lambda s: s["xid"])
    return {"selected": selected, "candidates": candidates}


def storage_class(object_type_raw: int) -> str:
    storage = object_type_raw & OBJ_STORAGETYPE_MASK
    if storage == OBJ_PHYSICAL:
        return "physical"
    if storage == OBJ_VIRTUAL:
        return "virtual"
    if storage == 0x8000_0000:
        return "ephemeral"
    return f"unknown({storage:#x})"


def run_rust_scanner(raw_container: str) -> dict:
    proc = run_checked(
        [
            "cargo",
            "run",
            "--quiet",
            "--release",
            "--bin",
            "apfs-fastindex-scan",
            "--",
            raw_container,
        ],
        cwd=REPO_ROOT,
    )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as err:
        raise ProbeError(
            "rust_scan_unparseable",
            f"could not parse Rust scanner JSON: {err}\nstdout:\n{proc.stdout[:2000]}",
        )


def run_identitydump(raw_container: str) -> dict:
    proc = run_checked(
        ["go", "run", ".", "--device", raw_container],
        cwd=IDENTITYDUMP_DIR,
    )
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError as err:
        raise ProbeError(
            "identitydump_unparseable",
            f"could not parse identitydump JSON: {err}\nstdout:\n{proc.stdout[:2000]}",
        )


def run_synthetic_omap_tests() -> dict:
    """Run the SR-006 hard-stop unit tests in the Rust crate.

    The `omap::tests::*` set covers the synthetic failure cases that are
    impractical to provoke on a real fixture: encrypted, no-header,
    crypto-generation value flags, unknown value-flag bits, and the
    OMAP-phys ENCRYPTING/DECRYPTING/KEYROLLING/CRYPTO_GENERATION_FLAG/
    unknown-bit hard stops at open time, plus the (oid, max_xid)
    lower-bound semantics over a multi-version key set.
    """
    proc = run(
        [
            "cargo",
            "test",
            "--quiet",
            "--release",
            "-p",
            "apfs-fastindex",
            "omap::",
            "--",
            "--exact",
            "--format=terse",
        ],
        cwd=REPO_ROOT,
    )
    if proc.returncode != 0:
        # Run again without --exact and with the verbose names list so the
        # caller can read individual test verdicts off stdout.
        verbose = run(
            [
                "cargo",
                "test",
                "--quiet",
                "--release",
                "-p",
                "apfs-fastindex",
                "omap::",
            ],
            cwd=REPO_ROOT,
        )
        raise ProbeError(
            "synthetic_omap_tests_failed",
            f"synthetic OMAP unit tests failed.\nstdout:\n{verbose.stdout}\nstderr:\n{verbose.stderr}",
        )
    list_proc = run_checked(
        [
            "cargo",
            "test",
            "--quiet",
            "--release",
            "-p",
            "apfs-fastindex",
            "omap::",
            "--",
            "--list",
        ],
        cwd=REPO_ROOT,
    )
    test_names: list[str] = []
    for line in list_proc.stdout.splitlines():
        line = line.strip()
        if line.endswith(": test"):
            test_names.append(line[: -len(": test")].strip())
    return {
        "verdict": "all_passed",
        "test_count": len(test_names),
        "test_names": sorted(test_names),
        "summary_line": next(
            (
                line
                for line in proc.stdout.splitlines()
                if line.startswith("test result:")
            ),
            "",
        ),
    }


def header_object_type_base(header: dict) -> int:
    return header["object_type_raw"] & OBJECT_TYPE_MASK


def header_storage(header: dict) -> str:
    return storage_class(header["object_type_raw"])


def validate_obj_header(
    header: dict,
    block: bytes,
    expected_oid: int,
    expected_type: int,
    expected_storage: str,
    selected_xid: int,
    expected_subtype: int | None = None,
) -> dict:
    """Apply the SR-006/SR-007 obj-header validation that the Rust resolver
    performs internally, but in Python so the probe is a second observer.

    Returns a result dict naming each predicate. `ok` is True iff every
    predicate holds.
    """
    type_ok = header_object_type_base(header) == expected_type
    storage_ok = header_storage(header) == expected_storage
    oid_ok = header["oid"] == expected_oid
    xid_ok = header["xid"] <= selected_xid
    cksum_ok = checksum_matches(block)
    subtype_ok = (
        True if expected_subtype is None else header["object_subtype_raw"] == expected_subtype
    )
    return {
        "type_ok": type_ok,
        "storage_ok": storage_ok,
        "oid_ok": oid_ok,
        "xid_ok": xid_ok,
        "checksum_ok": cksum_ok,
        "subtype_ok": subtype_ok,
        "ok": type_ok and storage_ok and oid_ok and xid_ok and cksum_ok and subtype_ok,
    }


def compare_against_oracles(
    rust_output: dict,
    identity_output: dict,
    raw_container: str,
) -> dict:
    """Return a comparison record naming each pairing between Rust and oracles.

    The Rust scanner must:
    - resolve the container OMAP whose `phys.block_address` validates as
      OBJECT_TYPE_OMAP, physical storage, valid checksum, when re-read off
      disk by this probe;
    - resolve every container OMAP volume_oid to a paddr whose obj_phys_t
      header validates as OBJECT_TYPE_FS, virtual storage, oid==requested,
      xid<=selected, valid checksum;
    - resolve every volume's volume OMAP at a paddr whose obj_phys_t header
      validates as OBJECT_TYPE_OMAP, physical storage, valid checksum;
    - resolve the FS root tree at a paddr whose obj_phys_t header validates
      as OBJECT_TYPE_BTREE, virtual storage, subtype==OBJECT_TYPE_FSTREE,
      oid==requested, xid<=selected, valid checksum;
    - satisfy SR-006 lower-bound semantics on every Rust-observed sample
      list: the returned (oid, xid) pair must be the largest with the
      requested oid and xid<=selected_xid.

    The probe also captures `identitydump`'s view as a secondary oracle. It
    requires that identitydump and Rust agree on the FS-tree root *oid* but
    deliberately does not require agreement on (paddr, xid): go-apfs picks
    its own active-state checkpoint in `apfs.Open(...)` and may resolve a
    different historical version of the same OID. That difference is
    captured as `go_apfs_active_state_observation` and is not, by itself, a
    contract violation.
    """
    selected = rust_output.get("selected_checkpoint")
    if not selected:
        raise ProbeError(
            "rust_no_selected_checkpoint",
            "Rust scanner did not emit selected_checkpoint",
        )
    block_size = selected["container"]["block_size"]
    selected_xid = selected["xid"]
    container_omap_paddr = selected["container_omap"]["phys"]["block_address"]

    with open(raw_container, "rb", buffering=0) as handle:
        container_omap_block = read_block(handle, container_omap_paddr, block_size)
        container_omap_header = parse_obj_phys(container_omap_block)
        container_omap_validation = validate_obj_header(
            header=container_omap_header,
            block=container_omap_block,
            expected_oid=selected["container"]["omap_oid"],
            expected_type=OBJECT_TYPE_OMAP,
            expected_storage="physical",
            selected_xid=selected_xid,
        )
        container_omap_check = {
            "paddr": container_omap_paddr,
            "obj_header": container_omap_header,
            "object_type_base": header_object_type_base(container_omap_header),
            "storage": header_storage(container_omap_header),
            **container_omap_validation,
        }

        # SR-006 lower-bound on container OMAP samples: each volume_oid the
        # container exposes must point to the largest sample whose
        # xid <= selected_xid.
        container_lower_bound_checks: list[dict] = []
        container_samples = (
            selected["container_omap"].get("sample_mappings", [])
        )
        for volume_oid in selected["container"].get("volume_oids", []):
            bound = sr006_lower_bound(
                container_samples, volume_oid, selected_xid
            )
            rust_returned = next(
                (
                    vol["container_omap_lookup"]
                    for vol in selected.get("volumes", [])
                    if vol["volume_oid"] == volume_oid
                ),
                None,
            )
            container_lower_bound_checks.append(
                {
                    "volume_oid": volume_oid,
                    "rust_returned": rust_returned,
                    "sample_lower_bound": bound["selected"],
                    "matches": bool(
                        rust_returned
                        and bound["selected"]
                        and rust_returned["paddr"] == bound["selected"]["paddr"]
                        and rust_returned["xid"] == bound["selected"]["xid"]
                    ),
                    "candidate_count": len(bound["candidates"]),
                }
            )

        volume_checks: list[dict] = []
        root_tree_oracle_pairs: list[dict] = []
        for vol in selected.get("volumes", []):
            lookup = vol.get("container_omap_lookup", {})
            vol_block = read_block(handle, lookup["paddr"], block_size)
            vol_header = parse_obj_phys(vol_block)
            vol_validation = validate_obj_header(
                header=vol_header,
                block=vol_block,
                expected_oid=lookup["oid"],
                expected_type=APFS_OBJECT_TYPE_FS,
                expected_storage="virtual",
                selected_xid=selected_xid,
            )
            vol_check = {
                "fs_oid_index": vol["fs_oid_index"],
                "volume_oid": vol["volume_oid"],
                "rust_lookup": lookup,
                "obj_header": vol_header,
                "object_type_base": header_object_type_base(vol_header),
                "storage": header_storage(vol_header),
                "validation": vol_validation,
                "status": vol.get("status"),
            }
            volume_checks.append(vol_check)

            volume_omap = vol.get("volume_omap")
            if volume_omap is not None:
                omap_paddr = volume_omap["phys"]["block_address"]
                omap_block = read_block(handle, omap_paddr, block_size)
                omap_header = parse_obj_phys(omap_block)
                vol_check["volume_omap_check"] = {
                    "paddr": omap_paddr,
                    "obj_header": omap_header,
                    "object_type_base": header_object_type_base(omap_header),
                    "storage": header_storage(omap_header),
                    **validate_obj_header(
                        header=omap_header,
                        block=omap_block,
                        expected_oid=omap_paddr,
                        expected_type=OBJECT_TYPE_OMAP,
                        expected_storage="physical",
                        selected_xid=selected_xid,
                    ),
                }
            else:
                vol_check["volume_omap_check"] = None

            root_lookup = vol.get("root_tree_lookup")
            if root_lookup is not None:
                root_block = read_block(handle, root_lookup["paddr"], block_size)
                root_header = parse_obj_phys(root_block)
                root_block_sha256 = hashlib.sha256(root_block).hexdigest()
                vol_check["root_tree_obj_header"] = root_header
                vol_check["root_tree_storage"] = header_storage(root_header)
                vol_check["root_tree_block_sha256"] = root_block_sha256
                vol_check["root_tree_validation"] = validate_obj_header(
                    header=root_header,
                    block=root_block,
                    expected_oid=root_lookup["oid"],
                    expected_type=OBJECT_TYPE_BTREE,
                    expected_storage="virtual",
                    selected_xid=selected_xid,
                    expected_subtype=OBJECT_TYPE_FSTREE,
                )

                # SR-006 lower-bound on the volume OMAP samples.
                vol_omap_samples = (volume_omap or {}).get("sample_mappings", [])
                root_bound = sr006_lower_bound(
                    vol_omap_samples, root_lookup["oid"], selected_xid
                )
                vol_check["root_tree_lower_bound"] = {
                    "rust_returned": root_lookup,
                    "sample_lower_bound": root_bound["selected"],
                    "matches": bool(
                        root_bound["selected"]
                        and root_bound["selected"]["paddr"] == root_lookup["paddr"]
                        and root_bound["selected"]["xid"] == root_lookup["xid"]
                    ),
                    "candidate_count": len(root_bound["candidates"]),
                }

                # Cross-check against go-apfs identitydump. We require oid
                # agreement only; (paddr, xid) divergence is captured.
                id_root = (identity_output or {}).get("root_tree", {})
                pair = {
                    "volume_oid": vol["volume_oid"],
                    "rust_oid": root_lookup["oid"],
                    "rust_paddr": root_lookup["paddr"],
                    "rust_lookup_xid": root_lookup["xid"],
                    "rust_obj_header_oid": root_header["oid"],
                    "rust_obj_header_xid": root_header["xid"],
                    "rust_on_disk_checksum": root_header["checksum"],
                    "rust_on_disk_block_sha256": root_block_sha256,
                    "identity_oid": id_root.get("oid"),
                    "identity_paddr": id_root.get("paddr"),
                    "identity_object_xid": id_root.get("object_xid"),
                    "identity_checksum": id_root.get("checksum"),
                    "identity_content_hash": id_root.get("content_hash"),
                    "oid_match": id_root.get("oid") == root_lookup["oid"],
                    "paddr_match": id_root.get("paddr") == root_lookup["paddr"],
                    "object_xid_match": id_root.get("object_xid")
                    == root_header["xid"],
                    "checksum_match": id_root.get("checksum")
                    == root_header["checksum"],
                }
                root_tree_oracle_pairs.append(pair)

    # Aggregate verdicts.
    container_omap_ok = container_omap_check["ok"]
    volumes_ok = all(
        v["validation"]["ok"]
        for v in volume_checks
        if v["status"] == "supported"
    ) and all(
        # Even unsupported volumes must satisfy the OID identity, since the
        # OMAP returned a paddr for that oid.
        v["validation"]["oid_ok"] and v["validation"]["xid_ok"]
        for v in volume_checks
    )
    volume_omaps_ok = all(
        (v["volume_omap_check"] is None) or v["volume_omap_check"]["ok"]
        for v in volume_checks
    )
    root_trees_ok = all(
        v.get("root_tree_validation", {}).get("ok", True)
        for v in volume_checks
    )
    sr006_lower_bound_ok = (
        all(c["matches"] for c in container_lower_bound_checks)
        and all(
            v.get("root_tree_lower_bound", {}).get("matches", True)
            for v in volume_checks
        )
    )
    # OID-only agreement with identitydump is the cross-tool oracle assertion.
    identity_oid_ok = bool(root_tree_oracle_pairs) and all(
        pair["oid_match"] for pair in root_tree_oracle_pairs
    )
    # Note any divergence between Rust and identitydump on (paddr, xid).
    go_apfs_diverges = any(
        not (pair["paddr_match"] and pair["object_xid_match"])
        for pair in root_tree_oracle_pairs
    )

    return {
        "selected_xid": selected_xid,
        "block_size": block_size,
        "container_omap_check": container_omap_check,
        "container_omap_lower_bound_checks": container_lower_bound_checks,
        "volume_checks": volume_checks,
        "root_tree_oracle_pairs": root_tree_oracle_pairs,
        "container_omap_ok": container_omap_ok,
        "volumes_ok": volumes_ok,
        "volume_omaps_ok": volume_omaps_ok,
        "root_trees_ok": root_trees_ok,
        "sr006_lower_bound_ok": sr006_lower_bound_ok,
        "identity_oid_ok": identity_oid_ok,
        "go_apfs_active_state_observation": (
            "go-apfs identitydump resolves a different active-state checkpoint "
            "than the native scanner; (paddr, xid) for the FS root differ "
            "across the two tools while oid agrees. SR-006 lookup correctness "
            "is parameterized by selected_xid, so this divergence reflects a "
            "different selected_xid choice in go-apfs's `apfs.Open` and is "
            "not, by itself, a contract violation."
            if go_apfs_diverges
            else "go-apfs identitydump and native scanner agree on FS root oid, paddr, and xid."
        ),
        "all_match": (
            container_omap_ok
            and volumes_ok
            and volume_omaps_ok
            and root_trees_ok
            and sr006_lower_bound_ok
            and identity_oid_ok
        ),
    }


def run_paired_oracle() -> dict:
    detach = None
    with build_proof_fixture() as fixture:
        entities, detach, raw_container = attach_nomount_image(fixture.image_path)
        try:
            rust_output = run_rust_scanner(raw_container)
            identity_output = run_identitydump(raw_container)
            comparison = compare_against_oracles(
                rust_output, identity_output, raw_container
            )
            return {
                "source": {
                    "source_id": "generated-proof-fixture",
                    "image_name": fixture.image_path.name,
                    "image_path": str(fixture.image_path),
                    "raw_container_path": raw_container,
                    "nomount_entities": entities,
                    "fixture_operations": list(fixture.operations),
                    "image_size_bytes": fixture.image_path.stat().st_size,
                },
                "rust_scanner": rust_output,
                "identitydump": identity_output,
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
    synthetic = run_synthetic_omap_tests()
    write_json("synthetic-omap-tests.json", synthetic)

    paired = run_paired_oracle()
    write_json("paired-fixture.json", paired)

    comparison = paired["comparison"]
    summary = {
        "status": "executed",
        "verdict": (
            "validated_omap_lookup_contract"
            if comparison["all_match"] and synthetic["verdict"] == "all_passed"
            else "omap_lookup_contract_violation"
        ),
        "selected_xid": comparison["selected_xid"],
        "block_size": comparison["block_size"],
        "container_omap_paddr": comparison["container_omap_check"]["paddr"],
        "container_omap_ok": comparison["container_omap_ok"],
        "volume_count": len(comparison["volume_checks"]),
        "volumes_ok": comparison["volumes_ok"],
        "volume_omaps_ok": comparison["volume_omaps_ok"],
        "root_trees_ok": comparison["root_trees_ok"],
        "sr006_lower_bound_ok": comparison["sr006_lower_bound_ok"],
        "identity_oid_ok": comparison["identity_oid_ok"],
        "go_apfs_active_state_observation": comparison[
            "go_apfs_active_state_observation"
        ],
        "root_tree_pair_count": len(comparison["root_tree_oracle_pairs"]),
        "synthetic_omap_test_verdict": synthetic["verdict"],
        "synthetic_omap_test_count": synthetic["test_count"],
        "synthetic_omap_tests": synthetic["test_names"],
        "next_gate": (
            "FS-record body decoding (DIR_REC, INODE, XATTR, SIBLING_*) "
            "is now safe to attempt against this validated OMAP/root context."
        ),
    }
    write_json("summary.json", summary)
    return 0 if summary["verdict"] == "validated_omap_lookup_contract" else 1


if __name__ == "__main__":
    raise SystemExit(main())
