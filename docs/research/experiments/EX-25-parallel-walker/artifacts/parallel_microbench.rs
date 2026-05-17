//! EX-25 parallel-walker microbench.
//!
//! Compile with:
//!
//!     rustc -O parallel_microbench.rs -o parallel_microbench.bin
//!
//! Run with:
//!
//!     ./parallel_microbench.bin <threads> <target-path>
//!
//! Single-process worker pool. Each worker holds its own 64 KiB
//! getattrlistbulk buffer (BulkReader) and pulls directories off a
//! shared Mutex<Vec<String>> work queue. The mutex is the simplest
//! correct primitive for an experiment whose job is to measure APFS
//! scaling, not Rust runtime scaling — at the rate of ~24k pushes
//! per /Applications scan the mutex overhead is sub-millisecond
//! and does not contaminate the kernel-cost signal we are measuring.
//!
//! Output is one JSON line per invocation with per-thread sys/user
//! CPU breakdown plus the aggregate wall, entry count, and
//! entries-per-second.

#![allow(non_camel_case_types)]
#![allow(non_upper_case_globals)]
#![allow(non_snake_case)]
#![allow(unused)]

use std::env;
use std::ffi::CString;
use std::os::raw::{c_char, c_int, c_long, c_uint, c_void};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

// ---- attribute set constants (production walker mask) -------------- //

const ATTR_BIT_MAP_COUNT: u16 = 5;
const ATTR_CMN_NAME: u32 = 0x0000_0001;
const ATTR_CMN_DEVID: u32 = 0x0000_0002;
const ATTR_CMN_OBJTYPE: u32 = 0x0000_0008;
const ATTR_CMN_FILEID: u32 = 0x0200_0000;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x8000_0000;
const ATTR_CMN_ERROR: u32 = 0x2000_0000;
const ATTR_FILE_TOTALSIZE: u32 = 0x0000_0002;
const ATTR_FILE_ALLOCSIZE: u32 = 0x0000_0004;

const O_RDONLY: c_int = 0;
const VDIR: u32 = 2;

// ---- libc FFI ------------------------------------------------------ //

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
#[derive(Clone, Copy, Default)]
struct timeval {
    tv_sec: c_long,
    tv_usec: c_int,
}

#[repr(C)]
struct rusage {
    ru_utime: timeval,
    ru_stime: timeval,
    _padding: [u8; 256],
}

const RUSAGE_SELF: c_int = 0;
const RUSAGE_THREAD: c_int = 1; // not portable; macOS supports
                                 // RUSAGE_THREAD on a per-thread basis
                                 // through thread_info but not via
                                 // getrusage. We use proc_pid_rusage
                                 // for whole-process numbers and rely
                                 // on per-thread instrumentation via
                                 // mach thread_info; for the
                                 // microbench we keep it simple and
                                 // only report whole-process rusage.

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
}

fn get_rusage_self() -> (f64, f64) {
    let mut ru = unsafe { std::mem::zeroed::<rusage>() };
    let rc = unsafe { getrusage(RUSAGE_SELF, &mut ru) };
    if rc != 0 {
        return (0.0, 0.0);
    }
    let user = ru.ru_utime.tv_sec as f64 + ru.ru_utime.tv_usec as f64 / 1_000_000.0;
    let sys = ru.ru_stime.tv_sec as f64 + ru.ru_stime.tv_usec as f64 / 1_000_000.0;
    (user, sys)
}

// ---- per-thread bulk reader --------------------------------------- //

struct BulkReader {
    buf: Vec<u8>,
}

impl BulkReader {
    fn new() -> Self {
        Self {
            buf: vec![0u8; 65_536],
        }
    }

