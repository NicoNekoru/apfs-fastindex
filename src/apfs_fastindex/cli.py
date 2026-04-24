from __future__ import annotations

import argparse
import json
import sys
from dataclasses import asdict

from .oracle_diff import compare_parser_output_to_oracle
from .parser import ParserSkeleton


def main(argv: list[str] | None = None) -> int:
    args = _build_parser().parse_args(argv)

    parser = ParserSkeleton()
    output = parser.parse(args.source)
    report = {
        "backend": output.backend_name,
        "source": asdict(output.source),
        "scan_state": asdict(output.scan_state),
        "entry_count": len(output.entries),
        "aggregate_count": len(output.aggregates),
    }

    if args.dump_entries:
        report["entries"] = [asdict(entry) for entry in output.entries]
        report["aggregates"] = [asdict(aggregate) for aggregate in output.aggregates]

    exit_code = 0
    if args.oracle:
        diff = compare_parser_output_to_oracle(output, args.oracle)
        report["oracle_diff"] = {
            "matched": diff.matched,
            "missing_paths": list(diff.missing_paths),
            "unexpected_paths": list(diff.unexpected_paths),
            "mismatches": [asdict(mismatch) for mismatch in diff.mismatches],
        }
        if not diff.matched:
            exit_code = 1

    json.dump(report, sys.stdout, indent=2, sort_keys=True)
    sys.stdout.write("\n")
    return exit_code


def _build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(description="Run the first APFS parser skeleton.")
    parser.add_argument("source", help="detached APFS .dmg or raw APFS container device")
    parser.add_argument("--oracle", help="path to an oracle.json file to diff against")
    parser.add_argument(
        "--dump-entries",
        action="store_true",
        help="include namespace entries and aggregates in the JSON output",
    )
    return parser
