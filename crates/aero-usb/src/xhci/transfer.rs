use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::fmt;

use super::trb::TRB_LEN;
pub use super::trb::{CompletionCode, Trb, TrbType};

use super::trb::{Trb as XhciTrb, TrbType as XhciTrbType};
use crate::device::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use crate::memory::MemoryBus;
use crate::{SetupPacket, UsbDeviceModel};

const TRB_SIZE: u64 = TRB_LEN as u64;

/// Maximum number of TRBs we'll inspect while skipping link TRBs at the start of a tick.
///
/// This is purely a safety bound against malformed rings (self-referential links, etc).
const MAX_LINK_SKIP: usize = 32;

/// Maximum number of TRBs we'll consider part of a single TD (chain).
const MAX_TD_TRBS: usize = 64;
pub fn read_trb<M: MemoryBus + ?Sized>(mem: &mut M, paddr: u64) -> Trb {
    Trb::read_from(mem, paddr)
}

pub fn write_trb<M: MemoryBus + ?Sized>(mem: &mut M, paddr: u64, trb: Trb) {
    trb.write_to(mem, paddr);
}

fn read_trb_checked<M: MemoryBus + ?Sized>(mem: &mut M, paddr: u64) -> Result<Trb, ()> {
    let mut bytes = [0u8; TRB_LEN];
    mem.read_physical(paddr, &mut bytes);
    // Treat an all-ones fetch as an invalid DMA read (commonly produced by open-bus/unmapped reads).
    // This avoids "successfully" processing garbage TRBs when the guest misprograms ring pointers.
    if bytes.iter().all(|&b| b == 0xFF) {
        return Err(());
    }
    Ok(Trb::from_bytes(bytes))
}

/// A completion notification emitted by the transfer executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferEvent {
    pub ep_addr: u8,
    /// Pointer to the TRB that generated the event (typically the last TRB of the TD).
    pub trb_ptr: u64,
    /// If the TD terminates with an Event Data TRB, this holds the TRB's `parameter` payload.
    ///
    /// Real xHCI controllers set the Transfer Event TRB's ED bit and copy this value into the
    /// Transfer Event TRB parameter field (instead of a TRB pointer).
    pub event_data: Option<u64>,
    /// The number of bytes remaining (residual) in the TD.
    pub residual: u32,
    pub completion_code: CompletionCode,
}

#[derive(Debug, Clone)]
struct BufferSegment {
    paddr: u64,
    len: u32,
}

#[derive(Debug, Clone)]
struct TdDescriptor {
    buffers: Vec<BufferSegment>,
    total_len: u32,
    last_trb_ptr: u64,
    last_ioc: bool,
    event_data: Option<u64>,
    next_dequeue_ptr: u64,
    next_cycle: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GatherTdResult {
    Incomplete,
    Ready,
    Fault { trb_ptr: u64 },
}

#[derive(Debug, Clone)]
pub struct TransferRingState {
    pub dequeue_ptr: u64,
    pub cycle: bool,
}

impl TransferRingState {
    pub fn new(dequeue_ptr: u64) -> Self {
        Self {
            dequeue_ptr,
            cycle: true,
        }
    }

    pub fn new_with_cycle(dequeue_ptr: u64, cycle: bool) -> Self {
        Self { dequeue_ptr, cycle }
    }
}

#[derive(Debug, Clone)]
pub struct EndpointState {
    pub ep_addr: u8,
    pub ring: TransferRingState,
    pub halted: bool,
}

impl EndpointState {
    pub fn new(ep_addr: u8, dequeue_ptr: u64) -> Self {
        Self {
            ep_addr,
            ring: TransferRingState::new(dequeue_ptr),
            halted: false,
        }
    }

    pub fn new_with_cycle(ep_addr: u8, dequeue_ptr: u64, cycle: bool) -> Self {
        Self {
            ep_addr,
            ring: TransferRingState::new_with_cycle(dequeue_ptr, cycle),
            halted: false,
        }
    }

    fn direction_in(&self) -> bool {
        (self.ep_addr & 0x80) != 0
    }
}

/// Minimal xHCI transfer ring executor for a single connected device.
///
/// Higher level models are expected to provide slot/addressing/endpoint configuration; this
/// executor only needs a mapping from endpoint address to a transfer ring dequeue pointer.
pub struct XhciTransferExecutor {
    device: Box<dyn UsbDeviceModel>,
    endpoints: BTreeMap<u8, EndpointState>,
    pending_events: Vec<TransferEvent>,
}

impl fmt::Debug for XhciTransferExecutor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XhciTransferExecutor")
            .field("endpoints", &self.endpoints)
            .field("pending_events", &self.pending_events)
            .finish_non_exhaustive()
    }
}

impl XhciTransferExecutor {
    pub fn new(device: Box<dyn UsbDeviceModel>) -> Self {
        Self {
            device,
            endpoints: BTreeMap::new(),
            pending_events: Vec::new(),
        }
    }

    pub fn device_mut(&mut self) -> &mut dyn UsbDeviceModel {
        &mut *self.device
    }

    pub fn add_endpoint(&mut self, ep_addr: u8, dequeue_ptr: u64) {
        self.endpoints
            .insert(ep_addr, EndpointState::new(ep_addr, dequeue_ptr));
    }

    pub fn add_endpoint_with_cycle(&mut self, ep_addr: u8, dequeue_ptr: u64, cycle: bool) {
        self.endpoints.insert(
            ep_addr,
            EndpointState::new_with_cycle(ep_addr, dequeue_ptr, cycle),
        );
    }

    pub fn endpoint_state(&self, ep_addr: u8) -> Option<&EndpointState> {
        self.endpoints.get(&ep_addr)
    }

    pub fn endpoint_state_mut(&mut self, ep_addr: u8) -> Option<&mut EndpointState> {
        self.endpoints.get_mut(&ep_addr)
    }

    pub fn take_events(&mut self) -> Vec<TransferEvent> {
        core::mem::take(&mut self.pending_events)
    }

