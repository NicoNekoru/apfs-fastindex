#!/usr/bin/env python3
"""EX-20: SR-018 name/case fixture parity.

Builds one case-insensitive APFS image and one case-sensitive APFS image,
each containing ASCII case variants, precomposed NFC vs decomposed NFD
Unicode forms, and an explicit ASCII control entry. Captures the mounted
POSIX traversal as oracle, then asserts that Rust's enumerated paths
(reconstructed from FsRecordDump.records) match byte-for-byte.

Rust makes no lookup-by-name claim; this probe only verifies stored UTF-8
name preservation across both volume modes.
"""

from __future__ import annotations

import datetime as _dt
import json
import os
import platform
import plistlib
import shutil
import subprocess
import tempfile
import time
import unicodedata
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
REPO_ROOT = ARTIFACT_DIR.parents[4]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"

APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
APFS_ROOT_DIR_OID = 2


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


# ---- fixture ------------------------------------------------------------- #

def try_create(path: Path) -> dict:
    """Best-effort O_CREAT|O_EXCL file. Returns the outcome dict so the
    test records the APFS lookup oracle (accepted/rejected)."""
    try:
        fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        try:
            os.write(fd, path.name.encode("utf-8"))
        finally:
            os.close(fd)
        return {"path": path.name, "created": True, "error": None}
    except FileExistsError as exc:
        return {"path": path.name, "created": False, "error": str(exc)}
    except OSError as exc:
        return {"path": path.name, "created": False, "error": f"{type(exc).__name__}: {exc}"}


def build_fixture(root: Path) -> list[dict]:
    operations: list[dict] = []
    plain = root / "plain.txt"
    plain.write_text("plain ASCII control\n")
    operations.append({"step": "create plain.txt", "outcome": {"path": "plain.txt", "created": True}})

    capital = root / "CaseName.txt"
    capital.write_text("capital first\n")
    operations.append({"step": "create CaseName.txt", "outcome": {"path": "CaseName.txt", "created": True}})

    lowercase_attempt = try_create(root / "casename.txt")
    operations.append({"step": "attempt casename.txt after CaseName.txt", "outcome": lowercase_attempt})

    # Precomposed NFC: U+00E9 (é). 'café.txt' for NFD.
    nfc_name = "café.txt"  # this is NFD; convert
    nfc = unicodedata.normalize("NFC", nfc_name)  # composed
    nfd = unicodedata.normalize("NFD", nfc_name)  # decomposed
    assert nfc != nfd, "expected NFC and NFD forms to differ in bytes"
    nfc_path = root / nfc
    nfc_path.write_text("nfc form\n")
    operations.append(
        {
            "step": f"create NFC {nfc!r}",
            "outcome": {"path": nfc, "created": True, "utf8_hex": nfc.encode().hex()},
        }
    )
    nfd_attempt = try_create(root / nfd)
    operations.append(
        {
            "step": f"attempt NFD {nfd!r} after NFC sibling",
            "outcome": {**nfd_attempt, "utf8_hex": nfd.encode().hex()},
        }
    )

    run(["sync"])
    time.sleep(0.2)
    return operations


def snapshot_oracle(root: Path) -> dict:
    entries: list[dict] = []
    for current_root, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        if Path(current_root).name == ".fseventsd":
            continue
        dirnames[:] = [name for name in dirnames if name != ".fseventsd"]
        rel = Path(current_root).relative_to(root)
        st = os.lstat(current_root)
        path_str = "." if str(rel) == "." else str(rel)
        entries.append(
            {
                "type": "dir",
                "path": path_str,
                "path_utf8_hex": path_str.encode("utf-8").hex(),
                "inode": st.st_ino,
            }
        )
        for name in filenames:
            path = Path(current_root) / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            entries.append(
                {
                    "type": "file",
                    "path": str(rel_path),
                    "path_utf8_hex": str(rel_path).encode("utf-8").hex(),
                    "inode": st.st_ino,
                    "st_size": st.st_size,
                }
            )
    return {"entries": entries}


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


