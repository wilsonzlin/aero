use alloc::vec;
use alloc::vec::Vec;

use crate::device::{UsbInResult, UsbOutResult};
use crate::memory::MemoryBus;
use crate::visited_set::VisitedSet;
use crate::SetupPacket;

use super::regs::{USBSTS_USBERRINT, USBSTS_USBINT};
use super::schedule::{addr_add, ScheduleError, MAX_ASYNC_QH_VISITS, MAX_QTD_STEPS_PER_QH};
use super::RootHub;

// -----------------------------------------------------------------------------
// Link pointers
// -----------------------------------------------------------------------------

const LINK_TERMINATE: u32 = 1 << 0;

/// Horizontal schedule element type, encoded in EHCI link pointers.
///
/// EHCI 1.0 spec, section 3.6.1.
const LINK_TYPE_SHIFT: u32 = 1;
const LINK_TYPE_MASK: u32 = 0b11 << LINK_TYPE_SHIFT;
const LINK_TYPE_QH: u32 = 0b01 << LINK_TYPE_SHIFT;

/// EHCI schedule elements are 32-byte aligned (lower 5 bits are reserved).
const LINK_ADDR_MASK: u32 = 0xffff_ffe0;

#[derive(Clone, Copy, Debug)]
struct HorizLink(u32);

impl HorizLink {
    fn terminated(self) -> bool {
        self.0 & LINK_TERMINATE != 0
    }

    fn is_qh(self) -> bool {
        (self.0 & LINK_TYPE_MASK) == LINK_TYPE_QH
    }

    fn addr(self) -> u32 {
        self.0 & LINK_ADDR_MASK
    }
}

#[derive(Clone, Copy, Debug)]
struct QtdLink(u32);

impl QtdLink {
    fn terminated(self) -> bool {
        self.0 & LINK_TERMINATE != 0
    }

    fn addr(self) -> u32 {
        self.0 & LINK_ADDR_MASK
    }
}

// -----------------------------------------------------------------------------
// QH / qTD definitions (subset)
// -----------------------------------------------------------------------------

const QH_HORIZ: u32 = 0x00;
const QH_EPCHAR: u32 = 0x04;
const QH_CUR_QTD: u32 = 0x0c;
const QH_NEXT_QTD: u32 = 0x10;
const QH_ALT_NEXT_QTD: u32 = 0x14;
const QH_TOKEN: u32 = 0x18;
const QH_BUF0: u32 = 0x1c; // 5 buffer pointers (0x1c..=0x2c)

const QTD_NEXT: u32 = 0x00;
const QTD_ALT_NEXT: u32 = 0x04;
const QTD_TOKEN: u32 = 0x08;
const QTD_BUF0: u32 = 0x0c; // 5 buffer pointers

// qTD token bits.
const QTD_STS_ACTIVE: u32 = 1 << 7;
const QTD_STS_HALT: u32 = 1 << 6;
const QTD_STS_BUFERR: u32 = 1 << 5;
const QTD_STS_BABBLE: u32 = 1 << 4;
const QTD_STS_XACTERR: u32 = 1 << 3;
const QTD_STS_MMF: u32 = 1 << 2;

const QTD_PID_SHIFT: u32 = 8;
const QTD_PID_MASK: u32 = 0b11 << QTD_PID_SHIFT;
const QTD_PID_OUT: u32 = 0b00 << QTD_PID_SHIFT;
const QTD_PID_IN: u32 = 0b01 << QTD_PID_SHIFT;
const QTD_PID_SETUP: u32 = 0b10 << QTD_PID_SHIFT;

const QTD_CPAGE_SHIFT: u32 = 12;
const QTD_CPAGE_MASK: u32 = 0b111 << QTD_CPAGE_SHIFT;

const QTD_IOC: u32 = 1 << 15;

const QTD_TOTAL_BYTES_SHIFT: u32 = 16;
const QTD_TOTAL_BYTES_MASK: u32 = 0x7fff << QTD_TOTAL_BYTES_SHIFT;

const QTD_ERROR_MASK: u32 =
    QTD_STS_HALT | QTD_STS_BUFERR | QTD_STS_BABBLE | QTD_STS_XACTERR | QTD_STS_MMF;

// -----------------------------------------------------------------------------
// Context / entrypoints
// -----------------------------------------------------------------------------

