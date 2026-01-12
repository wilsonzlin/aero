use std::collections::HashMap;

use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

const VHD_FOOTER_COOKIE: [u8; 8] = *b"conectix";
const VHD_DYNAMIC_COOKIE: [u8; 8] = *b"cxsparse";

const VHD_DISK_TYPE_FIXED: u32 = 2;
const VHD_DISK_TYPE_DYNAMIC: u32 = 3;
const VHD_FILE_FORMAT_VERSION: u32 = 0x0001_0000;

const SECTOR_SIZE: u32 = 512;

// Hard caps to avoid absurd allocations from untrusted images.
const MAX_BAT_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB
const MAX_BITMAP_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB

#[derive(Debug, Clone)]
struct VhdFooter {
    data_offset: u64,
    current_size: u64,
    disk_type: u32,
    raw: [u8; 512],
}

impl VhdFooter {
    fn parse(raw: [u8; 512]) -> DiskResult<Self> {
        if raw[..8] != VHD_FOOTER_COOKIE {
            return Err(DiskError::CorruptImage("vhd footer cookie mismatch"));
        }

        let file_format_version = be_u32(&raw[12..16]);
        if file_format_version != VHD_FILE_FORMAT_VERSION {
            return Err(DiskError::Unsupported("vhd file format version"));
        }

        let disk_type = be_u32(&raw[60..64]);
        let current_size = be_u64(&raw[48..56]);
        let data_offset = be_u64(&raw[16..24]);

        let expected = be_u32(&raw[64..68]);
        let actual = vhd_checksum_footer(&raw);
        if expected != actual {
            return Err(DiskError::CorruptImage("vhd footer checksum mismatch"));
        }

        if current_size == 0 || !current_size.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::CorruptImage("vhd current_size invalid"));
        }

        match disk_type {
            VHD_DISK_TYPE_FIXED => {
                // Per spec, fixed VHDs use 0xFFFF..FFFF to indicate there is no dynamic header.
                if data_offset != u64::MAX {
                    return Err(DiskError::CorruptImage("vhd fixed data_offset invalid"));
                }
            }
            VHD_DISK_TYPE_DYNAMIC => {
                if data_offset == u64::MAX {
                    return Err(DiskError::CorruptImage("vhd dynamic header offset invalid"));
                }
            }
            _ => {}
        }

        Ok(Self {
            data_offset,
            current_size,
            disk_type,
            raw,
        })
    }

    fn rewrite_checksum(&mut self) {
        self.raw[64..68].fill(0);
        let checksum = vhd_checksum_footer(&self.raw);
        self.raw[64..68].copy_from_slice(&checksum.to_be_bytes());
    }
}

#[derive(Debug, Clone)]
struct VhdDynamicHeader {
    table_offset: u64,
    max_table_entries: u32,
    block_size: u32,
}

impl VhdDynamicHeader {
    fn parse(raw: &[u8; 1024]) -> DiskResult<Self> {
        if raw[..8] != VHD_DYNAMIC_COOKIE {
            return Err(DiskError::CorruptImage(
                "vhd dynamic header cookie mismatch",
            ));
        }

        let table_offset = be_u64(&raw[16..24]);
        let header_version = be_u32(&raw[24..28]);
        if header_version != VHD_FILE_FORMAT_VERSION {
            return Err(DiskError::Unsupported("vhd dynamic header version"));
        }
        let max_table_entries = be_u32(&raw[28..32]);
        let block_size = be_u32(&raw[32..36]);

        if !table_offset.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::CorruptImage("vhd bat offset misaligned"));
        }
        if max_table_entries == 0 {
            return Err(DiskError::CorruptImage("vhd max_table_entries is zero"));
        }
        if block_size == 0 || !(block_size as u64).is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::CorruptImage("vhd block_size invalid"));
        }

        Ok(Self {
            table_offset,
            max_table_entries,
            block_size,
        })
    }
}

