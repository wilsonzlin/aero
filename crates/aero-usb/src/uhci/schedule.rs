use alloc::vec;

use crate::device::{UsbInResult, UsbOutResult};
use crate::hub::RootHub;
use crate::memory::MemoryBus;
use crate::visited_set::VisitedSet;
use crate::SetupPacket;

use super::regs::{
    USBINT_CAUSE_IOC, USBINT_CAUSE_SHORT_PACKET, USBSTS_HSE, USBSTS_USBERRINT, USBSTS_USBINT,
};

/// Maximum number of schedule link pointers walked per frame.
///
/// UHCI schedule structures are guest-controlled and can contain cycles. Without bounding, an
/// adversarial schedule can hang `tick_1ms()` in an infinite loop.
const MAX_SCHEDULE_LINKS_PER_FRAME: usize = 4096;

/// Maximum number of consecutive TDs processed as part of a single TD chain walk.
///
/// `process_td_chain` previously used recursion to process a linked list of TDs; an adversarial
/// guest could construct a very deep chain and risk overflowing the Rust stack. Keep this bounded
/// and iterative.
const MAX_TD_CHAIN_STEPS: usize = 1024;

/// Maximum number of TDs processed via a QH element list per frame.
const MAX_QH_ELEMENT_STEPS: usize = 1024;

const PID_IN: u8 = 0x69;
const PID_OUT: u8 = 0xe1;
const PID_SETUP: u8 = 0x2d;

const LINK_PTR_TERMINATE: u32 = 1 << 0;
const LINK_PTR_QH: u32 = 1 << 1;

const TD_STATUS_ACTIVE: u32 = 1 << 23;
const TD_STATUS_STALLED: u32 = 1 << 22;
const TD_STATUS_DATA_BUFFER_ERROR: u32 = 1 << 21;
const TD_STATUS_NAK: u32 = 1 << 19;
const TD_STATUS_CRC_TIMEOUT: u32 = 1 << 18;

const TD_CTRL_IOC: u32 = 1 << 24;
const TD_CTRL_SPD: u32 = 1 << 29;

#[derive(Clone, Copy, Debug)]
struct LinkPointer(u32);

impl LinkPointer {
    fn terminated(self) -> bool {
        self.0 & LINK_PTR_TERMINATE != 0
    }

    fn is_qh(self) -> bool {
        self.0 & LINK_PTR_QH != 0
    }

    fn addr(self) -> u32 {
        self.0 & 0xffff_fff0
    }
}

pub(crate) struct ScheduleContext<'a, M: MemoryBus + ?Sized> {
    pub mem: &'a mut M,
    pub hub: &'a mut RootHub,
    pub usbsts: &'a mut u16,
    pub usbint_causes: &'a mut u16,
}

pub(crate) fn process_frame<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    flbaseadd: u32,
    frame_index: u16,
) {
    let entry_off = (frame_index as u32).saturating_mul(4);
    let Some(entry_addr) = flbaseadd.checked_add(entry_off) else {
        // Frame list indexing overflowed the 32-bit physical address space. Treat this as a host
        // system error rather than allowing wraparound to alias low memory.
        *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
        return;
    };
    let link = LinkPointer(ctx.mem.read_u32(u64::from(entry_addr)));
    walk_link(ctx, link);
}

