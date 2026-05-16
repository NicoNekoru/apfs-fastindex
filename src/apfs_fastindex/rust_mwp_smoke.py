"""Rust MWP smoke: run apfs-fastindex-scan against the proof fixture and
diff its namespace + per-directory aggregate output against the same
mounted POSIX oracle the Python proof uses.

Gated by EX-18 (body-field parity), EX-19 (SR-017 logical-size precedence),
and EX-20 (SR-018 name preservation) — all green on the proof fixture.

Invoke as:

    PYTHONPATH=src python3 -m apfs_fastindex.rust_mwp_smoke

Returns 0 on `diff.matched`, 1 otherwise.
"""

from __future__ import annotations

import json
import subprocess
from dataclasses import asdict
from pathlib import Path

from .aggregate import build_directory_aggregates
from .models import (
    DirectoryAggregate,
    NamespaceEntry,
    ParserOutput,
    ScanState,
    SourceDescriptor,
)
from .oracle_diff import compare_parser_output_to_oracle
from .poc_fixture import build_proof_fixture

REPO_ROOT = Path(__file__).resolve().parents[2]
RUST_CRATE_DIR = REPO_ROOT / "crates" / "apfs-fastindex"


def _run_rust_scan(image_path: Path) -> dict:
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
            f"apfs-fastindex-scan failed (rc={proc.returncode})\n"
            f"stderr:\n{proc.stderr}\nstdout head:\n{proc.stdout[:2000]}"
        )
    return json.loads(proc.stdout)


def _rust_to_parser_output(rust_doc: dict) -> ParserOutput:
    parser_output = rust_doc["parser_output"]
    source = parser_output["source"]
    scan_state = parser_output["scan_state"]
    entries = tuple(
        NamespaceEntry(
            path=e["path"],
            entry_kind=e["entry_kind"],
            file_id=e["file_id"],
            logical_size=e.get("logical_size", 0),
            symlink_target=e.get("symlink_target"),
            allocated_size=e.get("allocated_size"),
        )
        for e in parser_output.get("entries", [])
    )
    aggregates = tuple(
        DirectoryAggregate(
            path=a["path"],
            unique_inode_logical_total=a["unique_inode_logical_total"],
            contributing_file_ids=tuple(a.get("contributing_file_ids", [])),
            unique_inode_allocated_total=a.get("unique_inode_allocated_total"),
        )
        for a in parser_output.get("aggregates", [])
    )
    return ParserOutput(
        source=SourceDescriptor(
            requested_path=Path(source["requested_path"]),
            raw_container_path=source["raw_container_path"],
            source_kind=source["source_kind"],
            allowlist_reason=source["allowlist_reason"],
        ),
        scan_state=ScanState(
            block_size=scan_state["block_size"],
            descriptor_blocks=scan_state["descriptor_blocks"],
            descriptor_base=scan_state["descriptor_base"],
            descriptor_base_non_contiguous=scan_state["descriptor_base_non_contiguous"],
            highest_xid=scan_state["highest_xid"],
            candidate_count=scan_state["candidate_count"],
        ),
        backend_name=parser_output["backend_name"],
        entries=entries,
        aggregates=aggregates,
    )


def _aggregate_diff(actual: tuple[DirectoryAggregate, ...],
                    expected: tuple[DirectoryAggregate, ...]) -> dict:
    actual_map = {a.path: a for a in actual}
    expected_map = {a.path: a for a in expected}
    missing = sorted(expected_map.keys() - actual_map.keys())
    extra = sorted(actual_map.keys() - expected_map.keys())
    mismatches = []
    for path in sorted(expected_map.keys() & actual_map.keys()):
        a = actual_map[path]
        e = expected_map[path]
        if a != e:
            mismatches.append(
                {
                    "path": path,
                    "expected_total": e.unique_inode_logical_total,
                    "actual_total": a.unique_inode_logical_total,
                    "expected_contributors": list(e.contributing_file_ids),
                    "actual_contributors": list(a.contributing_file_ids),
                }
            )
    return {
        "matched": not missing and not extra and not mismatches,
        "missing_paths": missing,
        "extra_paths": extra,
        "mismatches": mismatches,
    }


