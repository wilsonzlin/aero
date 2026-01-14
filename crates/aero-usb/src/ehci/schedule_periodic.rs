use alloc::vec;

use crate::device::{UsbInResult, UsbOutResult};
use crate::memory::MemoryBus;
use crate::visited_set::VisitedSet;
use crate::SetupPacket;

use super::regs::{USBSTS_USBERRINT, USBSTS_USBINT};
use super::schedule::{addr_add, ScheduleError, MAX_PERIODIC_LINKS_PER_FRAME, MAX_QTD_STEPS_PER_QH};
use super::RootHub;

// -----------------------------------
// Link pointer helpers (EHCI 3.6)
// -----------------------------------

const LP_TERMINATE: u32 = 1 << 0;
const LP_TYPE_SHIFT: u32 = 1;
const LP_TYPE_MASK: u32 = 0b11 << LP_TYPE_SHIFT;

const LP_TYPE_ITD: u32 = 0b00;
const LP_TYPE_QH: u32 = 0b01;
const LP_TYPE_SITD: u32 = 0b10;
const LP_TYPE_FSTN: u32 = 0b11;

const LP_ADDR_MASK: u32 = 0xffff_ffe0;

#[derive(Clone, Copy, Debug)]
struct LinkPointer(u32);

impl LinkPointer {
    fn terminated(self) -> bool {
        (self.0 & LP_TERMINATE) != 0
    }

    fn link_type(self) -> u32 {
        (self.0 & LP_TYPE_MASK) >> LP_TYPE_SHIFT
    }

    fn addr(self) -> u32 {
        self.0 & LP_ADDR_MASK
    }
}

#[derive(Clone, Copy, Debug)]
struct QtdPointer(u32);

impl QtdPointer {
    fn terminated(self) -> bool {
        (self.0 & 1) != 0
    }

    fn addr(self) -> u32 {
        self.0 & LP_ADDR_MASK
    }
}

// -----------------------------------
// qTD token bits (EHCI 3.5)
// -----------------------------------

const QTD_STATUS_PING_STATE: u32 = 1 << 0;
const QTD_STATUS_SPLIT_STATE: u32 = 1 << 1;
const QTD_STATUS_MISSED_MICROFRAME: u32 = 1 << 2;
const QTD_STATUS_TRANSACTION_ERROR: u32 = 1 << 3;
const QTD_STATUS_BABBLE: u32 = 1 << 4;
const QTD_STATUS_DATA_BUFFER_ERROR: u32 = 1 << 5;
const QTD_STATUS_HALTED: u32 = 1 << 6;
const QTD_STATUS_ACTIVE: u32 = 1 << 7;

const QTD_TOKEN_PID_SHIFT: u32 = 8;
const QTD_TOKEN_PID_MASK: u32 = 0b11 << QTD_TOKEN_PID_SHIFT;

const QTD_TOKEN_IOC: u32 = 1 << 15;

const QTD_TOKEN_BYTES_SHIFT: u32 = 16;
const QTD_TOKEN_BYTES_MASK: u32 = 0x7fff << QTD_TOKEN_BYTES_SHIFT;

const PID_OUT: u32 = 0;
const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;

pub(crate) struct PeriodicScheduleContext<'a, M: MemoryBus + ?Sized> {
    pub mem: &'a mut M,
    pub hub: &'a mut RootHub,
    pub usbsts: &'a mut u32,
}

/// Walk the current periodic frame list entry.
pub(crate) fn process_periodic_frame<M: MemoryBus + ?Sized>(
    ctx: &mut PeriodicScheduleContext<'_, M>,
    periodiclistbase: u32,
    frindex: u32,
) -> Result<(), ScheduleError> {
    let frame = (frindex >> 3) & 0x3ff;
    let microframe = (frindex & 0x7) as u8;

    let entry_off = frame
        .checked_mul(4)
        .ok_or(ScheduleError::AddressOverflow)?;
    let entry_addr = addr_add(periodiclistbase, entry_off)?;
    let link = LinkPointer(ctx.mem.read_u32(entry_addr));
    walk_link(ctx, link, microframe)
}

