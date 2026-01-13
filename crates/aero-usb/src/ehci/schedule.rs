use alloc::vec::Vec;

use crate::memory::MemoryBus;

/// Max number of horizontal QH links walked in the asynchronous schedule per 1ms tick.
///
/// This bounds the work `tick_1ms()` can do even if the guest provides an adversarial schedule.
pub const MAX_ASYNC_QH_VISITS: usize = 1024;

/// Max number of qTDs walked per QH per 1ms tick.
pub const MAX_QTD_STEPS_PER_QH: usize = 1024;

/// Max number of periodic frame list links walked per frame.
///
/// The periodic schedule can contain a mixture of QHs (interrupt) and other element types (iTD,
/// siTD, FSTN). We treat unknown types as opaque nodes with a "next link pointer" at offset 0 and
/// enforce a strict hop budget.
pub const MAX_PERIODIC_LINKS_PER_FRAME: usize = 1024;

/// Errors produced by schedule traversal/processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    AsyncQhBudgetExceeded,
    AsyncQhCycle,
    QtdBudgetExceeded,
    QtdCycle,
    PeriodicBudgetExceeded,
    PeriodicCycle,
}

// EHCI link pointer encoding (Queue Head / periodic list entries).
//
// Bit 0: Terminate (T)
// Bits 2:1: Type (0=iTD, 1=QH, 2=siTD, 3=FSTN) for frame-list pointers; ignored for qTD pointers.
// Bits 31:5: Address (32-byte aligned)
const LINK_T: u32 = 1 << 0;
const LINK_TYPE_SHIFT: u32 = 1;
const LINK_TYPE_MASK: u32 = 0x3;
const LINK_TYPE_QH: u32 = 0x1;
const LINK_ADDR_MASK: u32 = 0xffff_ffe0;

const QH_NEXT_QTD_OFFSET: u32 = 0x10;

#[derive(Clone, Copy, Debug)]
struct LinkPointer(u32);

impl LinkPointer {
    fn terminated(self) -> bool {
        self.0 & LINK_T != 0
    }

    fn addr(self) -> u32 {
        self.0 & LINK_ADDR_MASK
    }

    fn ty(self) -> u32 {
        (self.0 >> LINK_TYPE_SHIFT) & LINK_TYPE_MASK
    }

    fn is_qh(self) -> bool {
        self.ty() == LINK_TYPE_QH
    }
}

fn vec_contains(v: &[u32], needle: u32) -> bool {
    v.iter().any(|&x| x == needle)
}

pub fn walk_periodic(
    mem: &mut dyn MemoryBus,
    periodiclistbase: u32,
    frame_index: u16,
) -> Result<(), ScheduleError> {
    if periodiclistbase == 0 {
        return Ok(());
    }
    let entry_addr = periodiclistbase.wrapping_add(frame_index as u32 * 4) as u64;
    let mut link = LinkPointer(mem.read_u32(entry_addr));

    let mut visited: Vec<u32> = Vec::with_capacity(16);
    for _step in 0..MAX_PERIODIC_LINKS_PER_FRAME {
        if link.terminated() {
            return Ok(());
        }

        let addr = link.addr();
        if addr == 0 {
            return Ok(());
        }

        if vec_contains(&visited, addr) {
            return Err(ScheduleError::PeriodicCycle);
        }
        visited.push(addr);

        // If this is a QH, also bound the qTD chain scan.
        if link.is_qh() {
            let qtd_ptr = LinkPointer(mem.read_u32(addr.wrapping_add(QH_NEXT_QTD_OFFSET) as u64));
            walk_qtd_chain(mem, qtd_ptr)?;
        }

        // Periodic schedule element types all begin with a "next link pointer" field at offset 0,
        // so we can treat unknown types as opaque nodes and still walk forward safely.
        link = LinkPointer(mem.read_u32(addr as u64));
    }

    Err(ScheduleError::PeriodicBudgetExceeded)
}

fn walk_qtd_chain(mem: &mut dyn MemoryBus, mut ptr: LinkPointer) -> Result<(), ScheduleError> {
    let mut visited: Vec<u32> = Vec::with_capacity(8);

    for _step in 0..MAX_QTD_STEPS_PER_QH {
        if ptr.terminated() {
            return Ok(());
        }

        let addr = ptr.addr();
        if addr == 0 {
            return Ok(());
        }

        if vec_contains(&visited, addr) {
            return Err(ScheduleError::QtdCycle);
        }
        visited.push(addr);

        // qTD next pointer is at offset 0.
        ptr = LinkPointer(mem.read_u32(addr as u64));
    }

    Err(ScheduleError::QtdBudgetExceeded)
}