def main() -> int:
    with build_proof_fixture() as fixture:
        rust_doc = _run_rust_scan(fixture.image_path)
        parser_output = _rust_to_parser_output(rust_doc)
        entry_diff = compare_parser_output_to_oracle(parser_output, fixture.oracle_path)

        # Sanity-check aggregates: build the same aggregates from Rust's
        # entries via the Python aggregate.py (which is the SR-009 reference)
        # and diff them against the aggregates Rust emitted directly.
        expected_aggregates = build_directory_aggregates(parser_output.entries)
        aggregate_diff = _aggregate_diff(parser_output.aggregates, expected_aggregates)

        # SR-019 / EX-22 column smoke. The proof fixture contains
        # exactly one sparse file (`dst/sparse.bin`) and no decmpfs
        # files, so the expected per-row state is:
        #
        #   - dst/sparse.bin -> allocated_size is None (SR-019 step:
        #     regular + dstream + SPARSE_BYTES present -> fail closed)
        #   - every other regular file -> allocated_size is Some(int)
        #     and Some(int) > 0 for the non-empty fixture files
        #   - every symlink and directory -> allocated_size == 0
        #
        # And the expected per-aggregate state is:
        #
        #   - `.` and `dst` -> unique_inode_allocated_total is None
        #     (sparse.bin sits inside their subtrees and collapses
        #     the total by the SR-019 fail-closed contract)
        #   - `src` -> unique_inode_allocated_total is Some(int)
        #
        # Each list below records *unexpected* deviations from that
        # state; `allocated_column_ok` is true iff all four are
        # empty.
        EXPECTED_SPARSE_PATHS = {"dst/sparse.bin"}
        EXPECTED_AGGREGATE_NONE_PATHS = {".", "dst"}

        allocated_column_check: dict[str, list] = {
            "files_with_unexpected_none_allocated": [],
            "files_with_unexpected_some_allocated": [],
            "non_file_with_nonzero_allocated_size": [],
            "aggregate_with_unexpected_none_total": [],
            "aggregate_with_unexpected_some_total": [],
        }
        for entry in parser_output.entries:
            if entry.entry_kind == "file":
                expects_none = entry.path in EXPECTED_SPARSE_PATHS
                if expects_none and entry.allocated_size is not None:
                    allocated_column_check["files_with_unexpected_some_allocated"].append(
                        {"path": entry.path, "value": entry.allocated_size}
                    )
                elif not expects_none and entry.allocated_size is None:
                    allocated_column_check["files_with_unexpected_none_allocated"].append(
                        entry.path
                    )
            elif entry.entry_kind in {"dir", "symlink"}:
                if entry.allocated_size not in (0, None):
                    allocated_column_check["non_file_with_nonzero_allocated_size"].append(
                        {"path": entry.path, "value": entry.allocated_size}
                    )
        for agg in parser_output.aggregates:
            expects_none = agg.path in EXPECTED_AGGREGATE_NONE_PATHS
            if expects_none and agg.unique_inode_allocated_total is not None:
                allocated_column_check["aggregate_with_unexpected_some_total"].append(
                    {"path": agg.path, "value": agg.unique_inode_allocated_total}
                )
            elif not expects_none and agg.unique_inode_allocated_total is None:
                allocated_column_check["aggregate_with_unexpected_none_total"].append(
                    agg.path
                )

        allocated_column_ok = all(not v for v in allocated_column_check.values())

        report = {
            "fixture_image": str(fixture.image_path),
            "operations": list(fixture.operations),
            "rust_correctness_claim": rust_doc.get("correctness_claim"),
            "rust_not_claimed": list(rust_doc.get("not_claimed", [])),
            "rust_entry_count": len(parser_output.entries),
            "rust_aggregate_count": len(parser_output.aggregates),
            "entry_oracle_diff": {
                "matched": entry_diff.matched,
                "missing_paths": list(entry_diff.missing_paths),
                "unexpected_paths": list(entry_diff.unexpected_paths),
                "mismatches": [asdict(m) for m in entry_diff.mismatches],
            },
            "aggregate_python_vs_rust_diff": aggregate_diff,
            "allocated_column_check": allocated_column_check,
            "allocated_column_ok": allocated_column_ok,
        }
        print(json.dumps(report, indent=2, sort_keys=True))
        return (
            0
            if entry_diff.matched
            and aggregate_diff["matched"]
            and allocated_column_ok
            else 1
        )


if __name__ == "__main__":
    raise SystemExit(main())
