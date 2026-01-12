use crate::io::storage::cache::{BlockCache, CachedBlock};
use crate::io::storage::{DiskBackend, DiskBackendStats, LocalBoxFuture};
use crate::platform::storage::indexeddb as idb;
use crate::{Result, StorageError};
use std::collections::HashSet;
use wasm_bindgen::JsValue;

const DEFAULT_BLOCK_SIZE: usize = 1024 * 1024; // 1 MiB
const META_STORE: &str = "meta";
const BLOCKS_STORE: &str = "blocks";
const META_KEY_FORMAT_VERSION: &str = "format_version";
const META_KEY_BLOCK_SIZE: &str = "block_size";
const META_KEY_CAPACITY: &str = "capacity";
const FORMAT_VERSION: u32 = 1;
const DB_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone)]
pub struct IndexedDbBackendOptions {
    /// Maximum in-memory cache size in bytes.
    pub max_resident_bytes: usize,
    /// Maximum blocks to persist per IndexedDB transaction.
    pub flush_chunk_blocks: usize,
}

impl Default for IndexedDbBackendOptions {
    fn default() -> Self {
        Self {
            max_resident_bytes: 64 * 1024 * 1024,
            flush_chunk_blocks: 4,
        }
    }
}

pub struct IndexedDbBackend {
    db_name: String,
    db: web_sys::IdbDatabase,
    capacity: u64,
    cache: BlockCache,
    dirty: HashSet<u64>,
    flush_chunk_blocks: usize,
    stats: DiskBackendStats,
}

impl IndexedDbBackend {
    pub async fn open(
        db_name: impl Into<String>,
        capacity: u64,
        opts: IndexedDbBackendOptions,
    ) -> Result<Self> {
        let db_name = db_name.into();

        let db = idb::open_database_with_schema(&db_name, DB_SCHEMA_VERSION, |db, old, _new| {
            // Schema migrations.
            //
            // We version at the IndexedDB level so future format changes can
            // migrate object stores safely. For now, only v1 exists.
            if old < 1 {
                db.create_object_store(META_STORE)?;
                db.create_object_store(BLOCKS_STORE)?;
            }
            Ok(())
        })
        .await?;

        let existing_meta = Self::read_meta(&db).await?;
        let (disk_capacity, block_size) = match existing_meta {
            None => {
                Self::write_meta(&db, capacity, DEFAULT_BLOCK_SIZE).await?;
                (capacity, DEFAULT_BLOCK_SIZE)
            }
            Some(meta) => {
                if meta.format_version != FORMAT_VERSION {
                    return Err(StorageError::UnsupportedFormat(meta.format_version));
                }
                if meta.block_size != DEFAULT_BLOCK_SIZE as u32 {
                    return Err(StorageError::Corrupt("block size mismatch"));
                }
                if meta.capacity != capacity {
                    return Err(StorageError::Corrupt("capacity mismatch"));
                }
                (meta.capacity, meta.block_size as usize)
            }
        };

        Ok(Self {
            db_name,
            db,
            capacity: disk_capacity,
            cache: BlockCache::new(block_size, opts.max_resident_bytes),
            dirty: HashSet::new(),
            flush_chunk_blocks: opts.flush_chunk_blocks.max(1),
            stats: DiskBackendStats::default(),
        })
    }

    pub fn db_name(&self) -> &str {
        &self.db_name
    }

    pub async fn delete_database(db_name: &str) -> Result<()> {
        idb::delete_database(db_name).await
    }

    async fn read_meta(db: &web_sys::IdbDatabase) -> Result<Option<DiskMeta>> {
        let format_version = idb::get_string(db, META_STORE, META_KEY_FORMAT_VERSION).await?;
        if format_version.is_none() {
            return Ok(None);
        }

        let format_version: u32 = format_version
            .ok_or(StorageError::Corrupt("missing format_version"))?
            .parse()
            .map_err(|_| StorageError::Corrupt("invalid format_version"))?;

        let block_size: u32 = idb::get_string(db, META_STORE, META_KEY_BLOCK_SIZE)
            .await?
            .ok_or(StorageError::Corrupt("missing block_size"))?
            .parse()
            .map_err(|_| StorageError::Corrupt("invalid block_size"))?;

        let capacity: u64 = idb::get_string(db, META_STORE, META_KEY_CAPACITY)
            .await?
            .ok_or(StorageError::Corrupt("missing capacity"))?
            .parse()
            .map_err(|_| StorageError::Corrupt("invalid capacity"))?;

        Ok(Some(DiskMeta {
            format_version,
            block_size,
            capacity,
        }))
    }

