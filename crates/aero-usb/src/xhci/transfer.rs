use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

pub use super::trb::{CompletionCode, Trb, TrbType};
use super::trb::TRB_LEN;

use crate::device::{UsbInResult, UsbOutResult};
use crate::memory::MemoryBus;
use crate::UsbDeviceModel;

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

/// A completion notification emitted by the transfer executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TransferEvent {
    pub ep_addr: u8,
    /// Pointer to the TRB that generated the event (typically the last TRB of the TD).
    pub trb_ptr: u64,
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

    pub fn endpoint_state(&self, ep_addr: u8) -> Option<&EndpointState> {
        self.endpoints.get(&ep_addr)
    }

    pub fn endpoint_state_mut(&mut self, ep_addr: u8) -> Option<&mut EndpointState> {
        self.endpoints.get_mut(&ep_addr)
    }

    pub fn take_events(&mut self) -> Vec<TransferEvent> {
        core::mem::take(&mut self.pending_events)
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

        let trb = read_trb(mem, ep.ring.dequeue_ptr);
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
                    next_dequeue_ptr: ep.ring.dequeue_ptr,
                    next_cycle: ep.ring.cycle,
                };

                match self.gather_td(mem, ep, &mut td) {
                    GatherTdResult::Incomplete => return,
                    GatherTdResult::Ready => self.execute_td(mem, ep, td),
                    GatherTdResult::Fault { trb_ptr } => {
                        ep.halted = true;
                        self.pending_events.push(TransferEvent {
                            ep_addr: ep.ep_addr,
                            trb_ptr,
                            residual: 0,
                            completion_code: CompletionCode::TrbError,
                        });
                    }
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

    fn skip_link_trbs<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, ep: &mut EndpointState) -> bool {
        for _ in 0..MAX_LINK_SKIP {
            let trb = read_trb(mem, ep.ring.dequeue_ptr);
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
        td.next_dequeue_ptr = ptr;
        td.next_cycle = cycle;

        for _ in 0..MAX_TD_TRBS {
            let trb = read_trb(mem, ptr);
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