pub struct VhdDisk<S> {
    storage: S,
    footer: VhdFooter,
    dynamic: Option<VhdDynamicHeader>,
    bat: Vec<u32>,
    bitmap_cache: HashMap<u64, Vec<u8>>,
    fixed_data_offset: u64,
}

impl<S: ByteStorage> VhdDisk<S> {
    pub fn open(mut storage: S) -> DiskResult<Self> {
        let len = storage.len()?;
        if len < 512 {
            return Err(DiskError::CorruptImage("vhd file too small"));
        }
        if !len.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::CorruptImage("vhd file length misaligned"));
        }

        let footer_offset = len - 512;
        let mut raw_footer = [0u8; 512];
        storage.read_at(footer_offset, &mut raw_footer)?;
        let footer = VhdFooter::parse(raw_footer)?;

        match footer.disk_type {
            VHD_DISK_TYPE_FIXED => {
                // Some tools store an extra copy of the footer at offset 0 even for fixed disks.
                // When present and identical to the EOF footer, treat the data region as starting
                // immediately after this footer copy.
                let mut fixed_data_offset = 0u64;
                if len >= 1024 {
                    let mut raw_footer_copy = [0u8; 512];
                    storage.read_at(0, &mut raw_footer_copy)?;
                    if raw_footer_copy[..8] == VHD_FOOTER_COOKIE {
                        if let Ok(copy) = VhdFooter::parse(raw_footer_copy) {
                            if copy.raw == footer.raw && copy.disk_type == VHD_DISK_TYPE_FIXED {
                                fixed_data_offset = 512;
                            }
                        }
                    }
                }

                let required_len = footer
                    .current_size
                    .checked_add(fixed_data_offset)
                    .and_then(|v| v.checked_add(512))
                    .ok_or(DiskError::CorruptImage("vhd current_size overflow"))?;
                if len < required_len {
                    return Err(DiskError::CorruptImage("vhd fixed disk truncated"));
                }
                Ok(Self {
                    storage,
                    footer,
                    dynamic: None,
                    bat: Vec::new(),
                    bitmap_cache: HashMap::new(),
                    fixed_data_offset,
                })
            }
            VHD_DISK_TYPE_DYNAMIC => {
                // Dynamic VHDs must contain a copy of the footer at both offset 0 and EOF.
                //
                // We validate the footer copy up front so we don't silently treat a corrupted image
                // as valid and later overwrite whatever lives at offset 0 when allocating blocks.
                let mut raw_footer_copy = [0u8; 512];
                storage.read_at(0, &mut raw_footer_copy)?;
                let footer_copy = VhdFooter::parse(raw_footer_copy)?;
                if footer_copy.raw != footer.raw {
                    return Err(DiskError::CorruptImage("vhd footer copy mismatch"));
                }

                if !footer.data_offset.is_multiple_of(SECTOR_SIZE as u64) {
                    return Err(DiskError::CorruptImage(
                        "vhd dynamic header offset misaligned",
                    ));
                }
                if footer.data_offset < 512 {
                    return Err(DiskError::CorruptImage(
                        "vhd dynamic header overlaps footer copy",
                    ));
                }
                let dyn_header_end = footer
                    .data_offset
                    .checked_add(1024)
                    .ok_or(DiskError::OutOfBounds)?;
                if dyn_header_end > len {
                    return Err(DiskError::CorruptImage("vhd dynamic header truncated"));
                }
                if dyn_header_end > footer_offset {
                    return Err(DiskError::CorruptImage(
                        "vhd dynamic header overlaps footer",
                    ));
                }

                let mut raw_header = [0u8; 1024];
                storage.read_at(footer.data_offset, &mut raw_header)?;
                let dynamic = VhdDynamicHeader::parse(&raw_header)?;

                let required_entries = footer.current_size.div_ceil(dynamic.block_size as u64);
                if (dynamic.max_table_entries as u64) < required_entries {
                    return Err(DiskError::CorruptImage("vhd bat too small"));
                }

                // Validate the on-disk BAT size based on `max_table_entries`. We only *read* the
                // portion required for the advertised virtual size, but the metadata region must
                // still be coherent.
                let bat_size_on_disk = {
                    let bat_bytes = (dynamic.max_table_entries as u64)
                        .checked_mul(4)
                        .ok_or(DiskError::OutOfBounds)?;
                    let bat_bytes_aligned = align_up(bat_bytes, SECTOR_SIZE as u64)?;
                    if bat_bytes_aligned > MAX_BAT_BYTES {
                        return Err(DiskError::Unsupported("vhd bat too large"));
                    }
                    bat_bytes_aligned
                };

                let bat_end_on_disk = dynamic
                    .table_offset
                    .checked_add(bat_size_on_disk)
                    .ok_or(DiskError::OutOfBounds)?;
                if bat_end_on_disk > footer_offset {
                    return Err(DiskError::CorruptImage("vhd bat truncated"));
                }
                if dynamic.table_offset < 512 {
                    return Err(DiskError::CorruptImage("vhd bat overlaps footer copy"));
                }
                if ranges_overlap(
                    footer.data_offset,
                    dyn_header_end,
                    dynamic.table_offset,
                    bat_end_on_disk,
                ) {
                    return Err(DiskError::CorruptImage("vhd bat overlaps dynamic header"));
                }

                // Only read the BAT entries needed for the virtual size; this avoids allocating
                // memory proportional to `max_table_entries` for sparse/truncated images.
                let bat_bytes = required_entries
                    .checked_mul(4)
                    .ok_or(DiskError::OutOfBounds)?;
                if bat_bytes > MAX_BAT_BYTES {
                    return Err(DiskError::Unsupported("vhd bat too large"));
                }
                let entries: usize = required_entries
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;
                let bat_bytes_usize: usize = bat_bytes
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;

                let mut bat = Vec::new();
                bat.try_reserve_exact(entries)
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;

                let mut buf = Vec::new();
                buf.try_reserve_exact(64 * 1024)
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;
                buf.resize(64 * 1024, 0);

                let mut remaining = bat_bytes_usize;
                let mut off = dynamic.table_offset;
                while remaining > 0 {
                    let read_len = remaining.min(buf.len());
                    storage.read_at(off, &mut buf[..read_len])?;
                    for chunk in buf[..read_len].chunks_exact(4) {
                        bat.push(be_u32(chunk));
                    }
                    off = off
                        .checked_add(read_len as u64)
                        .ok_or(DiskError::OutOfBounds)?;
                    remaining -= read_len;
                }

                Ok(Self {
                    storage,
                    footer,
                    dynamic: Some(dynamic),
                    bat,
                    bitmap_cache: HashMap::new(),
                    fixed_data_offset: 0,
                })
            }
            _ => Err(DiskError::Unsupported("vhd disk type")),
        }
    }

    pub fn into_storage(self) -> S {
        self.storage
    }

    fn total_sectors_inner(&self) -> u64 {
        self.footer.current_size / SECTOR_SIZE as u64
    }

    fn check_range(&self, lba: u64, bytes: usize) -> DiskResult<()> {
        if !bytes.is_multiple_of(SECTOR_SIZE as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: bytes,
                sector_size: SECTOR_SIZE,
            });
        }
        let sectors = (bytes / SECTOR_SIZE as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.total_sectors_inner(),
        })?;
        if end > self.total_sectors_inner() {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.total_sectors_inner(),
            });
        }
        Ok(())
    }

    fn dyn_params(&self) -> DiskResult<(u64, u64)> {
        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;
        let sectors_per_block = (dyn_hdr.block_size as u64) / SECTOR_SIZE as u64;
        let bitmap_bytes = sectors_per_block.div_ceil(8);
        let bitmap_size = align_up(bitmap_bytes, SECTOR_SIZE as u64)?;
        if bitmap_size > MAX_BITMAP_BYTES {
            return Err(DiskError::Unsupported("vhd bitmap too large"));
        }
        Ok((sectors_per_block, bitmap_size))
    }

    fn data_region_start(&self) -> DiskResult<u64> {
        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;

        // Dynamic header is always 1024 bytes starting at `footer.data_offset`.
        let dyn_header_end = self
            .footer
            .data_offset
            .checked_add(1024)
            .ok_or(DiskError::OutOfBounds)?;

        // The on-disk BAT is sized by `max_table_entries` and must be sector aligned.
        let bat_bytes = (dyn_hdr.max_table_entries as u64)
            .checked_mul(4)
            .ok_or(DiskError::OutOfBounds)?;
        let bat_size = align_up(bat_bytes, SECTOR_SIZE as u64)?;
        let bat_end = dyn_hdr
            .table_offset
            .checked_add(bat_size)
            .ok_or(DiskError::OutOfBounds)?;

        Ok(512u64.max(dyn_header_end).max(bat_end))
    }

    fn validate_block_bounds(&mut self, block_start: u64, bitmap_size: u64) -> DiskResult<()> {
        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;

        let data_start = self.data_region_start()?;
        if block_start < data_start {
            return Err(DiskError::CorruptImage("vhd block overlaps metadata"));
        }

        let file_len = self.storage.len()?;
        if file_len < 512 {
            return Err(DiskError::CorruptImage("vhd file truncated"));
        }
        let footer_offset = file_len - 512;

        let block_total_size = bitmap_size
            .checked_add(dyn_hdr.block_size as u64)
            .ok_or(DiskError::OutOfBounds)?;
        let block_end = block_start
            .checked_add(block_total_size)
            .ok_or(DiskError::OutOfBounds)?;
        if block_end > footer_offset {
            return Err(DiskError::CorruptImage("vhd block overlaps footer"));
        }

        Ok(())
    }

    fn bitmap_get(bitmap: &[u8], sector_in_block: u64) -> DiskResult<bool> {
        let byte = usize::try_from(sector_in_block / 8).map_err(|_| DiskError::OutOfBounds)?;
        if byte >= bitmap.len() {
            return Err(DiskError::CorruptImage("vhd bitmap too small"));
        }
        let bit = 7 - (sector_in_block % 8) as u8;
        Ok((bitmap[byte] & (1u8 << bit)) != 0)
    }

    fn bitmap_set(bitmap: &mut [u8], sector_in_block: u64) -> DiskResult<()> {
        let byte = usize::try_from(sector_in_block / 8).map_err(|_| DiskError::OutOfBounds)?;
        if byte >= bitmap.len() {
            return Err(DiskError::CorruptImage("vhd bitmap too small"));
        }
        let bit = 7 - (sector_in_block % 8) as u8;
        bitmap[byte] |= 1u8 << bit;
        Ok(())
    }

    fn load_bitmap(&mut self, block_start: u64, bitmap_size: u64) -> DiskResult<Vec<u8>> {
        if let Some(v) = self.bitmap_cache.get(&block_start) {
            return Ok(v.clone());
        }
        let bytes = usize::try_from(bitmap_size)
            .map_err(|_| DiskError::Unsupported("vhd bitmap too large"))?;
        let mut bitmap = vec![0u8; bytes];
        self.storage.read_at(block_start, &mut bitmap)?;
        self.bitmap_cache.insert(block_start, bitmap.clone());
        Ok(bitmap)
    }

    fn store_bitmap(&mut self, block_start: u64, bitmap: Vec<u8>) -> DiskResult<()> {
        self.storage.write_at(block_start, &bitmap)?;
        self.bitmap_cache.insert(block_start, bitmap);
        Ok(())
    }

    fn allocate_block(&mut self, block_index: usize) -> DiskResult<u64> {
        let dyn_hdr = self
            .dynamic
            .clone()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;
        let (_sectors_per_block, bitmap_size) = self.dyn_params()?;
        let bitmap_size_usize: usize = bitmap_size
            .try_into()
            .map_err(|_| DiskError::Unsupported("vhd bitmap too large"))?;

        if block_index >= self.bat.len() {
            return Err(DiskError::OutOfBounds);
        }
        if self.bat[block_index] != u32::MAX {
            return Err(DiskError::CorruptImage("vhd block already allocated"));
        }

        let file_len = self.storage.len()?;
        if file_len < 512 {
            return Err(DiskError::CorruptImage("vhd file truncated"));
        }
        let old_footer_offset = file_len - 512;

        let block_total_size = bitmap_size
            .checked_add(dyn_hdr.block_size as u64)
            .ok_or(DiskError::OutOfBounds)?;
        let new_footer_offset = old_footer_offset
            .checked_add(block_total_size)
            .ok_or(DiskError::OutOfBounds)?;
        let new_len = new_footer_offset
            .checked_add(512)
            .ok_or(DiskError::OutOfBounds)?;

        self.storage.set_len(new_len)?;

        write_zeroes(&mut self.storage, old_footer_offset, bitmap_size)?;
        self.storage.flush()?;

        let block_sector = u32::try_from(old_footer_offset / SECTOR_SIZE as u64)
            .map_err(|_| DiskError::Unsupported("vhd block offset"))?;
        self.bat[block_index] = block_sector;
        let bat_entry_offset = dyn_hdr
            .table_offset
            .checked_add((block_index as u64) * 4)
            .ok_or(DiskError::OutOfBounds)?;
        self.storage
            .write_at(bat_entry_offset, &block_sector.to_be_bytes())?;
        self.storage.flush()?;

        let mut footer = self.footer.clone();
        footer.rewrite_checksum();
        self.storage.write_at(0, &footer.raw)?;
        self.storage.write_at(new_footer_offset, &footer.raw)?;
        self.storage.flush()?;

        self.bitmap_cache
            .insert(old_footer_offset, vec![0u8; bitmap_size_usize]);
        Ok(old_footer_offset)
    }
}

