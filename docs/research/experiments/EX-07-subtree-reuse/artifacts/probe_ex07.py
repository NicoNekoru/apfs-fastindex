#!/usr/bin/env python3
"""Run EX-07: subtree reuse proof probe.

The probe deliberately stays below a production incremental implementation. It
captures full raw state for each pinned image state, then simulates a reuse
decision by matching exact FS-tree node identity tuples across adjacent states.
"""

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
IDENTITY_DUMP_DIR = (
    ARTIFACT_DIR
    / "../../EX-06-identity-tracking/artifacts/identitydump"
).resolve()
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


def write_file(path: Path, text: str) -> None:
    path.write_text(text)


def build_initial_corpus(root: Path, fanout: int) -> dict:
    created = 0
    for dirname in ("stable-a", "stable-b", "hot", "moved"):
        directory = root / dirname
        directory.mkdir(exist_ok=True)
        sync_directory(directory)

    for dirname in ("stable-a", "stable-b", "hot"):
        directory = root / dirname
        for index in range(fanout):
            write_file(directory / f"{dirname}-{index:04d}.txt", f"{dirname} {index}\n")
            created += 1
        sync_directory(directory)

    for index in range(20):
        write_file(root / "moved" / f"seed-{index:04d}.txt", f"moved seed {index}\n")
        created += 1
    sync_directory(root / "moved")
    settle()
    return {"operation": "build initial separated subtree corpus", "created_files": created}