pub(crate) struct AsyncScheduleContext<'a, M: MemoryBus + ?Sized> {
    pub mem: &'a mut M,
    pub hub: &'a mut RootHub,
    pub usbsts: &'a mut u32,
}

/// Walk and execute the EHCI asynchronous schedule.
///
/// This is a bounded walker: it will stop after a fixed number of QHs / qTDs even if guest memory
/// contains loops or otherwise malformed pointers.
pub(crate) fn process_async_schedule<M: MemoryBus + ?Sized>(
    ctx: &mut AsyncScheduleContext<'_, M>,
    asynclistaddr: u32,
) -> Result<(), ScheduleError> {
    let head = asynclistaddr & LINK_ADDR_MASK;
    if head == 0 {
        return Ok(());
    }

    // The async schedule list is a circular list of QHs. Stop when we return to the head, but also
    // cap iterations to avoid infinite loops if the guest corrupts pointers.
    let mut visited_qh = VisitedSet::new(MAX_ASYNC_QH_VISITS);
    let mut qh_addr = head;
    for _ in 0..MAX_ASYNC_QH_VISITS {
        if visited_qh.insert(qh_addr) {
            return Err(ScheduleError::AsyncQhCycle);
        }

        process_qh(ctx, qh_addr)?;

        let horiz_addr = addr_add(qh_addr, QH_HORIZ)?;
        let horiz = HorizLink(ctx.mem.read_u32(horiz_addr));
        if horiz.terminated() || !horiz.is_qh() {
            return Ok(());
        }
        let next = horiz.addr();
        if next == 0 {
            return Ok(());
        }
        if next == head {
            return Ok(());
        }
        if next == qh_addr || visited_qh.contains(next) {
            return Err(ScheduleError::AsyncQhCycle);
        }
        qh_addr = next;
    }

    // If we consumed the entire budget without naturally terminating, treat that as a schedule
    // traversal fault so `tick_1ms()` remains bounded and the guest can observe an error.
    Err(ScheduleError::AsyncQhBudgetExceeded)
}

// -----------------------------------------------------------------------------
// qTD buffer cursor
// -----------------------------------------------------------------------------

#[derive(Clone, Debug)]
struct QtdCursor {
    page: usize,
    offset: usize,
    bufs: [u32; 5],
}

#[derive(Debug, Clone, Copy)]
enum BufferFault {
    /// qTD buffer references more than the 5 pages representable by EHCI qTDs.
    OutOfPages,
}

impl QtdCursor {
    fn from_token_and_bufs(token: u32, bufs: [u32; 5]) -> Self {
        let page = ((token & QTD_CPAGE_MASK) >> QTD_CPAGE_SHIFT) as usize;
        // CPAGE is only valid for values 0..=4. Preserve invalid values so we can surface a
        // deterministic buffer fault instead of silently clamping to an arbitrary page.
        let offset = if page < 5 {
            (bufs[page] & 0x0fff) as usize
        } else {
            0
        };
        Self { page, offset, bufs }
    }

    fn current_paddr(&self) -> Result<u64, BufferFault> {
        if self.page >= 5 {
            return Err(BufferFault::OutOfPages);
        }
        let base = (self.bufs[self.page] & 0xffff_f000) as u64;
        Ok(base + (self.offset as u64))
    }

    fn advance(&mut self, mut len: usize) -> Result<(), BufferFault> {
        while len > 0 {
            if self.page >= 5 {
                return Err(BufferFault::OutOfPages);
            }
            let page_remaining = 4096usize.saturating_sub(self.offset);
            let step = len.min(page_remaining);
            self.offset += step;
            len -= step;
            if self.offset >= 4096 {
                self.page += 1;
                self.offset = 0;
            }
        }
        Ok(())
    }