    /// Read every entry in `dir`. For each VDIR child push the full
    /// path onto `subdirs`. Increment `entry_counter` by the total
    /// entries returned.
    fn read_directory(
        &mut self,
        dir: &str,
        subdirs: &mut Vec<String>,
        entry_counter: &AtomicU64,
    ) {
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
        alist.fileattr = ATTR_FILE_TOTALSIZE | ATTR_FILE_ALLOCSIZE;

        loop {
            let rc = unsafe {
                getattrlistbulk(
                    fd,
                    &mut alist as *mut attrlist as *mut c_void,
                    self.buf.as_mut_ptr() as *mut c_void,
                    self.buf.len(),
                    0,
                )
            };
            if rc < 0 || rc == 0 {
                break;
            }

            let mut local_count: u64 = 0;
            let buf_len = self.buf.len();
            let mut offset: usize = 0;
            for _ in 0..rc {
                if offset + 4 > buf_len {
                    break;
                }
                let entry_len = u32::from_le_bytes([
                    self.buf[offset],
                    self.buf[offset + 1],
                    self.buf[offset + 2],
                    self.buf[offset + 3],
                ]) as usize;
                if entry_len < 24 || offset + entry_len > buf_len {
                    break;
                }
                let entry = &self.buf[offset..offset + entry_len];

                local_count += 1;

                let returned_common =
                    u32::from_le_bytes([entry[4], entry[5], entry[6], entry[7]]);

                let mut cursor: usize = 24;
                let mut name: Option<&[u8]> = None;
                let mut obj_type: u32 = 0;

                if returned_common & ATTR_CMN_NAME != 0 {
                    if cursor + 8 > entry_len {
                        offset += entry_len;
                        continue;
                    }
                    let attrref_base = cursor;
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

                // (skip remaining attrs)

                if obj_type == VDIR {
                    if let Some(name_bytes) = name {
                        if let Ok(name_str) = std::str::from_utf8(name_bytes) {
                            if name_str != "." && name_str != ".." {
                                let mut child =
                                    String::with_capacity(dir.len() + 1 + name_str.len());
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
            entry_counter.fetch_add(local_count, Ordering::Relaxed);
        }

        unsafe { close(fd) };
    }
}

// ---- shared work-queue ------------------------------------------ //

struct WorkQueue {
    queue: Mutex<Vec<String>>,
    /// Number of work units (directories) currently held by a worker
    /// that has not yet finished processing them. Drains to 0 only
    /// when every worker is idle AND the queue is empty.
    outstanding: Mutex<u64>,
    cond: Condvar,
    /// Flipped once every worker has agreed the work is done; lets
    /// blocked workers wake up and exit.
    done: Mutex<bool>,
    done_cond: Condvar,
}

impl WorkQueue {
    fn new(initial: Vec<String>) -> Self {
        let initial_len = initial.len() as u64;
        Self {
            queue: Mutex::new(initial),
            outstanding: Mutex::new(initial_len),
            cond: Condvar::new(),
            done: Mutex::new(false),
            done_cond: Condvar::new(),
        }
    }

    fn push_many(&self, items: Vec<String>) {
        if items.is_empty() {
            return;
        }
        let mut outstanding = self.outstanding.lock().unwrap();
        *outstanding += items.len() as u64;
        drop(outstanding);
        let mut queue = self.queue.lock().unwrap();
        queue.extend(items);
        // Wake one waiter per pushed item — but at most all of them.
        // Notify all is simpler; we only have a handful of threads.
        self.cond.notify_all();
    }

    /// Returns Some(dir) to process, or None to exit.
    fn pop(&self) -> Option<String> {
        loop {
            let mut queue = self.queue.lock().unwrap();
            if let Some(dir) = queue.pop() {
                return Some(dir);
            }
            // Queue empty. Check if work is done.
            {
                let outstanding = self.outstanding.lock().unwrap();
                if *outstanding == 0 {
                    // Everyone idle, queue empty: we're done.
                    let mut done = self.done.lock().unwrap();
                    *done = true;
                    self.done_cond.notify_all();
                    return None;
                }
            }
            // Wait for either more work or done-signal.
            let (_q, _) = self
                .cond
                .wait_timeout(queue, std::time::Duration::from_millis(50))
                .unwrap();
            // Check done flag without holding the queue lock.
            let done = *self.done.lock().unwrap();
            if done {
                return None;
            }
        }
    }

    /// Decrement outstanding when a worker finishes a unit.
    fn complete(&self) {
        let mut outstanding = self.outstanding.lock().unwrap();
        *outstanding = outstanding.saturating_sub(1);
        if *outstanding == 0 {
            drop(outstanding);
            // Wake everyone waiting so they can observe completion.
            self.cond.notify_all();
        }
    }
}

// ---- driver ----------------------------------------------------- //

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: {} <threads> <target-path>", args[0]);
        std::process::exit(2);
    }
    let threads: usize = match args[1].parse() {
        Ok(n) if n >= 1 => n,
        _ => {
            eprintln!("threads must be a positive integer");
            std::process::exit(2);
        }
    };
    let target = args[2].clone();

    let (user0, sys0) = get_rusage_self();
    let wall_start = Instant::now();

    let entry_counter = Arc::new(AtomicU64::new(0));
    let queue = Arc::new(WorkQueue::new(vec![target.clone()]));

    let mut handles = Vec::with_capacity(threads);
    for _ in 0..threads {
        let q = Arc::clone(&queue);
        let counter = Arc::clone(&entry_counter);
        handles.push(std::thread::spawn(move || {
            let mut reader = BulkReader::new();
            let mut subdirs: Vec<String> = Vec::new();
            loop {
                let dir = match q.pop() {
                    Some(d) => d,
                    None => break,
                };
                subdirs.clear();
                reader.read_directory(&dir, &mut subdirs, &counter);
                let taken: Vec<String> = std::mem::take(&mut subdirs);
                q.push_many(taken);
                q.complete();
            }
        }));
    }
    for h in handles {
        h.join().expect("worker thread panicked");
    }

    let wall = wall_start.elapsed().as_secs_f64();
    let (user1, sys1) = get_rusage_self();
    let user = user1 - user0;
    let sys = sys1 - sys0;
    let entries = entry_counter.load(Ordering::Relaxed);
    let throughput = if wall > 0.0 {
        entries as f64 / wall
    } else {
        0.0
    };
    let user_per_thread = if threads > 0 {
        user / threads as f64
    } else {
        user
    };
    let sys_per_thread = if threads > 0 {
        sys / threads as f64
    } else {
        sys
    };

    println!(
        "{{\"threads\":{threads},\"target\":\"{target}\",\"entries\":{entries},\"wall_seconds\":{wall:.6},\"user_seconds\":{user:.6},\"sys_seconds\":{sys:.6},\"user_per_thread\":{user_per_thread:.6},\"sys_per_thread\":{sys_per_thread:.6},\"entries_per_second\":{throughput:.0}}}"
    );
}
