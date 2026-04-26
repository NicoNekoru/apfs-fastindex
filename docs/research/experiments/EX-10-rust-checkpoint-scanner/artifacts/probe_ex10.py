#!/usr/bin/env python3
"""Run the Rust checkpoint scanner against the existing proof fixture and
assert on the structure of the native dump output.

The asserted invariants are oracle-backed contracts taken from the existing
Python proof skeleton:

* the Rust checkpoint scanner picks the highest-XID NXSB candidate,
* `selected_checkpoint` carries a populated container, checkpoint map,
  container OMAP, and at least one supported volume,
* every supported volume carries a volume OMAP and a populated FS-record
  family dump,
* `validation_gaps` is empty (any drift is a research finding, not a pass),
* the FS-record family counts cover the v1-namespace-scoped families that
  the proof fixture is designed to exercise: inode, dir_rec, dstream_id,
  xattr, sibling_link, sibling_map. file_extent shows up too, but is
  intentionally out of v1 namespace scope.
"""

from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path


ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
REPO_ROOT = ARTIFACT_DIR.parents[4]
GENERATED_DIR.mkdir(exist_ok=True)
sys.path.insert(0, str(REPO_ROOT / "src"))

from apfs_fastindex.poc_fixture import build_proof_fixture  # noqa: E402


REQUIRED_V1_FAMILIES = {
    "inode",
    "dir_rec",
    "dstream_id",
    "xattr",
    "sibling_link",
    "sibling_map",
}


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(
        json.dumps(payload, indent=2, sort_keys=True) + "\n"
    )


def assert_native_contract(stdout_json: dict) -> list[str]:
    """Return a list of contract violations; empty list means pass."""
    violations: list[str] = []
    if stdout_json is None:
        return ["scanner produced no stdout"]

    parser_output = stdout_json.get("parser_output", {})
    scan_state = parser_output.get("scan_state", {})
    if scan_state.get("validation_gaps"):
        violations.append(
            f"scan_state.validation_gaps must be empty, got "
            f"{scan_state['validation_gaps']!r}"
        )
    if scan_state.get("highest_xid", 0) <= 0:
        violations.append(
            "scan_state.highest_xid must be a positive XID drawn from the "
            "checkpoint descriptor area"
        )

    selected = stdout_json.get("selected_checkpoint")
    if not selected:
        return violations + [
            "selected_checkpoint missing - native dump never reached container decode"
        ]

    container = selected.get("container", {})
    if container.get("unsupported_incompatible_features", -1) != 0:
        violations.append(
            "container.unsupported_incompatible_features must be 0 to honor the "
            "feature allowlist contract"
        )
    if not container.get("volume_oids"):
        violations.append("container.volume_oids must list at least one OID")

    checkpoint_map = selected.get("checkpoint_map", {})
    if not checkpoint_map.get("last_flag_seen"):
        violations.append(
            "checkpoint_map.last_flag_seen must be true; the descriptor ring did "
            "not terminate cleanly for the selected checkpoint"
        )

    container_omap = selected.get("container_omap", {})
    if not container_omap.get("sample_mappings"):
        violations.append(
            "container_omap.sample_mappings must include at least the volume "
            "superblock mapping"
        )

    volumes = selected.get("volumes", [])
    if not volumes:
        violations.append("selected_checkpoint.volumes must not be empty")
    for volume in volumes:
        if volume.get("status") != "supported":
            violations.append(
                f"volume oid {volume.get('volume_oid')} status={volume.get('status')!r} "
                "but the proof fixture is built unencrypted on the v1 allowlist"
            )
            continue
        if not volume.get("volume_omap"):
            violations.append(
                f"volume oid {volume.get('volume_oid')} missing volume_omap "
                "(volume superblock decoded but OMAP load skipped)"
            )
        if not volume.get("root_tree_lookup"):
            violations.append(
                f"volume oid {volume.get('volume_oid')} missing root_tree_lookup; "
                "FS-tree could not be reached through the volume OMAP"
            )
        dump = volume.get("fs_record_dump")
        if not dump:
            violations.append(
                f"volume oid {volume.get('volume_oid')} missing fs_record_dump; "
                "FS-tree validation aborted before any record was counted"
            )
            continue
        if dump.get("unsupported_record_count", -1) != 0:
            violations.append(
                f"volume oid {volume.get('volume_oid')} reports "
                f"{dump.get('unsupported_record_count')} unsupported FS records; "
                "v1 must fail-closed on unknown record families"
            )
        if dump.get("leaf_record_count", 0) <= 0:
            violations.append(
                f"volume oid {volume.get('volume_oid')} fs_record_dump empty; "
                "expected at least the proof fixture's records"
            )
        family_names = {
            family["name"] for family in dump.get("family_counts", [])
        }
        missing = REQUIRED_V1_FAMILIES - family_names
        if missing:
            violations.append(
                f"volume oid {volume.get('volume_oid')} FS-record dump missing "
                f"v1-scope families {sorted(missing)}"
            )

    native_validation = selected.get("native_validation", {})
    if native_validation.get("validation_gaps"):
        violations.append(
            "native_validation.validation_gaps must be empty for a clean fixture run; "
            f"got {native_validation['validation_gaps']!r}"
        )

    return violations


def main() -> int:
    with build_proof_fixture() as fixture:
        proc = subprocess.run(
            [
                "cargo",
                "run",
                "--quiet",
                "--bin",
                "apfs-fastindex-scan",
                "--",
                str(fixture.image_path),
            ],
            cwd=REPO_ROOT,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
        )
        stdout_json = json.loads(proc.stdout) if proc.stdout.strip() else None
        violations = assert_native_contract(stdout_json or {})
        payload = {
            "command": "cargo run --quiet --bin apfs-fastindex-scan -- <proof-fixture.dmg>",
            "returncode": proc.returncode,
            "stderr": proc.stderr,
            "stdout_json": stdout_json,
            "fixture_operations": list(fixture.operations),
            "contract_violations": violations,
        }
        write_json("proof-fixture-smoke.json", payload)
        if proc.returncode != 0:
            print(
                f"apfs-fastindex-scan exited with status {proc.returncode}",
                file=sys.stderr,
            )
            return proc.returncode
        if violations:
            print("EX-10 contract violations:", file=sys.stderr)
            for line in violations:
                print(f"  - {line}", file=sys.stderr)
            return 1
        return 0


if __name__ == "__main__":
    raise SystemExit(main())