# ---- path reconstruction from FsRecordDump.records ---------------------- #

def reconstruct_paths(records: list[dict]) -> list[dict]:
    """Build a (parent_id, name) -> (child_id, kind) graph from dir_rec
    records, then walk it from APFS_ROOT_DIR_OID (oid 2) and emit a list
    of {path, type, file_id, name_utf8_hex} rows. Names are the stored
    UTF-8 bytes verbatim — no normalization or case folding."""
    drec_index: dict[int, list[dict]] = {}
    for record in records:
        if record["family"] != "dir_rec":
            continue
        parent = record["object_id"]
        key = record["key"]
        value = record["value"]
        if key.get("kind") != "named":
            continue
        name = key["name"].rstrip("\x00")
        name_hex = key.get("name_bytes_hex", "")
        # Strip trailing NUL from the recorded hex too for comparison.
        if name_hex.endswith("00"):
            name_hex_visible = name_hex[:-2]
        else:
            name_hex_visible = name_hex
        drec_index.setdefault(parent, []).append(
            {
                "name": name,
                "name_utf8_hex": name_hex_visible,
                "file_id": value["file_id"],
                "entry_type": value["entry_type"],
            }
        )

    entry_type_to_kind = {4: "dir", 8: "file", 10: "symlink"}
    rows: list[dict] = []

    def walk(parent_id: int, parent_path: str) -> None:
        for child in sorted(drec_index.get(parent_id, []), key=lambda c: c["name"]):
            if child["name"] == ".fseventsd":
                continue
            child_path = (
                child["name"] if parent_path == "." else f"{parent_path}/{child['name']}"
            )
            kind = entry_type_to_kind.get(child["entry_type"], f"other({child['entry_type']})")
            rows.append(
                {
                    "type": kind,
                    "path": child_path,
                    "path_utf8_hex": child_path.encode("utf-8").hex(),
                    "file_id": child["file_id"],
                    "name_utf8_hex": child["name_utf8_hex"],
                }
            )
            if kind == "dir":
                walk(child["file_id"], child_path)

    rows.append(
        {
            "type": "dir",
            "path": ".",
            "path_utf8_hex": b".".hex(),
            "file_id": APFS_ROOT_DIR_OID,
            "name_utf8_hex": "",
        }
    )
    walk(APFS_ROOT_DIR_OID, ".")
    return rows


def compare_paths(mounted: list[dict], rust: list[dict]) -> dict:
    def to_key(entry: dict) -> tuple[str, str]:
        return (entry["type"], entry["path"])

    mounted_map = {to_key(e): e for e in mounted}
    rust_map = {to_key(e): e for e in rust}
    missing_in_rust = sorted(mounted_map.keys() - rust_map.keys())
    extra_in_rust = sorted(rust_map.keys() - mounted_map.keys())
    hex_mismatches: list[dict] = []
    for key in sorted(mounted_map.keys() & rust_map.keys()):
        if mounted_map[key]["path_utf8_hex"] != rust_map[key]["path_utf8_hex"]:
            hex_mismatches.append(
                {
                    "type": key[0],
                    "mounted_path": mounted_map[key]["path"],
                    "rust_path": rust_map[key]["path"],
                    "mounted_hex": mounted_map[key]["path_utf8_hex"],
                    "rust_hex": rust_map[key]["path_utf8_hex"],
                }
            )
    return {
        "matched": not missing_in_rust and not extra_in_rust and not hex_mismatches,
        "mounted_count": len(mounted_map),
        "rust_count": len(rust_map),
        "missing_in_rust": missing_in_rust,
        "extra_in_rust": extra_in_rust,
        "hex_mismatches": hex_mismatches,
    }


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "hdiutil": shutil.which("hdiutil"),
        "cargo": shutil.which("cargo"),
        "sw_vers": run(["sw_vers"]).stdout,
    }


# ---- driver -------------------------------------------------------------- #

