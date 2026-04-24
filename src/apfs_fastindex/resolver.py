from __future__ import annotations

from typing import Protocol

from .models import ResolvedRoots, ScanState


class RootResolver(Protocol):
    def resolve_roots(self, raw_container_path: str, scan_state: ScanState) -> ResolvedRoots:
        ...


class NativeRootResolver:
    def resolve_roots(self, raw_container_path: str, scan_state: ScanState) -> ResolvedRoots:
        raise NotImplementedError(
            "native root resolution is intentionally not wired yet; the first skeleton uses "
            "checkpoint pinning plus a proof backend while this module boundary stays explicit"
        )
