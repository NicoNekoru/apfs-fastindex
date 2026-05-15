#!/usr/bin/env python3
"""EX-16: replay EX-13 raw-byte FS-record body decoding under the SR-015
single-cursor xfield rule.

Uses the EX-13 helpers for FS-tree walking and record-body decoding, but
replaces the candidate-scoring xfield parser with one cursor rule per SR-015:

  values start immediately after `xf_blob_t` + xfield metadata table, and
  each value occupies `round_up(x_size, 8)` bytes.

Captures per-record structural metrics (`xf_num_exts`, `xf_used_data`,
metadata-table length, padded value lengths, unused trailing bytes) and
asserts both `xf_used_data == sum(round_up(x_size, 8))` and the same
namespace + logical-size oracle parity EX-13 enforced.
"""

from __future__ import annotations

import datetime as _dt
import importlib.util
import json
import os
import platform
import plistlib
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
REPO_ROOT = ARTIFACT_DIR.parents[4]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"
EX13_PROBE = (
    REPO_ROOT
    / "docs"
    / "research"
    / "experiments"
    / "EX-13-native-fs-record-body-oracle"
    / "artifacts"
    / "probe_ex13.py"
)

APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
INO_EXT_TYPE_NAME = 4
INO_EXT_TYPE_DSTREAM = 8
INO_EXT_TYPE_SPARSE_BYTES = 13
DREC_EXT_TYPE_SIBLING_ID = 1


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


def load_ex13() -> Any:
    spec = importlib.util.spec_from_file_location("probe_ex13", EX13_PROBE)
    if spec is None or spec.loader is None:
        raise ProbeError("not_executed", f"unable to load {EX13_PROBE}")
    module = importlib.util.module_from_spec(spec)
    sys.modules["probe_ex13"] = module
    spec.loader.exec_module(module)
    return module


EX13 = load_ex13()


# ---- SR-015 single-cursor xfield decoder --------------------------------- #

def round_up_8(value: int) -> int:
    return (value + 7) & ~7


def parse_xfields_sr015(value: bytes, record_kind: str) -> dict:
    """Decode an xfield blob using SR-015's single cursor rule.

    Returns:
      {
        "present": bool,                   # whether xf_blob_t header present
        "xf_num_exts": int,
        "xf_used_data": int,
        "metadata_length": int,            # 4 + xf_num_exts * 4
        "fields": [
          {"x_type": int, "x_flags": int, "x_size": int, "padded_length": int,
           "value_hex": str, "interpreted": {...}}
        ],
        "padded_values_total": int,        # sum of round_up(x_size, 8)
        "xf_used_data_matches": bool,      # equals padded_values_total
        "unused_trailing_bytes": int,      # len(blob) - 4 - metadata - padded_values_total
        "decode_error": Optional[str],
      }
    """
    if not value:
        return {
            "present": False,
            "xf_num_exts": 0,
            "xf_used_data": 0,
            "metadata_length": 0,
            "fields": [],
            "padded_values_total": 0,
            "xf_used_data_matches": True,
            "unused_trailing_bytes": 0,
            "decode_error": None,
        }
    if len(value) < 4:
        return {
            "present": True,
            "decode_error": "xfield blob shorter than 4-byte xf_blob_t header",
            "xf_num_exts": 0,
            "xf_used_data": 0,
            "metadata_length": 0,
            "fields": [],
            "padded_values_total": 0,
            "xf_used_data_matches": False,
            "unused_trailing_bytes": 0,
        }
    xf_num_exts = EX13.le_u16(value, 0)
    xf_used_data = EX13.le_u16(value, 2)
    metadata_length = 4 + xf_num_exts * 4
    if metadata_length > len(value):
        return {
            "present": True,
            "decode_error": f"metadata table ({metadata_length} bytes) exceeds blob length ({len(value)})",
            "xf_num_exts": xf_num_exts,
            "xf_used_data": xf_used_data,
            "metadata_length": metadata_length,
            "fields": [],
            "padded_values_total": 0,
            "xf_used_data_matches": False,
            "unused_trailing_bytes": 0,
        }
    metadata: list[tuple[int, int, int]] = []
    for index in range(xf_num_exts):
        offset = 4 + index * 4
        metadata.append(
            (
                value[offset],
                value[offset + 1],
                EX13.le_u16(value, offset + 2),
            )
        )

    cursor = metadata_length
    fields: list[dict] = []
    padded_total = 0
    decode_error: str | None = None
    for x_type, x_flags, x_size in metadata:
        end_value = cursor + x_size
        if end_value > len(value):
            decode_error = (
                f"value at cursor {cursor} size {x_size} exceeds blob length {len(value)}"
            )
            break
        data = value[cursor:end_value]
        padded_length = round_up_8(x_size)
        interpreted = EX13.interpret_xfield(x_type, data)
        fields.append(
            {
                "x_type": x_type,
                "x_flags": x_flags,
                "x_size": x_size,
                "padded_length": padded_length,
                "value_hex": data.hex(),
                "interpreted": interpreted,
            }
        )
        padded_total += padded_length
        if cursor + padded_length > len(value) and (x_type, x_flags, x_size) != metadata[-1]:
            decode_error = (
                f"padded value cursor {cursor + padded_length} exceeds blob length {len(value)} "
                f"with more fields to read"
            )
            break
        cursor += padded_length

    used_matches = xf_used_data == padded_total
    unused_trailing = len(value) - metadata_length - padded_total
    return {
        "present": True,
        "record_kind": record_kind,
        "xf_num_exts": xf_num_exts,
        "xf_used_data": xf_used_data,
        "metadata_length": metadata_length,
        "fields": fields,
        "padded_values_total": padded_total,
        "xf_used_data_matches": used_matches,
        "unused_trailing_bytes": unused_trailing,
        "decode_error": decode_error,
    }


