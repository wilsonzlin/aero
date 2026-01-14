use memory::MemoryBus;

use aero_io_snapshot::io::storage::state::IdeBusMasterChannelState;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaDirection {
    /// Device -> guest memory.
    ToMemory,
    /// Guest memory -> device.
    FromMemory,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DmaError {
    /// The Bus Master command register direction did not match the queued request direction.
    DirectionMismatch,
    /// The PRD table ended before transferring the entire request buffer.
    PrdTooShort,
    /// The PRD table did not terminate before transferring the full buffer (missing EOT).
    PrdMissingEndOfTable,
}

pub type DmaResult<T> = Result<T, DmaError>;

#[derive(Debug)]
pub enum DmaCommit {
    AtaWrite { lba: u64, sectors: u64 },
}

#[derive(Debug)]
pub struct DmaRequest {
    pub direction: DmaDirection,
    pub buffer: Vec<u8>,
    pub commit: Option<DmaCommit>,
}

impl DmaRequest {
    pub fn ata_read(buffer: Vec<u8>) -> Self {
        Self {
            direction: DmaDirection::ToMemory,
            buffer,
            commit: None,
        }
    }

    pub fn ata_write(buffer: Vec<u8>, lba: u64, sectors: u64) -> Self {
        Self {
            direction: DmaDirection::FromMemory,
            buffer,
            commit: Some(DmaCommit::AtaWrite { lba, sectors }),
        }
    }

    pub fn atapi_data_in(buffer: Vec<u8>) -> Self {
        Self {
            direction: DmaDirection::ToMemory,
            buffer,
            commit: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrdEntry {
    pub addr: u32,
    pub byte_count: u16,
    pub end_of_table: bool,
}

impl PrdEntry {
    pub fn read_from(mem: &mut dyn MemoryBus, paddr: u64) -> Self {
        let addr = mem.read_u32(paddr);
        // Use wrapping arithmetic so malformed guest PRD pointers can't panic under
        // overflow-check builds.
        let byte_count = mem.read_u16(paddr.wrapping_add(4));
        let flags = mem.read_u16(paddr.wrapping_add(6));
        Self {
            addr,
            byte_count,
            end_of_table: (flags & 0x8000) != 0,
        }
    }

    fn effective_len(&self) -> usize {
        // Per Bus Master IDE spec, a byte_count of 0 encodes 64KiB.
        let c = self.byte_count as usize;
        if c == 0 {
            65536
        } else {
            c
        }
    }
}

/// One Bus Master IDE register block (primary or secondary).
#[derive(Debug, Clone, Copy)]
pub struct BusMasterChannel {
    cmd: u8,
    status: u8,
    prd_addr: u32,
    drive_dma_capable: [bool; 2],
}

impl BusMasterChannel {
    // Defensive cap to prevent pathological PRD tables (e.g. millions of 1-byte entries) from
    // turning a single synchronous DMA tick into an effectively unbounded loop.
    //
    // Real OS drivers use small PRD lists with reasonably-sized segments; treat exceeding this cap
    // as a PRD table error.
    const MAX_PRD_ENTRIES_PER_DMA: usize = 65_536;

    pub fn new() -> Self {
        Self {
            cmd: 0,
            status: 0,
            prd_addr: 0,
            drive_dma_capable: [false; 2],
        }
    }

    /// Reset the Bus Master IDE register block back to its power-on state.
    ///
    /// This intentionally preserves `drive_dma_capable` because it reflects attached device/media
    /// capabilities (host-managed) rather than guest-programmable state.
    pub fn reset(&mut self) {
        self.cmd = 0;
        self.status = 0;
        self.prd_addr = 0;
    }

    pub fn set_drive_dma_capable(&mut self, drive: usize, capable: bool) {
        if drive < 2 {
            self.drive_dma_capable[drive] = capable;
        }
    }

    pub fn is_started(&self) -> bool {
        (self.cmd & 0x01) != 0
    }

    pub fn read(&self, reg_off: u16, size: u8) -> u32 {
        if size == 0 {
            return 0;
        }
        match reg_off {
            0 => self.cmd as u32,
            2 => {
                let mut st = self.status;
                // DMA capability bits (read-only).
                if self.drive_dma_capable[0] {
                    st |= 1 << 5;
                }
                if self.drive_dma_capable[1] {
                    st |= 1 << 6;
                }
                st as u32
            }
            4 => match size {
                4 => self.prd_addr,
                2 => self.prd_addr & 0xFFFF,
                1 => self.prd_addr & 0xFF,
                _ => 0,
            },
            5 => (self.prd_addr >> 8) & 0xFF,
            6 => (self.prd_addr >> 16) & 0xFF,
            7 => (self.prd_addr >> 24) & 0xFF,
            _ => 0,
        }
    }

    pub fn write(&mut self, reg_off: u16, size: u8, val: u32) {
        if size == 0 {
            return;
        }
        match reg_off {
            0 => {
                // Only bits 0 (start) and 3 (direction) are writable.
                let masked = (val as u8) & 0x09;
                self.cmd = (self.cmd & !0x09) | masked;
                if (masked & 0x01) == 0 {
                    self.status &= !0x01; // clear active
                }
            }
            2 => {
                // Writing 1 clears bits.
                let v = val as u8;
                if (v & 0x04) != 0 {
                    self.status &= !0x04; // IRQ
                }
                if (v & 0x02) != 0 {
                    self.status &= !0x02; // error
                }
            }
            4 => {
                if size == 4 {
                    self.prd_addr = val & 0xFFFF_FFFC;
                }
            }
            _ => {}
        }
    }

    pub fn execute_dma(&mut self, mem: &mut dyn MemoryBus, req: &mut DmaRequest) -> DmaResult<()> {
        let bm_dir = if (self.cmd & 0x08) != 0 {
            DmaDirection::ToMemory
        } else {
            DmaDirection::FromMemory
        };
        if bm_dir != req.direction {
            return Err(DmaError::DirectionMismatch);
        }

        self.status |= 0x01; // active

        let mut remaining = req.buffer.len();
        let mut buf_off = 0usize;
        let mut prd_ptr = self.prd_addr as u64;

        // Guard against a guest that provides a PRD list with no EOT bit set and uses
        // byte_count=0 (64KiB) entries forever. The transfer will still complete once
        // `remaining` hits 0, but we want to guarantee termination even if the guest provides
        // pathological PRDs that don't make progress (e.g. zero-length buffer).
        if remaining == 0 {
            return Ok(());
        }

        let mut saw_eot = false;
        let mut entries_processed = 0usize;
        while remaining > 0 {
            if entries_processed >= Self::MAX_PRD_ENTRIES_PER_DMA {
                // We ran out of PRD entries we're willing to process; treat as a malformed/hostile
                // PRD list (missing EOT / too fragmented).
                return Err(DmaError::PrdMissingEndOfTable);
            }
            entries_processed += 1;

            let prd = PrdEntry::read_from(mem, prd_ptr);
            prd_ptr = prd_ptr.wrapping_add(8);

            let seg_len = prd.effective_len().min(remaining);
            let addr = prd.addr as u64;

            match req.direction {
                DmaDirection::ToMemory => {
                    mem.write_physical(addr, &req.buffer[buf_off..buf_off + seg_len]);
                }
                DmaDirection::FromMemory => {
                    mem.read_physical(addr, &mut req.buffer[buf_off..buf_off + seg_len]);
                }
            }

            buf_off += seg_len;
            remaining -= seg_len;

            if prd.end_of_table {
                saw_eot = true;
                if remaining != 0 {
                    return Err(DmaError::PrdTooShort);
                }
                break;
            }
        }

        // If the DMA transfer completed without encountering an EOT PRD, treat this as a PRD
        // table error. Real hardware behavior varies, but Windows drivers expect an EOT.
        if !saw_eot {
            return Err(DmaError::PrdMissingEndOfTable);
        }

        Ok(())
    }

    pub fn finish_success(&mut self) {
        self.status &= !0x01; // active
        self.status &= !0x02; // error
        self.status |= 0x04; // interrupt
    }

    pub fn finish_error(&mut self) {
        self.status &= !0x01;
        self.status |= 0x02; // error
        self.status |= 0x04; // interrupt
    }

    pub fn snapshot_state(&self) -> IdeBusMasterChannelState {
        IdeBusMasterChannelState {
            cmd: self.cmd,
            status: self.status,
            prd_addr: self.prd_addr,
        }
    }

    pub fn restore_state(&mut self, state: &IdeBusMasterChannelState) {
        // Keep the restored state consistent with what the guest can program via register writes.
        self.cmd = state.cmd & 0x09; // start + direction only
        self.status = state.status & 0x07; // active + error + irq only (capability bits are derived)
        self.prd_addr = state.prd_addr & 0xFFFF_FFFC;
    }
}

impl Default for BusMasterChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_storage::SECTOR_SIZE;
    use memory::{Bus, MemoryBus};

    #[test]
    fn size0_access_is_noop() {
        let mut bm = BusMasterChannel::new();

        bm.write(0, 1, 0x09);
        assert_eq!(bm.read(0, 1), 0x09);

        // Regression guard: size-0 accesses must be true no-ops.
        assert_eq!(bm.read(0, 0), 0);
        bm.write(0, 0, 0);
        assert_eq!(bm.read(0, 1), 0x09);
    }

    #[test]
    fn reset_preserves_dma_capability_bits() {
        let mut bm = BusMasterChannel::new();
        bm.set_drive_dma_capable(0, true);
        bm.set_drive_dma_capable(1, true);

        // Dirty the guest-visible register state.
        bm.write(0, 1, 0x09);
        bm.write(4, 4, 0x1234_5678);
        bm.finish_error();

        bm.reset();

        assert_eq!(bm.read(0, 1), 0);
        assert_eq!(bm.read(4, 4), 0);
        // Capability bits are derived from attached devices and must survive reset.
        assert_eq!(bm.read(2, 1), 0x60);
    }

    #[test]
    fn prd_entry_cap_prevents_pathological_dma_loop() {
        // Construct a PRD list that would require more than `MAX_PRD_ENTRIES_PER_DMA` entries to
        // transfer the request buffer. The DMA engine should stop early with a bounded amount of
        // work instead of iterating forever (or for millions of entries).
        let prd_entries = BusMasterChannel::MAX_PRD_ENTRIES_PER_DMA + 1;
        let prd_base: u64 = 0x1000;

        // 8 bytes per PRD entry, plus a bit of headroom.
        let mem_size = (prd_base as usize)
            .saturating_add(prd_entries.saturating_mul(8))
            .saturating_add(8);
        let mut mem = Bus::new(mem_size);

        // PRDs: 1 byte per entry; EOT only on the final entry (which is beyond our cap).
        for i in 0..prd_entries {
            let entry_addr = prd_base.wrapping_add((i as u64).wrapping_mul(8));
            mem.write_u32(entry_addr, 0); // addr (within RAM)
            mem.write_u16(entry_addr + 4, 1); // byte_count
            let flags = if i == prd_entries - 1 { 0x8000u16 } else { 0 };
            mem.write_u16(entry_addr + 6, flags);
        }

        let mut bm = BusMasterChannel::new();
        bm.write(4, 4, prd_base as u32);
        // Direction=ToMemory (bit 3), start bit set (bit 0).
        bm.write(0, 1, 0x09);

        let mut req =
            DmaRequest::ata_read(vec![0u8; BusMasterChannel::MAX_PRD_ENTRIES_PER_DMA + 1]);
        let err = bm.execute_dma(&mut mem, &mut req).unwrap_err();
        assert_eq!(err, DmaError::PrdMissingEndOfTable);
    }

    #[test]
    fn dma_direction_mismatch_is_reported() {
        let mut bm = BusMasterChannel::new();
        let mut mem = Bus::new(0x1000);

        // Program Bus Master direction=ToMemory.
        bm.write(0, 1, 0x09);

        let mut req = DmaRequest {
            direction: DmaDirection::FromMemory,
            buffer: vec![0u8; 4],
            commit: None,
        };

        let err = bm.execute_dma(&mut mem, &mut req).unwrap_err();
        assert_eq!(err, DmaError::DirectionMismatch);

        // Direction mismatch should not mark the DMA engine active until a transfer begins.
        assert_eq!(bm.read(2, 1) & 0x01, 0);

        bm.finish_error();
        let st = bm.read(2, 1) as u8;
        assert_eq!(
            st & 0x07,
            0x06,
            "finish_error should set IRQ+ERR and clear ACTIVE"
        );
    }

    #[test]
    fn prd_too_short_is_reported() {
        let mut bm = BusMasterChannel::new();

        let prd_base: u64 = 0x1000;
        let dma_buf: u64 = 0x2000;
        let mut mem = Bus::new(0x4000);

        // Single 256-byte PRD with EOT, but the request buffer is 512 bytes.
        mem.write_u32(prd_base, dma_buf as u32);
        mem.write_u16(prd_base + 4, 256);
        mem.write_u16(prd_base + 6, 0x8000);

        bm.write(4, 4, prd_base as u32);
        // Direction=ToMemory.
        bm.write(0, 1, 0x09);

        let mut req = DmaRequest::ata_read(vec![0xA5; SECTOR_SIZE]);
        let err = bm.execute_dma(&mut mem, &mut req).unwrap_err();
        assert_eq!(err, DmaError::PrdTooShort);
        assert_ne!(
            bm.read(2, 1) & 0x01,
            0,
            "ACTIVE should remain set until finish_* is called"
        );

        // The first segment should have been transferred before we discovered the table ended.
        let mut out = vec![0u8; 256];
        mem.read_physical(dma_buf, &mut out);
        assert_eq!(&out[..], &[0xA5; 256]);

        bm.finish_error();
        let st = bm.read(2, 1) as u8;
        assert_eq!(
            st & 0x07,
            0x06,
            "finish_error should set IRQ+ERR and clear ACTIVE"
        );
    }

    #[test]
    fn prd_missing_eot_is_reported() {
        let mut bm = BusMasterChannel::new();

        let prd_base: u64 = 0x1000;
        let dma_buf0: u64 = 0x2000;
        let dma_buf1: u64 = 0x3000;
        let mut mem = Bus::new(0x4000);

        // Two PRDs covering the full buffer but neither has EOT set.
        mem.write_u32(prd_base, dma_buf0 as u32);
        mem.write_u16(prd_base + 4, 256);
        mem.write_u16(prd_base + 6, 0x0000);

        mem.write_u32(prd_base + 8, dma_buf1 as u32);
        mem.write_u16(prd_base + 8 + 4, 256);
        mem.write_u16(prd_base + 8 + 6, 0x0000);

        bm.write(4, 4, prd_base as u32);
        bm.write(0, 1, 0x09); // direction=ToMemory

        let buf: Vec<u8> = (0u16..(SECTOR_SIZE as u16))
            .map(|v| (v & 0xff) as u8)
            .collect();
        let mut req = DmaRequest::ata_read(buf.clone());
        let err = bm.execute_dma(&mut mem, &mut req).unwrap_err();
        assert_eq!(err, DmaError::PrdMissingEndOfTable);
        assert_ne!(
            bm.read(2, 1) & 0x01,
            0,
            "ACTIVE should remain set until finish_* is called"
        );

        // Data should still have been written into the guest buffers.
        let mut seg0 = vec![0u8; 256];
        let mut seg1 = vec![0u8; 256];
        mem.read_physical(dma_buf0, &mut seg0);
        mem.read_physical(dma_buf1, &mut seg1);
        assert_eq!(seg0, buf[..256]);
        assert_eq!(seg1, buf[256..]);

        bm.finish_error();
        let st = bm.read(2, 1) as u8;
        assert_eq!(
            st & 0x07,
            0x06,
            "finish_error should set IRQ+ERR and clear ACTIVE"
        );
    }
}