    fn read_bytes<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        len: usize,
    ) -> Result<Vec<u8>, BufferFault> {
        let mut out = vec![0u8; len];
        let mut written = 0usize;
        while written < len {
            let paddr = self.current_paddr()?;
            let page_remaining = 4096usize.saturating_sub(self.offset);
            let chunk_len = (len - written).min(page_remaining);
            mem.read_physical(paddr, &mut out[written..written + chunk_len]);
            self.advance(chunk_len)?;
            written += chunk_len;
        }
        Ok(out)
    }

    fn write_bytes<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        data: &[u8],
    ) -> Result<(), BufferFault> {
        let mut written = 0usize;
        while written < data.len() {
            let paddr = self.current_paddr()?;
            let page_remaining = 4096usize.saturating_sub(self.offset);
            let chunk_len = (data.len() - written).min(page_remaining);
            mem.write_physical(paddr, &data[written..written + chunk_len]);
            self.advance(chunk_len)?;
            written += chunk_len;
        }
        Ok(())
    }

    fn encode_into_overlay(&self, token: &mut u32, out_bufs: &mut [u32; 5]) {
        // CPAGE is only valid for values 0..=4.
        //
        // `QtdCursor::advance` allows `self.page == 5` as an internal "cursor is exactly at the end
        // of the final page" state (a valid completion point). Avoid writing that reserved value
        // back into the token field to keep guest-visible state spec-aligned.
        let cpage = self.page.min(4);
        *token = (*token & !QTD_CPAGE_MASK) | ((cpage as u32) << QTD_CPAGE_SHIFT);
        for (i, out_buf) in out_bufs.iter_mut().enumerate() {
            let base = self.bufs[i] & 0xffff_f000;
            *out_buf = if i == cpage {
                base | ((self.offset as u32) & 0x0fff)
            } else {
                base
            };
        }
    }
}

// -----------------------------------------------------------------------------
// QH processing
// -----------------------------------------------------------------------------