# ---- record decoding under SR-015 ---------------------------------------- #

INODE_FIXED_SIZE = 0x5C
DREC_PREFIX_SIZE = 18
OBJ_ID_MASK = (1 << 60) - 1
OBJ_TYPE_SHIFT = 60


def walk_fs_tree_with_raw_bytes(handle: Any, root_paddr: int, block_size: int) -> list[dict]:
    """Walk the FS-tree like EX-13 but capture raw value bytes alongside the
    decoded record so the SR-015 cursor rule can replay xfields from raw
    bytes."""
    records: list[dict] = []

    def walk(paddr: int, is_root: bool) -> None:
        block = EX13.read_block(handle, paddr, block_size)
        node = EX13.parse_btree_node(block, block_size)
        for index in range(node["nkeys"]):
            key, value = EX13.node_entry(block, node, index)
            if node["is_leaf"]:
                if len(key) < 8:
                    continue
                key_word = EX13.le_u64(key, 0)
                object_id = key_word & OBJ_ID_MASK
                raw_type = key_word >> OBJ_TYPE_SHIFT
                family = EX13.family_name(raw_type)
                record = EX13.parse_record(paddr, index, key, value)
                record["raw_value_bytes"] = value
                record["raw_key_bytes"] = key
                records.append(record)
                # raw_type unused beyond consistency checks
                _ = object_id, family
            else:
                if len(value) < 8:
                    raise ProbeError(
                        "malformed_record_body",
                        f"internal FS-tree value at {paddr}/{index} shorter than child paddr",
                    )
                walk(EX13.le_u64(value, 0), False)

    walk(root_paddr, True)
    return records


def replay_record(record: dict) -> dict:
    family = record["family"]
    value_bytes: bytes = record.get("raw_value_bytes") or b""
    if family == "inode":
        if len(value_bytes) <= INODE_FIXED_SIZE:
            blob = b""
        else:
            blob = value_bytes[INODE_FIXED_SIZE:]
        return parse_xfields_sr015(blob, "inode")
    if family == "dir_rec":
        if len(value_bytes) <= DREC_PREFIX_SIZE:
            blob = b""
        else:
            blob = value_bytes[DREC_PREFIX_SIZE:]
        return parse_xfields_sr015(blob, "dir_rec")
    return {"present": False, "skipped": f"family {family} has no xfields under SR-014"}


# ---- driver glue --------------------------------------------------------- #

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


def run_rust_context(raw_container: str) -> dict:
    proc = run_checked(
        ["cargo", "run", "--quiet", "--bin", "apfs-fastindex-scan", "--", raw_container],
        cwd=RUST_CRATE_DIR,
    )
    return json.loads(proc.stdout)


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
        "go": shutil.which("go"),
        "fsck_apfs": shutil.which("fsck_apfs"),
    }


# ---- xfield-driven namespace + logical-size oracle comparison ----------- #

def expected_names_by_inode(records: list[dict]) -> dict[int, set[str]]:
    names: dict[int, set[str]] = {}
    for record in records:
        if record["family"] == "dir_rec":
            name = (record["key"].get("name") or "").rstrip("\x00")
            file_id = record["value"].get("file_id")
            if name and file_id is not None:
                names.setdefault(file_id, set()).add(name)
        elif record["family"] == "sibling_link":
            name = (record["value"].get("name") or "").rstrip("\x00")
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
    out: dict[int, int] = {}
    for record in records:
        if record["family"] == "sibling_map":
            out[record["object_id"]] = record["value"].get("file_id")
    return out


