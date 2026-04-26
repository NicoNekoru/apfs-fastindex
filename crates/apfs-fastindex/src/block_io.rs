//! Block-level I/O helpers shared by every native parser stage.
//!
//! The native parser only reads whole APFS blocks at chosen physical
//! addresses, so the rest of the crate goes through these helpers rather than
//! re-reading raw bytes. Centralizing this also means short reads, offset
//! overflow, and Fletcher-64 verification have one fail-closed implementation.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};

use crate::ScanError;

pub(crate) fn read_block<R: Read + Seek>(
    reader: &mut R,
    block_address: u64,
    block_size: usize,
) -> Result<Vec<u8>, ScanError> {
    let offset = block_address
        .checked_mul(block_size as u64)
        .ok_or_else(|| {
            ScanError::UnsupportedDescriptorLayout("block offset overflow".to_string())
        })?;
    reader.seek(SeekFrom::Start(offset))?;
    let mut block = vec![0u8; block_size];
    let mut read_total = 0;
    while read_total < block_size {
        let read_now = reader.read(&mut block[read_total..])?;
        if read_now == 0 {
            return Err(ScanError::ShortRead {
                block_address,
                expected: block_size,
                actual: read_total,
            });
        }
        read_total += read_now;
    }
    Ok(block)
}

/// Open a path as a seekable byte source. Kept here so callers do not import
/// `std::fs::File` for what is conceptually a block device.
pub(crate) fn open_block_source(path: &str) -> io::Result<File> {
    File::open(path)
}

pub(crate) fn le_u16(block: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(
        block[offset..offset + 2]
            .try_into()
            .expect("u16 field in block"),
    )
}

pub(crate) fn le_u32(block: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(
        block[offset..offset + 4]
            .try_into()
            .expect("u32 field in block"),
    )
}

pub(crate) fn le_u64(block: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(
        block[offset..offset + 8]
            .try_into()
            .expect("u64 field in block"),
    )
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn put_u16(block: &mut [u8], offset: usize, value: u16) {
    block[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
pub(crate) fn put_u32(block: &mut [u8], offset: usize, value: u32) {
    block[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

#[cfg(test)]
pub(crate) fn put_u64(block: &mut [u8], offset: usize, value: u64) {
    block[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
}

/// APFS Fletcher-64 over a full object block.
///
/// The first 8 bytes carry `o_cksum` and are zero during checksum computation,
/// so callers must either store zeros there before calling or rely on
/// [`checksum_matches`] which inserts that zeroing for them.
pub(crate) fn apfs_fletcher64(block: &[u8]) -> u64 {
    let mut lower = 0u64;
    let mut upper = 0u64;
    let mut words = block[8..].chunks(4);
    loop {
        let batch: Vec<_> = words.by_ref().take(1024).collect();
        if batch.is_empty() {
            break;
        }
        for chunk in batch {
            let mut word = [0u8; 4];
            word[..chunk.len()].copy_from_slice(chunk);
            lower += u64::from(u32::from_le_bytes(word));
            upper += lower;
        }
        lower %= 0xffff_ffff;
        upper %= 0xffff_ffff;
    }

    let checksum_lower = 0xffff_ffff - ((lower + upper) % 0xffff_ffff);
    let checksum_upper = 0xffff_ffff - ((lower + checksum_lower) % 0xffff_ffff);
    (checksum_upper << 32) | checksum_lower
}

pub(crate) fn checksum_matches(block: &[u8]) -> bool {
    le_u64(block, 0) == apfs_fletcher64(block)
}

#[cfg(test)]
pub(crate) fn resign_block(block: &mut [u8]) {
    block[..8].fill(0);
    let checksum = apfs_fletcher64(block);
    put_u64(block, 0, checksum);
}