impl<S: ByteStorage> DiskBackend for VhdDisk<S> {
    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn total_sectors(&self) -> u64 {
        self.total_sectors_inner()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }

        if self.dynamic.is_none() {
            let offset = lba
                .checked_mul(SECTOR_SIZE as u64)
                .and_then(|v| v.checked_add(self.fixed_data_offset))
                .ok_or(DiskError::OutOfBounds)?;
            self.storage.read_at(offset, buf)?;
            return Ok(());
        }

        let (sectors_per_block, bitmap_size) = self.dyn_params()?;

        let mut buf_off = 0usize;
        while buf_off < buf.len() {
            let sector_index = (buf_off / SECTOR_SIZE as usize) as u64;
            let cur_lba = lba
                .checked_add(sector_index)
                .ok_or(DiskError::OutOfBounds)?;
            let block_index =
                usize::try_from(cur_lba / sectors_per_block).map_err(|_| DiskError::OutOfBounds)?;
            let sector_in_block = cur_lba % sectors_per_block;

            if block_index >= self.bat.len() {
                return Err(DiskError::OutOfBounds);
            }

            let bat_entry = self.bat[block_index];
            if bat_entry == u32::MAX {
                buf[buf_off..buf_off + SECTOR_SIZE as usize].fill(0);
                buf_off += SECTOR_SIZE as usize;
                continue;
            }

            let block_start = (bat_entry as u64)
                .checked_mul(SECTOR_SIZE as u64)
                .ok_or(DiskError::OutOfBounds)?;
            self.validate_block_bounds(block_start, bitmap_size)?;
            let bitmap = self.load_bitmap(block_start, bitmap_size)?;
            if !Self::bitmap_get(&bitmap, sector_in_block)? {
                buf[buf_off..buf_off + SECTOR_SIZE as usize].fill(0);
                buf_off += SECTOR_SIZE as usize;
                continue;
            }

            let data_offset = block_start
                .checked_add(bitmap_size)
                .and_then(|v| v.checked_add(sector_in_block * SECTOR_SIZE as u64))
                .ok_or(DiskError::OutOfBounds)?;
            self.storage.read_at(
                data_offset,
                &mut buf[buf_off..buf_off + SECTOR_SIZE as usize],
            )?;
            buf_off += SECTOR_SIZE as usize;
        }

        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }

        if self.dynamic.is_none() {
            let offset = lba
                .checked_mul(SECTOR_SIZE as u64)
                .and_then(|v| v.checked_add(self.fixed_data_offset))
                .ok_or(DiskError::OutOfBounds)?;
            self.storage.write_at(offset, buf)?;
            return Ok(());
        }

        let (sectors_per_block, bitmap_size) = self.dyn_params()?;

        let mut buf_off = 0usize;
        while buf_off < buf.len() {
            let sector_index = (buf_off / SECTOR_SIZE as usize) as u64;
            let cur_lba = lba
                .checked_add(sector_index)
                .ok_or(DiskError::OutOfBounds)?;
            let block_index =
                usize::try_from(cur_lba / sectors_per_block).map_err(|_| DiskError::OutOfBounds)?;
            let sector_in_block = cur_lba % sectors_per_block;

            if block_index >= self.bat.len() {
                return Err(DiskError::OutOfBounds);
            }

            let bat_entry = self.bat[block_index];
            let block_start = if bat_entry == u32::MAX {
                self.allocate_block(block_index)?
            } else {
                (bat_entry as u64)
                    .checked_mul(SECTOR_SIZE as u64)
                    .ok_or(DiskError::OutOfBounds)?
            };
            self.validate_block_bounds(block_start, bitmap_size)?;

            let data_offset = block_start
                .checked_add(bitmap_size)
                .and_then(|v| v.checked_add(sector_in_block * SECTOR_SIZE as u64))
                .ok_or(DiskError::OutOfBounds)?;
            self.storage
                .write_at(data_offset, &buf[buf_off..buf_off + SECTOR_SIZE as usize])?;

            let mut bitmap = self.load_bitmap(block_start, bitmap_size)?;
            Self::bitmap_set(&mut bitmap, sector_in_block)?;
            self.store_bitmap(block_start, bitmap)?;

            buf_off += SECTOR_SIZE as usize;
        }

        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.storage.flush()
    }
}

