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
        let mut pos = 0usize;
        while pos < buf.len() {
            let abs = offset + pos as u64;
            let block_idx = abs / block_size;
            let within = (abs % block_size) as usize;
            let remaining = buf.len() - pos;
            let chunk_len = (block_size as usize - within).min(remaining);

            let (phys, existed) = self.overlay.ensure_block_allocated(block_idx)?;

            let full_block_write = within == 0 && chunk_len as u64 == block_size;
            if full_block_write {
                // No need to consult base; overwrite the whole block.
                self.overlay
                    .write_to_alloc_table(phys, 0, &buf[pos..pos + chunk_len])?;
                pos += chunk_len;
                continue;
            }

            // For partial writes we must preserve bytes we are not touching.
            // If the block is newly allocated, seed it from the base disk first.
            let mut block = vec![0u8; block_size as usize];

            if existed {
                self.overlay.read_from_alloc_table(phys, 0, &mut block)?;
            } else {
                // Read base data for this block (truncate for the final partial block).
                let block_start = block_idx
                    .checked_mul(block_size)
                    .ok_or(DiskError::OffsetOverflow)?;
                let max_len = (self.capacity_bytes() - block_start).min(block_size);
                self.base
                    .read_at(block_start, &mut block[..max_len as usize])?;
            }

            block[within..within + chunk_len].copy_from_slice(&buf[pos..pos + chunk_len]);
            self.overlay.write_to_alloc_table(phys, 0, &block)?;

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
