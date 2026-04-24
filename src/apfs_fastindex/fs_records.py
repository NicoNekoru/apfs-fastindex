from __future__ import annotations

from typing import Protocol

from .models import ScanState


class FSRecordReader(Protocol):
    def read_records(self, raw_container_path: str, scan_state: ScanState, file_id: int) -> object:
        ...


class NativeFSRecordReader:
    def read_records(self, raw_container_path: str, scan_state: ScanState, file_id: int) -> object:
        raise NotImplementedError(
            "native FS-record reading is intentionally deferred; the first skeleton keeps this "
            "boundary explicit while the proof backend supplies namespace entries end-to-end"
        )
