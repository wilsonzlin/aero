use lru::LruCache;
use std::num::NonZeroUsize;
use std::sync::Arc;

use crate::util::{align_up_u64, checked_range};
use crate::{DiskError, Result, StorageBackend, VirtualDisk, SECTOR_SIZE};

const VHD_FOOTER_COOKIE: [u8; 8] = *b"conectix";
const VHD_DYNAMIC_COOKIE: [u8; 8] = *b"cxsparse";

const VHD_DISK_TYPE_FIXED: u32 = 2;
const VHD_DISK_TYPE_DYNAMIC: u32 = 3;

// Hard caps to avoid absurd allocations from untrusted images.
const MAX_BAT_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB
const MAX_BITMAP_BYTES: u64 = 32 * 1024 * 1024; // 32 MiB

// Bound bitmap caching when reading large fully-allocated dynamic VHDs.
const VHD_BITMAP_CACHE_BUDGET_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

#[derive(Debug, Clone)]
struct VhdFooter {
    data_offset: u64,
    current_size: u64,
    disk_type: u32,
    raw: [u8; SECTOR_SIZE],
}

impl VhdFooter {
    fn parse(raw: [u8; SECTOR_SIZE]) -> Result<Self> {
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
    bitmap_cache: LruCache<u64, Arc<Vec<u8>>>,
}

impl<B: StorageBackend> VhdDisk<B> {
    pub fn open(mut backend: B) -> Result<Self> {
        let len = backend.len()?;
        if len < SECTOR_SIZE as u64 {
            return Err(DiskError::CorruptImage("vhd file too small"));
        }
        if !len.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::CorruptImage("vhd file length misaligned"));
        }

        let footer_offset = len - SECTOR_SIZE as u64;
        let mut raw_footer = [0u8; SECTOR_SIZE];
        match backend.read_at(footer_offset, &mut raw_footer) {
            Ok(()) => {}
            Err(DiskError::OutOfBounds { .. }) => {
                return Err(DiskError::CorruptImage("vhd footer truncated"));
            }
            Err(e) => return Err(e),
        }
        let footer = VhdFooter::parse(raw_footer)?;

