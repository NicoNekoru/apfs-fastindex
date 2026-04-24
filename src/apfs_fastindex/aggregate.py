from __future__ import annotations

from pathlib import PurePosixPath

from .models import DirectoryAggregate, NamespaceEntry


def build_directory_aggregates(entries: tuple[NamespaceEntry, ...]) -> tuple[DirectoryAggregate, ...]:
    directory_paths = {entry.path for entry in entries if entry.entry_kind == "dir"}
    directory_paths.add(".")
    contributors: dict[str, dict[int, int]] = {path: {} for path in directory_paths}

    for entry in entries:
        if entry.entry_kind != "file":
            continue
        for directory_path in _parent_directories(entry.path):
            if directory_path not in contributors:
                continue
            contributors[directory_path].setdefault(entry.file_id, entry.logical_size)

    aggregates = [
        DirectoryAggregate(
            path=path,
            unique_inode_logical_total=sum(file_sizes.values()),
            contributing_file_ids=tuple(sorted(file_sizes)),
        )
        for path, file_sizes in sorted(contributors.items())
    ]
    return tuple(aggregates)


def _parent_directories(path: str) -> tuple[str, ...]:
    parents = [str(parent) for parent in PurePosixPath(path).parents]
    normalized = []
    for parent in parents:
        normalized.append("." if parent in {"", "."} else parent)
    return tuple(normalized)