    /// Attempt to process at most one TD for a specific endpoint.
    ///
    /// This is useful for wiring into xHCI doorbell behavior where the guest explicitly notifies
    /// the controller that a particular endpoint has new work available.
    pub fn poll_endpoint<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, ep_addr: u8) {
        // See `tick_1ms`: move the endpoint map out to avoid holding a mutable borrow of
        // `self.endpoints` while calling helpers that need `&mut self`.
        let mut endpoints = core::mem::take(&mut self.endpoints);
        if let Some(ep) = endpoints.get_mut(&ep_addr) {
            if !ep.halted {
                self.process_one_td(mem, ep);
            }
        }
        self.endpoints = endpoints;
    }

    /// Clears the halted (stalled) state of an endpoint.
    pub fn reset_endpoint(&mut self, ep_addr: u8) {
        if let Some(ep) = self.endpoints.get_mut(&ep_addr) {
            ep.halted = false;
        }
    }

    pub fn tick_1ms<M: MemoryBus + ?Sized>(&mut self, mem: &mut M) {
        self.device.tick_1ms();

        // `BTreeMap::iter_mut()` holds a mutable borrow of `self.endpoints` for the duration of the
        // loop. Move the map out temporarily so we can still call helper methods that need `&mut
        // self` without fighting the borrow checker.
        //
        // This is allocation-free: it just swaps `self.endpoints` with an empty map.
        let mut endpoints = core::mem::take(&mut self.endpoints);
        for (_, ep) in endpoints.iter_mut() {
            if ep.halted {
                continue;
            }
            self.process_one_td(mem, ep);
        }
        self.endpoints = endpoints;
    }

    fn process_one_td<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, ep: &mut EndpointState) {
        // Advance past any link TRBs so the dequeue pointer naturally points at a transfer TRB (or
        // a not-yet-ready TRB).
        if !self.skip_link_trbs(mem, ep) {
            return;
        }

        let trb = match read_trb_checked(mem, ep.ring.dequeue_ptr) {
            Ok(trb) => trb,
            Err(()) => {
                ep.halted = true;
                self.pending_events.push(TransferEvent {
                    ep_addr: ep.ep_addr,
                    trb_ptr: ep.ring.dequeue_ptr,
                    event_data: None,
                    residual: 0,
                    completion_code: CompletionCode::TrbError,
                });
                return;
            }
        };
        if trb.cycle() != ep.ring.cycle {
            return;
        }

        match trb.trb_type() {
            TrbType::Normal => {
                let mut td = TdDescriptor {
                    buffers: Vec::new(),
                    total_len: 0,
                    last_trb_ptr: ep.ring.dequeue_ptr,
                    last_ioc: false,
                    event_data: None,
                    next_dequeue_ptr: ep.ring.dequeue_ptr,
                    next_cycle: ep.ring.cycle,
                };

                match self.gather_td(mem, ep, &mut td) {
                    GatherTdResult::Incomplete => (),
                    GatherTdResult::Ready => self.execute_td(mem, ep, td),
                    GatherTdResult::Fault { trb_ptr } => {
                        ep.halted = true;
                        self.pending_events.push(TransferEvent {
                            ep_addr: ep.ep_addr,
                            trb_ptr,
                            event_data: None,
                            residual: 0,
                            completion_code: CompletionCode::TrbError,
                        });
                    }
                }
            }
            TrbType::NoOp => {
                // A No-op Transfer TRB completes immediately without issuing any USB bus
                // transaction. Treat it as a zero-length TD and (optionally) generate a Transfer
                // Event if IOC is set.
                let trb_ptr = ep.ring.dequeue_ptr;
                let ioc = trb.ioc();

                match ep.ring.dequeue_ptr.checked_add(TRB_SIZE) {
                    Some(next) => ep.ring.dequeue_ptr = next,
                    None => {
                        ep.halted = true;
                        self.pending_events.push(TransferEvent {
                            ep_addr: ep.ep_addr,
                            trb_ptr,
                            event_data: None,
                            residual: 0,
                            completion_code: CompletionCode::TrbError,
                        });
                        return;
                    }
                }

                if ioc {
                    self.pending_events.push(TransferEvent {
                        ep_addr: ep.ep_addr,
                        trb_ptr,
                        event_data: None,
                        residual: 0,
                        completion_code: CompletionCode::Success,
                    });
                }
            }
            TrbType::Link => {
                // If we land on a link TRB after a TD commit, skip it now.
                let _ = self.skip_link_trbs(mem, ep);
            }
            _ => {
                // Unsupported TRB type; treat as TRB error and advance one TRB so we don't wedge.
                let event = TransferEvent {
                    ep_addr: ep.ep_addr,
                    trb_ptr: ep.ring.dequeue_ptr,
                    event_data: None,
                    residual: 0,
                    completion_code: CompletionCode::TrbError,
                };
                self.pending_events.push(event);
                match ep.ring.dequeue_ptr.checked_add(TRB_SIZE) {
                    Some(next) => ep.ring.dequeue_ptr = next,
                    None => ep.halted = true,
                }
            }
        }
    }

    fn skip_link_trbs<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        ep: &mut EndpointState,
    ) -> bool {
        for _ in 0..MAX_LINK_SKIP {
            let trb = match read_trb_checked(mem, ep.ring.dequeue_ptr) {
                Ok(trb) => trb,
                Err(()) => {
                    ep.halted = true;
                    self.pending_events.push(TransferEvent {
                        ep_addr: ep.ep_addr,
                        trb_ptr: ep.ring.dequeue_ptr,
                        event_data: None,
                        residual: 0,
                        completion_code: CompletionCode::TrbError,
                    });
                    return false;
                }
            };
            if trb.cycle() != ep.ring.cycle {
                return true;
            }
            if !matches!(trb.trb_type(), TrbType::Link) {
                return true;
            }

            let target = trb.link_segment_ptr();
            if target == 0 {
                ep.halted = true;
                self.pending_events.push(TransferEvent {
                    ep_addr: ep.ep_addr,
                    trb_ptr: ep.ring.dequeue_ptr,
                    event_data: None,
                    residual: 0,
                    completion_code: CompletionCode::TrbError,
                });
                return false;
            }
            let toggle = trb.link_toggle_cycle();
            ep.ring.dequeue_ptr = target;
            if toggle {
                ep.ring.cycle = !ep.ring.cycle;
            }
        }

        // Too many link TRBs without reaching a transfer TRB: treat as malformed to keep polling
        // bounded, and avoid spending MAX_LINK_SKIP work every tick forever.
        ep.halted = true;
        self.pending_events.push(TransferEvent {
            ep_addr: ep.ep_addr,
            trb_ptr: ep.ring.dequeue_ptr,
            event_data: None,
            residual: 0,
            completion_code: CompletionCode::TrbError,
        });
        false
    }

    fn gather_td<M: MemoryBus + ?Sized>(
        &self,
        mem: &mut M,
        ep: &EndpointState,
        td: &mut TdDescriptor,
    ) -> GatherTdResult {
        let mut ptr = ep.ring.dequeue_ptr;
        let mut cycle = ep.ring.cycle;

        td.buffers.clear();
        td.total_len = 0;
        td.last_trb_ptr = ptr;
        td.last_ioc = false;
        td.event_data = None;
        td.next_dequeue_ptr = ptr;
        td.next_cycle = cycle;

        for _ in 0..MAX_TD_TRBS {
            let trb = match read_trb_checked(mem, ptr) {
                Ok(trb) => trb,
                Err(()) => return GatherTdResult::Fault { trb_ptr: ptr },
            };
            if trb.cycle() != cycle {
                // TD is not fully written yet (or ring empty).
                return GatherTdResult::Incomplete;
            }

            match trb.trb_type() {
                TrbType::Link => {
                    // Link TRBs are not data buffers; follow them and keep gathering.
                    let target = trb.link_segment_ptr();
                    if target == 0 {
                        return GatherTdResult::Fault { trb_ptr: ptr };
                    }
                    let toggle = trb.link_toggle_cycle();
                    ptr = target;
                    if toggle {
                        cycle = !cycle;
                    }
                    continue;
                }
                TrbType::Normal => {
                    let trb_ptr = ptr;
                    let len = trb.transfer_len();
                    td.buffers.push(BufferSegment {
                        paddr: trb.parameter,
                        len,
                    });
                    td.total_len = td.total_len.saturating_add(len);

                    let Some(next) = ptr.checked_add(TRB_SIZE) else {
                        return GatherTdResult::Fault { trb_ptr };
                    };
                    ptr = next;

                    if !trb.chain() {
                        td.last_trb_ptr = trb_ptr;
                        td.last_ioc = trb.ioc();
                        td.next_dequeue_ptr = ptr;
                        td.next_cycle = cycle;
                        return GatherTdResult::Ready;
                    }

                    td.last_trb_ptr = trb_ptr;
                    td.last_ioc = trb.ioc();
                }
                TrbType::EventData => {
                    // Event Data TRBs are valid TD terminators. They do not contribute any buffer
                    // bytes, but may carry a driver-owned pointer in `parameter` for use by the real
                    // xHC when generating Transfer Events.
                    //
                    // Expose the event-data payload so callers can set the Transfer Event TRB ED
                    // bit and preserve the parameter field semantics expected by real xHCI drivers.
                    let trb_ptr = ptr;
                    if trb.chain() {
                        // Event Data TRBs are not expected to be chained.
                        return GatherTdResult::Fault { trb_ptr };
                    }
                    let Some(next) = ptr.checked_add(TRB_SIZE) else {
                        return GatherTdResult::Fault { trb_ptr };
                    };
                    ptr = next;

                    td.last_trb_ptr = trb_ptr;
                    td.last_ioc = trb.ioc();
                    td.event_data = Some(trb.parameter);
                    td.next_dequeue_ptr = ptr;
                    td.next_cycle = cycle;
                    return GatherTdResult::Ready;
                }
                _ => {
                    // Unexpected TRB type inside a TD.
                    return GatherTdResult::Fault { trb_ptr: ptr };
                }
            }
        }

        // Too many TRBs chained; treat as malformed and refuse to run.
        GatherTdResult::Fault {
            trb_ptr: td.last_trb_ptr,
        }
    }

    fn execute_td<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        ep: &mut EndpointState,
        td: TdDescriptor,
    ) {
        let max_len = td.total_len as usize;

        let (completion_code, transferred_bytes_opt) = if ep.direction_in() {
            match self.device.handle_in_transfer(ep.ep_addr, max_len) {
                UsbInResult::Data(mut data) => {
                    if data.len() > max_len {
                        data.truncate(max_len);
                    }
                    let transferred = self.dma_write_in(mem, &td.buffers, &data) as u32;
                    let code = if transferred < td.total_len {
                        CompletionCode::ShortPacket
                    } else {
                        CompletionCode::Success
                    };
                    (code, Some(transferred))
                }
                UsbInResult::Nak => {
                    // Leave TD pending; retry in a future tick.
                    return;
                }
                UsbInResult::Stall => {
                    ep.halted = true;
                    (CompletionCode::StallError, None)
                }
                UsbInResult::Timeout => (CompletionCode::UsbTransactionError, None),
            }
        } else {
            let data = self.dma_read_out(mem, &td.buffers, td.total_len as usize);
            match self.device.handle_out_transfer(ep.ep_addr, &data) {
                UsbOutResult::Ack => (CompletionCode::Success, Some(td.total_len)),
                UsbOutResult::Nak => {
                    return;
                }
                UsbOutResult::Stall => {
                    ep.halted = true;
                    (CompletionCode::StallError, None)
                }
                UsbOutResult::Timeout => (CompletionCode::UsbTransactionError, None),
            }
        };

        // Commit dequeue advancement (TD completed, even with an error).
        ep.ring.dequeue_ptr = td.next_dequeue_ptr;
        ep.ring.cycle = td.next_cycle;

        // xHCI reports *residual bytes* in the Transfer Event TRB.
        let residual = match transferred_bytes_opt {
            Some(transferred) => td.total_len.saturating_sub(transferred),
            None => td.total_len,
        };

        // Emit event if IOC was set on the last TRB, or on any error.
        if td.last_ioc || completion_code != CompletionCode::Success {
            self.pending_events.push(TransferEvent {
                ep_addr: ep.ep_addr,
                trb_ptr: td.last_trb_ptr,
                event_data: td.event_data,
                residual,
                completion_code,
            });
        }
    }

    fn dma_write_in<M: MemoryBus + ?Sized>(
        &self,
        mem: &mut M,
        buffers: &[BufferSegment],
        data: &[u8],
    ) -> usize {
        let mut remaining = data;
        let mut written = 0usize;
        for seg in buffers {
            if remaining.is_empty() {
                break;
            }
            let len = seg.len as usize;
            if len == 0 {
                continue;
            }
            let chunk_len = remaining.len().min(len);
            mem.write_bytes(seg.paddr, &remaining[..chunk_len]);
            remaining = &remaining[chunk_len..];
            written += chunk_len;
        }
        written
    }

    fn dma_read_out<M: MemoryBus + ?Sized>(
        &self,
        mem: &mut M,
        buffers: &[BufferSegment],
        total_len: usize,
    ) -> Vec<u8> {
        let mut out = vec![0u8; total_len];
        let mut offset = 0usize;
        for seg in buffers {
            let len = seg.len as usize;
            if len == 0 {
                continue;
            }
            if offset >= out.len() {
                break;
            }
            let chunk_len = len.min(out.len() - offset);
            mem.read_bytes(seg.paddr, &mut out[offset..offset + chunk_len]);
            offset += chunk_len;
        }
        out.truncate(offset);
        out
    }
}