def assert_xfield_oracle(record: dict, replay: dict, names_by_inode, sizes_by_inode, sparse_ids, sibling_id_to_file_id):
    """Return list of oracle failures for this record (empty list = pass)."""
    failures: list[str] = []
    if replay.get("decode_error"):
        failures.append(f"decode error: {replay['decode_error']}")
        return failures
    if replay.get("present") and not replay.get("xf_used_data_matches", True):
        failures.append(
            f"xf_used_data {replay['xf_used_data']} != padded_values_total {replay['padded_values_total']}"
        )
    for field in replay.get("fields", []) or []:
        x_type = field["x_type"]
        interpreted = field.get("interpreted") or {}
        if x_type == INO_EXT_TYPE_NAME and record["family"] == "inode":
            value = (interpreted.get("value") or "").rstrip("\x00")
            expected = names_by_inode.get(record["object_id"])
            if expected and value not in expected:
                failures.append(
                    f"INO_EXT_TYPE_NAME {value!r} not in raw names {sorted(expected)!r}"
                )
        elif x_type == INO_EXT_TYPE_DSTREAM and record["family"] == "inode":
            dstream = interpreted.get("value") or {}
            size = dstream.get("size")
            expected = sizes_by_inode.get(record["object_id"])
            if expected and size not in expected:
                failures.append(
                    f"INO_EXT_TYPE_DSTREAM.size {size!r} not in mounted sizes {sorted(expected)!r}"
                )
        elif x_type == INO_EXT_TYPE_SPARSE_BYTES and record["family"] == "inode":
            value = interpreted.get("value")
            expected = sizes_by_inode.get(record["object_id"]) or set()
            if record["object_id"] in sparse_ids and expected:
                max_size = max(expected)
                if not (isinstance(value, int) and 0 <= value <= max_size):
                    failures.append(
                        f"INO_EXT_TYPE_SPARSE_BYTES {value!r} outside logical size {max_size}"
                    )
        elif x_type == DREC_EXT_TYPE_SIBLING_ID and record["family"] == "dir_rec":
            value = interpreted.get("value")
            file_id = record["value"].get("file_id")
            mapping = sibling_id_to_file_id.get(value) if isinstance(value, int) else None
            if mapping is not None and mapping != file_id:
                failures.append(
                    f"DREC_EXT_TYPE_SIBLING_ID {value} -> file_id {mapping} != drec.file_id {file_id}"
                )
    return failures


