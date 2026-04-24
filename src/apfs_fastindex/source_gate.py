from __future__ import annotations

import os
import plistlib
import subprocess
from contextlib import contextmanager
from pathlib import Path
from typing import Iterator

from .models import SourceDescriptor


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"


class SourceGateError(RuntimeError):
    pass


def _run_checked(cmd: list[str]) -> subprocess.CompletedProcess[str]:
    proc = subprocess.run(
        cmd,
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    if proc.returncode != 0:
        raise SourceGateError(
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def _detach(device: str) -> None:
    subprocess.run(
        ["hdiutil", "detach", device],
        check=False,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _normalize_raw_device(device: str) -> str:
    if device.startswith("/dev/rdisk"):
        return device
    if device.startswith("/dev/disk"):
        return "/dev/r" + device.split("/dev/", 1)[1]
    raise SourceGateError(f"unsupported raw device path: {device}")


@contextmanager
def open_validated_source(source_path: str | Path) -> Iterator[SourceDescriptor]:
    path = Path(source_path)

    if str(path).startswith("/dev/"):
        raw_container_path = _normalize_raw_device(str(path))
        if not os.path.exists(raw_container_path):
            raise SourceGateError(f"raw device does not exist: {raw_container_path}")
        yield SourceDescriptor(
            requested_path=path,
            raw_container_path=raw_container_path,
            source_kind="raw_device",
            allowlist_reason="caller-supplied raw APFS container device",
        )
        return

    if not path.exists():
        raise SourceGateError(f"source path does not exist: {path}")

    if path.suffix.lower() != ".dmg":
        raise SourceGateError(
            "only detached APFS .dmg images or raw APFS container devices are in the current allowlist"
        )

    proc = _run_checked(["hdiutil", "attach", "-plist", "-nomount", str(path)])
    attach_info = plistlib.loads(proc.stdout.encode("utf-8"))
    entities = attach_info.get("system-entities", [])
    detach_target = entities[0]["dev-entry"] if entities else None
    try:
        container_device = next(
            (
                entity["dev-entry"]
                for entity in entities
                if entity.get("content-hint") == APFS_CONTAINER_HINT
            ),
            None,
        )
        if not container_device:
            raise SourceGateError(
                f"image does not expose a simple APFS container in the current allowlist: {path}"
            )
        yield SourceDescriptor(
            requested_path=path,
            raw_container_path=_normalize_raw_device(container_device),
            source_kind="dmg_image",
            allowlist_reason="detached image-backed APFS container",
        )
    finally:
        if detach_target:
            _detach(detach_target)
