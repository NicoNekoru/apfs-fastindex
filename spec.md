# **APFS High-Performance Filesystem Indexing (WizTree-like)**

## Design / Technical Spec

---

## **1. Problem Statement**

We want to build a **high-performance disk indexing tool** (WizTree-like) for APFS that can:

- Scan very large filesystems (millions of files)
- Produce a full directory tree with aggregate sizes
- Run repeatedly with **minimal incremental cost**
- Maintain **correctness and completeness**

### Challenge

Unlike NTFS, APFS does **not expose a flat metadata table** (e.g., MFT). Instead:

- Metadata is stored across **multiple copy-on-write B-trees**
- Filesystem state is represented as an **object graph (via OIDs)**
- Traversal requires **tree walking + object resolution**

This prevents a simple, linear, single-pass scan.

---

## **2. Motivation**

Existing approaches on macOS:

- System APIs (`readdir`, `getattrlistbulk`)
	- Stable, supported
	- But still require full traversal each run

Goal:

> Achieve **near-WizTree performance on repeat scans** by exploiting APFS’s structural properties.

Key observation:

- APFS is **copy-on-write (CoW)** and **transactional**
- Unchanged data retains **identical object IDs (OIDs) and block addresses**

This enables **incremental traversal via structural caching**

---

## **3. Key Insight**

APFS behaves like a **persistent tree structure**:

- Changes rewrite only:
	- Modified leaf nodes
	- Their ancestor nodes up to the root
- Unchanged subtrees remain **bitwise identical**

Therefore:

> If a B-tree node is unchanged, its entire subtree is unchanged.

---

## **4. System Model**

### Core Structures

- **Object Map (OMAP)**
  Maps `OID -> physical block address`

- **FS Tree (B-tree)**
  Contains:
	- Inodes
	- Directory entries (name -> inode)
	- Extents

- **Checkpoint / Transaction (XID)**
  Represents a consistent snapshot of the filesystem

---

## **5. High-Level Pipeline**

### Full Scan (baseline)

```
read container superblock
-> resolve latest checkpoint (XID)
-> initialize OMAP resolver
-> load FS tree root
-> traverse FS B-tree
    -> extract inodes + dir entries
-> reconstruct directory hierarchy
-> compute aggregates (sizes)
-> persist cache state
```

---

## **6. Incremental Scan Strategy**

### Stored State

Persist across runs:

- `last_scan_xid`
- `node_cache` (B-tree nodes)
- `inode_cache`
- `dir_cache`

---

### Incremental Algorithm

```
current_xid = get_latest_xid()

if current_xid == last_scan_xid:
    return cached results

walk FS tree:
    for each node:
        if node unchanged (same OID + block addr):
            reuse cached subtree
            skip traversal
        else:
            recurse into children

update affected inodes and directories only

persist updated caches
```

---

## **7. Cache Design**

### 7.1 Node Cache (critical)

Keyed by OID:

```
node_cache[oid] = {
    block_addr,
    parsed_records,
    subtree_summary (optional),
}
```

Purpose:

- Skip entire subtrees if unchanged

---

### 7.2 Inode Cache

```
inode_cache[oid] = {
    size,
    metadata,
    last_seen_xid,
}
```

---

### 7.3 Directory Cache

```
dir_cache[parent_oid] = [
    (name, child_oid)
]
```

---

### 7.4 Optional: Subtree Hash

```
node_hash = hash(records + child_oids)
```

Used to:

- Validate reuse beyond block address comparison

---

## **8. Traversal Model**

### Storage-level traversal

- **B-tree walk (DFS)**
- Driven by node structure, not directory hierarchy

### Logical traversal (post-processing)

- Build directory tree from:
	- inode map
	- directory entry map

---

## **9. Correctness Guarantees**

To ensure exactness:

- Always read from a **single checkpoint (XID)**
- Never mix objects across transactions
- Invalidate cache if:
	- Checkpoint chain is inconsistent
	- Volume was not cleanly unmounted
- Treat unchanged node ⇒ unchanged subtree (guaranteed by CoW)

---

## **10. Performance Characteristics**

### Initial scan

- Comparable to full B-tree traversal
- Slower than NTFS MFT scan due to:
	- Non-linear reads
	- Object indirection

### Incremental scans

- Complexity:
  ```
  O(changed_nodes × log N)
  ```
- Typically:
	- Very fast if filesystem churn is low
	- Skips majority of tree

---

## **11. Limitations / Non-Goals**

- No direct equivalent to NTFS linear scan
- Raw APFS parsing:
	- Complex
	- Sensitive to format changes
- Snapshot handling:
	- Out of scope initially (optional extension)

---

## **12. Future Enhancements**

- Parallel subtree traversal
- Persistent on-disk cache (memory-mapped)
- Snapshot-aware indexing
- Extent-level dedup accounting
- Hybrid mode (API + raw parsing fallback)