def main() -> int:
    write_json("environment.json", environment())
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex16-", dir="/tmp"))
    image_path = base / "EX16CI.dmg"
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
        # Reuse the EX-13 proof fixture builder by inlining its operations
        # here so we don't have to import build_proof_fixture's contextmanager
        # (which manages its own image lifecycle).
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "160m",
                "-fs",
                "APFS",
                "-volname",
                "EX16CI",
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = _build_ex13_fixture(mountpoint)
        mounted_entries = _snapshot_tree(mountpoint)
        write_json("ex16-fixture-operations.json", {"operations": operations})
        write_json(
            "ex16-mounted-posix-oracle.json",
            {
                "volume_label": "EX16CI",
                "entries": mounted_entries,
            },
        )
        detach_device(mounted_detach)
        mounted_detach = None
        time.sleep(0.4)

        _, nomount_detach, raw_container = attach_nomount(image_path)

        rust_context = run_rust_context(raw_container)
        write_json("ex16-rust-context.json", rust_context)
        selected = rust_context.get("selected_checkpoint")
        if not selected:
            raise ProbeError(
                "oracle_inconclusive",
                "Rust did not return selected_checkpoint; EX-15 gate must be revisited",
            )

        block_size = selected["container"]["block_size"]
        volume = selected["volumes"][0]
        root_paddr = volume["root_tree_lookup"]["paddr"]
        with open(raw_container, "rb", buffering=0) as handle:
            records = walk_fs_tree_with_raw_bytes(handle, root_paddr, block_size)

        # Override inode/dir_rec xfield-derived fields using ONLY the SR-015
        # decoder. Anything in the namespace + logical-size oracle that depends
        # on xfields now flows from the SR-015 cursor rule alone.
        for record in records:
            replay = replay_record(record)
            record["sr015_xfield_replay"] = replay
            if record["family"] == "inode":
                fields = replay.get("fields", []) or []
                dstream = next(
                    (
                        f["interpreted"]["value"]
                        for f in fields
                        if f.get("interpreted", {}).get("kind") == "dstream"
                    ),
                    None,
                )
                sparse = next(
                    (
                        f["interpreted"].get("value")
                        for f in fields
                        if f["x_type"] == INO_EXT_TYPE_SPARSE_BYTES
                        and f.get("interpreted", {}).get("kind") == "u64"
                    ),
                    None,
                )
                inode_name = next(
                    (
                        f["interpreted"].get("value")
                        for f in fields
                        if f["x_type"] == INO_EXT_TYPE_NAME
                        and f.get("interpreted", {}).get("kind") == "utf8"
                    ),
                    None,
                )
                record["value"]["dstream"] = dstream
                record["value"]["sparse_bytes"] = sparse
                record["value"]["inode_name"] = inode_name
            elif record["family"] == "dir_rec":
                fields = replay.get("fields", []) or []
                sibling_id = next(
                    (
                        f["interpreted"].get("value")
                        for f in fields
                        if f["x_type"] == DREC_EXT_TYPE_SIBLING_ID
                        and f.get("interpreted", {}).get("kind") == "u64"
                    ),
                    None,
                )
                record["value"]["sibling_id"] = sibling_id

        names_by_inode = expected_names_by_inode(records)
        sizes_by_inode = expected_sizes_by_inode(mounted_entries)
        sparse_ids = sparse_inodes(mounted_entries)
        sibling_id_to_file_id = sibling_map(records)

        replays: list[dict] = []
        per_record_failures: list[dict] = []
        used_data_count_pass = 0
        used_data_count_fail = 0
        records_with_xfields = 0
        for record in records:
            replay = record.get("sr015_xfield_replay") or {}
            if replay.get("present") and replay.get("decode_error") is None and replay.get("fields"):
                records_with_xfields += 1
                if replay.get("xf_used_data_matches"):
                    used_data_count_pass += 1
                else:
                    used_data_count_fail += 1
            failures = assert_xfield_oracle(
                record, replay, names_by_inode, sizes_by_inode, sparse_ids, sibling_id_to_file_id
            )
            if failures:
                per_record_failures.append(
                    {
                        "object_id": record["object_id"],
                        "family": record["family"],
                        "node_paddr": record["node_paddr"],
                        "entry_index": record["entry_index"],
                        "failures": failures,
                        "replay": replay,
                    }
                )
            replays.append(
                {
                    "object_id": record["object_id"],
                    "family": record["family"],
                    "node_paddr": record["node_paddr"],
                    "entry_index": record["entry_index"],
                    "replay": replay,
                }
            )

        # Cross-check namespace + logical size against the mounted oracle.
        native_entries = EX13.reconstruct_entries(records)
        namespace_diff = _compare_namespace(mounted_entries, native_entries)

        comparison = {
            "namespace": namespace_diff,
            "xfield_used_data_pass": used_data_count_pass,
            "xfield_used_data_fail": used_data_count_fail,
            "records_with_xfields": records_with_xfields,
            "per_record_failures": per_record_failures,
            "matched": namespace_diff["matched"] and not per_record_failures,
        }
        write_json(
            "ex16-xfield-replay.json",
            {
                "selected_xid": selected["xid"],
                "block_size": block_size,
                "records_with_xfields": records_with_xfields,
                "xfield_used_data_pass": used_data_count_pass,
                "xfield_used_data_fail": used_data_count_fail,
                "replays": replays,
            },
        )
        write_json("ex16-comparison.json", comparison)

        if comparison["matched"]:
            verdict = "validated_sr_015_cursor_rule"
            detail = (
                f"All {records_with_xfields} records with xfields have "
                "xf_used_data == sum(round_up(x_size, 8)); namespace and "
                "logical-size oracle parity preserved."
            )
        else:
            verdict = "xfield_rule_insufficient"
            detail = (
                f"{used_data_count_fail} of {records_with_xfields} records failed "
                "xf_used_data equality, or oracle parity broke. See "
                "ex16-comparison.json#per_record_failures."
            )
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["records_with_xfields"] = records_with_xfields
        summary["xfield_used_data_pass"] = used_data_count_pass
        summary["xfield_used_data_fail"] = used_data_count_fail
        summary["namespace_matched"] = namespace_diff["matched"]
        summary["per_record_failure_count"] = len(per_record_failures)
        write_json("summary.json", summary)
        return 0 if verdict == "validated_sr_015_cursor_rule" else 1
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


