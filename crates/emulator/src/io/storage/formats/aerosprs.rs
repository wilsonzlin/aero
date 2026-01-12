use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

const SPARSE_MAGIC: [u8; 8] = *b"AEROSPRS";
const SPARSE_VERSION: u32 = 1;
const HEADER_SIZE: u64 = 4096;
const JOURNAL_SIZE: u64 = 4096;
const JOURNAL_MAGIC: [u8; 4] = *b"JNL1";
// DoS guard: avoid allocating absurdly large in-memory allocation tables for untrusted images.
const MAX_TABLE_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB
                                                // DoS guard: keep allocation blocks bounded. Extremely large block sizes can cause pathological
                                                // behavior (e.g. allocating a single block can require zero-filling gigabytes).
                                                //
                                                // This cap intentionally matches the one used by the canonical `AEROSPAR` format in
                                                // `crates/aero-storage` so legacy images cannot request more work per block than current ones.
const MAX_BLOCK_SIZE_BYTES: u32 = 64 * 1024 * 1024; // 64 MiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SparseHeader {
    pub sector_size: u32,
    pub block_size: u32,
    pub total_sectors: u64,
    pub table_offset: u64,
    pub table_entries: u64,
    pub journal_offset: u64,
    pub data_offset: u64,
}

impl SparseHeader {
    fn encode(&self) -> [u8; HEADER_SIZE as usize] {
        let mut buf = [0u8; HEADER_SIZE as usize];
        buf[..8].copy_from_slice(&SPARSE_MAGIC);
        buf[8..12].copy_from_slice(&SPARSE_VERSION.to_le_bytes());
        buf[12..16].copy_from_slice(&self.sector_size.to_le_bytes());
        buf[16..20].copy_from_slice(&self.block_size.to_le_bytes());
        buf[20..28].copy_from_slice(&self.total_sectors.to_le_bytes());
        buf[28..36].copy_from_slice(&self.table_offset.to_le_bytes());
        buf[36..44].copy_from_slice(&self.table_entries.to_le_bytes());
        buf[44..52].copy_from_slice(&self.journal_offset.to_le_bytes());
        buf[52..60].copy_from_slice(&self.data_offset.to_le_bytes());
        buf
    }

