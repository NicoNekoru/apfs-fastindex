//! EX-24 standalone microbench.
//!
//! Compile with:
//!
//!     rustc -O microbench.rs -o microbench.bin
//!
//! Run with:
//!
//!     ./microbench.bin <config> <target-path>
//!
//!   where <config> is one of `drec_only`, `current_walker`, `fts`.
//!
//! Single-threaded. Walks the target tree recursively, counting
//! entries. Prints one JSON line per invocation with wall, user-CPU,
//! sys-CPU, and entry count. The driver (probe_ex24.py) runs each
//! config 5x and aggregates.
//!
//! No external crates; raw FFI only so this builds with plain
//! `rustc -O` against any toolchain new enough to support 2021
//! edition. The xnu attribute-set constants are hard-coded so the
//! microbench is a controlled replica of `fallback_bulk.rs` minus
//! all the validation and post-processing — the goal is to isolate
//! the kernel-side cost on the same host as the production walker.

#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(unused)]

use std::env;
use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_int, c_long, c_uint, c_ulong, c_void};
use std::time::Instant;

// ---- attribute set constants (from <sys/attr.h>) --------------------- //

const ATTR_BIT_MAP_COUNT: u16 = 5;

const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;

const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;

// ---- libc ABI declarations (raw FFI) --------------------------------- //

