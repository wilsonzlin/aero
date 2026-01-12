use crate::util::{align_up_u64, checked_range, div_ceil_u64};
use crate::{DiskError, Result, StorageBackend, VirtualDisk, SECTOR_SIZE};

const MAGIC: &[u8; 8] = b"AEROSPAR";
const VERSION: u32 = 1;
pub const HEADER_SIZE: usize = 64;
const ZERO_BUF: [u8; 4096] = [0; 4096];

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
        if bytes.len() < HEADER_SIZE {
            return Err(DiskError::InvalidSparseHeader("header too small"));
        }
        if &bytes[0..8] != MAGIC {
            return Err(DiskError::InvalidSparseHeader("bad magic"));
        }
        let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
        if version != VERSION {
            return Err(DiskError::InvalidSparseHeader("unsupported version"));
        }
        let header_size = u32::from_le_bytes(bytes[12..16].try_into().unwrap());
        if header_size as usize != HEADER_SIZE {
            return Err(DiskError::InvalidSparseHeader("unexpected header size"));
        }
        let block_size_bytes = u32::from_le_bytes(bytes[16..20].try_into().unwrap());
        let disk_size_bytes = u64::from_le_bytes(bytes[24..32].try_into().unwrap());
        let table_offset = u64::from_le_bytes(bytes[32..40].try_into().unwrap());
        if table_offset != HEADER_SIZE as u64 {
            return Err(DiskError::InvalidSparseHeader("unsupported table offset"));
        }
        let table_entries = u64::from_le_bytes(bytes[40..48].try_into().unwrap());
        let data_offset = u64::from_le_bytes(bytes[48..56].try_into().unwrap());
        let allocated_blocks = u64::from_le_bytes(bytes[56..64].try_into().unwrap());

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
        if cfg.disk_size_bytes == 0 {
            return Err(DiskError::InvalidSparseHeader("disk_size must be non-zero"));
        }

        let table_entries = div_ceil_u64(cfg.disk_size_bytes, block_size)?;
        let table_entries_usize: usize = table_entries
            .try_into()
            .map_err(|_| DiskError::InvalidConfig("allocation table too large"))?;
        let table_bytes = table_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;
        let data_offset = align_up_u64(HEADER_SIZE as u64 + table_bytes, block_size)?;

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

        Ok(Self {
            backend,
            header,
            table: vec![0; table_entries_usize],
        })
    }

    pub fn open(mut backend: B) -> Result<Self> {
        let mut header_bytes = [0u8; HEADER_SIZE];
        backend.read_at(0, &mut header_bytes)?;
        let header = AeroSparseHeader::decode(&header_bytes)?;

        let backend_len = backend.len()?;

        let block_size = header.block_size_u64();
        if block_size == 0 || !block_size.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::InvalidSparseHeader(
                "block_size must be a non-zero multiple of 512",
            ));
        }
        if !header.block_size_bytes.is_power_of_two() {
            return Err(DiskError::InvalidSparseHeader(
                "block_size must be power of two",
            ));
        }
        if header.disk_size_bytes == 0 {
            return Err(DiskError::InvalidSparseHeader("disk_size must be non-zero"));
        }
        if header.table_entries == 0 {
            return Err(DiskError::InvalidSparseHeader(
                "table_entries must be non-zero",
            ));
        }

        // Validate table_entries matches disk_size_bytes and block_size_bytes.
        let expected_table_entries = div_ceil_u64(header.disk_size_bytes, block_size)?;
        if expected_table_entries != header.table_entries {
            return Err(DiskError::InvalidSparseHeader("unexpected table_entries"));
        }

        // Validate data_offset.
        let expected_table_bytes = header
            .table_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;
        let expected_data_offset =
            align_up_u64(HEADER_SIZE as u64 + expected_table_bytes, block_size)?;
        if expected_data_offset != header.data_offset {
            return Err(DiskError::InvalidSparseHeader("unexpected data_offset"));
        }

        // Validate the image isn't truncated before reading the allocation table.
        let table_end = (HEADER_SIZE as u64)
            .checked_add(expected_table_bytes)
            .ok_or(DiskError::OffsetOverflow)?;
        if backend_len < table_end {
            return Err(DiskError::CorruptSparseImage(
                "allocation table out of bounds",
            ));
        }
        if backend_len < header.data_offset {
            return Err(DiskError::CorruptSparseImage("data region out of bounds"));
        }

        if header.allocated_blocks > header.table_entries {
            return Err(DiskError::InvalidSparseHeader(
                "allocated_blocks exceeds table_entries",
            ));
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

        // Read allocation table.
        let expected_table_bytes_usize: usize = expected_table_bytes
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        let table_entries_usize: usize = header
            .table_entries
            .try_into()
            .map_err(|_| DiskError::OffsetOverflow)?;
        let mut table_bytes = vec![0u8; expected_table_bytes_usize];
        backend.read_at(HEADER_SIZE as u64, &mut table_bytes)?;
        let mut table = Vec::with_capacity(table_entries_usize);
        for chunk in table_bytes.chunks_exact(8) {
            table.push(u64::from_le_bytes(chunk.try_into().unwrap()));
        }

        // Validate allocation table entries; corrupt entries can otherwise trigger huge writes
        // (e.g. MemBackend attempting to resize to a bogus physical offset).
        for &phys in &table {
            if phys == 0 {
                continue;
            }
            if phys < header.data_offset {
                return Err(DiskError::CorruptSparseImage(
                    "data block offset before data region",
                ));
            }
            let rel = phys - header.data_offset;
            if rel % block_size != 0 {
                return Err(DiskError::CorruptSparseImage(
                    "misaligned data block offset",
                ));
            }
            let phys_end = phys
                .checked_add(block_size)
                .ok_or(DiskError::OffsetOverflow)?;
            if phys_end > expected_min_len {
                return Err(DiskError::CorruptSparseImage(
                    "data block offset out of bounds",
                ));
            }
        }

        Ok(Self {
            backend,
            header,
            table,
        })
    }

    pub fn header(&self) -> &AeroSparseHeader {
        &self.header
    }

    pub fn is_block_allocated(&self, block_idx: u64) -> bool {
        self.table
            .get(block_idx as usize)
            .is_some_and(|&off| off != 0)
    }

    pub fn into_backend(self) -> B {
        self.backend
    }

    pub(crate) fn ensure_block_allocated(&mut self, block_idx: u64) -> Result<(u64, bool)> {
        let entry = self
            .table
            .get_mut(block_idx as usize)
            .ok_or(DiskError::CorruptSparseImage("block index out of range"))?;

        if *entry != 0 {
            return Ok((*entry, true));
        }

        let block_size = self.header.block_size_u64();
        let data_offset = self.header.data_offset;
        let phys = data_offset
            .checked_add(
                self.header
                    .allocated_blocks
                    .checked_mul(block_size)
                    .ok_or(DiskError::OffsetOverflow)?,
            )
            .ok_or(DiskError::OffsetOverflow)?;

        self.header.allocated_blocks = self
            .header
            .allocated_blocks
            .checked_add(1)
            .ok_or(DiskError::OffsetOverflow)?;
        *entry = phys;

        // Persist the updated header and the single updated table entry immediately.
        self.backend.write_at(0, &self.header.encode())?;
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

impl<B: StorageBackend> VirtualDisk for AeroSparseDisk<B> {
    fn capacity_bytes(&self) -> u64 {
        self.header.disk_size_bytes
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let block_size = self.header.block_size_u64();
        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / block_size;
            let within = (abs % block_size) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (block_size as usize - within).min(remaining);

            let phys = *self
                .table
                .get(block_idx as usize)
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

            let (phys, existed) = self.ensure_block_allocated(block_idx)?;

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
}
