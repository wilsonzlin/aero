use crate::util::checked_range;
use crate::{AeroSparseConfig, AeroSparseDisk, DiskError, Result, StorageBackend, VirtualDisk};

/// Copy-on-write disk built from a read-only base disk plus a writable sparse overlay.
///
/// Reads consult the overlay first; if the relevant overlay block is unallocated the data
/// is read from the base. Writes always go to the overlay.
pub struct AeroCowDisk<Base, OverlayBackend> {
    base: Base,
    overlay: AeroSparseDisk<OverlayBackend>,
}

impl<Base: VirtualDisk, OverlayBackend: StorageBackend> AeroCowDisk<Base, OverlayBackend> {
    pub fn create(
        base: Base,
        overlay_backend: OverlayBackend,
        block_size_bytes: u32,
    ) -> Result<Self> {
        let overlay = AeroSparseDisk::create(
            overlay_backend,
            AeroSparseConfig {
                disk_size_bytes: base.capacity_bytes(),
                block_size_bytes,
            },
        )?;
        Ok(Self { base, overlay })
    }

    pub fn open(base: Base, overlay_backend: OverlayBackend) -> Result<Self> {
        let overlay = AeroSparseDisk::open(overlay_backend)?;
        if overlay.capacity_bytes() != base.capacity_bytes() {
            return Err(DiskError::InvalidSparseHeader(
                "overlay size does not match base disk size",
            ));
        }
        Ok(Self { base, overlay })
    }

    pub fn overlay(&self) -> &AeroSparseDisk<OverlayBackend> {
        &self.overlay
    }

    pub fn overlay_mut(&mut self) -> &mut AeroSparseDisk<OverlayBackend> {
        &mut self.overlay
    }

    pub fn into_parts(self) -> (Base, AeroSparseDisk<OverlayBackend>) {
        (self.base, self.overlay)
    }
}

impl<Base: VirtualDisk, OverlayBackend: StorageBackend> VirtualDisk
    for AeroCowDisk<Base, OverlayBackend>
{
    fn capacity_bytes(&self) -> u64 {
        self.base.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let block_size = self.overlay.header().block_size_u64();
        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / block_size;
            let within = (abs % block_size) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (block_size as usize - within).min(remaining);

            if self.overlay.is_block_allocated(block_idx) {
                self.overlay.read_at(abs, &mut buf[pos..pos + chunk_len])?;
            } else {
                self.base.read_at(abs, &mut buf[pos..pos + chunk_len])?;
            }

            pos += chunk_len;
        }

        Ok(())
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> Result<()> {
        checked_range(offset, buf.len(), self.capacity_bytes())?;

        let block_size = self.overlay.header().block_size_u64();
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

            let (phys, existed) = self.overlay.ensure_block_allocated(block_idx)?;

            let full_block_write = within == 0 && chunk_len as u64 == block_size;
            if full_block_write {
                // No need to consult base; overwrite the whole block.
                self.overlay
                    .write_to_alloc_table(phys, 0, &buf[pos..pos + chunk_len])?;
                pos += chunk_len;
                continue;
            }

            if existed {
                // Existing overlay blocks already contain the correct bytes for regions we are
                // not touching; avoid a full-block read-modify-write.
                self.overlay
                    .write_to_alloc_table(phys, within, &buf[pos..pos + chunk_len])?;
                pos += chunk_len;
                continue;
            }

            // Newly allocated block: seed untouched bytes from the base disk, then apply the
            // guest write. Do this in small chunks to avoid allocating an entire block-sized
            // buffer (blocks can be large).
            let block_start = block_idx
                .checked_mul(block_size)
                .ok_or(DiskError::OffsetOverflow)?;
            let max_len_u64 = (self.capacity_bytes() - block_start).min(block_size);
            let max_len: usize = max_len_u64
                .try_into()
                .map_err(|_| DiskError::OffsetOverflow)?;

            let mut scratch = [0u8; 4096];

            // Prefix before the write.
            let mut base_off = block_start;
            let mut overlay_off = 0usize;
            let mut remaining_prefix = within.min(max_len);
            while remaining_prefix > 0 {
                let chunk = remaining_prefix.min(scratch.len());
                self.base.read_at(base_off, &mut scratch[..chunk])?;
                self.overlay
                    .write_to_alloc_table(phys, overlay_off, &scratch[..chunk])?;
                base_off = base_off
                    .checked_add(chunk as u64)
                    .ok_or(DiskError::OffsetOverflow)?;
                overlay_off += chunk;
                remaining_prefix -= chunk;
            }

            // The actual write.
            self.overlay
                .write_to_alloc_table(phys, within, &buf[pos..pos + chunk_len])?;

            // Suffix after the write.
            let write_end = within + chunk_len;
            if write_end < max_len {
                let mut base_off = block_start
                    .checked_add(write_end as u64)
                    .ok_or(DiskError::OffsetOverflow)?;
                let mut overlay_off = write_end;
                let mut remaining_suffix = max_len - write_end;
                while remaining_suffix > 0 {
                    let chunk = remaining_suffix.min(scratch.len());
                    self.base.read_at(base_off, &mut scratch[..chunk])?;
                    self.overlay
                        .write_to_alloc_table(phys, overlay_off, &scratch[..chunk])?;
                    base_off = base_off
                        .checked_add(chunk as u64)
                        .ok_or(DiskError::OffsetOverflow)?;
                    overlay_off += chunk;
                    remaining_suffix -= chunk;
                }
            }

            pos += chunk_len;
        }

        Ok(())
    }

    fn flush(&mut self) -> Result<()> {
        // Base is treated as read-only; flushing it is harmless.
        self.base.flush()?;
        self.overlay.flush()
    }
}
