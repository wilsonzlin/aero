use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::device::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use crate::{MemoryBus, SetupPacket, UsbDeviceModel};

use super::context::{
    EndpointContext, EndpointType, InputContext32, XHCI_ROUTE_STRING_MAX_DEPTH,
    XHCI_ROUTE_STRING_MAX_PORT,
};
use super::trb::{CompletionCode, Trb, TrbType, TRB_LEN};

const CONTEXT_ALIGN: u64 = 64;
const CONTEXT_SIZE: u64 = super::context::CONTEXT_SIZE as u64;

// Input context layout (32-byte contexts):
//   0x00: Input Control Context (32 bytes)
//   0x20: Slot Context (32 bytes)
//   0x40: Endpoint 0 Context (32 bytes)
const INPUT_CONTROL_CTX_SIZE: u64 = CONTEXT_SIZE;
const INPUT_SLOT_CTX_OFFSET: u64 = INPUT_CONTROL_CTX_SIZE;
const INPUT_EP0_CTX_OFFSET: u64 = INPUT_CONTROL_CTX_SIZE + CONTEXT_SIZE;

// Device context layout (32-byte contexts):
//   0x00: Slot Context
//   0x20: Endpoint 0 Context
const DEVICE_SLOT_CTX_OFFSET: u64 = 0;
const DEVICE_EP0_CTX_OFFSET: u64 = CONTEXT_SIZE;

// Slot Context DW3 contains:
// - bits 0..=7: USB Device Address (xHC-owned after Address Device)
// - bits 27..=31: Slot State (xHC-owned)
//
// When copying Slot Contexts from guest-provided Input Contexts into the output Device Context we
// must preserve controller-owned fields.
const SLOT_STATE_MASK_DWORD3: u32 = 0xF800_00FF;

const ICC_DROP_FLAGS_OFFSET: u64 = 0;
const ICC_ADD_FLAGS_OFFSET: u64 = 4;

const ICC_CTX_FLAG_SLOT: u32 = 1 << 0;
const ICC_CTX_FLAG_EP0: u32 = 1 << 1;
const ICC_CTX_FLAG_EP1_IN: u32 = 1 << 3;
const ICC_CTX_FLAG_EP2_OUT: u32 = 1 << 4;
const ICC_CTX_FLAG_EP2_IN: u32 = 1 << 5;

const EP_STATE_MASK_DWORD0: u32 = 0x7;

const EP_STATE_RUNNING: u32 = 1;
const EP_STATE_HALTED: u32 = 2;
const EP_STATE_STOPPED: u32 = 3;

const USB_REQUEST_SET_ADDRESS: u8 = 0x05;

/// Command ring state (dequeue pointer + cycle state).
#[derive(Clone, Copy, Debug)]
pub struct CommandRing {
    pub dequeue_ptr: u64,
    pub cycle_state: bool,
}

impl CommandRing {
    pub fn new(dequeue_ptr: u64) -> Self {
        Self {
            dequeue_ptr,
            cycle_state: true,
        }
    }

    /// Construct command ring consumer state from the Command Ring Control Register (CRCR).
    ///
    /// xHCI encodes the dequeue pointer in bits 63:6 (64-byte aligned) and the ring cycle state in
    /// bit 0.
    pub const fn from_crcr(crcr: u64) -> Self {
        Self {
            dequeue_ptr: crcr & !0x3f,
            cycle_state: (crcr & 1) != 0,
        }
    }
}

/// Single-segment event ring producer state.
#[derive(Clone, Copy, Debug)]
pub struct EventRing {
    pub base: u64,
    pub size_trbs: u16,
    pub enqueue_index: u16,
    pub cycle_state: bool,
}

impl EventRing {
    pub fn new(base: u64, size_trbs: u16) -> Self {
        Self {
            base,
            size_trbs,
            enqueue_index: 0,
            cycle_state: true,
        }
    }

    fn entry_addr(&self) -> u64 {
        self.base + u64::from(self.enqueue_index) * (TRB_LEN as u64)
    }

