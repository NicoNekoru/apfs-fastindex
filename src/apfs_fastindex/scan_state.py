from __future__ import annotations

import struct

from .models import ScanState


NX_MAGIC = 0x4253584E


class ScanStateError(RuntimeError):
    pass


def pin_scan_state(raw_container_path: str) -> ScanState:
    with open(raw_container_path, "rb", buffering=0) as handle:
        block0 = handle.read(4096)
        if len(block0) < 4096:
            raise ScanStateError(f"short read from raw container: {raw_container_path}")

        block_size = struct.unpack_from("<I", block0, 0x24)[0]
        descriptor_blocks = struct.unpack_from("<I", block0, 0x68)[0]
        descriptor_base_raw = struct.unpack_from("<Q", block0, 0x70)[0]
        descriptor_base_non_contiguous = bool(descriptor_base_raw >> 63)
        descriptor_base = descriptor_base_raw & ((1 << 63) - 1)

        if descriptor_base_non_contiguous:
            raise ScanStateError("non-contiguous checkpoint descriptor layouts are outside the current allowlist")

        highest_xid = None
        candidate_count = 0
        for index in range(descriptor_blocks):
            handle.seek((descriptor_base + index) * block_size)
            block = handle.read(block_size)
            if len(block) < block_size:
                continue
            magic = struct.unpack_from("<I", block, 0x20)[0]
            if magic != NX_MAGIC:
                continue
            xid = struct.unpack_from("<Q", block, 0x10)[0]
            highest_xid = xid if highest_xid is None or xid > highest_xid else highest_xid
            candidate_count += 1

    if highest_xid is None:
        raise ScanStateError("no valid checkpoint candidates were found")

    return ScanState(
        block_size=block_size,
        descriptor_blocks=descriptor_blocks,
        descriptor_base=descriptor_base,
        descriptor_base_non_contiguous=descriptor_base_non_contiguous,
        highest_xid=highest_xid,
        candidate_count=candidate_count,
    )
