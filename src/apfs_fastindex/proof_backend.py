from __future__ import annotations

import json
import subprocess
from pathlib import Path

from .models import NamespaceEntry, ScanState


class ProofBackendError(RuntimeError):
    pass


class ProofRawWalkBackend:
    name = "proof-rawwalk"

    def __init__(self, rawwalk_dir: Path | None = None) -> None:
        repo_root = Path(__file__).resolve().parents[2]
        self._rawwalk_dir = rawwalk_dir or (
            repo_root
            / "docs/research/experiments/EX-03-pinned-state-raw-vs-oracle/artifacts/rawwalk"
        )

    def collect_entries(
        self,
        raw_container_path: str,
        scan_state: ScanState,
    ) -> list[NamespaceEntry]:
        # This temporary backend keeps the skeleton runnable while native root and
        # FS-record modules are still being split out.
        del scan_state
        proc = subprocess.run(
            ["go", "run", ".", "--device", raw_container_path],
            cwd=self._rawwalk_dir,
            check=False,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        if proc.returncode != 0:
            raise ProofBackendError(
                f"proof backend failed for {raw_container_path}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
            )

        payload = json.loads(proc.stdout)
        entries: list[NamespaceEntry] = []
        for item in payload["entries"]:
            entry_kind = item["type"]
            if entry_kind not in {"dir", "file", "symlink"}:
                entry_kind = "other"
            entries.append(
                NamespaceEntry(
                    path=item["path"],
                    entry_kind=entry_kind,
                    file_id=int(item["file_id"]),
                    logical_size=int(item.get("logical_size", 0)),
                    symlink_target=item.get("symlink_target"),
                )
            )

        return entries