        match footer.disk_type {
            VHD_DISK_TYPE_FIXED => {
                let required_len = footer
                    .current_size
                    .checked_add(SECTOR_SIZE as u64)
                    .ok_or(DiskError::CorruptImage("vhd current_size overflow"))?;
                if len < required_len {
                    return Err(DiskError::CorruptImage("vhd fixed disk truncated"));
                }
                Ok(Self {
                    backend,
                    footer,
                    dynamic: None,
                    bat: Vec::new(),
                    bitmap_cache: LruCache::new(NonZeroUsize::MIN),
                })
            }
            VHD_DISK_TYPE_DYNAMIC => {
                if footer.data_offset == u64::MAX {
                    return Err(DiskError::CorruptImage("vhd dynamic header offset invalid"));
                }
                if !footer.data_offset.is_multiple_of(SECTOR_SIZE as u64) {
                    return Err(DiskError::CorruptImage(
                        "vhd dynamic header offset misaligned",
                    ));
                }
                if footer.data_offset < SECTOR_SIZE as u64 {
                    return Err(DiskError::CorruptImage(
                        "vhd dynamic header overlaps footer copy",
                    ));
                }
                let footer_offset = len - SECTOR_SIZE as u64;
                let dyn_header_end = footer
                    .data_offset
                    .checked_add(1024)
                    .ok_or(DiskError::OffsetOverflow)?;
                if dyn_header_end > len {
                    return Err(DiskError::CorruptImage("vhd dynamic header truncated"));
                }
                if dyn_header_end > footer_offset {
                    return Err(DiskError::CorruptImage(
                        "vhd dynamic header overlaps footer",
                    ));
                }

                let mut raw_header = [0u8; 1024];
                match backend.read_at(footer.data_offset, &mut raw_header) {
                    Ok(()) => {}
                    Err(DiskError::OutOfBounds { .. }) => {
                        return Err(DiskError::CorruptImage("vhd dynamic header truncated"));
                    }
                    Err(e) => return Err(e),
                }
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
                        .ok_or(DiskError::OffsetOverflow)?;
                    let bat_bytes_aligned = align_up_u64(bat_bytes, SECTOR_SIZE as u64)?;
                    if bat_bytes_aligned > MAX_BAT_BYTES {
                        return Err(DiskError::Unsupported("vhd bat too large"));
                    }
                    bat_bytes_aligned
                };
                if len < SECTOR_SIZE as u64 {
                    return Err(DiskError::CorruptImage("vhd file too small"));
                }
                let footer_offset = len - SECTOR_SIZE as u64;
                let bat_end_on_disk = dynamic
                    .table_offset
                    .checked_add(bat_size_on_disk)
                    .ok_or(DiskError::OffsetOverflow)?;
                if bat_end_on_disk > footer_offset {
                    return Err(DiskError::CorruptImage("vhd bat truncated"));
                }
                if dynamic.table_offset < SECTOR_SIZE as u64 {
                    return Err(DiskError::CorruptImage("vhd bat overlaps footer copy"));
                }
                if dynamic.table_offset < dyn_header_end && footer.data_offset < bat_end_on_disk {
                    return Err(DiskError::CorruptImage("vhd bat overlaps dynamic header"));
                }

                // Only read the BAT entries needed for the virtual size; this avoids allocating
                // memory proportional to `max_table_entries` for sparse/truncated images.
                let bat_bytes = required_entries
                    .checked_mul(4)
                    .ok_or(DiskError::OffsetOverflow)?;
                if bat_bytes > MAX_BAT_BYTES {
                    return Err(DiskError::Unsupported("vhd bat too large"));
                }
                let entries: usize = required_entries
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;
                let bat_bytes_usize: usize = bat_bytes
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd bat too large"))?;

                let mut bat_buf = vec![0u8; bat_bytes_usize];
                match backend.read_at(dynamic.table_offset, &mut bat_buf) {
                    Ok(()) => {}
                    Err(DiskError::OutOfBounds { .. }) => {
                        return Err(DiskError::CorruptImage("vhd bat truncated"));
                    }
                    Err(e) => return Err(e),
                }
                let mut bat = Vec::with_capacity(entries);
                for chunk in bat_buf.chunks_exact(4) {
                    bat.push(be_u32(chunk));
                }

                // Size bitmap caching based on the bitmap size for this image.
                let sectors_per_block = (dynamic.block_size as u64) / SECTOR_SIZE as u64;
                let bitmap_bytes = sectors_per_block.div_ceil(8);
                let bitmap_size = align_up_u64(bitmap_bytes, SECTOR_SIZE as u64)?;
                let cap_entries = (VHD_BITMAP_CACHE_BUDGET_BYTES / bitmap_size).max(1) as usize;
                let cap_entries = cap_entries
                    .min(VHD_BITMAP_CACHE_BUDGET_BYTES as usize / 512)
                    .max(1);
                let cap =
                    NonZeroUsize::new(cap_entries).ok_or(DiskError::InvalidConfig("vhd cache"))?;

                Ok(Self {
                    backend,
                    footer,
                    dynamic: Some(dynamic),
                    bat,
                    bitmap_cache: LruCache::new(cap),
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

    fn sector_run_len(
        bitmap: &[u8],
        sectors_per_block: u64,
        within_block: u64,
        remaining: u64,
        allocated: bool,
    ) -> Result<u64> {
        let start_sector = within_block / SECTOR_SIZE as u64;
        let limit = within_block
            .checked_add(remaining)
            .ok_or(DiskError::OffsetOverflow)?;

        let mut sector = start_sector;
        let mut end = ((sector + 1) * SECTOR_SIZE as u64).min(limit);

        while end < limit {
            sector = sector.checked_add(1).ok_or(DiskError::OffsetOverflow)?;
            if sector >= sectors_per_block {
                break;
            }
            let bit = Self::bitmap_get(bitmap, sector)?;
            if bit != allocated {
                break;
            }
            end = ((sector + 1) * SECTOR_SIZE as u64).min(limit);
        }

        Ok(end - within_block)
    }

    fn load_bitmap(&mut self, block_start: u64, bitmap_size: u64) -> Result<Arc<Vec<u8>>> {
        if let Some(v) = self.bitmap_cache.get(&block_start) {
            return Ok(v.clone());
        }
        let bytes: usize = bitmap_size
            .try_into()
            .map_err(|_| DiskError::Unsupported("vhd bitmap too large"))?;
        let mut bitmap = vec![0u8; bytes];
        self.backend_read_at(block_start, &mut bitmap, "vhd block bitmap truncated")?;
        let arc = Arc::new(bitmap);
        let _ = self.bitmap_cache.push(block_start, arc.clone());
        Ok(arc)
    }

    fn data_region_start(&self) -> Result<u64> {
        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;

        let footer_copy_end = SECTOR_SIZE as u64;
        let dyn_header_end = self
            .footer
            .data_offset
            .checked_add(1024)
            .ok_or(DiskError::OffsetOverflow)?;

        let bat_bytes = (dyn_hdr.max_table_entries as u64)
            .checked_mul(4)
            .ok_or(DiskError::OffsetOverflow)?;
        let bat_size = align_up_u64(bat_bytes, SECTOR_SIZE as u64)?;
        let bat_end = dyn_hdr
            .table_offset
            .checked_add(bat_size)
            .ok_or(DiskError::OffsetOverflow)?;

        Ok(footer_copy_end.max(dyn_header_end).max(bat_end))
    }

    fn validate_block_bounds(&mut self, block_start: u64, bitmap_size: u64) -> Result<()> {
        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd is not dynamic"))?;

        // Prevent a corrupt BAT entry from pointing into the file header / BAT region.
        let data_start = self.data_region_start()?;
        if block_start < data_start {
            return Err(DiskError::CorruptImage("vhd block overlaps metadata"));
        }

        // Prevent allocated blocks from overlapping the required footer at EOF.
        let file_len = self.backend.len()?;
        if file_len < SECTOR_SIZE as u64 {
            return Err(DiskError::CorruptImage("vhd file truncated"));
        }
        let footer_offset = file_len - SECTOR_SIZE as u64;
        let block_total_size = bitmap_size
            .checked_add(dyn_hdr.block_size as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        let block_end = block_start
            .checked_add(block_total_size)
            .ok_or(DiskError::OffsetOverflow)?;
        if block_end > footer_offset {
            return Err(DiskError::CorruptImage("vhd block overlaps footer"));
        }

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
        self.validate_block_bounds(block_start, bitmap_size)?;
        let bitmap = self.load_bitmap(block_start, bitmap_size)?;
        Self::bitmap_get(bitmap.as_slice(), sector_in_block)
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
        self.validate_block_bounds(block_start, bitmap_size)?;
        let bitmap = self.load_bitmap(block_start, bitmap_size)?;
        if !Self::bitmap_get(bitmap.as_slice(), sector_in_block)? {
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
        if data.iter().all(|b| *b == 0) {
            // Keep the image sparse: writing zeros to an unallocated sector doesn't need to
            // allocate anything.
            if !self.is_sector_allocated(lba)? {
                return Ok(());
            }
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
        self.validate_block_bounds(block_start, bitmap_size)?;

        let data_offset = block_start
            .checked_add(bitmap_size)
            .and_then(|v| v.checked_add(sector_in_block * SECTOR_SIZE as u64))
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.write_at(data_offset, data)?;

        // Update the per-block bitmap.
        //
        // We keep the bitmap cached in memory and only write back the single modified byte.
        let byte_index =
            usize::try_from(sector_in_block / 8).map_err(|_| DiskError::OffsetOverflow)?;
        let bit = 7 - (sector_in_block % 8) as u8;
        let mask = 1u8 << bit;

        // Ensure bitmap is present in the cache so we can update it without cloning.
        let _ = self.load_bitmap(block_start, bitmap_size)?;

        let (byte_offset, old_byte, new_byte) = {
            let entry = self
                .bitmap_cache
                .get_mut(&block_start)
                .ok_or(DiskError::CorruptImage("vhd bitmap cache missing"))?;
            let bitmap_vec: &mut Vec<u8> = Arc::make_mut(entry);
            if byte_index >= bitmap_vec.len() {
                return Err(DiskError::CorruptImage("vhd bitmap too small"));
            }
            let old = bitmap_vec[byte_index];
            let new = old | mask;
            // Update the cached bitmap first so subsequent reads in this process observe the
            // newly-written sector. If writing the bitmap byte back to the backend fails,
            // we roll this change back below.
            bitmap_vec[byte_index] = new;
            (
                block_start
                    .checked_add(byte_index as u64)
                    .ok_or(DiskError::OffsetOverflow)?,
                old,
                new,
            )
        };

        if new_byte != old_byte {
            if let Err(e) = self.backend.write_at(byte_offset, &[new_byte]) {
                // Best-effort rollback so a failed write doesn't leave the in-memory bitmap
                // claiming the sector is present when the on-disk bitmap was not updated.
                if let Some(entry) = self.bitmap_cache.get_mut(&block_start) {
                    let bitmap_vec: &mut Vec<u8> = Arc::make_mut(entry);
                    if byte_index < bitmap_vec.len() {
                        bitmap_vec[byte_index] = old_byte;
                    }
                }
                return Err(e);
            }
        }

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
        if file_len < SECTOR_SIZE as u64 {
            return Err(DiskError::CorruptImage("vhd file truncated"));
        }
        let old_footer_offset = file_len - SECTOR_SIZE as u64;
        if old_footer_offset < self.data_region_start()? {
            return Err(DiskError::CorruptImage("vhd footer overlaps metadata"));
        }

        let block_total_size = bitmap_size
            .checked_add(dyn_hdr.block_size as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        let new_footer_offset = old_footer_offset
            .checked_add(block_total_size)
            .ok_or(DiskError::OffsetOverflow)?;
        let new_len = new_footer_offset
            .checked_add(SECTOR_SIZE as u64)
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

        let bitmap_size_usize: usize = bitmap_size
            .try_into()
            .map_err(|_| DiskError::Unsupported("vhd bitmap too large"))?;
        self.bitmap_cache
            .push(old_footer_offset, Arc::new(vec![0u8; bitmap_size_usize]));

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

        let dyn_hdr = self
            .dynamic
            .as_ref()
            .ok_or(DiskError::CorruptImage("vhd dynamic header missing"))?;
        let block_size = dyn_hdr.block_size as u64;
        let mut pos = 0usize;
        let (sectors_per_block, bitmap_size) = self.dyn_params()?;
        while pos < buf.len() {
            let abs = offset
                .checked_add(pos as u64)
                .ok_or(DiskError::OffsetOverflow)?;

            let block_index = (abs / block_size) as usize;
            let within_block = abs % block_size;
            let remaining_in_block = block_size - within_block;
            let chunk_len = remaining_in_block.min((buf.len() - pos) as u64) as usize;

            if block_index >= self.bat.len() {
                return Err(DiskError::CorruptImage("vhd block index out of range"));
            }
            let bat_entry = self.bat[block_index];
            if bat_entry == u32::MAX {
                buf[pos..pos + chunk_len].fill(0);
                pos += chunk_len;
                continue;
            }

            let block_start = (bat_entry as u64)
                .checked_mul(SECTOR_SIZE as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            self.validate_block_bounds(block_start, bitmap_size)?;
            let bitmap = self.load_bitmap(block_start, bitmap_size)?;

            let mut within = within_block;
            let mut remaining = chunk_len;
            while remaining > 0 {
                let sector_in_block = within / SECTOR_SIZE as u64;
                if sector_in_block >= sectors_per_block {
                    return Err(DiskError::CorruptImage("vhd sector index out of range"));
                }

                let allocated = Self::bitmap_get(bitmap.as_slice(), sector_in_block)?;
                let run_len_u64 = Self::sector_run_len(
                    bitmap.as_slice(),
                    sectors_per_block,
                    within,
                    remaining as u64,
                    allocated,
                )?;
                let run_len: usize = run_len_u64
                    .try_into()
                    .map_err(|_| DiskError::Unsupported("vhd read too large"))?;

                if allocated {
                    let phys = block_start
                        .checked_add(bitmap_size)
                        .and_then(|v| v.checked_add(within))
                        .ok_or(DiskError::OffsetOverflow)?;
                    self.backend_read_at(
                        phys,
                        &mut buf[pos..pos + run_len],
                        "vhd block data truncated",
                    )?;
                } else {
                    buf[pos..pos + run_len].fill(0);
                }

                within = within
                    .checked_add(run_len_u64)
                    .ok_or(DiskError::OffsetOverflow)?;
                pos += run_len;
                remaining -= run_len;
            }

            debug_assert_eq!(remaining, 0);
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

fn vhd_checksum_footer(raw: &[u8; SECTOR_SIZE]) -> u32 {
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
