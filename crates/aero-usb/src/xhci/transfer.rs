use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use crate::device::{UsbInResult, UsbOutResult};
use crate::memory::MemoryBus;
use crate::UsbDeviceModel;

const TRB_SIZE: u64 = 16;

/// Maximum number of TRBs we'll inspect while skipping link TRBs at the start of a tick.
///
/// This is purely a safety bound against malformed rings (self-referential links, etc).
const MAX_LINK_SKIP: usize = 32;

/// Maximum number of TRBs we'll consider part of a single TD (chain).
const MAX_TD_TRBS: usize = 64;

/// xHCI TRB type (TRBType field).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TrbType {
    Normal = 1,
    Link = 6,
    TransferEvent = 32,
}

/// xHCI transfer completion codes (Completion Code field).
///
/// This is a minimal subset required for interrupt HID transfers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CompletionCode {
    Invalid = 0,
    Success = 1,
    UsbTransactionError = 4,
    TrbError = 5,
    StallError = 6,
    ShortPacket = 13,
}

/// Raw 16-byte TRB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trb {
    pub dword0: u32,
    pub dword1: u32,
    pub dword2: u32,
    pub dword3: u32,
}

impl Trb {
    pub fn from_bytes(bytes: [u8; 16]) -> Self {
        Self {
            dword0: u32::from_le_bytes(bytes[0..4].try_into().unwrap()),
            dword1: u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            dword2: u32::from_le_bytes(bytes[8..12].try_into().unwrap()),
            dword3: u32::from_le_bytes(bytes[12..16].try_into().unwrap()),
        }
    }

    pub fn to_bytes(self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.dword0.to_le_bytes());
        out[4..8].copy_from_slice(&self.dword1.to_le_bytes());
        out[8..12].copy_from_slice(&self.dword2.to_le_bytes());
        out[12..16].copy_from_slice(&self.dword3.to_le_bytes());
        out
    }

    pub fn parameter(self) -> u64 {
        (self.dword0 as u64) | ((self.dword1 as u64) << 32)
    }

    pub fn trb_type(self) -> u8 {
        ((self.dword3 >> 10) & 0x3f) as u8
    }

    pub fn cycle(self) -> bool {
        (self.dword3 & 0x1) != 0
    }

    pub fn chain(self) -> bool {
        (self.dword3 & (1 << 4)) != 0
    }

    pub fn ioc(self) -> bool {
        (self.dword3 & (1 << 5)) != 0
    }

    pub fn toggle_cycle(self) -> bool {
        // Link TRB Toggle Cycle (TC) bit.
        (self.dword3 & (1 << 1)) != 0
    }

    pub fn transfer_len(self) -> u32 {
        // TRB Transfer Length field (17 bits).
        self.dword2 & 0x1ffff
    }
}

pub fn read_trb<M: MemoryBus + ?Sized>(mem: &mut M, paddr: u64) -> Trb {
    let mut bytes = [0u8; 16];
    mem.read_physical(paddr, &mut bytes);
    Trb::from_bytes(bytes)
}

pub fn write_trb<M: MemoryBus + ?Sized>(mem: &mut M, paddr: u64, trb: Trb) {
    mem.write_physical(paddr, &trb.to_bytes());
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
        self.skip_link_trbs(mem, ep);

        let trb = read_trb(mem, ep.ring.dequeue_ptr);
        if trb.cycle() != ep.ring.cycle {
            return;
        }

        match trb.trb_type() {
            t if t == TrbType::Normal as u8 => {
                let Some(td) = self.gather_td(mem, ep) else {
                    return;
                };
                self.execute_td(mem, ep, td);
            }
            t if t == TrbType::Link as u8 => {
                // If we land on a link TRB after a TD commit, skip it now.
                self.skip_link_trbs(mem, ep);
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
                ep.ring.dequeue_ptr = ep.ring.dequeue_ptr.wrapping_add(TRB_SIZE);
            }
        }
    }

    fn skip_link_trbs<M: MemoryBus + ?Sized>(&self, mem: &mut M, ep: &mut EndpointState) {
        for _ in 0..MAX_LINK_SKIP {
            let trb = read_trb(mem, ep.ring.dequeue_ptr);
            if trb.cycle() != ep.ring.cycle {
                break;
            }
            if trb.trb_type() != TrbType::Link as u8 {
                break;
            }

            let target = trb.parameter() & !0xF;
            let toggle = trb.toggle_cycle();
            ep.ring.dequeue_ptr = target;
            if toggle {
                ep.ring.cycle = !ep.ring.cycle;
            }
        }
    }

    fn gather_td<M: MemoryBus + ?Sized>(&self, mem: &mut M, ep: &EndpointState) -> Option<TdDescriptor> {
        let mut ptr = ep.ring.dequeue_ptr;
        let mut cycle = ep.ring.cycle;

        let mut buffers = Vec::new();
        let mut total_len: u32 = 0;

        for _ in 0..MAX_TD_TRBS {
            let trb = read_trb(mem, ptr);
            if trb.cycle() != cycle {
                // TD is not fully written yet (or ring empty).
                return None;
            }

            match trb.trb_type() {
                t if t == TrbType::Link as u8 => {
                    // Link TRBs are not data buffers; follow them and keep gathering.
                    let target = trb.parameter() & !0xF;
                    let toggle = trb.toggle_cycle();
                    ptr = target;
                    if toggle {
                        cycle = !cycle;
                    }
                    continue;
                }
                t if t == TrbType::Normal as u8 => {
                    let trb_ptr = ptr;
                    let len = trb.transfer_len();
                    buffers.push(BufferSegment {
                        paddr: trb.parameter(),
                        len,
                    });
                    total_len = total_len.saturating_add(len);

                    ptr = ptr.wrapping_add(TRB_SIZE);

                    if !trb.chain() {
                        return Some(TdDescriptor {
                            buffers,
                            total_len,
                            last_trb_ptr: trb_ptr,
                            last_ioc: trb.ioc(),
                            next_dequeue_ptr: ptr,
                            next_cycle: cycle,
                        });
                    }
                }
                _ => {
                    // Unexpected TRB type inside a TD.
                    return Some(TdDescriptor {
                        buffers,
                        total_len,
                        last_trb_ptr: ptr,
                        last_ioc: true,
                        next_dequeue_ptr: ptr.wrapping_add(TRB_SIZE),
                        next_cycle: cycle,
                    });
                }
            }
        }

        // Too many TRBs chained; treat as malformed and refuse to run.
        None
    }

    fn execute_td<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, ep: &mut EndpointState, td: TdDescriptor) {
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
            mem.write_physical(seg.paddr, &remaining[..chunk_len]);
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
            mem.read_physical(seg.paddr, &mut out[offset..offset + chunk_len]);
            offset += chunk_len;
        }
        out.truncate(offset);
        out
    }
}