fn walk_link<M: MemoryBus + ?Sized>(ctx: &mut ScheduleContext<'_, M>, mut link: LinkPointer) {
    let mut visited = VisitedSet::new(MAX_SCHEDULE_LINKS_PER_FRAME);
    for _ in 0..MAX_SCHEDULE_LINKS_PER_FRAME {
        if link.terminated() {
            return;
        }

        let addr = link.addr();
        if addr == 0 {
            // Treat null pointers as terminated. This prevents hangs when the guest programs an
            // uninitialized frame list (all zeros).
            return;
        }

        if visited.insert(addr) {
            // Cycle detected in guest schedule memory.
            *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
            return;
        }

        link = if link.is_qh() {
            process_qh(ctx, addr)
        } else {
            process_td_chain(ctx, addr)
        };
    }

    // Schedule traversal budget exceeded.
    // If the schedule naturally terminated exactly at the budget boundary, treat that as success;
    // otherwise surface a schedule fault so `tick_1ms()` stays bounded.
    //
    // Note that we treat `addr=0` as termination throughout schedule walking to avoid hangs when the
    // guest programs an uninitialized frame list (all zeros). Preserve that behavior here too.
    if !link.terminated() && link.addr() != 0 {
        *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;

    struct TestMem {
        data: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self {
                data: vec![0; size],
            }
        }

        fn write_u32(&mut self, addr: u64, value: u32) {
            let addr = addr as usize;
            self.data[addr..addr + 4].copy_from_slice(&value.to_le_bytes());
        }
    }

    impl MemoryBus for TestMem {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = paddr as usize;
            let end = start.saturating_add(buf.len());
            if end > self.data.len() {
                buf.fill(0);
                return;
            }
            buf.copy_from_slice(&self.data[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = paddr as usize;
            let end = start.saturating_add(buf.len());
            if end > self.data.len() {
                return;
            }
            self.data[start..end].copy_from_slice(buf);
        }
    }

    struct PanicMem;

    impl MemoryBus for PanicMem {
        fn read_physical(&mut self, paddr: u64, _buf: &mut [u8]) {
            panic!("unexpected DMA read at {paddr:#x}");
        }

        fn write_physical(&mut self, paddr: u64, _buf: &[u8]) {
            panic!("unexpected DMA write at {paddr:#x}");
        }
    }

    #[test]
    fn uhci_schedule_walk_terminates_on_self_referential_qh() {
        // Frame list entry points at a QH that links to itself (horizontal), with no element chain.
        let mut mem = TestMem::new(0x2000);
        let mut hub = RootHub::new();
        let mut usbsts = 0u16;
        let mut usbint_causes = 0u16;
        let mut ctx = ScheduleContext {
            mem: &mut mem,
            hub: &mut hub,
            usbsts: &mut usbsts,
            usbint_causes: &mut usbint_causes,
        };

        let flbaseadd = 0u32;
        let qh_addr = 0x100u32;
        let qh_link = qh_addr | LINK_PTR_QH;

        // Frame list entry 0 -> QH.
        ctx.mem.write_u32(flbaseadd as u64, qh_link);
        // QH horizontal pointer -> itself.
        ctx.mem.write_u32(qh_addr as u64, qh_link);
        // QH element pointer -> terminate.
        ctx.mem.write_u32(qh_addr as u64 + 4, LINK_PTR_TERMINATE);

        // Should return (bounded traversal); without a cycle guard this would spin forever.
        process_frame(&mut ctx, flbaseadd, 0);

        // Cycle detection should flag a host system error / USB error interrupt.
        assert_ne!(usbsts & USBSTS_USBERRINT, 0);
        assert_ne!(usbsts & USBSTS_HSE, 0);
    }

    #[test]
    fn uhci_frame_list_pointer_add_overflow_sets_hse_without_dma() {
        // `process_frame` takes the (already-masked) FLBASEADD register value, but it is a
        // guest-controlled pointer. Ensure we never allow 32-bit arithmetic to wrap and alias low
        // memory.
        let mut mem = PanicMem;
        let mut hub = RootHub::new();
        let mut usbsts = 0u16;
        let mut usbint_causes = 0u16;
        let mut ctx = ScheduleContext {
            mem: &mut mem,
            hub: &mut hub,
            usbsts: &mut usbsts,
            usbint_causes: &mut usbint_causes,
        };

        // Force an overflow in `flbaseadd + frame_index*4`.
        process_frame(&mut ctx, 0xffff_fffc, 1);

        assert_ne!(usbsts & USBSTS_USBERRINT, 0);
        assert_ne!(usbsts & USBSTS_HSE, 0);
    }
}

fn process_qh<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    qh_addr: u32,
) -> LinkPointer {
    let horiz = LinkPointer(ctx.mem.read_u32(qh_addr as u64));
    let mut elem = LinkPointer(ctx.mem.read_u32(u64::from(qh_addr) + 4));

    let mut visited = VisitedSet::new(MAX_QH_ELEMENT_STEPS);
    let mut budget_exhausted = true;
    for _ in 0..MAX_QH_ELEMENT_STEPS {
        if elem.terminated() || elem.is_qh() {
            budget_exhausted = false;
            break;
        }

        let td_addr = elem.addr();
        if td_addr == 0 {
            // Treat null element pointers as terminated to avoid walking uninitialized schedules.
            budget_exhausted = false;
            break;
        }
        if visited.insert(td_addr) {
            *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
            budget_exhausted = false;
            break;
        }

        match process_single_td(ctx, td_addr) {
            TdProgress::NoProgress => {
                budget_exhausted = false;
                break;
            }
            TdProgress::Advanced { next_link, stop } => {
                ctx.mem.write_u32(u64::from(qh_addr) + 4, next_link);
                elem = LinkPointer(next_link);
                if stop {
                    budget_exhausted = false;
                    break;
                }
            }
            TdProgress::Nak => {
                budget_exhausted = false;
                break;
            }
        }
    }
    if budget_exhausted && !elem.terminated() && !elem.is_qh() && elem.addr() != 0 {
        *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
    }

    horiz
}

