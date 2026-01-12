use std::num::NonZeroUsize;

use lru::LruCache;

use crate::io::storage::disk::{ByteStorage, DiskBackend};
use crate::io::storage::error::{DiskError, DiskResult};

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";
const SECTOR_SIZE: u32 = 512;

// QCOW2 is a big-endian on-disk format.
const QCOW2_OFLAG_COPIED: u64 = 1 << 63;
const QCOW2_OFLAG_COMPRESSED: u64 = 1 << 62;
// "Zero cluster" flag (introduced in qcow2 v3). Treat as unallocated.
const QCOW2_OFLAG_ZERO: u64 = 1 << 0;

// Hard cap to avoid absurd allocations when parsing untrusted images.
const MAX_TABLE_BYTES: u64 = 128 * 1024 * 1024; // 128 MiB

// Bound in-memory metadata caching when accessing untrusted images.
//
// Each L2 table or refcount block is exactly one cluster in size. We size the cache based on
// a fixed budget in bytes divided by cluster size.
const QCOW2_L2_CACHE_BUDGET_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB
const QCOW2_REFCOUNT_CACHE_BUDGET_BYTES: u64 = 16 * 1024 * 1024; // 16 MiB

fn try_alloc_zeroed(len: usize) -> DiskResult<Vec<u8>> {
    let mut buf = Vec::new();
    buf.try_reserve_exact(len)
        .map_err(|_| DiskError::QuotaExceeded)?;
    buf.resize(len, 0);
    Ok(buf)
}

#[derive(Debug, Clone)]
struct Qcow2Header {
    cluster_bits: u32,
    size: u64,
    header_length: u32,
    l1_entries: u64,
    l1_table_offset: u64,
    refcount_table_offset: u64,
    refcount_table_clusters: u32,
}

impl Qcow2Header {
    fn parse<S: ByteStorage>(storage: &mut S) -> DiskResult<Self> {
        let len = storage.len()?;
        if len < 72 {
            return Err(DiskError::CorruptImage("qcow2 header truncated"));
        }

        let mut header_72 = [0u8; 72];
        storage.read_at(0, &mut header_72)?;
        if header_72[..4] != QCOW2_MAGIC {
            return Err(DiskError::CorruptImage("qcow2 magic mismatch"));
        }

        let version = be_u32(&header_72[4..8]);
        if version != 2 && version != 3 {
            return Err(DiskError::Unsupported("qcow2 version"));
        }

        let backing_file_offset = be_u64(&header_72[8..16]);
        let backing_file_size = be_u32(&header_72[16..20]);
        let cluster_bits = be_u32(&header_72[20..24]);
        let size = be_u64(&header_72[24..32]);
        let crypt_method = be_u32(&header_72[32..36]);
        let l1_size = be_u32(&header_72[36..40]);
        let l1_table_offset = be_u64(&header_72[40..48]);
        let refcount_table_offset = be_u64(&header_72[48..56]);
        let refcount_table_clusters = be_u32(&header_72[56..60]);
        let nb_snapshots = be_u32(&header_72[60..64]);
        let snapshots_offset = be_u64(&header_72[64..72]);

        let (incompatible_features, refcount_order, header_length) = if version == 3 {
            if len < 104 {
                return Err(DiskError::CorruptImage("qcow2 v3 header truncated"));
            }
            let mut extra = [0u8; 32];
            storage.read_at(72, &mut extra)?;
            (
                be_u64(&extra[0..8]),
                be_u32(&extra[24..28]),
                be_u32(&extra[28..32]),
            )
        } else {
            (0, 4, 72)
        };

        if incompatible_features != 0 {
            return Err(DiskError::Unsupported("qcow2 incompatible features"));
        }

        if version == 3 && header_length < 104 {
            return Err(DiskError::CorruptImage("qcow2 header_length too small"));
        }
        if len < header_length as u64 {
            return Err(DiskError::CorruptImage("qcow2 header truncated"));
        }
        let header_length_u64 = header_length as u64;
        if l1_table_offset < header_length_u64 || refcount_table_offset < header_length_u64 {
            return Err(DiskError::CorruptImage("qcow2 table overlaps header"));
        }

        if crypt_method != 0 {
            return Err(DiskError::Unsupported("qcow2 encryption"));
        }

        if backing_file_offset != 0 || backing_file_size != 0 {
            return Err(DiskError::Unsupported("qcow2 backing file"));
        }

        if nb_snapshots != 0 || snapshots_offset != 0 {
            return Err(DiskError::Unsupported("qcow2 internal snapshots"));
        }

        if size == 0 {
            return Err(DiskError::CorruptImage("qcow2 size is zero"));
        }
        if !size.is_multiple_of(SECTOR_SIZE as u64) {
            return Err(DiskError::CorruptImage(
                "qcow2 size not multiple of sector size",
            ));
        }

        if !(9..=21).contains(&cluster_bits) {
            return Err(DiskError::Unsupported("qcow2 cluster size"));
        }

        if l1_size == 0 {
            return Err(DiskError::CorruptImage("qcow2 l1_size is zero"));
        }
        if !l1_table_offset.is_multiple_of(8) || !refcount_table_offset.is_multiple_of(8) {
            return Err(DiskError::CorruptImage("qcow2 table offset misaligned"));
        }
        if refcount_table_clusters == 0 {
            return Err(DiskError::CorruptImage(
                "qcow2 refcount_table_clusters is zero",
            ));
        }
        if refcount_order != 4 {
            return Err(DiskError::Unsupported("qcow2 refcount order"));
        }

        let cluster_size = 1u64 << cluster_bits;
        if !l1_table_offset.is_multiple_of(cluster_size)
            || !refcount_table_offset.is_multiple_of(cluster_size)
        {
            return Err(DiskError::CorruptImage(
                "qcow2 table offset not cluster aligned",
            ));
        }
        let l2_entries_per_table = cluster_size / 8;
        let guest_clusters = size.div_ceil(cluster_size);
        let required_l1 = guest_clusters.div_ceil(l2_entries_per_table);
        if (l1_size as u64) < required_l1 {
            return Err(DiskError::CorruptImage("qcow2 l1 table too small"));
        }

        let l1_bytes = required_l1
            .checked_mul(8)
            .ok_or(DiskError::Unsupported("qcow2 l1 table too large"))?;
        if l1_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("qcow2 l1 table too large"));
        }

        Ok(Self {
            cluster_bits,
            size,
            header_length,
            l1_entries: required_l1,
            l1_table_offset,
            refcount_table_offset,
            refcount_table_clusters,
        })
    }

    fn cluster_size(&self) -> u64 {
        1u64 << self.cluster_bits
    }
}

