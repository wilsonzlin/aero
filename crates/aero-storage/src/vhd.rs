use std::collections::HashMap;

use crate::util::{align_up_u64, checked_range};
use crate::{DiskError, Result, StorageBackend, VirtualDisk, SECTOR_SIZE};

const VHD_FOOTER_COOKIE: [u8; 8] = *b"conectix";
const VHD_DYNAMIC_COOKIE: [u8; 8] = *b"cxsparse";

const VHD_DISK_TYPE_FIXED: u32 = 2;
const VHD_DISK_TYPE_DYNAMIC: u32 = 3;

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
    fn parse(raw: [u8; 512]) -> Result<Self> {
        if raw[..8] != VHD_FOOTER_COOKIE {
            return Err(DiskError::CorruptImage("vhd footer cookie mismatch"));
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
    fn parse(raw: &[u8; 1024]) -> Result<Self> {
        if raw[..8] != VHD_DYNAMIC_COOKIE {
            return Err(DiskError::CorruptImage(
                "vhd dynamic header cookie mismatch",
            ));
        }

        let table_offset = be_u64(&raw[16..24]);
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

/// VHD fixed/dynamic disk (subset).
pub struct VhdDisk<B> {
    backend: B,
    footer: VhdFooter,
    dynamic: Option<VhdDynamicHeader>,
    bat: Vec<u32>,
    bitmap_cache: HashMap<u64, Vec<u8>>,
}

impl<B: StorageBackend> VhdDisk<B> {
    pub fn open(mut backend: B) -> Result<Self> {
        let len = backend.len()?;
        if len < 512 {
            return Err(DiskError::CorruptImage("vhd file too small"));
        }

        let footer_offset = len - 512;
        let mut raw_footer = [0u8; 512];
        backend.read_at(footer_offset, &mut raw_footer)?;
        let footer = VhdFooter::parse(raw_footer)?;

        match footer.disk_type {
            VHD_DISK_TYPE_FIXED => {
                let required_len = footer
                    .current_size
                    .checked_add(512)
                    .ok_or(DiskError::CorruptImage("vhd current_size overflow"))?;
                if len < required_len {
                    return Err(DiskError::CorruptImage("vhd fixed disk truncated"));
                }
                Ok(Self {
                    backend,
                    footer,
                    dynamic: None,
                    bat: Vec::new(),
                    bitmap_cache: HashMap::new(),
                })
            }
            VHD_DISK_TYPE_DYNAMIC => {
                if footer.data_offset == u64::MAX {
                    return Err(DiskError::CorruptImage("vhd dynamic header offset invalid"));
                }
                if footer
                    .data_offset
                    .checked_add(1024)
                    .ok_or(DiskError::OffsetOverflow)?
                    > len
                {
                    return Err(DiskError::CorruptImage("vhd dynamic header truncated"));
                }

                let mut raw_header = [0u8; 1024];
                backend.read_at(footer.data_offset, &mut raw_header)?;
                let dynamic = VhdDynamicHeader::parse(&raw_header)?;

                let required_entries = footer.current_size.div_ceil(dynamic.block_size as u64);
                if (dynamic.max_table_entries as u64) < required_entries {
                    return Err(DiskError::CorruptImage("vhd bat too small"));
                }

                let bat_bytes = (dynamic.max_table_entries as u64)
                    .checked_mul(4)
                    .ok_or(DiskError::OffsetOverflow)?;
                if bat_bytes > MAX_BAT_BYTES {
                    return Err(DiskError::Unsupported("vhd bat too large"));
                }
                let entries: usize = dynamic
                    .max_table_entries
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;
                let bat_bytes_usize: usize = bat_bytes
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;

                let bat_end = dynamic
                    .table_offset
                    .checked_add(bat_bytes)
                    .ok_or(DiskError::OffsetOverflow)?;
                if bat_end > len {
                    return Err(DiskError::CorruptImage("vhd bat truncated"));
                }

                let mut bat_buf = vec![0u8; bat_bytes_usize];
                backend.read_at(dynamic.table_offset, &mut bat_buf)?;
                let mut bat = Vec::with_capacity(entries);
                for chunk in bat_buf.chunks_exact(4) {
                    bat.push(be_u32(chunk));
                }

                Ok(Self {
                    backend,
                    footer,
                    dynamic: Some(dynamic),
                    bat,
                    bitmap_cache: HashMap::new(),
                })
            }
            _ => Err(DiskError::Unsupported("vhd disk type")),
        }
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    fn backend_read_at(&mut self, offset: u64, buf: &mut [u8], ctx: &'static str) -> Result<()> {
        match self.backend.read_at(offset, buf) {
            Ok(()) => Ok(()),
            Err(DiskError::OutOfBounds { .. }) => Err(DiskError::CorruptImage(ctx)),
            Err(e) => Err(e),
        }
    }

    fn dyn_params(&self) -> Result<(u64, u64)> {
        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;
        let sectors_per_block = (dyn_hdr.block_size as u64) / SECTOR_SIZE as u64;
        let bitmap_bytes = sectors_per_block.div_ceil(8);
        let bitmap_size = align_up_u64(bitmap_bytes, SECTOR_SIZE as u64)?;
        if bitmap_size > MAX_BITMAP_BYTES {
            return Err(DiskError::Unsupported("vhd bitmap too large"));
        }
        Ok((sectors_per_block, bitmap_size))
    }

    fn bitmap_get(bitmap: &[u8], sector_in_block: u64) -> Result<bool> {
        let byte: usize = (sector_in_block / 8)
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        if byte >= bitmap.len() {
            return Err(DiskError::CorruptImage("vhd bitmap too small"));
        }
        let bit = 7 - (sector_in_block % 8) as u8;
        Ok((bitmap[byte] & (1u8 << bit)) != 0)
    }

    fn bitmap_set(bitmap: &mut [u8], sector_in_block: u64) -> Result<()> {
        let byte: usize = (sector_in_block / 8)
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        if byte >= bitmap.len() {
            return Err(DiskError::CorruptImage("vhd bitmap too small"));
        }
        let bit = 7 - (sector_in_block % 8) as u8;
        bitmap[byte] |= 1u8 << bit;
        Ok(())
    }

    fn load_bitmap(&mut self, block_start: u64, bitmap_size: u64) -> Result<Vec<u8>> {
        if let Some(v) = self.bitmap_cache.get(&block_start) {
            return Ok(v.clone());
        }
        let bytes: usize = bitmap_size
            .try_into()
            .map_err(|_| DiskError::Unsupported("vhd bitmap too large"))?;
        let mut bitmap = vec![0u8; bytes];
        self.backend_read_at(block_start, &mut bitmap, "vhd block bitmap truncated")?;
        self.bitmap_cache.insert(block_start, bitmap.clone());
        Ok(bitmap)
    }

    fn store_bitmap(&mut self, block_start: u64, bitmap: Vec<u8>) -> Result<()> {
        self.backend.write_at(block_start, &bitmap)?;
        self.bitmap_cache.insert(block_start, bitmap);
        Ok(())
    }

    fn is_sector_allocated(&mut self, lba: u64) -> Result<bool> {
        let (sectors_per_block, bitmap_size) = self.dyn_params()?;
        let block_index = (lba / sectors_per_block) as usize;
        let sector_in_block = lba % sectors_per_block;
        if block_index >= self.bat.len() {
            return Err(DiskError::CorruptImage("vhd block index out of range"));
        }
        let bat_entry = self.bat[block_index];
        if bat_entry == u32::MAX {
            return Ok(false);
        }
        let block_start = (bat_entry as u64)
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        let bitmap = self.load_bitmap(block_start, bitmap_size)?;
        Self::bitmap_get(&bitmap, sector_in_block)
    }

    fn read_sector_dyn(&mut self, lba: u64, out: &mut [u8; SECTOR_SIZE]) -> Result<()> {
        let (sectors_per_block, bitmap_size) = self.dyn_params()?;

        let block_index = (lba / sectors_per_block) as usize;
        let sector_in_block = lba % sectors_per_block;

        if block_index >= self.bat.len() {
            return Err(DiskError::CorruptImage("vhd block index out of range"));
        }

        let bat_entry = self.bat[block_index];
        if bat_entry == u32::MAX {
            out.fill(0);
            return Ok(());
        }

        let block_start = (bat_entry as u64)
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        let bitmap = self.load_bitmap(block_start, bitmap_size)?;
        if !Self::bitmap_get(&bitmap, sector_in_block)? {
            out.fill(0);
            return Ok(());
        }

        let data_offset = block_start
            .checked_add(bitmap_size)
            .and_then(|v| v.checked_add(sector_in_block * SECTOR_SIZE as u64))
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend_read_at(data_offset, out, "vhd block data truncated")?;
        Ok(())
    }

    fn write_sector_dyn(&mut self, lba: u64, data: &[u8; SECTOR_SIZE]) -> Result<()> {
        if !self.is_sector_allocated(lba)? && data.iter().all(|b| *b == 0) {
            // Keep the image sparse: writing zeros to an unallocated sector doesn't need to
            // allocate anything.
            return Ok(());
        }

        let dyn_hdr = self
            .dynamic
            .clone()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;
        let (sectors_per_block, bitmap_size) = self.dyn_params()?;

        let block_index = (lba / sectors_per_block) as usize;
        let sector_in_block = lba % sectors_per_block;
        if block_index >= self.bat.len() {
            return Err(DiskError::CorruptImage("vhd block index out of range"));
        }

        let bat_entry = self.bat[block_index];
        let block_start = if bat_entry == u32::MAX {
            self.allocate_block(block_index, &dyn_hdr, bitmap_size)?
        } else {
            (bat_entry as u64) * SECTOR_SIZE as u64
        };

        let data_offset = block_start
            .checked_add(bitmap_size)
            .and_then(|v| v.checked_add(sector_in_block * SECTOR_SIZE as u64))
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.write_at(data_offset, data)?;

        let mut bitmap = self.load_bitmap(block_start, bitmap_size)?;
        Self::bitmap_set(&mut bitmap, sector_in_block)?;
        self.store_bitmap(block_start, bitmap)?;

        Ok(())
    }

    fn allocate_block(
        &mut self,
        block_index: usize,
        dyn_hdr: &VhdDynamicHeader,
        bitmap_size: u64,
    ) -> Result<u64> {
        if block_index >= self.bat.len() {
            return Err(DiskError::CorruptImage("vhd block index out of range"));
        }
        if self.bat[block_index] != u32::MAX {
            return Err(DiskError::CorruptImage("vhd block already allocated"));
        }

        let file_len = self.backend.len()?;
        if file_len < 512 {
            return Err(DiskError::CorruptImage("vhd file truncated"));
        }
        let old_footer_offset = file_len - 512;

        let block_total_size = bitmap_size
            .checked_add(dyn_hdr.block_size as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        let new_footer_offset = old_footer_offset
            .checked_add(block_total_size)
            .ok_or(DiskError::OffsetOverflow)?;
        let new_len = new_footer_offset
            .checked_add(512)
            .ok_or(DiskError::OffsetOverflow)?;

        self.backend.set_len(new_len)?;

        // Initialize the per-block bitmap. The data area can remain uninitialized because
        // reads for sectors with bitmap=0 must return zeros.
        write_zeroes(&mut self.backend, old_footer_offset, bitmap_size)?;

        let block_sector: u32 = (old_footer_offset / SECTOR_SIZE as u64)
            .try_into()
            .map_err(|_| DiskError::Unsupported("vhd block offset"))?;
        self.bat[block_index] = block_sector;
        let bat_entry_offset = dyn_hdr
            .table_offset
            .checked_add((block_index as u64) * 4)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend
            .write_at(bat_entry_offset, &block_sector.to_be_bytes())?;

        // The dynamic disk footer must exist at both offset 0 and the end of the file.
        self.footer.rewrite_checksum();
        self.backend.write_at(0, &self.footer.raw)?;
        self.backend.write_at(new_footer_offset, &self.footer.raw)?;

        self.bitmap_cache
            .insert(old_footer_offset, vec![0u8; bitmap_size as usize]);

        Ok(old_footer_offset)
    }
}

impl<B: StorageBackend> VirtualDisk for VhdDisk<B> {
    fn capacity_bytes(&self) -> u64 {
        self.footer.current_size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        if buf.is_empty() {
            return Ok(());
        }

        if self.dynamic.is_none() {
            self.backend_read_at(offset, buf, "vhd fixed disk truncated")?;
            return Ok(());
        }

        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset
                .checked_add(pos as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let lba = abs / SECTOR_SIZE as u64;
            let within = (abs % SECTOR_SIZE as u64) as usize;
            let chunk_len = (SECTOR_SIZE - within).min(buf.len() - pos);

            let mut sector = [0u8; SECTOR_SIZE];
            self.read_sector_dyn(lba, &mut sector)?;
            buf[pos..pos + chunk_len].copy_from_slice(&sector[within..within + chunk_len]);

            pos += chunk_len;
        }

        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        if buf.is_empty() {
            return Ok(());
        }

        if self.dynamic.is_none() {
            self.backend.write_at(offset, buf)?;
            return Ok(());
        }

        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset
                .checked_add(pos as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let lba = abs / SECTOR_SIZE as u64;
            let within = (abs % SECTOR_SIZE as u64) as usize;
            let chunk_len = (SECTOR_SIZE - within).min(buf.len() - pos);

            if within == 0 && chunk_len == SECTOR_SIZE {
                let mut sector = [0u8; SECTOR_SIZE];
                sector.copy_from_slice(&buf[pos..pos + SECTOR_SIZE]);
                self.write_sector_dyn(lba, &sector)?;
            } else {
                let mut sector = [0u8; SECTOR_SIZE];
                self.read_sector_dyn(lba, &mut sector)?;
                sector[within..within + chunk_len].copy_from_slice(&buf[pos..pos + chunk_len]);
                self.write_sector_dyn(lba, &sector)?;
            }

            pos += chunk_len;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.backend.flush()
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

fn write_zeroes<B: StorageBackend>(backend: &mut B, mut offset: u64, mut len: u64) -> Result<()> {
    const CHUNK: usize = 64 * 1024;
    let buf = [0u8; CHUNK];
    while len > 0 {
        let to_write = (len as usize).min(CHUNK);
        backend.write_at(offset, &buf[..to_write])?;
        offset = offset
            .checked_add(to_write as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        len -= to_write as u64;
    }
    Ok(())
}