// --- Endpoint 0 control transfer-ring engine ---------------------------------

const MAX_TRBS_PER_RUN: usize = 1024;
const MAX_CONTROL_DATA_LEN: u32 = 64 * 1024;
/// Maximum number of control DATA packets processed per `ControlEndpoint::process` call.
///
/// Control transfer DATA TRBs can specify up to `MAX_CONTROL_DATA_LEN` bytes. Processing that entire
/// payload packet-by-packet in one call can be extremely expensive (especially when the max packet
/// size is small), so cap the number of packets we will process per call and retry in a future
/// tick. This keeps work deterministic and bounded even for adversarial guests.
const MAX_CONTROL_DATA_PACKETS_PER_RUN: usize = 256;

// Common TRB control bits.
const TRB_CTRL_IOC: u32 = 1 << 5;
const TRB_CTRL_IDT: u32 = 1 << 6;

// Data/Status stage direction bit (DIR).
const DATA_STATUS_TRB_DIR_IN: u32 = 1 << 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    Out,
    In,
}

impl Direction {
    fn from_dir_bit_set(dir_in: bool) -> Self {
        if dir_in {
            Direction::In
        } else {
            Direction::Out
        }
    }
}

fn read_xhci_trb<M: MemoryBus + ?Sized>(mem: &mut M, addr: u64) -> XhciTrb {
    let mut bytes = [0u8; TRB_LEN];
    mem.read_bytes(addr, &mut bytes);
    XhciTrb::from_bytes(bytes)
}