fn be_u32(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn be_u64(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn align_up(value: u64, align: u64) -> DiskResult<u64> {
    if align == 0 {
        return Ok(value);
    }
    let rem = value % align;
    if rem == 0 {
        Ok(value)
    } else {
        value.checked_add(align - rem).ok_or(DiskError::OutOfBounds)
    }
}

fn vhd_checksum_footer(raw: &[u8; 512]) -> u32 {
    let mut sum: u32 = 0;
    for (i, b) in raw.iter().enumerate() {
        if (64..68).contains(&i) {
            continue;
        }
        sum = sum.wrapping_add(*b as u32);
    }
    !sum
}

fn write_zeroes<S: ByteStorage>(storage: &mut S, mut offset: u64, mut len: u64) -> DiskResult<()> {
    const CHUNK: usize = 64 * 1024;
    let buf = [0u8; CHUNK];
    while len > 0 {
        let to_write_u64 = len.min(CHUNK as u64);
        let to_write = to_write_u64 as usize;
        storage.write_at(offset, &buf[..to_write])?;
        offset = offset
            .checked_add(to_write_u64)
            .ok_or(DiskError::OutOfBounds)?;
        len -= to_write_u64;
    }
    Ok(())
}

fn ranges_overlap(start_a: u64, end_a: u64, start_b: u64, end_b: u64) -> bool {
    start_a < end_b && start_b < end_a
}
