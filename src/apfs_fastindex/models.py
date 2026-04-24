from __future__ import annotations

from dataclasses import dataclass
from pathlib import Path
from typing import Literal


EntryKind = Literal["dir", "file", "symlink", "other"]


@dataclass(frozen=True)
class SourceDescriptor:
    requested_path: Path
    raw_container_path: str
    source_kind: str
    allowlist_reason: str


@dataclass(frozen=True)
class ScanState:
    block_size: int
    descriptor_blocks: int
    descriptor_base: int
    descriptor_base_non_contiguous: bool
    highest_xid: int
    candidate_count: int


@dataclass(frozen=True)
class ResolvedObject:
    omap_context: str
    oid: int
    paddr: int
    xid: int
    object_type: str
    object_subtype: str


@dataclass(frozen=True)
class ResolvedRoots:
    selected_volume_oid: int
    container_omap_oid: int
    volume_omap_oid: int
    fs_root_oid: int


@dataclass(frozen=True)
class NamespaceEntry:
    path: str
    entry_kind: EntryKind
    file_id: int
    logical_size: int = 0
    symlink_target: str | None = None


@dataclass(frozen=True)
class DirectoryAggregate:
    path: str
    unique_inode_logical_total: int
    contributing_file_ids: tuple[int, ...]


@dataclass(frozen=True)
class ParserOutput:
    source: SourceDescriptor
    scan_state: ScanState
    backend_name: str
    entries: tuple[NamespaceEntry, ...]
    aggregates: tuple[DirectoryAggregate, ...]


@dataclass(frozen=True)
class OracleMismatch:
    path: str
    expected: dict[str, object]
    actual: dict[str, object]


@dataclass(frozen=True)
class OracleDiff:
    matched: bool
    missing_paths: tuple[str, ...]
    unexpected_paths: tuple[str, ...]
    mismatches: tuple[OracleMismatch, ...]
