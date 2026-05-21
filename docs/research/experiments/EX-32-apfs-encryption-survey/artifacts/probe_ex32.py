#!/usr/bin/env python3
"""EX-32 Phase B: enumerate the host's APFS volume encryption state.

Public APIs only — `diskutil apfs list -plist` + `diskutil info
-plist`. No root, no decryption, no read of /dev/disk*. Just the
encryption metadata Apple exposes for any user.

Saves `artifacts/generated/ex32_host_state_<date>.json` with:

- Container layout (UUID, physical store, capacity).
- Per-volume encryption flags: encrypted yes/no, FileVault
  enabled, locked vs unlocked, encryption-rolled state if
  exposed.
- Volume keybag location is **not** exposed by diskutil; that
  field lives in the apfs_superblock_t which we'd need raw
  block reads for (EX-28-blocked on /dev/disk3s* under SIP).
  Documented absence is itself a data point.
- Cross-reference with `fdesetup status` for FileVault state
  reported by the userland tool (sometimes diverges from
  what's on disk during a rollover).

Usage (from repo root):
  python3 docs/research/experiments/EX-32-apfs-encryption-survey/\
artifacts/probe_ex32.py
"""

from __future__ import annotations

import datetime as _dt
import json
import platform
import plistlib
import subprocess
import sys
from pathlib import Path

ARTIFACT_DIR = Path(__file__).resolve().parent
GEN_DIR = ARTIFACT_DIR / "generated"
GEN_DIR.mkdir(parents=True, exist_ok=True)


def _check_output(cmd: list[str]) -> bytes:
    return subprocess.check_output(cmd, stderr=subprocess.PIPE)


def _diskutil_apfs() -> dict:
    return plistlib.loads(_check_output(["diskutil", "apfs", "list", "-plist"]))


def _diskutil_info(device: str) -> dict:
    return plistlib.loads(_check_output(["diskutil", "info", "-plist", device]))


# Well-known crypto-user UUIDs Apple uses across all Macs.
# These map to specific BAG_TYPE_UNLOCK_RECORDS slots in the
# on-disk keybag and are documented across diskutil's own
# `diskutil apfs listCryptoUsers` output + community RE work.
WELL_KNOWN_CRYPTO_USER_UUIDS = {
    "64C0C6EB-0000-11AA-AA11-00306543ECAC": "iCloud Recovery External Key",
    "EBC6C064-0000-11AA-AA11-00306543ECAC": "Personal Recovery User",
    # iCloud Recovery User (escrow) uses a per-host UUID, not
    # a constant. Local Open Directory User likewise.
}


def _diskutil_list_crypto_users(device: str) -> list[dict]:
    """`diskutil apfs listCryptoUsers <device>` returns the user
    list as plain text. The crypto-user UUID + type are the
    fields we want; the printed format is:

      Cryptographic users for disk3s5 (4 found)
      |
      +-- <UUID>
      |   Type: <text>
      |   Volume Owner: Yes/No
    """
    try:
        out = subprocess.check_output(
            ["diskutil", "apfs", "listCryptoUsers", device],
            stderr=subprocess.PIPE,
            timeout=10,
        ).decode("utf-8", errors="replace")
    except subprocess.CalledProcessError:
        return []
    entries: list[dict] = []
    current: dict | None = None
    for raw in out.splitlines():
        line = raw.strip(" |+-")
        if not line:
            continue
        if line.count("-") == 4 and len(line) == 36:
            # UUID line.
            if current:
                entries.append(current)
            current = {"uuid": line.upper()}
            note = WELL_KNOWN_CRYPTO_USER_UUIDS.get(current["uuid"])
            if note:
                current["well_known_role"] = note
        elif line.startswith("Type:") and current is not None:
            current["type"] = line[len("Type:"):].strip()
        elif line.startswith("Volume Owner:") and current is not None:
            current["volume_owner"] = line[len("Volume Owner:"):].strip()
    if current:
        entries.append(current)
    return entries


def _fdesetup_status() -> str:
    """`fdesetup status` reports the FileVault state of the boot
    data volume. It's a userland summary; the on-disk truth is
    in apfs_superblock_t.fs_flags. Capture both."""
    try:
        out = subprocess.check_output(
            ["fdesetup", "status"],
            stderr=subprocess.PIPE,
            timeout=10,
        )
        return out.decode("utf-8", errors="replace").strip()
    except Exception as e:
        return f"(fdesetup error: {e})"