    async fn write_meta(db: &web_sys::IdbDatabase, capacity: u64, block_size: usize) -> Result<()> {
        let (tx, store) = idb::transaction_rw(db, META_STORE)?;

        // Queue all puts synchronously; do not `.await` in the middle of an
        // IndexedDB transaction.
        let _ = store.put_with_key(
            &JsValue::from_str(&FORMAT_VERSION.to_string()),
            &JsValue::from_str(META_KEY_FORMAT_VERSION),
        )?;
        let _ = store.put_with_key(
            &JsValue::from_str(&block_size.to_string()),
            &JsValue::from_str(META_KEY_BLOCK_SIZE),
        )?;
        let _ = store.put_with_key(
            &JsValue::from_str(&capacity.to_string()),
            &JsValue::from_str(META_KEY_CAPACITY),
        )?;

        idb::await_transaction(tx).await?;
        Ok(())
    }

    fn block_size_u64(&self) -> u64 {
        self.cache.block_size() as u64
    }

    fn check_bounds(&self, offset: u64, len: usize) -> Result<()> {
        if len == 0 {
            return Ok(());
        }
        let end = offset
            .checked_add(len as u64)
            .ok_or(StorageError::OutOfBounds {
                offset,
                len,
                capacity: self.capacity,
            })?;
        if end > self.capacity {
            return Err(StorageError::OutOfBounds {
                offset,
                len,
                capacity: self.capacity,
            });
        }
        Ok(())
    }

    async fn ensure_space_for_block(&mut self) -> Result<()> {
        while self.cache.len() >= self.cache.max_blocks().get() {
            let Some((evicted_idx, evicted)) = self.cache.pop_lru() else {
                break;
            };

            if evicted.dirty {
                // Write-back on eviction to keep resident memory bounded.
                let persist_res = self.persist_single_block(evicted_idx, &evicted.data).await;
                if let Err(err) = persist_res {
                    // Put the block back so we don't lose data.
                    self.cache.put(evicted_idx, evicted);
                    return Err(err);
                }
                self.dirty.remove(&evicted_idx);
            }
        }
        Ok(())
    }

    async fn load_block(&mut self, block_idx: u64) -> Result<()> {
        if self.cache.contains(&block_idx) {
            return Ok(());
        }

        self.stats.cache_misses += 1;

        let key = block_key(block_idx);
        let val = idb::get_value(&self.db, BLOCKS_STORE, &key).await?;
        let mut data = vec![0u8; self.cache.block_size()];
        if let Some(val) = val {
            idb::js_value_copy_to_bytes(&val, &mut data)?;
        }

        self.ensure_space_for_block().await?;
        self.cache
            .put(block_idx, CachedBlock { data, dirty: false });
        self.stats.blocks_read += 1;
        Ok(())
    }

    async fn persist_single_block(&mut self, block_idx: u64, data: &[u8]) -> Result<()> {
        let key = block_key(block_idx);
        if is_all_zero(data) {
            idb::delete_value(&self.db, BLOCKS_STORE, &key).await?;
        } else {
            let bytes = idb::bytes_to_js_value(data);
            idb::put_value(&self.db, BLOCKS_STORE, &key, &bytes).await?;
        }
        self.stats.blocks_written += 1;
        Ok(())
    }
}

impl DiskBackend for IndexedDbBackend {
    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn stats(&self) -> DiskBackendStats {
        self.stats
    }

    fn block_size(&self) -> usize {
        self.cache.block_size()
    }

    fn read_at<'a>(&'a mut self, offset: u64, buf: &'a mut [u8]) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.check_bounds(offset, buf.len())?;
            if buf.is_empty() {
                return Ok(());
            }