    pub(crate) fn decode(buf: &[u8]) -> DiskResult<Self> {
        if buf.len() < HEADER_SIZE as usize {
            return Err(DiskError::CorruptImage("sparse header truncated"));
        }
        if buf[..8] != SPARSE_MAGIC {
            return Err(DiskError::CorruptImage("sparse magic mismatch"));
        }
        let version = u32::from_le_bytes(
            buf[8..12]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        if version != SPARSE_VERSION {
            return Err(DiskError::Unsupported("sparse version"));
        }
        let sector_size = u32::from_le_bytes(
            buf[12..16]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        let block_size = u32::from_le_bytes(
            buf[16..20]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        let total_sectors = u64::from_le_bytes(
            buf[20..28]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        let table_offset = u64::from_le_bytes(
            buf[28..36]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        let table_entries = u64::from_le_bytes(
            buf[36..44]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        let journal_offset = u64::from_le_bytes(
            buf[44..52]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );
        let data_offset = u64::from_le_bytes(
            buf[52..60]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse header truncated"))?,
        );

        if sector_size != 512 && sector_size != 4096 {
            return Err(DiskError::Unsupported("sector size (expected 512 or 4096)"));
        }
        if block_size == 0 {
            return Err(DiskError::CorruptImage("invalid block size"));
        }
        if !(block_size as u64).is_multiple_of(sector_size as u64) {
            return Err(DiskError::CorruptImage(
                "block size must be multiple of sector size",
            ));
        }
        if block_size > MAX_BLOCK_SIZE_BYTES {
            return Err(DiskError::Unsupported("block size too large"));
        }
        if total_sectors == 0 {
            return Err(DiskError::CorruptImage("total sectors is zero"));
        }

        // Validate size math so later LBA->byte offset computation can't overflow.
        let total_bytes = total_sectors
            .checked_mul(sector_size as u64)
            .ok_or(DiskError::CorruptImage("disk size overflow"))?;

        let block_size_u64 = block_size as u64;
        let expected_entries = total_bytes.div_ceil(block_size_u64);
        if table_entries != expected_entries {
            return Err(DiskError::CorruptImage("table entries mismatch"));
        }

        let table_bytes = table_entries
            .checked_mul(8)
            .ok_or(DiskError::CorruptImage("table size overflow"))?;
        if table_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("allocation table too large"));
        }

        if table_offset < HEADER_SIZE {
            return Err(DiskError::CorruptImage("table offset invalid"));
        }
        if journal_offset < HEADER_SIZE {
            return Err(DiskError::CorruptImage("journal offset invalid"));
        }
        if data_offset < HEADER_SIZE {
            return Err(DiskError::CorruptImage("data offset invalid"));
        }
        if !table_offset.is_multiple_of(8) || !journal_offset.is_multiple_of(8) {
            return Err(DiskError::CorruptImage("metadata offset misaligned"));
        }

        let journal_end = journal_offset
            .checked_add(JOURNAL_SIZE)
            .ok_or(DiskError::CorruptImage("journal offset overflow"))?;
        if journal_end > table_offset {
            return Err(DiskError::CorruptImage("journal overlaps allocation table"));
        }

        let table_end = table_offset
            .checked_add(table_bytes)
            .ok_or(DiskError::CorruptImage("table offset overflow"))?;
        if table_end > data_offset {
            return Err(DiskError::CorruptImage(
                "allocation table overlaps data region",
            ));
        }
        if !data_offset.is_multiple_of(block_size_u64) {
            return Err(DiskError::CorruptImage("data offset not block aligned"));
        }

        Ok(Self {
            sector_size,
            block_size,
            total_sectors,
            table_offset,
            table_entries,
            journal_offset,
            data_offset,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct JournalRecord {
    state: u8,
    logical_block: u64,
    physical_offset: u64,
}

impl JournalRecord {
    fn empty() -> Self {
        Self {
            state: 0,
            logical_block: 0,
            physical_offset: 0,
        }
    }

    fn encode(&self) -> [u8; JOURNAL_SIZE as usize] {
        let mut buf = [0u8; JOURNAL_SIZE as usize];
        buf[..4].copy_from_slice(&JOURNAL_MAGIC);
        buf[4] = self.state;
        buf[8..16].copy_from_slice(&self.logical_block.to_le_bytes());
        buf[16..24].copy_from_slice(&self.physical_offset.to_le_bytes());
        buf
    }

    fn decode(buf: &[u8]) -> DiskResult<Self> {
        if buf.len() < JOURNAL_SIZE as usize {
            return Err(DiskError::CorruptImage("sparse journal truncated"));
        }
        if buf[..4] != JOURNAL_MAGIC {
            // Treat missing magic as empty journal for forward compatibility.
            return Ok(Self::empty());
        }
        let state = buf[4];
        let logical_block = u64::from_le_bytes(
            buf[8..16]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse journal truncated"))?,
        );
        let physical_offset = u64::from_le_bytes(
            buf[16..24]
                .try_into()
                .map_err(|_| DiskError::CorruptImage("sparse journal truncated"))?,
        );
        Ok(Self {
            state,
            logical_block,
            physical_offset,
        })
    }
}

pub struct SparseDisk<S> {
    storage: S,
    header: SparseHeader,
    table: Vec<u64>,
}

impl<S: ByteStorage> SparseDisk<S> {
    pub fn create(
        mut storage: S,
        sector_size: u32,
        total_sectors: u64,
        block_size: u32,
    ) -> DiskResult<Self> {
        if sector_size != 512 && sector_size != 4096 {
            return Err(DiskError::Unsupported("sector size (expected 512 or 4096)"));
        }
        if block_size == 0 || !(block_size as u64).is_multiple_of(sector_size as u64) {
            return Err(DiskError::Unsupported(
                "block size must be a multiple of sector size",
            ));
        }
        if block_size > MAX_BLOCK_SIZE_BYTES {
            return Err(DiskError::Unsupported("block size too large"));
        }

        let total_bytes = total_sectors
            .checked_mul(sector_size as u64)
            .ok_or(DiskError::Unsupported("disk size overflow"))?;
        let block_size_u64 = block_size as u64;
        let table_entries = total_bytes.div_ceil(block_size_u64);
        let table_bytes = table_entries
            .checked_mul(8)
            .ok_or(DiskError::Unsupported("table size overflow"))?;
        if table_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("allocation table too large"));
        }

        let journal_offset = HEADER_SIZE;
        let table_offset = journal_offset
            .checked_add(JOURNAL_SIZE)
            .ok_or(DiskError::Unsupported("table offset overflow"))?;
        let table_end = table_offset
            .checked_add(table_bytes)
            .ok_or(DiskError::Unsupported("table offset overflow"))?;
        let data_offset = align_up(table_end, block_size_u64)?;

        let header = SparseHeader {
            sector_size,
            block_size,
            total_sectors,
            table_offset,
            table_entries,
            journal_offset,
            data_offset,
        };

        storage.write_at(0, &header.encode())?;
        storage.write_at(journal_offset, &JournalRecord::empty().encode())?;
        write_zeroes(&mut storage, table_offset, table_bytes)?;
        storage.set_len(data_offset)?;
        storage.flush()?;

        let table_entries_usize: usize = table_entries
            .try_into()
            .map_err(|_| DiskError::Unsupported("allocation table too large"))?;
        let mut table = Vec::new();
        table
            .try_reserve_exact(table_entries_usize)
            .map_err(|_| DiskError::QuotaExceeded)?;
        table.resize(table_entries_usize, 0);
        Ok(Self {
            storage,
            header,
            table,
        })
    }

    pub fn open(mut storage: S) -> DiskResult<Self> {
        let mut header_buf = [0u8; HEADER_SIZE as usize];
        match storage.read_at(0, &mut header_buf) {
            Ok(()) => {}
            Err(DiskError::OutOfBounds) => {
                return Err(DiskError::CorruptImage("sparse header truncated"));
            }
            Err(e) => return Err(e),
        }
        let header = SparseHeader::decode(&header_buf)?;

        let file_len = storage.len()?;
        if file_len < header.data_offset {
            return Err(DiskError::CorruptImage("sparse image truncated"));
        }

        let table_bytes = header
            .table_entries
            .checked_mul(8)
            .ok_or(DiskError::CorruptImage("table size overflow"))?;
        if table_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("allocation table too large"));
        }
        let table_entries_usize: usize = header
            .table_entries
            .try_into()
            .map_err(|_| DiskError::Unsupported("allocation table too large"))?;

        // Read the allocation table without allocating an additional full-size temporary buffer.
        //
        // This keeps opens lightweight even for large-but-valid tables, and prevents aborting on
        // OOM for corrupt images that claim extreme table sizes.
        let mut table = Vec::new();
        table
            .try_reserve_exact(table_entries_usize)
            .map_err(|_| DiskError::QuotaExceeded)?;
        let table_bytes_usize: usize = table_bytes
            .try_into()
            .map_err(|_| DiskError::Unsupported("allocation table too large"))?;
        let mut buf = Vec::new();
        buf.try_reserve_exact(64 * 1024)
            .map_err(|_| DiskError::QuotaExceeded)?;
        buf.resize(64 * 1024, 0);
        let mut remaining = table_bytes_usize;
        let mut off = header.table_offset;
        while remaining > 0 {
            let read_len = remaining.min(buf.len());
            match storage.read_at(off, &mut buf[..read_len]) {
                Ok(()) => {}
                Err(DiskError::OutOfBounds) => {
                    return Err(DiskError::CorruptImage("allocation table out of bounds"));
                }
                Err(e) => return Err(e),
            }
            for chunk in buf[..read_len].chunks_exact(8) {
                let bytes: [u8; 8] = chunk
                    .try_into()
                    .map_err(|_| DiskError::CorruptImage("allocation table decode error"))?;
                table.push(u64::from_le_bytes(bytes));
            }
            off = off
                .checked_add(read_len as u64)
                .ok_or(DiskError::OutOfBounds)?;
            remaining -= read_len;
        }

        let mut disk = Self {
            storage,
            header,
            table,
        };
        disk.recover_journal()?;
        Ok(disk)
    }

    pub fn header(&self) -> &SparseHeader {
        &self.header
    }

    pub fn allocated_blocks(&self) -> impl Iterator<Item = (u64, u64)> + '_ {
        self.table
            .iter()
            .enumerate()
            .filter_map(|(idx, &phys)| (phys != 0).then_some((idx as u64, phys)))
    }

    pub fn into_storage(self) -> S {
        self.storage
    }

    fn recover_journal(&mut self) -> DiskResult<()> {
        let mut jbuf = [0u8; JOURNAL_SIZE as usize];
        self.storage
            .read_at(self.header.journal_offset, &mut jbuf)?;
        let record = JournalRecord::decode(&jbuf)?;
        if record.state == 0 {
            return Ok(());
        }
        if record.logical_block >= self.header.table_entries {
            return Err(DiskError::CorruptImage(
                "journal logical block out of range",
            ));
        }
        if record.physical_offset != 0
            && !record
                .physical_offset
                .is_multiple_of(self.header.block_size as u64)
        {
            return Err(DiskError::CorruptImage(
                "journal physical offset not aligned",
            ));
        }
        let idx: usize = record
            .logical_block
            .try_into()
            .map_err(|_| DiskError::CorruptImage("journal logical block out of range"))?;
        if idx >= self.table.len() {
            return Err(DiskError::CorruptImage(
                "journal logical block out of range",
            ));
        }
        let existing = self.table[idx];
        if existing != 0 && existing != record.physical_offset {
            return Err(DiskError::CorruptImage(
                "journal conflicts with allocation table",
            ));
        }
        self.table[idx] = record.physical_offset;
        self.write_table_entry(record.logical_block, record.physical_offset)?;

        // Clearing the journal is idempotent; if we crash before it lands on disk the record will
        // be replayed again on next open.
        self.storage
            .write_at(self.header.journal_offset, &JournalRecord::empty().encode())?;
        self.storage.flush()?;
        Ok(())
    }

    fn write_table_entry(&mut self, logical_block: u64, physical_offset: u64) -> DiskResult<()> {
        let logical_block_bytes = logical_block
            .checked_mul(8)
            .ok_or(DiskError::Unsupported("table offset overflow"))?;
        let offset = self
            .header
            .table_offset
            .checked_add(logical_block_bytes)
            .ok_or(DiskError::Unsupported("table offset overflow"))?;
        self.storage
            .write_at(offset, &physical_offset.to_le_bytes())?;
        Ok(())
    }

    fn check_rw_range(&self, lba: u64, bytes: usize) -> DiskResult<(u64, u64)> {
        if !bytes.is_multiple_of(self.header.sector_size as usize) {
            return Err(DiskError::UnalignedBuffer {
                len: bytes,
                sector_size: self.header.sector_size,
            });
        }
        let sectors = (bytes / self.header.sector_size as usize) as u64;
        let end = lba.checked_add(sectors).ok_or(DiskError::OutOfRange {
            lba,
            sectors,
            capacity_sectors: self.header.total_sectors,
        })?;
        if end > self.header.total_sectors {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.header.total_sectors,
            });
        }
        Ok((sectors, end))
    }

    fn allocate_block(&mut self) -> DiskResult<u64> {
        let block_size = self.header.block_size as u64;
        let mut len = self.storage.len()?;
        if len < self.header.data_offset {
            len = self.header.data_offset;
        }
        let offset = align_up(len, block_size)?;
        // Ensure the newly allocated block is fully zero-initialized so partial writes preserve
        // the semantics of unallocated blocks returning zero.
        write_zeroes(&mut self.storage, offset, block_size)?;
        Ok(offset)
    }
}

impl<S: ByteStorage> DiskBackend for SparseDisk<S> {
    fn sector_size(&self) -> u32 {
        self.header.sector_size
    }

