use memory::MemoryBus;

use crate::io::usb::core::{UsbInResult, UsbOutResult};
use crate::io::usb::hub::RootHub;
use crate::io::usb::SetupPacket;

use super::regs::{USBINTR_IOC, USBINTR_SHORT_PACKET, USBSTS_USBERRINT, USBSTS_USBINT};

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
    pub usbintr: u16,
}

pub(crate) fn process_frame<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    flbaseadd: u32,
    frame_index: u16,
) {
    let entry_addr = flbaseadd.wrapping_add(frame_index as u32 * 4) as u64;
    let link = LinkPointer(ctx.mem.read_u32(entry_addr));
    walk_link(ctx, link, 0);
}

fn walk_link<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    mut link: LinkPointer,
    depth: u32,
) {
    if depth > 4096 {
        return;
    }

    while !link.terminated() {
        if link.is_qh() {
            link = process_qh(ctx, link.addr());
        } else {
            link = process_td_chain(ctx, link.addr(), 0);
        }
    }
}

fn process_qh<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    qh_addr: u32,
) -> LinkPointer {
    let horiz = LinkPointer(ctx.mem.read_u32(qh_addr as u64));
    let mut elem = LinkPointer(ctx.mem.read_u32(qh_addr.wrapping_add(4) as u64));

    let mut iterations = 0;
    while !elem.terminated() && !elem.is_qh() {
        if iterations > 1024 {
            break;
        }
        iterations += 1;

        let td_addr = elem.addr();
        match process_single_td(ctx, td_addr) {
            TdProgress::NoProgress => break,
            TdProgress::Advanced { next_link, stop } => {
                ctx.mem.write_u32(qh_addr.wrapping_add(4) as u64, next_link);
                elem = LinkPointer(next_link);
                if stop {
                    break;
                }
            }
            TdProgress::Nak => break,
        }
    }

    horiz
}

fn process_td_chain<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
    td_addr: u32,
    depth: u32,
) -> LinkPointer {
    if depth > 1024 {
        return LinkPointer(LINK_PTR_TERMINATE);
    }

    let link = LinkPointer(ctx.mem.read_u32(td_addr as u64));
    match process_single_td(ctx, td_addr) {
        TdProgress::NoProgress => link,
        TdProgress::Advanced { stop, .. } => {
            if stop {
                // Stop further TD processing within this chain for the current frame, but still
                // continue walking the schedule at the first non-TD link (QH/terminate).
                let mut skip = link;
                let mut skip_depth = depth + 1;
                while !skip.terminated() && !skip.is_qh() {
                    if skip_depth > 1024 {
                        return LinkPointer(LINK_PTR_TERMINATE);
                    }
                    skip = LinkPointer(ctx.mem.read_u32(skip.addr() as u64));
                    skip_depth += 1;
                }
                skip
            } else if link.terminated() || link.is_qh() {
                link
            } else {
                process_td_chain(ctx, link.addr(), depth + 1)
            }
        }
        TdProgress::Nak => link,
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

    let mut status = ctx.mem.read_u32(td_addr.wrapping_add(4) as u64);
    let token = ctx.mem.read_u32(td_addr.wrapping_add(8) as u64);
    let buffer = ctx.mem.read_u32(td_addr.wrapping_add(12) as u64);

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

    let Some(dev) = ctx.hub.device_mut_for_address(dev_addr) else {
        status |= TD_STATUS_CRC_TIMEOUT;
        complete_td(ctx, td_addr, status, 0, true);
        *ctx.usbsts |= USBSTS_USBERRINT;
        return TdProgress::Advanced {
            next_link,
            stop: true,
        };
    };

    match pid {
        PID_SETUP => {
            if max_len != 8 {
                status |= TD_STATUS_DATA_BUFFER_ERROR;
                complete_td(ctx, td_addr, status, 0, true);
                *ctx.usbsts |= USBSTS_USBERRINT;
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
                    complete_td(ctx, td_addr, status, 8, false);
                    if status & TD_CTRL_IOC != 0 && ctx.usbintr & USBINTR_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: false,
                    }
                }
                UsbOutResult::Nak => {
                    status |= TD_STATUS_NAK;
                    ctx.mem
                        .write_u32(td_addr.wrapping_add(4) as u64, status | TD_STATUS_ACTIVE);
                    TdProgress::Nak
                }
                UsbOutResult::Stall => {
                    status |= TD_STATUS_STALLED;
                    complete_td(ctx, td_addr, status, 0, true);
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
                    complete_td(ctx, td_addr, status, out_data.len(), false);
                    if status & TD_CTRL_IOC != 0 && ctx.usbintr & USBINTR_IOC != 0 {
                        *ctx.usbsts |= USBSTS_USBINT;
                    }
                    TdProgress::Advanced {
                        next_link,
                        stop: false,
                    }
                }
                UsbOutResult::Nak => {
                    status |= TD_STATUS_NAK;
                    ctx.mem
                        .write_u32(td_addr.wrapping_add(4) as u64, status | TD_STATUS_ACTIVE);
                    TdProgress::Nak
                }
                UsbOutResult::Stall => {
                    status |= TD_STATUS_STALLED;
                    complete_td(ctx, td_addr, status, 0, true);
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
                complete_td(ctx, td_addr, status, data.len(), false);
                if status & TD_CTRL_IOC != 0 && ctx.usbintr & USBINTR_IOC != 0 {
                    *ctx.usbsts |= USBSTS_USBINT;
                }
                let stop = short && status & TD_CTRL_SPD != 0;
                if stop && ctx.usbintr & USBINTR_SHORT_PACKET != 0 {
                    *ctx.usbsts |= USBSTS_USBINT;
                }
                TdProgress::Advanced {
                    next_link,
                    stop,
                }
            }
            UsbInResult::Nak => {
                status |= TD_STATUS_NAK;
                ctx.mem
                    .write_u32(td_addr.wrapping_add(4) as u64, status | TD_STATUS_ACTIVE);
                TdProgress::Nak
            }
            UsbInResult::Stall => {
                status |= TD_STATUS_STALLED;
                complete_td(ctx, td_addr, status, 0, true);
                TdProgress::Advanced {
                    next_link,
                    stop: true,
                }
            }
        },
        _ => {
            status |= TD_STATUS_STALLED;
            complete_td(ctx, td_addr, status, 0, true);
            TdProgress::Advanced {
                next_link,
                stop: true,
            }
        }
    }
}

fn complete_td<M: MemoryBus + ?Sized>(
    ctx: &mut ScheduleContext<'_, M>,
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
    ctx.mem.write_u32(td_addr.wrapping_add(4) as u64, status);

    if error {
        *ctx.usbsts |= USBSTS_USBERRINT;
    }
}