fn write_xhci_trb<M: MemoryBus + ?Sized>(mem: &mut M, addr: u64, trb: &XhciTrb) {
    mem.write_bytes(addr, &trb.to_bytes());
}

fn xhci_trb_transfer_len(trb: &XhciTrb) -> u32 {
    // Transfer TRBs use bits 0..=16.
    trb.status & 0x1ffff
}

fn xhci_trb_idt(trb: &XhciTrb) -> bool {
    trb.control & TRB_CTRL_IDT != 0
}

fn xhci_trb_ioc(trb: &XhciTrb) -> bool {
    trb.control & TRB_CTRL_IOC != 0
}

fn xhci_trb_data_status_direction(trb: &XhciTrb) -> Direction {
    Direction::from_dir_bit_set(trb.control & DATA_STATUS_TRB_DIR_IN != 0)
}

#[derive(Debug)]
struct EventRing {
    base: u64,
    size: u16,
    enqueue_index: u16,
    cycle: bool,
}

impl EventRing {
    fn new(base: u64, size: u16) -> Self {
        Self {
            base,
            size: size.max(1),
            enqueue_index: 0,
            cycle: true,
        }
    }

    fn push<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, mut trb: XhciTrb) {
        // Overwrite the cycle bit according to the producer cycle state.
        trb.set_cycle(self.cycle);

        let addr = self.base.wrapping_add(self.enqueue_index as u64 * TRB_SIZE);
        write_xhci_trb(mem, addr, &trb);

        self.enqueue_index = self.enqueue_index.wrapping_add(1);
        if self.enqueue_index >= self.size {
            self.enqueue_index = 0;
            self.cycle = !self.cycle;
        }
    }

    fn push_transfer_event<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        trb_pointer: u64,
        completion: CompletionCode,
        transfer_len: u32,
        endpoint_id: u8,
        slot_id: u8,
    ) {
        let status = (transfer_len & 0x00ff_ffff) | ((completion as u32) << 24);
        let mut trb = XhciTrb::new(trb_pointer, status, 0);
        trb.set_trb_type(XhciTrbType::TransferEvent);
        trb.set_endpoint_id(endpoint_id);
        trb.set_slot_id(slot_id);
        self.push(mem, trb);
    }
}

