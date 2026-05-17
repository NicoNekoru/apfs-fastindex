//! `getattrlistbulk(2)` backend for the POSIX fallback walker.
//!
//! `read_dir + symlink_metadata` does two syscalls per directory entry. On a
//! cold-cache scan of `/`, that's ~10M syscalls for 5M entries; the user
//! pays for every one of them in kernel transitions. macOS's
//! `getattrlistbulk` returns dozens to hundreds of entries per syscall with
//! the attributes we care about (name, kind, inode, size, device id) all
//! filled in by the kernel.
//!
//! Scope of this module:
//!
//! - macOS only. On other targets the public API errors out and the caller
//!   (`fallback.rs::walk_node`) falls back to `read_dir + lstat`.
//! - Read-only attribute fetch. We never set anything.
//! - Returns one `Vec<BulkEntry>` per call: the loop that drains a directory
//!   batch lives in this module so the caller stays simple.
//! - On any error (open failure, parse failure, kernel error mid-stream)
//!   the caller falls back to the std implementation. The bulk path is a
//!   perf optimization, not a correctness gate.
//!
//! ## Buffer reuse
//!
//! A whole-machine scan touches ~200k directories. Allocating a 64 KiB
//! kernel-fill buffer for each one is ~13 GiB of allocation churn. The
//! public surface here is a `BulkReader` that owns one buffer for the
//! lifetime of the walk; the caller calls `read_directory` per
//! directory and the buffer (plus the output `Vec`) are reused
//! between calls.

#![allow(non_upper_case_globals)]

use std::ffi::CString;
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use crate::EntryKind;

/// Per-entry result produced by the bulk backend. Mirrors the fields the
/// std backend extracts from `read_dir + symlink_metadata` so the two are
/// freely interchangeable from the walker's point of view.
#[derive(Debug, Clone)]
pub(crate) struct BulkEntry {
    pub name: String,
    pub kind: EntryKind,
    pub file_id: u64,
    pub logical_size: u64,
    /// Allocated bytes for the entry, i.e. `st_blocks * 512` semantics.
    /// Populated from `ATTR_FILE_ALLOCSIZE` (per-fork allocated bytes).
    /// EX-22 uses `st_blocks * 512` as the per-file allocated-bytes
    /// oracle on macOS; `ATTR_FILE_ALLOCSIZE` reports the same number
    /// in bytes (the kernel publishes it directly rather than as
    /// 512-byte units).
    pub allocated_bytes: u64,
    pub dev_id: u32,
}

/// One-shot bulk-attribute reader. Owns the 64 KiB kernel-fill buffer
/// (`vec![0u8; 65_536]` reallocated per directory was ~13 GiB of churn
/// on a 200k-directory `/` scan) and the per-call output `Vec`. Reuse
/// across directories by calling `read_directory(dir, &mut out)`
/// repeatedly with the same `out` Vec; `out` is cleared at the top of
/// each call.
pub(crate) struct BulkReader {
    #[cfg(target_os = "macos")]
    buf: Vec<u8>,
}

