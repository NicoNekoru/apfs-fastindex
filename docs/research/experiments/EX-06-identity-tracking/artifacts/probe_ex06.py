#!/usr/bin/env python3
"""Run EX-06: OID, paddr, XID, checksum identity tracking."""

from __future__ import annotations

import fcntl
import json
import os
import plistlib
import shutil
import stat
import struct
import subprocess
import tempfile
import time
from pathlib import Path


APFS_CONTAINER_HINT = "EF57347C-0000-11AA-AA11-00306543ECAC"
F_FULLFSYNC = 51
NX_MAGIC = 0x4253584E
ARTIFACT_DIR = Path(__file__).resolve().parent
GENERATED_DIR = ARTIFACT_DIR / "generated"
IDENTITY_DUMP_DIR = ARTIFACT_DIR / "identitydump"
GENERATED_DIR.mkdir(exist_ok=True)


def run(cmd: list[str], input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    return subprocess.run(
        cmd,
        input=input_text,
        text=True,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        check=False,
    )


def run_checked(cmd: list[str], input_text: str | None = None) -> subprocess.CompletedProcess[str]:
    proc = run(cmd, input_text=input_text)
    if proc.returncode != 0:
        raise RuntimeError(
            f"command failed: {' '.join(cmd)}\nstdout:\n{proc.stdout}\nstderr:\n{proc.stderr}"
        )
    return proc


def write_json(name: str, payload: object) -> None:
    (GENERATED_DIR / name).write_text(json.dumps(payload, indent=2, sort_keys=True) + "\n")


def attach_image(image_path: Path, mountpoint: Path) -> dict:
    proc = run_checked(
        ["hdiutil", "attach", "-plist", "-mountpoint", str(mountpoint), str(image_path)]
    )
    return plistlib.loads(proc.stdout.encode("utf-8"))


def attach_nomount_image(image_path: Path) -> dict:
    proc = run_checked(["hdiutil", "attach", "-plist", "-nomount", str(image_path)])
    return plistlib.loads(proc.stdout.encode("utf-8"))


def detach_device(dev_entry: str) -> None:
    run(["hdiutil", "detach", dev_entry])


def sync_file(path: Path) -> None:
    with path.open("ab") as handle:
        handle.flush()
        os.fsync(handle.fileno())
        try:
            fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
        except OSError:
            pass


def sync_directory(path: Path) -> None:
    dirfd = os.open(path, os.O_RDONLY)
    try:
        os.fsync(dirfd)
    finally:
        os.close(dirfd)


def settle() -> None:
    run(["sync"])
    time.sleep(0.15)


def latest_checkpoint_info(raw_container_path: str) -> dict:
    with open(raw_container_path, "rb", buffering=0) as handle:
        block0 = handle.read(4096)
        block_size = struct.unpack_from("<I", block0, 0x24)[0]
        desc_blocks = struct.unpack_from("<I", block0, 0x68)[0]
        desc_base_raw = struct.unpack_from("<Q", block0, 0x70)[0]
        desc_base = desc_base_raw & ((1 << 63) - 1)
        non_contiguous = bool(desc_base_raw >> 63)

        candidates = []
        highest_xid = None
        for index in range(desc_blocks):
            handle.seek((desc_base + index) * block_size)
            block = handle.read(block_size)
            if len(block) < block_size:
                continue
            magic = struct.unpack_from("<I", block, 0x20)[0]
            if magic != NX_MAGIC:
                continue
            xid = struct.unpack_from("<Q", block, 0x10)[0]
            candidates.append(
                {
                    "descriptor_index": index,
                    "xid": xid,
                    "obj_type": struct.unpack_from("<I", block, 0x18)[0],
                }
            )
            highest_xid = xid if highest_xid is None or xid > highest_xid else highest_xid

    return {
        "block_size": block_size,
        "descriptor_blocks": desc_blocks,
        "descriptor_base": desc_base,
        "descriptor_base_non_contiguous": non_contiguous,
        "highest_xid": highest_xid,
        "candidate_count": len(candidates),
        "candidates": candidates,
    }


def entry_type(path: Path, st: os.stat_result) -> str:
    mode = st.st_mode
    if stat.S_ISDIR(mode):
        return "dir"
    if stat.S_ISLNK(mode):
        return "symlink"
    if stat.S_ISREG(mode):
        return "file"
    if stat.S_ISFIFO(mode):
        return "other(DT_FIFO)"
    return "other(DT_UNKNOWN)"


def snapshot_tree(root: Path) -> list[dict]:
    entries: list[dict] = []
    for current_root, dirnames, filenames in os.walk(root):
        dirnames.sort()
        filenames.sort()
        rel_root = Path(current_root).relative_to(root)
        if str(rel_root).startswith(".fseventsd"):
            continue
        dirnames[:] = [name for name in dirnames if name != ".fseventsd"]
        current_path = Path(current_root)
        current_stat = os.lstat(current_path)
        entries.append(
            {
                "type": "dir",
                "path": "." if str(rel_root) == "." else str(rel_root),
                "inode": current_stat.st_ino,
                "nlink": current_stat.st_nlink,
            }
        )
        for name in filenames:
            path = current_path / name
            rel_path = path.relative_to(root)
            st = os.lstat(path)
            kind = entry_type(path, st)
            entry = {
                "type": kind,
                "path": str(rel_path),
                "inode": st.st_ino,
                "nlink": st.st_nlink,
            }
            if kind == "symlink":
                target = os.readlink(path)
                entry["logical_size"] = len(target)
                entry["symlink_target"] = target
            elif kind == "file":
                entry["logical_size"] = st.st_size
                entry["allocated_bytes"] = st.st_blocks * 512
            entries.append(entry)
    return entries


def run_identitydump(raw_container_path: str) -> dict:
    proc = run_checked(["go", "run", ".", "--device", raw_container_path])
    return json.loads(proc.stdout)


def mutate(root: Path, step: str) -> dict:
    work = root / "work"
    work.mkdir(exist_ok=True)
    if step == "initial-empty":
        sync_directory(root)
        settle()
        return {"operation": "initial empty state"}
    if step == "create-alpha":
        alpha = work / "alpha.txt"
        alpha.write_text("alpha\n")
        sync_file(alpha)
        sync_directory(work)
        settle()
        return {"operation": "create work/alpha.txt"}
    if step == "append-alpha":
        alpha = work / "alpha.txt"
        with alpha.open("a") as handle:
            handle.write("beta\n")
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(work)
        settle()
        return {"operation": "append to work/alpha.txt"}
    if step == "rename-alpha":
        (work / "alpha.txt").rename(work / "renamed.txt")
        sync_directory(work)
        settle()
        return {"operation": "rename work/alpha.txt -> work/renamed.txt"}
    if step == "create-beta":
        beta = work / "beta.txt"
        beta.write_text("beta\n")
        sync_file(beta)
        sync_directory(work)
        settle()
        return {"operation": "create work/beta.txt"}
    if step == "delete-beta":
        (work / "beta.txt").unlink()
        sync_directory(work)
        settle()
        return {"operation": "delete work/beta.txt"}
    if step == "recreate-beta":
        beta = work / "beta.txt"
        beta.write_text("beta recreated\n")
        sync_file(beta)
        sync_directory(work)
        settle()
        return {"operation": "recreate work/beta.txt"}
    if step == "truncate-renamed":
        renamed = work / "renamed.txt"
        with renamed.open("r+b") as handle:
            handle.truncate(3)
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(work)
        settle()
        return {"operation": "truncate work/renamed.txt"}
    raise ValueError(step)


def capture_state(image_path: Path, label: str, mutation: dict) -> dict:
    nomount_entities: list[dict] = []
    try:
        nomount_info = attach_nomount_image(image_path)
        nomount_entities = nomount_info["system-entities"]
        container_dev = next(
            entity["dev-entry"]
            for entity in nomount_entities
            if entity.get("content-hint") == APFS_CONTAINER_HINT
        )
        raw_container_path = "/dev/r" + container_dev.split("/dev/")[1]
        checkpoint = latest_checkpoint_info(raw_container_path)
        raw_identity = run_identitydump(raw_container_path)
        return {
            "label": label,
            "mutation": mutation,
            "pinned_state": checkpoint,
            "raw_identity": raw_identity,
            "nomount_entities": nomount_entities,
        }
    finally:
        if nomount_entities:
            detach_device(nomount_entities[0]["dev-entry"])


def run_experiment() -> dict:
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex06-", dir="/tmp"))
    image_path = base / "ex06-identity.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_entities: list[dict] = []
    states: list[dict] = []
    steps = [
        "initial-empty",
        "create-alpha",
        "append-alpha",
        "rename-alpha",
        "create-beta",
        "delete-beta",
        "recreate-beta",
        "truncate-renamed",
    ]

    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "192m",
                "-fs",
                "APFS",
                "-volname",
                "EX06Identity",
                "-nospotlight",
                str(image_path),
            ]
        )

        for index, step in enumerate(steps):
            attach_info = attach_image(image_path, mountpoint)
            mounted_entities = attach_info["system-entities"]
            mutation = mutate(mountpoint, step)
            oracle = snapshot_tree(mountpoint)
            detach_device(mounted_entities[0]["dev-entry"])
            mounted_entities = []

            state = capture_state(image_path, f"{index:02d}-{step}", mutation)
            state["mounted_oracle"] = oracle
            states.append(state)

        return {"image_name": image_path.name, "states": states}
    finally:
        if mounted_entities:
            detach_device(mounted_entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)


