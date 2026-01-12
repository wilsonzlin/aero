use lru::LruCache;
use std::num::NonZeroUsize;

use crate::util::align_up_u64;
use crate::util::checked_range;
use crate::{DiskError, Result, StorageBackend, VirtualDisk, SECTOR_SIZE};

const QCOW2_MAGIC: [u8; 4] = *b"QFI\xfb";

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
    fn parse<B: StorageBackend>(backend: &mut B) -> Result<Self> {
        let len = backend.len()?;
        if len < 72 {
            return Err(DiskError::CorruptImage("qcow2 header truncated"));
        }

        let mut header_72 = [0u8; 72];
        backend.read_at(0, &mut header_72)?;
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
            backend.read_at(72, &mut extra)?;
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

        // Cluster sizes > 2 MiB are excessive for our use cases and can blow up metadata
        // tables, but cluster sizes < 512 are invalid since the guest is sector addressed.
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
            .ok_or(DiskError::OffsetOverflow)?;
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

/// QCOW2 v2/v3 disk image (subset).
///
/// Supported:
/// - unencrypted
/// - uncompressed
/// - no backing file
/// - no internal snapshots
pub struct Qcow2Disk<B> {
    backend: B,
    header: Qcow2Header,
    l1_table: Vec<u64>,
    refcount_table: Vec<u64>,
    l2_cache: LruCache<u64, Vec<u64>>,
    refcount_cache: LruCache<u64, Vec<u16>>,
    next_free_offset: u64,
}

impl<B: StorageBackend> Qcow2Disk<B> {
    pub fn open(mut backend: B) -> Result<Self> {
        let header = Qcow2Header::parse(&mut backend)?;
        let cluster_size = header.cluster_size();

        let file_len = backend.len()?;

        // ----- L1 table -----
        let l1_bytes = header
            .l1_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;
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
            .ok_or(DiskError::OffsetOverflow)?;
        if l1_end > file_len {
            return Err(DiskError::CorruptImage("qcow2 l1 table truncated"));
        }

        // ----- Refcount table -----
        let refcount_table_bytes = (header.refcount_table_clusters as u64)
            .checked_mul(cluster_size)
            .ok_or(DiskError::OffsetOverflow)?;
        if refcount_table_bytes > MAX_TABLE_BYTES {
            return Err(DiskError::Unsupported("qcow2 refcount table too large"));
        }
        if !refcount_table_bytes.is_multiple_of(8) {
            return Err(DiskError::CorruptImage("qcow2 refcount table size invalid"));
        }
        let refcount_bytes_usize: usize = refcount_table_bytes
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 refcount table too large"))?;

        let refcount_end = header
            .refcount_table_offset
            .checked_add(refcount_table_bytes)
            .ok_or(DiskError::OffsetOverflow)?;
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

        let mut l1_buf = vec![0u8; l1_bytes_usize];
        match backend.read_at(header.l1_table_offset, &mut l1_buf) {
            Ok(()) => {}
            Err(DiskError::OutOfBounds { .. }) => {
                return Err(DiskError::CorruptImage("qcow2 l1 table truncated"));
            }
            Err(e) => return Err(e),
        }
        let mut l1_table = Vec::with_capacity(l1_entries);
        for chunk in l1_buf.chunks_exact(8) {
            l1_table.push(be_u64(chunk));
        }

        let mut refcount_buf = vec![0u8; refcount_bytes_usize];
        match backend.read_at(header.refcount_table_offset, &mut refcount_buf) {
            Ok(()) => {}
            Err(DiskError::OutOfBounds { .. }) => {
                return Err(DiskError::CorruptImage("qcow2 refcount table truncated"));
            }
            Err(e) => return Err(e),
        }
        let mut refcount_table = Vec::with_capacity(refcount_bytes_usize / 8);
        for chunk in refcount_buf.chunks_exact(8) {
            refcount_table.push(be_u64(chunk));
        }

        let next_free_offset = align_up_u64(file_len, cluster_size)?;

        let cluster_size_usize: usize = cluster_size
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 cluster size too large"))?;
        let l2_cache_cap_entries = (QCOW2_L2_CACHE_BUDGET_BYTES / cluster_size).max(1) as usize;
        let refcount_cache_cap_entries =
            (QCOW2_REFCOUNT_CACHE_BUDGET_BYTES / cluster_size).max(1) as usize;
        // Clamp cache sizes to avoid absurd entry counts for tiny cluster sizes.
        let max_entries = (QCOW2_L2_CACHE_BUDGET_BYTES as usize / cluster_size_usize).max(1);
        let l2_cache_cap_entries = l2_cache_cap_entries.min(max_entries);
        let refcount_cache_cap_entries = refcount_cache_cap_entries.min(max_entries);

        let l2_cache_cap = NonZeroUsize::new(l2_cache_cap_entries)
            .ok_or(DiskError::InvalidConfig("qcow2 l2 cache size is zero"))?;
        let refcount_cache_cap = NonZeroUsize::new(refcount_cache_cap_entries).ok_or(
            DiskError::InvalidConfig("qcow2 refcount cache size is zero"),
        )?;

        Ok(Self {
            backend,
            header,
            l1_table,
            refcount_table,
            l2_cache: LruCache::new(l2_cache_cap),
            refcount_cache: LruCache::new(refcount_cache_cap),
            next_free_offset,
        })
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

    fn validate_cluster_present(&mut self, cluster_offset: u64, ctx: &'static str) -> Result<()> {
        let cluster_size = self.cluster_size();
        let end = cluster_offset
            .checked_add(cluster_size)
            .ok_or(DiskError::OffsetOverflow)?;
        let len = self.backend.len()?;
        if end > len {
            return Err(DiskError::CorruptImage(ctx));
        }
        Ok(())
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

    fn mask_offset(&self, entry: u64) -> u64 {
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        (entry & !(QCOW2_OFLAG_COPIED | QCOW2_OFLAG_COMPRESSED)) & !low_mask
    }

    fn l1_l2_index(&self, guest_cluster_index: u64) -> Result<(usize, usize)> {
        let l2_entries = self.l2_entries_per_table();
        let l1_index = guest_cluster_index / l2_entries;
        let l2_index = guest_cluster_index % l2_entries;

        let l1_index: usize = l1_index
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 image too large"))?;
        let l2_index: usize = l2_index
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 image too large"))?;
        if l1_index >= self.l1_table.len() {
            return Err(DiskError::CorruptImage("qcow2 l1 index out of range"));
        }
        Ok((l1_index, l2_index))
    }

    fn l2_table_offset_from_l1_entry(&self, l1_entry: u64) -> Result<Option<u64>> {
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

    fn data_cluster_offset_from_l2_entry(&self, l2_entry: u64) -> Result<Option<u64>> {
        if l2_entry == 0 {
            return Ok(None);
        }
        if (l2_entry & QCOW2_OFLAG_COMPRESSED) != 0 {
            return Err(DiskError::Unsupported("qcow2 compressed cluster"));
        }
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        if (l2_entry & QCOW2_OFLAG_ZERO) != 0 {
            if (l2_entry & low_mask) != QCOW2_OFLAG_ZERO {
                return Err(DiskError::CorruptImage("qcow2 invalid zero cluster entry"));
            }
            if self.mask_offset(l2_entry) != 0 {
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

    fn validate_cluster_not_overlapping_metadata(&self, cluster_offset: u64) -> Result<()> {
        let cluster_size = self.cluster_size();
        if !cluster_offset.is_multiple_of(cluster_size) {
            return Err(DiskError::CorruptImage("qcow2 cluster offset not aligned"));
        }
        let cluster_end = cluster_offset
            .checked_add(cluster_size)
            .ok_or(DiskError::OffsetOverflow)?;

        let header_end = self.header.header_length as u64;
        if cluster_offset < header_end {
            return Err(DiskError::CorruptImage("qcow2 cluster overlaps header"));
        }

        let l1_bytes = self
            .header
            .l1_entries
            .checked_mul(8)
            .ok_or(DiskError::OffsetOverflow)?;
        let l1_end = self
            .header
            .l1_table_offset
            .checked_add(l1_bytes)
            .ok_or(DiskError::OffsetOverflow)?;
        if ranges_overlap(
            cluster_offset,
            cluster_end,
            self.header.l1_table_offset,
            l1_end,
        ) {
            return Err(DiskError::CorruptImage("qcow2 cluster overlaps l1 table"));
        }

        let refcount_table_bytes = (self.header.refcount_table_clusters as u64)
            .checked_mul(cluster_size)
            .ok_or(DiskError::OffsetOverflow)?;
        let refcount_end = self
            .header
            .refcount_table_offset
            .checked_add(refcount_table_bytes)
            .ok_or(DiskError::OffsetOverflow)?;
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

    fn load_l2_table(&mut self, l2_offset: u64) -> Result<Vec<u64>> {
        let cluster_size: usize = self
            .cluster_size()
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 cluster size too large"))?;
        let mut buf = vec![0u8; cluster_size];
        self.backend_read_at(l2_offset, &mut buf, "qcow2 l2 table truncated")?;
        let mut entries = Vec::with_capacity(cluster_size / 8);
        for chunk in buf.chunks_exact(8) {
            entries.push(be_u64(chunk));
        }
        Ok(entries)
    }

    fn ensure_l2_cached(&mut self, l2_offset: u64) -> Result<()> {
        if self.l2_cache.get(&l2_offset).is_some() {
            return Ok(());
        }
        let table = self.load_l2_table(l2_offset)?;
        let _ = self.l2_cache.push(l2_offset, table);
        Ok(())
    }

    fn lookup_data_cluster(&mut self, guest_cluster_index: u64) -> Result<Option<u64>> {
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

    fn set_l2_entry(&mut self, l2_offset: u64, l2_index: usize, entry: u64) -> Result<()> {
        self.ensure_l2_cached(l2_offset)?;
        {
            let table = self
                .l2_cache
                .get(&l2_offset)
                .ok_or(DiskError::CorruptImage("qcow2 l2 cache missing"))?;
            if l2_index >= table.len() {
                return Err(DiskError::CorruptImage("qcow2 l2 index out of range"));
            }
        }
        let offset = l2_offset
            .checked_add((l2_index as u64) * 8)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.write_at(offset, &entry.to_be_bytes())?;
        let table = self
            .l2_cache
            .get_mut(&l2_offset)
            .ok_or(DiskError::CorruptImage("qcow2 l2 cache missing"))?;
        table[l2_index] = entry;
        Ok(())
    }

    fn ensure_l2_table(&mut self, l1_index: usize) -> Result<u64> {
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
        write_zeroes(&mut self.backend, new_l2_offset, cluster_size)?;

        self.set_refcount_for_offset(new_l2_offset, 1)?;

        let entry = new_l2_offset | QCOW2_OFLAG_COPIED;
        let l1_entry_offset = self
            .header
            .l1_table_offset
            .checked_add((l1_index as u64) * 8)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend
            .write_at(l1_entry_offset, &entry.to_be_bytes())?;
        self.l1_table[l1_index] = entry;

        let l2_entries: usize = self
            .l2_entries_per_table()
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 l2 table too large"))?;
        let _ = self.l2_cache.push(new_l2_offset, vec![0u64; l2_entries]);

        Ok(new_l2_offset)
    }

    fn ensure_data_cluster(&mut self, guest_cluster_index: u64) -> Result<u64> {
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
        write_zeroes(&mut self.backend, new_data_offset, cluster_size)?;

        self.set_refcount_for_offset(new_data_offset, 1)?;

        let entry = new_data_offset | QCOW2_OFLAG_COPIED;
        self.set_l2_entry(l2_offset, l2_index, entry)?;
        Ok(new_data_offset)
    }

    fn allocate_cluster_raw(&mut self) -> Result<u64> {
        let cluster_size = self.cluster_size();
        let offset = self.next_free_offset;
        let new_len = offset
            .checked_add(cluster_size)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.set_len(new_len)?;
        self.next_free_offset = new_len;
        Ok(offset)
    }

    fn set_refcount_for_offset(&mut self, cluster_offset: u64, value: u16) -> Result<()> {
        let cluster_size = self.cluster_size();
        if !cluster_offset.is_multiple_of(cluster_size) {
            return Err(DiskError::CorruptImage("qcow2 cluster offset not aligned"));
        }
        self.set_refcount(cluster_offset / cluster_size, value)
    }

    fn set_refcount(&mut self, cluster_index: u64, value: u16) -> Result<()> {
        let entries_per_block = self.refcount_entries_per_block();
        let block_index = cluster_index / entries_per_block;
        let entry_index = cluster_index % entries_per_block;

        let block_index: usize = block_index
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 image too large"))?;
        let entry_index: usize = entry_index
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 image too large"))?;

        let block_offset = self.ensure_refcount_block(block_index)?;
        self.ensure_refcount_block_cached(block_offset)?;

        {
            let block = self
                .refcount_cache
                .get(&block_offset)
                .ok_or(DiskError::CorruptImage("qcow2 refcount cache missing"))?;
            if entry_index >= block.len() {
                return Err(DiskError::CorruptImage("qcow2 refcount entry out of range"));
            }
        }

        let entry_offset = block_offset
            .checked_add((entry_index as u64) * 2)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend.write_at(entry_offset, &value.to_be_bytes())?;
        let block = self
            .refcount_cache
            .get_mut(&block_offset)
            .ok_or(DiskError::CorruptImage("qcow2 refcount cache missing"))?;
        block[entry_index] = value;
        Ok(())
    }

    fn ensure_refcount_block(&mut self, block_index: usize) -> Result<u64> {
        if block_index >= self.refcount_table.len() {
            return Err(DiskError::Unsupported("qcow2 refcount table too small"));
        }

        let existing = self.refcount_table[block_index];
        if (existing & QCOW2_OFLAG_COMPRESSED) != 0 {
            return Err(DiskError::Unsupported("qcow2 compressed refcount block"));
        }
        let low_mask = (1u64 << self.header.cluster_bits) - 1;
        if (existing & low_mask) != 0 {
            return Err(DiskError::CorruptImage(
                "qcow2 unaligned refcount block entry",
            ));
        }
        let existing_offset = self.mask_offset(existing);
        if existing_offset != 0 {
            self.validate_cluster_not_overlapping_metadata(existing_offset)?;
            self.ensure_refcount_block_cached(existing_offset)?;
            return Ok(existing_offset);
        }
        if existing != 0 {
            return Err(DiskError::CorruptImage(
                "qcow2 invalid refcount block entry",
            ));
        }

        let cluster_size = self.cluster_size();
        let new_block_offset = self.allocate_cluster_raw()?;
        write_zeroes(&mut self.backend, new_block_offset, cluster_size)?;

        let entry_offset = self
            .header
            .refcount_table_offset
            .checked_add((block_index as u64) * 8)
            .ok_or(DiskError::OffsetOverflow)?;
        self.backend
            .write_at(entry_offset, &new_block_offset.to_be_bytes())?;
        self.refcount_table[block_index] = new_block_offset;

        self.ensure_refcount_block_cached(new_block_offset)?;

        // The refcount block itself is a cluster that must have a non-zero refcount.
        self.set_refcount_for_offset(new_block_offset, 1)?;

        Ok(new_block_offset)
    }

    fn ensure_refcount_block_cached(&mut self, block_offset: u64) -> Result<()> {
        if self.refcount_cache.get(&block_offset).is_some() {
            return Ok(());
        }

        let cluster_size: usize = self
            .cluster_size()
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 cluster size too large"))?;
        let mut buf = vec![0u8; cluster_size];
        self.backend_read_at(block_offset, &mut buf, "qcow2 refcount block truncated")?;
        let mut entries = Vec::with_capacity(cluster_size / 2);
        for chunk in buf.chunks_exact(2) {
            entries.push(u16::from_be_bytes([chunk[0], chunk[1]]));
        }
        let _ = self.refcount_cache.push(block_offset, entries);
        Ok(())
    }
}

impl<B: StorageBackend> VirtualDisk for Qcow2Disk<B> {
    fn capacity_bytes(&self) -> u64 {
        self.header.size
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        if buf.is_empty() {
            return Ok(());
        }

        let cluster_size = self.cluster_size();
        let cluster_size_usize: usize = cluster_size
            .try_into()
            .map_err(|_| DiskError::Unsupported("qcow2 cluster size too large"))?;
        if cluster_size_usize == 0 {
            return Err(DiskError::CorruptImage("qcow2 cluster size is zero"));
        }

        let mut pos = 0usize;
        while pos < buf.len() {
            let cur_guest = offset
                .checked_add(pos as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let guest_cluster_index = cur_guest / cluster_size;
            let offset_in_cluster = (cur_guest % cluster_size) as usize;

            let remaining = buf.len() - pos;
            let remaining_in_cluster = cluster_size_usize - offset_in_cluster;
            let chunk_len = remaining_in_cluster.min(remaining);

            // Fast path: if we are cluster-aligned and reading whole clusters, merge contiguous
            // clusters into a single backend read (or zero-fill) to reduce IO calls during
            // sequential streaming/conversion.
            let aligned_full_cluster = offset_in_cluster == 0 && chunk_len == cluster_size_usize;
            if aligned_full_cluster {
                let max_clusters = (remaining / cluster_size_usize) as u64;
                debug_assert!(max_clusters >= 1);

                let first = self.lookup_data_cluster(guest_cluster_index)?;
                match first {
                    Some(first_phys) => {
                        let mut run_clusters = 1u64;
                        while run_clusters < max_clusters {
                            let idx = guest_cluster_index
                                .checked_add(run_clusters)
                                .ok_or(DiskError::OffsetOverflow)?;
                            let Some(next_phys) = self.lookup_data_cluster(idx)? else {
                                break;
                            };
                            let expected = first_phys
                                .checked_add(
                                    run_clusters
                                        .checked_mul(cluster_size)
                                        .ok_or(DiskError::OffsetOverflow)?,
                                )
                                .ok_or(DiskError::OffsetOverflow)?;
                            if next_phys != expected {
                                break;
                            }
                            run_clusters += 1;
                        }

                        let run_bytes_u64 = run_clusters
                            .checked_mul(cluster_size)
                            .ok_or(DiskError::OffsetOverflow)?;
                        let run_bytes: usize = run_bytes_u64
                            .try_into()
                            .map_err(|_| DiskError::OffsetOverflow)?;

                        self.backend_read_at(
                            first_phys,
                            &mut buf[pos..pos + run_bytes],
                            "qcow2 data cluster truncated",
                        )?;
                        pos += run_bytes;
                        continue;
                    }
                    None => {
                        let mut run_clusters = 1u64;
                        while run_clusters < max_clusters {
                            let idx = guest_cluster_index
                                .checked_add(run_clusters)
                                .ok_or(DiskError::OffsetOverflow)?;
                            if self.lookup_data_cluster(idx)?.is_some() {
                                break;
                            }
                            run_clusters += 1;
                        }

                        let run_bytes_u64 = run_clusters
                            .checked_mul(cluster_size)
                            .ok_or(DiskError::OffsetOverflow)?;
                        let run_bytes: usize = run_bytes_u64
                            .try_into()
                            .map_err(|_| DiskError::OffsetOverflow)?;

                        buf[pos..pos + run_bytes].fill(0);
                        pos += run_bytes;
                        continue;
                    }
                }
            }

            // Slow path: partial-cluster read.
            if let Some(data_cluster) = self.lookup_data_cluster(guest_cluster_index)? {
                let phys = data_cluster
                    .checked_add(offset_in_cluster as u64)
                    .ok_or(DiskError::OffsetOverflow)?;
                self.backend_read_at(
                    phys,
                    &mut buf[pos..pos + chunk_len],
                    "qcow2 data cluster truncated",
                )?;
            } else {
                buf[pos..pos + chunk_len].fill(0);
            }

            pos += chunk_len;
        }

        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;
        if buf.is_empty() {
            return Ok(());
        }

        let cluster_size = self.cluster_size();

        let mut buf_off = 0usize;
        while buf_off < buf.len() {
            let cur_guest = offset
                .checked_add(buf_off as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            let guest_cluster_index = cur_guest / cluster_size;
            let offset_in_cluster = (cur_guest % cluster_size) as usize;
            let remaining_in_cluster = cluster_size as usize - offset_in_cluster;
            let chunk_len = remaining_in_cluster.min(buf.len() - buf_off);

            let chunk = &buf[buf_off..buf_off + chunk_len];
            let existing = self.lookup_data_cluster(guest_cluster_index)?;
            if existing.is_none() && chunk.iter().all(|b| *b == 0) {
                buf_off += chunk_len;
                continue;
            }

            let data_cluster = match existing {
                Some(off) => {
                    self.validate_cluster_present(off, "qcow2 data cluster truncated")?;
                    off
                }
                None => self.ensure_data_cluster(guest_cluster_index)?,
            };
            let phys = data_cluster
                .checked_add(offset_in_cluster as u64)
                .ok_or(DiskError::OffsetOverflow)?;
            self.backend.write_at(phys, chunk)?;

            buf_off += chunk_len;
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

fn ranges_overlap(start_a: u64, end_a: u64, start_b: u64, end_b: u64) -> bool {
    start_a < end_b && start_b < end_a
}