pub struct Qcow2Disk<S> {
    storage: S,
    header: Qcow2Header,
    l1_table: Vec<u64>,
    refcount_table: Vec<u64>,
    l2_cache: LruCache<u64, Vec<u64>>,
    refcount_cache: LruCache<u64, Vec<u16>>,
    next_free_offset: u64,
}

impl<S: ByteStorage> Qcow2Disk<S> {
    pub fn open(mut storage: S) -> DiskResult<Self> {
        let header = Qcow2Header::parse(&mut storage)?;
        let cluster_size = header.cluster_size();
        let file_len = storage.len()?;

        // ----- L1 table -----
        let l1_bytes = header
            .l1_entries
            .checked_mul(8)
            .ok_or(DiskError::Unsupported("qcow2 l1 table too large"))?;
        if l1_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("qcow2 l1 table too large"));
        }
        let l1_entries: usize = header
            .l1_entries
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 l1 table too large"))?;
        let l1_bytes_usize: usize = l1_bytes
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 l1 table too large"))?;

        let l1_end = header
            .l1_table_offset
            .checked_add(l1_bytes)
            .ok_or(DiskError::OutOfBounds)?;
        if l1_end > file_len {
            return Err(DiskError::CorruptImage("qcow2 l1 table truncated"));
        }

        // ----- Refcount table -----
        let refcount_table_bytes = (header.refcount_table_clusters as u64)
            .checked_mul(cluster_size)
            .ok_or(DiskError::Unsupported("qcow2 refcount table too large"))?;
        if refcount_table_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("qcow2 refcount table too large"));
        }
        let refcount_bytes_usize = usize::try_from(refcount_table_bytes)
            .map_err(|_| DiskError::Unsupported("qcow2 refcount table too large"))?;
        if !refcount_table_bytes.is_multiple_of(8) {
            return Err(DiskError::CorruptImage("qcow2 refcount table size invalid"));
        }
        let refcount_end = header
            .refcount_table_offset
            .checked_add(refcount_table_bytes)
            .ok_or(DiskError::OutOfBounds)?;
        if refcount_end > file_len {
            return Err(DiskError::CorruptImage("qcow2 refcount table truncated"));
        }

        if ranges_overlap(
            header.l1_table_offset,
            l1_end,
            header.refcount_table_offset,
            refcount_end,
        ) {
            return Err(DiskError::CorruptImage("qcow2 metadata tables overlap"));
        }

        // Read the L1 table without allocating an additional full-size temporary buffer.
        let mut l1_table = Vec::new();
        l1_table
            .try_reserve_exact(l1_entries)
            .map_err(|_| DiskError::Unsupported("qcow2 l1 table too large"))?;
        let mut l1_buf = Vec::new();
        l1_buf
            .try_reserve_exact(64 * 1024)
            .map_err(|_| DiskError::Unsupported("qcow2 l1 table too large"))?;
        l1_buf.resize(64 * 1024, 0);
        let mut remaining = l1_bytes_usize;
        let mut off = header.l1_table_offset;
        while remaining > 0 {
            let read_len = remaining.min(l1_buf.len());
            storage.read_at(off, &mut l1_buf[..read_len])?;
            for chunk in l1_buf[..read_len].chunks_exact(8) {
                l1_table.push(be_u64(chunk));
            }
            off = off
                .checked_add(read_len as u64)
                .ok_or(DiskError::OutOfBounds)?;
            remaining -= read_len;
        }

        // Read the refcount table without allocating an additional full-size temporary buffer.
        let refcount_entries = refcount_bytes_usize / 8;
        let mut refcount_table = Vec::new();
        refcount_table
            .try_reserve_exact(refcount_entries)
            .map_err(|_| DiskError::Unsupported("qcow2 refcount table too large"))?;
        let mut refcount_buf = Vec::new();
        refcount_buf
            .try_reserve_exact(64 * 1024)
            .map_err(|_| DiskError::Unsupported("qcow2 refcount table too large"))?;
        refcount_buf.resize(64 * 1024, 0);
        let mut remaining = refcount_bytes_usize;
        let mut off = header.refcount_table_offset;
        while remaining > 0 {
            let read_len = remaining.min(refcount_buf.len());
            storage.read_at(off, &mut refcount_buf[..read_len])?;
            for chunk in refcount_buf[..read_len].chunks_exact(8) {
                refcount_table.push(be_u64(chunk));
            }
            off = off
                .checked_add(read_len as u64)
                .ok_or(DiskError::OutOfBounds)?;
            remaining -= read_len;
        }

        let next_free_offset = align_up(file_len, cluster_size)?;

        let cluster_size_usize: usize = cluster_size
            .try_into()
            .map_err(|_| DiskError::OutOfBounds)?;
        let l2_cache_cap_entries = (QCOW2_L2_CACHE_BUDGET_BYTES / cluster_size).max(1) as usize;
        let refcount_cache_cap_entries =
            (QCOW2_REFCOUNT_CACHE_BUDGET_BYTES / cluster_size).max(1) as usize;
        // Clamp cache sizes to avoid absurd entry counts for tiny cluster sizes.
        let max_entries = (QCOW2_L2_CACHE_BUDGET_BYTES as usize / cluster_size_usize).max(1);
        let l2_cache_cap_entries = l2_cache_cap_entries.min(max_entries);
        let refcount_cache_cap_entries = refcount_cache_cap_entries.min(max_entries);

        let l2_cache_cap = NonZeroUsize::new(l2_cache_cap_entries)
            .ok_or(DiskError::Unsupported("qcow2 l2 cache size is zero"))?;
        let refcount_cache_cap = NonZeroUsize::new(refcount_cache_cap_entries)
            .ok_or(DiskError::Unsupported("qcow2 refcount cache size is zero"))?;

        Ok(Self {
            storage,
            header,
            l1_table,
            refcount_table,
            l2_cache: LruCache::new(l2_cache_cap),
            refcount_cache: LruCache::new(refcount_cache_cap),
            next_free_offset,
        })
    }

    pub fn into_storage(self) -> S {
        self.storage
    }

    fn capacity_sectors(&self) -> u64 {
        self.header.size / SECTOR_SIZE as u64
    }

    fn cluster_size(&self) -> u64 {
        self.header.cluster_size()
    }

    fn l2_entries_per_table(&self) -> u64 {
        self.cluster_size() / 8
    }

    fn refcount_entries_per_block(&self) -> u64 {
        self.cluster_size() / 2
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
            capacity_sectors: self.capacity_sectors(),
        })?;
        if end > self.capacity_sectors() {
            return Err(DiskError::OutOfRange {
                lba,
                sectors,
                capacity_sectors: self.capacity_sectors(),
            });
        }
        Ok(())
    }

    fn mask_offset(&self, entry: u64) -> u64 {
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        (entry & !(QCOW2_OFLAG_COPIED | QCOW2_OFLAG_COMPRESSED)) & !low_mask
    }

    fn l1_table_end(&self) -> DiskResult<u64> {
        let bytes = (self.l1_table.len() as u64)
            .checked_mul(8)
            .ok_or(DiskError::OutOfBounds)?;
        self.header
            .l1_table_offset
            .checked_add(bytes)
            .ok_or(DiskError::OutOfBounds)
    }

    fn refcount_table_end(&self) -> DiskResult<u64> {
        let bytes = (self.refcount_table.len() as u64)
            .checked_mul(8)
            .ok_or(DiskError::OutOfBounds)?;
        self.header
            .refcount_table_offset
            .checked_add(bytes)
            .ok_or(DiskError::OutOfBounds)
    }

    fn validate_cluster_not_overlapping_metadata(&self, cluster_offset: u64) -> DiskResult<()> {
        let cluster_size = self.cluster_size();
        if !cluster_offset.is_multiple_of(cluster_size) {
            return Err(DiskError::CorruptImage("qcow2 cluster offset not aligned"));
        }
        let cluster_end = cluster_offset
            .checked_add(cluster_size)
            .ok_or(DiskError::OutOfBounds)?;

        let header_end = self.header.header_length as u64;
        if cluster_offset < header_end {
            return Err(DiskError::CorruptImage("qcow2 cluster overlaps header"));
        }

        let l1_end = self.l1_table_end()?;
        if ranges_overlap(
            cluster_offset,
            cluster_end,
            self.header.l1_table_offset,
            l1_end,
        ) {
            return Err(DiskError::CorruptImage("qcow2 cluster overlaps l1 table"));
        }

        let refcount_end = self.refcount_table_end()?;
        if ranges_overlap(
            cluster_offset,
            cluster_end,
            self.header.refcount_table_offset,
            refcount_end,
        ) {
            return Err(DiskError::CorruptImage(
                "qcow2 cluster overlaps refcount table",
            ));
        }

        Ok(())
    }

    fn validate_cluster_present(
        &mut self,
        cluster_offset: u64,
        ctx: &'static str,
    ) -> DiskResult<()> {
        let cluster_size = self.cluster_size();
        let end = cluster_offset
            .checked_add(cluster_size)
            .ok_or(DiskError::OutOfBounds)?;
        let len = self.storage.len()?;
        if end > len {
            return Err(DiskError::CorruptImage(ctx));
        }
        Ok(())
    }

    fn l1_l2_index(&self, guest_cluster_index: u64) -> DiskResult<(usize, usize)> {
        let l2_entries = self.l2_entries_per_table();
        let l1_index = guest_cluster_index / l2_entries;
        let l2_index = guest_cluster_index % l2_entries;

        let l1_index = usize::try_from(l1_index).map_err(|_| DiskError::OutOfBounds)?;
        let l2_index = usize::try_from(l2_index).map_err(|_| DiskError::OutOfBounds)?;
        if l1_index >= self.l1_table.len() {
            return Err(DiskError::CorruptImage("qcow2 l1 index out of range"));
        }
        Ok((l1_index, l2_index))
    }

    fn l2_table_offset_from_l1_entry(&self, l1_entry: u64) -> DiskResult<Option<u64>> {
        if l1_entry == 0 {
            return Ok(None);
        }
        if (l1_entry & QCOW2_OFLAG_COMPRESSED) != 0 {
            return Err(DiskError::Unsupported("qcow2 compressed l1"));
        }
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        if (l1_entry & low_mask) != 0 {
            return Err(DiskError::CorruptImage("qcow2 unaligned l1 entry"));
        }
        let offset = self.mask_offset(l1_entry);
        if offset == 0 {
            return Err(DiskError::CorruptImage("qcow2 invalid l1 entry"));
        }
        self.validate_cluster_not_overlapping_metadata(offset)?;
        Ok(Some(offset))
    }

    fn data_cluster_offset_from_l2_entry(&self, l2_entry: u64) -> DiskResult<Option<u64>> {
        if l2_entry == 0 {
            return Ok(None);
        }
        if (l2_entry & QCOW2_OFLAG_COMPRESSED) != 0 {
            return Err(DiskError::Unsupported("qcow2 compressed cluster"));
        }
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        if (l2_entry & QCOW2_OFLAG_ZERO) != 0 {
            if (l2_entry & low_mask) != QCOW2_OFLAG_ZERO || self.mask_offset(l2_entry) != 0 {
                return Err(DiskError::CorruptImage("qcow2 invalid zero cluster entry"));
            }
            return Ok(None);
        }
        if (l2_entry & low_mask) != 0 {
            return Err(DiskError::CorruptImage("qcow2 unaligned l2 entry"));
        }
        let offset = self.mask_offset(l2_entry);
        if offset == 0 {
            return Err(DiskError::CorruptImage("qcow2 invalid l2 entry"));
        }
        self.validate_cluster_not_overlapping_metadata(offset)?;
        Ok(Some(offset))
    }

    fn load_l2_table(&mut self, l2_offset: u64) -> DiskResult<Vec<u64>> {
        self.validate_cluster_present(l2_offset, "qcow2 l2 table truncated")?;
        let cluster_size =
            usize::try_from(self.cluster_size()).map_err(|_| DiskError::OutOfBounds)?;
        let mut buf = try_alloc_zeroed(cluster_size)?;
        self.storage.read_at(l2_offset, &mut buf)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(cluster_size / 8)
            .map_err(|_| DiskError::QuotaExceeded)?;
        for chunk in buf.chunks_exact(8) {
            entries.push(be_u64(chunk));
        }
        Ok(entries)
    }

    fn ensure_l2_cached(&mut self, l2_offset: u64) -> DiskResult<()> {
        if self.l2_cache.get(&l2_offset).is_some() {
            return Ok(());
        }
        let table = self.load_l2_table(l2_offset)?;
        let _ = self.l2_cache.push(l2_offset, table);
        Ok(())
    }

    fn lookup_data_cluster(&mut self, guest_cluster_index: u64) -> DiskResult<Option<u64>> {
        let (l1_index, l2_index) = self.l1_l2_index(guest_cluster_index)?;
        let l1_entry = self.l1_table[l1_index];
        let Some(l2_offset) = self.l2_table_offset_from_l1_entry(l1_entry)? else {
            return Ok(None);
        };
        self.ensure_l2_cached(l2_offset)?;
        let table = self
            .l2_cache
            .get(&l2_offset)
            .ok_or(DiskError::CorruptImage("qcow2 l2 cache missing"))?;
        let l2_entry = *table
            .get(l2_index)
            .ok_or(DiskError::CorruptImage("qcow2 l2 index out of range"))?;
        self.data_cluster_offset_from_l2_entry(l2_entry)
    }

    fn set_l2_entry(&mut self, l2_offset: u64, l2_index: usize, entry: u64) -> DiskResult<()> {
        self.ensure_l2_cached(l2_offset)?;
        let table = self
            .l2_cache
            .get_mut(&l2_offset)
            .ok_or(DiskError::CorruptImage("qcow2 l2 cache missing"))?;
        if l2_index >= table.len() {
            return Err(DiskError::CorruptImage("qcow2 l2 index out of range"));
        }
        table[l2_index] = entry;
        let offset = l2_offset
            .checked_add((l2_index as u64) * 8)
            .ok_or(DiskError::OutOfBounds)?;
        self.storage.write_at(offset, &entry.to_be_bytes())?;
        Ok(())
    }

    fn ensure_l2_table(&mut self, l1_index: usize) -> DiskResult<u64> {
        if l1_index >= self.l1_table.len() {
            return Err(DiskError::CorruptImage("qcow2 l1 index out of range"));
        }
        let l1_entry = self.l1_table[l1_index];
        if let Some(offset) = self.l2_table_offset_from_l1_entry(l1_entry)? {
            self.ensure_l2_cached(offset)?;
            return Ok(offset);
        }

        let cluster_size = self.cluster_size();
        let new_l2_offset = self.allocate_cluster_raw()?;
        write_zeroes(&mut self.storage, new_l2_offset, cluster_size)?;
        self.storage.flush()?;

        self.set_refcount_for_offset(new_l2_offset, 1)?;
        self.storage.flush()?;

        let entry = new_l2_offset | QCOW2_OFLAG_COPIED;
        self.l1_table[l1_index] = entry;
        let l1_entry_offset = self
            .header
            .l1_table_offset
            .checked_add((l1_index as u64) * 8)
            .ok_or(DiskError::OutOfBounds)?;
        self.storage
            .write_at(l1_entry_offset, &entry.to_be_bytes())?;
        self.storage.flush()?;

        let l2_entries =
            usize::try_from(self.l2_entries_per_table()).map_err(|_| DiskError::OutOfBounds)?;
        let _ = self.l2_cache.push(new_l2_offset, vec![0u64; l2_entries]);

        Ok(new_l2_offset)
    }

    fn ensure_data_cluster(&mut self, guest_cluster_index: u64) -> DiskResult<u64> {
        let (l1_index, l2_index) = self.l1_l2_index(guest_cluster_index)?;
        let l2_offset = self.ensure_l2_table(l1_index)?;

        self.ensure_l2_cached(l2_offset)?;
        let l2_entry = {
            let table = self
                .l2_cache
                .get(&l2_offset)
                .ok_or(DiskError::CorruptImage("qcow2 l2 cache missing"))?;
            *table
                .get(l2_index)
                .ok_or(DiskError::CorruptImage("qcow2 l2 index out of range"))?
        };
        if let Some(existing) = self.data_cluster_offset_from_l2_entry(l2_entry)? {
            return Ok(existing);
        }

        let cluster_size = self.cluster_size();
        let new_data_offset = self.allocate_cluster_raw()?;
        write_zeroes(&mut self.storage, new_data_offset, cluster_size)?;
        self.storage.flush()?;

        self.set_refcount_for_offset(new_data_offset, 1)?;
        self.storage.flush()?;

        let entry = new_data_offset | QCOW2_OFLAG_COPIED;
        self.set_l2_entry(l2_offset, l2_index, entry)?;
        self.storage.flush()?;
        Ok(new_data_offset)
    }

    fn allocate_cluster_raw(&mut self) -> DiskResult<u64> {
        let cluster_size = self.cluster_size();
        let offset = self.next_free_offset;
        let new_len = offset
            .checked_add(cluster_size)
            .ok_or(DiskError::OutOfBounds)?;
        self.storage.set_len(new_len)?;
        self.next_free_offset = new_len;
        Ok(offset)
    }

    fn set_refcount_for_offset(&mut self, cluster_offset: u64, value: u16) -> DiskResult<()> {
        let cluster_size = self.cluster_size();
        if !cluster_offset.is_multiple_of(cluster_size) {
            return Err(DiskError::CorruptImage("qcow2 cluster offset not aligned"));
        }
        self.set_refcount(cluster_offset / cluster_size, value)
    }

    fn refcount_block_offset_from_entry(&self, entry: u64) -> DiskResult<Option<u64>> {
        if entry == 0 {
            return Ok(None);
        }
        if (entry & QCOW2_OFLAG_COMPRESSED) != 0 {
            return Err(DiskError::Unsupported("qcow2 compressed refcount block"));
        }
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        if (entry & low_mask) != 0 {
            return Err(DiskError::CorruptImage(
                "qcow2 unaligned refcount block entry",
            ));
        }
        let offset = self.mask_offset(entry);
        if offset == 0 {
            return Err(DiskError::CorruptImage(
                "qcow2 invalid refcount block entry",
            ));
        }
        self.validate_cluster_not_overlapping_metadata(offset)?;
        Ok(Some(offset))
    }

    fn set_refcount(&mut self, cluster_index: u64, value: u16) -> DiskResult<()> {
        let entries_per_block = self.refcount_entries_per_block();
        let block_index = cluster_index / entries_per_block;
        let entry_index = cluster_index % entries_per_block;

        let block_index = usize::try_from(block_index).map_err(|_| DiskError::OutOfBounds)?;
        let entry_index = usize::try_from(entry_index).map_err(|_| DiskError::OutOfBounds)?;

        let block_offset = self.ensure_refcount_block(block_index)?;
        self.ensure_refcount_block_cached(block_offset)?;

        {
            let block = self
                .refcount_cache
                .get_mut(&block_offset)
                .ok_or(DiskError::CorruptImage("qcow2 refcount cache missing"))?;
            if entry_index >= block.len() {
                return Err(DiskError::CorruptImage("qcow2 refcount entry out of range"));
            }
            block[entry_index] = value;
        }

        let entry_offset = block_offset
            .checked_add((entry_index as u64) * 2)
            .ok_or(DiskError::OutOfBounds)?;
        self.storage.write_at(entry_offset, &value.to_be_bytes())?;
        Ok(())
    }

    fn ensure_refcount_block(&mut self, block_index: usize) -> DiskResult<u64> {
        if block_index >= self.refcount_table.len() {
            return Err(DiskError::Unsupported("qcow2 refcount table too small"));
        }

        let existing = self.refcount_table[block_index];
        if let Some(existing_offset) = self.refcount_block_offset_from_entry(existing)? {
            self.ensure_refcount_block_cached(existing_offset)?;
            return Ok(existing_offset);
        }

        let cluster_size = self.cluster_size();
        let new_block_offset = self.allocate_cluster_raw()?;
        write_zeroes(&mut self.storage, new_block_offset, cluster_size)?;
        self.storage.flush()?;

        self.refcount_table[block_index] = new_block_offset;
        let entry_offset = self
            .header
            .refcount_table_offset
            .checked_add((block_index as u64) * 8)
            .ok_or(DiskError::OutOfBounds)?;
        self.storage
            .write_at(entry_offset, &new_block_offset.to_be_bytes())?;
        self.storage.flush()?;

        self.ensure_refcount_block_cached(new_block_offset)?;

        self.set_refcount_for_offset(new_block_offset, 1)?;
        self.storage.flush()?;

        Ok(new_block_offset)
    }

    fn ensure_refcount_block_cached(&mut self, block_offset: u64) -> DiskResult<()> {
        if self.refcount_cache.get(&block_offset).is_some() {
            return Ok(());
        }

        self.validate_cluster_present(block_offset, "qcow2 refcount block truncated")?;
        let cluster_size =
            usize::try_from(self.cluster_size()).map_err(|_| DiskError::OutOfBounds)?;
        let mut buf = try_alloc_zeroed(cluster_size)?;
        self.storage.read_at(block_offset, &mut buf)?;
        let mut entries = Vec::new();
        entries
            .try_reserve_exact(cluster_size / 2)
            .map_err(|_| DiskError::QuotaExceeded)?;
        for chunk in buf.chunks_exact(2) {
            entries.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        let _ = self.refcount_cache.push(block_offset, entries);
        Ok(())
    }
}