def mutate(root: Path, step: str, fanout: int) -> dict:
    hot = root / "hot"
    moved = root / "moved"
    if step == "baseline":
        return build_initial_corpus(root, fanout)
    if step == "append-hot":
        target = hot / "hot-0000.txt"
        with target.open("a") as handle:
            handle.write("append payload\n")
            handle.flush()
            os.fsync(handle.fileno())
            try:
                fcntl.fcntl(handle.fileno(), F_FULLFSYNC)
            except OSError:
                pass
        sync_directory(hot)
        settle()
        return {"operation": "append to hot/hot-0000.txt"}
    if step == "rename-hot":
        (hot / "hot-0001.txt").rename(hot / "hot-0001-renamed.txt")
        sync_directory(hot)
        settle()
        return {"operation": "rename hot/hot-0001.txt within hot"}
    if step == "move-hot":
        (hot / "hot-0002.txt").rename(moved / "hot-0002.txt")
        sync_directory(hot)
        sync_directory(moved)
        settle()
        return {"operation": "move hot/hot-0002.txt to moved/hot-0002.txt"}
    if step == "delete-recreate-hot":
        target = hot / "hot-0003.txt"
        target.unlink()
        write_file(target, "hot recreated 3\n")
        sync_file(target)
        sync_directory(hot)
        settle()
        return {"operation": "delete and recreate hot/hot-0003.txt"}
    if step == "add-hot-fanout":
        for index in range(fanout):
            write_file(hot / f"extra-{index:04d}.txt", f"extra {index}\n")
        sync_directory(hot)
        settle()
        return {"operation": "add extra hot fanout", "created_files": fanout}
    if step == "delete-hot-fanout":
        deleted = 0
        for index in range(fanout // 2):
            target = hot / f"extra-{index:04d}.txt"
            if target.exists():
                target.unlink()
                deleted += 1
        sync_directory(hot)
        settle()
        return {"operation": "delete half of extra hot fanout", "deleted_files": deleted}
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
    base = Path(tempfile.mkdtemp(prefix="apfsfi-ex07-", dir="/tmp"))
    image_path = base / "ex07-subtree-reuse.dmg"
    mountpoint = base / "mnt"
    mountpoint.mkdir()
    mounted_entities: list[dict] = []
    states: list[dict] = []
    fanout = 200
    steps = [
        "baseline",
        "append-hot",
        "rename-hot",
        "move-hot",
        "delete-recreate-hot",
        "add-hot-fanout",
        "delete-hot-fanout",
    ]

    try:
        run_checked(
            [
                "hdiutil",
                "create",
                "-size",
                "384m",
                "-fs",
                "APFS",
                "-volname",
                "EX07Reuse",
                "-nospotlight",
                str(image_path),
            ]
        )

        for index, step in enumerate(steps):
            attach_info = attach_image(image_path, mountpoint)
            mounted_entities = attach_info["system-entities"]
            mutation = mutate(mountpoint, step, fanout)
            oracle = snapshot_tree(mountpoint)
            detach_device(mounted_entities[0]["dev-entry"])
            mounted_entities = []

            state = capture_state(image_path, f"{index:02d}-{step}", mutation)
            state["mounted_oracle"] = oracle
            states.append(state)

        return {
            "image_name": image_path.name,
            "fanout_per_hot_or_stable_directory": fanout,
            "states": states,
        }
    finally:
        if mounted_entities:
            detach_device(mounted_entities[0]["dev-entry"])
        shutil.rmtree(base, ignore_errors=True)


def entry_digest(entries: list[dict], prefix: str) -> list[str]:
    selected = []
    for entry in entries:
        path = entry["path"]
        if path == prefix or path.startswith(prefix + "/"):
            selected.append(
                "|".join(
                    [
                        path,
                        entry["type"],
                        str(entry["file_id"]),
                        str(entry.get("logical_size", "")),
                        str(entry.get("symlink_target", "")),
                    ]
                )
            )
    return sorted(selected)


def node_index(state: dict) -> dict[str, dict]:
    return {
        summary["node_key"]: summary
        for summary in state["raw_identity"].get("node_summaries", [])
    }


def summarize(result: dict) -> dict:
    state_summaries = []
    transitions = []
    for state in result["states"]:
        raw = state["raw_identity"]
        state_summaries.append(
            {
                "label": state["label"],
                "checkpoint_xid": state["pinned_state"]["highest_xid"],
                "node_count": len(raw["nodes"]),
                "leaf_summary_count": sum(
                    1 for summary in raw.get("node_summaries", []) if summary["is_leaf"]
                ),
                "entry_count": len(raw["entries"]),
                "stable_a_digest_count": len(entry_digest(raw["entries"], "stable-a")),
                "stable_b_digest_count": len(entry_digest(raw["entries"], "stable-b")),
                "hot_digest_count": len(entry_digest(raw["entries"], "hot")),
            }
        )

    for previous, current in zip(result["states"], result["states"][1:]):
        previous_nodes = node_index(previous)
        current_nodes = node_index(current)
        reusable_keys = sorted(set(previous_nodes) & set(current_nodes))
        false_reuse = [
            key
            for key in reusable_keys
            if previous_nodes[key]["summary_hash"] != current_nodes[key]["summary_hash"]
        ]
        previous_raw = previous["raw_identity"]
        current_raw = current["raw_identity"]
        stable_a_unchanged = (
            entry_digest(previous_raw["entries"], "stable-a")
            == entry_digest(current_raw["entries"], "stable-a")
        )
        stable_b_unchanged = (
            entry_digest(previous_raw["entries"], "stable-b")
            == entry_digest(current_raw["entries"], "stable-b")
        )
        transitions.append(
            {
                "from": previous["label"],
                "to": current["label"],
                "previous_node_count": len(previous_nodes),
                "current_node_count": len(current_nodes),
                "reusable_node_count": len(reusable_keys),
                "reusable_leaf_count": sum(1 for key in reusable_keys if current_nodes[key]["is_leaf"]),
                "current_reuse_fraction": (
                    len(reusable_keys) / len(current_nodes) if current_nodes else 0.0
                ),
                "false_reuse_count": len(false_reuse),
                "false_reuse_node_keys": false_reuse,
                "stable_a_unchanged": stable_a_unchanged,
                "stable_b_unchanged": stable_b_unchanged,
            }
        )

    return {
        "state_count": len(result["states"]),
        "state_summaries": state_summaries,
        "transitions": transitions,
        "candidate_reuse_rule": (
            "reuse only when exact node_key matches; node_key includes OMAP domain, OID, "
            "object XID, paddr, checksum, type/subtype, and SHA-256 block hash"
        ),
        "any_false_reuse": any(item["false_reuse_count"] for item in transitions),
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