impl BulkReader {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            buf: vec![0u8; 65_536],
        }
    }

    /// Read every entry in `dir` via `getattrlistbulk`. Writes the
    /// entries into `out` in the order the kernel returned them; the
    /// caller sorts.
    ///
    /// On any error this returns `Err`. The caller is expected to
    /// fall through to `std::fs::read_dir` and continue. `out` is
    /// cleared at entry so a partial fill on the error path is
    /// discarded.
    #[cfg(target_os = "macos")]
    pub(crate) fn read_directory(
        &mut self,
        dir: &Path,
        out: &mut Vec<BulkEntry>,
    ) -> io::Result<()> {
        use libc::c_void;

        const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;

        out.clear();

        let c_path = CString::new(dir.as_os_str().as_bytes())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        // SAFETY: pointer is a valid NUL-terminated C string for the
        // duration of the call.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDONLY) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let _guard = FdGuard(fd);

        let mut alist: libc::attrlist = unsafe { std::mem::zeroed() };
        alist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        alist.commonattr = libc::ATTR_CMN_RETURNED_ATTRS
            | libc::ATTR_CMN_NAME
            | libc::ATTR_CMN_DEVID
            | libc::ATTR_CMN_OBJTYPE
            | libc::ATTR_CMN_FILEID
            | ATTR_CMN_ERROR;
        // ATTR_FILE_TOTALSIZE returns the logical size (matches `st_size`
        // for regular files); ATTR_FILE_ALLOCSIZE returns the allocated
        // size in bytes (matches `st_blocks * 512`). Both are file-only
        // attributes that come back zero for directories and symlinks;
        // the parser handles that below.
        alist.fileattr = libc::ATTR_FILE_TOTALSIZE | libc::ATTR_FILE_ALLOCSIZE;

        loop {
            // SAFETY: alist and self.buf are valid for the duration of
            // the call; the kernel writes at most `self.buf.len()`
            // bytes into self.buf.
            let count = unsafe {
                libc::getattrlistbulk(
                    fd,
                    &mut alist as *mut _ as *mut c_void,
                    self.buf.as_mut_ptr() as *mut c_void,
                    self.buf.len(),
                    0,
                )
            };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            if count == 0 {
                break;
            }
            let buf_len = self.buf.len();
            let mut offset = 0usize;
            for _ in 0..count {
                let entry_len = read_u32_le(&self.buf, offset)? as usize;
                if entry_len < 24 || offset + entry_len > buf_len {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "getattrlistbulk returned malformed entry length",
                    ));
                }
                let entry = &self.buf[offset..offset + entry_len];
                match parse_entry(entry) {
                    Ok(Some(parsed)) => out.push(parsed),
                    Ok(None) => {} // entry had an error; skip silently
                    Err(err) => return Err(err),
                }
                offset += entry_len;
            }
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn read_directory(
        &mut self,
        _dir: &Path,
        _out: &mut Vec<BulkEntry>,
    ) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "getattrlistbulk is macOS-only",
        ))
    }
}

#[cfg(target_os = "macos")]
fn parse_entry(entry: &[u8]) -> io::Result<Option<BulkEntry>> {
    // Bit values for the attributes we requested. Order in the per-entry
    // payload is: returned_attrs (always first), then attrs in ascending
    // bit-value order within each attrgroup.
    const ATTR_CMN_NAME: u32 = 0x0000_0001;
    const ATTR_CMN_DEVID: u32 = 0x0000_0002;
    const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
    const ATTR_CMN_FILEID: u32 = 0x0200_0000;
    const ATTR_CMN_ERROR: u32 = 0x2000_0000;
    const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
    const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;

    // Skip the leading u32 entry length and read the 20-byte returned
    // attribute_set_t. attribute_set_t is five u32s: commonattr, volattr,
    // dirattr, fileattr, forkattr.
    let mut cursor = 4usize;
    let returned_common = read_u32_le(entry, cursor)?;
    let _returned_vol = read_u32_le(entry, cursor + 4)?;
    let _returned_dir = read_u32_le(entry, cursor + 8)?;
    let returned_file = read_u32_le(entry, cursor + 12)?;
    let _returned_fork = read_u32_le(entry, cursor + 16)?;
    cursor += 20;

    // Common-attr block: NAME, DEVID, OBJTYPE, FILEID, ERROR — in bit order.
    let mut name: Option<String> = None;
    let mut dev_id: u32 = 0;
    let mut obj_type: u32 = 0;
    let mut file_id: u64 = 0;
    let mut had_error = false;

    if returned_common & ATTR_CMN_NAME != 0 {
        let attrref_base = cursor;
        let data_offset = read_i32_le(entry, cursor)? as isize;
        let length = read_u32_le(entry, cursor + 4)? as usize;
        cursor += 8;
        let start = (attrref_base as isize)
            .checked_add(data_offset)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "name attrreference offset overflow",
                )
            })? as usize;
        // `length` includes the trailing NUL.
        if length == 0 || start + length > entry.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "name attrreference points outside entry buffer",
            ));
        }
        let bytes = &entry[start..start + length];
        let trimmed = if bytes.last() == Some(&0) {
            &bytes[..length - 1]
        } else {
            bytes
        };
        let parsed = std::str::from_utf8(trimmed).map_err(|err| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!("entry name is not valid UTF-8: {err}"),
            )
        })?;
        name = Some(parsed.to_string());
    }

    if returned_common & ATTR_CMN_DEVID != 0 {
        dev_id = read_u32_le(entry, cursor)?;
        cursor += 4;
    }

    if returned_common & ATTR_CMN_OBJTYPE != 0 {
        obj_type = read_u32_le(entry, cursor)?;
        cursor += 4;
    }

    if returned_common & ATTR_CMN_FILEID != 0 {
        // u64 is naturally 8-byte aligned in the buffer; pad as needed.
        cursor = (cursor + 7) & !7;
        file_id = read_u64_le(entry, cursor)?;
        cursor += 8;
    }

    if returned_common & ATTR_CMN_ERROR != 0 {
        let err = read_u32_le(entry, cursor)?;
        cursor += 4;
        if err != 0 {
            had_error = true;
        }
    }

    let mut logical_size: u64 = 0;
    if returned_file & ATTR_FILE_TOTALSIZE != 0 {
        cursor = (cursor + 7) & !7;
        logical_size = read_u64_le(entry, cursor)?;
        cursor += 8;
    }
    let mut allocated_bytes: u64 = 0;
    if returned_file & ATTR_FILE_ALLOCSIZE != 0 {
        cursor = (cursor + 7) & !7;
        allocated_bytes = read_u64_le(entry, cursor)?;
    }

    if had_error {
        return Ok(None);
    }
    let name = match name {
        Some(n) => n,
        None => return Ok(None),
    };
    let kind = entry_kind_from_obj_type(obj_type);
    if !matches!(kind, EntryKind::File) {
        // ATTR_FILE_TOTALSIZE / ATTR_FILE_ALLOCSIZE only apply to
        // regular files. Zero for others; the caller treats symlink
        // size as the target byte length and gets that via readlink
        // anyway. The fallback walker maps both symlink and dir
        // allocated_size to Some(0) for shape parity with raw mode.
        logical_size = 0;
        allocated_bytes = 0;
    }

    Ok(Some(BulkEntry {
        name,
        kind,
        file_id,
        logical_size,
        allocated_bytes,
        dev_id,
    }))
}