fn walk_link<M: MemoryBus + ?Sized>(
    ctx: &mut PeriodicScheduleContext<'_, M>,
    mut link: LinkPointer,
    microframe: u8,
) -> Result<(), ScheduleError> {
    let mut visited = VisitedSet::new(MAX_PERIODIC_LINKS_PER_FRAME);
    for _ in 0..MAX_PERIODIC_LINKS_PER_FRAME {
        if link.terminated() {
            return Ok(());
        }

        let addr = link.addr();
        if addr == 0 {
            return Ok(());
        }

        if visited.insert(addr) {
            return Err(ScheduleError::PeriodicCycle);
        }

        match link.link_type() {
            LP_TYPE_QH => {
                link = process_qh(ctx, addr, microframe)?;
            }
            // Unsupported periodic descriptor types. We do not emulate their transfers yet, but we
            // still follow their forward link pointers so that interrupt QHs later in the list can
            // make progress (e.g. systems scheduling isochronous audio alongside HID polling).
            //
            // EHCI 3.6: iTD and siTD dword0 is the Next Link Pointer; FSTN dword0 is the Normal Path
            // Link Pointer.
            LP_TYPE_ITD | LP_TYPE_SITD | LP_TYPE_FSTN => {
                link = LinkPointer(ctx.mem.read_u32(addr as u64));
            }
            _ => return Ok(()),
        }
    }

    // If the link chain naturally terminated exactly at the budget boundary, treat that as success;
    // otherwise surface a schedule fault so `tick_1ms()` remains bounded.
    if link.terminated() || link.addr() == 0 {
        Ok(())
    } else {
        Err(ScheduleError::PeriodicBudgetExceeded)
    }
}

fn process_qh<M: MemoryBus + ?Sized>(
    ctx: &mut PeriodicScheduleContext<'_, M>,
    qh_addr: u32,
    microframe: u8,
) -> Result<LinkPointer, ScheduleError> {
    // Queue Head layout (32-bit addressing):
    // dword0: Horizontal Link Pointer
    // dword1: Endpoint Characteristics
    // dword2: Endpoint Capabilities
    // dword3: Current qTD Pointer
    // dword4: Next qTD Pointer
    // dword5: Alternate Next qTD Pointer
    // dword6..: qTD overlay area

    let horiz = LinkPointer(ctx.mem.read_u32(qh_addr as u64));
    let ep_char = ctx.mem.read_u32(addr_add(qh_addr, 0x04)?);
    let ep_caps = ctx.mem.read_u32(addr_add(qh_addr, 0x08)?);

    let dev_addr = (ep_char & 0x7f) as u8;
    let endpoint = ((ep_char >> 8) & 0x0f) as u8;

    // Optional: honor uFrame S-mask. If SMASK is 0, treat it as always runnable.
    let s_mask = (ep_caps & 0xff) as u8;
    if s_mask != 0 && (s_mask & (1u8 << microframe)) == 0 {
        return Ok(horiz);
    }

    let mut next = QtdPointer(ctx.mem.read_u32(addr_add(qh_addr, 0x10)?));
    let mut visited_qtd = VisitedSet::new(MAX_QTD_STEPS_PER_QH);
    for _ in 0..MAX_QTD_STEPS_PER_QH {
        if next.terminated() {
            return Ok(horiz);
        }

        let qtd_addr = next.addr();
        if qtd_addr == 0 {
            return Ok(horiz);
        }
        if visited_qtd.insert(qtd_addr) {
            return Err(ScheduleError::QtdCycle);
        }

        let next_ptr = ctx.mem.read_u32(qtd_addr as u64);

        match process_single_qtd(ctx, qtd_addr, dev_addr, endpoint) {
            QtdProgress::NoProgress => return Ok(horiz),
            QtdProgress::Nak => return Ok(horiz),
            QtdProgress::Advanced { stop } => {
                // Advance the QH overlay "Next qTD Pointer" so software sees forward progress.
                ctx.mem
                    .write_u32(addr_add(qh_addr, 0x10)?, next_ptr);
                next = QtdPointer(next_ptr);
                if stop {
                    return Ok(horiz);
                }
            }
        }
    }

    // If the qTD list naturally terminated exactly at the budget boundary, treat that as success.
    if next.terminated() || next.addr() == 0 {
        Ok(horiz)
    } else {
        Err(ScheduleError::QtdBudgetExceeded)
    }
}

#[derive(Debug)]
enum QtdProgress {
    NoProgress,
    Nak,
    Advanced { stop: bool },
}