impl<S: ByteStorage> DiskBackend for Qcow2Disk<S> {
    fn sector_size(&self) -> u32 {
        SECTOR_SIZE
    }

    fn total_sectors(&self) -> u64 {
        self.capacity_sectors()
    }

    fn read_sectors(&mut self, lba: u64, buf: &mut [u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }

        let guest_offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OutOfBounds)?;
        let cluster_size = self.cluster_size();

        let mut buf_off = 0usize;
        while buf_off < buf.len() {
            let cur_guest = guest_offset
                .checked_add(buf_off as u64)
                .ok_or(DiskError::OutOfBounds)?;
            let guest_cluster_index = cur_guest / cluster_size;
            let offset_in_cluster = (cur_guest % cluster_size) as usize;
            let remaining_in_cluster = cluster_size as usize - offset_in_cluster;
            let chunk_len = remaining_in_cluster.min(buf.len() - buf_off);

            if let Some(data_cluster) = self.lookup_data_cluster(guest_cluster_index)? {
                let phys = data_cluster
                    .checked_add(offset_in_cluster as u64)
                    .ok_or(DiskError::OutOfBounds)?;
                self.storage
                    .read_at(phys, &mut buf[buf_off..buf_off + chunk_len])?;
            } else {
                buf[buf_off..buf_off + chunk_len].fill(0);
            }

            buf_off += chunk_len;
        }

