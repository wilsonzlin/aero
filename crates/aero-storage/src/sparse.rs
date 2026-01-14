use crate::util::{align_up_u64, checked_range, div_ceil_u64};
use crate::{DiskError, Result, StorageBackend, VirtualDisk, SECTOR_SIZE};

const MAGIC: &[u8; 8] = b"AEROSPAR";
const VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;
const ZERO_BUF: [u8; 4096] = [0; 4096];

// Hard cap to avoid absurd allocations from untrusted images.
const MAX_TABLE_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB
const MAX_TABLE_ENTRIES: u64 = MAX_TABLE_BYTES / 8;
// Keep allocation units bounded. Extremely large block sizes cause pathological I/O patterns
// (e.g. allocating a single block can require zero-filling gigabytes).
//
// Note: this cap should stay in sync with snapshot-layer overlay validation
// (`MAX_OVERLAY_BLOCK_SIZE_BYTES` in `aero-io-snapshot`).
const MAX_BLOCK_SIZE_BYTES: u32 = 64 * 1024 * 1024; // 64 MiB

/// Parameters used when creating a new sparse disk.
#[derive(Copy, Clone, Debug)]
pub struct AeroSparseConfig {
    pub disk_size_bytes: u64,
    /// Allocation unit size.
    ///
    /// Larger blocks reduce metadata size and improve sequential throughput, but increase
    /// write amplification for small random writes. 1 MiB is a good starting point.
    pub block_size_bytes: u32,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct AeroSparseHeader {
    pub version: u32,
    pub block_size_bytes: u32,
    pub disk_size_bytes: u64,
    pub table_entries: u64,
    pub data_offset: u64,
    pub allocated_blocks: u64,
}

impl AeroSparseHeader {
    pub fn encode(&self) -> [u8; HEADER_SIZE] {
        let mut out = [0u8; HEADER_SIZE];
        out[0..8].copy_from_slice(MAGIC);
        out[8..12].copy_from_slice(&self.version.to_le_bytes());
        out[12..16].copy_from_slice(&(HEADER_SIZE as u32).to_le_bytes());
        out[16..20].copy_from_slice(&self.block_size_bytes.to_le_bytes());
        out[20..24].copy_from_slice(&0u32.to_le_bytes()); // reserved
        out[24..32].copy_from_slice(&self.disk_size_bytes.to_le_bytes());
        out[32..40].copy_from_slice(&(HEADER_SIZE as u64).to_le_bytes()); // table_offset
        out[40..48].copy_from_slice(&self.table_entries.to_le_bytes());
        out[48..56].copy_from_slice(&self.data_offset.to_le_bytes());
        out[56..64].copy_from_slice(&self.allocated_blocks.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let magic = bytes
            .get(0..8)
            .ok_or(DiskError::InvalidSparseHeader("header too small"))?;
        if magic != MAGIC {
            return Err(DiskError::InvalidSparseHeader("bad magic"));
        }
        let version = u32::from_le_bytes(
            bytes
                .get(8..12)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        if version != VERSION {
            return Err(DiskError::InvalidSparseHeader("unsupported version"));
        }
        let header_size = u32::from_le_bytes(
            bytes
                .get(12..16)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        if header_size as usize != HEADER_SIZE {
            return Err(DiskError::InvalidSparseHeader("unexpected header size"));
        }
        let block_size_bytes = u32::from_le_bytes(
            bytes
                .get(16..20)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        let disk_size_bytes = u64::from_le_bytes(
            bytes
                .get(24..32)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        let table_offset = u64::from_le_bytes(
            bytes
                .get(32..40)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        if table_offset != HEADER_SIZE as u64 {
            return Err(DiskError::InvalidSparseHeader("unsupported table offset"));
        }
        let table_entries = u64::from_le_bytes(
            bytes
                .get(40..48)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        let data_offset = u64::from_le_bytes(
            bytes
                .get(48..56)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );
        let allocated_blocks = u64::from_le_bytes(
            bytes
                .get(56..64)
                .ok_or(DiskError::InvalidSparseHeader("header too small"))?
                .try_into()
                .map_err(|_| DiskError::InvalidSparseHeader("header too small"))?,
        );

        // Validate header invariants. This is intentionally strict because the image may be
        // untrusted/corrupt, and later code assumes these values are sane.
        let block_size = block_size_bytes as u64;
        if block_size == 0 || !block_size.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::InvalidSparseHeader(
                "block_size must be a non-zero multiple of 512",
            ));
        }
        if !block_size_bytes.is_power_of_two() {
            return Err(DiskError::InvalidSparseHeader(
                "block_size must be power of two",
            ));
        }
        if block_size_bytes > MAX_BLOCK_SIZE_BYTES {
            return Err(DiskError::InvalidSparseHeader("block_size too large"));
        }
        if disk_size_bytes == 0 || !disk_size_bytes.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::InvalidSparseHeader(
                "disk_size must be a non-zero multiple of 512",
            ));
        }
        if table_entries == 0 {
            return Err(DiskError::InvalidSparseHeader(
                "table_entries must be non-zero",
            ));
        }

        let expected_table_entries = div_ceil_u64(disk_size_bytes, block_size)?;
        if expected_table_entries != table_entries {
            return Err(DiskError::InvalidSparseHeader("unexpected table_entries"));
        }

        let expected_table_bytes = table_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;

        // Reject absurd allocation tables *before* validating `data_offset` so opening a corrupt
        // image never attempts to compute or allocate based on extreme values.
        if expected_table_bytes > MAX_TABLE_BYTES || table_entries > MAX_TABLE_ENTRIES {
            return Err(DiskError::Unsupported(
                "aerosparse allocation table too large",
            ));
        }

        let table_end = (HEADER_SIZE as u64)
            .checked_add(expected_table_bytes)
            .ok_or(DiskError::OffsetOverflow)?;
        let expected_data_offset = align_up_u64(table_end, block_size)?;
        if expected_data_offset != data_offset {
            return Err(DiskError::InvalidSparseHeader("unexpected data_offset"));
        }

        if allocated_blocks > table_entries {
            return Err(DiskError::InvalidSparseHeader(
                "allocated_blocks exceeds table_entries",
            ));
        }

        Ok(Self {
            version,
            block_size_bytes,
            disk_size_bytes,
            table_entries,
            data_offset,
            allocated_blocks,
        })
    }

    pub fn block_size_u64(&self) -> u64 {
        self.block_size_bytes as u64
    }
}

/// Aero-specific sparse disk format.
///
/// The file layout is:
/// - Header (64 bytes)
/// - Allocation table (`table_entries` u64s). Each entry stores the physical byte offset
///   of the data block, or 0 if unallocated.
/// - Data area: fixed-size blocks appended as they are allocated.
pub struct AeroSparseDisk<B> {
    backend: B,
    header: AeroSparseHeader,
    table: Vec<u64>,
    /// Bitset tracking which physical block slots are currently referenced by the allocation
    /// table. This enables block deallocation + reuse without any extra on-disk metadata.
    ///
    /// `header.allocated_blocks` is treated as the total number of physical block slots in the
    /// data region (i.e. the high-water mark for appended blocks). The number of non-zero table
    /// entries may be smaller due to deallocations.
    phys_used: Vec<u64>,
    mapped_blocks: u64,
}

impl<B: StorageBackend> AeroSparseDisk<B> {
    pub fn create(mut backend: B, cfg: AeroSparseConfig) -> Result<Self> {
        let block_size = cfg.block_size_bytes as u64;
        if block_size == 0 || !block_size.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::InvalidSparseHeader(
                "block_size must be a non-zero multiple of 512",
            ));
        }
        if !cfg.block_size_bytes.is_power_of_two() {
            return Err(DiskError::InvalidSparseHeader(
                "block_size must be power of two",
            ));
        }
        if cfg.block_size_bytes > MAX_BLOCK_SIZE_BYTES {
            return Err(DiskError::InvalidSparseHeader("block_size too large"));
        }
        if cfg.disk_size_bytes == 0 || !cfg.disk_size_bytes.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::InvalidSparseHeader(
                "disk_size must be a non-zero multiple of 512",
            ));
        }

