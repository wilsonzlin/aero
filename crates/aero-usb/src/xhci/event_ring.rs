//! xHCI Event Ring producer.
//!
//! The Event Ring lives in guest memory and is described by an Event Ring Segment Table (ERST)
//! programmed by the guest. The host controller writes Event TRBs into the ring and notifies the
//! guest via Interrupter registers (IMAN/IP).
//!
//! This module implements only the minimal behaviour required for driver bring-up:
//! - Single-producer event TRB enqueue with cycle state tracking.
//! - Segment walking and cycle toggling when the producer wraps.
//! - Tracking ERDP writes so we can (conservatively) detect ring-full scenarios.

use alloc::vec::Vec;

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::SnapshotResult;

use crate::MemoryBus;

use super::interrupter::InterrupterRegs;
use super::trb::{Trb, TRB_LEN};

/// Maximum number of ERST entries the device model will scan when mapping ERDP back into a segment.
///
/// This is a defensive cap against pathological guests that set `ERSTSZ` to a huge value and then
/// perform frequent ERDP writes to force the controller into scanning megabytes of guest RAM.
const ERST_SCAN_LIMIT: usize = 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
struct RingPos {
    seg: u16,
    off: u32,
}

#[derive(Clone, Copy, Debug)]
struct ErstEntry {
    base: u64,
    size_trbs: u32,
}