fn process_td_chain<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    mut td_addr: u32,
) -> LinkPointer {
    // The stop/skip path has its own loop bound, so this chain can touch up to
    // `2 * MAX_TD_CHAIN_STEPS` TDs in the worst case.
    let mut visited = VisitedSet::new(MAX_TD_CHAIN_STEPS.saturating_mul(2));
    let mut steps = 0usize;

    loop {
        if td_addr == 0 {
            // Treat null pointers as terminated.
            return LinkPointer(LINK_PTR_TERMINATE);
        }
        if steps >= MAX_TD_CHAIN_STEPS {
            *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
            return LinkPointer(LINK_PTR_TERMINATE);
        }
        steps += 1;
        if visited.insert(td_addr) {
            *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
            return LinkPointer(LINK_PTR_TERMINATE);
        }

        let link = LinkPointer(ctx.mem.read_u32(td_addr as u64));
        match process_single_td(ctx, td_addr) {
            TdProgress::NoProgress => return link,
            TdProgress::Nak => return link,
            TdProgress::Advanced { stop, .. } => {
                if stop {
                    // Stop further TD processing within this chain for the current frame, but still
                    // continue walking the schedule at the first non-TD link (QH/terminate).
                    let mut skip = link;
                    let mut skip_steps = 0usize;
                    while !skip.terminated() && !skip.is_qh() {
                        let addr = skip.addr();
                        if addr == 0 {
                            return LinkPointer(LINK_PTR_TERMINATE);
                        }
                        if skip_steps >= MAX_TD_CHAIN_STEPS {
                            *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
                            return LinkPointer(LINK_PTR_TERMINATE);
                        }
                        skip_steps += 1;
                        if visited.insert(addr) {
                            *ctx.usbsts |= USBSTS_USBERRINT | USBSTS_HSE;
                            return LinkPointer(LINK_PTR_TERMINATE);
                        }
                        skip = LinkPointer(ctx.mem.read_u32(addr as u64));
                    }
                    return skip;
                }

                if link.terminated() || link.is_qh() {
                    return link;
                }
                td_addr = link.addr();
            }
        }
    }
}

#[derive(Debug)]
enum TdProgress {
    NoProgress,
    Advanced { next_link: u32, stop: bool },
    Nak,
}

