use crate::MemoryBus;

use super::context::EndpointContext;
use super::trb::{CompletionCode, Trb, TrbType, TRB_LEN};
use alloc::vec;
use alloc::vec::Vec;

const CONTEXT_ALIGN: u64 = 64;
const CONTEXT_SIZE: u64 = 32;

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

const SLOT_STATE_MASK_DWORD3: u32 = 0xF800_0000;

const ICC_DROP_FLAGS_OFFSET: u64 = 0;
const ICC_ADD_FLAGS_OFFSET: u64 = 4;

const ICC_CTX_FLAG_SLOT: u32 = 1 << 0;
const ICC_CTX_FLAG_EP0: u32 = 1 << 1;

const EP_STATE_RUNNING: u32 = 1;
const EP_STATE_STOPPED: u32 = 3;

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

    /// Set when we detect a fatal error (e.g. malformed ring pointers).
    pub host_controller_error: bool,
}

impl CommandRingProcessor {
    pub fn new(mem_size: u64, max_slots: u8, dcbaa_ptr: u64, command_ring: CommandRing, event_ring: EventRing) -> Self {
        Self {
            mem_size,
            max_slots,
            dcbaa_ptr,
            slots_enabled: vec![false; usize::from(max_slots).saturating_add(1)],
            command_ring,
            event_ring,
            host_controller_error: false,
        }
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
            self.slots_enabled
                .get(usize::from(id))
                .copied()
                .unwrap_or(false)
                == false
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

        (CompletionCode::Success, slot_id)
    }

    fn handle_disable_slot(&mut self, mem: &mut dyn MemoryBus, slot_id: u8) -> CompletionCode {
        if slot_id == 0 || slot_id > self.max_slots {
            return CompletionCode::ParameterError;
        }
        let idx = usize::from(slot_id);
        let enabled = self.slots_enabled.get(idx).copied().unwrap_or(false);
        if !enabled {
            return CompletionCode::SlotNotEnabledError;
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
        &mut self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
    ) -> Result<u64, CompletionCode> {
        if slot_id == 0 || slot_id > self.max_slots {
            return Err(CompletionCode::SlotNotEnabledError);
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
            return Err(CompletionCode::SlotNotEnabledError);
        }
        if (dev_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return Err(CompletionCode::ParameterError);
        }

        Ok(dev_ctx_ptr)
    }

    fn endpoint_context_addr(
        &mut self,
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
        ctx.set_endpoint_state(EP_STATE_STOPPED as u8);
        ctx.write_to(mem, ep_ctx);
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
        // MVP: clear halted/stopped and allow transfers again.
        ctx.set_endpoint_state(EP_STATE_RUNNING as u8);
        ctx.write_to(mem, ep_ctx);
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

        let ptr = cmd.parameter & !0x0f;
        let dcs = cmd.parameter & 0x01;
        if ptr == 0 {
            return CompletionCode::ParameterError;
        }
        if !self.check_range(ptr, TRB_LEN as u64) {
            return CompletionCode::ParameterError;
        }
        let dcs = dcs != 0;
        let mut ctx = EndpointContext::read_from(mem, ep_ctx);
        ctx.set_tr_dequeue_pointer(ptr, dcs);
        ctx.write_to(mem, ep_ctx);

        // MVP: preserve existing endpoint state (typically Stopped) and only update the dequeue
        // pointer + DCS.
        CompletionCode::Success
    }

    fn handle_evaluate_context(&mut self, mem: &mut dyn MemoryBus, cmd: Trb) -> CompletionCode {
        let slot_id = cmd.slot_id();
        if slot_id == 0 || slot_id > self.max_slots {
            return CompletionCode::SlotNotEnabledError;
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

        let dev_ctx_ptr = match self.read_u64(mem, dcbaa_entry_addr) {
            Ok(v) => v,
            Err(_) => return CompletionCode::ParameterError,
        };
        if dev_ctx_ptr == 0 {
            return CompletionCode::SlotNotEnabledError;
        }
        if (dev_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        // We must be able to touch at least Slot + EP0 contexts.
        let min_device_ctx_len = (2 * CONTEXT_SIZE) as u64;
        if !self.check_range(dev_ctx_ptr, min_device_ctx_len) {
            return CompletionCode::ParameterError;
        }

        let input_ctx_ptr = cmd.pointer();
        if (input_ctx_ptr & (CONTEXT_ALIGN - 1)) != 0 {
            return CompletionCode::ParameterError;
        }

        // Must be able to read at least ICC + Slot + EP0 contexts.
        let min_input_ctx_len = (3 * CONTEXT_SIZE) as u64;
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
