//! MVP xHCI command ring handling focused on endpoint management commands.
//!
//! This module intentionally models only the subset of xHCI endpoint commands that common OS
//! drivers exercise to recover from errors and re-prime transfer rings:
//! - Stop Endpoint
//! - Reset Endpoint
//! - Set TR Dequeue Pointer
//!
//! The state machine is simplified to three states: Running/Stopped/Halted.

use alloc::vec::Vec;

use crate::MemoryBus;

use super::ring::{RingCursor, RingPoll};
use super::trb::{Trb, TrbType, TRB_LEN};

/// xHCI completion codes used by endpoint management commands.
///
/// This is a deliberately small subset; additional completion codes can be added as more commands
/// are implemented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CompletionCode {
    Invalid = 0,
    Success = 1,
    TrbError = 5,
    SlotNotEnabled = 11,
    EndpointNotEnabled = 12,
    ParameterError = 17,
    ContextStateError = 19,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointState {
    Running,
    Stopped,
    Halted,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndpointContext {
    pub tr_dequeue_ptr: u64,
    pub dcs: bool,
}

impl EndpointContext {
    fn new(ptr: u64, dcs: bool) -> Self {
        Self {
            tr_dequeue_ptr: ptr & !0x0f,
            dcs,
        }
    }

    fn sync_from_ring(&mut self, ring: &RingCursor) {
        self.tr_dequeue_ptr = ring.dequeue_ptr();
        self.dcs = ring.cycle_state();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    CommandCompletion {
        completion_code: CompletionCode,
        slot_id: u8,
        endpoint_id: u8,
        command_trb_pointer: u64,
    },
    TransferEvent {
        completion_code: CompletionCode,
        slot_id: u8,
        endpoint_id: u8,
        trb_pointer: u64,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Endpoint {
    state: EndpointState,
    context: EndpointContext,
    ring: RingCursor,
}

impl Endpoint {
    fn new(ring: RingCursor) -> Self {
        let context = EndpointContext::new(ring.dequeue_ptr(), ring.cycle_state());
        Self {
            state: EndpointState::Running,
            context,
            ring,
        }
    }

    fn set_dequeue_ptr(&mut self, ptr: u64, dcs: bool) {
        self.ring = RingCursor::new(ptr, dcs);
        self.context.tr_dequeue_ptr = ptr & !0x0f;
        self.context.dcs = dcs;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Slot {
    endpoints: [Option<Endpoint>; 32],
}

impl Slot {
    fn new() -> Self {
        Self {
            endpoints: core::array::from_fn(|_| None),
        }
    }
}

const DEFAULT_RING_STEP_BUDGET: usize = 64;

/// A small, deterministic endpoint-management executor.
///
/// This is used by unit tests and intended to be embedded by a future full xHCI controller model.
pub struct XhciEndpointManager {
    max_slots: u8,
    slots: Vec<Option<Slot>>,
    events: Vec<Event>,
}

impl XhciEndpointManager {
    pub fn new(max_slots: u8) -> Self {
        let mut slots = Vec::with_capacity(max_slots as usize + 1);
        slots.push(None); // slot 0 is reserved.
        slots.extend((0..max_slots).map(|_| None));
        Self {
            max_slots,
            slots,
            events: Vec::new(),
        }
    }

    /// Enable a slot (MVP helper; a full xHCI model uses the Enable Slot command).
    pub fn enable_slot(&mut self, slot_id: u8) -> Result<(), CompletionCode> {
        if slot_id == 0 || slot_id > self.max_slots {
            return Err(CompletionCode::ParameterError);
        }
        if self.slots.get(slot_id as usize).is_none() {
            return Err(CompletionCode::ParameterError);
        }
        self.slots[slot_id as usize] = Some(Slot::new());
        Ok(())
    }

    /// Configure an endpoint with an initial Transfer Ring dequeue pointer.
    pub fn configure_endpoint(
        &mut self,
        slot_id: u8,
        endpoint_id: u8,
        tr_dequeue_ptr: u64,
        dcs: bool,
    ) -> Result<(), CompletionCode> {
        let slot = self.slot_mut(slot_id)?;
        if endpoint_id == 0 || endpoint_id as usize >= slot.endpoints.len() {
            return Err(CompletionCode::ParameterError);
        }
        slot.endpoints[endpoint_id as usize] = Some(Endpoint::new(RingCursor::new(tr_dequeue_ptr, dcs)));
        Ok(())
    }

    pub fn drain_events(&mut self) -> Vec<Event> {
        core::mem::take(&mut self.events)
    }

    /// Poll and execute all available command TRBs from the command ring.
    pub fn process_command_ring<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, ring: &mut RingCursor) {
        loop {
            match ring.poll(mem, DEFAULT_RING_STEP_BUDGET) {
                RingPoll::Ready(item) => self.execute_command_trb(item.trb, item.paddr),
                RingPoll::NotReady => break,
                RingPoll::Err(_) => break,
            }
        }
    }

    /// Execute a single command TRB.
    pub fn execute_command_trb(&mut self, trb: Trb, command_trb_pointer: u64) {
        let slot_id = trb.slot_id();
        let endpoint_id = trb.endpoint_id();

        let completion_code = match trb.trb_type() {
            TrbType::StopEndpointCommand => self.cmd_stop_endpoint(slot_id, endpoint_id),
            TrbType::ResetEndpointCommand => self.cmd_reset_endpoint(slot_id, endpoint_id),
            TrbType::SetTrDequeuePointerCommand => {
                let ptr = trb.parameter & !0x0f;
                let dcs = (trb.parameter & 0x01) != 0;
                self.cmd_set_tr_dequeue_pointer(slot_id, endpoint_id, ptr, dcs)
            }
            _ => CompletionCode::TrbError,
        };

        self.events.push(Event::CommandCompletion {
            completion_code,
            slot_id,
            endpoint_id,
            command_trb_pointer,
        });
    }

    /// Ring a doorbell for an endpoint.
    ///
    /// MVP semantics:
    /// - Running endpoints consume TRBs and emit Transfer Events.
    /// - Stopped/Halted endpoints ignore doorbells (no progress/events).
    pub fn ring_doorbell<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, slot_id: u8, endpoint_id: u8) {
        let mut new_events = Vec::new();
        {
            let Ok(ep) = self.endpoint_mut(slot_id, endpoint_id) else {
                return;
            };
            if ep.state != EndpointState::Running {
                return;
            }

            loop {
                match ep.ring.poll(mem, DEFAULT_RING_STEP_BUDGET) {
                    RingPoll::Ready(item) => {
                        // Keep the in-memory endpoint context in sync with the dequeue pointer.
                        ep.context.sync_from_ring(&ep.ring);

                        // MVP: treat Normal TRBs as successful transfers; anything else is a TRB
                        // error and halts the endpoint.
                        let completion_code = if matches!(item.trb.trb_type(), TrbType::Normal) {
                            CompletionCode::Success
                        } else {
                            ep.state = EndpointState::Halted;
                            CompletionCode::TrbError
                        };

                        new_events.push(Event::TransferEvent {
                            completion_code,
                            slot_id,
                            endpoint_id,
                            trb_pointer: item.paddr,
                        });

                        if completion_code != CompletionCode::Success {
                            break;
                        }
                    }
                    RingPoll::NotReady => break,
                    RingPoll::Err(_) => {
                        ep.state = EndpointState::Halted;
                        break;
                    }
                }
            }
        }
        self.events.extend(new_events);
    }

    fn slot_mut(&mut self, slot_id: u8) -> Result<&mut Slot, CompletionCode> {
        if slot_id == 0 || slot_id > self.max_slots {
            return Err(CompletionCode::ParameterError);
        }
        self.slots
            .get_mut(slot_id as usize)
            .and_then(|s| s.as_mut())
            .ok_or(CompletionCode::SlotNotEnabled)
    }

    fn endpoint_mut(&mut self, slot_id: u8, endpoint_id: u8) -> Result<&mut Endpoint, CompletionCode> {
        let slot = self.slot_mut(slot_id)?;
        if endpoint_id == 0 || endpoint_id as usize >= slot.endpoints.len() {
            return Err(CompletionCode::ParameterError);
        }
        slot.endpoints[endpoint_id as usize]
            .as_mut()
            .ok_or(CompletionCode::EndpointNotEnabled)
    }

    fn cmd_stop_endpoint(&mut self, slot_id: u8, endpoint_id: u8) -> CompletionCode {
        match self.endpoint_mut(slot_id, endpoint_id) {
            Ok(ep) => {
                ep.state = EndpointState::Stopped;
                CompletionCode::Success
            }
            Err(code) => code,
        }
    }

    fn cmd_reset_endpoint(&mut self, slot_id: u8, endpoint_id: u8) -> CompletionCode {
        match self.endpoint_mut(slot_id, endpoint_id) {
            Ok(ep) => {
                // MVP: always clear the halted condition and allow transfers.
                ep.state = EndpointState::Running;
                CompletionCode::Success
            }
            Err(code) => code,
        }
    }

    fn cmd_set_tr_dequeue_pointer(
        &mut self,
        slot_id: u8,
        endpoint_id: u8,
        ptr: u64,
        dcs: bool,
    ) -> CompletionCode {
        if ptr == 0 || !ptr.is_multiple_of(TRB_LEN as u64) {
            return CompletionCode::ParameterError;
        }

        match self.endpoint_mut(slot_id, endpoint_id) {
            Ok(ep) => {
                ep.set_dequeue_ptr(ptr, dcs);
                // MVP: allow transfers again after re-priming.
                ep.state = EndpointState::Running;
                CompletionCode::Success
            }
            Err(code) => code,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestMemory {
        data: Vec<u8>,
    }

    impl TestMemory {
        fn new(size: usize) -> Self {
            Self { data: vec![0; size] }
        }
    }

    impl MemoryBus for TestMemory {
        fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
            let start = usize::try_from(paddr).expect("paddr too large for TestMemory");
            let end = start + buf.len();
            buf.copy_from_slice(&self.data[start..end]);
        }

        fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
            let start = usize::try_from(paddr).expect("paddr too large for TestMemory");
            let end = start + buf.len();
            self.data[start..end].copy_from_slice(buf);
        }
    }

    fn write_trb(mem: &mut TestMemory, addr: u64, trb: Trb) {
        trb.write_to(mem, addr);
    }

    fn normal_trb(cycle: bool) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::Normal);
        trb.set_cycle(cycle);
        trb
    }

    fn link_trb(target: u64, cycle: bool, toggle_cycle: bool) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::Link);
        trb.set_cycle(cycle);
        trb.parameter = target & !0x0f;
        trb.set_link_toggle_cycle(toggle_cycle);
        trb
    }

    fn stop_endpoint_cmd(slot_id: u8, endpoint_id: u8) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::StopEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb
    }

    fn reset_endpoint_cmd(slot_id: u8, endpoint_id: u8) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::ResetEndpointCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb
    }

    fn set_tr_dequeue_ptr_cmd(slot_id: u8, endpoint_id: u8, ptr: u64, dcs: bool) -> Trb {
        let mut trb = Trb::default();
        trb.set_trb_type(TrbType::SetTrDequeuePointerCommand);
        trb.set_slot_id(slot_id);
        trb.set_endpoint_id(endpoint_id);
        trb.parameter = (ptr & !0x0f) | if dcs { 1 } else { 0 };
        trb
    }

    #[test]
    fn endpoint_stop_set_tr_dequeue_pointer_and_reset() {
        let mut mem = TestMemory::new(0x10000);

        let ring_base = 0x1000u64;

        // Prime a transfer ring with 3 Normal TRBs (cycle=1) and a Link TRB back to the start.
        write_trb(&mut mem, ring_base + 0 * TRB_LEN as u64, normal_trb(true));
        write_trb(&mut mem, ring_base + 1 * TRB_LEN as u64, normal_trb(true));
        write_trb(&mut mem, ring_base + 2 * TRB_LEN as u64, normal_trb(true));
        write_trb(
            &mut mem,
            ring_base + 3 * TRB_LEN as u64,
            link_trb(ring_base, true, true),
        );

        // Create a manager with one slot and one endpoint.
        let mut mgr = XhciEndpointManager::new(1);
        mgr.enable_slot(1).unwrap();
        mgr.configure_endpoint(1, 1, ring_base, true).unwrap();

        // Stop Endpoint should produce a completion event.
        mgr.execute_command_trb(stop_endpoint_cmd(1, 1), 0x2000);
        assert_eq!(
            mgr.drain_events(),
            vec![Event::CommandCompletion {
                completion_code: CompletionCode::Success,
                slot_id: 1,
                endpoint_id: 1,
                command_trb_pointer: 0x2000,
            }]
        );

        // Doorbell while stopped should not advance the ring or produce transfer events.
        mgr.ring_doorbell(&mut mem, 1, 1);
        assert!(mgr.drain_events().is_empty());

        // Set TR Dequeue Pointer to TRB1; transfers should resume from the new pointer.
        let new_ptr = ring_base + TRB_LEN as u64;
        mgr.execute_command_trb(set_tr_dequeue_ptr_cmd(1, 1, new_ptr, true), 0x2010);
        assert_eq!(
            mgr.drain_events(),
            vec![Event::CommandCompletion {
                completion_code: CompletionCode::Success,
                slot_id: 1,
                endpoint_id: 1,
                command_trb_pointer: 0x2010,
            }]
        );

        mgr.ring_doorbell(&mut mem, 1, 1);
        assert_eq!(
            mgr.drain_events(),
            vec![
                Event::TransferEvent {
                    completion_code: CompletionCode::Success,
                    slot_id: 1,
                    endpoint_id: 1,
                    trb_pointer: ring_base + 1 * TRB_LEN as u64,
                },
                Event::TransferEvent {
                    completion_code: CompletionCode::Success,
                    slot_id: 1,
                    endpoint_id: 1,
                    trb_pointer: ring_base + 2 * TRB_LEN as u64,
                }
            ]
        );

        // After consuming the TD and following the Link TRB, the ring should have wrapped and the
        // cycle state should have toggled.
        {
            let slot = mgr.slots[1].as_ref().unwrap();
            let ep = slot.endpoints[1].as_ref().unwrap();
            assert_eq!(ep.ring.dequeue_ptr(), ring_base);
            assert!(!ep.ring.cycle_state());
        }

        // Prime a new Normal TRB with cycle=0 at the start of the ring.
        write_trb(&mut mem, ring_base, normal_trb(false));

        // Force a halted endpoint (simulating a transfer error).
        mgr.slots[1].as_mut().unwrap().endpoints[1]
            .as_mut()
            .unwrap()
            .state = EndpointState::Halted;

        // Doorbell while halted should not generate events.
        mgr.ring_doorbell(&mut mem, 1, 1);
        assert!(mgr.drain_events().is_empty());

        // Reset Endpoint should clear halted and allow transfers.
        mgr.execute_command_trb(reset_endpoint_cmd(1, 1), 0x2020);
        assert_eq!(
            mgr.drain_events(),
            vec![Event::CommandCompletion {
                completion_code: CompletionCode::Success,
                slot_id: 1,
                endpoint_id: 1,
                command_trb_pointer: 0x2020,
            }]
        );

        mgr.ring_doorbell(&mut mem, 1, 1);
        assert_eq!(
            mgr.drain_events(),
            vec![Event::TransferEvent {
                completion_code: CompletionCode::Success,
                slot_id: 1,
                endpoint_id: 1,
                trb_pointer: ring_base,
            }]
        );

    }
}