        Ok(())
    }

    fn write_sectors(&mut self, lba: u64, buf: &[u8]) -> DiskResult<()> {
        self.check_range(lba, buf.len())?;
        if buf.is_empty() {
            return Ok(());
        }

        let guest_offset = lba
            .checked_mul(SECTOR_SIZE as u64)
            .ok_or(DiskError::OutOfBounds)?;
        let cluster_size = self.cluster_size();

        let mut buf_off = 0usize;
        while buf_off < buf.len() {
            let cur_guest = guest_offset
                .checked_add(buf_off as u64)
                .ok_or(DiskError::OutOfBounds)?;
            let guest_cluster_index = cur_guest / cluster_size;
            let offset_in_cluster = (cur_guest % cluster_size) as usize;
            let remaining_in_cluster = cluster_size as usize - offset_in_cluster;
            let chunk_len = remaining_in_cluster.min(buf.len() - buf_off);

            let chunk = &buf[buf_off..buf_off + chunk_len];
            if chunk.iter().all(|b| *b == 0)
                && self.lookup_data_cluster(guest_cluster_index)?.is_none()
            {
                buf_off += chunk_len;
                continue;
            }

            let data_cluster = self.ensure_data_cluster(guest_cluster_index)?;
            let phys = data_cluster
                .checked_add(offset_in_cluster as u64)
                .ok_or(DiskError::OutOfBounds)?;
            self.storage.write_at(phys, chunk)?;

            buf_off += chunk_len;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default, Clone)]
    struct MemStorage {
        data: Vec<u8>,
    }

    impl MemStorage {
        fn with_len(len: usize) -> Self {
            Self { data: vec![0; len] }
        }
    }

    impl ByteStorage for MemStorage {
        fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> DiskResult<()> {
            let offset = usize::try_from(offset).map_err(|_| DiskError::OutOfBounds)?;
            let end = offset
                .checked_add(buf.len())
                .ok_or(DiskError::OutOfBounds)?;
            if end > self.data.len() {
                return Err(DiskError::OutOfBounds);
            }
            buf.copy_from_slice(&self.data[offset..end]);
            Ok(())
        }

        fn write_at(&mut self, offset: u64, buf: &[u8]) -> DiskResult<()> {
            let offset = usize::try_from(offset).map_err(|_| DiskError::OutOfBounds)?;
            let end = offset
                .checked_add(buf.len())
                .ok_or(DiskError::OutOfBounds)?;
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
            let len = usize::try_from(len).map_err(|_| DiskError::OutOfBounds)?;
            self.data.resize(len, 0);
            Ok(())
        }
    }

    fn write_be_u32(buf: &mut [u8], offset: usize, val: u32) {
        buf[offset..offset + 4].copy_from_slice(&val.to_be_bytes());
    }

    fn write_be_u64(buf: &mut [u8], offset: usize, val: u64) {
        buf[offset..offset + 8].copy_from_slice(&val.to_be_bytes());
    }

    #[test]
    fn qcow2_l2_cache_is_bounded() {
        let cluster_bits = 21u32; // 2MiB clusters
        let cluster_size = 1u64 << cluster_bits;
        let l2_entries_per_table = cluster_size / 8;

        let l1_entries = 10u64;
        let guest_clusters = l1_entries * l2_entries_per_table;
        let virtual_size = guest_clusters * cluster_size;

        let refcount_table_offset = cluster_size;
        let l1_table_offset = cluster_size * 2;
        let l2_table_offset = cluster_size * 4;

        // Provide a unique L2 table cluster for each L1 entry.
        let file_len = cluster_size * (4 + l1_entries);
        let mut storage = MemStorage::with_len(file_len as usize);

        let mut header = [0u8; 104];
        header[0..4].copy_from_slice(b"QFI\xfb");
        write_be_u32(&mut header, 4, 3); // version
        write_be_u32(&mut header, 20, cluster_bits);
        write_be_u64(&mut header, 24, virtual_size);
        write_be_u32(&mut header, 36, l1_entries as u32); // l1_size
        write_be_u64(&mut header, 40, l1_table_offset);
        write_be_u64(&mut header, 48, refcount_table_offset);
        write_be_u32(&mut header, 56, 1); // refcount_table_clusters
        write_be_u64(&mut header, 72, 0); // incompatible_features
        write_be_u64(&mut header, 80, 0); // compatible_features
        write_be_u64(&mut header, 88, 0); // autoclear_features
        write_be_u32(&mut header, 96, 4); // refcount_order (16-bit)
        write_be_u32(&mut header, 100, 104); // header_length
        storage.write_at(0, &header).unwrap();

        for i in 0..l1_entries {
            let entry = (l2_table_offset + i * cluster_size) | QCOW2_OFLAG_COPIED;
            storage
                .write_at(l1_table_offset + i * 8, &entry.to_be_bytes())
                .unwrap();
        }

        let mut disk = Qcow2Disk::open(storage).unwrap();
        for i in 0..l1_entries {
            disk.ensure_l2_cached(l2_table_offset + i * cluster_size)
                .unwrap();
        }

        let expected_cap = (QCOW2_L2_CACHE_BUDGET_BYTES / cluster_size).max(1) as usize;
        assert!(disk.l2_cache.len() <= expected_cap);
    }
}
