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
    pub dev_id: u32,
}

/// Read every entry in `dir` via `getattrlistbulk`. Returns the entries in
/// the order the kernel returned them (the caller sorts).
///
/// On any error this returns `Err`. The caller is expected to fall through
/// to `std::fs::read_dir` and continue.
#[cfg(target_os = "macos")]
pub(crate) fn read_directory_bulk(dir: &Path) -> io::Result<Vec<BulkEntry>> {
    use libc::c_void;

    const ATTR_CMN_ERROR: libc::attrgroup_t = 0x2000_0000;

    let c_path = CString::new(dir.as_os_str().as_bytes())
        .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
    // SAFETY: pointer is a valid NUL-terminated C string for the duration of
    // the call.
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
    alist.fileattr = libc::ATTR_FILE_TOTALSIZE;

    // 64 KiB buffer; in practice 16-32 entries per batch.
    let mut buf = vec![0u8; 65_536];
    let mut out: Vec<BulkEntry> = Vec::new();
    loop {
        // SAFETY: alist and buf are valid for the duration of the call;
        // the kernel writes at most `buf.len()` bytes into buf.
        let count = unsafe {
            libc::getattrlistbulk(
                fd,
                &mut alist as *mut _ as *mut c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                0,
            )
        };
        if count < 0 {
            return Err(io::Error::last_os_error());
        }
        if count == 0 {
            break;
        }
        let mut offset = 0usize;
        for _ in 0..count {
            let entry_len = read_u32_le(&buf, offset)? as usize;
            if entry_len < 24 || offset + entry_len > buf.len() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "getattrlistbulk returned malformed entry length",
                ));
            }
            let entry = &buf[offset..offset + entry_len];
            match parse_entry(entry) {
                Ok(Some(parsed)) => out.push(parsed),
                Ok(None) => {} // entry had an error; skip silently
                Err(err) => return Err(err),
            }
            offset += entry_len;
        }
    }
    Ok(out)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read_directory_bulk(_dir: &Path) -> io::Result<Vec<BulkEntry>> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "getattrlistbulk is macOS-only",
    ))
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
        // ATTR_FILE_TOTALSIZE only applies to regular files. Zero for
        // others; the caller treats symlink size as the target byte
        // length and gets that via readlink anyway.
        logical_size = 0;
    }

    Ok(Some(BulkEntry {
        name,
        kind,
        file_id,
        logical_size,
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

        let mut bulk = read_directory_bulk(root).expect("bulk reads");
        bulk.sort_by(|a, b| a.name.cmp(&b.name));

        let names: Vec<&str> = bulk.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["dir-a", "dir-b", "file-1.txt", "file-2.bin", "link.txt"]
        );

        let file1 = bulk.iter().find(|e| e.name == "file-1.txt").unwrap();
        assert!(matches!(file1.kind, EntryKind::File));
        assert_eq!(file1.logical_size, 5);

        let file2 = bulk.iter().find(|e| e.name == "file-2.bin").unwrap();
        assert!(matches!(file2.kind, EntryKind::File));
        assert_eq!(file2.logical_size, 1024);

        let link = bulk.iter().find(|e| e.name == "link.txt").unwrap();
        assert!(matches!(link.kind, EntryKind::Symlink));

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
        let err = read_directory_bulk(&f).expect_err("non-dir is rejected");
        // The exact errno varies (ENOTDIR / EISDIR depending on platform)
        // but either way it should be an io::Error.
        assert!(
            !err.to_string().is_empty(),
            "expected a real io::Error message"
        );
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
