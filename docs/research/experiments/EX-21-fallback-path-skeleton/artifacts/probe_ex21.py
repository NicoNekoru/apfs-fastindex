#!/usr/bin/env python3
"""EX-21: fallback POSIX traversal vs Rust raw output on the proof fixture.

Builds the proof fixture, runs the fallback POSIX traversal while the
image is mounted, then detaches and runs the Rust raw scanner against
the detached `.dmg`. Diffs the two `NamespaceEntry` lists and the two
`DirectoryAggregate` lists field-by-field.

Records `file_id` divergence as a soft note rather than a mismatch: the
fallback path uses POSIX inode numbers and the raw path uses APFS
virtual OIDs, which the v1 contract permits to differ across source
classes.
"""

from __future__ import annotations

import datetime as _dt
import json
import platform
import shutil
import subprocess
import sys
import time
from dataclasses import asdict
from pathlib import Path
from typing import Any

ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
GENERATED_DIR.mkdir(exist_ok=True)
REPO_ROOT = ARTIFACT_DIR.parents[4]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"
sys.path.insert(0, str(REPO_ROOT / "src"))

from apfs_fastindex.fallback_traversal import traverse_mounted_directory  # noqa: E402
from apfs_fastindex.poc_fixture import build_proof_fixture  # noqa: E402


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(
        json.dumps(payload, indent=2, sort_keys=True, default=_json_default) + "\n"
    )


def _json_default(obj: Any) -> Any:
    if isinstance(obj, bytes):
        return obj.hex()
    if hasattr(obj, "__dict__"):
        return asdict(obj)
    raise TypeError(f"unserializable: {type(obj).__name__}")


def run_rust(image_path: Path) -> dict:
    proc = subprocess.run(
        ["cargo", "run", "--quiet", "--bin", "apfs-fastindex-scan", "--", str(image_path)],
        cwd=str(RUST_CRATE_DIR),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        check=False,
    )
    if proc.returncode != 0:
        raise RuntimeError(
            f"apfs-fastindex-scan failed (rc={proc.returncode}); stderr:\n{proc.stderr}"
        )
    return json.loads(proc.stdout)


def to_compare_dict(entry: dict) -> dict:
    out = {
        "path": entry["path"],
        "entry_kind": entry["entry_kind"],
        "logical_size": entry.get("logical_size", 0),
    }
    if entry["entry_kind"] == "symlink":
        out["symlink_target"] = entry.get("symlink_target")
    return out


def fallback_to_dicts(entries) -> list[dict]:
    out: list[dict] = []
    for entry in entries:
        item = {
            "path": entry.path,
            "entry_kind": entry.entry_kind,
            "file_id": entry.file_id,
            "logical_size": entry.logical_size,
        }
        if entry.entry_kind == "symlink":
            item["symlink_target"] = entry.symlink_target
        out.append(item)
    return out


def fallback_aggregates_to_dicts(aggregates) -> list[dict]:
    return [
        {
            "path": agg.path,
            "unique_inode_logical_total": agg.unique_inode_logical_total,
            "contributing_file_ids": list(agg.contributing_file_ids),
        }
        for agg in aggregates
    ]


def diff_entries(fallback: list[dict], rust: list[dict]) -> dict:
    """Compare shape-relevant fields only (path, entry_kind, logical_size,
    symlink_target). `file_id` divergence is recorded as a soft note."""
    fallback_map = {e["path"]: to_compare_dict(e) for e in fallback}
    rust_map = {e["path"]: to_compare_dict(e) for e in rust}
    missing = sorted(fallback_map.keys() - rust_map.keys())
    extra = sorted(rust_map.keys() - fallback_map.keys())
    mismatches = []
    for path in sorted(fallback_map.keys() & rust_map.keys()):
        if fallback_map[path] != rust_map[path]:
            mismatches.append(
                {
                    "path": path,
                    "fallback": fallback_map[path],
                    "rust": rust_map[path],
                }
            )
    file_id_divergence = []
    for path in sorted(fallback_map.keys() & rust_map.keys()):
        fb_id = next(e["file_id"] for e in fallback if e["path"] == path)
        ru_id = next(e["file_id"] for e in rust if e["path"] == path)
        if fb_id != ru_id:
            file_id_divergence.append(
                {"path": path, "fallback_file_id": fb_id, "rust_file_id": ru_id}
            )
    return {
        "matched": not missing and not extra and not mismatches,
        "fallback_count": len(fallback_map),
        "rust_count": len(rust_map),
        "missing_in_rust": missing,
        "extra_in_rust": extra,
        "mismatches": mismatches,
        "file_id_divergence_count": len(file_id_divergence),
        "file_id_divergence": file_id_divergence,
    }