#[derive(Clone, Copy, Debug)]
struct Ep0RingState {
    dequeue: u64,
    cycle: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Ep0RingError {
    AddressOverflow,
    InvalidLinkTarget,
    LinkLoop,
}

impl Ep0RingState {
    fn peek<M: MemoryBus + ?Sized>(&self, mem: &mut M) -> Option<XhciTrb> {
        let trb = read_xhci_trb(mem, self.dequeue);
        (trb.cycle() == self.cycle).then_some(trb)
    }

    fn consume(&mut self, trb: &XhciTrb) -> Result<(), Ep0RingError> {
        if matches!(trb.trb_type(), XhciTrbType::Link) {
            let target = trb.link_segment_ptr();
            if target == 0 {
                return Err(Ep0RingError::InvalidLinkTarget);
            }
            if target == self.dequeue && !trb.link_toggle_cycle() {
                // A self-referential link TRB without cycle toggle would never make progress.
                return Err(Ep0RingError::LinkLoop);
            }
            self.dequeue = target;
            if trb.link_toggle_cycle() {
                self.cycle = !self.cycle;
            }
        } else {
            self.dequeue = self
                .dequeue
                .checked_add(TRB_SIZE)
                .ok_or(Ep0RingError::AddressOverflow)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug)]
enum ControlEp0State {
    ExpectSetup,
    /// SETUP consumed; next TRB should be DATA or STATUS.
    ExpectDataOrStatus,
    Data {
        direction: Direction,
        trb_addr: u64,
        buffer_ptr: u64,
        len: u32,
        transferred: u32,
        idt: bool,
        immediate: [u8; 8],
        ioc: bool,
    },
    /// DATA consumed; next TRB should be STATUS.
    ExpectStatus {
        expected_data_len: u32,
        actual_data_len: u32,
        short_packet: bool,
    },
    Status {
        direction: Direction,
        trb_addr: u64,
        expected_data_len: u32,
        actual_data_len: u32,
        short_packet: bool,
        ioc: bool,
    },
}

#[derive(Clone, Debug)]
struct ControlEndpoint {
    ring: Ep0RingState,
    max_packet_size: u16,
    retry_at_ms: u64,
    doorbell_pending: bool,
    faulted: bool,
    state: ControlEp0State,
}

impl ControlEndpoint {
    fn new(dequeue: u64, cycle: bool, max_packet_size: u16) -> Self {
        Self {
            ring: Ep0RingState { dequeue, cycle },
            max_packet_size: max_packet_size.max(1),
            retry_at_ms: 0,
            doorbell_pending: false,
            faulted: false,
            state: ControlEp0State::ExpectSetup,
        }
    }

    fn has_pending_work(&self) -> bool {
        if self.faulted {
            return false;
        }
        self.doorbell_pending || !matches!(self.state, ControlEp0State::ExpectSetup)
    }

    fn schedule_retry(&mut self, now_ms: u64) {
        self.retry_at_ms = now_ms.saturating_add(1);
    }

    fn push_event<M: MemoryBus + ?Sized>(
        events: &mut Option<EventRing>,
        mem: &mut M,
        trb_pointer: u64,
        completion: CompletionCode,
        transfer_len: u32,
        endpoint_id: u8,
        slot_id: u8,
    ) {
        if let Some(events) = events.as_mut() {
            events.push_transfer_event(
                mem,
                trb_pointer,
                completion,
                transfer_len,
                endpoint_id,
                slot_id,
            );
        }
    }

    fn reset_to_setup(&mut self) {
        self.state = ControlEp0State::ExpectSetup;
    }

    fn fault_ring<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        events: &mut Option<EventRing>,
        trb_addr: u64,
        endpoint_id: u8,
        slot_id: u8,
    ) {
        Self::push_event(
            events,
            mem,
            trb_addr,
            CompletionCode::TrbError,
            0,
            endpoint_id,
            slot_id,
        );
        self.faulted = true;
        self.doorbell_pending = false;
        self.reset_to_setup();
    }

    fn skip_until_status_or_empty<M: MemoryBus + ?Sized>(&mut self, mem: &mut M) {
        let mut iterations = 0usize;
        while iterations < 32 {
            iterations += 1;
            let Some(trb) = self.ring.peek(mem) else {
                break;
            };
            let ty = trb.trb_type();
            if self.ring.consume(&trb).is_err() {
                break;
            }
            if matches!(ty, XhciTrbType::StatusStage) {
                break;
            }
        }
    }