fn process_single_qtd<M: MemoryBus + ?Sized>(
    ctx: &mut PeriodicScheduleContext<'_, M>,
    qtd_addr: u32,
    dev_addr: u8,
    endpoint: u8,
) -> QtdProgress {
    // qTD layout (32-bit addressing):
    // dword0: Next qTD Pointer
    // dword1: Alternate Next qTD Pointer
    // dword2: Token
    // dword3-7: Buffer Page Pointers 0-4
    let token_addr = u64::from(qtd_addr) + 0x08;
    let mut token = ctx.mem.read_u32(token_addr);

    if (token & QTD_STATUS_ACTIVE) == 0 {
        return QtdProgress::NoProgress;
    }

    // Clear sticky status bits we might set.
    token &= !(QTD_STATUS_HALTED
        | QTD_STATUS_DATA_BUFFER_ERROR
        | QTD_STATUS_BABBLE
        | QTD_STATUS_TRANSACTION_ERROR
        | QTD_STATUS_MISSED_MICROFRAME
        | QTD_STATUS_SPLIT_STATE
        | QTD_STATUS_PING_STATE);

    let pid = (token & QTD_TOKEN_PID_MASK) >> QTD_TOKEN_PID_SHIFT;
    let total_bytes = ((token & QTD_TOKEN_BYTES_MASK) >> QTD_TOKEN_BYTES_SHIFT) as usize;
    let ioc = (token & QTD_TOKEN_IOC) != 0;

    let Some(mut dev) = ctx.hub.device_mut_for_address(dev_addr) else {
        token |= QTD_STATUS_HALTED | QTD_STATUS_TRANSACTION_ERROR;
        token &= !QTD_STATUS_ACTIVE;
        ctx.mem.write_u32(token_addr, token);
        *ctx.usbsts |= USBSTS_USBERRINT;
        if ioc {
            *ctx.usbsts |= USBSTS_USBINT;
        }
        return QtdProgress::Advanced { stop: true };
    };

    match pid {
        PID_SETUP => {
            if total_bytes != 8 {
                token |= QTD_STATUS_HALTED | QTD_STATUS_DATA_BUFFER_ERROR;
                token &= !QTD_STATUS_ACTIVE;
                token &= !QTD_TOKEN_BYTES_MASK;
                ctx.mem.write_u32(token_addr, token);
                *ctx.usbsts |= USBSTS_USBERRINT;
                if ioc {
                    *ctx.usbsts |= USBSTS_USBINT;
                }
                return QtdProgress::Advanced { stop: true };
            }

            let mut setup_bytes = [0u8; 8];
            read_qtd_buffer(ctx.mem, qtd_addr, &mut setup_bytes);
            let setup = SetupPacket::from_bytes(setup_bytes);

            match dev.handle_setup(setup) {
                UsbOutResult::Ack => {
                    complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, 0, false);
                    if ioc {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    QtdProgress::Advanced { stop: false }
                }
                UsbOutResult::Nak => QtdProgress::Nak,
                UsbOutResult::Stall => {
                    complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, 8, true);
                    if ioc {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    QtdProgress::Advanced { stop: true }
                }
                UsbOutResult::Timeout => {
                    complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, 8, true);
                    if ioc {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    QtdProgress::Advanced { stop: true }
                }
            }
        }
        PID_OUT => {
            let mut out_data = vec![0u8; total_bytes];
            if total_bytes != 0 {
                read_qtd_buffer(ctx.mem, qtd_addr, &mut out_data);
            }

            match dev.handle_out(endpoint, &out_data) {
                UsbOutResult::Ack => {
                    complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, 0, false);
                    if ioc {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    QtdProgress::Advanced { stop: false }
                }
                UsbOutResult::Nak => QtdProgress::Nak,
                UsbOutResult::Stall => {
                    complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, total_bytes, true);
                    if ioc {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    QtdProgress::Advanced { stop: true }
                }
                UsbOutResult::Timeout => {
                    complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, total_bytes, true);
                    if ioc {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    QtdProgress::Advanced { stop: true }
                }
            }
        }
        PID_IN => match dev.handle_in(endpoint, total_bytes) {
            UsbInResult::Data(mut data) => {
                if data.len() > total_bytes {
                    data.truncate(total_bytes);
                }
                if !data.is_empty() {
                    write_qtd_buffer(ctx.mem, qtd_addr, &data);
                }
                let remaining = total_bytes.saturating_sub(data.len());
                complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, remaining, false);
                if ioc {
                    *ctx.usbsts |= USBSTS_USBINT;
                }
                QtdProgress::Advanced { stop: false }
            }
            UsbInResult::Nak => QtdProgress::Nak,
            UsbInResult::Stall => {
                complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, total_bytes, true);
                if ioc {
                    *ctx.usbsts |= USBSTS_USBINT;
                }
                QtdProgress::Advanced { stop: true }
            }
            UsbInResult::Timeout => {
                complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, total_bytes, true);
                if ioc {
                    *ctx.usbsts |= USBSTS_USBINT;
                }
                QtdProgress::Advanced { stop: true }
            }
        },
        _ => {
            // Reserved PID.
            complete_qtd(ctx.mem, ctx.usbsts, qtd_addr, token, total_bytes, true);
            if ioc {
                *ctx.usbsts |= USBSTS_USBINT;
            }
            QtdProgress::Advanced { stop: true }
        }
    }
}