fn process_qh<M: MemoryBus + ?Sized>(
    ctx: &mut AsyncScheduleContext<'_, M>,
    qh_addr: u32,
) -> Result<(), ScheduleError> {
    let ep_char_addr = addr_add(qh_addr, QH_EPCHAR)?;
    let ep_char = ctx.mem.read_u32(ep_char_addr);

    let dev_addr = (ep_char & 0x7f) as u8;
    let endpoint = ((ep_char >> 8) & 0x0f) as u8;
    let speed = ((ep_char >> 12) & 0x03) as u8;
    let max_packet = ((ep_char >> 16) & 0x7ff) as usize;
    let max_packet = max_packet.max(1);

    // Only high-speed is currently supported (sufficient for EHCI + WebUSB passthrough). Other
    // speeds are treated as a controller-side failure: we complete the current qTD with HALT.
    const SPEED_HIGH: u8 = 2;
    if speed != SPEED_HIGH {
        complete_current_qtd_with_error(ctx, qh_addr, QTD_STS_HALT | QTD_STS_XACTERR, true)?;
        return Ok(());
    }

    // Resolve the device once per QH iteration.
    let Some(mut dev) = ctx.hub.device_mut_for_address(dev_addr) else {
        complete_current_qtd_with_error(ctx, qh_addr, QTD_STS_HALT | QTD_STS_XACTERR, true)?;
        return Ok(());
    };

    let mut visited_qtd = VisitedSet::new(MAX_QTD_STEPS_PER_QH);
    for _ in 0..MAX_QTD_STEPS_PER_QH {
        // If no current qTD is loaded, attempt to fetch the one pointed to by QH.Next qTD.
        //
        // Load and process the qTD within the same budget iteration so that the qTD step budget
        // corresponds to the number of qTDs observed (rather than counting an extra iteration just
        // to populate the QH overlay).
        let mut cur_qtd =
            ctx.mem.read_u32(addr_add(qh_addr, QH_CUR_QTD)?) & LINK_ADDR_MASK;
        if cur_qtd == 0 {
            let next = QtdLink(ctx.mem.read_u32(addr_add(qh_addr, QH_NEXT_QTD)?));
            if next.terminated() {
                return Ok(());
            }
            let addr = next.addr();
            if addr == 0 {
                return Ok(());
            }
            if visited_qtd.contains(addr) {
                return Err(ScheduleError::QtdCycle);
            }
            load_qtd_into_qh_overlay(ctx.mem, qh_addr, addr)?;
            cur_qtd = addr;
        }

        // Track visited qTDs so malicious guests cannot craft cyclic qTD lists that would otherwise
        // cause unbounded work.
        if visited_qtd.insert(cur_qtd) {
            return Err(ScheduleError::QtdCycle);
        }

        let mut token = ctx.mem.read_u32(addr_add(qh_addr, QH_TOKEN)?);

        // If the current qTD is inactive, either it is complete (and we can advance) or it has
        // halted/error status and we must stop.
        if token & QTD_STS_ACTIVE == 0 {
            if token & QTD_ERROR_MASK != 0 {
                return Ok(());
            }
            // Advance to the next qTD if present; otherwise clear QH.CUR_QTD to indicate idle.
            let next = QtdLink(ctx.mem.read_u32(addr_add(qh_addr, QH_NEXT_QTD)?));
            if next.terminated() {
                ctx.mem.write_u32(addr_add(qh_addr, QH_CUR_QTD)?, 0);
                return Ok(());
            }
            let addr = next.addr();
            if addr == 0 {
                ctx.mem.write_u32(addr_add(qh_addr, QH_CUR_QTD)?, 0);
                return Ok(());
            }
            if visited_qtd.contains(addr) {
                return Err(ScheduleError::QtdCycle);
            }
            load_qtd_into_qh_overlay(ctx.mem, qh_addr, addr)?;
            continue;
        }

        let mut overlay_bufs = [0u32; 5];
        for (i, buf) in overlay_bufs.iter_mut().enumerate() {
            *buf = ctx
                .mem
                .read_u32(addr_add(qh_addr, QH_BUF0 + i as u32 * 4)?);
        }
        let mut cursor = QtdCursor::from_token_and_bufs(token, overlay_bufs);

        let pid = token & QTD_PID_MASK;
        let mut remaining = ((token & QTD_TOTAL_BYTES_MASK) >> QTD_TOTAL_BYTES_SHIFT) as usize;
        let ioc = token & QTD_IOC != 0;

        // Execute packets until the qTD completes or we observe NAK.
        let mut nak = false;
        let mut short_packet = false;
        let mut error_bits = 0u32;

        match pid {
            QTD_PID_SETUP => {
                // Setup stage must be exactly 8 bytes.
                if remaining != 8 {
                    error_bits = QTD_STS_HALT | QTD_STS_BUFERR;
                } else {
                    let mut pkt_cursor = cursor.clone();
                    let bytes = match pkt_cursor.read_bytes(ctx.mem, 8) {
                        Ok(b) => b,
                        Err(_) => {
                            error_bits = QTD_STS_HALT | QTD_STS_BUFERR;
                            Vec::new()
                        }
                    };
                    if error_bits == 0 {
                        let mut setup_bytes = [0u8; 8];
                        setup_bytes.copy_from_slice(&bytes[..8]);
                        let setup = SetupPacket::from_bytes(setup_bytes);
                        match dev.handle_setup(setup) {
                            UsbOutResult::Ack => {
                                cursor = pkt_cursor;
                                remaining = 0;
                            }
                            UsbOutResult::Nak => {
                                nak = true;
                            }
                            UsbOutResult::Stall => {
                                error_bits = QTD_STS_HALT;
                            }
                            UsbOutResult::Timeout => {
                                error_bits = QTD_STS_XACTERR;
                            }
                        }
                    }
                }
            }
            QTD_PID_OUT => {
                // A qTD with TotalBytes=0 represents a ZLP.
                if remaining == 0 {
                    match dev.handle_out(endpoint, &[]) {
                        UsbOutResult::Ack => {}
                        UsbOutResult::Nak => nak = true,
                        UsbOutResult::Stall => error_bits = QTD_STS_HALT,
                        UsbOutResult::Timeout => error_bits = QTD_STS_XACTERR,
                    }
                } else {
                    const MAX_PACKETS_PER_QTD: usize = 4096;
                    let mut packets = 0usize;
                    while remaining > 0 && packets < MAX_PACKETS_PER_QTD {
                        packets += 1;
                        let pkt_len = remaining.min(max_packet);
                        let mut pkt_cursor = cursor.clone();
                        let data = match pkt_cursor.read_bytes(ctx.mem, pkt_len) {
                            Ok(b) => b,
                            Err(_) => {
                                error_bits = QTD_STS_HALT | QTD_STS_BUFERR;
                                break;
                            }
                        };
                        match dev.handle_out(endpoint, &data) {
                            UsbOutResult::Ack => {
                                cursor = pkt_cursor;
                                remaining = remaining.saturating_sub(pkt_len);
                            }
                            UsbOutResult::Nak => {
                                // No further progress this tick. Do not commit the cursor advance
                                // for this (non-acked) packet.
                                nak = true;
                                break;
                            }
                            UsbOutResult::Stall => {
                                error_bits = QTD_STS_HALT;
                                break;
                            }
                            UsbOutResult::Timeout => {
                                error_bits = QTD_STS_XACTERR;
                                break;
                            }
                        }
                    }
                    // If we exhausted our per-qTD packet budget, yield and leave the qTD active so
                    // it can continue next tick.
                    if remaining > 0 && packets >= MAX_PACKETS_PER_QTD && error_bits == 0 && !nak {
                        nak = true;
                    }
                }
            }
            QTD_PID_IN => {
                // A qTD with TotalBytes=0 represents a ZLP.
                if remaining == 0 {
                    match dev.handle_in(endpoint, 0) {
                        UsbInResult::Data(data) => {
                            if !data.is_empty() {
                                error_bits = QTD_STS_HALT | QTD_STS_BUFERR;
                            }
                        }
                        UsbInResult::Nak => nak = true,
                        UsbInResult::Stall => error_bits = QTD_STS_HALT,
                        UsbInResult::Timeout => error_bits = QTD_STS_XACTERR,
                    }
                } else {
                    const MAX_PACKETS_PER_QTD: usize = 4096;
                    let mut packets = 0usize;
                    while remaining > 0 && packets < MAX_PACKETS_PER_QTD {
                        packets += 1;
                        let pkt_len = remaining.min(max_packet);
                        match dev.handle_in(endpoint, pkt_len) {
                            UsbInResult::Data(data) => {
                                let actual = data.len().min(pkt_len);
                                if actual != 0
                                    && cursor.write_bytes(ctx.mem, &data[..actual]).is_err()
                                {
                                    error_bits = QTD_STS_HALT | QTD_STS_BUFERR;
                                    break;
                                }
                                remaining = remaining.saturating_sub(actual);
                                if actual < pkt_len {
                                    short_packet = true;
                                    break;
                                }
                            }
                            UsbInResult::Nak => {
                                nak = true;
                                break;
                            }
                            UsbInResult::Stall => {
                                error_bits = QTD_STS_HALT;
                                break;
                            }
                            UsbInResult::Timeout => {
                                error_bits = QTD_STS_XACTERR;
                                break;
                            }
                        }
                    }
                    // If we exhausted our per-qTD packet budget without encountering a device NAK,
                    // yield and leave the qTD active so it can continue next tick.
                    if remaining > 0
                        && packets >= MAX_PACKETS_PER_QTD
                        && error_bits == 0
                        && !nak
                        && !short_packet
                    {
                        nak = true;
                    }
                }
            }
            _ => {
                error_bits = QTD_STS_HALT | QTD_STS_XACTERR;
            }
        }

        // Update TotalBytes (remaining) and current page in both the QH overlay and the qTD token.
        token = (token & !QTD_TOTAL_BYTES_MASK) | ((remaining as u32) << QTD_TOTAL_BYTES_SHIFT);

        let mut new_bufs = cursor.bufs;
        cursor.encode_into_overlay(&mut token, &mut new_bufs);
        for (i, buf) in new_bufs.iter().enumerate() {
            ctx.mem
                .write_u32(addr_add(qh_addr, QH_BUF0 + i as u32 * 4)?, *buf);
        }

        // Apply NAK / completion / error handling.
        if error_bits != 0 {
            token &= !QTD_STS_ACTIVE;
            token |= error_bits;
            // Any error should surface USBERRINT so guests observe an interrupt even if IOC is not
            // set on the qTD.
            *ctx.usbsts |= USBSTS_USBERRINT;
            if ioc {
                *ctx.usbsts |= USBSTS_USBINT;
            }
            write_back_current_qtd_token(ctx.mem, qh_addr, cur_qtd, token)?;
            // Halted/error qTDs stop queue processing.
            return Ok(());
        }

        if nak {
            // Keep the qTD active (so the guest retries). The cursor and TotalBytes have already
            // been updated for any progress that occurred before the NAK.
            write_back_qh_overlay_token(ctx.mem, qh_addr, token)?;
            return Ok(());
        }

        // Completed (either because remaining==0 or because of a short packet).
        token &= !QTD_STS_ACTIVE;
        write_back_current_qtd_token(ctx.mem, qh_addr, cur_qtd, token)?;
        if ioc {
            *ctx.usbsts |= USBSTS_USBINT;
        }

        // Choose the next qTD: on short packet completion prefer the alternate next pointer if it
        // is present, matching the EHCI early-termination semantics.
        let mut next_ptr = QtdLink(ctx.mem.read_u32(addr_add(qh_addr, QH_NEXT_QTD)?));
        if short_packet {
            let alt = QtdLink(ctx.mem.read_u32(addr_add(qh_addr, QH_ALT_NEXT_QTD)?));
            if !alt.terminated() {
                next_ptr = alt;
            }
        }

        if next_ptr.terminated() {
            // Queue is now empty.
            ctx.mem.write_u32(addr_add(qh_addr, QH_CUR_QTD)?, 0);
            return Ok(());
        }
        let addr = next_ptr.addr();
        if addr == 0 {
            ctx.mem.write_u32(addr_add(qh_addr, QH_CUR_QTD)?, 0);
            return Ok(());
        }
        if visited_qtd.contains(addr) {
            return Err(ScheduleError::QtdCycle);
        }
        load_qtd_into_qh_overlay(ctx.mem, qh_addr, addr)?;
        // Continue to process the next qTD in the same tick.
    }

    Err(ScheduleError::QtdBudgetExceeded)
}