def _classify_volume(vol: dict) -> dict:
    """Pull the encryption-relevant fields from a single
    diskutil apfs volume entry."""
    return {
        "device": vol.get("DeviceIdentifier"),
        "volume_uuid": vol.get("APFSVolumeUUID"),
        "name": vol.get("Name"),
        "role": vol.get("Roles", []),
        "mount_point": vol.get("MountPoint", ""),
        # The fields that matter:
        "encrypted": vol.get("Encryption"),  # bool
        "filevault": vol.get("FileVault"),  # bool
        "locked": vol.get("Locked"),  # bool — false if unlocked
        "encryption_progress": vol.get("EncryptionProgressPercent"),
        "encryption_rolling_state": vol.get("EncryptionRollingState"),
        # CryptoUsers maps to BAG_TYPE_UNLOCK_RECORDS entries.
        # Each user is a UUID; the username isn't on disk, just the
        # wrapping key derived from that user's password.
        "crypto_users": vol.get("CryptoUsers"),
        "personal_recovery_key": vol.get("APFSPersonalRecoveryKey", None),
        "institutional_recovery_key": vol.get("InstitutionalRecoveryKey", None),
        "capacity_in_use": vol.get("CapacityInUse"),
    }


def _container_summary(container: dict) -> dict:
    return {
        "container_uuid": container.get("APFSContainerUUID"),
        "container_ref": container.get("ContainerReference"),
        "physical_stores": [
            {"device_id": ps.get("DeviceIdentifier"), "size": ps.get("Size")}
            for ps in container.get("PhysicalStores", [])
        ],
        "capacity_ceiling": container.get("CapacityCeiling"),
        "capacity_free": container.get("CapacityFree"),
        "fusion": container.get("Fusion", False),
        "volumes": [_classify_volume(v) for v in container.get("Volumes", [])],
    }


def _per_volume_info(device_id: str) -> dict:
    """Augment with `diskutil info -plist` fields not in
    `diskutil apfs list`, including the on-disk crypto info if
    diskutil chose to surface it."""
    try:
        info = _diskutil_info(device_id)
    except subprocess.CalledProcessError:
        return {}
    return {
        "writable": info.get("Writable"),
        "readonly_media": info.get("ReadOnlyMedia"),
        "smart_status": info.get("SMARTStatus"),
        "encrypted": info.get("Encryption"),
        "filevault_master": info.get("FileVaultMasterPassword"),
        # The boot-system flag tells us this is the sealed
        # system snapshot — not encrypted in the usual sense
        # (the sealed system uses an integrity hash instead).
        "boot_snapshot": info.get("APFSSnapshot", False),
        "snapshot_name": info.get("APFSSnapshotName"),
    }


def _summarise_encryption_landscape(record: dict) -> dict:
    """Roll up the per-volume data into a high-level summary
    so the next reader can answer 'how does this host's
    encryption look' without parsing the full plist tree."""
    counts: dict = {
        "total_volumes": 0,
        "encrypted": 0,
        "filevault": 0,
        "locked": 0,
        "boot_snapshot": 0,
        "rolling": 0,
    }
    volumes_of_interest: list[dict] = []
    for container in record["apfs_containers"]:
        for v in container["volumes"]:
            counts["total_volumes"] += 1
            if v.get("encrypted"):
                counts["encrypted"] += 1
            if v.get("filevault"):
                counts["filevault"] += 1
            if v.get("locked"):
                counts["locked"] += 1
            if v.get("encryption_rolling_state"):
                counts["rolling"] += 1
            extra = record["per_volume_info"].get(v["device"], {})
            if extra.get("boot_snapshot"):
                counts["boot_snapshot"] += 1
            # Hold onto the high-signal volumes — boot data,
            # FileVaulted, or rolling — for the executive summary.
            if (v.get("filevault")
                    or v.get("encrypted")
                    or v.get("encryption_rolling_state")
                    or "Data" in v.get("role", [])):
                volumes_of_interest.append({
                    "device": v["device"],
                    "name": v["name"],
                    "role": v["role"],
                    "encrypted": v.get("encrypted"),
                    "filevault": v.get("filevault"),
                    "locked": v.get("locked"),
                    "rolling_state": v.get("encryption_rolling_state"),
                })
    return {"counts": counts, "volumes_of_interest": volumes_of_interest}