fn process_single_td<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    td_addr: u32,
) -> TdProgress {
    let next_link = ctx.mem.read_u32(td_addr as u64);

    let mut status = ctx.mem.read_u32(u64::from(td_addr) + 4);
    let token = ctx.mem.read_u32(u64::from(td_addr) + 8);
    let buffer = ctx.mem.read_u32(u64::from(td_addr) + 12);

    if status & TD_STATUS_ACTIVE == 0 {
        return TdProgress::NoProgress;
    }

    status &=
        !(TD_STATUS_STALLED | TD_STATUS_DATA_BUFFER_ERROR | TD_STATUS_NAK | TD_STATUS_CRC_TIMEOUT);

    let pid = (token & 0xff) as u8;
    let dev_addr = ((token >> 8) & 0x7f) as u8;
    let endpoint = ((token >> 15) & 0x0f) as u8;

    let max_len_field = ((token >> 21) & 0x7ff) as u16;
    let max_len = if max_len_field == 0x7ff {
        0usize
    } else {
        max_len_field as usize + 1
    };

    let Some(mut dev) = ctx.hub.device_mut_for_address(dev_addr) else {
        status |= TD_STATUS_CRC_TIMEOUT;
        complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
        if status & TD_CTRL_IOC != 0 {
            *ctx.usbsts |= USBSTS_USBINT;
            *ctx.usbint_causes |= USBINT_CAUSE_IOC;
        }
        return TdProgress::Advanced {
            next_link,
            stop: true,
        };
    };

    match pid {
        PID_SETUP => {
            if max_len != 8 {
                status |= TD_STATUS_DATA_BUFFER_ERROR;
                complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                if status & TD_CTRL_IOC != 0 {
                    *ctx.usbsts |= USBSTS_USBINT;
                    *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                }
                return TdProgress::Advanced {
                    next_link,
                    stop: true,
                };
            }
            let mut bytes = [0u8; 8];
            ctx.mem.read_physical(buffer as u64, &mut bytes);
            let setup = SetupPacket::from_bytes(bytes);
            match dev.handle_setup(setup) {
                UsbOutResult::Ack => {
                    complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 8, false);
                    if status & TD_CTRL_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                        *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: false,
                    }
                }
                UsbOutResult::Nak => {
                    status |= TD_STATUS_NAK;
                    ctx.mem
                        .write_u32(u64::from(td_addr) + 4, status | TD_STATUS_ACTIVE);
                    TdProgress::Nak
                }
                UsbOutResult::Stall => {
                    status |= TD_STATUS_STALLED;
                    complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                    if status & TD_CTRL_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                        *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: true,
                    }
                }
                UsbOutResult::Timeout => {
                    status |= TD_STATUS_CRC_TIMEOUT;
                    complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                    if status & TD_CTRL_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                        *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: true,
                    }
                }
            }
        }
        PID_OUT => {
            let mut out_data = vec![0u8; max_len];
            if max_len != 0 {
                ctx.mem.read_physical(buffer as u64, &mut out_data);
            }
            match dev.handle_out(endpoint, &out_data) {
                UsbOutResult::Ack => {
                    complete_td(
                        &mut *ctx.mem,
                        &mut *ctx.usbsts,
                        td_addr,
                        status,
                        out_data.len(),
                        false,
                    );
                    if status & TD_CTRL_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                        *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: false,
                    }
                }
                UsbOutResult::Nak => {
                    status |= TD_STATUS_NAK;
                    ctx.mem
                        .write_u32(u64::from(td_addr) + 4, status | TD_STATUS_ACTIVE);
                    TdProgress::Nak
                }
                UsbOutResult::Stall => {
                    status |= TD_STATUS_STALLED;
                    complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                    if status & TD_CTRL_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                        *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: true,
                    }
                }
                UsbOutResult::Timeout => {
                    status |= TD_STATUS_CRC_TIMEOUT;
                    complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                    if status & TD_CTRL_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                        *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: true,
                    }
                }
            }
        }
        PID_IN => match dev.handle_in(endpoint, max_len) {
            UsbInResult::Data(mut data) => {
                if data.len() > max_len {
                    data.truncate(max_len);
                }
                if !data.is_empty() {
                    ctx.mem.write_physical(buffer as u64, &data);
                }
                let short = data.len() < max_len;
                complete_td(
                    &mut *ctx.mem,
                    &mut *ctx.usbsts,
                    td_addr,
                    status,
                    data.len(),
                    false,
                );
                if status & TD_CTRL_IOC != 0 {
                    *ctx.usbsts |= USBSTS_USBINT;
                    *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                }
                let stop = short && status & TD_CTRL_SPD != 0;
                if stop {
                    *ctx.usbsts |= USBSTS_USBINT;
                    *ctx.usbint_causes |= USBINT_CAUSE_SHORT_PACKET;
                }
                TdProgress::Advanced { next_link, stop }
            }
            UsbInResult::Nak => {
                status |= TD_STATUS_NAK;
                ctx.mem
                    .write_u32(u64::from(td_addr) + 4, status | TD_STATUS_ACTIVE);
                TdProgress::Nak
            }
            UsbInResult::Stall => {
                status |= TD_STATUS_STALLED;
                complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                if status & TD_CTRL_IOC != 0 {
                    *ctx.usbsts |= USBSTS_USBINT;
                    *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                }
                TdProgress::Advanced {
                    next_link,
                    stop: true,
                }
            }
            UsbInResult::Timeout => {
                status |= TD_STATUS_CRC_TIMEOUT;
                complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
                if status & TD_CTRL_IOC != 0 {
                    *ctx.usbsts |= USBSTS_USBINT;
                    *ctx.usbint_causes |= USBINT_CAUSE_IOC;
                }
                TdProgress::Advanced {
                    next_link,
                    stop: true,
                }
            }
        },
        _ => {
            status |= TD_STATUS_STALLED;
            complete_td(&mut *ctx.mem, &mut *ctx.usbsts, td_addr, status, 0, true);
            if status & TD_CTRL_IOC != 0 {
                *ctx.usbsts |= USBSTS_USBINT;
                *ctx.usbint_causes |= USBINT_CAUSE_IOC;
            }
            TdProgress::Advanced {
                next_link,
                stop: true,
            }
        }
    }
}

fn complete_td<M: MemoryBus + ?Sized>(
    mem: &mut M,
    usbsts: &mut u16,
    td_addr: u32,
    mut status: u32,
    actual_len: usize,
    error: bool,
) {
    status &= !TD_STATUS_ACTIVE;
    let al = if actual_len == 0 {
        0x7ffu32
    } else {
        (actual_len as u32).saturating_sub(1) & 0x7ff
    };
    status = (status & !0x7ff) | al;
    mem.write_u32(u64::from(td_addr) + 4, status);

    if error {
        *usbsts |= USBSTS_USBERRINT;
    }
}
