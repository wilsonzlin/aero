use std::io::{Read, Write};

use crate::error::{Result, SnapshotError};
use crate::io::{ReadLeExt, WriteLeExt};

pub(crate) const MAX_PAGE_SIZE: u32 = crate::limits::MAX_RAM_PAGE_SIZE;
pub(crate) const MAX_CHUNK_SIZE: u32 = crate::limits::MAX_RAM_CHUNK_SIZE;

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
    let mut compressed = Vec::new();
    if opts.compression == Compression::Lz4 {
        compressed.resize(max_lz4_compressed_len(opts.chunk_size) as usize, 0);
    }
    while offset < total_len {
        let remaining = total_len - offset;
        let uncompressed_len = (remaining.min(chunk_size)) as usize;
        let buf_slice = &mut buf[..uncompressed_len];
        read_ram(offset, buf_slice)?;

        let uncompressed_len_u32: u32 = uncompressed_len
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("chunk too large"))?;
        let compressed_len_u32: u32;
        let payload: &[u8];
        match opts.compression {
            Compression::None => {
                compressed_len_u32 = uncompressed_len_u32;
                payload = buf_slice;
            }
            Compression::Lz4 => {
                let written = lz4_compress_into(buf_slice, &mut compressed)?;
                compressed_len_u32 = written
                    .try_into()
                    .map_err(|_| SnapshotError::Corrupt("compressed chunk too large"))?;
                payload = &compressed[..written];
            }
        }
        w.write_u32_le(uncompressed_len_u32)?;
        w.write_u32_le(compressed_len_u32)?;
        w.write_bytes(payload)?;

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
    // The `dirty_pages` list can originate from runtime tracking; keep encoding deterministic by
    // sorting + de-duplicating page indices before serializing.
    let mut dirty_pages: Vec<u64> = dirty_pages.to_vec();
    dirty_pages.sort_unstable();
    dirty_pages.dedup();

    w.write_u64_le(
        dirty_pages
            .len()
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("too many dirty pages"))?,
    )?;
    let page_size = opts.page_size as u64;
    let mut buf = vec![0u8; opts.page_size as usize];
    let mut compressed = Vec::new();
    if opts.compression == Compression::Lz4 {
        compressed.resize(max_lz4_compressed_len(opts.page_size) as usize, 0);
    }
    for page_idx in dirty_pages {
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

        let compressed_len_u32: u32;
        let payload: &[u8];
        match opts.compression {
            Compression::None => {
                compressed_len_u32 = uncompressed_len as u32;
                payload = buf_slice;
            }
            Compression::Lz4 => {
                let written = lz4_compress_into(buf_slice, &mut compressed)?;
                compressed_len_u32 = written as u32;
                payload = &compressed[..written];
            }
        }
        w.write_u64_le(page_idx)?;
        w.write_u32_le(uncompressed_len as u32)?;
        w.write_u32_le(compressed_len_u32)?;
        w.write_bytes(payload)?;
    }
    Ok(())
}

fn max_lz4_compressed_len(uncompressed_len: u32) -> u32 {
    // `lz4_flex` exposes the exact maximum output size for its block format. Using the library's
    // bound avoids off-by-one mistakes and ensures our encoder preallocates enough space.
    lz4_flex::block::get_maximum_output_size(uncompressed_len as usize) as u32
}

fn lz4_compress_into(input: &[u8], output: &mut [u8]) -> Result<usize> {
    lz4_flex::block::compress_into(input, output)
        .map_err(|_| SnapshotError::Corrupt("lz4 compression failed"))
}

