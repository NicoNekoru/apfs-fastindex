//! `getattrlistbulk(2)` backend for the POSIX fallback walker.
//!
//! `read_dir + symlink_metadata` needs a metadata syscall for each entry,
//! in addition to batched directory reads. macOS's `getattrlistbulk` returns
//! multiple entries per syscall with
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
use std::fs::File;
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
    /// Populated from `ATTR_FILE_ALLOCSIZE`, including every fork.
    /// EX-22 uses `st_blocks * 512` as the per-file allocated-bytes
    /// oracle on macOS; `ATTR_FILE_ALLOCSIZE` reports the same number
    /// in bytes (the kernel publishes it directly rather than as
    /// 512-byte units).
    pub allocated_bytes: u64,
    /// Darwin dev_t is signed. Match MetadataExt::dev() when widening it.
    pub dev_id: u64,
    /// Bulk attributes describe the underlying mountpoint or firmlink.
    /// The walker must refresh directory metadata through the visible path
    /// before deciding mount boundaries and inode deduplication.
    pub directory_needs_refresh: bool,
}

/// Buffer size for the `getattrlistbulk` per-directory fill. The
/// kernel returns as many directory entries as fit. Larger
/// buffers reduce syscall count on big dirs but reserve more memory per
/// worker. Override at runtime via `APFS_BULK_BUF_KIB`, from 4 to 1024.
/// Invalid values use the default, preventing unbounded allocations and
/// overflow in the KiB conversion.
const DEFAULT_BULK_BUF_BYTES: usize = 65_536;
const MIN_BULK_BUF_BYTES: usize = 4_096;
const MAX_BULK_BUF_BYTES: usize = 1_048_576;

fn configured_bulk_buf_bytes(value: Option<&str>) -> usize {
    value
        .and_then(|s| s.parse::<usize>().ok())
        .and_then(|kib| kib.checked_mul(1024))
        .filter(|bytes| (MIN_BULK_BUF_BYTES..=MAX_BULK_BUF_BYTES).contains(bytes))
        .unwrap_or(DEFAULT_BULK_BUF_BYTES)
}

fn bulk_buf_bytes() -> usize {
    // Read once and cache; an env var lookup per `BulkReader::new`
    // would otherwise cost a few hundred ns per worker startup.
    use std::sync::OnceLock;
    static CACHED: OnceLock<usize> = OnceLock::new();
    *CACHED.get_or_init(|| {
        configured_bulk_buf_bytes(std::env::var("APFS_BULK_BUF_KIB").ok().as_deref())
    })
}

/// One-shot bulk-attribute reader. Owns the kernel-fill buffer
/// (`vec![0u8; N]` reallocated per directory was ~13 GiB of churn
/// on a 200k-directory `/` scan) and the per-call output `Vec`.
/// Reuse across directories by calling `read_directory(dir,
/// &mut out)` repeatedly with the same `out` Vec; `out` is
/// cleared at the top of each call.
pub(crate) struct BulkReader {
    #[cfg(target_os = "macos")]
    buf: Vec<u8>,
}

impl BulkReader {
    pub(crate) fn new() -> Self {
        Self {
            #[cfg(target_os = "macos")]
            buf: vec![0u8; bulk_buf_bytes()],
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
    #[cfg_attr(not(test), allow(dead_code))] // Path wrapper is retained for fixtures.
    pub(crate) fn read_directory(
        &mut self,
        dir: &Path,
        out: &mut Vec<BulkEntry>,
    ) -> io::Result<()> {
        out.clear();

        let c_path = CString::new(dir.as_os_str().as_bytes())
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidInput, err))?;
        // SAFETY: pointer is a valid NUL-terminated C string for the
        // duration of the call.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        use std::os::fd::FromRawFd;
        // SAFETY: this newly opened descriptor has exactly one owner.
        let directory = unsafe { File::from_raw_fd(fd) };
        self.read_directory_fd(&directory, out)
    }

    /// Enumerate the caller's pinned directory and consume its file offset.
    /// This borrows the File and never closes it. A later std/readdir fallback
    /// must openat(fd, ".") to get an independent open-file description.
    /// Cloning the File would share offsets and mix incompatible iterators.
    #[cfg(target_os = "macos")]
    pub(crate) fn read_directory_fd(
        &mut self,
        directory: &File,
        out: &mut Vec<BulkEntry>,
    ) -> io::Result<()> {
        use std::os::fd::AsRawFd;

        out.clear();
        let result = self.read_open_directory(directory.as_raw_fd(), out);
        if result.is_err() {
            out.clear();
        }
        result
    }