fn load_qtd_into_qh_overlay<M: MemoryBus + ?Sized>(
    mem: &mut M,
    qh_addr: u32,
    qtd_addr: u32,
) -> Result<(), ScheduleError> {
    let qtd_addr = qtd_addr & LINK_ADDR_MASK;

    let next = mem.read_u32(addr_add(qtd_addr, QTD_NEXT)?);
    let alt = mem.read_u32(addr_add(qtd_addr, QTD_ALT_NEXT)?);
    let token = mem.read_u32(addr_add(qtd_addr, QTD_TOKEN)?);

    mem.write_u32(addr_add(qh_addr, QH_CUR_QTD)?, qtd_addr);
    mem.write_u32(addr_add(qh_addr, QH_NEXT_QTD)?, next);
    mem.write_u32(addr_add(qh_addr, QH_ALT_NEXT_QTD)?, alt);
    mem.write_u32(addr_add(qh_addr, QH_TOKEN)?, token);

    for i in 0..5 {
        let buf = mem.read_u32(addr_add(qtd_addr, QTD_BUF0 + i as u32 * 4)?);
        mem.write_u32(addr_add(qh_addr, QH_BUF0 + i as u32 * 4)?, buf);
    }
    Ok(())
}

fn write_back_current_qtd_token<M: MemoryBus + ?Sized>(
    mem: &mut M,
    qh_addr: u32,
    cur_qtd: u32,
    token: u32,
) -> Result<(), ScheduleError> {
    let cur_qtd = cur_qtd & LINK_ADDR_MASK;
    mem.write_u32(addr_add(cur_qtd, QTD_TOKEN)?, token);
    write_back_qh_overlay_token(mem, qh_addr, token)
}