    fn total_sectors(&self) -> u64 {
        self.header.total_sectors
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        let (sectors, _) = self.check_rw_range(lba, buf.len())?;
        if sectors == 0 {
            return Ok(());
        }
        let sector_size = self.header.sector_size as u64;
        let block_size = self.header.block_size as u64;

        let mut remaining = buf;
        let mut cur_lba = lba;
        while !remaining.is_empty() {
            let byte_offset = cur_lba
                .checked_mul(sector_size)
                .ok_or(DiskError::OutOfRange {
                    lba,
                    sectors,
                    capacity_sectors: self.header.total_sectors,
                })?;
            let logical_block = byte_offset / block_size;
            let block_off = (byte_offset % block_size) as usize;
            let logical_block_idx: usize = logical_block
                .try_into()
                .map_err(|_| DiskError::CorruptImage("logical block out of range"))?;
            if logical_block_idx >= self.table.len() {
                return Err(DiskError::CorruptImage("logical block out of range"));
            }
            let physical = self.table[logical_block_idx];
            let max_in_block = (block_size as usize).saturating_sub(block_off);
            let to_copy = max_in_block.min(remaining.len());

            if physical == 0 {
                remaining[..to_copy].fill(0);
            } else {
                let phys = physical
                    .checked_add(block_off as u64)
                    .ok_or(DiskError::CorruptImage("physical offset overflow"))?;
                self.storage.read_at(phys, &mut remaining[..to_copy])?;
            }

            remaining = &mut remaining[to_copy..];
            cur_lba += (to_copy as u64) / sector_size;
        }
        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        let (sectors, _) = self.check_rw_range(lba, buf.len())?;
        if sectors == 0 {
            return Ok(());
        }
        let sector_size = self.header.sector_size as u64;
        let block_size = self.header.block_size as u64;

        let mut remaining = buf;
        let mut cur_lba = lba;
        while !remaining.is_empty() {
            let byte_offset = cur_lba
                .checked_mul(sector_size)
                .ok_or(DiskError::OutOfRange {
                    lba,
                    sectors,
                    capacity_sectors: self.header.total_sectors,
                })?;
            let logical_block = byte_offset / block_size;
            let block_off = (byte_offset % block_size) as usize;
            let max_in_block = (block_size as usize).saturating_sub(block_off);
            let to_copy = max_in_block.min(remaining.len());
            let idx: usize = logical_block
                .try_into()
                .map_err(|_| DiskError::CorruptImage("logical block out of range"))?;
            if idx >= self.table.len() {
                return Err(DiskError::CorruptImage("logical block out of range"));
            }

            let physical = if self.table[idx] == 0 {
                // If this sub-range is all zeros and the block is currently unallocated, we can
                // keep it sparse.
                if remaining[..to_copy].iter().all(|b| *b == 0) {
                    remaining = &remaining[to_copy..];
                    cur_lba += (to_copy as u64) / sector_size;
                    continue;
                }

                let new_physical = self.allocate_block()?;
                // Write data into the freshly zero-initialized block.
                let phys = new_physical
                    .checked_add(block_off as u64)
                    .ok_or(DiskError::CorruptImage("physical offset overflow"))?;
                self.storage.write_at(phys, &remaining[..to_copy])?;
                // Commit mapping via journal + table update.
                let rec = JournalRecord {
                    state: 1,
                    logical_block,
                    physical_offset: new_physical,
                };
                self.storage
                    .write_at(self.header.journal_offset, &rec.encode())?;
                self.storage.flush()?;

                self.table[idx] = new_physical;
                self.write_table_entry(logical_block, new_physical)?;
                self.storage.flush()?;

                self.storage
                    .write_at(self.header.journal_offset, &JournalRecord::empty().encode())?;
                // Clearing the journal doesn't need to be immediately flushed for correctness; the
                // table entry is the source of truth. We do it anyway to keep opens fast.
                self.storage.flush()?;
                new_physical
            } else {
                let physical = self.table[idx];
                let phys = physical
                    .checked_add(block_off as u64)
                    .ok_or(DiskError::CorruptImage("physical offset overflow"))?;
                self.storage.write_at(phys, &remaining[..to_copy])?;
                physical
            };

            let _ = physical; // reserved for future coalescing/trace hooks

            remaining = &remaining[to_copy..];
            cur_lba += (to_copy as u64) / sector_size;
        }
        Ok(())
    }

