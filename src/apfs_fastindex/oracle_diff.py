from __future__ import annotations

import json
from pathlib import Path

from .models import NamespaceEntry, OracleDiff, OracleMismatch, ParserOutput


def compare_parser_output_to_oracle(
    parser_output: ParserOutput,
    oracle_path: str | Path,
) -> OracleDiff:
    oracle_document = json.loads(Path(oracle_path).read_text())
    oracle_entries = oracle_document["entries"]

    expected = {
        entry["path"]: _normalize_oracle_entry(entry)
        for entry in oracle_entries
        if entry["path"] != "."
    }
    actual = {entry.path: _normalize_parser_entry(entry) for entry in parser_output.entries}

    missing_paths = tuple(sorted(path for path in expected if path not in actual))
    unexpected_paths = tuple(sorted(path for path in actual if path not in expected))

    mismatches = []
    for path in sorted(set(expected) & set(actual)):
        if expected[path] != actual[path]:
            mismatches.append(
                OracleMismatch(
                    path=path,
                    expected=expected[path],
                    actual=actual[path],
                )
            )

    return OracleDiff(
        matched=not missing_paths and not unexpected_paths and not mismatches,
        missing_paths=missing_paths,
        unexpected_paths=unexpected_paths,
        mismatches=tuple(mismatches),
    )


def _normalize_oracle_entry(entry: dict[str, object]) -> dict[str, object]:
    normalized = {
        "entry_kind": entry["type"],
        "file_id": entry["inode"],
    }
    if entry["type"] in {"file", "symlink"}:
        normalized["logical_size"] = entry["logical_size"]
    if entry["type"] == "symlink":
        normalized["symlink_target"] = entry["symlink_target"]
    return normalized


def _normalize_parser_entry(entry: NamespaceEntry) -> dict[str, object]:
    normalized = {
        "entry_kind": entry.entry_kind,
        "file_id": entry.file_id,
    }
    if entry.entry_kind in {"file", "symlink"}:
        normalized["logical_size"] = entry.logical_size
    if entry.entry_kind == "symlink":
        normalized["symlink_target"] = entry.symlink_target
    return normalized
