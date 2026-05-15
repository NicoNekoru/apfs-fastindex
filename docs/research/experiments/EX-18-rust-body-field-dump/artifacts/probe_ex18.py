#!/usr/bin/env python3
"""EX-18: diff Rust FsRecordDump.records against EX-13 + EX-16 Python output.

Rebuilds the EX-13 proof fixture, runs the patched Rust scanner and a
Python parser (EX-13 helpers + EX-16 SR-015 xfield replay), then compares
each record `(node_paddr, entry_index)` field-by-field. Records the
per-record divergences if any.
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
EX16_PROBE = (
    REPO_ROOT
    / "docs"
    / "research"
    / "experiments"
    / "EX-16-sr-015-xfield-replay"
    / "artifacts"
    / "probe_ex16.py"
)

APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"


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


def load_module(name: str, path: Path):
    spec = importlib.util.spec_from_file_location(name, path)
    if spec is None or spec.loader is None:
        raise ProbeError("not_executed", f"unable to load {path}")
    module = importlib.util.module_from_spec(spec)
    sys.modules[name] = module
    spec.loader.exec_module(module)
    return module


EX13 = load_module("probe_ex13", EX13_PROBE)
EX16 = load_module("probe_ex16", EX16_PROBE)


# ---- normalization ------------------------------------------------------- #

def normalize_python_key(key: dict) -> dict:
    kind = key.get("kind")
    if kind == "named":
        return {
            "kind": "named",
            "raw_key_form": key.get("raw_key_form"),
            "name_len": key.get("name_len"),
            "name": (key.get("name") or "").rstrip("\x00"),
            "name_bytes_hex": key.get("name_bytes_hex"),
        }
    if kind == "sibling_link":
        return {"kind": "sibling_link", "sibling_id": key.get("sibling_id")}
    return {"kind": "plain"}


def normalize_python_value(value: dict, record: dict) -> dict:
    kind = value.get("kind")
    if kind == "inode":
        sr015 = record.get("sr015_xfield_replay") or {}
        return {
            "kind": "inode",
            "parent_id": value["parent_id"],
            "private_id": value["private_id"],
            "internal_flags": value["internal_flags"],
            "nchildren_or_nlink": value["nchildren_or_nlink"],
            "bsd_flags": value["bsd_flags"],
            "owner": value["owner"],
            "group": value["group"],
            "mode": value["mode"],
            "uncompressed_size": value["uncompressed_size"],
            "has_uncompressed_size": value["has_uncompressed_size"],
            "xfields": [normalize_python_xfield(f) for f in (sr015.get("fields") or [])],
            "xfield_used_data": sr015.get("xf_used_data", 0),
            "xfield_padded_total": sr015.get("padded_values_total", 0),
            "xfield_unused_trailing_bytes": sr015.get("unused_trailing_bytes", 0),
            "dstream": value.get("dstream"),
            "sparse_bytes": value.get("sparse_bytes"),
            "inode_name": (value.get("inode_name") or "").rstrip("\x00") or None
            if value.get("inode_name") is not None
            else None,
        }
    if kind == "dir_rec":
        sr015 = record.get("sr015_xfield_replay") or {}
        return {
            "kind": "dir_rec",
            "file_id": value["file_id"],
            "date_added": value["date_added"],
            "flags": value["flags"],
            "entry_type": value["entry_type"],
            "sibling_id": value.get("sibling_id"),
            "xfields": [normalize_python_xfield(f) for f in (sr015.get("fields") or [])],
            "xfield_used_data": sr015.get("xf_used_data", 0),
            "xfield_padded_total": sr015.get("padded_values_total", 0),
            "xfield_unused_trailing_bytes": sr015.get("unused_trailing_bytes", 0),
        }
    if kind == "xattr":
        return {
            "kind": "xattr",
            "flags": value["flags"],
            "xdata_len": value.get("xdata_len"),
            "embedded": bool(value["flags"] & 0x2),
            "stream": bool(value["flags"] & 0x1),
            "payload_hex": value.get("payload_hex") or "",
            "payload_utf8": (value.get("payload_utf8") or None) if value.get("payload_utf8") is not None else None,
            "stream_xattr_obj_id": value.get("stream_xattr_obj_id"),
            "stream_dstream": value.get("stream_dstream"),
        }
    if kind == "sibling_link":
        return {
            "kind": "sibling_link",
            "parent_id": value["parent_id"],
            "name_len": value["name_len"],
            "name": (value.get("name") or "").rstrip("\x00"),
            "name_bytes_hex": value.get("name_bytes_hex"),
        }
    if kind == "dstream_id":
        return {"kind": "dstream_id", "refcnt": value.get("refcnt")}
    if kind == "sibling_map":
        return {"kind": "sibling_map", "file_id": value.get("file_id")}
    return {"kind": "unsupported", "reason": "record family is outside the v1 body decoder allowlist"}


def normalize_python_xfield(field: dict) -> dict:
    interpreted = field.get("interpreted")
    norm: dict[str, Any] = {
        "x_type": field["x_type"],
        "x_flags": field["x_flags"],
        "x_size": field["x_size"],
        "padded_length": field.get("padded_length"),
        "value_hex": field.get("value_hex"),
        "interpreted": None,
    }
    if interpreted is None:
        return norm
    kind = interpreted.get("kind")
    if kind == "u64":
        norm["interpreted"] = {"kind": "u64", "value": interpreted["value"]}
    elif kind == "utf8":
        norm["interpreted"] = {
            "kind": "utf8",
            "value": (interpreted.get("value") or "").rstrip("\x00"),
        }
    elif kind == "dstream":
        norm["interpreted"] = {"kind": "dstream", "value": interpreted["value"]}
    return norm


def normalize_python_record(record: dict) -> dict:
    return {
        "node_paddr": record["node_paddr"],
        "entry_index": record["entry_index"],
        "object_id": record["object_id"],
        "raw_type": record["raw_type"],
        "family": record["family"],
        "key_len": record["key_len"],
        "value_len": record["value_len"],
        "key": normalize_python_key(record["key"]),
        "value": normalize_python_value(record["value"], record),
        "validation_notes": record.get("validation_notes") or [],
    }


def normalize_rust_record(record: dict) -> dict:
    # Rust output already matches the target shape; just round-trip through
    # dict to drop ordering differences.
    return json.loads(json.dumps(record, sort_keys=True))


def diff_records(
    rust_records: list[dict], python_records: list[dict]
) -> dict:
    rust_index = {(r["node_paddr"], r["entry_index"]): r for r in rust_records}
    py_index = {(r["node_paddr"], r["entry_index"]): r for r in python_records}
    rust_keys = set(rust_index)
    py_keys = set(py_index)
    missing_in_rust = sorted(py_keys - rust_keys)
    extra_in_rust = sorted(rust_keys - py_keys)
    mismatches: list[dict] = []
    for key in sorted(rust_keys & py_keys):
        rust = rust_index[key]
        py = py_index[key]
        if rust != py:
            mismatches.append(
                {
                    "node_paddr": key[0],
                    "entry_index": key[1],
                    "rust": rust,
                    "python": py,
                    "diff": shallow_diff(rust, py),
                }
            )
    return {
        "matched": not missing_in_rust and not extra_in_rust and not mismatches,
        "rust_record_count": len(rust_index),
        "python_record_count": len(py_index),
        "missing_in_rust_keys": missing_in_rust,
        "extra_in_rust_keys": extra_in_rust,
        "mismatches": mismatches,
    }


def shallow_diff(rust: dict, py: dict) -> dict:
    rust_keys = set(rust)
    py_keys = set(py)
    only_rust = sorted(rust_keys - py_keys)
    only_py = sorted(py_keys - rust_keys)
    differing: list[str] = []
    for k in sorted(rust_keys & py_keys):
        if rust[k] != py[k]:
            differing.append(k)
    return {
        "only_in_rust": only_rust,
        "only_in_python": only_py,
        "differing_fields": differing,
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


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
    }


# ---- driver -------------------------------------------------------------- #

def main() -> int:
    write_json("environment.json", environment())
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex18-", dir="/tmp"))
    image_path = base / "EX18CI.dmg"
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
                "EX18CI",
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        EX16._build_ex13_fixture(mountpoint)  # reuse EX-13 operations
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
        # The proof fixture is single-volume.
        volume = sel["volumes"][0]
        dump = volume.get("fs_record_dump") or {}
        rust_records_raw = dump.get("records") or []
        rust_records = [normalize_rust_record(r) for r in rust_records_raw]
        write_json("ex18-rust-records.json", rust_records)

        block_size = sel["container"]["block_size"]
        root_paddr = volume["root_tree_lookup"]["paddr"]
        with open(raw_container, "rb", buffering=0) as handle:
            py_records_raw = EX16.walk_fs_tree_with_raw_bytes(handle, root_paddr, block_size)
        # Apply SR-015 replay to each record so xfields match the cursor rule.
        for record in py_records_raw:
            record["sr015_xfield_replay"] = EX16.replay_record(record)
            # Override inode/dir_rec xfield-derived value fields.
            if record["family"] == "inode":
                fields = record["sr015_xfield_replay"].get("fields") or []
                record["value"]["dstream"] = next(
                    (
                        f["interpreted"]["value"]
                        for f in fields
                        if (f.get("interpreted") or {}).get("kind") == "dstream"
                    ),
                    None,
                )
                record["value"]["sparse_bytes"] = next(
                    (
                        f["interpreted"].get("value")
                        for f in fields
                        if f["x_type"] == 13
                        and (f.get("interpreted") or {}).get("kind") == "u64"
                    ),
                    None,
                )
                record["value"]["inode_name"] = next(
                    (
                        f["interpreted"].get("value")
                        for f in fields
                        if f["x_type"] == 4
                        and (f.get("interpreted") or {}).get("kind") == "utf8"
                    ),
                    None,
                )
            elif record["family"] == "dir_rec":
                fields = record["sr015_xfield_replay"].get("fields") or []
                record["value"]["sibling_id"] = next(
                    (
                        f["interpreted"].get("value")
                        for f in fields
                        if f["x_type"] == 1
                        and (f.get("interpreted") or {}).get("kind") == "u64"
                    ),
                    None,
                )
        py_records = [normalize_python_record(r) for r in py_records_raw]
        write_json("ex18-python-records.json", py_records)

        comparison = diff_records(rust_records, py_records)
        write_json("ex18-comparison.json", comparison)

        if comparison["matched"]:
            verdict = "field_level_parity"
            detail = (
                f"Rust and Python produced {comparison['rust_record_count']} matching "
                "records with zero divergent fields."
            )
        else:
            verdict = "field_divergence"
            detail = (
                f"{len(comparison['mismatches'])} record(s) differ; "
                f"missing_in_rust={len(comparison['missing_in_rust_keys'])}; "
                f"extra_in_rust={len(comparison['extra_in_rust_keys'])}"
            )
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["rust_record_count"] = comparison["rust_record_count"]
        summary["python_record_count"] = comparison["python_record_count"]
        summary["mismatch_count"] = len(comparison["mismatches"])
        summary["missing_in_rust_count"] = len(comparison["missing_in_rust_keys"])
        summary["extra_in_rust_count"] = len(comparison["extra_in_rust_keys"])
        write_json("summary.json", summary)
        return 0 if verdict == "field_level_parity" else 1
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