fn write_back_qh_overlay_token<M: MemoryBus + ?Sized>(
    mem: &mut M,
    qh_addr: u32,
    token: u32,
) -> Result<(), ScheduleError> {
    mem.write_u32(addr_add(qh_addr, QH_TOKEN)?, token);
    Ok(())
}

fn complete_current_qtd_with_error<M: MemoryBus + ?Sized>(
    ctx: &mut AsyncScheduleContext<'_, M>,
    qh_addr: u32,
    error_bits: u32,
    force_usb_errint: bool,
) -> Result<(), ScheduleError> {
    // If there's no current qTD loaded, try to load one so the guest sees a completion instead of
    // an indefinite hang.
    let mut cur_qtd = ctx.mem.read_u32(addr_add(qh_addr, QH_CUR_QTD)?) & LINK_ADDR_MASK;
    if cur_qtd == 0 {
        let next = QtdLink(ctx.mem.read_u32(addr_add(qh_addr, QH_NEXT_QTD)?));
        if next.terminated() {
            return Ok(());
        }
        let addr = next.addr();
        if addr == 0 {
            return Ok(());
        }
        load_qtd_into_qh_overlay(ctx.mem, qh_addr, addr)?;
        cur_qtd = addr;
    }

    let mut token = ctx.mem.read_u32(addr_add(qh_addr, QH_TOKEN)?);
    let ioc = token & QTD_IOC != 0;
    token &= !QTD_STS_ACTIVE;
    token |= error_bits;
    write_back_current_qtd_token(ctx.mem, qh_addr, cur_qtd, token)?;

    if force_usb_errint {
        *ctx.usbsts |= USBSTS_USBERRINT;
    }
    if ioc {
        *ctx.usbsts |= USBSTS_USBINT;
    }
    Ok(())
}
