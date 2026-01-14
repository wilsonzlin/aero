//! TRB ring walking helpers.
//!
//! xHCI rings live in guest memory. The host controller consumes TRBs from a dequeue pointer while
//! tracking a "cycle state" bit to distinguish new entries from old ones. Rings are segmented using
//! Link TRBs, which can optionally toggle the cycle state when wrapping.

use crate::MemoryBus;

use super::trb::{Trb, TrbType, TRB_LEN};

/// A TRB fetched from a ring along with its guest address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingItem {
    pub paddr: u64,
    pub trb: Trb,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingError {
    /// The ring could not be advanced within the provided step budget.
    ///
    /// This is used as a safety measure against malformed rings (e.g. a loop of Link TRBs that
    /// never reaches a non-Link TRB or a cycle mismatch).
    StepBudgetExceeded,

    /// Dequeue pointer arithmetic overflowed `u64`.
    AddressOverflow,

    /// Link TRB pointed at an invalid segment base address (e.g. null).
    InvalidLinkTarget,

    /// TRB fetch returned an all-ones value (commonly produced by unmapped DMA reads).
    ///
    /// This is treated as a fatal ring error to avoid "successfully" processing garbage TRBs when
    /// the guest misprograms ring pointers or DMA is unavailable.
    InvalidDmaRead,
}

/// Result of polling a ring cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RingPoll {
    /// A TRB was available and returned. The cursor has been advanced past the TRB.
    Ready(RingItem),
    /// The next TRB is not yet available (cycle bit mismatch). The cursor is unchanged.
    NotReady,
    /// The ring was malformed or otherwise could not be advanced safely.
    Err(RingError),
}

/// Walks a TRB ring in guest memory.
///
/// The cursor tracks a dequeue pointer (`paddr`) and expected cycle state. It will transparently
/// follow Link TRBs and perform cycle toggling when `TC=1`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RingCursor {
    paddr: u64,
    cycle: bool,
}

impl RingCursor {
    /// Create a new cursor from a dequeue pointer and cycle state.
    ///
    /// The pointer is masked to 16-byte alignment (xHCI ring pointers store flags in low bits).
    pub const fn new(dequeue_ptr: u64, cycle: bool) -> Self {
        Self {
            paddr: dequeue_ptr & !0x0f,
            cycle,
        }
    }

    /// Derive a command ring cursor from CRCR.
    ///
    /// xHCI encodes the ring cycle state in bit 0 and the base pointer in bits 63:6.
    pub const fn from_crcr(crcr: u64) -> Self {
        let cycle = (crcr & 1) != 0;
        let ptr = crcr & !0x3f;
        Self::new(ptr, cycle)
    }

    /// Current dequeue pointer (16-byte aligned).
    pub const fn dequeue_ptr(&self) -> u64 {
        self.paddr
    }

    /// Current expected cycle state.
    pub const fn cycle_state(&self) -> bool {
        self.cycle
    }

    /// Poll the ring and return the next available TRB (if any).
    ///
    /// `step_budget` bounds the number of TRBs read while trying to find a returnable TRB. This
    /// prevents an infinite loop on malformed rings, such as a chain of Link TRBs that never
    /// reaches a non-Link TRB.
    pub fn poll<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, step_budget: usize) -> RingPoll {
        for _ in 0..step_budget {
            let mut bytes = [0u8; TRB_LEN];
            mem.read_physical(self.paddr, &mut bytes);
            if bytes.iter().all(|&b| b == 0xFF) {
                return RingPoll::Err(RingError::InvalidDmaRead);
            }
            let trb = Trb::from_bytes(bytes);

            if trb.cycle() != self.cycle {
                return RingPoll::NotReady;
            }

            if matches!(trb.trb_type(), TrbType::Link) {
                // Link TRB parameter contains the next segment pointer; low bits are reserved.
                let next = trb.link_segment_ptr();
                if next == 0 {
                    return RingPoll::Err(RingError::InvalidLinkTarget);
                }
                self.paddr = next;
                if trb.link_toggle_cycle() {
                    self.cycle = !self.cycle;
                }
                continue;
            }

            let item = RingItem {
                paddr: self.paddr,
                trb,
            };

            if let Err(err) = self.consume() {
                return RingPoll::Err(err);
            }

            return RingPoll::Ready(item);
        }

        RingPoll::Err(RingError::StepBudgetExceeded)
    }

    /// Peek the next available TRB without consuming it.
    ///
    /// This behaves like [`RingCursor::poll`] except the cursor is left pointing at the returned
    /// TRB. Link TRBs are still consumed transparently so the cursor never gets "stuck" on a Link
    /// TRB across calls.
    ///
    /// This is useful for modelling NAK behaviour: when a transfer TRB would NAK, the host
    /// controller must *not* advance its dequeue pointer past the TRB so it can be retried later.
    pub fn peek<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, step_budget: usize) -> RingPoll {
        for _ in 0..step_budget {
            let mut bytes = [0u8; TRB_LEN];
            mem.read_physical(self.paddr, &mut bytes);
            if bytes.iter().all(|&b| b == 0xFF) {
                return RingPoll::Err(RingError::InvalidDmaRead);
            }
            let trb = Trb::from_bytes(bytes);

            if trb.cycle() != self.cycle {
                return RingPoll::NotReady;
            }

            if matches!(trb.trb_type(), TrbType::Link) {
                let next = trb.link_segment_ptr();
                if next == 0 {
                    return RingPoll::Err(RingError::InvalidLinkTarget);
                }
                self.paddr = next;
                if trb.link_toggle_cycle() {
                    self.cycle = !self.cycle;
                }
                continue;
            }

            return RingPoll::Ready(RingItem {
                paddr: self.paddr,
                trb,
            });
        }

        RingPoll::Err(RingError::StepBudgetExceeded)
    }

    /// Consume the current TRB by advancing the dequeue pointer by one TRB.
    pub fn consume(&mut self) -> Result<(), RingError> {
        self.paddr = self
            .paddr
            .checked_add(TRB_LEN as u64)
            .ok_or(RingError::AddressOverflow)?;
        Ok(())
    }
}