    #[cfg(target_os = "macos")]
    fn read_open_directory(&mut self, fd: i32, out: &mut Vec<BulkEntry>) -> io::Result<()> {
        use libc::c_void;

        let mut alist: libc::attrlist = unsafe { std::mem::zeroed() };
        alist.bitmapcount = libc::ATTR_BIT_MAP_COUNT;
        alist.commonattr = REQUESTED_COMMON;
        alist.dirattr = libc::ATTR_DIR_MOUNTSTATUS;
        // st_size is the data-fork length. TOTALSIZE also includes resource
        // forks and therefore disagrees with the std metadata fallback.
        // st_blocks includes all forks, so retain all-fork ALLOCSIZE.
        alist.fileattr = REQUESTED_FILE;

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
            decode_batch(&self.buf, count as usize, out)?;
        }
        Ok(())
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn read_directory(
        &mut self,
        _dir: &Path,
        out: &mut Vec<BulkEntry>,
    ) -> io::Result<()> {
        out.clear();
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "getattrlistbulk is macOS-only",
        ))
    }

    #[cfg(not(target_os = "macos"))]
    pub(crate) fn read_directory_fd(
        &mut self,
        _directory: &File,
        out: &mut Vec<BulkEntry>,
    ) -> io::Result<()> {
        out.clear();
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "getattrlistbulk is macOS-only",
        ))
    }
}

#[cfg(target_os = "macos")]
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
#[cfg(target_os = "macos")]
const SF_FIRMLINK: u32 = 0x0080_0000;
#[cfg(target_os = "macos")]
const REQUIRED_COMMON: u32 = libc::ATTR_CMN_RETURNED_ATTRS
    | libc::ATTR_CMN_NAME
    | libc::ATTR_CMN_DEVID
    | libc::ATTR_CMN_OBJTYPE
    | libc::ATTR_CMN_FILEID;
#[cfg(target_os = "macos")]
const REQUESTED_COMMON: u32 = REQUIRED_COMMON | libc::ATTR_CMN_FLAGS | ATTR_CMN_ERROR;
#[cfg(target_os = "macos")]
const REQUESTED_FILE: u32 = libc::ATTR_FILE_ALLOCSIZE | libc::ATTR_FILE_DATALENGTH;

#[cfg(target_os = "macos")]
fn decode_batch(buf: &[u8], count: usize, out: &mut Vec<BulkEntry>) -> io::Result<()> {
    let result = (|| {
        if count > buf.len() / 24 {
            return Err(invalid("bulk record count exceeds buffer capacity"));
        }
        let mut offset = 0usize;
        for _ in 0..count {
            // Record starts are eight-byte aligned. Attribute fields inside
            // each record are only four-byte aligned, including u64 fields.
            if !offset.is_multiple_of(8) {
                return Err(invalid("bulk record start is not eight-byte aligned"));
            }
            let len = read_u32_le(buf, offset)? as usize;
            let end = offset.checked_add(len).ok_or_else(short)?;
            if len < 24 || !len.is_multiple_of(4) || end > buf.len() {
                return Err(invalid("getattrlistbulk returned malformed entry length"));
            }
            out.push(parse_entry(&buf[offset..end])?);
            offset = end;
        }
        Ok(())
    })();
    if result.is_err() {
        // Never expose a partial batch or earlier batches from a failed read.
        out.clear();
    }
    result
}