            let block_size = self.block_size_u64();
            let mut buf_off = 0usize;
            while buf_off < buf.len() {
                let abs_off = offset + buf_off as u64;
                let block_idx = abs_off / block_size;
                let in_block = (abs_off % block_size) as usize;
                let to_copy = (buf.len() - buf_off).min(self.cache.block_size() - in_block);

                let hit = self.cache.contains(&block_idx);
                if hit {
                    self.stats.cache_hits += 1;
                } else {
                    self.load_block(block_idx).await?;
                }
                let data_slice = {
                    let block = self
                        .cache
                        .get(&block_idx)
                        .ok_or(StorageError::Corrupt("missing cached block after load"))?;
                    &block.data[in_block..in_block + to_copy]
                };
                buf[buf_off..buf_off + to_copy].copy_from_slice(data_slice);
                buf_off += to_copy;
            }
            Ok(())
        })
    }

    fn write_at<'a>(&'a mut self, offset: u64, buf: &'a [u8]) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            self.check_bounds(offset, buf.len())?;
            if buf.is_empty() {
                return Ok(());
            }

            let block_size = self.block_size_u64();
            let mut buf_off = 0usize;
            while buf_off < buf.len() {
                let abs_off = offset + buf_off as u64;
                let block_idx = abs_off / block_size;
                let in_block = (abs_off % block_size) as usize;
                let to_copy = (buf.len() - buf_off).min(self.cache.block_size() - in_block);

                let is_full_block_write = in_block == 0 && to_copy == self.cache.block_size();

                if is_full_block_write && !self.cache.contains(&block_idx) {
                    self.ensure_space_for_block().await?;
                    self.cache.put(
                        block_idx,
                        CachedBlock {
                            data: buf[buf_off..buf_off + to_copy].to_vec(),
                            dirty: true,
                        },
                    );
                    self.dirty.insert(block_idx);
                } else {
                    self.load_block(block_idx).await?;
                    let block = self
                        .cache
                        .get_mut(&block_idx)
                        .ok_or(StorageError::Corrupt("missing cached block after load"))?;
                    block.data[in_block..in_block + to_copy]
                        .copy_from_slice(&buf[buf_off..buf_off + to_copy]);
                    block.dirty = true;
                    self.dirty.insert(block_idx);
                }
                buf_off += to_copy;
            }

            Ok(())
        })
    }

    fn flush<'a>(&'a mut self) -> LocalBoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if self.dirty.is_empty() {
                return Ok(());
            }

            let mut dirty_blocks: Vec<u64> = self.dirty.iter().copied().collect();
            dirty_blocks.sort_unstable();

            let chunk_size = self.flush_chunk_blocks.max(1);
            for chunk in dirty_blocks.chunks(chunk_size) {
                // Open a fresh transaction per chunk to avoid a single giant
                // transaction that can stall the browser.
                let (tx, store) = idb::transaction_rw(&self.db, BLOCKS_STORE)?;

                for &block_idx in chunk {
                    let key = block_key(block_idx);
                    let (is_zero, value) = {
                        let Some(block) = self.cache.peek(&block_idx) else {
                            return Err(StorageError::Corrupt("dirty block missing from cache"));
                        };
                        if is_all_zero(&block.data) {
                            (true, None)
                        } else {
                            (false, Some(idb::bytes_to_js_value(&block.data)))
                        }
                    };

                    if is_zero {
                        let _ = store.delete(&key)?;
                    } else {
                        let _ = store.put_with_key(value.as_ref().expect("non-zero"), &key)?;
                    }
                }

                idb::await_transaction(tx).await?;

                for &block_idx in chunk {
                    if let Some(block) = self.cache.peek_mut(&block_idx) {
                        block.dirty = false;
                    }
                    self.dirty.remove(&block_idx);
                    self.stats.blocks_written += 1;
                }

                // Yield to the event loop between chunks.
                idb::yield_to_event_loop().await;
            }

            Ok(())
        })
    }
}

impl Drop for IndexedDbBackend {
    fn drop(&mut self) {
        self.db.close();
    }
}

#[derive(Debug)]
struct DiskMeta {
    format_version: u32,
    block_size: u32,
    capacity: u64,
}

fn block_key(block_idx: u64) -> JsValue {
    // Use a string key to avoid precision issues with JS numbers beyond 2^53.
    JsValue::from_str(&block_idx.to_string())
}

fn is_all_zero(data: &[u8]) -> bool {
    data.iter().all(|&b| b == 0)
}
