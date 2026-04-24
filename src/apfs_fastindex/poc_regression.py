from __future__ import annotations

import json
from dataclasses import asdict

from .oracle_diff import compare_parser_output_to_oracle
from .parser import ParserSkeleton
from .poc_fixture import build_proof_fixture


def main() -> int:
    parser = ParserSkeleton()
    with build_proof_fixture() as fixture:
        output = parser.parse(fixture.image_path)
        diff = compare_parser_output_to_oracle(output, fixture.oracle_path)
        report = {
            "fixture_image": str(fixture.image_path),
            "operations": list(fixture.operations),
            "entry_count": len(output.entries),
            "aggregate_count": len(output.aggregates),
            "scan_state": asdict(output.scan_state),
            "oracle_diff": {
                "matched": diff.matched,
                "missing_paths": list(diff.missing_paths),
                "unexpected_paths": list(diff.unexpected_paths),
                "mismatches": [asdict(mismatch) for mismatch in diff.mismatches],
            },
        }
        print(json.dumps(report, indent=2, sort_keys=True))
        return 0 if diff.matched else 1


if __name__ == "__main__":
    raise SystemExit(main())