    fn process<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        dev: &mut AttachedUsbDevice,
        events: &mut Option<EventRing>,
        now_ms: u64,
        slot_id: u8,
        endpoint_id: u8,
    ) {
        if now_ms < self.retry_at_ms {
            return;
        }
        if self.faulted {
            return;
        }

        let mut processed_any = false;

        for _ in 0..MAX_TRBS_PER_RUN {
            if now_ms < self.retry_at_ms {
                break;
            }

            let Some(trb) = self.ring.peek(mem) else {
                break;
            };
            let trb_addr = self.ring.dequeue;

            // Link TRBs are transparent to the endpoint state machine.
            if matches!(trb.trb_type(), XhciTrbType::Link) {
                if self.ring.consume(&trb).is_err() {
                    self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                    break;
                }
                processed_any = true;
                continue;
            }

            match &mut self.state {
                ControlEp0State::ExpectSetup => {
                    if !matches!(trb.trb_type(), XhciTrbType::SetupStage) {
                        // Unexpected TRB: consume it so we don't get stuck on malformed rings.
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            CompletionCode::TrbError,
                            0,
                            endpoint_id,
                            slot_id,
                        );
                        if self.ring.consume(&trb).is_err() {
                            self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                            break;
                        }
                        processed_any = true;
                        continue;
                    }

                    let setup_bytes = trb.parameter.to_le_bytes();
                    let setup = SetupPacket::from_bytes(setup_bytes);
                    match dev.handle_setup(setup) {
                        UsbOutResult::Ack => {
                            if xhci_trb_ioc(&trb) {
                                Self::push_event(
                                    events,
                                    mem,
                                    trb_addr,
                                    CompletionCode::Success,
                                    0,
                                    endpoint_id,
                                    slot_id,
                                );
                            }
                            if self.ring.consume(&trb).is_err() {
                                self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                                break;
                            }
                            self.state = ControlEp0State::ExpectDataOrStatus;
                            processed_any = true;
                        }
                        UsbOutResult::Nak => {
                            self.schedule_retry(now_ms);
                            break;
                        }
                        UsbOutResult::Stall => {
                            Self::push_event(
                                events,
                                mem,
                                trb_addr,
                                CompletionCode::StallError,
                                0,
                                endpoint_id,
                                slot_id,
                            );
                            if self.ring.consume(&trb).is_err() {
                                self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                                break;
                            }
                            self.skip_until_status_or_empty(mem);
                            self.reset_to_setup();
                            processed_any = true;
                        }
                        UsbOutResult::Timeout => {
                            Self::push_event(
                                events,
                                mem,
                                trb_addr,
                                CompletionCode::UsbTransactionError,
                                0,
                                endpoint_id,
                                slot_id,
                            );
                            if self.ring.consume(&trb).is_err() {
                                self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                                break;
                            }
                            self.skip_until_status_or_empty(mem);
                            self.reset_to_setup();
                            processed_any = true;
                        }
                    }
                }
                ControlEp0State::ExpectDataOrStatus => match trb.trb_type() {
                    XhciTrbType::DataStage => {
                        let direction = xhci_trb_data_status_direction(&trb);
                        let len = xhci_trb_transfer_len(&trb).min(MAX_CONTROL_DATA_LEN);
                        let idt = xhci_trb_idt(&trb);
                        let mut immediate = trb.parameter.to_le_bytes();
                        // For IN immediate-data transfers, we will write the response bytes back
                        // into the TRB parameter field. Start from zeroed bytes so we do not
                        // preserve any guest-provided garbage.
                        if idt && matches!(direction, Direction::In) {
                            immediate = [0u8; 8];
                        }
                        let buffer_ptr = trb.parameter;
                        let ioc = xhci_trb_ioc(&trb);
                        self.state = ControlEp0State::Data {
                            direction,
                            trb_addr,
                            buffer_ptr,
                            len,
                            transferred: 0,
                            idt,
                            immediate,
                            ioc,
                        };
                        processed_any = true;
                    }
                    XhciTrbType::StatusStage => {
                        let direction = xhci_trb_data_status_direction(&trb);
                        let ioc = xhci_trb_ioc(&trb);
                        self.state = ControlEp0State::Status {
                            direction,
                            trb_addr,
                            expected_data_len: 0,
                            actual_data_len: 0,
                            short_packet: false,
                            ioc,
                        };
                        processed_any = true;
                    }
                    _ => {
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            CompletionCode::TrbError,
                            0,
                            endpoint_id,
                            slot_id,
                        );
                        if self.ring.consume(&trb).is_err() {
                            self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                            break;
                        }
                        self.reset_to_setup();
                        processed_any = true;
                    }
                },
                ControlEp0State::Data {
                    direction,
                    trb_addr: expected_addr,
                    buffer_ptr,
                    len,
                    transferred,
                    idt,
                    immediate,
                    ioc,
                } => {
                    if *expected_addr != trb_addr
                        || !matches!(trb.trb_type(), XhciTrbType::DataStage)
                    {
                        // Ring corruption: bail out to SETUP state.
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            CompletionCode::TrbError,
                            0,
                            endpoint_id,
                            slot_id,
                        );
                        self.reset_to_setup();
                        processed_any = true;
                        continue;
                    }

                    let mut short_packet = false;
                    let mut completion: Option<CompletionCode> = None;
                    let max_packet = self.max_packet_size as u32;
                    let mut remaining = len.saturating_sub(*transferred);
                    let mut packet_budget = MAX_CONTROL_DATA_PACKETS_PER_RUN;

                    while remaining != 0 && packet_budget != 0 {
                        packet_budget -= 1;
                        let chunk_max = remaining.min(max_packet) as usize;

                        match direction {
                            Direction::In => match dev.handle_in(0, chunk_max) {
                                UsbInResult::Data(mut chunk) => {
                                    if chunk.len() > chunk_max {
                                        chunk.truncate(chunk_max);
                                    }

                                    let got = chunk.len() as u32;
                                    if *idt {
                                        let offset = *transferred as usize;
                                        let end = offset.saturating_add(chunk.len());
                                        if end > immediate.len() {
                                            completion = Some(CompletionCode::TrbError);
                                            break;
                                        }
                                        immediate[offset..end].copy_from_slice(&chunk);

                                        // Write the updated immediate bytes back into the TRB
                                        // parameter field in guest memory.
                                        let mut updated_trb = trb;
                                        updated_trb.parameter = u64::from_le_bytes(*immediate);
                                        write_xhci_trb(mem, trb_addr, &updated_trb);
                                    } else {
                                        let addr = buffer_ptr.wrapping_add(*transferred as u64);
                                        mem.write_physical(addr, &chunk);
                                    }
                                    *transferred = transferred.saturating_add(got);
                                    remaining = remaining.saturating_sub(got);
                                    if got < chunk_max as u32 {
                                        // Short packet terminates the data stage early.
                                        short_packet = true;
                                        break;
                                    }
                                }
                                UsbInResult::Nak => {
                                    self.schedule_retry(now_ms);
                                    return;
                                }
                                UsbInResult::Stall => {
                                    completion = Some(CompletionCode::StallError);
                                    break;
                                }
                                UsbInResult::Timeout => {
                                    completion = Some(CompletionCode::UsbTransactionError);
                                    break;
                                }
                            },
                            Direction::Out => {
                                let mut chunk = Vec::with_capacity(chunk_max);
                                if *idt {
                                    // Immediate data is carried in the parameter field (up to 8
                                    // bytes). This is rare for control transfers; support the basic
                                    // OUT case for robustness.
                                    let offset = (*transferred as usize).min(immediate.len());
                                    let avail = immediate[offset..].len().min(chunk_max);
                                    chunk.extend_from_slice(&immediate[offset..offset + avail]);
                                    if avail != chunk_max {
                                        completion = Some(CompletionCode::TrbError);
                                        break;
                                    }
                                } else {
                                    chunk.resize(chunk_max, 0);
                                    let addr = buffer_ptr.wrapping_add(*transferred as u64);
                                    mem.read_physical(addr, &mut chunk);
                                }

                                match dev.handle_out(0, &chunk) {
                                    UsbOutResult::Ack => {
                                        *transferred = transferred.saturating_add(chunk_max as u32);
                                        remaining = remaining.saturating_sub(chunk_max as u32);
                                    }
                                    UsbOutResult::Nak => {
                                        // Retry without advancing the transfer offset.
                                        self.schedule_retry(now_ms);
                                        return;
                                    }
                                    UsbOutResult::Stall => {
                                        completion = Some(CompletionCode::StallError);
                                        break;
                                    }
                                    UsbOutResult::Timeout => {
                                        completion = Some(CompletionCode::UsbTransactionError);
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    // If we still have bytes remaining and did not observe any terminal condition
                    // (short packet / stall / timeout), we've exhausted the deterministic per-call
                    // work budget. Yield and retry the DATA stage on a future tick without
                    // consuming the DATA TRB.
                    if remaining != 0 && completion.is_none() && !short_packet {
                        self.schedule_retry(now_ms);
                        return;
                    }

                    if let Some(completion) = completion {
                        let remaining_len = len.saturating_sub(*transferred);
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            completion,
                            remaining_len,
                            endpoint_id,
                            slot_id,
                        );
                        // Consume the DATA TRB and any following STATUS TRB so the ring can
                        // continue.
                        if self.ring.consume(&trb).is_err() {
                            self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                            break;
                        }
                        self.skip_until_status_or_empty(mem);
                        self.reset_to_setup();
                        processed_any = true;
                        continue;
                    }

                    let expected_data_len = *len;
                    let actual_data_len = *transferred;
                    let short = short_packet || actual_data_len < expected_data_len;

                    if *ioc {
                        let remaining_len = expected_data_len.saturating_sub(actual_data_len);
                        let completion = if short {
                            CompletionCode::ShortPacket
                        } else {
                            CompletionCode::Success
                        };
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            completion,
                            remaining_len,
                            endpoint_id,
                            slot_id,
                        );
                    }

                    // DATA stage completed (possibly short). Advance past the DATA TRB and proceed
                    // to STATUS.
                    if self.ring.consume(&trb).is_err() {
                        self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                        break;
                    }
                    self.state = ControlEp0State::ExpectStatus {
                        expected_data_len,
                        actual_data_len,
                        short_packet: short,
                    };
                    processed_any = true;
                }
                ControlEp0State::ExpectStatus {
                    expected_data_len,
                    actual_data_len,
                    short_packet,
                } => {
                    if matches!(trb.trb_type(), XhciTrbType::StatusStage) {
                        let direction = xhci_trb_data_status_direction(&trb);
                        let ioc = xhci_trb_ioc(&trb);
                        self.state = ControlEp0State::Status {
                            direction,
                            trb_addr,
                            expected_data_len: *expected_data_len,
                            actual_data_len: *actual_data_len,
                            short_packet: *short_packet,
                            ioc,
                        };
                        processed_any = true;
                    } else {
                        // Unexpected; consume and abort this transfer.
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            CompletionCode::TrbError,
                            0,
                            endpoint_id,
                            slot_id,
                        );
                        if self.ring.consume(&trb).is_err() {
                            self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                            break;
                        }
                        self.reset_to_setup();
                        processed_any = true;
                    }
                }
                ControlEp0State::Status {
                    direction,
                    trb_addr: expected_addr,
                    expected_data_len,
                    actual_data_len,
                    short_packet,
                    ioc,
                } => {
                    if *expected_addr != trb_addr
                        || !matches!(trb.trb_type(), XhciTrbType::StatusStage)
                    {
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            CompletionCode::TrbError,
                            0,
                            endpoint_id,
                            slot_id,
                        );
                        self.reset_to_setup();
                        processed_any = true;
                        continue;
                    }

                    let completion = match direction {
                        Direction::In => match dev.handle_in(0, 0) {
                            UsbInResult::Data(data) => {
                                if !data.is_empty() {
                                    CompletionCode::TrbError
                                } else if *short_packet {
                                    CompletionCode::ShortPacket
                                } else {
                                    CompletionCode::Success
                                }
                            }
                            UsbInResult::Nak => {
                                self.schedule_retry(now_ms);
                                break;
                            }
                            UsbInResult::Stall => CompletionCode::StallError,
                            UsbInResult::Timeout => CompletionCode::UsbTransactionError,
                        },
                        Direction::Out => match dev.handle_out(0, &[]) {
                            UsbOutResult::Ack => {
                                if *short_packet {
                                    CompletionCode::ShortPacket
                                } else {
                                    CompletionCode::Success
                                }
                            }
                            UsbOutResult::Nak => {
                                self.schedule_retry(now_ms);
                                break;
                            }
                            UsbOutResult::Stall => CompletionCode::StallError,
                            UsbOutResult::Timeout => CompletionCode::UsbTransactionError,
                        },
                    };

                    // Consume the status TRB regardless of success/error.
                    if self.ring.consume(&trb).is_err() {
                        self.fault_ring(mem, events, trb_addr, endpoint_id, slot_id);
                        break;
                    }

                    // xHCI Transfer Event TRBs report the number of bytes remaining for the TD.
                    let remaining_len = expected_data_len.saturating_sub(*actual_data_len);

                    // Generate an event on IOC or for non-success completions so software can
                    // observe failures/short packets.
                    if *ioc || completion != CompletionCode::Success {
                        Self::push_event(
                            events,
                            mem,
                            trb_addr,
                            completion,
                            remaining_len,
                            endpoint_id,
                            slot_id,
                        );
                    }

                    self.reset_to_setup();
                    processed_any = true;
                }
            }
        }

        if processed_any && matches!(self.state, ControlEp0State::ExpectSetup) {
            // If we've drained the ring and are idle, clear the doorbell latch.
            if self.ring.peek(mem).is_none() {
                self.doorbell_pending = false;
            }
        }
    }
}

