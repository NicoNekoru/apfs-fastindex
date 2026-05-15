"""POSIX-traversal fallback path for the v1 narrow parser.

Emits the same `NamespaceEntry` + `DirectoryAggregate` shape as the Rust
raw scanner. Used when raw mode is rejected per the v1 spec's
fall-closed boundary, and as the Gate-2-enabler skeleton from EX-21.

Today this walks via `os.walk` + `os.lstat` + `os.readlink`. A future
pass can swap in `getattrlistbulk` for performance; the public contract
does not change.

Support-matrix cell covered: locally mounted directory (detached APFS
image, or any other POSIX-mountable source where raw mode is rejected).
Gate-2 cells (live boot, encrypted runtime, snapshot-assisted, boot-root
merged) are NOT covered.
"""

from __future__ import annotations

import os
import stat
from pathlib import Path

from .aggregate import build_directory_aggregates
from .models import DirectoryAggregate, EntryKind, NamespaceEntry


_SKIP_TOP_LEVEL_NAMES = frozenset({".fseventsd", ".Spotlight-V100", ".Trashes"})


def _entry_kind_for(mode: int) -> EntryKind:
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "file"
    return "other"


def traverse_mounted_directory(root: Path) -> tuple[tuple[NamespaceEntry, ...],
                                                    tuple[DirectoryAggregate, ...]]:
    """Walk a mounted directory and emit (entries, aggregates).

    The root itself is not emitted as a `NamespaceEntry` (the contract
    excludes `.`), but it is the implicit parent of the per-directory
    aggregate keyed by `.`.
    """
    root = root.resolve()
    entries: list[NamespaceEntry] = []
    for current_root, dirnames, filenames in os.walk(root, followlinks=False):
        dirnames.sort()
        filenames.sort()
        current_path = Path(current_root)
        rel_root = current_path.relative_to(root)
        # Strip macOS-private top-level metadata directories from product
        # output the same way the raw-mode walker does.
        if rel_root.parts and rel_root.parts[0] in _SKIP_TOP_LEVEL_NAMES:
            dirnames[:] = []
            continue
        dirnames[:] = [name for name in dirnames if name not in _SKIP_TOP_LEVEL_NAMES]

        # Emit directories first so the output order is deterministic and
        # the aggregate builder sees each directory ancestor before its
        # files. (The aggregate builder does not depend on order, but
        # keeping it stable keeps oracle diffs readable.)
        for name in dirnames:
            path = current_path / name
            try:
                st = os.lstat(path)
            except OSError:
                continue
            rel = path.relative_to(root)
            entries.append(
                NamespaceEntry(
                    path=str(rel),
                    entry_kind=_entry_kind_for(st.st_mode),
                    file_id=int(st.st_ino),
                    logical_size=0,
                    symlink_target=None,
                )
            )
        for name in filenames:
            path = current_path / name
            try:
                st = os.lstat(path)
            except OSError:
                continue
            rel = path.relative_to(root)
            kind = _entry_kind_for(st.st_mode)
            symlink_target = None
            logical_size = int(st.st_size)
            if kind == "symlink":
                try:
                    target = os.readlink(path)
                except OSError:
                    target = ""
                symlink_target = target
                logical_size = len(target.encode("utf-8"))
            entries.append(
                NamespaceEntry(
                    path=str(rel),
                    entry_kind=kind,
                    file_id=int(st.st_ino),
                    logical_size=logical_size,
                    symlink_target=symlink_target,
                )
            )

    entries.sort(key=lambda entry: entry.path)
    entries_tuple = tuple(entries)
    aggregates = build_directory_aggregates(entries_tuple)
    return entries_tuple, aggregates


__all__ = ["traverse_mounted_directory"]