#[cfg(target_os = "macos")]
fn entry_kind_from_obj_type(obj_type: u32) -> EntryKind {
    // fsobj_type_t values from <sys/vnode.h>
    const VREG: u32 = 1;
    const VDIR: u32 = 2;
    const VLNK: u32 = 5;
    match obj_type {
        VREG => EntryKind::File,
        VDIR => EntryKind::Dir,
        VLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    }
}

fn read_u32_le(buf: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset.checked_add(4).ok_or_else(short)?;
    if end > buf.len() {
        return Err(short());
    }
    Ok(u32::from_le_bytes(buf[offset..end].try_into().unwrap()))
}

#[cfg(target_os = "macos")]
fn read_i32_le(buf: &[u8], offset: usize) -> io::Result<i32> {
    let end = offset.checked_add(4).ok_or_else(short)?;
    if end > buf.len() {
        return Err(short());
    }
    Ok(i32::from_le_bytes(buf[offset..end].try_into().unwrap()))
}

#[cfg(target_os = "macos")]
fn read_u64_le(buf: &[u8], offset: usize) -> io::Result<u64> {
    let end = offset.checked_add(8).ok_or_else(short)?;
    if end > buf.len() {
        return Err(short());
    }
    Ok(u64::from_le_bytes(buf[offset..end].try_into().unwrap()))
}

fn short() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "getattrlistbulk entry truncated",
    )
}

#[cfg(target_os = "macos")]
struct FdGuard(i32);

#[cfg(target_os = "macos")]
impl Drop for FdGuard {
    fn drop(&mut self) {
        // SAFETY: fd was returned by `open` and not yet closed.
        unsafe {
            libc::close(self.0);
        }
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    #[test]
    fn bulk_read_returns_the_same_entries_as_read_dir() {
        let tmp = TempDir::new();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("dir-a")).unwrap();
        std::fs::create_dir_all(root.join("dir-b")).unwrap();
        {
            let mut f = File::create(root.join("file-1.txt")).unwrap();
            f.write_all(b"hello").unwrap();
        }
        {
            let mut f = File::create(root.join("file-2.bin")).unwrap();
            f.write_all(&vec![0u8; 1024]).unwrap();
        }
        symlink("file-1.txt", root.join("link.txt")).unwrap();

        let mut reader = BulkReader::new();
        let mut bulk: Vec<BulkEntry> = Vec::new();
        reader.read_directory(root, &mut bulk).expect("bulk reads");
        bulk.sort_by(|a, b| a.name.cmp(&b.name));

        let names: Vec<&str> = bulk.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["dir-a", "dir-b", "file-1.txt", "file-2.bin", "link.txt"]
        );