        let table_entries = div_ceil_u64(cfg.disk_size_bytes, block_size)?;
        let table_entries_usize: usize = table_entries
            .try_into()
            .map_err(|_| DiskError::InvalidConfig("aerosparse allocation table too large"))?;
        let table_bytes = table_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;
        if table_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::InvalidConfig(
                "aerosparse allocation table too large",
            ));
        }
        let table_end = (HEADER_SIZE as u64)
            .checked_add(table_bytes)
            .ok_or(DiskError::OffsetOverflow)?;
        let data_offset = align_up_u64(table_end, block_size)?;

        let header = AeroSparseHeader {
            version: VERSION,
            block_size_bytes: cfg.block_size_bytes,
            disk_size_bytes: cfg.disk_size_bytes,
            table_entries,
            data_offset,
            allocated_blocks: 0,
        };

        // Ensure the table region exists (filled with zeros).
        backend.set_len(data_offset)?;
        backend.write_at(0, &header.encode())?;

        let mut table: Vec<u64> = Vec::new();
        table
            .try_reserve_exact(table_entries_usize)
            .map_err(|_| DiskError::InvalidConfig("aerosparse allocation table too large"))?;
        table.resize(table_entries_usize, 0);

        Ok(Self {
            backend,
            header,
            table,
            phys_used: Vec::new(),
            mapped_blocks: 0,
        })
    }

    pub fn open(mut backend: B) -> Result<Self> {
        let mut header_bytes = [0u8; HEADER_SIZE];
        backend.read_at(0, &mut header_bytes).map_err(|e| match e {
            DiskError::OutOfBounds { .. } => {
                DiskError::CorruptSparseImage("truncated sparse header")
            }
            other => other,
        })?;
        let header = AeroSparseHeader::decode(&header_bytes)?;

        let block_size = header.block_size_u64();
        let expected_table_bytes = header
            .table_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;
        if expected_table_bytes > MAX_TABLE_BYTES || header.table_entries > MAX_TABLE_ENTRIES {
            return Err(DiskError::Unsupported(
                "aerosparse allocation table too large",
            ));
        }
        let expected_table_bytes_usize: usize = expected_table_bytes
            .try_into()
            .map_err(|_| DiskError::Unsupported("aerosparse allocation table too large"))?;
        let table_entries_usize: usize = header
            .table_entries
            .try_into()
            .map_err(|_| DiskError::Unsupported("aerosparse allocation table too large"))?;

        let table_end = (HEADER_SIZE as u64)
            .checked_add(expected_table_bytes)
            .ok_or(DiskError::OffsetOverflow)?;

        let backend_len = backend.len()?;

        // Validate the image isn't truncated before reading the allocation table.
        if backend_len < table_end {
            return Err(DiskError::CorruptSparseImage(
                "allocation table out of bounds",
            ));
        }
        if backend_len < header.data_offset {
            return Err(DiskError::CorruptSparseImage("data region out of bounds"));
        }

        let expected_min_len = header
            .data_offset
            .checked_add(
                header
                    .allocated_blocks
                    .checked_mul(block_size)
                    .ok_or(DiskError::OffsetOverflow)?,
            )
            .ok_or(DiskError::OffsetOverflow)?;
        if backend_len < expected_min_len {
            return Err(DiskError::CorruptSparseImage(
                "allocated blocks extend beyond end of image",
            ));
        }

        // Prepare allocation table validation state.
        let mut mapped_blocks = 0u64;
        let allocated_blocks_usize: usize = header
            .allocated_blocks
            .try_into()
            .map_err(|_| DiskError::CorruptSparseImage("allocated_blocks out of range"))?;
        // Use a bitset instead of `Vec<bool>` to keep validation overhead small even for
        // large tables (important on wasm32).
        let bitset_len = allocated_blocks_usize
            .checked_add(63)
            .ok_or(DiskError::OffsetOverflow)?
            / 64;
        let mut seen_phys_idx: Vec<u64> = Vec::new();
        seen_phys_idx
            .try_reserve_exact(bitset_len)
            .map_err(|_| DiskError::Unsupported("aerosparse allocation table too large"))?;
        seen_phys_idx.resize(bitset_len, 0);

        // Read allocation table.
        //
        // IMPORTANT:
        // - Don't allocate a single `Vec<u8>` for the full table.
        // - Use fallible allocations (`try_reserve_exact`) so we return a structured error
        //   instead of aborting on OOM (especially important on wasm32).
        let mut table = Vec::new();
        table
            .try_reserve_exact(table_entries_usize)
            .map_err(|_| DiskError::Unsupported("aerosparse allocation table too large"))?;

        // Buffer used to stream the allocation table from the backend.
        // Must be a multiple of 8 since table entries are u64s.
        let mut buf: Vec<u8> = Vec::new();
        buf.try_reserve_exact(64 * 1024)
            .map_err(|_| DiskError::Unsupported("aerosparse allocation table too large"))?;
        buf.resize(64 * 1024, 0);
        let mut offset = HEADER_SIZE as u64;
        let mut remaining = expected_table_bytes_usize;
        while remaining > 0 {
            let read_len = remaining.min(buf.len());
            backend
                .read_at(offset, &mut buf[..read_len])
                .map_err(|e| match e {
                    DiskError::OutOfBounds { .. } => {
                        DiskError::CorruptSparseImage("allocation table out of bounds")
                    }
                    other => other,
                })?;
            for chunk in buf[..read_len].chunks_exact(8) {
                let bytes: [u8; 8] = chunk
                    .try_into()
                    .map_err(|_| DiskError::CorruptSparseImage("allocation table chunk size"))?;
                let phys = u64::from_le_bytes(bytes);
                table.push(phys);

                if phys == 0 {
                    continue;
                }

                // Validate entries as we go so corrupt images fail fast without reading the
                // entire allocation table first.
                mapped_blocks = mapped_blocks
                    .checked_add(1)
                    .ok_or(DiskError::OffsetOverflow)?;

                if phys < header.data_offset {
                    return Err(DiskError::CorruptSparseImage(
                        "data block offset before data region",
                    ));
                }
                let rel = phys - header.data_offset;
                if !rel.is_multiple_of(block_size) {
                    return Err(DiskError::CorruptSparseImage(
                        "misaligned data block offset",
                    ));
                }

                let phys_idx = rel / block_size;
                if phys_idx >= header.allocated_blocks {
                    return Err(DiskError::CorruptSparseImage(
                        "data block offset out of bounds",
                    ));
                }
                let phys_end =
                    phys.checked_add(block_size)
                        .ok_or(DiskError::CorruptSparseImage(
                            "data block offset out of bounds",
                        ))?;
                if phys_end > expected_min_len {
                    return Err(DiskError::CorruptSparseImage(
                        "data block offset out of bounds",
                    ));
                }

                let phys_idx_usize: usize = phys_idx.try_into().map_err(|_| {
                    DiskError::CorruptSparseImage("data block offset out of bounds")
                })?;
                let word_idx = phys_idx_usize / 64;
                let bit_idx = phys_idx_usize % 64;
                let mask = 1u64 << bit_idx;
                let word = seen_phys_idx
                    .get_mut(word_idx)
                    .ok_or(DiskError::CorruptSparseImage(
                        "data block offset out of bounds",
                    ))?;
                if (*word & mask) != 0 {
                    return Err(DiskError::CorruptSparseImage("duplicate data block offset"));
                }
                *word |= mask;
            }
            offset = offset
                .checked_add(read_len as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            remaining -= read_len;
        }

        Ok(Self {
            backend,
            header,
            table,
            phys_used: seen_phys_idx,
            mapped_blocks,
        })
    }

    pub fn header(&self) -> &AeroSparseHeader {
        &self.header
    }

    pub fn is_block_allocated(&self, block_idx: u64) -> bool {
        let Ok(idx) = usize::try_from(block_idx) else {
            return false;
        };
        self.table.get(idx).is_some_and(|&off| off != 0)
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    /// Number of currently allocated logical blocks (non-zero entries in the allocation table).
    ///
    /// Note: this can be smaller than `header().allocated_blocks` since the latter tracks the
    /// total number of physical block slots in the data region (including freed/unreferenced
    /// slots).
    pub fn allocated_block_count(&self) -> u64 {
        self.mapped_blocks
    }

    fn phys_offset_for_idx(&self, phys_idx: u64) -> Result<u64> {
        let block_size = self.header.block_size_u64();
        self.header
            .data_offset
            .checked_add(
                phys_idx
                    .checked_mul(block_size)
                    .ok_or(DiskError::OffsetOverflow)?,
            )
            .ok_or(DiskError::OffsetOverflow)
    }

    fn set_phys_used(&mut self, phys_idx: u64, used: bool) -> Result<()> {
        let phys_idx_usize: usize = phys_idx
            .try_into()
            .map_err(|_| DiskError::CorruptSparseImage("data block offset out of bounds"))?;
        let word_idx = phys_idx_usize / 64;
        let bit_idx = phys_idx_usize % 64;
        let mask = 1u64 << bit_idx;
        let word = self
            .phys_used
            .get_mut(word_idx)
            .ok_or(DiskError::CorruptSparseImage(
                "data block offset out of bounds",
            ))?;
        if used {
            *word |= mask;
        } else {
            *word &= !mask;
        }
        Ok(())
    }

    fn is_phys_used(&self, phys_idx: u64) -> Result<bool> {
        let phys_idx_usize: usize = phys_idx
            .try_into()
            .map_err(|_| DiskError::CorruptSparseImage("data block offset out of bounds"))?;
        let word_idx = phys_idx_usize / 64;
        let bit_idx = phys_idx_usize % 64;
        let mask = 1u64 << bit_idx;
        let word = self
            .phys_used
            .get(word_idx)
            .ok_or(DiskError::CorruptSparseImage(
                "data block offset out of bounds",
            ))?;
        Ok((*word & mask) != 0)
    }

    fn find_free_phys_idx(&mut self) -> Result<Option<u64>> {
        let total = self.header.allocated_blocks;
        for (word_idx, word) in self.phys_used.iter_mut().enumerate() {
            if *word == u64::MAX {
                continue;
            }
            let inv = !*word;
            let bit = inv.trailing_zeros() as usize;
            let idx_usize = word_idx
                .checked_mul(64)
                .and_then(|v| v.checked_add(bit))
                .ok_or(DiskError::OffsetOverflow)?;
            let idx = u64::try_from(idx_usize).map_err(|_| DiskError::OffsetOverflow)?;
            if idx >= total {
                // The remaining bits in this (final) word are outside the valid phys_idx range.
                return Ok(None);
            }
            *word |= 1u64 << bit;
            return Ok(Some(idx));
        }
        Ok(None)
    }

    fn trim_trailing_free_phys(&mut self) -> Result<()> {
        if self.header.allocated_blocks == 0 {
            return Ok(());
        }

        let mut new_allocated = self.header.allocated_blocks;
        while new_allocated > 0 {
            let last = new_allocated - 1;
            if self.is_phys_used(last)? {
                break;
            }
            new_allocated -= 1;
        }

        if new_allocated == self.header.allocated_blocks {
            return Ok(());
        }

        self.header.allocated_blocks = new_allocated;

        let new_len_usize: usize = new_allocated
            .div_ceil(64)
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        self.phys_used.truncate(new_len_usize);

        // Persist the updated header before truncating the backend, to ensure the image is
        // never left in a state where it references data beyond the backend length.
        self.backend.write_at(0, &self.header.encode())?;

        let block_size = self.header.block_size_u64();
        let new_end = self
            .header
            .data_offset
            .checked_add(
                new_allocated
                    .checked_mul(block_size)
                    .ok_or(DiskError::OffsetOverflow)?,
            )
            .ok_or(DiskError::OffsetOverflow)?;
        let cur_len = self.backend.len()?;
        if new_end < cur_len {
            self.backend.set_len(new_end)?;
        }
        Ok(())
    }

    pub(crate) fn ensure_block_allocated(&mut self, block_idx: u64) -> Result<(u64, bool)> {
        let block_idx_usize: usize = block_idx
            .try_into()
            .map_err(|_| DiskError::CorruptSparseImage("block index out of range"))?;
        let entry = *self
            .table
            .get(block_idx_usize)
            .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;

        if entry != 0 {
            return Ok((entry, true));
        }

        let block_size = self.header.block_size_u64();
        let data_offset = self.header.data_offset;

        let (phys, new_phys_slot) = if let Some(phys_idx) = self.find_free_phys_idx()? {
            (self.phys_offset_for_idx(phys_idx)?, false)
        } else {
            let phys_idx = self.header.allocated_blocks;
            let phys = data_offset
                .checked_add(
                    phys_idx
                        .checked_mul(block_size)
                        .ok_or(DiskError::OffsetOverflow)?,
                )
                .ok_or(DiskError::OffsetOverflow)?;

            self.header.allocated_blocks = self
                .header
                .allocated_blocks
                .checked_add(1)
                .ok_or(DiskError::OffsetOverflow)?;

            let new_len_usize: usize = self
                .header
                .allocated_blocks
                .div_ceil(64)
                .try_into()
                .map_err(|_| DiskError::OffsetOverflow)?;
            if self.phys_used.len() < new_len_usize {
                self.phys_used.resize(new_len_usize, 0);
            }
            self.set_phys_used(phys_idx, true)?;

            (phys, true)
        };

        let entry = self
            .table
            .get_mut(block_idx_usize)
            .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;
        *entry = phys;
        self.mapped_blocks = self
            .mapped_blocks
            .checked_add(1)
            .ok_or(DiskError::OffsetOverflow)?;

        // Persist the updated header (if changed) and the single updated table entry immediately.
        if new_phys_slot {
            self.backend.write_at(0, &self.header.encode())?;
        }
        let table_entry_off = (HEADER_SIZE as u64)
            .checked_add(block_idx.checked_mul(8).ok_or(DiskError::OffsetOverflow)?)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend
            .write_at(table_entry_off, &phys.to_le_bytes())?;

        // Ensure the file covers the newly allocated block (some backends rely on set_len).
        let end = phys
            .checked_add(block_size)
            .ok_or(DiskError::OffsetOverflow)?;
        if end > self.backend.len()? {
            self.backend.set_len(end)?;
        }

        Ok((phys, false))
    }

    fn deallocate_block_inner(&mut self, block_idx: u64) -> Result<bool> {
        let block_idx_usize: usize = block_idx
            .try_into()
            .map_err(|_| DiskError::CorruptSparseImage("block index out of range"))?;
        let phys = *self
            .table
            .get(block_idx_usize)
            .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;
        if phys == 0 {
            return Ok(false);
        }

        // Mark the logical block as unallocated first; the old physical data is treated as
        // unreachable once the allocation table entry is cleared.
        let entry = self
            .table
            .get_mut(block_idx_usize)
            .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;
        *entry = 0;

        let table_entry_off = (HEADER_SIZE as u64)
            .checked_add(block_idx.checked_mul(8).ok_or(DiskError::OffsetOverflow)?)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend
            .write_at(table_entry_off, &0u64.to_le_bytes())?;

        // Update the in-memory phys-used bitmap.
        let block_size = self.header.block_size_u64();
        if phys < self.header.data_offset {
            return Err(DiskError::CorruptSparseImage(
                "data block offset before data region",
            ));
        }
        let rel = phys - self.header.data_offset;
        if !rel.is_multiple_of(block_size) {
            return Err(DiskError::CorruptSparseImage(
                "misaligned data block offset",
            ));
        }
        let phys_idx = rel / block_size;
        if phys_idx >= self.header.allocated_blocks {
            return Err(DiskError::CorruptSparseImage(
                "data block offset out of bounds",
            ));
        }
        self.set_phys_used(phys_idx, false)?;

        self.mapped_blocks =
            self.mapped_blocks
                .checked_sub(1)
                .ok_or(DiskError::CorruptSparseImage(
                    "allocated_blocks out of range",
                ))?;

        Ok(true)
    }

    /// Deallocate a single logical block.
    ///
    /// Returns `Ok(true)` if the block was previously allocated, `Ok(false)` if it was already
    /// unallocated.
    pub fn deallocate_block(&mut self, block_idx: u64) -> Result<bool> {
        let existed = self.deallocate_block_inner(block_idx)?;
        if existed {
            self.trim_trailing_free_phys()?;
        }
        Ok(existed)
    }

    /// Deallocate all *fully covered* blocks in the given byte range.
    ///
    /// Blocks are only deallocated if the discard range covers the entire block; partial blocks
    /// are left allocated (callers that need partial-zeroing should write zeros instead).
    pub fn discard_range(&mut self, offset: u64, len: u64) -> Result<()> {
        let capacity = self.header.disk_size_bytes;
        if len == 0 {
            // Still validate offset is in-bounds.
            if offset > capacity {
                return Err(DiskError::OutOfBounds {
                    offset,
                    len: 0,
                    capacity,
                });
            }
            return Ok(());
        }

        let end = offset.checked_add(len).ok_or(DiskError::OffsetOverflow)?;
        if end > capacity {
            return Err(DiskError::OutOfBounds {
                offset,
                len: usize::try_from(len).unwrap_or(usize::MAX),
                capacity,
            });
        }

        let block_size = self.header.block_size_u64();
        let start_block = div_ceil_u64(offset, block_size)?;
        let end_block = end / block_size;
        if start_block >= end_block {
            return Ok(());
        }

        for block_idx in start_block..end_block {
            self.deallocate_block_inner(block_idx)?;
        }

        self.trim_trailing_free_phys()
    }

    pub(crate) fn read_from_alloc_table(
        &mut self,
        phys: u64,
        offset_in_block: usize,
        dst: &mut [u8],
    ) -> Result<()> {
        let phys_off = phys
            .checked_add(offset_in_block as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.read_at(phys_off, dst)
    }

    pub(crate) fn write_to_alloc_table(
        &mut self,
        phys: u64,
        offset_in_block: usize,
        src: &[u8],
    ) -> Result<()> {
        let phys_off = phys
            .checked_add(offset_in_block as u64)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.write_at(phys_off, src)
    }

    fn write_zeros_in_block(
        &mut self,
        phys: u64,
        offset_in_block: usize,
        len: usize,
    ) -> Result<()> {
        let mut remaining = len;
        let mut off = offset_in_block;
        while remaining > 0 {
            let chunk_len = remaining.min(ZERO_BUF.len());
            self.write_to_alloc_table(phys, off, &ZERO_BUF[..chunk_len])?;
            off += chunk_len;
            remaining -= chunk_len;
        }
        Ok(())
    }
}

impl<B: StorageBackend + crate::disk::VirtualDiskSend> VirtualDisk for AeroSparseDisk<B> {
    fn capacity_bytes(&self) -> u64 {
        self.header.disk_size_bytes
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let block_size = self.header.block_size_u64();
        let block_size_usize: usize = block_size
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / block_size;
            let within = (abs % block_size) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (block_size_usize - within).min(remaining);

            let block_idx_usize: usize = block_idx
                .try_into()
                .map_err(|_| DiskError::CorruptSparseImage("block index out of range"))?;
            let phys = *self
                .table
                .get(block_idx_usize)
                .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;
            if phys == 0 {
                buf[pos..pos + chunk_len].fill(0);
            } else {
                self.read_from_alloc_table(phys, within, &mut buf[pos..pos + chunk_len])?;
            }

            pos += chunk_len;
        }

        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let block_size = self.header.block_size_u64();
        let block_size_usize: usize = block_size
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / block_size;
            let within = (abs % block_size) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (block_size_usize - within).min(remaining);

            let block_idx_usize: usize = block_idx
                .try_into()
                .map_err(|_| DiskError::CorruptSparseImage("block index out of range"))?;
            let entry = *self
                .table
                .get(block_idx_usize)
                .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;

            // Fast path: if the target block is unallocated, and we're only writing zeros, treat
            // this as a no-op to keep the image sparse.
            if entry == 0 && buf[pos..pos + chunk_len].iter().all(|&b| b == 0) {
                pos += chunk_len;
                continue;
            }

            let (phys, existed) = if entry != 0 {
                (entry, true)
            } else {
                self.ensure_block_allocated(block_idx)?
            };

            if !existed {
                if within > 0 {
                    self.write_zeros_in_block(phys, 0, within)?;
                }
                let end = within + chunk_len;
                if end < block_size_usize {
                    self.write_zeros_in_block(phys, end, block_size_usize - end)?;
                }
            }
            self.write_to_alloc_table(phys, within, &buf[pos..pos + chunk_len])?;

            pos += chunk_len;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        self.backend.flush()
    }

    fn discard_range(&mut self, offset: u64, len: u64) -> Result<()> {
        AeroSparseDisk::discard_range(self, offset, len)
    }
}
