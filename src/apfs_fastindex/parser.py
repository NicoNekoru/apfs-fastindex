from __future__ import annotations

from pathlib import Path

from .aggregate import build_directory_aggregates
from .models import ParserOutput
from .proof_backend import ProofRawWalkBackend
from .scan_state import pin_scan_state
from .source_gate import open_validated_source


class ParserSkeleton:
    def __init__(self, backend: ProofRawWalkBackend | None = None) -> None:
        self._backend = backend or ProofRawWalkBackend()

    def parse(self, source_path: str | Path) -> ParserOutput:
        with open_validated_source(source_path) as source:
            scan_state = pin_scan_state(source.raw_container_path)
            entries = tuple(self._backend.collect_entries(source.raw_container_path, scan_state))
            aggregates = build_directory_aggregates(entries)
            return ParserOutput(
                source=source,
                scan_state=scan_state,
                backend_name=self._backend.name,
                entries=entries,
                aggregates=aggregates,
            )