def _build_ex13_fixture(root: Path) -> list[str]:
    """Mirror src/apfs_fastindex/poc_fixture.py operations so the resulting
    image matches the one EX-13 reasoned about for SR-015."""
    import fcntl
    F_FULLFSYNC = 51
    operations: list[str] = []

    def _sync_directory(path: Path) -> None:
        fd = os.open(path, os.O_RDONLY)
        try:
            os.fsync(fd)
        finally:
            os.close(fd)

    def _full_sync(path: Path) -> None:
        with path.open("ab") as handle:
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass

    def _settle() -> None:
        run(["sync"])
        time.sleep(0.15)

    src = root / "src"
    dst = root / "dst"
    src.mkdir()
    dst.mkdir()
    _sync_directory(root)
    _settle()
    operations.append("create src and dst directories")

    base_file = src / "base.txt"
    base_file.write_text("alpha\n")
    _full_sync(base_file)
    _sync_directory(src)
    _settle()
    operations.append("create src/base.txt")

    renamed = src / "renamed.txt"
    base_file.rename(renamed)
    _sync_directory(src)
    _settle()
    operations.append("rename src/base.txt -> src/renamed.txt")

    moved = dst / "moved.txt"
    renamed.rename(moved)
    _sync_directory(src)
    _sync_directory(dst)
    _settle()
    operations.append("move src/renamed.txt -> dst/moved.txt")

    hard_link = dst / "hard.txt"
    os.link(moved, hard_link)
    _sync_directory(dst)
    _settle()
    operations.append("create hard link dst/hard.txt")

    sparse = dst / "sparse.bin"
    with sparse.open("wb") as handle:
        handle.write(b"HEAD")
        handle.seek(1024 * 1024 - 4)
        handle.write(b"TAIL")
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass
    _sync_directory(dst)
    _settle()
    operations.append("create sparse file dst/sparse.bin")

    clone = dst / "clone.txt"
    proc = run(["cp", "-c", str(moved), str(clone)])
    if proc.returncode != 0:
        raise ProbeError("fixture_build", f"clone step failed:\n{proc.stderr}")
    _sync_directory(dst)
    _settle()
    operations.append("clone dst/moved.txt -> dst/clone.txt")

    with moved.open("a") as handle:
        handle.write("beta\n")
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass
    _sync_directory(dst)
    _settle()
    operations.append("append to dst/moved.txt")

    symlink = dst / "link.txt"
    os.symlink("moved.txt", symlink)
    _sync_directory(dst)
    _settle()
    operations.append("create symlink dst/link.txt -> moved.txt")

    return operations


def _snapshot_tree(root: Path) -> list[dict]:
    import stat as _stat
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
                "nlink": st.st_nlink,
            }
        )
        for name in filenames:
            path = Path(current_root) / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            mode = st.st_mode
            if _stat.S_ISLNK(mode):
                target = os.readlink(path)
                entries.append(
                    {
                        "type": "symlink",
                        "path": str(rel_path),
                        "inode": st.st_ino,
                        "nlink": st.st_nlink,
                        "logical_size": len(target),
                        "symlink_target": target,
                    }
                )
            elif _stat.S_ISREG(mode):
                entries.append(
                    {
                        "type": "file",
                        "path": str(rel_path),
                        "inode": st.st_ino,
                        "nlink": st.st_nlink,
                        "logical_size": st.st_size,
                        "allocated_bytes": st.st_blocks * 512,
                    }
                )
    return entries


def _compare_namespace(mounted: list[dict], native: list[dict]) -> dict:
    def normalize(entry: dict) -> dict:
        out = {
            "path": entry["path"],
            "type": entry["type"],
            "file_id": entry.get("file_id", entry.get("inode")),
        }
        if entry["type"] in {"file", "symlink"}:
            out["logical_size"] = entry.get("logical_size")
        if entry["type"] == "symlink":
            out["symlink_target"] = entry.get("symlink_target")
        return out

    mounted_map = {e["path"]: normalize(e) for e in mounted}
    native_map = {e["path"]: normalize(e) for e in native}
    missing = sorted(set(mounted_map) - set(native_map))
    unexpected = sorted(set(native_map) - set(mounted_map))
    mismatches: list[dict] = []
    for path in sorted(set(mounted_map) & set(native_map)):
        if mounted_map[path] != native_map[path]:
            mismatches.append(
                {"path": path, "expected": mounted_map[path], "actual": native_map[path]}
            )
    return {
        "matched": not missing and not unexpected and not mismatches,
        "missing_paths": missing,
        "unexpected_paths": unexpected,
        "mismatches": mismatches,
    }


if __name__ == "__main__":
    raise SystemExit(main())