    fn advance(&mut self) {
        self.enqueue_index += 1;
        if self.enqueue_index >= self.size_trbs {
            self.enqueue_index = 0;
            self.cycle_state = !self.cycle_state;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct EndpointState {
    dequeue_ptr: u64,
    cycle: bool,
    stopped: bool,
    halted: bool,
}

#[derive(Clone, Debug)]
struct SlotState {
    root_port: Option<u8>,
    route: Vec<u8>,
    address: u8,
    /// Endpoint states indexed by DCI (Device Context Index).
    ///
    /// DCI 0 is unused (slot context), DCI 1..=31 correspond to endpoint contexts.
    endpoints: Vec<Option<EndpointState>>,
}

impl SlotState {
    fn new() -> Self {
        Self {
            root_port: None,
            route: Vec::new(),
            address: 0,
            endpoints: vec![None; 32],
        }
    }
}

/// Minimal xHCI command ring processor.
///
/// This is *not* a full xHCI controller model; it is only the command-ring + event-ring plumbing
/// required by common OS drivers during early enumeration.
pub struct CommandRingProcessor {
    /// Guest physical address space size (bytes). Used for defensive bounds checking.
    mem_size: u64,

    /// Maximum number of device slots supported by the controller.
    max_slots: u8,

    /// DCBAA base pointer (guest physical address).
    dcbaa_ptr: u64,

    /// Slots enabled via Enable Slot Command.
    ///
    /// Index 0 is unused (slot IDs are 1-based).
    slots_enabled: Vec<bool>,

    pub command_ring: CommandRing,
    pub event_ring: EventRing,

    root_ports: Vec<Option<AttachedUsbDevice>>,
    slots: Vec<Option<SlotState>>,
    next_device_address: u8,

    /// Set when we detect a fatal error (e.g. malformed ring pointers).
    pub host_controller_error: bool,
}

impl CommandRingProcessor {
    pub fn new(
        mem_size: u64,
        max_slots: u8,
        dcbaa_ptr: u64,
        command_ring: CommandRing,
        event_ring: EventRing,
    ) -> Self {
        Self {
            mem_size,
            max_slots,
            dcbaa_ptr,
            slots_enabled: vec![false; usize::from(max_slots).saturating_add(1)],
            command_ring,
            event_ring,
            root_ports: Vec::new(),
            slots: vec![None; (max_slots as usize) + 1],
            next_device_address: 1,
            host_controller_error: false,
        }
    }

    pub fn attach_root_port(&mut self, port: u8, model: Box<dyn UsbDeviceModel>) {
        let idx = (port as usize).saturating_sub(1);
        if idx >= self.root_ports.len() {
            self.root_ports.resize_with(idx + 1, || None);
        }
        self.root_ports[idx] = Some(AttachedUsbDevice::new(model));
    }

    pub fn port_device(&self, port: u8) -> Option<&AttachedUsbDevice> {
        let idx = (port as usize).checked_sub(1)?;
        self.root_ports.get(idx)?.as_ref()
    }

    pub fn port_device_mut(&mut self, port: u8) -> Option<&mut AttachedUsbDevice> {
        let idx = (port as usize).checked_sub(1)?;
        self.root_ports.get_mut(idx)?.as_mut()
    }

    /// Resolve a device in the xHCI topology.
    ///
    /// - `root_port` is the 1-based Root Hub Port Number from the Slot Context.
    /// - `route` is the decoded Route String, where each element is a 1-based downstream hub port.
    fn find_device_by_topology(&mut self, root_port: u8, route: &[u8]) -> Option<&mut AttachedUsbDevice> {
        if root_port == 0 {
            return None;
        }
        if route.len() > XHCI_ROUTE_STRING_MAX_DEPTH {
            return None;
        }

        let idx = usize::from(root_port.checked_sub(1)?);
        let mut dev = self.root_ports.get_mut(idx)?.as_mut()?;
        for &hop in route {
            if hop == 0 || hop > XHCI_ROUTE_STRING_MAX_PORT {
                return None;
            }
            dev = dev.model_mut().hub_port_device_mut(hop).ok()?;
        }
        Some(dev)
    }

    /// Process up to `max_trbs` TRBs from the command ring.
    ///
    /// Processing stops early when:
    /// - the next TRB's cycle bit does not match `command_ring.cycle_state` (ring empty)
    /// - a fatal ring pointer error is encountered
    pub fn process(&mut self, mem: &mut dyn MemoryBus, max_trbs: usize) {
        if self.host_controller_error {
            return;
        }

        for _ in 0..max_trbs {
            let trb_addr = self.command_ring.dequeue_ptr;
            let trb = match self.read_trb(mem, trb_addr) {
                Ok(trb) => trb,
                Err(_) => {
                    self.host_controller_error = true;
                    return;
                }
            };

            if trb.cycle() != self.command_ring.cycle_state {
                // No more commands available.
                return;
            }

            match trb.trb_type() {
                TrbType::Link => {
                    if !self.handle_link_trb(mem, trb_addr, trb) {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::EnableSlotCommand => {
                    let (code, slot_id) = self.handle_enable_slot(mem);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::DisableSlotCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_disable_slot(mem, slot_id);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::NoOpCommand => {
                    let slot_id = trb.slot_id();
                    self.emit_command_completion(mem, trb_addr, CompletionCode::Success, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::AddressDeviceCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_address_device(mem, trb);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::ConfigureEndpointCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_configure_endpoint(mem, trb);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::EvaluateContextCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_evaluate_context(mem, trb);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::StopEndpointCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_stop_endpoint(mem, trb);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::ResetEndpointCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_reset_endpoint(mem, trb);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                TrbType::SetTrDequeuePointerCommand => {
                    let slot_id = trb.slot_id();
                    let code = self.handle_set_tr_dequeue_pointer(mem, trb);
                    self.emit_command_completion(mem, trb_addr, code, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
                _ => {
                    // Unsupported command. Spec would typically return TRB Error.
                    let slot_id = trb.slot_id();
                    self.emit_command_completion(mem, trb_addr, CompletionCode::TrbError, slot_id);
                    if !self.advance_cmd_dequeue() {
                        self.host_controller_error = true;
                        return;
                    }
                }
            }
        }
    }

    fn handle_enable_slot(&mut self, mem: &mut dyn MemoryBus) -> (CompletionCode, u8) {
        if self.dcbaa_ptr == 0 {
            return (CompletionCode::ContextStateError, 0);
        }
        if (self.dcbaa_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return (CompletionCode::ParameterError, 0);
        }

        let slot_id = match (1u8..=self.max_slots).find(|&id| {
            !self
                .slots_enabled
                .get(usize::from(id))
                .copied()
                .unwrap_or(false)
        }) {
            Some(id) => id,
            None => return (CompletionCode::NoSlotsAvailableError, 0),
        };

        let dcbaa_entry_addr = match self
            .dcbaa_ptr
            .checked_add(u64::from(slot_id) * 8)
        {
            Some(addr) => addr,
            None => return (CompletionCode::ParameterError, 0),
        };
        if !self.check_range(dcbaa_entry_addr, 8) {
            return (CompletionCode::ParameterError, 0);
        }

        // Initialise the DCBAA entry to 0; software will install a Device Context pointer during
        // Address Device.
        mem.write_physical(dcbaa_entry_addr, &0u64.to_le_bytes());
        if let Some(flag) = self.slots_enabled.get_mut(usize::from(slot_id)) {
            *flag = true;
        }
        if let Some(slot) = self.slots.get_mut(usize::from(slot_id)) {
            *slot = Some(SlotState::new());
        }

        (CompletionCode::Success, slot_id)
    }

    fn handle_disable_slot(&mut self, mem: &mut dyn MemoryBus, slot_id: u8) -> CompletionCode {
        // Treat malformed/out-of-range Slot IDs as "not enabled" so we never index out of bounds
        // and callers get a stable completion code.
        if slot_id == 0 || slot_id > self.max_slots {
            return CompletionCode::SlotNotEnabledError;
        }
        let idx = usize::from(slot_id);
        let enabled = self.slots_enabled.get(idx).copied().unwrap_or(false);
        if !enabled {
            return CompletionCode::SlotNotEnabledError;
        }

        if self.dcbaa_ptr == 0 {
            return CompletionCode::ContextStateError;
        }
        if (self.dcbaa_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        let dcbaa_entry_addr = match self
            .dcbaa_ptr
            .checked_add(u64::from(slot_id) * 8)
        {
            Some(addr) => addr,
            None => return CompletionCode::ParameterError,
        };
        if !self.check_range(dcbaa_entry_addr, 8) {
            return CompletionCode::ParameterError;
        }

        mem.write_physical(dcbaa_entry_addr, &0u64.to_le_bytes());
        if let Some(flag) = self.slots_enabled.get_mut(idx) {
            *flag = false;
        }
        if let Some(state) = self.slots.get_mut(idx).and_then(|s| s.take()) {
            if let Some(root_port) = state.root_port {
                if let Some(dev) = self.find_device_by_topology(root_port, &state.route) {
                    dev.reset();
                }
            }
        }
        CompletionCode::Success
    }

    fn advance_cmd_dequeue(&mut self) -> bool {
        match self.command_ring.dequeue_ptr.checked_add(TRB_LEN as u64) {
            Some(next) => {
                self.command_ring.dequeue_ptr = next;
                true
            }
            None => false,
        }
    }

    fn alloc_device_address(&mut self) -> Option<u8> {
        for _ in 0..127 {
            let addr = self.next_device_address;
            self.next_device_address = if addr >= 127 { 1 } else { addr + 1 };
            if !self.address_in_use(addr) {
                return Some(addr);
            }
        }
        None
    }

    fn address_in_use(&self, addr: u8) -> bool {
        self.slots
            .iter()
            .flatten()
            .any(|slot| slot.address == addr)
    }

    fn handle_link_trb(&mut self, _mem: &mut dyn MemoryBus, _addr: u64, trb: Trb) -> bool {
        let target = trb.pointer();
        if (target & 0xF) != 0 {
            return false;
        }
        if !self.check_range(target, TRB_LEN as u64) {
            return false;
        }

        self.command_ring.dequeue_ptr = target;
        if trb.link_toggle_cycle() {
            self.command_ring.cycle_state = !self.command_ring.cycle_state;
        }
        true
    }

    fn read_device_context_ptr(
        &self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
    ) -> Result<u64, CompletionCode> {
        if slot_id == 0 || slot_id > self.max_slots {
            return Err(CompletionCode::SlotNotEnabledError);
        }
        let idx = usize::from(slot_id);
        if !self.slots_enabled.get(idx).copied().unwrap_or(false) {
            return Err(CompletionCode::SlotNotEnabledError);
        }
        if self.dcbaa_ptr == 0 {
            return Err(CompletionCode::ContextStateError);
        }

        if (self.dcbaa_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return Err(CompletionCode::ParameterError);
        }

        let dcbaa_entry_addr = self
            .dcbaa_ptr
            .checked_add(u64::from(slot_id) * 8)
            .ok_or(CompletionCode::ParameterError)?;
        if !self.check_range(dcbaa_entry_addr, 8) {
            return Err(CompletionCode::ParameterError);
        }

        let dev_ctx_ptr = self
            .read_u64(mem, dcbaa_entry_addr)
            .map_err(|_| CompletionCode::ParameterError)?;
        if dev_ctx_ptr == 0 {
            return Err(CompletionCode::ContextStateError);
        }
        if (dev_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return Err(CompletionCode::ParameterError);
        }

        Ok(dev_ctx_ptr)
    }

    fn endpoint_context_addr(
        &self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
    ) -> Result<u64, CompletionCode> {
        if endpoint_id == 0 || endpoint_id > 31 {
            return Err(CompletionCode::ParameterError);
        }
        let dev_ctx_ptr = self.read_device_context_ptr(mem, slot_id)?;
        let addr = dev_ctx_ptr
            .checked_add(u64::from(endpoint_id) * CONTEXT_SIZE)
            .ok_or(CompletionCode::ParameterError)?;
        if !self.check_range(addr, CONTEXT_SIZE) {
            return Err(CompletionCode::ParameterError);
        }
        Ok(addr)
    }

    fn handle_stop_endpoint(&mut self, mem: &mut dyn MemoryBus, cmd: Trb) -> CompletionCode {
        let slot_id = cmd.slot_id();
        let endpoint_id = cmd.endpoint_id();
        let ep_ctx = match self.endpoint_context_addr(mem, slot_id, endpoint_id) {
            Ok(addr) => addr,
            Err(code) => return code,
        };

        let mut ctx = EndpointContext::read_from(mem, ep_ctx);
        let prev_state = ctx.endpoint_state() as u32;
        if prev_state == 0 {
            return CompletionCode::EndpointNotEnabledError;
        }
        ctx.set_endpoint_state(EP_STATE_STOPPED as u8);
        ctx.write_to(mem, ep_ctx);

        // Keep internal endpoint state in sync with guest context. Stop Endpoint updates endpoint
        // run-state but does not change the transfer ring dequeue pointer.
        if let Some(slot) = self.internal_slot_mut(slot_id) {
            if let Some(entry) = slot.endpoints.get_mut(endpoint_id as usize) {
                match entry {
                    Some(ep) => {
                        ep.stopped = true;
                    }
                    None => {
                        *entry = Some(EndpointState {
                            dequeue_ptr: ctx.tr_dequeue_pointer(),
                            cycle: ctx.dcs(),
                            stopped: true,
                            // Stop Endpoint should not clear a previously halted endpoint (MVP:
                            // preserve if we can infer it from the previous guest state).
                            halted: prev_state == EP_STATE_HALTED,
                        });
                    }
                }
            }
        }
        CompletionCode::Success
    }

    fn handle_reset_endpoint(&mut self, mem: &mut dyn MemoryBus, cmd: Trb) -> CompletionCode {
        let slot_id = cmd.slot_id();
        let endpoint_id = cmd.endpoint_id();
        let ep_ctx = match self.endpoint_context_addr(mem, slot_id, endpoint_id) {
            Ok(addr) => addr,
            Err(code) => return code,
        };

        let mut ctx = EndpointContext::read_from(mem, ep_ctx);
        if ctx.endpoint_state() == 0 {
            return CompletionCode::EndpointNotEnabledError;
        }
        // MVP: clear halted/stopped and allow transfers again.
        ctx.set_endpoint_state(EP_STATE_RUNNING as u8);
        ctx.write_to(mem, ep_ctx);

        // Reset Endpoint clears stopped/halted state but does not modify the transfer ring dequeue
        // pointer.
        if let Some(slot) = self.internal_slot_mut(slot_id) {
            if let Some(entry) = slot.endpoints.get_mut(endpoint_id as usize) {
                match entry {
                    Some(ep) => {
                        ep.stopped = false;
                        ep.halted = false;
                    }
                    None => {
                        *entry = Some(EndpointState {
                            dequeue_ptr: ctx.tr_dequeue_pointer(),
                            cycle: ctx.dcs(),
                            stopped: false,
                            halted: false,
                        });
                    }
                }
            }
        }
        CompletionCode::Success
    }

    fn handle_set_tr_dequeue_pointer(
        &mut self,
        mem: &mut dyn MemoryBus,
        cmd: Trb,
    ) -> CompletionCode {
        let slot_id = cmd.slot_id();
        let endpoint_id = cmd.endpoint_id();
        let ep_ctx = match self.endpoint_context_addr(mem, slot_id, endpoint_id) {
            Ok(addr) => addr,
            Err(code) => return code,
        };

        // Bits 1..=3 are reserved in the command parameter field (bit0 is DCS).
        if (cmd.parameter & 0x0e) != 0 {
            return CompletionCode::ParameterError;
        }
        // Streams are not supported by this model. Stream ID lives in DW2 bits 16..=31.
        let stream_id = (cmd.status >> 16) & 0xffff;
        if stream_id != 0 {
            return CompletionCode::ParameterError;
        }

        let ptr = cmd.parameter & !0x0f;
        let dcs = (cmd.parameter & 0x01) != 0;
        if ptr == 0 {
            return CompletionCode::ParameterError;
        }
        if !self.check_range(ptr, TRB_LEN as u64) {
            return CompletionCode::ParameterError;
        }

        let mut ctx = EndpointContext::read_from(mem, ep_ctx);
        let endpoint_state = ctx.endpoint_state() as u32;
        if endpoint_state == 0 {
            return CompletionCode::EndpointNotEnabledError;
        }
        ctx.set_tr_dequeue_pointer(ptr, dcs);
        ctx.write_to(mem, ep_ctx);

        // MVP: preserve existing endpoint state (typically Stopped) and only update the dequeue
        // pointer + DCS.
        if let Some(slot) = self.internal_slot_mut(slot_id) {
            if let Some(entry) = slot.endpoints.get_mut(endpoint_id as usize) {
                match entry {
                    Some(ep) => {
                        ep.dequeue_ptr = ptr;
                        ep.cycle = dcs;
                    }
                    None => {
                        *entry = Some(EndpointState {
                            dequeue_ptr: ptr,
                            cycle: dcs,
                            stopped: endpoint_state == EP_STATE_STOPPED,
                            halted: endpoint_state == EP_STATE_HALTED,
                        });
                    }
                }
            }
        }
        CompletionCode::Success
    }

    fn internal_slot_mut(&mut self, slot_id: u8) -> Option<&mut SlotState> {
        if slot_id == 0 || slot_id > self.max_slots {
            return None;
        }
        let idx = usize::from(slot_id);
        let slot = self.slots.get_mut(idx)?;
        Some(slot.get_or_insert_with(SlotState::new))
    }

    fn handle_evaluate_context(&mut self, mem: &mut dyn MemoryBus, cmd: Trb) -> CompletionCode {
        let slot_id = cmd.slot_id();
        if slot_id == 0 || slot_id > self.max_slots {
            return CompletionCode::SlotNotEnabledError;
        }
        let idx = usize::from(slot_id);
        if !self.slots_enabled.get(idx).copied().unwrap_or(false) {
            return CompletionCode::SlotNotEnabledError;
        }

        if (self.dcbaa_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        let dcbaa_entry_addr = match self.dcbaa_ptr.checked_add(u64::from(slot_id) * 8) {
            Some(addr) => addr,
            None => return CompletionCode::ParameterError,
        };
        if !self.check_range(dcbaa_entry_addr, 8) {
            return CompletionCode::ParameterError;
        }

        let dev_ctx_ptr = match self.read_u64(mem, dcbaa_entry_addr) {
            Ok(v) => v,
            Err(_) => return CompletionCode::ParameterError,
        };
        if dev_ctx_ptr == 0 {
            return CompletionCode::ContextStateError;
        }
        if (dev_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        // We must be able to touch at least Slot + EP0 contexts.
        let min_device_ctx_len = 2 * CONTEXT_SIZE;
        if !self.check_range(dev_ctx_ptr, min_device_ctx_len) {
            return CompletionCode::ParameterError;
        }

        let input_ctx_ptr = cmd.pointer();
        if (input_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        // Must be able to read at least ICC + Slot + EP0 contexts.
        let min_input_ctx_len = 3 * CONTEXT_SIZE;
        if !self.check_range(input_ctx_ptr, min_input_ctx_len) {
            return CompletionCode::ParameterError;
        }

        let drop_flags = match self.read_u32(mem, input_ctx_ptr + ICC_DROP_FLAGS_OFFSET) {
            Ok(v) => v,
            Err(_) => return CompletionCode::ParameterError,
        };
        let add_flags = match self.read_u32(mem, input_ctx_ptr + ICC_ADD_FLAGS_OFFSET) {
            Ok(v) => v,
            Err(_) => return CompletionCode::ParameterError,
        };

        // MVP: we support updating EP0, and allow Slot Context updates (but do not require them).
        let supported_add = ICC_CTX_FLAG_SLOT | ICC_CTX_FLAG_EP0;
        if drop_flags != 0 {
            return CompletionCode::ParameterError;
        }
        if (add_flags & ICC_CTX_FLAG_EP0) == 0 {
            return CompletionCode::ParameterError;
        }
        if (add_flags & !supported_add) != 0 {
            return CompletionCode::ParameterError;
        }

        // Slot context updates are optional for the EP0 MPS use-case, but Linux commonly includes
        // the slot context in the input context for Evaluate Context. Apply it defensively while
        // preserving the xHC-owned Slot State field.
        if (add_flags & ICC_CTX_FLAG_SLOT) != 0 {
            let in_slot = input_ctx_ptr + INPUT_SLOT_CTX_OFFSET;
            let out_slot = dev_ctx_ptr + DEVICE_SLOT_CTX_OFFSET;
            if let Err(code) = self.copy_slot_context_preserve_state(mem, in_slot, out_slot) {
                return code;
            }
        }

        let in_ep0 = input_ctx_ptr + INPUT_EP0_CTX_OFFSET;
        let out_ep0 = dev_ctx_ptr + DEVICE_EP0_CTX_OFFSET;
        if let Err(code) = self.update_ep0_context(mem, in_ep0, out_ep0) {
            return code;
        }

        CompletionCode::Success
    }

    fn handle_address_device(&mut self, mem: &mut dyn MemoryBus, cmd: Trb) -> CompletionCode {
        // Address Device (xHCI 1.2 §4.6.5, §6.4.3.4).
        //
        // MVP:
        // - Validate ICC + Slot + EP0 contexts from the guest Input Context.
        // - Send USB SET_ADDRESS to the attached root-port device.
        // - Copy Slot Context + EP0 Context into the output Device Context.
        let slot_id = cmd.slot_id();
        if slot_id == 0 || slot_id > self.max_slots {
            return CompletionCode::SlotNotEnabledError;
        }
        let idx = usize::from(slot_id);
        if !self.slots_enabled.get(idx).copied().unwrap_or(false) {
            return CompletionCode::SlotNotEnabledError;
        }
        if cmd.address_device_bsr() {
            // We do not implement the "Block Set Address Request" mode yet.
            return CompletionCode::ParameterError;
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(code) => return code,
        };

        // We must be able to touch at least Slot + EP0 contexts.
        let min_device_ctx_len = 2 * CONTEXT_SIZE;
        if !self.check_range(dev_ctx_ptr, min_device_ctx_len) {
            return CompletionCode::ParameterError;
        }

        let input_ctx_ptr = cmd.pointer();
        if (input_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        // Must be able to read at least ICC + Slot + EP0 contexts.
        let min_input_ctx_len = 3 * CONTEXT_SIZE;
        if !self.check_range(input_ctx_ptr, min_input_ctx_len) {
            return CompletionCode::ParameterError;
        }

        let input_ctx = InputContext32::new(input_ctx_ptr);
        let icc = input_ctx.input_control(mem);

        // Address Device requires Slot + EP0 contexts.
        let supported_add = ICC_CTX_FLAG_SLOT | ICC_CTX_FLAG_EP0;
        if icc.drop_flags() != 0 {
            return CompletionCode::ParameterError;
        }
        if (icc.add_flags() & (ICC_CTX_FLAG_SLOT | ICC_CTX_FLAG_EP0)) != (ICC_CTX_FLAG_SLOT | ICC_CTX_FLAG_EP0) {
            return CompletionCode::ParameterError;
        }
        if (icc.add_flags() & !supported_add) != 0 {
            return CompletionCode::ParameterError;
        }

        // Validate Route String + Root Hub Port Number.
        let in_slot = match input_ctx.slot_context(mem) {
            Ok(ctx) => ctx,
            Err(_) => return CompletionCode::ParameterError,
        };
        let route = match in_slot.parsed_route_string() {
            Ok(rs) => rs.ports_from_root(),
            Err(_) => return CompletionCode::ParameterError,
        };
        if in_slot.root_hub_port_number() == 0 {
            return CompletionCode::ParameterError;
        }
        let root_port = in_slot.root_hub_port_number();

        // Validate EP0 type.
        let in_ep0 = match input_ctx.endpoint_context(mem, 1) {
            Ok(ctx) => ctx,
            Err(_) => return CompletionCode::ParameterError,
        };
        if in_ep0.endpoint_type() != EndpointType::Control {
            return CompletionCode::ParameterError;
        }

        let Some(addr) = self.alloc_device_address() else {
            return CompletionCode::TrbError;
        };

        // USB-level side effect: issue SET_ADDRESS to the attached device.
        {
            let Some(dev) = self.find_device_by_topology(root_port, &route) else {
                return CompletionCode::ContextStateError;
            };
            let setup = SetupPacket {
                bm_request_type: 0x00, // HostToDevice | Standard | Device
                b_request: USB_REQUEST_SET_ADDRESS,
                w_value: addr as u16,
                w_index: 0,
                w_length: 0,
            };
            if dev.handle_setup(setup) != UsbOutResult::Ack {
                return CompletionCode::TrbError;
            }
            match dev.handle_in(0, 0) {
                UsbInResult::Data(data) if data.is_empty() => {}
                _ => return CompletionCode::TrbError,
            }
        }

        // Update internal slot state.
        let ep0_state = EndpointState {
            dequeue_ptr: in_ep0.tr_dequeue_pointer(),
            cycle: in_ep0.dcs(),
            stopped: false,
            halted: false,
        };
        if let Some(slot) = self.internal_slot_mut(slot_id) {
            slot.root_port = Some(root_port);
            slot.route = route;
            slot.address = addr;
            if let Some(ep) = slot.endpoints.get_mut(1) {
                *ep = Some(ep0_state);
            }
        }

        // Apply Slot Context (preserve Slot State field).
        let in_slot_addr = input_ctx_ptr + INPUT_SLOT_CTX_OFFSET;
        let out_slot_addr = dev_ctx_ptr + DEVICE_SLOT_CTX_OFFSET;
        if let Err(code) = self.copy_slot_context_preserve_state(mem, in_slot_addr, out_slot_addr) {
            return code;
        }
        // Slot Context DW3 bits 0..=7 are the USB device address, which is assigned by the
        // controller. Ensure we reflect the newly allocated address in the output context.
        let out_slot_dw3_addr = out_slot_addr + 12;
        let out_dw3 = match self.read_u32(mem, out_slot_dw3_addr) {
            Ok(v) => v,
            Err(_) => return CompletionCode::ParameterError,
        };
        let out_dw3 = (out_dw3 & !0xff) | u32::from(addr);
        if self.write_u32(mem, out_slot_dw3_addr, out_dw3).is_err() {
            return CompletionCode::ParameterError;
        }

        // Apply EP0 Context (preserve Endpoint State field).
        let in_ep0_addr = input_ctx_ptr + INPUT_EP0_CTX_OFFSET;
        let out_ep0_addr = dev_ctx_ptr + DEVICE_EP0_CTX_OFFSET;
        if let Err(code) = self.copy_endpoint_context_preserve_state(mem, in_ep0_addr, out_ep0_addr) {
            return code;
        }

        CompletionCode::Success
    }

    fn handle_configure_endpoint(&mut self, mem: &mut dyn MemoryBus, cmd: Trb) -> CompletionCode {
        // Configure Endpoint (xHCI 1.2 §6.4.3.5).
        //
        // MVP: apply context add/drop flags for a small subset of endpoints:
        // - EP0 (Control)
        // - one Interrupt IN endpoint (HID) => Device Context index 3 (Endpoint 1 IN)
        // - one Bulk IN/OUT pair (WebUSB passthrough) => indices 4/5 (Endpoint 2 OUT/IN)
        let slot_id = cmd.slot_id();
        if slot_id == 0 || slot_id > self.max_slots {
            return CompletionCode::SlotNotEnabledError;
        }
        let idx = usize::from(slot_id);
        if !self.slots_enabled.get(idx).copied().unwrap_or(false) {
            return CompletionCode::SlotNotEnabledError;
        }
        if cmd.configure_endpoint_deconfigure() {
            // Deconfigure mode not supported yet.
            return CompletionCode::ParameterError;
        }

        // Configure Endpoint is only valid once Address Device has bound the slot to a topology.
        let (root_port, route) = {
            let Some(slot) = self.slots.get(idx).and_then(|s| s.as_ref()) else {
                return CompletionCode::SlotNotEnabledError;
            };
            let Some(root_port) = slot.root_port else {
                return CompletionCode::ContextStateError;
            };
            (root_port, slot.route.clone())
        };
        if self.find_device_by_topology(root_port, &route).is_none() {
            return CompletionCode::ContextStateError;
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(code) => return code,
        };

        let input_ctx_ptr = cmd.pointer();
        if (input_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        // Must be able to read at least the Input Control Context + Slot Context.
        let min_input_ctx_len = 2 * CONTEXT_SIZE;
        if !self.check_range(input_ctx_ptr, min_input_ctx_len) {
            return CompletionCode::ParameterError;
        }

        let input_ctx = InputContext32::new(input_ctx_ptr);
        let icc = input_ctx.input_control(mem);
        let drop_flags = icc.drop_flags();
        let add_flags = icc.add_flags();

        // Ensure the input context covers any endpoint contexts we're about to read.
        //
        // Input context layout (32-byte contexts):
        //   0: ICC
        //   1: Slot Context (DCI 0)
        //   2: EP0 (DCI 1)
        //   3: EP1 OUT (DCI 2)
        //   4: EP1 IN (DCI 3)
        //   5: EP2 OUT (DCI 4)
        //   6: EP2 IN (DCI 5)
        let mut required_input_contexts = 2u64; // ICC + Slot
        if (add_flags & ICC_CTX_FLAG_EP0) != 0 {
            required_input_contexts = required_input_contexts.max(3);
        }
        if (add_flags & ICC_CTX_FLAG_EP1_IN) != 0 {
            required_input_contexts = required_input_contexts.max(5);
        }
        if (add_flags & (ICC_CTX_FLAG_EP2_OUT | ICC_CTX_FLAG_EP2_IN)) != 0 {
            required_input_contexts = required_input_contexts.max(7);
        }
        if !self.check_range(input_ctx_ptr, required_input_contexts * CONTEXT_SIZE) {
            return CompletionCode::ParameterError;
        }

        let supported_add =
            ICC_CTX_FLAG_SLOT | ICC_CTX_FLAG_EP0 | ICC_CTX_FLAG_EP1_IN | ICC_CTX_FLAG_EP2_OUT | ICC_CTX_FLAG_EP2_IN;
        let supported_drop = ICC_CTX_FLAG_EP1_IN | ICC_CTX_FLAG_EP2_OUT | ICC_CTX_FLAG_EP2_IN;

        if (add_flags & !supported_add) != 0 {
            return CompletionCode::ParameterError;
        }
        if (drop_flags & !supported_drop) != 0 {
            return CompletionCode::ParameterError;
        }

        // For now, require bulk endpoints to be configured/dropped as a pair.
        let bulk_bits = ICC_CTX_FLAG_EP2_OUT | ICC_CTX_FLAG_EP2_IN;
        if (add_flags & bulk_bits) != 0 && (add_flags & bulk_bits) != bulk_bits {
            return CompletionCode::ParameterError;
        }
        if (drop_flags & bulk_bits) != 0 && (drop_flags & bulk_bits) != bulk_bits {
            return CompletionCode::ParameterError;
        }

        // Reject contradictory add+drop for the same context.
        if (add_flags & drop_flags) != 0 {
            return CompletionCode::ParameterError;
        }

        // We must be able to touch any contexts we drop/add in the output Device Context. Since the
        // MVP supports contexts up to index 5, require enough space for Slot + EP0 + endpoints up to
        // EP2 IN.
        let min_device_ctx_len = 6 * CONTEXT_SIZE;
        if !self.check_range(dev_ctx_ptr, min_device_ctx_len) {
            return CompletionCode::ParameterError;
        }

        // Validate all contexts to be added before mutating the output Device Context.
        let mut updates: Vec<(u8, Option<EndpointState>)> = Vec::new();

        if (add_flags & ICC_CTX_FLAG_SLOT) != 0 {
            let in_slot = match input_ctx.slot_context(mem) {
                Ok(ctx) => ctx,
                Err(_) => return CompletionCode::ParameterError,
            };
            if in_slot.parsed_route_string().is_err() {
                return CompletionCode::ParameterError;
            }
            if in_slot.root_hub_port_number() == 0 {
                return CompletionCode::ParameterError;
            }
        }

        if (add_flags & ICC_CTX_FLAG_EP0) != 0 {
            let in_ep0 = match input_ctx.endpoint_context(mem, 1) {
                Ok(ctx) => ctx,
                Err(_) => return CompletionCode::ParameterError,
            };
            if in_ep0.endpoint_type() != EndpointType::Control {
                return CompletionCode::ParameterError;
            }
            updates.push((
                1,
                Some(EndpointState {
                    dequeue_ptr: in_ep0.tr_dequeue_pointer(),
                    cycle: in_ep0.dcs(),
                    stopped: false,
                    halted: false,
                }),
            ));
        }

        if (add_flags & ICC_CTX_FLAG_EP1_IN) != 0 {
            let in_ep = match input_ctx.endpoint_context(mem, 3) {
                Ok(ctx) => ctx,
                Err(_) => return CompletionCode::ParameterError,
            };
            if in_ep.endpoint_type() != EndpointType::InterruptIn {
                return CompletionCode::ParameterError;
            }
            updates.push((
                3,
                Some(EndpointState {
                    dequeue_ptr: in_ep.tr_dequeue_pointer(),
                    cycle: in_ep.dcs(),
                    stopped: false,
                    halted: false,
                }),
            ));
        }

        if (add_flags & bulk_bits) != 0 {
            let in_out = match input_ctx.endpoint_context(mem, 4) {
                Ok(ctx) => ctx,
                Err(_) => return CompletionCode::ParameterError,
            };
            if in_out.endpoint_type() != EndpointType::BulkOut {
                return CompletionCode::ParameterError;
            }
            let in_in = match input_ctx.endpoint_context(mem, 5) {
                Ok(ctx) => ctx,
                Err(_) => return CompletionCode::ParameterError,
            };
            if in_in.endpoint_type() != EndpointType::BulkIn {
                return CompletionCode::ParameterError;
            }
            updates.push((
                4,
                Some(EndpointState {
                    dequeue_ptr: in_out.tr_dequeue_pointer(),
                    cycle: in_out.dcs(),
                    stopped: false,
                    halted: false,
                }),
            ));
            updates.push((
                5,
                Some(EndpointState {
                    dequeue_ptr: in_in.tr_dequeue_pointer(),
                    cycle: in_in.dcs(),
                    stopped: false,
                    halted: false,
                }),
            ));
        }

        // Apply drops first.
        for &idx in &[3u8, 4u8, 5u8] {
            if (drop_flags & (1u32 << idx)) != 0 {
                let out_addr = dev_ctx_ptr + (u64::from(idx) * CONTEXT_SIZE);
                if let Err(code) = self.clear_context(mem, out_addr) {
                    return code;
                }
                updates.push((idx, None));
            }
        }

        // Apply adds.
        if (add_flags & ICC_CTX_FLAG_SLOT) != 0 {
            let in_slot_addr = input_ctx_ptr + INPUT_SLOT_CTX_OFFSET;
            let out_slot_addr = dev_ctx_ptr + DEVICE_SLOT_CTX_OFFSET;
            if let Err(code) = self.copy_slot_context_preserve_state(mem, in_slot_addr, out_slot_addr) {
                return code;
            }
        }

        if (add_flags & ICC_CTX_FLAG_EP0) != 0 {
            let in_addr = input_ctx_ptr + INPUT_EP0_CTX_OFFSET;
            let out_addr = dev_ctx_ptr + DEVICE_EP0_CTX_OFFSET;
            if let Err(code) = self.copy_endpoint_context_preserve_state(mem, in_addr, out_addr) {
                return code;
            }
        }

        if (add_flags & ICC_CTX_FLAG_EP1_IN) != 0 {
            let in_addr = input_ctx_ptr + (4u64 * CONTEXT_SIZE); // input ctx index = dci + 1 => 3 + 1 = 4
            let out_addr = dev_ctx_ptr + (3u64 * CONTEXT_SIZE);
            if let Err(code) = self.copy_endpoint_context_preserve_state(mem, in_addr, out_addr) {
                return code;
            }
        }

        if (add_flags & bulk_bits) != 0 {
            let in_out_addr = input_ctx_ptr + (5u64 * CONTEXT_SIZE); // input index 5 = dci 4 + 1
            let out_out_addr = dev_ctx_ptr + (4u64 * CONTEXT_SIZE);
            if let Err(code) = self.copy_endpoint_context_preserve_state(mem, in_out_addr, out_out_addr) {
                return code;
            }

            let in_in_addr = input_ctx_ptr + (6u64 * CONTEXT_SIZE); // input index 6 = dci 5 + 1
            let out_in_addr = dev_ctx_ptr + (5u64 * CONTEXT_SIZE);
            if let Err(code) = self.copy_endpoint_context_preserve_state(mem, in_in_addr, out_in_addr) {
                return code;
            }
        }

        // Apply internal endpoint state updates after all memory writes succeed.
        if let Some(slot) = self.internal_slot_mut(slot_id) {
            for (dci, state) in updates {
                if let Some(ep) = slot.endpoints.get_mut(dci as usize) {
                    *ep = state;
                }
            }
        }

        CompletionCode::Success
    }

    fn update_ep0_context(
        &mut self,
        mem: &mut dyn MemoryBus,
        in_ep0: u64,
        out_ep0: u64,
    ) -> Result<(), CompletionCode> {
        let in_ctx = EndpointContext::read_from(mem, in_ep0);
        let mut out_ctx = EndpointContext::read_from(mem, out_ep0);

        out_ctx.set_interval(in_ctx.interval());
        out_ctx.set_max_packet_size(in_ctx.max_packet_size());
        // TR Dequeue Pointer (dwords 2-3): copy verbatim (includes DCS + reserved low bits).
        out_ctx.set_tr_dequeue_pointer_raw(in_ctx.tr_dequeue_pointer_raw());
        out_ctx.write_to(mem, out_ep0);

        Ok(())
    }

    fn copy_slot_context_preserve_state(
        &mut self,
        mem: &mut dyn MemoryBus,
        in_slot: u64,
        out_slot: u64,
    ) -> Result<(), CompletionCode> {
        for i in 0..8u64 {
            let in_dw = self
                .read_u32(mem, in_slot + i * 4)
                .map_err(|_| CompletionCode::ParameterError)?;
            let out_addr = out_slot + i * 4;
            let value = if i == 3 {
                let out_dw = self
                    .read_u32(mem, out_addr)
                    .map_err(|_| CompletionCode::ParameterError)?;
                (in_dw & !SLOT_STATE_MASK_DWORD3) | (out_dw & SLOT_STATE_MASK_DWORD3)
            } else {
                in_dw
            };
            self.write_u32(mem, out_addr, value)
                .map_err(|_| CompletionCode::ParameterError)?;
        }

        Ok(())
    }

    fn copy_endpoint_context_preserve_state(
        &mut self,
        mem: &mut dyn MemoryBus,
        in_ep: u64,
        out_ep: u64,
    ) -> Result<(), CompletionCode> {
        for i in 0..8u64 {
            let in_dw = self
                .read_u32(mem, in_ep + i * 4)
                .map_err(|_| CompletionCode::ParameterError)?;
            let out_addr = out_ep + i * 4;
            let value = if i == 0 {
                let out_dw = self
                    .read_u32(mem, out_addr)
                    .map_err(|_| CompletionCode::ParameterError)?;
                (in_dw & !EP_STATE_MASK_DWORD0) | (out_dw & EP_STATE_MASK_DWORD0)
            } else {
                in_dw
            };
            self.write_u32(mem, out_addr, value)
                .map_err(|_| CompletionCode::ParameterError)?;
        }

        Ok(())
    }

    fn clear_context(&mut self, mem: &mut dyn MemoryBus, ctx_addr: u64) -> Result<(), CompletionCode> {
        for i in 0..8u64 {
            self.write_u32(mem, ctx_addr + i * 4, 0)
                .map_err(|_| CompletionCode::ParameterError)?;
        }
        Ok(())
    }

    fn emit_command_completion(
        &mut self,
        mem: &mut dyn MemoryBus,
        command_trb_ptr: u64,
        code: CompletionCode,
        slot_id: u8,
    ) {
        let mut event = Trb::new(command_trb_ptr & !0x0f, u32::from(code.as_u8()) << 24, 0);
        event.set_trb_type(TrbType::CommandCompletionEvent);
        event.set_slot_id(slot_id);
        // If we fail to write to the event ring, this is a host controller error; but we must not
        // panic. Set HCE and stop processing further commands.
        if self.push_event(mem, event).is_err() {
            self.host_controller_error = true;
        }
    }

    fn push_event(&mut self, mem: &mut dyn MemoryBus, mut trb: Trb) -> Result<(), ()> {
        if self.event_ring.size_trbs == 0 {
            return Err(());
        }
        let addr = self.event_ring.entry_addr();
        if !self.check_range(addr, TRB_LEN as u64) {
            return Err(());
        }

        trb.set_cycle(self.event_ring.cycle_state);
        self.write_trb(mem, addr, trb).map_err(|_| ())?;
        self.event_ring.advance();
        Ok(())
    }

    fn check_range(&self, addr: u64, len: u64) -> bool {
        let end = match addr.checked_add(len) {
            Some(end) => end,
            None => return false,
        };
        end <= self.mem_size
    }

    fn read_trb(&self, mem: &mut dyn MemoryBus, addr: u64) -> Result<Trb, ()> {
        if !self.check_range(addr, TRB_LEN as u64) {
            return Err(());
        }
        let mut bytes = [0u8; TRB_LEN];
        mem.read_physical(addr, &mut bytes);
        Ok(Trb::from_bytes(bytes))
    }

    fn write_trb(&self, mem: &mut dyn MemoryBus, addr: u64, trb: Trb) -> Result<(), ()> {
        if !self.check_range(addr, TRB_LEN as u64) {
            return Err(());
        }
        mem.write_physical(addr, &trb.to_bytes());
        Ok(())
    }

    fn read_u32(&self, mem: &mut dyn MemoryBus, addr: u64) -> Result<u32, ()> {
        if !self.check_range(addr, 4) {
            return Err(());
        }
        let mut buf = [0u8; 4];
        mem.read_physical(addr, &mut buf);
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&self, mem: &mut dyn MemoryBus, addr: u64) -> Result<u64, ()> {
        if !self.check_range(addr, 8) {
            return Err(());
        }
        let mut buf = [0u8; 8];
        mem.read_physical(addr, &mut buf);
        Ok(u64::from_le_bytes(buf))
    }

    fn write_u32(&self, mem: &mut dyn MemoryBus, addr: u64, value: u32) -> Result<(), ()> {
        if !self.check_range(addr, 4) {
            return Err(());
        }
        mem.write_physical(addr, &value.to_le_bytes());
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TestMem {
        data: Vec<u8>,
    }

    impl TestMem {
        fn new(size: usize) -> Self {
            Self { data: vec![0; size] }
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

    #[test]
    fn endpoint_commands_update_internal_endpoint_state() {
        let mut mem = TestMem::new(0x20_000);
        let mem_size = mem.data.len() as u64;

        let dcbaa = 0x1000u64;
        let dev_ctx = 0x2000u64;
        let cmd_ring = 0x4000u64;
        let event_ring = 0x5000u64;

        let max_slots = 8;
        let slot_id = 1u8;
        let endpoint_id = 2u8; // EP1 OUT (DCI=2)
        let ep_ctx = dev_ctx + u64::from(endpoint_id) * CONTEXT_SIZE;

        // Seed endpoint context state + dequeue pointer (DCS=1).
        mem.write_u32(ep_ctx + 0, EP_STATE_RUNNING);
        mem.write_u32(ep_ctx + 8, 0x1110 | 1);
        mem.write_u32(ep_ctx + 12, 0);

        // Command ring:
        //  - TRB0: Enable Slot
        //  - TRB1: Stop Endpoint
        //  - TRB2: Set TR Dequeue Pointer (0x6000, DCS=0)
        //  - TRB3: Reset Endpoint
        {
            let mut trb0 = Trb::new(0, 0, 0);
            trb0.set_trb_type(TrbType::EnableSlotCommand);
            trb0.set_cycle(true);
            trb0.write_to(&mut mem, cmd_ring);
        }
        {
            let mut trb1 = Trb::new(0, 0, 0);
            trb1.set_trb_type(TrbType::StopEndpointCommand);
            trb1.set_cycle(true);
            trb1.set_slot_id(slot_id);
            trb1.set_endpoint_id(endpoint_id);
            trb1.write_to(&mut mem, cmd_ring + TRB_LEN as u64);
        }

        let new_trdp = 0x6000u64;
        {
            let mut trb2 = Trb::new(new_trdp, 0, 0);
            trb2.set_trb_type(TrbType::SetTrDequeuePointerCommand);
            trb2.set_cycle(true);
            trb2.set_slot_id(slot_id);
            trb2.set_endpoint_id(endpoint_id);
            trb2.write_to(&mut mem, cmd_ring + 2 * TRB_LEN as u64);
        }
        {
            let mut trb3 = Trb::new(0, 0, 0);
            trb3.set_trb_type(TrbType::ResetEndpointCommand);
            trb3.set_cycle(true);
            trb3.set_slot_id(slot_id);
            trb3.set_endpoint_id(endpoint_id);
            trb3.write_to(&mut mem, cmd_ring + 3 * TRB_LEN as u64);
        }

        let mut processor = CommandRingProcessor::new(
            mem_size,
            max_slots,
            dcbaa,
            CommandRing {
                dequeue_ptr: cmd_ring,
                cycle_state: true,
            },
            EventRing::new(event_ring, 16),
        );

        // Process Enable Slot so the processor allocates slot state. Enable Slot clears DCBAA[1] to
        // 0, so install the device context pointer after it completes.
        processor.process(&mut mem, 1);
        mem.write_u64(dcbaa + 8, dev_ctx);

        // Stop Endpoint should update guest memory and set the internal "stopped" flag.
        processor.process(&mut mem, 1);
        assert_eq!(mem.read_u32(ep_ctx + 0) & 0x7, EP_STATE_STOPPED);

        let slot = processor.slots[usize::from(slot_id)]
            .as_ref()
            .expect("slot state should exist after Enable Slot");
        let ep = slot.endpoints[usize::from(endpoint_id)]
            .as_ref()
            .expect("endpoint state should have been created by Stop Endpoint");
        assert_eq!(ep.dequeue_ptr, 0x1110);
        assert!(ep.cycle);
        assert!(ep.stopped);
        assert!(!ep.halted);

        // Simulate a transfer-ring error halting the endpoint; Set TR Dequeue Pointer must not
        // clear stopped/halted flags.
        processor.slots[usize::from(slot_id)]
            .as_mut()
            .unwrap()
            .endpoints[usize::from(endpoint_id)]
            .as_mut()
            .unwrap()
            .halted = true;

        processor.process(&mut mem, 1);

        let ctx = EndpointContext::read_from(&mut mem, ep_ctx);
        assert_eq!(ctx.tr_dequeue_pointer(), new_trdp);
        assert!(!ctx.dcs());

        let slot = processor.slots[usize::from(slot_id)].as_ref().unwrap();
        let ep = slot.endpoints[usize::from(endpoint_id)].as_ref().unwrap();
        assert_eq!(ep.dequeue_ptr, new_trdp);
        assert!(!ep.cycle);
        assert!(ep.stopped);
        assert!(ep.halted);

        // Reset Endpoint clears stopped/halted state but preserves the ring cursor.
        processor.process(&mut mem, 1);
        assert_eq!(mem.read_u32(ep_ctx + 0) & 0x7, EP_STATE_RUNNING);

        let slot = processor.slots[usize::from(slot_id)].as_ref().unwrap();
        let ep = slot.endpoints[usize::from(endpoint_id)].as_ref().unwrap();
        assert_eq!(ep.dequeue_ptr, new_trdp);
        assert!(!ep.cycle);
        assert!(!ep.stopped);
        assert!(!ep.halted);
    }
}