#[cfg(target_os = "macos")]
fn parse_entry(entry: &[u8]) -> io::Result<BulkEntry> {
    if entry.len() < 24
        || !entry.len().is_multiple_of(4)
        || read_u32_le(entry, 0)? as usize != entry.len()
    {
        return Err(invalid("bulk record length disagrees with its buffer"));
    }
    let returned_common = read_u32_le(entry, 4)?;
    let returned_vol = read_u32_le(entry, 8)?;
    let returned_dir = read_u32_le(entry, 12)?;
    let returned_file = read_u32_le(entry, 16)?;
    let returned_fork = read_u32_le(entry, 20)?;
    if returned_common & libc::ATTR_CMN_RETURNED_ATTRS == 0 {
        return Err(invalid("bulk record omitted RETURNED_ATTRS"));
    }

    let mut cursor = Cursor {
        buf: entry,
        offset: 24,
    };
    // ERROR follows RETURNED_ATTRS, before NAME. Successful native rows may
    // omit ERROR. An error row must trigger std fallback, not lose its name.
    if returned_common & ATTR_CMN_ERROR != 0 {
        let error = cursor.u32()?;
        if error != 0 {
            let error = i32::try_from(error)
                .map_err(|_| invalid("bulk record contains an invalid errno"))?;
            return Err(io::Error::from_raw_os_error(error));
        }
    }
    if returned_common & !REQUESTED_COMMON != 0
        || returned_vol != 0
        || returned_dir & !libc::ATTR_DIR_MOUNTSTATUS != 0
        || returned_file & !REQUESTED_FILE != 0
        || returned_fork != 0
    {
        return Err(invalid("bulk record returned unrequested attributes"));
    }
    if returned_common & REQUIRED_COMMON != REQUIRED_COMMON {
        return Err(invalid("bulk record omitted mandatory identity attributes"));
    }

    let name_ref_base = cursor.offset;
    let name_offset = cursor.u32()? as i32;
    let name_length = cursor.u32()? as usize;
    let dev_id = (cursor.u32()? as i32) as u64;
    let kind = entry_kind_from_obj_type(cursor.u32()?)?;
    let flags = if returned_common & libc::ATTR_CMN_FLAGS != 0 {
        Some(cursor.u32()?)
    } else {
        None
    };
    let file_id = cursor.u64()?;
    if file_id == 0 {
        return Err(invalid("bulk record has no usable file ID"));
    }
    let mount_status = if returned_dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
        Some(cursor.u32()?)
    } else {
        None
    };
    if matches!(kind, EntryKind::Dir) && returned_file != 0
        || !matches!(kind, EntryKind::Dir) && returned_dir != 0
    {
        return Err(invalid(
            "bulk record attribute group disagrees with object type",
        ));
    }
    let allocated_bytes = if returned_file & libc::ATTR_FILE_ALLOCSIZE != 0 {
        cursor.nonnegative_size()?
    } else {
        0
    };
    let logical_size = if returned_file & libc::ATTR_FILE_DATALENGTH != 0 {
        cursor.nonnegative_size()?
    } else {
        0
    };
    if matches!(kind, EntryKind::File) && returned_file & REQUESTED_FILE != REQUESTED_FILE {
        return Err(invalid(
            "bulk regular file omitted requested size attributes",
        ));
    }

    let name_start = name_ref_base
        .checked_add_signed(name_offset as isize)
        .ok_or_else(|| invalid("name attrreference offset overflow"))?;
    let name_end = name_start
        .checked_add(name_length)
        .ok_or_else(|| invalid("name attrreference length overflow"))?;
    if name_length < 2
        || name_start < cursor.offset
        || !name_start.is_multiple_of(4)
        || name_end > entry.len()
    {
        return Err(invalid(
            "name attrreference points outside the variable buffer",
        ));
    }
    let bytes = &entry[name_start..name_end];
    if bytes.last() != Some(&0) {
        return Err(invalid("bulk entry name lacks a NUL terminator"));
    }
    let bytes = &bytes[..bytes.len() - 1];
    if bytes.contains(&0) || bytes.contains(&b'/') || bytes == b"." || bytes == b".." {
        return Err(invalid("bulk entry name is not a single path component"));
    }
    let name = std::str::from_utf8(bytes)
        .map_err(|_| invalid("bulk entry name is not valid UTF-8"))?
        .to_owned();

    let is_dir = matches!(kind, EntryKind::Dir);
    let is_file = matches!(kind, EntryKind::File);
    Ok(BulkEntry {
        name,
        kind,
        file_id,
        logical_size: if is_file { logical_size } else { 0 },
        allocated_bytes: if is_file { allocated_bytes } else { 0 },
        dev_id,
        directory_needs_refresh: is_dir
            && (flags.is_none_or(|value| value & SF_FIRMLINK != 0)
                || mount_status.is_none_or(|value| value != 0)),
    })
}

#[cfg(target_os = "macos")]
struct Cursor<'a> {
    buf: &'a [u8],
    offset: usize,
}

#[cfg(target_os = "macos")]
impl Cursor<'_> {
    fn u32(&mut self) -> io::Result<u32> {
        let value = read_u32_le(self.buf, self.offset)?;
        self.offset += 4;
        Ok(value)
    }

    fn u64(&mut self) -> io::Result<u64> {
        let value = read_u64_le(self.buf, self.offset)?;
        self.offset += 8;
        Ok(value)
    }

    fn nonnegative_size(&mut self) -> io::Result<u64> {
        let value = self.u64()? as i64;
        u64::try_from(value).map_err(|_| invalid("bulk record returned a negative off_t size"))
    }
}