def summarize(result: dict) -> dict:
    root_identities = []
    path_histories: dict[str, list[dict]] = {}
    for state in result["states"]:
        label = state["label"]
        raw = state["raw_identity"]
        root = raw["root_tree"]
        root_identities.append(
            {
                "label": label,
                "checkpoint_xid": state["pinned_state"]["highest_xid"],
                "root_oid": root["oid"],
                "root_object_xid": root["object_xid"],
                "root_paddr": root["paddr"],
                "root_checksum": root["checksum"],
                "root_content_hash": root["content_hash"],
                "node_count": len(raw["nodes"]),
            }
        )
        for entry in raw["entries"]:
            path_histories.setdefault(entry["path"], []).append(
                {
                    "label": label,
                    "file_id": entry["file_id"],
                    "type": entry["type"],
                    "logical_size": entry.get("logical_size"),
                }
            )

    root_changes = []
    previous = None
    for current in root_identities:
        if previous is not None:
            root_changes.append(
                {
                    "from": previous["label"],
                    "to": current["label"],
                    "same_oid": previous["root_oid"] == current["root_oid"],
                    "same_paddr": previous["root_paddr"] == current["root_paddr"],
                    "same_object_xid": previous["root_object_xid"] == current["root_object_xid"],
                    "same_checksum": previous["root_checksum"] == current["root_checksum"],
                    "same_content_hash": previous["root_content_hash"] == current["root_content_hash"],
                }
            )
        previous = current

    return {
        "state_count": len(result["states"]),
        "root_identities": root_identities,
        "root_changes": root_changes,
        "path_histories": path_histories,
    }


def main() -> None:
    environment = {
        "sw_vers": run_checked(["sw_vers"]).stdout,
        "go_version": run_checked(["go", "version"]).stdout.strip(),
        "identitydump_module": {
            "path": str(IDENTITY_DUMP_DIR),
            "go_mod": (IDENTITY_DUMP_DIR / "go.mod").read_text(),
        },
    }
    write_json("environment.json", environment)

    cwd = os.getcwd()
    os.chdir(IDENTITY_DUMP_DIR)
    try:
        run_checked(["go", "mod", "tidy"])
        result = run_experiment()
    finally:
        os.chdir(cwd)

    for state in result["states"]:
        write_json(f"{state['label']}.json", state)
    summary = summarize(result)
    write_json("summary.json", summary)
    write_json("run.json", result)


if __name__ == "__main__":
    main()