fn complete_qtd<M: MemoryBus + ?Sized>(
    mem: &mut M,
    usbsts: &mut u32,
    qtd_addr: u32,
    mut token: u32,
    remaining: usize,
    error: bool,
) {
    let token_addr = u64::from(qtd_addr) + 0x08;
    token &= !QTD_STATUS_ACTIVE;
    token &= !QTD_TOKEN_BYTES_MASK;
    token |= ((remaining as u32) & 0x7fff) << QTD_TOKEN_BYTES_SHIFT;

    if error {
        token |= QTD_STATUS_HALTED | QTD_STATUS_TRANSACTION_ERROR;
        *usbsts |= USBSTS_USBERRINT;
    } else {
        // Successful completion clears all sticky error bits and halt.
        token &= !(QTD_STATUS_HALTED
            | QTD_STATUS_DATA_BUFFER_ERROR
            | QTD_STATUS_BABBLE
            | QTD_STATUS_TRANSACTION_ERROR
            | QTD_STATUS_MISSED_MICROFRAME
            | QTD_STATUS_SPLIT_STATE
            | QTD_STATUS_PING_STATE);
        // Preserve the Interrupt-on-Complete bit.
    }

    // Write updated token back to guest memory.
    mem.write_u32(token_addr, token);
}

fn read_qtd_buffer<M: MemoryBus + ?Sized>(mem: &mut M, qtd_addr: u32, out: &mut [u8]) {
    let mut ptrs = [0u32; 5];
    for (i, ptr) in ptrs.iter_mut().enumerate() {
        let off = 0x0c + (i as u32) * 4;
        *ptr = mem.read_u32(u64::from(qtd_addr) + u64::from(off));
    }

    let start_off = (ptrs[0] & 0xfff) as usize;
    let mut offset = 0usize;
    while offset < out.len() {
        let abs = start_off + offset;
        let page = abs / 4096;
        if page >= ptrs.len() {
            break;
        }
        let page_off = abs % 4096;
        let base = (ptrs[page] & 0xffff_f000) as u64;
        let chunk_len = (4096 - page_off).min(out.len() - offset);
        mem.read_physical(base + page_off as u64, &mut out[offset..offset + chunk_len]);
        offset += chunk_len;
    }
}

fn write_qtd_buffer<M: MemoryBus + ?Sized>(mem: &mut M, qtd_addr: u32, data: &[u8]) {
    let mut ptrs = [0u32; 5];
    for (i, ptr) in ptrs.iter_mut().enumerate() {
        let off = 0x0c + (i as u32) * 4;
        *ptr = mem.read_u32(u64::from(qtd_addr) + u64::from(off));
    }

    let start_off = (ptrs[0] & 0xfff) as usize;
    let mut offset = 0usize;
    while offset < data.len() {
        let abs = start_off + offset;
        let page = abs / 4096;
        if page >= ptrs.len() {
            break;
        }
        let page_off = abs % 4096;
        let base = (ptrs[page] & 0xffff_f000) as u64;
        let chunk_len = (4096 - page_off).min(data.len() - offset);
        mem.write_physical(base + page_off as u64, &data[offset..offset + chunk_len]);
        offset += chunk_len;
    }
}