#[cfg(target_os = "macos")]
fn entry_kind_from_obj_type(obj_type: u32) -> io::Result<EntryKind> {
    // Normal POSIX vnode types from <sys/vnode.h>. VNON, VBAD and unknown
    // values do not establish a kind and must be retried through std.
    match obj_type {
        1 => Ok(EntryKind::File),
        2 => Ok(EntryKind::Dir),
        5 => Ok(EntryKind::Symlink),
        3 | 4 | 6 | 7 => Ok(EntryKind::Other),
        _ => Err(invalid("bulk record returned an unknown object type")),
    }
}

fn invalid(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

fn read_u32_le(buf: &[u8], offset: usize) -> io::Result<u32> {
    let end = offset.checked_add(4).ok_or_else(short)?;
    if end > buf.len() {
        return Err(short());
    }
    Ok(u32::from_le_bytes(buf[offset..end].try_into().unwrap()))
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

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{symlink, MetadataExt, OpenOptionsExt};

    struct Record {
        common: u32,
        dir: u32,
        file: u32,
        error: u32,
        obj_type: u32,
        flags: u32,
        mount_status: u32,
        dev: i32,
        file_id: u64,
        allocated: i64,
        logical: i64,
        name: Vec<u8>,
    }

    impl Default for Record {
        fn default() -> Self {
            Self {
                common: REQUESTED_COMMON,
                dir: 0,
                file: REQUESTED_FILE,
                error: 0,
                obj_type: 1,
                flags: 0,
                mount_status: 0,
                dev: 17,
                file_id: 0x0102_0304_0506_0708,
                allocated: 0x1020_3040_5060,
                logical: 0x7080_90a0_b0c0,
                name: b"file\0".to_vec(),
            }
        }
    }

    impl Record {
        fn directory() -> Self {
            Self {
                obj_type: 2,
                dir: libc::ATTR_DIR_MOUNTSTATUS,
                file: 0,
                ..Self::default()
            }
        }

        fn bytes(&self) -> Vec<u8> {
            let mut b = vec![0u8; 24];
            put32(&mut b, 4, self.common);
            put32(&mut b, 12, self.dir);
            put32(&mut b, 16, self.file);
            if self.common & ATTR_CMN_ERROR != 0 {
                b.extend_from_slice(&self.error.to_le_bytes());
            }
            let name_ref = b.len();
            if self.common & libc::ATTR_CMN_NAME != 0 {
                b.extend_from_slice(&[0u8; 8]);
            }
            if self.common & libc::ATTR_CMN_DEVID != 0 {
                b.extend_from_slice(&self.dev.to_le_bytes());
            }
            if self.common & libc::ATTR_CMN_OBJTYPE != 0 {
                b.extend_from_slice(&self.obj_type.to_le_bytes());
            }
            if self.common & libc::ATTR_CMN_FLAGS != 0 {
                b.extend_from_slice(&self.flags.to_le_bytes());
            }
            if self.common & libc::ATTR_CMN_FILEID != 0 {
                b.extend_from_slice(&self.file_id.to_le_bytes());
            }
            if self.dir & libc::ATTR_DIR_MOUNTSTATUS != 0 {
                b.extend_from_slice(&self.mount_status.to_le_bytes());
            }
            if self.file & libc::ATTR_FILE_ALLOCSIZE != 0 {
                b.extend_from_slice(&self.allocated.to_le_bytes());
            }
            if self.file & libc::ATTR_FILE_DATALENGTH != 0 {
                b.extend_from_slice(&self.logical.to_le_bytes());
            }
            if self.common & libc::ATTR_CMN_NAME != 0 {
                let start = b.len();
                put32(&mut b, name_ref, (start - name_ref) as u32);
                put32(&mut b, name_ref + 4, self.name.len() as u32);
                b.extend_from_slice(&self.name);
            }
            b.resize((b.len() + 7) & !7, 0);
            let length = b.len() as u32;
            put32(&mut b, 0, length);
            b
        }
    }

    fn put32(bytes: &mut [u8], offset: usize, value: u32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    #[test]
    fn configured_buffers_are_bounded_without_overflow() {
        for bad in [
            "0",
            "1",
            "3",
            "1025",
            "-1",
            "no",
            "",
            "18446744073709551615",
        ] {
            assert_eq!(
                configured_bulk_buf_bytes(Some(bad)),
                DEFAULT_BULK_BUF_BYTES,
                "{bad}"
            );
        }
        assert_eq!(configured_bulk_buf_bytes(None), DEFAULT_BULK_BUF_BYTES);
        assert_eq!(configured_bulk_buf_bytes(Some("4")), MIN_BULK_BUF_BYTES);
        assert_eq!(
            configured_bulk_buf_bytes(Some("64")),
            DEFAULT_BULK_BUF_BYTES
        );
        assert_eq!(configured_bulk_buf_bytes(Some("1024")), MAX_BULK_BUF_BYTES);
    }

    #[test]
    fn parser_handles_error_first_and_four_byte_u64_alignment() {
        for error_present in [false, true] {
            for flags_present in [false, true] {
                let mut record = Record::default();
                if !error_present {
                    record.common &= !ATTR_CMN_ERROR;
                }
                if !flags_present {
                    record.common &= !libc::ATTR_CMN_FLAGS;
                }
                let parsed = parse_entry(&record.bytes()).unwrap();
                assert_eq!(parsed.name, "file");
                assert_eq!(parsed.file_id, record.file_id);
                assert_eq!(parsed.allocated_bytes, record.allocated as u64);
                assert_eq!(parsed.logical_size, record.logical as u64);
                assert_eq!(parsed.dev_id, 17);
                assert!(!parsed.directory_needs_refresh);
            }
        }
    }

    #[test]
    fn parser_widens_signed_dev_like_std_metadata() {
        for dev in [0, 17, -1, i32::MIN] {
            let record = Record {
                dev,
                ..Record::default()
            };
            assert_eq!(parse_entry(&record.bytes()).unwrap().dev_id, dev as u64);
        }
    }

    #[test]
    fn error_rows_force_fallback_and_keep_the_errno() {
        for errno in [libc::EACCES, libc::EPERM, libc::ENOENT, libc::ENOTSUP] {
            let row = Record {
                common: libc::ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_ERROR | libc::ATTR_CMN_NAME,
                file: 0,
                error: errno as u32,
                ..Record::default()
            };
            assert_eq!(
                parse_entry(&row.bytes()).unwrap_err().raw_os_error(),
                Some(errno)
            );
        }
        let row = Record {
            error: u32::MAX,
            ..Record::default()
        };
        assert_eq!(
            parse_entry(&row.bytes()).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn missing_identity_and_regular_file_sizes_never_become_known_zeroes() {
        for bit in [
            libc::ATTR_CMN_RETURNED_ATTRS,
            libc::ATTR_CMN_NAME,
            libc::ATTR_CMN_DEVID,
            libc::ATTR_CMN_OBJTYPE,
            libc::ATTR_CMN_FILEID,
        ] {
            let row = Record {
                common: REQUESTED_COMMON & !bit,
                ..Record::default()
            };
            assert!(
                parse_entry(&row.bytes()).is_err(),
                "missing common bit {bit:#x}"
            );
        }
        for bit in [libc::ATTR_FILE_ALLOCSIZE, libc::ATTR_FILE_DATALENGTH] {
            let row = Record {
                file: REQUESTED_FILE & !bit,
                ..Record::default()
            };
            assert!(
                parse_entry(&row.bytes()).is_err(),
                "missing file bit {bit:#x}"
            );
        }
        let zero_sizes = Record {
            allocated: 0,
            logical: 0,
            ..Record::default()
        };
        let parsed = parse_entry(&zero_sizes.bytes()).unwrap();
        assert_eq!((parsed.logical_size, parsed.allocated_bytes), (0, 0));
        assert!(parse_entry(
            &Record {
                file_id: 0,
                ..Record::default()
            }
            .bytes()
        )
        .is_err());
    }

    #[test]
    fn missing_guards_refresh_directories_conservatively() {
        let ordinary = Record::directory();
        assert!(
            !parse_entry(&ordinary.bytes())
                .unwrap()
                .directory_needs_refresh
        );
        for row in [
            Record {
                common: ordinary.common & !libc::ATTR_CMN_FLAGS,
                ..Record::directory()
            },
            Record {
                dir: 0,
                ..Record::directory()
            },
            Record {
                flags: SF_FIRMLINK,
                ..Record::directory()
            },
            Record {
                mount_status: 1,
                ..Record::directory()
            },
            Record {
                mount_status: 2,
                ..Record::directory()
            },
            Record {
                mount_status: 0x8000_0000,
                ..Record::directory()
            },
        ] {
            assert!(parse_entry(&row.bytes()).unwrap().directory_needs_refresh);
        }
    }

    #[test]
    fn unknown_types_and_unrequested_attribute_groups_fail_closed() {
        for obj_type in [0, 8, 9, 10, u32::MAX] {
            assert!(parse_entry(
                &Record {
                    obj_type,
                    ..Record::default()
                }
                .bytes()
            )
            .is_err());
        }
        for (offset, extra) in [
            (4, libc::ATTR_CMN_MODTIME),
            (8, 1),
            (12, libc::ATTR_DIR_LINKCOUNT),
            (16, libc::ATTR_FILE_TOTALSIZE),
            (20, 1),
        ] {
            let mut bytes = Record::default().bytes();
            let old = read_u32_le(&bytes, offset).unwrap();
            put32(&mut bytes, offset, old | extra);
            assert!(parse_entry(&bytes).is_err(), "unexpected group at {offset}");
        }
    }

    #[test]
    fn attribute_groups_must_match_the_reported_object_kind() {
        let file_with_directory_group = Record {
            dir: libc::ATTR_DIR_MOUNTSTATUS,
            ..Record::default()
        };
        let directory_with_file_group = Record {
            file: REQUESTED_FILE,
            ..Record::directory()
        };
        assert!(parse_entry(&file_with_directory_group.bytes()).is_err());
        assert!(parse_entry(&directory_with_file_group.bytes()).is_err());
    }

    #[test]
    fn nonregular_contract_zeroes_sizes_but_rejects_negative_off_t() {
        for obj_type in [3, 4, 5, 6, 7] {
            for file in [0, REQUESTED_FILE] {
                let row = Record {
                    obj_type,
                    file,
                    ..Record::default()
                };
                let parsed = parse_entry(&row.bytes()).unwrap();
                assert_eq!((parsed.logical_size, parsed.allocated_bytes), (0, 0));
                assert!(!parsed.directory_needs_refresh);
            }
        }
        for (allocated, logical) in [(-1, 0), (0, -1), (i64::MIN, 0), (0, i64::MIN)] {
            for obj_type in [1, 5] {
                assert!(parse_entry(
                    &Record {
                        obj_type,
                        allocated,
                        logical,
                        ..Record::default()
                    }
                    .bytes()
                )
                .is_err());
            }
        }
    }

    #[test]
    fn malformed_names_cannot_become_traversal_paths() {
        for name in [
            b"".as_slice(),
            b"\0",
            b".\0",
            b"..\0",
            b"a/b\0",
            b"/absolute\0",
            b"a\0b\0",
            b"unterminated",
            b"\xff\0",
        ] {
            assert!(
                parse_entry(
                    &Record {
                        name: name.to_vec(),
                        ..Record::default()
                    }
                    .bytes()
                )
                .is_err(),
                "{name:?}"
            );
        }
        for name in ["line\nbreak", "café", "cafe\u{301}", "...", " spaces "] {
            let row = Record {
                name: [name.as_bytes(), b"\0"].concat(),
                ..Record::default()
            };
            assert_eq!(parse_entry(&row.bytes()).unwrap().name, name);
        }
    }

    #[test]
    fn malformed_lengths_and_references_are_bounds_checked() {
        let bytes = Record::default().bytes();
        for length in 0..bytes.len() {
            assert!(parse_entry(&bytes[..length]).is_err());
        }
        for offset in [i32::MIN as u32, (-1i32) as u32, 0, 1, 2, 3, i32::MAX as u32] {
            let mut bad = bytes.clone();
            put32(&mut bad, 28, offset);
            assert!(parse_entry(&bad).is_err(), "reference offset {offset}");
        }
        for length in [0, 1, u32::MAX] {
            let mut bad = bytes.clone();
            put32(&mut bad, 32, length);
            assert!(parse_entry(&bad).is_err(), "name length {length}");
        }
        for length in [0, 23, 25, u32::MAX] {
            let mut bad = bytes.clone();
            put32(&mut bad, 0, length);
            assert!(parse_entry(&bad).is_err(), "record length {length}");
        }
        assert!(read_u32_le(&bytes, usize::MAX).is_err());
        assert!(read_u64_le(&bytes, usize::MAX).is_err());
    }

    #[test]
    fn malformed_mutations_never_panic_or_accept_an_invalid_component() {
        let original = Record::default().bytes();
        let mut seed = 0x5a17_39c4_82b0_f16du64;
        for iteration in 0..10_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let mut bytes = original.clone();
            bytes[seed as usize % original.len()] = (seed >> 32) as u8;
            if iteration % 3 == 0 {
                bytes.truncate(seed as usize % (original.len() + 1));
            }
            let result = std::panic::catch_unwind(|| parse_entry(&bytes));
            let parsed = result.expect("malformed bulk record must return an error, never panic");
            if let Ok(entry) = parsed {
                assert!(!entry.name.is_empty());
                assert!(!entry.name.contains('/') && !entry.name.contains('\0'));
                assert!(entry.name != "." && entry.name != "..");
                assert!(entry.logical_size <= i64::MAX as u64);
                assert!(entry.allocated_bytes <= i64::MAX as u64);
            }
        }
    }

    #[test]
    fn batch_errors_discard_every_partial_entry() {
        let good = Record::default().bytes();
        let error = Record {
            error: libc::EACCES as u32,
            ..Record::default()
        }
        .bytes();
        let mut out = vec![parse_entry(&good).unwrap()];
        let mut mixed = [good.as_slice(), error.as_slice()].concat();
        assert!(decode_batch(&mixed, 2, &mut out).is_err());
        assert!(out.is_empty());
        mixed = [good.as_slice(), good.as_slice()].concat();
        decode_batch(&mixed, 2, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        for count in [3, usize::MAX] {
            assert!(decode_batch(&mixed, count, &mut out).is_err());
            assert!(out.is_empty());
        }
        let mut bad = good.clone();
        put32(&mut bad, 0, u32::MAX);
        assert!(decode_batch(&bad, 1, &mut out).is_err());
        assert!(out.is_empty());
    }

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
    fn bulk_metadata_matches_stat_for_resource_forks_sparse_files_and_links() {
        let tmp = TempDir::new();
        let root = tmp.path();
        let plain = root.join("forked");
        std::fs::write(&plain, b"hello\n").unwrap();
        std::fs::write(plain.join("..namedfork/rsrc"), b"resource fork\n").unwrap();
        std::fs::hard_link(&plain, root.join("hardlink")).unwrap();
        File::create(root.join("empty")).unwrap();
        let sparse = File::create(root.join("sparse")).unwrap();
        sparse.set_len(4 * 1024 * 1024).unwrap();
        std::fs::create_dir(root.join("directory")).unwrap();
        symlink("forked", root.join("link")).unwrap();
        symlink("absent", root.join("broken-link")).unwrap();
        for name in ["line\nbreak", "composed-café", "decomposed-cafe\u{301}"] {
            std::fs::write(root.join(name), b"utf8").unwrap();
        }

        let mut reader = BulkReader::new();
        let mut bulk = Vec::new();
        reader.read_directory(root, &mut bulk).unwrap();
        let by_name: BTreeMap<_, _> = bulk
            .iter()
            .map(|entry| (entry.name.as_str(), entry))
            .collect();
        assert_eq!(by_name.len(), 10);
        let std_names: std::collections::BTreeSet<_> = std::fs::read_dir(root)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(
            by_name.keys().copied().collect::<Vec<_>>(),
            std_names.iter().map(String::as_str).collect::<Vec<_>>()
        );
        for entry in &bulk {
            let metadata = std::fs::symlink_metadata(root.join(&entry.name)).unwrap();
            assert_eq!(entry.file_id, metadata.ino(), "inode {}", entry.name);
            assert_eq!(entry.dev_id, metadata.dev(), "device {}", entry.name);
            let expected = if metadata.is_file() {
                EntryKind::File
            } else if metadata.is_dir() {
                EntryKind::Dir
            } else if metadata.file_type().is_symlink() {
                EntryKind::Symlink
            } else {
                EntryKind::Other
            };
            assert_eq!(entry.kind, expected, "kind {}", entry.name);
            if metadata.is_file() {
                assert_eq!(
                    entry.logical_size,
                    metadata.size(),
                    "logical {}",
                    entry.name
                );
                assert_eq!(
                    entry.allocated_bytes,
                    metadata.blocks() * 512,
                    "allocated {}",
                    entry.name
                );
            } else {
                assert_eq!((entry.logical_size, entry.allocated_bytes), (0, 0));
            }
        }
        assert_eq!(by_name["forked"].logical_size, 6);
        assert_eq!(by_name["forked"].file_id, by_name["hardlink"].file_id);
        assert_eq!(
            by_name["forked"].allocated_bytes,
            by_name["hardlink"].allocated_bytes
        );
        assert!(by_name["sparse"].allocated_bytes < by_name["sparse"].logical_size);
        assert!(!by_name["directory"].directory_needs_refresh);
    }

    #[test]
    fn bulk_fd_is_borrowed_and_readdir_fallback_can_start_at_zero() {
        let tmp = TempDir::new();
        std::fs::write(tmp.path().join("one"), b"1").unwrap();
        std::fs::write(tmp.path().join("two"), b"2").unwrap();
        let directory = File::open(tmp.path()).unwrap();
        let fd = directory.as_raw_fd();
        let mut reader = BulkReader::new();
        let mut out = Vec::new();
        reader.read_directory_fd(&directory, &mut out).unwrap();
        assert_eq!(out.len(), 2);
        // The borrowed File remains alive, even though bulk consumed its offset.
        assert!(directory.metadata().unwrap().is_dir());
        // SAFETY: fd is live and "." is an independent description of it.
        let fresh = unsafe {
            libc::openat(
                fd,
                c".".as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
            )
        };
        assert!(fresh >= 0);
        // SAFETY: fdopendir owns fresh on success.
        let stream = unsafe { libc::fdopendir(fresh) };
        assert!(!stream.is_null());
        let mut names = Vec::new();
        loop {
            // SAFETY: stream is live until closed below, and each name is
            // copied before the next call can overwrite the dirent buffer.
            let entry = unsafe { libc::readdir(stream) };
            if entry.is_null() {
                break;
            }
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }
                .to_str()
                .unwrap();
            if name != "." && name != ".." {
                names.push(name.to_owned());
            }
        }
        unsafe { libc::closedir(stream) };
        names.sort();
        assert_eq!(names, ["one", "two"]);
    }

    #[test]
    fn pinned_descriptor_survives_a_path_replaced_by_a_symlink() {
        let tmp = TempDir::new();
        let original = tmp.path().join("original");
        let moved = tmp.path().join("moved");
        let replacement = tmp.path().join("replacement");
        std::fs::create_dir(&original).unwrap();
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(original.join("original-file"), b"old").unwrap();
        std::fs::write(replacement.join("replacement-file"), b"new").unwrap();
        let directory = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&original)
            .unwrap();
        std::fs::rename(&original, &moved).unwrap();
        symlink(&replacement, &original).unwrap();
        let mut reader = BulkReader::new();
        let mut out = Vec::new();
        reader.read_directory_fd(&directory, &mut out).unwrap();
        assert_eq!(
            out.iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["original-file"]
        );
        assert!(reader.read_directory(&original, &mut out).is_err());
        assert!(out.is_empty());
    }

    #[test]
    fn unsupported_fd_and_tiny_kernel_buffer_leave_no_partial_rows() {
        let tmp = TempDir::new();
        let file = File::create(tmp.path().join("plain")).unwrap();
        let mut out = vec![parse_entry(&Record::default().bytes()).unwrap()];
        let mut reader = BulkReader::new();
        assert!(reader.read_directory_fd(&file, &mut out).is_err());
        assert!(out.is_empty());
        // A test-only buffer below one record provokes ERANGE from the kernel.
        let mut reader = BulkReader { buf: vec![0; 24] };
        out.push(parse_entry(&Record::default().bytes()).unwrap());
        assert!(reader.read_directory(tmp.path(), &mut out).is_err());
        assert!(out.is_empty());
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

    #[test]
    fn small_buffer_drains_multiple_batches_without_losing_long_names() {
        let tmp = TempDir::new();
        let expected: std::collections::BTreeSet<_> = (0..80)
            .map(|index| format!("{index:03}-{}", "x".repeat(200)))
            .collect();
        for name in &expected {
            std::fs::write(tmp.path().join(name), b"batch").unwrap();
        }
        let mut reader = BulkReader {
            buf: vec![0; MIN_BULK_BUF_BYTES],
        };
        let mut out = Vec::new();
        reader.read_directory(tmp.path(), &mut out).unwrap();
        assert_eq!(out.len(), expected.len());
        assert_eq!(
            out.iter()
                .map(|entry| entry.name.clone())
                .collect::<std::collections::BTreeSet<_>>(),
            expected
        );
        for entry in out {
            assert_eq!(entry.logical_size, 5);
            assert_eq!(
                entry.file_id,
                std::fs::symlink_metadata(tmp.path().join(entry.name))
                    .unwrap()
                    .ino()
            );
        }
    }

    struct TempDir {
        directory: tempfile::TempDir,
    }

    impl TempDir {
        fn new() -> Self {
            Self {
                directory: tempfile::Builder::new()
                    .prefix("apfsfi-bulk-test-")
                    .tempdir()
                    .expect("create exclusive temp dir"),
            }
        }

        fn path(&self) -> &Path {
            self.directory.path()
        }
    }
}