    fn flush(&mut self) -> DiskResult<()> {
        self.storage.flush()
    }
}

fn align_up(value: u64, align: u64) -> DiskResult<u64> {
    if align == 0 {
        return Ok(value);
    }
    let rem = value % align;
    if rem == 0 {
        Ok(value)
    } else {
        value
            .checked_add(align - rem)
            .ok_or(DiskError::Unsupported("offset overflow"))
    }
}

fn write_zeroes<S: ByteStorage>(storage: &mut S, mut offset: u64, mut len: u64) -> DiskResult<()> {
    const CHUNK: u64 = 64 * 1024;
    let buf = [0u8; CHUNK as usize];

    while len > 0 {
        let to_write = len.min(CHUNK);
        storage.write_at(offset, &buf[..to_write as usize])?;
        offset = offset
            .checked_add(to_write)
            .ok_or(DiskError::Unsupported("offset overflow"))?;
        len -= to_write;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::storage::disk::ByteStorage;

    #[derive(Default, Clone)]
    struct MemStorage {
        data: Vec<u8>,
    }

    impl ByteStorage for MemStorage {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
            let offset = offset as usize;
            let end = offset + buf.len();
            if end > self.data.len() {
                return Err(DiskError::Io("read past end".into()));
            }
            buf.copy_from_slice(&self.data[offset..end]);
            Ok(())
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
            let offset = offset as usize;
            let end = offset + buf.len();
            if end > self.data.len() {
                self.data.resize(end, 0);
            }
            self.data[offset..end].copy_from_slice(buf);
            Ok(())
        }

        fn flush(&mut self) -> DiskResult<()> {
            Ok(())
        }

        fn len(&mut self) -> DiskResult<u64> {
            Ok(self.data.len() as u64)
        }

        fn set_len(&mut self, len: u64) -> DiskResult<()> {
            self.data.resize(len as usize, 0);
            Ok(())
        }
    }

    #[test]
    fn header_roundtrip() {
        let header = SparseHeader {
            sector_size: 512,
            block_size: 1024 * 1024,
            total_sectors: 1024,
            table_offset: 4096 + 4096,
            table_entries: 1,
            journal_offset: 4096,
            data_offset: 1024 * 1024,
        };
        let enc = header.encode();
        let dec = SparseHeader::decode(&enc).unwrap();
        assert_eq!(header, dec);
    }

    #[test]
    fn decode_rejects_total_size_overflow() {
        let header = SparseHeader {
            sector_size: 512,
            block_size: 512,
            total_sectors: u64::MAX,
            table_offset: 8192,
            table_entries: 1,
            journal_offset: 4096,
            data_offset: 8192,
        };
        let enc = header.encode();
        let err = SparseHeader::decode(&enc).unwrap_err();
        assert!(matches!(err, DiskError::CorruptImage("disk size overflow")));
    }

    #[test]
    fn decode_rejects_table_entries_mismatch() {
        // total_sectors=1024, sector_size=512 => 512KiB image. With block_size=1MiB the expected
        // table entries is 1; claim 2 and ensure the header is rejected.
        let header = SparseHeader {
            sector_size: 512,
            block_size: 1024 * 1024,
            total_sectors: 1024,
            table_offset: 8192,
            table_entries: 2,
            journal_offset: 4096,
            data_offset: 1024 * 1024,
        };
        let enc = header.encode();
        let err = SparseHeader::decode(&enc).unwrap_err();
        assert!(matches!(
            err,
            DiskError::CorruptImage("table entries mismatch")
        ));
    }

    #[test]
    fn decode_rejects_allocation_table_too_large() {
        // Construct the smallest valid header that still exceeds the MAX_TABLE_BYTES guard.
        let table_entries = (MAX_TABLE_BYTES / 8) + 1;

        let header = SparseHeader {
            sector_size: 512,
            block_size: 512,
            total_sectors: table_entries,
            table_offset: 8192,
            table_entries,
            journal_offset: 4096,
            data_offset: 8192,
        };
        let enc = header.encode();
        let err = SparseHeader::decode(&enc).unwrap_err();
        assert!(matches!(
            err,
            DiskError::Unsupported("allocation table too large")
        ));
    }

    #[test]
    fn decode_rejects_block_size_too_large() {
        let header = SparseHeader {
            sector_size: 512,
            block_size: MAX_BLOCK_SIZE_BYTES + 512,
            total_sectors: 1024,
            table_offset: 8192,
            table_entries: 1,
            journal_offset: 4096,
            data_offset: (MAX_BLOCK_SIZE_BYTES as u64) + 512,
        };
        let enc = header.encode();
        let err = SparseHeader::decode(&enc).unwrap_err();
        assert!(matches!(
            err,
            DiskError::Unsupported("block size too large")
        ));
    }

    #[test]
    fn unallocated_reads_zero() {
        let storage = MemStorage::default();
        let mut disk = SparseDisk::create(storage, 512, 2048, 1024 * 1024).unwrap();
        let mut buf = vec![0xAA; 512 * 4];
        disk.read_sectors(0, &mut buf).unwrap();
        assert!(buf.iter().all(|b| *b == 0));
    }

    #[test]
    fn writes_persist_across_open() {
        let storage = MemStorage::default();
        let mut disk = SparseDisk::create(storage, 512, 4096, 1024 * 1024).unwrap();
        let mut data = vec![0u8; 512 * 8];
        for (i, b) in data.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(3);
        }
        disk.write_sectors(10, &data).unwrap();
        disk.flush().unwrap();

        let storage = disk.into_storage();
        let mut disk = SparseDisk::open(storage).unwrap();
        let mut read_back = vec![0u8; data.len()];
        disk.read_sectors(10, &mut read_back).unwrap();
        assert_eq!(data, read_back);
    }

    #[test]
    fn journal_replays_on_open() {
        let mut storage = MemStorage::default();
        {
            let disk = SparseDisk::create(storage.clone(), 512, 4096, 1024 * 1024).unwrap();
            storage = disk.into_storage();
        }

        // Manually allocate the first data block and write a pattern, then write a journal record
        // mapping logical block 0 to it without updating the allocation table.
        let mut header_buf = vec![0u8; HEADER_SIZE as usize];
        storage.read_at(0, &mut header_buf).unwrap();
        let header = SparseHeader::decode(&header_buf).unwrap();
        let block_offset = header.data_offset;

        storage
            .write_at(block_offset, &vec![0x5A; header.block_size as usize])
            .unwrap();
        storage
            .set_len(block_offset + header.block_size as u64)
            .unwrap();

        let rec = JournalRecord {
            state: 1,
            logical_block: 0,
            physical_offset: block_offset,
        };
        storage
            .write_at(header.journal_offset, &rec.encode())
            .unwrap();

        let mut disk = SparseDisk::open(storage).unwrap();
        let mut buf = vec![0u8; 512];
        disk.read_sectors(0, &mut buf).unwrap();
        assert!(buf.iter().all(|b| *b == 0x5A));
    }
}