/// Minimal xHCI-style root hub used by [`XhciController`](super::XhciController).
pub struct XhciRootHub {
    ports: Vec<Option<AttachedUsbDevice>>,
}

impl XhciRootHub {
    pub fn new(num_ports: usize) -> Self {
        Self {
            ports: (0..num_ports).map(|_| None).collect(),
        }
    }

    pub fn num_ports(&self) -> usize {
        self.ports.len()
    }

    pub fn attach(&mut self, port: usize, model: Box<dyn UsbDeviceModel>) {
        if let Some(slot) = self.ports.get_mut(port) {
            *slot = Some(AttachedUsbDevice::new(model));
        }
    }

    pub fn detach(&mut self, port: usize) {
        if let Some(slot) = self.ports.get_mut(port) {
            *slot = None;
        }
    }

    fn port_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.as_mut()
    }

    pub fn tick_1ms(&mut self) {
        for dev in self.ports.iter_mut().filter_map(|p| p.as_mut()) {
            dev.tick_1ms();
        }
    }
}

impl Default for XhciRootHub {
    fn default() -> Self {
        Self::new(1)
    }
}

#[derive(Clone, Debug)]
struct Slot {
    port: usize,
    ep0: Option<ControlEndpoint>,
}

/// Endpoint-0 transfer-ring engine.
///
/// This is the "transfer ring" portion of an eventual xHCI controller implementation, scoped to
/// control transfers on endpoint 0 (Setup/Data/Status TRBs).
pub struct Ep0TransferEngine {
    hub: XhciRootHub,
    slots: Vec<Option<Slot>>,
    event_ring: Option<EventRing>,
    now_ms: u64,
}