        let file1 = bulk.iter().find(|e| e.name == "file-1.txt").unwrap();
        assert!(matches!(file1.kind, EntryKind::File));
        assert_eq!(file1.logical_size, 5);
        // EX-22 oracle: ATTR_FILE_ALLOCSIZE returns the per-file
        // allocated bytes (== st_blocks * 512). For any non-empty
        // file the kernel will have allocated at least one block, so
        // the value should be > 0 and >= logical_size for these tiny
        // non-sparse files.
        assert!(
            file1.allocated_bytes >= file1.logical_size,
            "expected allocated_bytes >= logical_size for non-sparse file-1.txt; \
             got allocated={} logical={}",
            file1.allocated_bytes,
            file1.logical_size,
        );

        let file2 = bulk.iter().find(|e| e.name == "file-2.bin").unwrap();
        assert!(matches!(file2.kind, EntryKind::File));
        assert_eq!(file2.logical_size, 1024);
        assert!(file2.allocated_bytes >= file2.logical_size);

        let link = bulk.iter().find(|e| e.name == "link.txt").unwrap();
        assert!(matches!(link.kind, EntryKind::Symlink));
        // Non-file kinds get zero allocated_bytes from the bulk
        // parser; the fallback walker then maps that to Some(0) for
        // shape parity with raw mode.
        assert_eq!(link.allocated_bytes, 0);

        let dir_a = bulk.iter().find(|e| e.name == "dir-a").unwrap();
        assert!(matches!(dir_a.kind, EntryKind::Dir));
        assert_eq!(dir_a.allocated_bytes, 0);

        for entry in &bulk {
            // POSIX inodes are non-zero on real filesystems; this is a
            // basic sanity check that FILEID came back populated.
            assert!(entry.file_id > 0, "missing file_id for {}", entry.name);
            assert!(entry.dev_id > 0, "missing dev_id for {}", entry.name);
        }
    }

    #[test]
    fn bulk_read_errors_on_non_directory() {
        let tmp = TempDir::new();
        let f = tmp.path().join("a-file");
        File::create(&f).unwrap();
        let mut reader = BulkReader::new();
        let mut out: Vec<BulkEntry> = Vec::new();
        let err = reader
            .read_directory(&f, &mut out)
            .expect_err("non-dir is rejected");
        // The exact errno varies (ENOTDIR / EISDIR depending on platform)
        // but either way it should be an io::Error.
        assert!(
            !err.to_string().is_empty(),
            "expected a real io::Error message"
        );
    }

    #[test]
    fn bulk_reader_reuses_buffer_across_directories() {
        // Two siblings; reusing the reader's buffer must not bleed
        // results from the first directory into the second.
        let tmp = TempDir::new();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("dir-a")).unwrap();
        std::fs::create_dir_all(root.join("dir-b")).unwrap();
        for name in ["alpha.txt", "beta.txt"] {
            let mut f = File::create(root.join("dir-a").join(name)).unwrap();
            f.write_all(b"a").unwrap();
        }
        {
            let mut f = File::create(root.join("dir-b").join("gamma.bin")).unwrap();
            f.write_all(b"b").unwrap();
        }

        let mut reader = BulkReader::new();
        let mut out: Vec<BulkEntry> = Vec::new();

        reader
            .read_directory(&root.join("dir-a"), &mut out)
            .expect("read dir-a");
        let dir_a_names: Vec<String> = out.iter().map(|e| e.name.clone()).collect();

        reader
            .read_directory(&root.join("dir-b"), &mut out)
            .expect("read dir-b");
        let dir_b_names: Vec<String> = out.iter().map(|e| e.name.clone()).collect();

        let mut sorted_a = dir_a_names.clone();
        sorted_a.sort();
        assert_eq!(sorted_a, vec!["alpha.txt", "beta.txt"]);
        assert_eq!(dir_b_names, vec!["gamma.bin"]);
    }

    /// Reuses the same tiny in-tree TempDir helper from fallback.rs::tests.
    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static COUNTER: AtomicU64 = AtomicU64::new(0);
            let pid = std::process::id();
            let seq = COUNTER.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir().join(format!("apfsfi-bulk-test-{pid}-{seq}"));
            std::fs::create_dir_all(&path).expect("create temp dir");
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }
}
