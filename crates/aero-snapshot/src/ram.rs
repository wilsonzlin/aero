use std::io::{Read, Write};

use crate::error::{Result, SnapshotError};
use crate::io::{ReadLeExt, WriteLeExt};

const MAX_PAGE_SIZE: u32 = 2 * 1024 * 1024;
const MAX_CHUNK_SIZE: u32 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum RamMode {
    Full = 0,
    Dirty = 1,
}

impl RamMode {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(RamMode::Full),
            1 => Ok(RamMode::Dirty),
            _ => Err(SnapshotError::Corrupt("invalid ram mode")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Compression {
    None = 0,
    Lz4 = 1,
}

impl Compression {
    pub fn from_u8(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Compression::None),
            1 => Ok(Compression::Lz4),
            _ => Err(SnapshotError::Corrupt("invalid compression kind")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RamWriteOptions {
    pub mode: RamMode,
    pub compression: Compression,
    pub page_size: u32,
    pub chunk_size: u32,
}

impl Default for RamWriteOptions {
    fn default() -> Self {
        Self {
            mode: RamMode::Full,
            compression: Compression::Lz4,
            page_size: 4096,
            chunk_size: 1024 * 1024,
        }
    }
}

pub fn encode_ram_section<W: Write>(
    w: &mut W,
    total_len: u64,
    opts: RamWriteOptions,
    dirty_pages: Option<&[u64]>,
    read_ram: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<()> {
    if opts.page_size == 0 || opts.page_size > MAX_PAGE_SIZE {
        return Err(SnapshotError::Corrupt("invalid page size"));
    }
    if opts.chunk_size == 0 || opts.chunk_size > MAX_CHUNK_SIZE {
        return Err(SnapshotError::Corrupt("invalid chunk size"));
    }

    w.write_u64_le(total_len)?;
    w.write_u32_le(opts.page_size)?;
    w.write_u8(opts.mode as u8)?;
    w.write_u8(opts.compression as u8)?;
    w.write_u16_le(0)?; // reserved

    match opts.mode {
        RamMode::Full => encode_full(w, total_len, opts, read_ram),
        RamMode::Dirty => {
            let dirty_pages = dirty_pages.ok_or(SnapshotError::Corrupt(
                "dirty ram mode requires dirty page list",
            ))?;
            encode_dirty(w, total_len, opts, dirty_pages, read_ram)
        }
    }
}

fn encode_full<W: Write>(
    w: &mut W,
    total_len: u64,
    opts: RamWriteOptions,
    mut read_ram: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<()> {
    w.write_u32_le(opts.chunk_size)?;

    let chunk_size = opts.chunk_size as u64;
    let mut offset = 0u64;
    let mut buf = vec![0u8; opts.chunk_size as usize];
    while offset < total_len {
        let remaining = total_len - offset;
        let uncompressed_len = (remaining.min(chunk_size)) as usize;
        let buf_slice = &mut buf[..uncompressed_len];
        read_ram(offset, buf_slice)?;

        let compressed = compress(opts.compression, buf_slice)?;
        w.write_u32_le(
            uncompressed_len
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("chunk too large"))?,
        )?;
        w.write_u32_le(
            compressed
                .len()
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("compressed chunk too large"))?,
        )?;
        w.write_bytes(&compressed)?;

        offset += uncompressed_len as u64;
    }
    Ok(())
}

fn encode_dirty<W: Write>(
    w: &mut W,
    total_len: u64,
    opts: RamWriteOptions,
    dirty_pages: &[u64],
    mut read_ram: impl FnMut(u64, &mut [u8]) -> Result<()>,
) -> Result<()> {
    w.write_u64_le(
        dirty_pages
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("too many dirty pages"))?,
    )?;
    let page_size = opts.page_size as u64;
    let mut buf = vec![0u8; opts.page_size as usize];
    for &page_idx in dirty_pages {
        let offset = page_idx
            .checked_mul(page_size)
            .ok_or(SnapshotError::Corrupt("dirty page offset overflow"))?;
        if offset >= total_len {
            return Err(SnapshotError::Corrupt("dirty page out of range"));
        }

        let remaining = total_len - offset;
        let uncompressed_len = (remaining.min(page_size)) as usize;
        let buf_slice = &mut buf[..uncompressed_len];
        read_ram(offset, buf_slice)?;

        let compressed = compress(opts.compression, buf_slice)?;
        w.write_u64_le(page_idx)?;
        w.write_u32_le(uncompressed_len as u32)?;
        w.write_u32_le(compressed.len() as u32)?;
        w.write_bytes(&compressed)?;
    }
    Ok(())
}

fn max_lz4_compressed_len(uncompressed_len: u32) -> u32 {
    // LZ4 worst-case size: uncompressed + (uncompressed / 255) + 16
    uncompressed_len
        .saturating_add(uncompressed_len / 255)
        .saturating_add(16)
}

fn compress(kind: Compression, input: &[u8]) -> Result<Vec<u8>> {
    Ok(match kind {
        Compression::None => input.to_vec(),
        Compression::Lz4 => lz4_flex::block::compress(input),
    })
}

fn decompress(kind: Compression, compressed: &[u8], expected_len: usize) -> Result<Vec<u8>> {
    match kind {
        Compression::None => {
            if compressed.len() != expected_len {
                return Err(SnapshotError::Corrupt("uncompressed chunk length mismatch"));
            }
            Ok(compressed.to_vec())
        }
        Compression::Lz4 => Ok(lz4_flex::block::decompress(compressed, expected_len)?),
    }
}

pub fn decode_ram_section_into<R: Read>(
    r: &mut R,
    expected_total_len: u64,
    write_ram: impl FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    let total_len = r.read_u64_le()?;
    if total_len != expected_total_len {
        return Err(SnapshotError::RamLenMismatch {
            expected: expected_total_len,
            found: total_len,
        });
    }
    let page_size = r.read_u32_le()?;
    if page_size == 0 || page_size > MAX_PAGE_SIZE {
        return Err(SnapshotError::Corrupt("invalid page size"));
    }
    let mode = RamMode::from_u8(r.read_u8()?)?;
    let compression = Compression::from_u8(r.read_u8()?)?;
    let _reserved = r.read_u16_le()?;

    match mode {
        RamMode::Full => decode_full(r, total_len, compression, write_ram),
        RamMode::Dirty => decode_dirty(r, total_len, page_size, compression, write_ram),
    }
}

fn decode_full<R: Read>(
    r: &mut R,
    total_len: u64,
    compression: Compression,
    mut write_ram: impl FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    let chunk_size = r.read_u32_le()?;
    if chunk_size == 0 || chunk_size > MAX_CHUNK_SIZE {
        return Err(SnapshotError::Corrupt("invalid chunk size"));
    }

    let chunk_size_u64 = chunk_size as u64;
    let mut offset = 0u64;
    while offset < total_len {
        let expected_uncompressed = (total_len - offset).min(chunk_size_u64) as u32;
        let uncompressed_len = r.read_u32_le()?;
        if uncompressed_len != expected_uncompressed {
            return Err(SnapshotError::Corrupt("chunk uncompressed length mismatch"));
        }
        let compressed_len = r.read_u32_le()?;
        validate_compressed_len(compression, uncompressed_len, compressed_len)?;
        let compressed = r.read_exact_vec(compressed_len as usize)?;
        let decompressed = decompress(compression, &compressed, uncompressed_len as usize)?;
        write_ram(offset, &decompressed)?;
        offset += uncompressed_len as u64;
    }
    Ok(())
}

fn decode_dirty<R: Read>(
    r: &mut R,
    total_len: u64,
    page_size: u32,
    compression: Compression,
    mut write_ram: impl FnMut(u64, &[u8]) -> Result<()>,
) -> Result<()> {
    let page_size_u64 = page_size as u64;
    let count = r.read_u64_le()?;
    for _ in 0..count {
        let page_idx = r.read_u64_le()?;
        let offset = page_idx
            .checked_mul(page_size_u64)
            .ok_or(SnapshotError::Corrupt("dirty page offset overflow"))?;
        if offset >= total_len {
            return Err(SnapshotError::Corrupt("dirty page out of range"));
        }

        let expected_uncompressed = (total_len - offset).min(page_size_u64) as u32;
        let uncompressed_len = r.read_u32_le()?;
        if uncompressed_len != expected_uncompressed {
            return Err(SnapshotError::Corrupt(
                "dirty page uncompressed length mismatch",
            ));
        }
        let compressed_len = r.read_u32_le()?;
        validate_compressed_len(compression, uncompressed_len, compressed_len)?;
        let compressed = r.read_exact_vec(compressed_len as usize)?;
        let decompressed = decompress(compression, &compressed, uncompressed_len as usize)?;
        write_ram(offset, &decompressed)?;
    }
    Ok(())
}

fn validate_compressed_len(
    compression: Compression,
    uncompressed_len: u32,
    compressed_len: u32,
) -> Result<()> {
    match compression {
        Compression::None => {
            if compressed_len != uncompressed_len {
                return Err(SnapshotError::Corrupt(
                    "compressed_len must equal uncompressed_len for no compression",
                ));
            }
        }
        Compression::Lz4 => {
            let max = max_lz4_compressed_len(uncompressed_len);
            if compressed_len > max {
                return Err(SnapshotError::Corrupt("lz4 chunk too large"));
            }
        }
    }
    Ok(())
}
