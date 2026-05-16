from __future__ import annotations

from pathlib import PurePosixPath

from .models import DirectoryAggregate, NamespaceEntry


def build_directory_aggregates(entries: tuple[NamespaceEntry, ...]) -> tuple[DirectoryAggregate, ...]:
    directory_paths = {entry.path for entry in entries if entry.entry_kind == "dir"}
    directory_paths.add(".")
    contributors: dict[str, dict[int, tuple[int, int | None]]] = {path: {} for path in directory_paths}

    for entry in entries:
        if entry.entry_kind != "file":
            continue
        for directory_path in _parent_directories(entry.path):
            if directory_path not in contributors:
                continue
            contributors[directory_path].setdefault(
                entry.file_id, (entry.logical_size, entry.allocated_size)
            )

    aggregates = []
    for path, file_sizes in sorted(contributors.items()):
        logical_total = sum(logical for logical, _ in file_sizes.values())
        allocated_values = [allocated for _, allocated in file_sizes.values()]
        allocated_total: int | None
        if any(a is None for a in allocated_values):
            allocated_total = None
        else:
            allocated_total = sum(a for a in allocated_values if a is not None)
        aggregates.append(
            DirectoryAggregate(
                path=path,
                unique_inode_logical_total=logical_total,
                contributing_file_ids=tuple(sorted(file_sizes)),
                unique_inode_allocated_total=allocated_total,
            )
        )
    return tuple(aggregates)


def _parent_directories(path: str) -> tuple[str, ...]:
    parents = [str(parent) for parent in PurePosixPath(path).parents]
    normalized = []
    for parent in parents:
        normalized.append("." if parent in {"", "."} else parent)
    return tuple(normalized)