fn lz4_decompress_into(compressed: &[u8], output: &mut [u8]) -> Result<()> {
    lz4_flex::block::decompress_into(compressed, output)?;
    Ok(())
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
    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    if compression == Compression::Lz4 {
        decompressed.resize(chunk_size as usize, 0);
    }
    while offset < total_len {
        let expected_uncompressed = (total_len - offset).min(chunk_size_u64) as u32;
        let uncompressed_len = r.read_u32_le()?;
        if uncompressed_len != expected_uncompressed {
            return Err(SnapshotError::Corrupt("chunk uncompressed length mismatch"));
        }
        let compressed_len = r.read_u32_le()?;
        validate_compressed_len(compression, uncompressed_len, compressed_len)?;
        r.read_exact_into_vec(&mut compressed, compressed_len as usize)?;
        match compression {
            Compression::None => {
                write_ram(offset, &compressed)?;
            }
            Compression::Lz4 => {
                let out = &mut decompressed[..uncompressed_len as usize];
                lz4_decompress_into(&compressed, out)?;
                write_ram(offset, out)?;
            }
        }
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
    let max_pages = total_len
        .checked_add(page_size_u64 - 1)
        .ok_or(SnapshotError::Corrupt("ram length overflow"))?
        / page_size_u64;
    if count > max_pages {
        return Err(SnapshotError::Corrupt("too many dirty pages"));
    }

    let mut compressed = Vec::new();
    let mut decompressed = Vec::new();
    if compression == Compression::Lz4 {
        decompressed.resize(page_size as usize, 0);
    }
    let mut prev_page_idx: Option<u64> = None;
    for _ in 0..count {
        let page_idx = r.read_u64_le()?;
        if let Some(prev) = prev_page_idx {
            if page_idx <= prev {
                return Err(SnapshotError::Corrupt(
                    "dirty page list not strictly increasing",
                ));
            }
        }
        prev_page_idx = Some(page_idx);
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
        r.read_exact_into_vec(&mut compressed, compressed_len as usize)?;
        match compression {
            Compression::None => {
                write_ram(offset, &compressed)?;
            }
            Compression::Lz4 => {
                let out = &mut decompressed[..uncompressed_len as usize];
                lz4_decompress_into(&compressed, out)?;
                write_ram(offset, out)?;
            }
        }
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

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    use rand::{rngs::StdRng, RngCore, SeedableRng};

    fn make_deterministic_ram(len: usize) -> Vec<u8> {
        let mut buf = vec![0u8; len];
        let mut rng = StdRng::seed_from_u64(0x5EED);
        rng.fill_bytes(&mut buf);
        buf
    }

    fn read_from_vec(src: &[u8], offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        buf.copy_from_slice(&src[offset..offset + buf.len()]);
        Ok(())
    }

    fn write_into_vec(dst: &mut [u8], offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        dst[offset..offset + data.len()].copy_from_slice(data);
        Ok(())
    }

    #[test]
    fn full_roundtrip_none() -> Result<()> {
        let ram = make_deterministic_ram(1_000_123);
        let opts = RamWriteOptions {
            mode: RamMode::Full,
            compression: Compression::None,
            page_size: 4096,
            chunk_size: 64 * 1024,
        };

        let mut encoded = Vec::new();
        encode_ram_section(&mut encoded, ram.len() as u64, opts, None, |offset, buf| {
            read_from_vec(&ram, offset, buf)
        })?;

        let mut decoded = vec![0u8; ram.len()];
        decode_ram_section_into(
            &mut std::io::Cursor::new(&encoded),
            ram.len() as u64,
            |offset, data| write_into_vec(&mut decoded, offset, data),
        )?;

        assert_eq!(decoded, ram);
        Ok(())
    }

    #[test]
    fn full_roundtrip_lz4() -> Result<()> {
        let ram = make_deterministic_ram(1_000_123);
        let opts = RamWriteOptions {
            mode: RamMode::Full,
            compression: Compression::Lz4,
            page_size: 4096,
            chunk_size: 64 * 1024,
        };

        let mut encoded = Vec::new();
        encode_ram_section(&mut encoded, ram.len() as u64, opts, None, |offset, buf| {
            read_from_vec(&ram, offset, buf)
        })?;

        let mut decoded = vec![0u8; ram.len()];
        decode_ram_section_into(
            &mut std::io::Cursor::new(&encoded),
            ram.len() as u64,
            |offset, data| write_into_vec(&mut decoded, offset, data),
        )?;

        assert_eq!(decoded, ram);
        Ok(())
    }

    fn dirty_roundtrip(compression: Compression) -> Result<()> {
        let page_size: u32 = 4096;
        let page_size_usize = page_size as usize;
        let ram_len = page_size_usize * 31 + 123;
        let base = make_deterministic_ram(ram_len);
        let mut updated = base.clone();

        let last_page = (ram_len - 1) / page_size_usize;
        // Intentionally out of order.
        let dirty_pages = vec![last_page as u64, 0, 7];
        for &page_idx in &dirty_pages {
            let start = page_idx as usize * page_size_usize;
            let end = (start + page_size_usize).min(ram_len);
            for b in &mut updated[start..end] {
                *b ^= 0xA5;
            }
        }

        let opts = RamWriteOptions {
            mode: RamMode::Dirty,
            compression,
            page_size,
            chunk_size: 1024 * 1024,
        };

        let mut encoded = Vec::new();
        encode_ram_section(
            &mut encoded,
            ram_len as u64,
            opts,
            Some(&dirty_pages),
            |offset, buf| read_from_vec(&updated, offset, buf),
        )?;

        let mut decoded = base;
        decode_ram_section_into(
            &mut std::io::Cursor::new(&encoded),
            ram_len as u64,
            |offset, data| write_into_vec(&mut decoded, offset, data),
        )?;

        assert_eq!(decoded, updated);
        Ok(())
    }

    #[test]
    fn dirty_roundtrip_none() -> Result<()> {
        dirty_roundtrip(Compression::None)
    }

    #[test]
    fn dirty_roundtrip_lz4() -> Result<()> {
        dirty_roundtrip(Compression::Lz4)
    }

    #[test]
    fn lz4_compress_into_matches_allocating_api() {
        let input = make_deterministic_ram(256 * 1024 + 3);
        let expected = lz4_flex::block::compress(&input);

        let max = lz4_flex::block::get_maximum_output_size(input.len());
        let mut out = vec![0u8; max];
        let written = lz4_flex::block::compress_into(&input, &mut out).unwrap();
        assert_eq!(&out[..written], expected);
    }
}