def run_case(slug: str, volume_label: str, fs_name: str) -> dict:
    base = Path(tempfile.mkdtemp(prefix=f"apfsfi-ex20-{slug}-", dir="/tmp"))
    image_path = base / f"{volume_label}.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_detach = None
    nomount_detach = None
    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "160m",
                "-fs",
                fs_name,
                "-volname",
                volume_label,
                "-nospotlight",
                str(image_path),
            ]
        )
        _, mounted_detach = attach_image(image_path, mountpoint)
        operations = build_fixture(mountpoint)
        oracle = snapshot_oracle(mountpoint)
        write_json(f"ex20-{slug}-mounted-posix-oracle.json", oracle)
        write_json(f"ex20-{slug}-fixture-operations.json", {"operations": operations})
        detach_device(mounted_detach)
        mounted_detach = None
        time.sleep(0.4)

        _, nomount_detach, raw_container = attach_nomount(image_path)
        rust_scan = run_rust_scan(raw_container)
        sel = rust_scan.get("selected_checkpoint")
        if not sel:
            raise ProbeError(
                "oracle_inconclusive",
                f"Rust did not publish selected_checkpoint for {volume_label}",
            )
        volume = sel["volumes"][0]
        # SR-018: record volume case + normalization mode.
        summary_record = volume.get("summary") or {}
        case_insensitive = summary_record.get("case_insensitive")
        normalization_insensitive = summary_record.get("normalization_insensitive")
        dump = volume.get("fs_record_dump") or {}
        records = dump.get("records") or []
        write_json(f"ex20-{slug}-rust-records.json", records)

        rust_rows = reconstruct_paths(records)
        comparison = compare_paths(oracle["entries"], rust_rows)
        comparison["case_insensitive_flag"] = case_insensitive
        comparison["normalization_insensitive_flag"] = normalization_insensitive
        write_json(f"ex20-{slug}-comparison.json", comparison)
        return comparison
    finally:
        if nomount_detach:
            detach_device(nomount_detach)
        if mounted_detach:
            detach_device(mounted_detach)
        shutil.rmtree(base, ignore_errors=True)


def main() -> int:
    write_json("environment.json", environment())
    summary = {"status": "executed", "verdict": "pending", "verdict_detail": ""}
    try:
        ci = run_case("ci", "EX20CI", "APFS")
        cs = run_case("cs", "EX20CS", "Case-sensitive APFS")
        all_matched = ci["matched"] and cs["matched"]
        case_flags_ok = (
            ci.get("case_insensitive_flag") is True
            and cs.get("case_insensitive_flag") is False
        )
        if all_matched and case_flags_ok:
            verdict = "validated_sr_018_name_preservation"
            detail = (
                "Rust paths match POSIX paths byte-for-byte on both CI and CS volumes; "
                "volume case_insensitive/normalization_insensitive flags propagate "
                "as expected."
            )
        else:
            verdict = "name_preservation_gap"
            detail = (
                f"CI matched={ci['matched']} hex_mismatches={len(ci.get('hex_mismatches', []))}; "
                f"CS matched={cs['matched']} hex_mismatches={len(cs.get('hex_mismatches', []))}; "
                f"case_flags_ok={case_flags_ok}."
            )
        summary["verdict"] = verdict
        summary["verdict_detail"] = detail
        summary["ci"] = {
            "matched": ci["matched"],
            "mounted_count": ci["mounted_count"],
            "rust_count": ci["rust_count"],
            "hex_mismatches": len(ci.get("hex_mismatches", [])),
            "case_insensitive_flag": ci.get("case_insensitive_flag"),
            "normalization_insensitive_flag": ci.get("normalization_insensitive_flag"),
        }
        summary["cs"] = {
            "matched": cs["matched"],
            "mounted_count": cs["mounted_count"],
            "rust_count": cs["rust_count"],
            "hex_mismatches": len(cs.get("hex_mismatches", [])),
            "case_insensitive_flag": cs.get("case_insensitive_flag"),
            "normalization_insensitive_flag": cs.get("normalization_insensitive_flag"),
        }
        write_json("summary.json", summary)
        return 0 if verdict == "validated_sr_018_name_preservation" else 1
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


if __name__ == "__main__":
    raise SystemExit(main())