impl fmt::Debug for Ep0TransferEngine {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Ep0TransferEngine")
            .field("num_ports", &self.hub.num_ports())
            .field("now_ms", &self.now_ms)
            .finish()
    }
}

impl Ep0TransferEngine {
    pub fn new_with_ports(num_ports: usize) -> Self {
        Self {
            hub: XhciRootHub::new(num_ports),
            slots: vec![None; 256],
            event_ring: None,
            now_ms: 0,
        }
    }

    pub fn reset_state(&mut self) {
        // Preserve attached devices/topology; only reset controller-local transfer state.
        self.slots = vec![None; 256];
        self.event_ring = None;
        self.now_ms = 0;
    }

    pub fn hub_mut(&mut self) -> &mut XhciRootHub {
        &mut self.hub
    }

    pub fn hub(&self) -> &XhciRootHub {
        &self.hub
    }

    pub fn set_event_ring(&mut self, base: u64, size: u16) {
        self.event_ring = Some(EventRing::new(base, size));
    }

    pub fn enable_slot(&mut self, port: usize) -> Option<u8> {
        if port >= self.hub.num_ports() {
            return None;
        }
        if self.hub.ports.get(port)?.is_none() {
            return None;
        }
        for slot_id in 1..self.slots.len() {
            if self.slots[slot_id].is_none() {
                self.slots[slot_id] = Some(Slot { port, ep0: None });
                return Some(slot_id as u8);
            }
        }
        None
    }

    /// Configure the default control endpoint (endpoint ID 1) transfer ring for a slot.
    pub fn configure_ep0(
        &mut self,
        slot_id: u8,
        dequeue: u64,
        cycle: bool,
        max_packet_size: u16,
    ) -> bool {
        let Some(slot) = self
            .slots
            .get_mut(slot_id as usize)
            .and_then(|s| s.as_mut())
        else {
            return false;
        };
        slot.ep0 = Some(ControlEndpoint::new(dequeue, cycle, max_packet_size));
        true
    }

    pub fn ring_doorbell<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        slot_id: u8,
        endpoint_id: u8,
    ) {
        if endpoint_id != 1 {
            return;
        }
        let Some(slot) = self
            .slots
            .get_mut(slot_id as usize)
            .and_then(|s| s.as_mut())
        else {
            return;
        };
        let Some(ep0) = slot.ep0.as_mut() else {
            return;
        };
        ep0.doorbell_pending = true;
        self.process_slot_ep0(mem, slot_id);
    }

    pub fn tick_1ms<M: MemoryBus + ?Sized>(&mut self, mem: &mut M) {
        self.now_ms = self.now_ms.wrapping_add(1);
        self.hub.tick_1ms();

        // Process any endpoints that are waiting on NAK pacing or were doorbelled.
        for slot_id in 1..self.slots.len() {
            let Some(slot) = self.slots[slot_id].as_mut() else {
                continue;
            };
            let Some(ep0) = slot.ep0.as_mut() else {
                continue;
            };
            if !ep0.has_pending_work() {
                continue;
            }
            if self.now_ms < ep0.retry_at_ms {
                continue;
            }
            self.process_slot_ep0(mem, slot_id as u8);
        }
    }

    fn process_slot_ep0<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, slot_id: u8) {
        let now_ms = self.now_ms;

        let Some(slot) = self
            .slots
            .get_mut(slot_id as usize)
            .and_then(|s| s.as_mut())
        else {
            return;
        };
        let Some(ep0) = slot.ep0.as_mut() else {
            return;
        };
        if now_ms < ep0.retry_at_ms {
            return;
        }
        let Some(dev) = self.hub.port_device_mut(slot.port) else {
            return;
        };

        ep0.process(mem, dev, &mut self.event_ring, now_ms, slot_id, 1);
    }
}

impl Default for Ep0TransferEngine {
    fn default() -> Self {
        Self::new_with_ports(1)
    }
}