#[repr(C)]
struct attrlist {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct timeval {
    tv_sec: c_long,
    tv_usec: c_int, // suseconds_t on darwin = i32
}

#[repr(C)]
struct rusage {
    ru_utime: timeval,
    ru_stime: timeval,
    // We don't care about the rest; the struct is larger but we only
    // read the first two fields. Cast safe because getrusage takes a
    // *mut rusage and writes the whole struct; we read only what we
    // declared. To be safe we'll allocate a larger buffer below.
    _padding: [u8; 256],
}

const O_RDONLY: c_int = 0;
const RUSAGE_SELF: c_int = 0;
const FTS_PHYSICAL: c_int = 0x0010; // do not follow symlinks
const FTS_NOSTAT: c_int = 0x0100;   // do not collect stat info
const FTS_XDEV: c_int = 0x0040;     // don't cross mount boundaries

const FTS_D: u16 = 1;
const FTS_DC: u16 = 2;
const FTS_DEFAULT: u16 = 3;
const FTS_DNR: u16 = 4;
const FTS_DOT: u16 = 5;
const FTS_DP: u16 = 6;
const FTS_ERR: u16 = 7;
const FTS_F: u16 = 8;
const FTS_INIT: u16 = 9;
const FTS_NS: u16 = 10;
const FTS_NSOK: u16 = 11;
const FTS_SL: u16 = 12;
const FTS_SLNONE: u16 = 13;

#[repr(C)]
struct FTSENT {
    fts_cycle: *mut FTSENT,
    fts_parent: *mut FTSENT,
    fts_link: *mut FTSENT,
    fts_number: c_long,
    fts_pointer: *mut c_void,
    fts_accpath: *mut c_char,
    fts_path: *mut c_char,
    fts_errno: c_int,
    fts_symfd: c_int,
    fts_pathlen: c_uint,
    fts_namelen: c_uint,
    fts_ino: u64,
    fts_dev: c_int,
    fts_nlink: c_int,
    fts_level: c_int,
    fts_info: u16,
    fts_flags: u16,
    fts_instr: u16,
    fts_pad: u16,
    fts_statp: *mut c_void,
    fts_name: [c_char; 1],
}

#[repr(C)]
struct FTS {
    _opaque: [u8; 0],
}

extern "C" {
    fn open(path: *const c_char, oflag: c_int) -> c_int;
    fn close(fd: c_int) -> c_int;
    fn getattrlistbulk(
        dirfd: c_int,
        alist: *mut c_void,
        attr_buf: *mut c_void,
        attr_buf_size: usize,
        options: u64,
    ) -> c_int;
    fn getrusage(who: c_int, usage: *mut rusage) -> c_int;
    fn fts_open(
        path_argv: *const *mut c_char,
        options: c_int,
        compar: Option<extern "C" fn(*const *const FTSENT, *const *const FTSENT) -> c_int>,
    ) -> *mut FTS;
    fn fts_read(fts: *mut FTS) -> *mut FTSENT;
    fn fts_close(fts: *mut FTS) -> c_int;
    fn __error() -> *mut c_int;
}

fn errno() -> i32 {
    unsafe { *__error() }
}

// ---- harness --------------------------------------------------------- //

fn get_rusage() -> (f64, f64) {
    let mut ru = unsafe { std::mem::zeroed::<rusage>() };
    let rc = unsafe { getrusage(RUSAGE_SELF, &mut ru) };
    if rc != 0 {
        return (0.0, 0.0);
    }
    let user = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1_000_000.0;
    let sys = ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1_000_000.0;
    (user, sys)
}

// ---- bulk walker (drec_only / current_walker) ----------------------- //

#[derive(Clone, Copy)]
enum BulkMask {
    DrecOnly,
    CurrentWalker,
}

fn walk_bulk(root: &str, mask: BulkMask) -> u64 {
    let mut buf = vec![0u8; 65_536];
    let mut count: u64 = 0;
    walk_bulk_dir(root, &mut buf, mask, &mut count);
    count
}

fn walk_bulk_dir(dir: &str, buf: &mut [u8], mask: BulkMask, count: &mut u64) {
    let c_path = match CString::new(dir) {
        Ok(s) => s,
        Err(_) => return,
    };
    let fd = unsafe { open(c_path.as_ptr(), O_RDONLY) };
    if fd < 0 {
        return;
    }

    let mut alist: attrlist = unsafe { std::mem::zeroed() };
    alist.bitmapcount = ATTR_BIT_MAP_COUNT;
    alist.commonattr = ATTR_CMN_RETURNED_ATTRS
        | ATTR_CMN_NAME
        | ATTR_CMN_DEVID
        | ATTR_CMN_OBJTYPE
        | ATTR_CMN_FILEID
        | ATTR_CMN_ERROR;
    alist.fileattr = match mask {
        BulkMask::DrecOnly => 0,
        BulkMask::CurrentWalker => ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE,
    };

    // Collect subdirectories to recurse into.
    let mut subdirs: Vec<String> = Vec::new();

    loop {
        let rc = unsafe {
            getattrlistbulk(
                fd,
                &mut alist as *mut attrlist as *mut c_void,
                buf.as_mut_ptr() as *mut c_void,
                buf.len(),
                0,
            )
        };
        if rc < 0 {
            break;
        }
        if rc == 0 {
            break;
        }

        let mut offset: usize = 0;
        for _ in 0..rc {
            if offset + 4 > buf.len() {
                break;
            }
            let entry_len = u32::from_le_bytes([
                buf[offset],
                buf[offset + 1],
                buf[offset + 2],
                buf[offset + 3],
            ]) as usize;
            if entry_len < 24 || offset + entry_len > buf.len() {
                break;
            }
            let entry = &buf[offset..offset + entry_len];

            *count += 1;

            // Decode just enough to find subdirectories.
            // attribute_set_t at offset 4..24 = (commonattr, volattr,
            // dirattr, fileattr, forkattr). We read commonattr (which
            // always matches the requested mask because we asked for
            // ATTR_CMN_RETURNED_ATTRS).
            let returned_common = u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);

            let mut cursor: usize = 24;

            // Walk attributes in bit order: NAME, DEVID, OBJTYPE,
            // FILEID, ERROR. We only need NAME (to recurse) and
            // OBJTYPE (to know if it's a directory).
            let mut name: Option<&[u8]> = None;
            let mut obj_type: u32 = 0;

            if returned_common & ATTR_CMN_NAME != 0 {
                let attrref_base = cursor;
                if cursor + 8 > entry_len {
                    offset += entry_len;
                    continue;
                }
                let data_offset = i32::from_le_bytes([
                    entry[cursor],
                    entry[cursor + 1],
                    entry[cursor + 2],
                    entry[cursor + 3],
                ]) as isize;
                let length = u32::from_le_bytes([
                    entry[cursor + 4],
                    entry[cursor + 5],
                    entry[cursor + 6],
                    entry[cursor + 7],
                ]) as usize;
                cursor += 8;
                let start = (attrref_base as isize + data_offset) as usize;
                if length == 0 || start + length > entry_len {
                    offset += entry_len;
                    continue;
                }
                let bytes = &entry[start..start + length];
                let trimmed = if bytes.last() == Some(&0) {
                    &bytes[..length - 1]
                } else {
                    bytes
                };
                name = Some(trimmed);
            }

            if returned_common & ATTR_CMN_DEVID != 0 {
                cursor += 4;
            }

            if returned_common & ATTR_CMN_OBJTYPE != 0 {
                if cursor + 4 <= entry_len {
                    obj_type = u32::from_le_bytes([
                        entry[cursor],
                        entry[cursor + 1],
                        entry[cursor + 2],
                        entry[cursor + 3],
                    ]);
                }
                cursor += 4;
            }

            // Skip remaining attributes; we don't need them.

            // 2 = VDIR per <sys/vnode.h>.
            const VDIR: u32 = 2;
            if obj_type == VDIR {
                if let Some(name_bytes) = name {
                    if let Ok(name_str) = std::str::from_utf8(name_bytes) {
                        // Drop synthetic top-level dot-dirs to mirror
                        // the production walker.
                        if name_str != "." && name_str != ".." {
                            let mut child = String::with_capacity(dir.len() + 1 + name_str.len());
                            child.push_str(dir);
                            if !dir.ends_with('/') {
                                child.push('/');
                            }
                            child.push_str(name_str);
                            subdirs.push(child);
                        }
                    }
                }
            }

            offset += entry_len;
        }
    }