def diff_aggregates(fallback: list[dict], rust: list[dict]) -> dict:
    fb_map = {a["path"]: a for a in fallback}
    ru_map = {a["path"]: a for a in rust}
    missing = sorted(fb_map.keys() - ru_map.keys())
    extra = sorted(ru_map.keys() - fb_map.keys())
    mismatches = []
    for path in sorted(fb_map.keys() & ru_map.keys()):
        if fb_map[path]["unique_inode_logical_total"] != ru_map[path]["unique_inode_logical_total"]:
            mismatches.append(
                {
                    "path": path,
                    "fallback_total": fb_map[path]["unique_inode_logical_total"],
                    "rust_total": ru_map[path]["unique_inode_logical_total"],
                }
            )
    return {
        "matched": not missing and not extra and not mismatches,
        "missing_in_rust": missing,
        "extra_in_rust": extra,
        "mismatches": mismatches,
    }


def environment() -> dict:
    return {
        "platform": platform.platform(),
        "python": platform.python_version(),
        "cwd": str(REPO_ROOT),
        "timestamp": _dt.datetime.now(_dt.timezone.utc).isoformat(),
        "cargo": shutil.which("cargo"),
    }


def main() -> int:
    write_json("environment.json", environment())
    summary = {"status": "executed", "verdict": "pending", "verdict_detail": ""}
    try:
        with build_proof_fixture() as fixture:
            # Re-attach the image so we can run the fallback against a
            # mounted directory (build_proof_fixture detaches before
            # yielding). Use hdiutil directly.
            import plistlib
            attach = subprocess.run(
                ["hdiutil", "attach", "-plist", "-mountpoint",
                 str(fixture.image_path.parent / "ex21-mnt"), str(fixture.image_path)],
                check=True, capture_output=True, text=True,
            )
            (fixture.image_path.parent / "ex21-mnt").mkdir(exist_ok=True)
            info = plistlib.loads(attach.stdout.encode("utf-8"))
            detach_dev = info["system-entities"][0]["dev-entry"]
            try:
                mountpoint = Path(fixture.image_path.parent / "ex21-mnt")
                fb_entries, fb_aggregates = traverse_mounted_directory(mountpoint)
                fallback_entries = fallback_to_dicts(fb_entries)
                fallback_aggregates = fallback_aggregates_to_dicts(fb_aggregates)
            finally:
                subprocess.run(["hdiutil", "detach", detach_dev], check=False)
                time.sleep(0.3)

            rust_doc = run_rust(fixture.image_path)
            rust_parser_output = rust_doc["parser_output"]
            rust_entries = rust_parser_output.get("entries", [])
            rust_aggregates = rust_parser_output.get("aggregates", [])

            write_json("ex21-fallback-entries.json", {
                "entries": fallback_entries,
                "aggregates": fallback_aggregates,
            })
            write_json("ex21-rust-entries.json", {
                "entries": rust_entries,
                "aggregates": rust_aggregates,
            })

            entries_diff = diff_entries(fallback_entries, rust_entries)
            aggregates_diff = diff_aggregates(fallback_aggregates, rust_aggregates)
            write_json("ex21-comparison.json", {
                "entries": entries_diff,
                "aggregates": aggregates_diff,
            })

            matched = entries_diff["matched"] and aggregates_diff["matched"]
            if matched:
                verdict = "validated_fallback_skeleton"
                detail = (
                    f"Fallback emitted {entries_diff['fallback_count']} entries and "
                    f"{len(fallback_aggregates)} aggregates; Rust raw output matches "
                    "shape contract exactly."
                )
            else:
                verdict = "fallback_shape_drift"
                detail = (
                    f"entries.matched={entries_diff['matched']} ({len(entries_diff['mismatches'])} mismatches); "
                    f"aggregates.matched={aggregates_diff['matched']} ({len(aggregates_diff['mismatches'])} mismatches)"
                )
            summary["verdict"] = verdict
            summary["verdict_detail"] = detail
            summary["entries"] = entries_diff
            summary["aggregates"] = aggregates_diff
            write_json("summary.json", summary)
            return 0 if matched else 1
    except Exception as err:
        summary["verdict"] = "probe_exception"
        summary["verdict_detail"] = f"{type(err).__name__}: {err}"
        write_json("summary.json", summary)
        raise


if __name__ == "__main__":
    raise SystemExit(main())