impl ErstEntry {
    fn read(mem: &mut dyn MemoryBus, erstba: u64, idx: u16) -> Option<Self> {
        let off = (idx as u64).checked_mul(16)?;
        let paddr = erstba.checked_add(off)?;
        let base = mem.read_u64(paddr);
        let size = mem.read_u32(paddr + 8) & 0xffff;
        Some(Self {
            base: base & !0x0f,
            size_trbs: size,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EnqueueError {
    /// The guest has not configured the Event Ring (ERSTSZ/ERSTBA/ERDP).
    NotConfigured,
    /// The event ring appears to be full (producer has caught up to consumer).
    RingFull,
    /// The guest-provided ERST configuration is invalid (e.g. zero-sized segment).
    InvalidConfig,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct EventRingProducer {
    // Cached ERST configuration.
    erstsz: u16,
    erstba: u64,
    last_erst_gen: u64,

    // Producer state.
    prod_pos: RingPos,
    prod_cycle: bool,

    // Consumer state derived from ERDP writes.
    cons_pos: RingPos,
    cons_cycle: bool,
    last_erdp_gen: u64,

    // Whether we have successfully initialised the ring pointers.
    ready: bool,
}

impl EventRingProducer {
    pub(crate) fn save_snapshot(&self) -> Vec<u8> {
        Encoder::new()
            .u16(self.erstsz)
            .u64(self.erstba)
            .u64(self.last_erst_gen)
            .u16(self.prod_pos.seg)
            .u32(self.prod_pos.off)
            .bool(self.prod_cycle)
            .u16(self.cons_pos.seg)
            .u32(self.cons_pos.off)
            .bool(self.cons_cycle)
            .u64(self.last_erdp_gen)
            .bool(self.ready)
            .finish()
    }

    pub(crate) fn load_snapshot(&mut self, buf: &[u8]) -> SnapshotResult<()> {
        let mut d = Decoder::new(buf);
        self.erstsz = d.u16()?;
        self.erstba = d.u64()? & !0x3f;
        self.last_erst_gen = d.u64()?;

        self.prod_pos.seg = d.u16()?;
        self.prod_pos.off = d.u32()?;
        self.prod_cycle = d.bool()?;

        self.cons_pos.seg = d.u16()?;
        self.cons_pos.off = d.u32()?;
        self.cons_cycle = d.bool()?;
        self.last_erdp_gen = d.u64()?;

        self.ready = d.bool()?;
        d.finish()?;

        // Defensive validation: if the restored positions don't make sense for the configured ERST
        // size, mark the ring as not ready so it will be re-initialised from ERDP on first use.
        if self.erstsz == 0 || self.erstba == 0 {
            self.ready = false;
            self.prod_pos = RingPos { seg: 0, off: 0 };
            self.cons_pos = RingPos { seg: 0, off: 0 };
            self.prod_cycle = true;
            self.cons_cycle = true;
        } else {
            if self.prod_pos.seg >= self.erstsz || self.cons_pos.seg >= self.erstsz {
                self.ready = false;
                self.prod_pos = RingPos { seg: 0, off: 0 };
                self.cons_pos = RingPos { seg: 0, off: 0 };
                self.prod_cycle = true;
                self.cons_cycle = true;
            }
        }
        Ok(())
    }

    pub(crate) fn refresh(&mut self, mem: &mut dyn MemoryBus, intr: &InterrupterRegs) {
        // Detect ERST configuration changes (including resets) and reinitialise the ring state.
        let new_erstsz = intr.erstsz();
        let new_erstba = intr.erstba();
        if intr.erst_gen != self.last_erst_gen
            || new_erstsz != self.erstsz
            || new_erstba != self.erstba
        {
            self.last_erst_gen = intr.erst_gen;
            self.erstsz = new_erstsz;
            self.erstba = new_erstba;
            self.ready = false;
            self.prod_pos = RingPos { seg: 0, off: 0 };
            self.cons_pos = RingPos { seg: 0, off: 0 };
            self.prod_cycle = true;
            self.cons_cycle = true;
            self.last_erdp_gen = 0;
        }

        if !self.is_configured(intr) {
            // Treat a missing ERDP pointer as "not configured" and force the ring state back to an
            // uninitialised baseline so a future configuration write starts from scratch.
            self.ready = false;
            return;
        }

        // Observe ERDP writes (even if the guest writes the same value).
        //
        // For bring-up convenience, also perform an initial ERDP mapping pass when the ring is not
        // yet ready (e.g. after snapshot restore) even if no ERDP write has been observed.
        let saw_erdp_write = intr.erdp_gen != self.last_erdp_gen;
        if saw_erdp_write {
            self.last_erdp_gen = intr.erdp_gen;
        } else if self.ready {
            return;
        }

        let Some(new_pos) = self.map_ptr_to_pos(mem, intr.erdp_ptr()) else {
            // If we can't map ERDP back to a segment, treat the ring as unconfigured.
            self.ready = false;
            return;
        };

        let old_pos = self.cons_pos;
        if new_pos < old_pos {
            self.cons_cycle = !self.cons_cycle;
        } else if new_pos == old_pos && saw_erdp_write {
            // ERDP pointer did not change. This can legitimately happen when software consumes a
            // whole number of ring laps (e.g. exactly one full ring) and writes ERDP back to the
            // same address. If the ring was previously full, assume software advanced through the
            // wrap point and toggle the consumer cycle state to unblock the producer.
            if self.is_full() {
                self.cons_cycle = !self.cons_cycle;
            }
        }
        self.cons_pos = new_pos;

        if !self.ready {
            // Start the producer at the consumer position so the ring is initially empty.
            self.prod_pos = self.cons_pos;
            self.prod_cycle = self.cons_cycle;
            self.ready = true;
        }
    }

    pub(crate) fn try_enqueue(
        &mut self,
        mem: &mut dyn MemoryBus,
        intr: &InterrupterRegs,
        trb: Trb,
    ) -> Result<(), EnqueueError> {
        if !self.is_configured(intr) {
            return Err(EnqueueError::NotConfigured);
        }
        if !self.ready {
            // Attempt to lazily initialise from the current ERDP pointer.
            self.refresh(mem, intr);
            if !self.ready {
                // ERST is configured but ERDP did not map into any segment: treat as an invalid
                // guest configuration (should raise HCE in the controller model).
                return Err(EnqueueError::InvalidConfig);
            }
        }

        if self.is_full() {
            return Err(EnqueueError::RingFull);
        }

        let paddr = self.pos_to_paddr(mem, self.prod_pos)?;
        let mut trb = trb;
        trb.set_cycle(self.prod_cycle);
        trb.write_to(mem, paddr);

        self.advance_producer(mem)?;
        Ok(())
    }

    fn is_configured(&self, intr: &InterrupterRegs) -> bool {
        self.erstsz != 0 && self.erstba != 0 && intr.erdp_ptr() != 0
    }

    fn is_full(&self) -> bool {
        self.ready && self.prod_pos == self.cons_pos && self.prod_cycle != self.cons_cycle
    }

    fn pos_to_paddr(&self, mem: &mut dyn MemoryBus, pos: RingPos) -> Result<u64, EnqueueError> {
        let Some(entry) = ErstEntry::read(mem, self.erstba, pos.seg) else {
            return Err(EnqueueError::InvalidConfig);
        };
        if entry.size_trbs == 0 {
            return Err(EnqueueError::InvalidConfig);
        }
        if pos.off >= entry.size_trbs {
            return Err(EnqueueError::InvalidConfig);
        }
        let off_bytes = (pos.off as u64)
            .checked_mul(TRB_LEN as u64)
            .ok_or(EnqueueError::InvalidConfig)?;
        entry
            .base
            .checked_add(off_bytes)
            .ok_or(EnqueueError::InvalidConfig)
    }

    fn map_ptr_to_pos(&self, mem: &mut dyn MemoryBus, ptr: u64) -> Option<RingPos> {
        let mut scanned = 0usize;
        for seg in 0..self.erstsz {
            scanned += 1;
            if scanned > ERST_SCAN_LIMIT {
                return None;
            }

            let entry = ErstEntry::read(mem, self.erstba, seg)?;
            if entry.size_trbs == 0 {
                continue;
            }
            let seg_len_bytes = (entry.size_trbs as u64).checked_mul(TRB_LEN as u64)?;
            let end = entry.base.checked_add(seg_len_bytes)?;
            if ptr < entry.base || ptr >= end {
                continue;
            }
            let rel = ptr - entry.base;
            if !rel.is_multiple_of(TRB_LEN as u64) {
                return None;
            }
            let off = (rel / (TRB_LEN as u64)) as u32;
            return Some(RingPos { seg, off });
        }
        None
    }

    fn advance_producer(&mut self, mem: &mut dyn MemoryBus) -> Result<(), EnqueueError> {
        let Some(entry) = ErstEntry::read(mem, self.erstba, self.prod_pos.seg) else {
            return Err(EnqueueError::InvalidConfig);
        };
        if entry.size_trbs == 0 {
            return Err(EnqueueError::InvalidConfig);
        }

        let next_off = self.prod_pos.off + 1;
        if next_off < entry.size_trbs {
            self.prod_pos.off = next_off;
            return Ok(());
        }

        // Move to the next segment.
        let mut next_seg = self.prod_pos.seg + 1;
        if next_seg >= self.erstsz {
            next_seg = 0;
            self.prod_cycle = !self.prod_cycle;
        }
        self.prod_pos = RingPos {
            seg: next_seg,
            off: 0,
        };
        Ok(())
    }
}