    unsafe { close(fd) };

    for sub in subdirs {
        walk_bulk_dir(&sub, buf, mask, count);
    }
}

// ---- fts walker ----------------------------------------------------- //

fn walk_fts(root: &str) -> u64 {
    let c_path = match CString::new(root) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let mut path_argv = [c_path.as_ptr() as *mut c_char, std::ptr::null_mut()];

    // FTS_PHYSICAL = don't follow symlinks. We deliberately do NOT
    // pass FTS_NOSTAT because that would give fts an unfair advantage
    // (skipping the per-entry stat). Our production walker fetches
    // size/kind per entry, so fts must do the same to be a fair
    // comparison.
    let fts = unsafe { fts_open(path_argv.as_ptr(), FTS_PHYSICAL, None) };
    if fts.is_null() {
        return 0;
    }

    let mut count: u64 = 0;
    loop {
        let entry = unsafe { fts_read(fts) };
        if entry.is_null() {
            break;
        }
        let info = unsafe { (*entry).fts_info };
        match info {
            // Pre-order directory: count and let fts_read recurse.
            // We count both pre-order (FTS_D) and leaf-type entries
            // (FTS_F, FTS_SL, FTS_SLNONE, FTS_DEFAULT) to match the
            // production walker's entry definition.
            FTS_D | FTS_F | FTS_SL | FTS_SLNONE | FTS_DEFAULT => {
                count += 1;
            }
            // Post-order directory: already counted at FTS_D.
            FTS_DP => {}
            // Errors and other special states: don't count.
            _ => {}
        }
    }

    unsafe { fts_close(fts) };
    count
}

// ---- driver --------------------------------------------------------- //

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} <config> <target-path>", args[0]);
        eprintln!("  config = drec_only | current_walker | fts");
        std::process::exit(2);
    }
    let config = args[1].as_str();
    let target = args[2].as_str();

    let (user0, sys0) = get_rusage();
    let wall_start = Instant::now();

    let count = match config {
        "drec_only" => walk_bulk(target, BulkMask::DrecOnly),
        "current_walker" => walk_bulk(target, BulkMask::CurrentWalker),
        "fts" => walk_fts(target),
        other => {
            eprintln!("unknown config: {other}");
            std::process::exit(2);
        }
    };

    let wall = wall_start.elapsed().as_secs_f64();
    let (user1, sys1) = get_rusage();
    let user = user1 - user0;
    let sys = sys1 - sys0;

    let throughput = if wall > 0.0 {
        (count as f64) / wall
    } else {
        0.0
    };

    println!(
        "{{\"config\":\"{config}\",\"target\":\"{target}\",\"entries\":{count},\"wall_seconds\":{wall:.6},\"user_seconds\":{user:.6},\"sys_seconds\":{sys:.6},\"entries_per_second\":{throughput:.0}}}"
    );
}