def main() -> int:
    apfs = _diskutil_apfs()
    containers = [_container_summary(c) for c in apfs.get("Containers", [])]

    per_vol: dict = {}
    crypto_users: dict = {}
    for c in containers:
        for v in c["volumes"]:
            if v["device"]:
                per_vol[v["device"]] = _per_volume_info(v["device"])
                if v.get("encrypted"):
                    # Only query crypto users for encrypted
                    # volumes — diskutil errors out on
                    # unencrypted volumes.
                    crypto_users[v["device"]] = _diskutil_list_crypto_users(v["device"])

    fdesetup = _fdesetup_status()

    record = {
        "experiment": "EX-32",
        "title": "APFS encryption host state survey",
        "date": _dt.date.today().isoformat(),
        "host": {
            "platform": platform.platform(),
            "kernel": platform.release(),
        },
        "apfs_containers": containers,
        "per_volume_info": per_vol,
        "crypto_users_per_volume": crypto_users,
        "fdesetup_status": fdesetup,
        "notes": [
            "Volume keybag location (apfs_keybag_loc in the "
            "volume superblock) is not exposed by diskutil. "
            "Reading it requires a raw read of the volume's "
            "checkpoint blocks, which is EX-28-blocked on the "
            "live data volume under SIP. A detached encrypted "
            ".dmg could be read raw, but we don't have one in "
            "the fixture set.",
            "diskutil's `Encryption: bool` reflects the on-disk "
            "fs_flags ~APFS_FS_UNENCRYPTED state. During an "
            "encryption rollover (FileVault being turned on for "
            "the first time, or a key rotation in progress) the "
            "EncryptionRollingState field is populated.",
            "Apple silicon Macs always have Data Protection on "
            "the data volume even if FileVault is off — the "
            "class keys are wrapped by Secure Enclave hardware "
            "keys. FileVault adds a user-password-derived "
            "wrapping layer on top.",
        ],
        "references_archived": [
            "Apple File System Reference (2020) - "
            "https://developer.apple.com/support/downloads/"
            "Apple-File-System-Reference.pdf",
            "Apple Platform Security (Dec 2024) - "
            "https://help.apple.com/pdf/security/en-us/"
            "apple-platform-security-guide.pdf",
            "apfs-fuse - https://github.com/sgan81/apfs-fuse",
            "linux-apfs-rw - https://github.com/linux-apfs/linux-apfs-rw",
        ],
    }
    record["summary"] = _summarise_encryption_landscape(record)

    out_file = GEN_DIR / f"ex32_host_state_{_dt.date.today().isoformat()}.json"
    out_file.write_text(json.dumps(record, indent=2, default=str))

    s = record["summary"]
    print("=== EX-32 host encryption state ===")
    print(f"  Host:                  {record['host']['platform']}")
    print(f"  APFS containers:       {len(containers)}")
    print(f"  Volumes total:         {s['counts']['total_volumes']}")
    print(f"    encrypted:           {s['counts']['encrypted']}")
    print(f"    FileVault:           {s['counts']['filevault']}")
    print(f"    locked:              {s['counts']['locked']}")
    print(f"    boot snapshot:       {s['counts']['boot_snapshot']}")
    print(f"    rolling:             {s['counts']['rolling']}")
    print(f"  fdesetup status:       {fdesetup}")
    print(f"  Volumes of interest:")
    for v in s["volumes_of_interest"]:
        print(f"    {v['device']:10} {','.join(v['role']) or '(no role)':10} "
              f"name={v['name']!r:25} "
              f"fv={v.get('filevault')} enc={v.get('encrypted')} "
              f"locked={v.get('locked')}")
    if crypto_users:
        print(f"  Keybag unlock records (per encrypted volume):")
        for dev, users in crypto_users.items():
            print(f"    {dev}: {len(users)} entries")
            for u in users:
                role = u.get("well_known_role") or u.get("type") or "(?)"
                print(f"      {u['uuid']}  {role}")
    print(f"  Output: {out_file}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
