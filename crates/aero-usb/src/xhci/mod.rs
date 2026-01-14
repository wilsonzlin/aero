//! xHCI (USB 3.x) host controller scaffolding.
//!
//! Aero's canonical USB stack lives in `aero-usb`. Today the project ships a UHCI host controller
//! model that is sufficient for Windows 7's in-box USB/HID drivers. xHCI support is being added
//! incrementally.
//!
//! The primary consumers of this module are:
//! - The xHCI controller MMIO/PCI integration in `crates/emulator` (`emulator::io::usb::xhci`)
//! - Unit tests for the core xHCI data structures (`trb`, `ring`, `context`)
//!
//! The controller implementation here is intentionally small; it currently provides:
//! - a minimal MMIO register file with basic size/unaligned access support
//! - a DMA read on the first transition of `USBCMD.RUN` (to validate PCI BME gating in the wrapper)
//! - a level-triggered `irq_level()` surface (to validate PCI INTx disable gating)
//! - DCBAAP register storage + controller-local slot allocation (Enable Slot scaffolding)
//! - a minimal runtime interrupter 0 register block + guest event ring producer (ERST-based)
//!
//! Full xHCI semantics (doorbells, command/event rings, device contexts, interrupters, etc) remain
//! future work.
//!
//! In addition:
//! - `command_ring` provides a minimal command ring + event ring processor used by unit tests and
//!   early enumeration harnesses.
//! - `command` provides an MVP endpoint-management state machine (Stop/Reset Endpoint + Set TR
//!   Dequeue Pointer) with doorbell gating semantics.
//! - `transfer` provides:
//!   - a small, deterministic transfer-ring executor that can process Normal TRBs for non-control
//!     endpoints (sufficient for HID interrupt IN/OUT)
//!   - a minimal control-transfer engine for endpoint 0 (Setup/Data/Status TRBs) used by unit tests
//!     and early emulator integration.
//!
//! Finally, this module models a tiny root hub (USB2 ports only) and generates Port Status Change
//! Event TRBs when devices connect/disconnect or a port reset completes.

pub mod command;
pub mod command_ring;
pub mod context;
mod event_ring;
pub mod interrupter;
pub mod regs;
pub mod ring;
pub mod transfer;
pub mod trb;

mod port;

pub use regs::{PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_PR, PORTSC_PRC};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::fmt;

use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use crate::device::{AttachedUsbDevice, UsbInResult, UsbOutResult};
use crate::hub::UsbHubDevice;
use crate::{MemoryBus, SetupPacket, UsbDeviceModel, UsbHubAttachError};

use self::port::XhciPort;
use self::trb::{CompletionCode, Trb, TrbType};

use event_ring::EventRingProducer;
use interrupter::InterrupterRegs;

// Most PC xHCI controllers expose multiple root ports. Use a reasonably sized default so common
// guest drivers see a realistic topology without requiring explicit configuration.
const DEFAULT_PORT_COUNT: u8 = 8;
const MAX_PENDING_EVENTS: usize = 256;
const COMPLETION_CODE_SUCCESS: u8 = 1;
const MAX_TRBS_PER_TICK: usize = 256;
const RING_STEP_BUDGET: usize = 64;
const MAX_CONTROL_DATA_LEN: usize = 64 * 1024;

/// Maximum number of event TRBs written into the guest event ring per controller tick.
pub const EVENT_ENQUEUE_BUDGET_PER_TICK: usize = 64;

const COMMAND_BUDGET_PER_MMIO: usize = 16;
const COMMAND_RING_STEP_BUDGET: usize = 64;

use self::context::{
    Dcbaa, DeviceContext32, EndpointContext, InputControlContext, SlotContext, CONTEXT_SIZE,
};
use self::ring::{RingCursor, RingPoll};

/// xHCI command completion codes (subset).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommandCompletionCode {
    Success,
    NoSlotsAvailableError,
    ParameterError,
    ContextStateError,
    Unknown(u8),
}

impl CommandCompletionCode {
    #[inline]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Success,
            9 => Self::NoSlotsAvailableError,
            17 => Self::ParameterError,
            19 => Self::ContextStateError,
            other => Self::Unknown(other),
        }
    }

    #[inline]
    pub const fn raw(self) -> u8 {
        match self {
            Self::Success => 1,
            Self::NoSlotsAvailableError => 9,
            Self::ParameterError => 17,
            Self::ContextStateError => 19,
            Self::Unknown(raw) => raw,
        }
    }
}

/// Result of completing an xHCI command.
///
/// In hardware this data is typically delivered via a Command Completion Event TRB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandCompletion {
    pub completion_code: CommandCompletionCode,
    pub slot_id: u8,
}

impl CommandCompletion {
    #[inline]
    pub const fn success(slot_id: u8) -> Self {
        Self {
            completion_code: CommandCompletionCode::Success,
            slot_id,
        }
    }

    #[inline]
    pub const fn failure(code: CommandCompletionCode) -> Self {
        Self {
            completion_code: code,
            slot_id: 0,
        }
    }
}

/// Per-slot state tracked by the controller.
#[derive(Debug, Clone)]
pub struct SlotState {
    enabled: bool,
    port_id: Option<u8>,
    device_attached: bool,

    /// Guest physical address of the Device Context structure, as stored in DCBAAP.
    device_context_ptr: u64,

    /// Shadow context state (mirrors guest memory once Address Device/Configure Endpoint are
    /// implemented).
    slot_context: SlotContext,
    endpoint_contexts: [EndpointContext; 31],

    /// Per-endpoint transfer ring cursors indexed by Endpoint Context ID (1..=31).
    transfer_rings: [Option<RingCursor>; 31],
}

impl Default for SlotState {
    fn default() -> Self {
        Self {
            enabled: false,
            port_id: None,
            device_attached: false,
            device_context_ptr: 0,
            slot_context: SlotContext::default(),
            endpoint_contexts: [EndpointContext::default(); 31],
            transfer_rings: [None; 31],
        }
    }
}

impl SlotState {
    pub fn enabled(&self) -> bool {
        self.enabled
    }

    pub fn port_id(&self) -> Option<u8> {
        self.port_id
    }

    pub fn device_attached(&self) -> bool {
        self.device_attached
    }

    pub fn device_context_ptr(&self) -> u64 {
        self.device_context_ptr
    }

    pub fn slot_context(&self) -> &SlotContext {
        &self.slot_context
    }

    pub fn endpoint_context(&self, idx: usize) -> Option<&EndpointContext> {
        self.endpoint_contexts.get(idx)
    }

    /// Returns the transfer ring cursor for the given Endpoint Context ID (1..=31).
    pub fn transfer_ring(&self, endpoint_id: u8) -> Option<RingCursor> {
        let idx = endpoint_id.checked_sub(1)? as usize;
        self.transfer_rings.get(idx).copied().flatten()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ActiveEndpoint {
    slot_id: u8,
    endpoint_id: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ControlTdState {
    data_expected: usize,
    data_transferred: usize,
    completion_code: CompletionCode,
}

impl Default for ControlTdState {
    fn default() -> Self {
        Self {
            data_expected: 0,
            data_transferred: 0,
            completion_code: CompletionCode::Success,
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct EndpointOutcome {
    trbs_consumed: usize,
    keep_active: bool,
}

impl EndpointOutcome {
    fn idle() -> Self {
        Self {
            trbs_consumed: 0,
            keep_active: false,
        }
    }

    fn keep(trbs_consumed: usize) -> Self {
        Self {
            trbs_consumed,
            keep_active: true,
        }
    }

    fn done(trbs_consumed: usize) -> Self {
        Self {
            trbs_consumed,
            keep_active: false,
        }
    }
}

/// Minimal xHCI controller model.
///
/// This is *not* a full xHCI implementation. It is sufficient to wire into a PCI/MMIO wrapper and
/// to host unit tests that need a stable controller surface.
pub struct XhciController {
    port_count: u8,
    ext_caps: Vec<u32>,

    // Minimal MMIO-visible register file for emulator PCI/MMIO integration.
    usbcmd: u32,
    usbsts: u32,
    /// Sticky host controller error latch surfaced via USBSTS.HCE.
    host_controller_error: bool,
    crcr: u64,
    dcbaap: u64,
    slots: Vec<SlotState>,

    // Root hub ports.
    ports: Vec<XhciPort>,

    // Command ring cursor + doorbell kick.
    //
    // Guest software programs CRCR with a dequeue pointer + cycle state and rings doorbell 0 to
    // notify the controller. We keep a small "kick" flag so command processing can continue across
    // subsequent MMIO accesses without requiring the guest to ring doorbell 0 for every TRB.
    command_ring: Option<RingCursor>,
    cmd_kick: bool,

    // Runtime registers: interrupter 0 + guest event ring delivery.
    interrupter0: InterrupterRegs,
    event_ring: EventRingProducer,

    // Host-side event buffering.
    pending_events: VecDeque<Trb>,
    dropped_event_trbs: u64,

    // --- Endpoint transfer execution (subset) ---
    active_endpoints: Vec<ActiveEndpoint>,
    ep0_control_td: Vec<ControlTdState>,
}

impl fmt::Debug for XhciController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XhciController")
            .field("port_count", &self.port_count)
            .field("ext_caps_dwords", &self.ext_caps.len())
            .field("usbcmd", &self.usbcmd)
            .field("usbsts", &self.usbsts)
            .field("host_controller_error", &self.host_controller_error)
            .field("crcr", &self.crcr)
            .field("dcbaap", &self.dcbaap)
            .field("slots", &self.slots.len())
            .field("command_ring", &self.command_ring)
            .field("cmd_kick", &self.cmd_kick)
            .field("pending_events", &self.pending_events.len())
            .field("dropped_event_trbs", &self.dropped_event_trbs)
            .field("interrupter0", &self.interrupter0)
            .field("active_endpoints", &self.active_endpoints.len())
            .finish()
    }
}

impl Default for XhciController {
    fn default() -> Self {
        Self::with_port_count(DEFAULT_PORT_COUNT)
    }
}

impl XhciController {
    /// Size of the MMIO BAR exposed by the emulator integration.
    ///
    /// Real xHCI controllers expose a 64KiB MMIO window. The current model only implements a small
    /// subset of the architectural register set, but we still reserve the full window so PCI BAR
    /// probing/alignment matches the canonical PCI profile and the Web runtime device wrapper.
    pub const MMIO_SIZE: u32 = 0x10000;

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_port_count(port_count: u8) -> Self {
        assert!(port_count > 0, "xHCI controller must expose at least one port");
        const DEFAULT_MAX_SLOTS: usize = 32;
        let slots: Vec<SlotState> = core::iter::repeat_with(SlotState::default)
            .take(DEFAULT_MAX_SLOTS + 1)
            .collect();
        let slot_count = slots.len();
        let mut ctrl = Self {
            port_count,
            ext_caps: Vec::new(),
            usbcmd: 0,
            usbsts: 0,
            host_controller_error: false,
            crcr: 0,
            dcbaap: 0,
            slots,
            cmd_kick: false,
            ports: (0..port_count).map(|_| XhciPort::new()).collect(),
            command_ring: None,
            interrupter0: InterrupterRegs::default(),
            event_ring: EventRingProducer::default(),
            pending_events: VecDeque::new(),
            dropped_event_trbs: 0,
            active_endpoints: Vec::new(),
            ep0_control_td: vec![ControlTdState::default(); slot_count],
        };

        ctrl.rebuild_ext_caps();
        ctrl
    }

    pub fn port_count(&self) -> u8 {
        self.port_count
    }

    /// Attach a USB device model at a host-visible topology path.
    ///
    /// Path numbering matches the `hub::RootHub` contract used by UHCI:
    /// - `path[0]` is the root port index (0-based).
    /// - `path[1..]` are downstream hub ports (1-based, per USB spec).
    pub fn attach_at_path(
        &mut self,
        path: &[u8],
        model: Box<dyn UsbDeviceModel>,
    ) -> Result<(), UsbHubAttachError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        let root_port = root_port as usize;
        if root_port >= self.ports.len() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        // xHCI Slot Context Route Strings encode downstream hub port numbers in 4-bit nibbles,
        // limiting reachable ports to 1..=15 and limiting depth to 5 hub tiers.
        if rest.len() > context::XHCI_ROUTE_STRING_MAX_DEPTH {
            return Err(UsbHubAttachError::InvalidPort);
        }
        for &hop in rest {
            if hop == 0 || hop > context::XHCI_ROUTE_STRING_MAX_PORT {
                return Err(UsbHubAttachError::InvalidPort);
            }
        }

        if rest.is_empty() {
            if self.ports[root_port].has_device() {
                return Err(UsbHubAttachError::PortOccupied);
            }
            self.attach_device(root_port, model);
            return Ok(());
        }

        let Some(root_dev) = self.ports[root_port].device_mut() else {
            return Err(UsbHubAttachError::NoDevice);
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev = root_dev;
        for &hop in hub_path {
            hub_dev = hub_dev.model_mut().hub_port_device_mut(hop)?;
        }
        hub_dev.model_mut().hub_attach_device(leaf_port, model)
    }

    /// Detach any USB device model at a host-visible topology path.
    pub fn detach_at_path(&mut self, path: &[u8]) -> Result<(), UsbHubAttachError> {
        let Some((&root_port, rest)) = path.split_first() else {
            return Err(UsbHubAttachError::InvalidPort);
        };
        let root_port = root_port as usize;
        if root_port >= self.ports.len() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        if rest.len() > context::XHCI_ROUTE_STRING_MAX_DEPTH {
            return Err(UsbHubAttachError::InvalidPort);
        }
        for &hop in rest {
            if hop == 0 || hop > context::XHCI_ROUTE_STRING_MAX_PORT {
                return Err(UsbHubAttachError::InvalidPort);
            }
        }

        if rest.is_empty() {
            if !self.ports[root_port].has_device() {
                return Err(UsbHubAttachError::NoDevice);
            }
            self.detach_device(root_port);
            return Ok(());
        }

        let Some(root_dev) = self.ports[root_port].device_mut() else {
            return Err(UsbHubAttachError::NoDevice);
        };

        let (&leaf_port, hub_path) = rest.split_last().expect("rest is non-empty");
        let mut hub_dev = root_dev;
        for &hop in hub_path {
            hub_dev = hub_dev.model_mut().hub_port_device_mut(hop)?;
        }
        hub_dev.model_mut().hub_detach_device(leaf_port)
    }

    /// Convenience helper for attaching an external USB hub device to a root port.
    pub fn attach_hub(&mut self, root_port: u8, port_count: u8) -> Result<(), UsbHubAttachError> {
        if port_count == 0 {
            return Err(UsbHubAttachError::InvalidPort);
        }
        // Clamp to the maximum downstream port number representable in a Route String nibble.
        let port_count = port_count.min(context::XHCI_ROUTE_STRING_MAX_PORT);
        let root_port = root_port as usize;
        if root_port >= self.ports.len() {
            return Err(UsbHubAttachError::InvalidPort);
        }
        self.attach_device(root_port, Box::new(UsbHubDevice::with_port_count(port_count)));
        Ok(())
    }

    pub fn mmio_read_u32(&mut self, mem: &mut dyn MemoryBus, offset: u64) -> u32 {
        self.mmio_read(mem, offset, 4)
    }

    /// Returns the currently configured DCBAAP base (64-byte aligned), if set.
    pub fn dcbaap(&self) -> Option<u64> {
        if self.dcbaap == 0 {
            None
        } else {
            Some(self.dcbaap)
        }
    }

    /// Store the guest-provided DCBAAP pointer.
    ///
    /// The base address is 64-byte aligned; low bits are masked away.
    pub fn set_dcbaap(&mut self, paddr: u64) {
        self.dcbaap = paddr & !0x3f;
    }

    fn dcbaap_entry_paddr(&self, slot_id: u8) -> Option<u64> {
        let base = self.dcbaap()?;
        base.checked_add((slot_id as u64).checked_mul(8)?)
    }

    /// Return the slot state for `slot_id` if the slot is currently enabled.
    pub fn slot_state(&self, slot_id: u8) -> Option<&SlotState> {
        let idx = usize::from(slot_id);
        let state = self.slots.get(idx)?;
        if state.enabled {
            Some(state)
        } else {
            None
        }
    }

    /// Enable a new device slot.
    ///
    /// This is the core of handling an Enable Slot Command TRB. It allocates the lowest available
    /// slot ID and initialises controller-local state.
    ///
    /// The method is defensive: missing/zero DCBAAP returns a completion code instead of panicking.
    pub fn enable_slot<M: MemoryBus + ?Sized>(&mut self, mem: &mut M) -> CommandCompletion {
        if self.dcbaap().is_none() {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let slot_id = match (1u8..)
            .take(self.slots.len().saturating_sub(1))
            .find(|&id| !self.slots[usize::from(id)].enabled)
        {
            Some(id) => id,
            None => return CommandCompletion::failure(CommandCompletionCode::NoSlotsAvailableError),
        };

        let Some(entry_addr) = self.dcbaap_entry_paddr(slot_id) else {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        };

        // Initialise DCBAAP entry. This is safe even if the guest already zeroed it.
        mem.write_physical(entry_addr, &0u64.to_le_bytes());

        let slot = &mut self.slots[usize::from(slot_id)];
        *slot = SlotState {
            enabled: true,
            ..SlotState::default()
        };

        CommandCompletion::success(slot_id)
    }

    fn disable_slot<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, slot_id: u8) -> CompletionCode {
        let idx = usize::from(slot_id);
        if idx == 0 || idx >= self.slots.len() {
            return CompletionCode::ParameterError;
        }
        if !self.slots[idx].enabled {
            return CompletionCode::SlotNotEnabledError;
        }

        if let Some(entry) = self.dcbaap_entry_paddr(slot_id) {
            mem.write_physical(entry, &0u64.to_le_bytes());
        }

        self.slots[idx] = SlotState::default();
        CompletionCode::Success
    }

    /// Configure the command ring cursor (dequeue pointer + cycle state).
    ///
    /// This is a host-side harness used by unit tests and early bring-up while a full guest-facing
    /// command ring model is still in flux.
    pub fn set_command_ring(&mut self, dequeue_ptr: u64, cycle: bool) {
        self.command_ring = Some(RingCursor::new(dequeue_ptr, cycle));
    }

    /// Process up to `max_trbs` command TRBs from the configured command ring.
    ///
    /// Returns `true` when the ring appears empty (cycle mismatch or fatal ring error).
    pub fn process_command_ring<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, max_trbs: usize) -> bool {
        let Some(mut cursor) = self.command_ring else {
            return true;
        };

        for _ in 0..max_trbs {
            match cursor.poll(mem, COMMAND_RING_STEP_BUDGET) {
                RingPoll::Ready(item) => self.handle_command(mem, item.paddr, item.trb),
                RingPoll::NotReady => {
                    self.command_ring = Some(cursor);
                    return true;
                }
                RingPoll::Err(_) => {
                    self.command_ring = Some(cursor);
                    return true;
                }
            }
        }

        self.command_ring = Some(cursor);
        false
    }

    fn handle_command<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        match trb.trb_type() {
            TrbType::EnableSlotCommand => self.cmd_enable_slot(mem, cmd_paddr),
            TrbType::DisableSlotCommand => self.cmd_disable_slot(mem, cmd_paddr, trb),
            TrbType::AddressDeviceCommand => self.cmd_address_device(mem, cmd_paddr, trb),
            TrbType::NoOpCommand => self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::Success,
                trb.slot_id(),
            ),
            _ => self.queue_command_completion_event(cmd_paddr, CompletionCode::TrbError, trb.slot_id()),
        }
    }

    fn cmd_enable_slot<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64) {
        let result = self.enable_slot(mem);
        let (code, slot_id) = match result.completion_code {
            CommandCompletionCode::Success => (CompletionCode::Success, result.slot_id),
            CommandCompletionCode::ContextStateError => (CompletionCode::ContextStateError, 0),
            CommandCompletionCode::ParameterError => (CompletionCode::ParameterError, 0),
            CommandCompletionCode::NoSlotsAvailableError => (CompletionCode::NoSlotsAvailableError, 0),
            CommandCompletionCode::Unknown(_) => (CompletionCode::TrbError, 0),
        };
        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn cmd_disable_slot<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        let slot_id = trb.slot_id();
        let code = self.disable_slot(mem, slot_id);
        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn cmd_address_device<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        const CONTROL_BSR: u32 = 1 << 9;
        let slot_id = trb.slot_id();
        let slot_idx = usize::from(slot_id);

        if slot_id == 0 || slot_idx >= self.slots.len() || !self.slots[slot_idx].enabled {
            self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::SlotNotEnabledError,
                slot_id,
            );
            return;
        }

        // Input Context Pointer is 64-byte aligned; low bits are reserved.
        let input_ctx_raw = trb.parameter;
        let input_ctx_ptr = input_ctx_raw & !0x3f;
        if input_ctx_ptr == 0 || (input_ctx_raw & 0x3f) != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        let bsr = (trb.control & CONTROL_BSR) != 0;

        let icc = InputControlContext::read_from(mem, input_ctx_ptr);
        if icc.drop_flags() != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        const REQUIRED_ADD: u32 = 0b11;
        if icc.add_flags() != REQUIRED_ADD {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        let mut slot_ctx = SlotContext::read_from(mem, input_ctx_ptr + CONTEXT_SIZE as u64);
        let ep0_ctx = EndpointContext::read_from(mem, input_ctx_ptr + (2 * CONTEXT_SIZE) as u64);

        let port_id = slot_ctx.root_hub_port_number();
        if port_id == 0 || port_id > self.port_count {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        let route = match slot_ctx
            .parsed_route_string()
            .map(|rs| rs.ports_from_root())
        {
            Ok(route) => route,
            Err(_) => {
                self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
                return;
            }
        };

        let expected_speed = match self.find_device_by_topology(port_id, &route) {
            Some(dev) => {
                if !bsr {
                    // xHCI Address Device performs a SET_ADDRESS request on the default control
                    // endpoint. Aero's `AttachedUsbDevice` virtualizes this request internally.
                    let set_address = SetupPacket {
                        bm_request_type: 0x00, // HostToDevice | Standard | Device
                        b_request: 0x05,       // SET_ADDRESS
                        w_value: slot_id as u16,
                        w_index: 0,
                        w_length: 0,
                    };
                    match dev.handle_setup(set_address) {
                        UsbOutResult::Ack => {}
                        UsbOutResult::Nak => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::UsbTransactionError,
                                slot_id,
                            );
                            return;
                        }
                        UsbOutResult::Stall => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::StallError,
                                slot_id,
                            );
                            return;
                        }
                        UsbOutResult::Timeout => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::UsbTransactionError,
                                slot_id,
                            );
                            return;
                        }
                    }

                    match dev.handle_in(0, 0) {
                        UsbInResult::Data(data) if data.is_empty() => {}
                        UsbInResult::Nak => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::UsbTransactionError,
                                slot_id,
                            );
                            return;
                        }
                        UsbInResult::Stall => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::StallError,
                                slot_id,
                            );
                            return;
                        }
                        UsbInResult::Timeout => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::UsbTransactionError,
                                slot_id,
                            );
                            return;
                        }
                        UsbInResult::Data(_) => {
                            self.queue_command_completion_event(
                                cmd_paddr,
                                CompletionCode::TrbError,
                                slot_id,
                            );
                            return;
                        }
                    }
                }

                port::port_speed_id(dev.speed())
            }
            None => {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ContextStateError,
                    slot_id,
                );
                return;
            }
        };
        slot_ctx.set_speed(expected_speed);

        let Some(dcbaa_entry) = self.dcbaap_entry_paddr(slot_id) else {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ContextStateError, slot_id);
            return;
        };
        let dev_ctx_raw = mem.read_u64(dcbaa_entry);
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ContextStateError, slot_id);
            return;
        }

        // Mirror contexts to the output Device Context.
        slot_ctx.write_to(mem, dev_ctx_ptr);
        ep0_ctx.write_to(mem, dev_ctx_ptr + CONTEXT_SIZE as u64);

        let slot_state = &mut self.slots[slot_idx];
        slot_state.port_id = Some(port_id);
        slot_state.device_attached = true;
        slot_state.device_context_ptr = dev_ctx_ptr;
        slot_state.slot_context = slot_ctx;
        slot_state.endpoint_contexts[0] = ep0_ctx;
        slot_state.transfer_rings[0] =
            Some(RingCursor::new(ep0_ctx.tr_dequeue_pointer(), ep0_ctx.dcs()));

        self.queue_command_completion_event(cmd_paddr, CompletionCode::Success, slot_id);
    }

    fn queue_command_completion_event(
        &mut self,
        command_trb_ptr: u64,
        code: CompletionCode,
        slot_id: u8,
    ) {
        let mut trb = Trb::new(
            command_trb_ptr & !0x0f,
            (u32::from(code.as_u8())) << Trb::STATUS_COMPLETION_CODE_SHIFT,
            0,
        );
        trb.set_cycle(true);
        trb.set_trb_type(TrbType::CommandCompletionEvent);
        trb.set_slot_id(slot_id);
        self.post_event(trb);
    }

    /// Resolve an attached USB device by xHCI topology information.
    ///
    /// - `root_port` is the xHCI Root Hub Port Number (1-based).
    /// - `route` is the decoded Route String, where each element is a 1-based hub port number.
    pub fn find_device_by_topology(
        &mut self,
        root_port: u8,
        route: &[u8],
    ) -> Option<&mut AttachedUsbDevice> {
        if root_port == 0 {
            return None;
        }
        if route.len() > context::XHCI_ROUTE_STRING_MAX_DEPTH {
            return None;
        }

        let root_index = usize::from(root_port.checked_sub(1)?);
        let mut dev = self.ports.get_mut(root_index)?.device_mut()?;
        for &hop in route {
            if hop == 0 || hop > context::XHCI_ROUTE_STRING_MAX_PORT {
                return None;
            }
            dev = dev.model_mut().hub_port_device_mut(hop).ok()?;
        }
        Some(dev)
    }

    /// Topology-only Address Device handling.
    ///
    /// This does not implement full xHCI semantics yet, but it does resolve the slot's
    /// `RootHubPortNumber` + `RouteString` to a concrete [`AttachedUsbDevice`] behind an external hub
    /// (if present) and stores the Slot Context in controller-local state.
    pub fn address_device(&mut self, slot_id: u8, slot_ctx: SlotContext) -> CommandCompletion {
        let idx = usize::from(slot_id);
        if idx == 0 || idx >= self.slots.len() {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }
        if !self.slots[idx].enabled {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let route = match slot_ctx
            .parsed_route_string()
            .map(|rs| rs.ports_from_root())
        {
            Ok(route) => route,
            Err(_) => return CommandCompletion::failure(CommandCompletionCode::ParameterError),
        };
        let root_port = slot_ctx.root_hub_port_number();

        if self.find_device_by_topology(root_port, &route).is_none() {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let slot = &mut self.slots[idx];
        slot.port_id = Some(root_port);
        slot.device_attached = true;
        slot.slot_context = slot_ctx;

        CommandCompletion::success(slot_id)
    }

    /// Address Device using an Input Context pointer in guest memory.
    ///
    /// This is a thin wrapper that reads the Slot Context from the input context (32-byte
    /// contexts, `HCCPARAMS1.CSZ = 0`) and then applies the same topology binding as
    /// [`XhciController::address_device`].
    ///
    /// Note: This does **not** implement full xHCI Address Device semantics yet; it exists so test
    /// harnesses and future command ring plumbing can use the architectural input-context format.
    pub fn address_device_input_context(
        &mut self,
        mem: &mut impl MemoryBus,
        slot_id: u8,
        input_ctx_ptr: u64,
    ) -> CommandCompletion {
        // xHCI spec: input contexts are 64-byte aligned.
        if (input_ctx_ptr & 0x3f) != 0 {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        let input_ctx = context::InputContext32::new(input_ctx_ptr);
        let slot_ctx = match input_ctx.slot_context(mem) {
            Ok(ctx) => ctx,
            Err(_) => return CommandCompletion::failure(CommandCompletionCode::ParameterError),
        };
        self.address_device(slot_id, slot_ctx)
    }

    /// Topology-only Configure Endpoint handling.
    ///
    /// For now, configuring endpoints is equivalent to re-validating that the slot context still
    /// resolves to a reachable device.
    pub fn configure_endpoint(&mut self, slot_id: u8, slot_ctx: SlotContext) -> CommandCompletion {
        self.address_device(slot_id, slot_ctx)
    }

    /// Configure Endpoint using an Input Context pointer in guest memory.
    ///
    /// Like [`XhciController::address_device_input_context`], this is a thin wrapper that reads the
    /// Slot Context from guest memory and re-validates topology. Full endpoint configuration state
    /// machines are future work.
    pub fn configure_endpoint_input_context(
        &mut self,
        mem: &mut impl MemoryBus,
        slot_id: u8,
        input_ctx_ptr: u64,
    ) -> CommandCompletion {
        if (input_ctx_ptr & 0x3f) != 0 {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        let input_ctx = context::InputContext32::new(input_ctx_ptr);
        let slot_ctx = match input_ctx.slot_context(mem) {
            Ok(ctx) => ctx,
            Err(_) => return CommandCompletion::failure(CommandCompletionCode::ParameterError),
        };
        self.configure_endpoint(slot_id, slot_ctx)
    }

    fn read_device_context_ptr(
        &self,
        mem: &mut impl MemoryBus,
        slot_id: u8,
    ) -> Result<u64, CommandCompletionCode> {
        let dcbaap = self
            .dcbaap()
            .ok_or(CommandCompletionCode::ContextStateError)?;
        let dcbaa = Dcbaa::new(dcbaap);
        let dev_ctx_ptr = dcbaa
            .read_device_context_ptr(mem, slot_id)
            .map_err(|_| CommandCompletionCode::ParameterError)?
            & !0x3f;
        if dev_ctx_ptr == 0 {
            return Err(CommandCompletionCode::ContextStateError);
        }
        Ok(dev_ctx_ptr)
    }

    /// Stop an endpoint (MVP semantics).
    ///
    /// Updates the Endpoint Context Endpoint State field to `Stopped (3)` and preserves all other
    /// fields. If the device context pointer is missing, returns `ContextStateError`.
    pub fn stop_endpoint(
        &mut self,
        mem: &mut impl MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
    ) -> CommandCompletion {
        const EP_STATE_STOPPED: u8 = 3;

        if !(1..=31).contains(&endpoint_id) {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        // Validate slot before mutating guest memory.
        let slot_idx = usize::from(slot_id);
        if slot_id == 0 || slot_idx >= self.slots.len() {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }
        if !self.slots[slot_idx].enabled {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(code) => return CommandCompletion::failure(code),
        };
        let dev_ctx = DeviceContext32::new(dev_ctx_ptr);

        let mut ep_ctx = match dev_ctx.endpoint_context(mem, endpoint_id) {
            Ok(ctx) => ctx,
            Err(_) => return CommandCompletion::failure(CommandCompletionCode::ParameterError),
        };
        ep_ctx.set_endpoint_state(EP_STATE_STOPPED);
        if dev_ctx
            .write_endpoint_context(mem, endpoint_id, &ep_ctx)
            .is_err()
        {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        // Keep controller-local shadow context in sync for future work.
        let slot = &mut self.slots[slot_idx];
        slot.endpoint_contexts[usize::from(endpoint_id - 1)] = ep_ctx;
        slot.device_context_ptr = dev_ctx_ptr;

        CommandCompletion::success(slot_id)
    }

    /// Reset an endpoint (MVP semantics).
    ///
    /// Clears a halted/stopped endpoint and allows transfers again by setting the Endpoint State to
    /// `Running (1)`.
    pub fn reset_endpoint(
        &mut self,
        mem: &mut impl MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
    ) -> CommandCompletion {
        const EP_STATE_RUNNING: u8 = 1;

        if !(1..=31).contains(&endpoint_id) {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        let slot_idx = usize::from(slot_id);
        if slot_id == 0 || slot_idx >= self.slots.len() {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }
        if !self.slots[slot_idx].enabled {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(code) => return CommandCompletion::failure(code),
        };
        let dev_ctx = DeviceContext32::new(dev_ctx_ptr);

        let mut ep_ctx = match dev_ctx.endpoint_context(mem, endpoint_id) {
            Ok(ctx) => ctx,
            Err(_) => return CommandCompletion::failure(CommandCompletionCode::ParameterError),
        };
        ep_ctx.set_endpoint_state(EP_STATE_RUNNING);
        if dev_ctx
            .write_endpoint_context(mem, endpoint_id, &ep_ctx)
            .is_err()
        {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        let slot = &mut self.slots[slot_idx];
        slot.endpoint_contexts[usize::from(endpoint_id - 1)] = ep_ctx;
        slot.device_context_ptr = dev_ctx_ptr;

        CommandCompletion::success(slot_id)
    }

    /// Set Transfer Ring Dequeue Pointer (MVP semantics).
    ///
    /// Updates the Endpoint Context TR Dequeue Pointer and internal transfer ring cursor state.
    pub fn set_tr_dequeue_pointer(
        &mut self,
        mem: &mut impl MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
        tr_dequeue_ptr: u64,
        dcs: bool,
    ) -> CommandCompletion {
        if !(1..=31).contains(&endpoint_id) {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }
        if tr_dequeue_ptr == 0 || (tr_dequeue_ptr & 0x0f) != 0 {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        let slot_idx = usize::from(slot_id);
        if slot_id == 0 || slot_idx >= self.slots.len() {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }
        if !self.slots[slot_idx].enabled {
            return CommandCompletion::failure(CommandCompletionCode::ContextStateError);
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(code) => return CommandCompletion::failure(code),
        };
        let dev_ctx = DeviceContext32::new(dev_ctx_ptr);

        let mut ep_ctx = match dev_ctx.endpoint_context(mem, endpoint_id) {
            Ok(ctx) => ctx,
            Err(_) => return CommandCompletion::failure(CommandCompletionCode::ParameterError),
        };
        ep_ctx.set_tr_dequeue_pointer(tr_dequeue_ptr, dcs);
        if dev_ctx
            .write_endpoint_context(mem, endpoint_id, &ep_ctx)
            .is_err()
        {
            return CommandCompletion::failure(CommandCompletionCode::ParameterError);
        }

        let slot = &mut self.slots[slot_idx];
        slot.endpoint_contexts[usize::from(endpoint_id - 1)] = ep_ctx;
        slot.transfer_rings[usize::from(endpoint_id - 1)] =
            Some(RingCursor::new(tr_dequeue_ptr, dcs));
        slot.device_context_ptr = dev_ctx_ptr;

        CommandCompletion::success(slot_id)
    }

    fn sync_command_ring_from_crcr(&mut self) {
        // CRCR bits 63:6 contain the ring pointer; bits 3:0 contain flags (RCS/CS/CA/CRR).
        // Preserve the low flag bits while masking the pointer to the required alignment.
        let flags = self.crcr & 0x0f;
        let cycle = (flags & 0x1) != 0;
        let ptr = self.crcr & !0x3f;
        self.crcr = ptr | flags;
        self.command_ring = if ptr == 0 {
            None
        } else {
            Some(RingCursor::new(ptr, cycle))
        };
    }

    fn sync_crcr_from_command_ring(&mut self) {
        if let Some(ring) = self.command_ring {
            let ptr = ring.dequeue_ptr() & !0x3f;
            let mut flags = self.crcr & 0x0e;
            if ring.cycle_state() {
                flags |= 0x1;
            }
            self.crcr = ptr | flags;
        }
    }

    fn ring_doorbell0(&mut self) {
        self.cmd_kick = true;
    }

    fn maybe_process_command_ring(&mut self, mem: &mut dyn MemoryBus) {
        if !self.cmd_kick {
            return;
        }
        if (self.usbcmd & regs::USBCMD_RUN) == 0 {
            return;
        }
        // Process a bounded number of command TRBs and flush completion events to the guest event
        // ring.
        let ring_empty = self.process_command_ring(mem, COMMAND_BUDGET_PER_MMIO);
        self.sync_crcr_from_command_ring();
        self.service_event_ring(mem);
        if ring_empty {
            self.cmd_kick = false;
        }
    }

    /// Return the USB device currently bound to a slot, if any.
    pub fn slot_device_mut(&mut self, slot_id: u8) -> Option<&mut AttachedUsbDevice> {
        let idx = usize::from(slot_id);
        let slot_ctx = {
            let slot = self.slots.get(idx)?;
            if !slot.enabled || !slot.device_attached {
                return None;
            }
            slot.slot_context
        };

        let root_port = slot_ctx.root_hub_port_number();
        let route = slot_ctx.parsed_route_string().ok()?.ports_from_root();
        self.find_device_by_topology(root_port, &route)
    }

    /// Configure the guest transfer ring for an endpoint.
    ///
    /// `endpoint_id` is the xHCI Device Context Index (DCI). Endpoint 0 uses DCI=1.
    pub fn set_endpoint_ring(&mut self, slot_id: u8, endpoint_id: u8, dequeue_ptr: u64, cycle: bool) {
        let idx = usize::from(slot_id);
        let Some(slot) = self.slots.get_mut(idx) else {
            return;
        };
        let endpoint_id = endpoint_id & 0x1f;
        let Some(dci) = endpoint_id.checked_sub(1) else {
            return;
        };
        let Some(entry) = slot.transfer_rings.get_mut(dci as usize) else {
            return;
        };
        *entry = Some(RingCursor::new(dequeue_ptr, cycle));
        if endpoint_id == 1 {
            if let Some(state) = self.ep0_control_td.get_mut(idx) {
                *state = ControlTdState::default();
            }
        }
    }

    /// Handle a device endpoint doorbell write.
    ///
    /// `target` corresponds to the doorbell register index (slot id). For non-zero targets, the
    /// low 8 bits of `value` contain the endpoint ID (DCI).
    pub fn write_doorbell(&mut self, target: u8, value: u32) {
        if target == 0 {
            // Doorbell 0 is the command ring; not modelled yet.
            return;
        }
        let endpoint_id = (value & 0xff) as u8;
        self.ring_doorbell(target, endpoint_id);
    }

    /// Ring a device endpoint doorbell.
    ///
    /// This marks the endpoint as active. [`XhciController::tick`] will process pending work.
    pub fn ring_doorbell(&mut self, slot_id: u8, endpoint_id: u8) {
        let endpoint_id = endpoint_id & 0x1f;
        let entry = ActiveEndpoint {
            slot_id,
            endpoint_id,
        };
        if !self.active_endpoints.contains(&entry) {
            self.active_endpoints.push(entry);
        }
    }

    /// Process active endpoints.
    ///
    /// This is intentionally bounded to avoid guest-induced hangs (e.g. malformed transfer rings).
    pub fn tick(&mut self, mem: &mut (impl MemoryBus + ?Sized)) {
        let mut trb_budget = MAX_TRBS_PER_TICK;
        let mut i = 0;
        while i < self.active_endpoints.len() && trb_budget > 0 {
            let ep = self.active_endpoints[i];
            let outcome = self.process_endpoint(mem, ep.slot_id, ep.endpoint_id, trb_budget);
            trb_budget = trb_budget.saturating_sub(outcome.trbs_consumed);

            if outcome.keep_active {
                i += 1;
            } else {
                self.active_endpoints.swap_remove(i);
            }
        }
    }

    pub fn irq_level(&self) -> bool {
        // Preserve the skeleton's existing "DMA-on-RUN asserts EINT" behaviour while also exposing
        // a functional interrupter/event-ring driven interrupt condition.
        (self.usbsts & regs::USBSTS_EINT) != 0
            || (self.interrupter0.interrupt_enable() && self.interrupter0.interrupt_pending())
    }

    /// Returns true if there are pending event TRBs queued in host memory.
    pub fn irq_pending(&self) -> bool {
        !self.pending_events.is_empty()
    }

    /// Read-only view of interrupter 0 runtime registers.
    pub fn interrupter0(&self) -> &InterrupterRegs {
        &self.interrupter0
    }

    /// Queue an event TRB for delivery through the guest-configured event ring.
    pub fn post_event(&mut self, trb: Trb) {
        if self.pending_events.len() >= MAX_PENDING_EVENTS {
            // Keep the queue bounded even if the guest never configures an event ring.
            // Prefer newer events by dropping the oldest entry and tracking the loss.
            self.pending_events.pop_front();
            self.dropped_event_trbs += 1;
        }
        self.pending_events.push_back(trb);
    }

    /// Drain queued events into the guest event ring with a bounded per-tick budget.
    pub fn service_event_ring(&mut self, mem: &mut dyn MemoryBus) {
        self.event_ring.refresh(mem, &self.interrupter0);

        for _ in 0..EVENT_ENQUEUE_BUDGET_PER_TICK {
            let Some(&trb) = self.pending_events.front() else {
                break;
            };

            match self.event_ring.try_enqueue(mem, &self.interrupter0, trb) {
                Ok(()) => {
                    self.pending_events.pop_front();
                    self.interrupter0.set_interrupt_pending(true);
                }
                Err(event_ring::EnqueueError::NotConfigured) | Err(event_ring::EnqueueError::RingFull) => {
                    break
                }
                Err(event_ring::EnqueueError::InvalidConfig) => {
                    // Malformed guest configuration (e.g. ERST points out of bounds) should not
                    // panic; instead surface Host Controller Error as a sticky flag.
                    self.host_controller_error = true;
                    break;
                }
            }
        }
    }

    pub fn dropped_event_trbs(&self) -> u64 {
        self.dropped_event_trbs
    }

    pub fn read_portsc(&self, port: usize) -> u32 {
        self.ports.get(port).map(|p| p.read_portsc()).unwrap_or(0)
    }

    pub fn write_portsc(&mut self, port: usize, value: u32) {
        let Some(port_state) = self.ports.get_mut(port) else {
            return;
        };

        let changed = port_state.write_portsc(value);
        if changed {
            self.queue_port_status_change_event(port);
        }
    }

    /// Advances controller internal time by 1ms.
    pub fn tick_1ms(&mut self) {
        let mut ports_with_events = Vec::new();
        for (i, port) in self.ports.iter_mut().enumerate() {
            if port.tick_1ms() {
                ports_with_events.push(i);
            }
        }
        for port in ports_with_events {
            self.queue_port_status_change_event(port);
        }
    }

    /// Advances controller internal time by 1ms and drains any queued event TRBs into the
    /// guest-configured event ring.
    ///
    /// This is a convenience wrapper for integrations that want "one call per millisecond frame"
    /// behaviour:
    /// - advances port timers,
    /// - executes any pending transfer ring work, and
    /// - delivers queued events into the guest event ring.
    ///
    /// Note that both transfer execution and event ring delivery perform DMA into guest memory and
    /// should therefore be gated on PCI Bus Master Enable by the caller.
    pub fn tick_1ms_and_service_event_ring(&mut self, mem: &mut dyn MemoryBus) {
        self.tick_1ms();
        self.tick(mem);
        self.service_event_ring(mem);
    }

    /// Attach a device model to a root hub port (0-based).
    pub fn attach_device(&mut self, port: usize, dev: Box<dyn UsbDeviceModel>) {
        // Replace any existing device (host-side convenience).
        if self.ports.get(port).is_some_and(|p| p.has_device()) {
            self.detach_device(port);
        }

        let Some(port_state) = self.ports.get_mut(port) else {
            return;
        };
        let changed = port_state.attach(dev);
        if changed {
            self.queue_port_status_change_event(port);
        }
    }

    /// Detach any device from a root hub port (0-based).
    pub fn detach_device(&mut self, port: usize) {
        let Some(port_state) = self.ports.get_mut(port) else {
            return;
        };
        let changed = port_state.detach();
        if changed {
            self.queue_port_status_change_event(port);
        }

        // If the root hub port is disconnected, any slot previously bound to that port is no longer
        // reachable. Leave the slot enabled but mark the device as detached so `slot_device_mut()`
        // fails fast.
        let port_id = (port + 1) as u8;
        for slot in self.slots.iter_mut().skip(1) {
            if slot.enabled && slot.port_id == Some(port_id) {
                slot.device_attached = false;
            }
        }
    }

    /// Pops the next pending event TRB (interrupter 0), if any.
    ///
    /// This is a temporary host-facing interface until the full guest event ring model is
    /// implemented.
    pub fn pop_pending_event(&mut self) -> Option<Trb> {
        self.pending_events.pop_front()
    }

    pub fn pending_event_count(&self) -> usize {
        self.pending_events.len()
    }

    fn rebuild_ext_caps(&mut self) {
        self.ext_caps = self.build_ext_caps();
    }

    fn build_ext_caps(&self) -> Vec<u32> {
        let mut caps = Vec::new();

        // USB Legacy Support Capability.
        //
        // Real xHCI controllers often expose this for BIOS→OS handoff. We advertise it with
        // BIOS-owned cleared and OS-owned set so guests that probe the capability do not block.
        //
        // Layout:
        // - DWORD0: header + semaphores.
        // - DWORD1: legacy control/status (unused; all zeros).
        let supported_protocol_offset_bytes = regs::EXT_CAPS_OFFSET_BYTES + 8;
        let supported_protocol_offset_dwords = supported_protocol_offset_bytes / 4;
        let usb_legsup = (regs::EXT_CAP_ID_USB_LEGACY_SUPPORT as u32)
            | ((supported_protocol_offset_dwords as u32) << 8)
            | regs::USBLEGSUP_OS_OWNED;
        caps.push(usb_legsup);
        caps.push(0);

        // Supported Protocol Capability for USB 2.0.
        //
        // The roothub port range is 1-based, so we expose all ports as a single USB 2.0 range.
        let psic = 3u8; // low/full/high-speed entries.
        let header0 = (regs::EXT_CAP_ID_SUPPORTED_PROTOCOL as u32)
            | (0u32 << 8) // next pointer (0 => end of list)
            | ((regs::USB_REVISION_2_0 as u32) << 16);
        caps.push(header0);
        caps.push(regs::PROTOCOL_NAME_USB2);
        caps.push((1u32) | ((self.port_count as u32) << 8));
        // DWORD3: PSIC (0..=15) + Protocol Slot Type + PSI descriptor table offset.
        //
        // The PSI descriptor table begins immediately after DWORD3, at offset 4 dwords from the
        // start of the capability.
        let psio = 4u16;
        caps.push(
            (psic as u32)
                | ((regs::USB2_PROTOCOL_SLOT_TYPE as u32) << 8)
                | ((psio as u32) << 16),
        );

        // Protocol Speed ID descriptors.
        // These values are consumed by guest xHCI drivers to interpret PORTSC.PS values.
        caps.push(regs::encode_psi(
            regs::PSIV_FULL_SPEED,
            regs::PSI_TYPE_FULL,
            12,
            1,
        ));
        caps.push(regs::encode_psi(
            regs::PSIV_LOW_SPEED,
            regs::PSI_TYPE_LOW,
            15,
            0,
        ));
        caps.push(regs::encode_psi(
            regs::PSIV_HIGH_SPEED,
            regs::PSI_TYPE_HIGH,
            48,
            2,
        ));

        caps
    }

    fn mmio_read_u8(&self, offset: u64) -> u8 {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let port_regs_base = regs::REG_USBCMD + regs::port::PORTREGS_BASE;
        let port_regs_end =
            port_regs_base + u64::from(self.port_count) * regs::port::PORTREGS_STRIDE;

        // Reflect interrupter pending in USBSTS.EINT for drivers.
        let running = (self.usbcmd & regs::USBCMD_RUN) != 0;
        let usbsts = self.usbsts
            | if self.interrupter0.interrupt_pending() {
                regs::USBSTS_EINT
            } else {
                0
            }
            | if running { 0 } else { regs::USBSTS_HCHALTED }
            | if self.host_controller_error {
                regs::USBSTS_HCE
            } else {
                0
            };

        let value32 = match aligned {
            off if off >= port_regs_base && off < port_regs_end => {
                let rel = off - port_regs_base;
                let port = (rel / regs::port::PORTREGS_STRIDE) as usize;
                let reg_off = rel % regs::port::PORTREGS_STRIDE;
                match reg_off {
                    regs::port::PORTSC => self.ports.get(port).map(|p| p.read_portsc()).unwrap_or(0),
                    _ => 0,
                }
            }
            regs::REG_CAPLENGTH_HCIVERSION => regs::CAPLENGTH_HCIVERSION,
            regs::REG_HCSPARAMS1 => {
                // HCSPARAMS1: MaxSlots (7:0), MaxIntrs (18:8), MaxPorts (31:24).
                let max_slots = 32u32;
                let max_intrs = 1u32;
                let max_ports = self.port_count as u32;
                (max_slots & 0xff) | ((max_intrs & 0x7ff) << 8) | ((max_ports & 0xff) << 24)
            }
            regs::REG_HCCPARAMS1 => {
                // HCCPARAMS1.xECP: offset (in DWORDs) to the xHCI Extended Capabilities list.
                let xecp_dwords = (regs::EXT_CAPS_OFFSET_BYTES / 4) & 0xffff;
                // CSZ=0 => 32-byte contexts (MVP).
                (xecp_dwords << 16) & !regs::HCCPARAMS1_CSZ_64B
            }
            regs::REG_DBOFF => regs::DBOFF_VALUE,
            regs::REG_RTSOFF => regs::RTSOFF_VALUE,
            off
                if off >= regs::EXT_CAPS_OFFSET_BYTES as u64
                    && off
                        < regs::EXT_CAPS_OFFSET_BYTES as u64
                            + (self.ext_caps.len().saturating_mul(4) as u64) =>
            {
                let idx = (off - regs::EXT_CAPS_OFFSET_BYTES as u64) / 4;
                self.ext_caps.get(idx as usize).copied().unwrap_or(0)
            }

            regs::REG_USBCMD => self.usbcmd,
            regs::REG_USBSTS => usbsts,
            regs::REG_PAGESIZE => regs::PAGESIZE_4K,
            regs::REG_CRCR_LO => (self.crcr & 0xffff_ffff) as u32,
            regs::REG_CRCR_HI => (self.crcr >> 32) as u32,
            regs::REG_DCBAAP_LO => (self.dcbaap & 0xffff_ffff) as u32,
            regs::REG_DCBAAP_HI => (self.dcbaap >> 32) as u32,

            // Runtime interrupter 0 registers.
            regs::REG_INTR0_IMAN => self.interrupter0.iman_raw(),
            regs::REG_INTR0_IMOD => self.interrupter0.imod_raw(),
            regs::REG_INTR0_ERSTSZ => self.interrupter0.erstsz_raw(),
            regs::REG_INTR0_ERSTBA_LO => self.interrupter0.erstba_raw() as u32,
            regs::REG_INTR0_ERSTBA_HI => (self.interrupter0.erstba_raw() >> 32) as u32,
            regs::REG_INTR0_ERDP_LO => self.interrupter0.erdp_raw() as u32,
            regs::REG_INTR0_ERDP_HI => (self.interrupter0.erdp_raw() >> 32) as u32,

            _ => 0,
        };

        ((value32 >> shift) & 0xff) as u8
    }

    /// Read from the controller's MMIO register space.
    pub fn mmio_read(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // Treat out-of-range reads as open bus.
        let open_bus = match size {
            1 => 0xff,
            2 => 0xffff,
            4 => u32::MAX,
            _ => 0,
        };
        if !matches!(size, 1 | 2 | 4) {
            return open_bus;
        }
        let Some(end) = offset.checked_add(size as u64) else {
            return open_bus;
        };
        if end > u64::from(Self::MMIO_SIZE) {
            return open_bus;
        }

        self.maybe_process_command_ring(mem);

        // Read per-byte so unaligned/cross-dword reads behave like normal little-endian memory.
        // This is more robust against guests doing odd-sized or misaligned accesses.
        let mut out = 0u32;
        for i in 0..size {
            let Some(off) = offset.checked_add(i as u64) else {
                break;
            };
            let byte = self.mmio_read_u8(off);
            out |= (byte as u32) << (i * 8);
        }

        out
    }

    /// Write to the controller's MMIO register space.
    pub fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let Some(end) = offset.checked_add(size as u64) else {
            return;
        };
        if end > u64::from(Self::MMIO_SIZE) {
            return;
        }

        // Split cross-dword writes into byte writes so we don't lose bytes when a multi-byte access
        // spans two registers.
        let start_in_dword = (offset & 3) as usize;
        if start_in_dword + size > 4 && size > 1 {
            for i in 0..size {
                let Some(off) = offset.checked_add(i as u64) else {
                    break;
                };
                let byte = ((value >> (i * 8)) & 0xff) as u32;
                self.mmio_write(mem, off, 1, byte);
            }
            return;
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let (mask, value_shifted) = match size {
            1 => (0xffu32 << shift, (value & 0xff) << shift),
            2 => (0xffffu32 << shift, (value & 0xffff) << shift),
            4 => (u32::MAX, value),
            _ => return,
        };

        let portregs_base = regs::REG_USBCMD + regs::port::PORTREGS_BASE;
        let portregs_end =
            portregs_base + u64::from(self.port_count) * regs::port::PORTREGS_STRIDE;

        if aligned >= portregs_base && aligned < portregs_end {
            let rel = aligned - portregs_base;
            let port = (rel / regs::port::PORTREGS_STRIDE) as usize;
            let off_in_port = rel % regs::port::PORTREGS_STRIDE;
            if off_in_port == regs::port::PORTSC {
                // PORTSC is write-sensitive (W1C change bits, PR start). Forward the guest write.
                self.write_portsc(port, value_shifted);
            }
            return;
        }

        let merge = |cur: u32| (cur & !mask) | (value_shifted & mask);

        let doorbell_base = u64::from(regs::DBOFF_VALUE);
        let doorbell_end =
            doorbell_base + u64::from(regs::doorbell::DOORBELL_STRIDE) * 256u64 /*max doorbells*/;

        match aligned {
            off if off >= doorbell_base && off < doorbell_end => {
                let target = ((off - doorbell_base) / 4) as u8;
                let write_val = merge(0);
                self.write_doorbell(target, write_val);
            }
            regs::REG_USBCMD => {
                let prev = self.usbcmd;
                let next = merge(self.usbcmd);

                if (next & regs::USBCMD_HCRST) != 0 {
                    // Host Controller Reset (HCRST) is self-clearing. We model it as an immediate
                    // reset of operational registers and controller-local bookkeeping so real
                    // xHCI drivers waiting for the bit to clear can make progress.
                    self.reset_controller();
                    return;
                }

                self.usbcmd = next;

                // On the rising edge of RUN, perform a small DMA read from CRCR to validate PCI Bus
                // Master Enable (BME) gating in the emulator wrapper.
                let was_running = (prev & regs::USBCMD_RUN) != 0;
                let now_running = (self.usbcmd & regs::USBCMD_RUN) != 0;
                if !was_running && now_running {
                    self.dma_on_run(mem);
                }
            }
            regs::REG_USBSTS => {
                // Treat USBSTS as RW1C. Writing 1 clears the bit.
                let write_val = merge(0);
                self.usbsts &= !write_val;
                // Allow acknowledging event interrupts via USBSTS.EINT by also clearing
                // Interrupter 0's pending bit (IMAN.IP). This is a minimal model of the xHCI
                // "summary" interrupt status bit.
                if (write_val & regs::USBSTS_EINT) != 0 {
                    self.interrupter0.set_interrupt_pending(false);
                }
            }
            regs::REG_CRCR_LO => {
                let lo = merge(self.crcr as u32) as u64;
                self.crcr = (self.crcr & 0xffff_ffff_0000_0000) | lo;
                self.sync_command_ring_from_crcr();
            }
            regs::REG_CRCR_HI => {
                let hi = merge((self.crcr >> 32) as u32) as u64;
                self.crcr = (self.crcr & 0x0000_0000_ffff_ffff) | (hi << 32);
                self.sync_command_ring_from_crcr();
            }
            regs::REG_DCBAAP_LO => {
                let lo = merge(self.dcbaap as u32) as u64;
                self.dcbaap = (self.dcbaap & 0xffff_ffff_0000_0000) | lo;
                self.dcbaap &= !0x3f;
            }
            regs::REG_DCBAAP_HI => {
                let hi = merge((self.dcbaap >> 32) as u32) as u64;
                self.dcbaap = (self.dcbaap & 0x0000_0000_ffff_ffff) | (hi << 32);
                self.dcbaap &= !0x3f;
            }

            // Runtime interrupter 0 registers.
            regs::REG_INTR0_IMAN => {
                self.interrupter0.write_iman_masked(value_shifted, mask);
            }
            regs::REG_INTR0_IMOD => {
                let v = merge(self.interrupter0.imod_raw());
                self.interrupter0.write_imod(v);
            }
            regs::REG_INTR0_ERSTSZ => {
                let v = merge(self.interrupter0.erstsz_raw());
                self.interrupter0.write_erstsz(v);
            }
            regs::REG_INTR0_ERSTBA_LO => {
                let lo = merge(self.interrupter0.erstba_raw() as u32) as u64;
                let v = (self.interrupter0.erstba_raw() & 0xffff_ffff_0000_0000) | lo;
                self.interrupter0.write_erstba(v);
            }
            regs::REG_INTR0_ERSTBA_HI => {
                let hi = merge((self.interrupter0.erstba_raw() >> 32) as u32) as u64;
                let v = (self.interrupter0.erstba_raw() & 0x0000_0000_ffff_ffff) | (hi << 32);
                self.interrupter0.write_erstba(v);
            }
            regs::REG_INTR0_ERDP_LO => {
                let lo = merge(self.interrupter0.erdp_raw() as u32) as u64;
                let v = (self.interrupter0.erdp_raw() & 0xffff_ffff_0000_0000) | lo;
                self.interrupter0.write_erdp(v);
            }
            regs::REG_INTR0_ERDP_HI => {
                let hi = merge((self.interrupter0.erdp_raw() >> 32) as u32) as u64;
                let v = (self.interrupter0.erdp_raw() & 0x0000_0000_ffff_ffff) | (hi << 32);
                self.interrupter0.write_erdp(v);
            }

            off if off == u64::from(regs::DBOFF_VALUE) => {
                // Doorbell 0: notify controller that command ring contains new commands.
                let _ = value_shifted;
                self.ring_doorbell0();
            }

            _ => {}
        }

        self.maybe_process_command_ring(mem);
    }

    fn reset_controller(&mut self) {
        self.usbcmd = 0;
        self.usbsts = 0;
        self.host_controller_error = false;
        self.crcr = 0;
        self.dcbaap = 0;
        self.command_ring = None;
        self.cmd_kick = false;

        for slot in self.slots.iter_mut() {
            *slot = SlotState::default();
        }

        for port in self.ports.iter_mut() {
            port.host_controller_reset();
        }

        self.interrupter0 = InterrupterRegs::default();
        self.event_ring = EventRingProducer::default();
        self.pending_events.clear();
        self.dropped_event_trbs = 0;
    }

    fn queue_port_status_change_event(&mut self, port: usize) {
        let port_id = (port + 1) as u8;
        self.post_event(make_port_status_change_event_trb(port_id));
    }

    fn process_endpoint(
        &mut self,
        mem: &mut (impl MemoryBus + ?Sized),
        slot_id: u8,
        endpoint_id: u8,
        trb_budget: usize,
    ) -> EndpointOutcome {
        // Only control endpoint 0 (DCI=1) is modelled today.
        if endpoint_id != 1 {
            return EndpointOutcome::idle();
        }

        let slot_idx = usize::from(slot_id);
        let Some(slot) = self.slots.get(slot_idx) else {
            return EndpointOutcome::idle();
        };
        if !slot.enabled || !slot.device_attached {
            return EndpointOutcome::idle();
        }

        let Some(mut ring) = slot.transfer_rings[0] else {
            return EndpointOutcome::idle();
        };

        let mut control_td = self
            .ep0_control_td
            .get(slot_idx)
            .copied()
            .unwrap_or_default();

        let mut events: Vec<Trb> = Vec::new();
        let mut trbs_consumed = 0usize;
        let mut keep_active = false;

        {
            let Some(device) = self.slot_device_mut(slot_id) else {
                return EndpointOutcome::idle();
            };

            while trbs_consumed < trb_budget {
                let item = match ring.peek(mem, RING_STEP_BUDGET) {
                    RingPoll::Ready(item) => item,
                    RingPoll::NotReady => {
                        keep_active = false;
                        break;
                    }
                    RingPoll::Err(_) => {
                        keep_active = false;
                        break;
                    }
                };

                let trb = item.trb;
                let trb_paddr = item.paddr;

                match trb.trb_type() {
                    TrbType::SetupStage => {
                        control_td = ControlTdState::default();

                        let setup_bytes = trb.parameter.to_le_bytes();
                        let setup = SetupPacket::from_bytes(setup_bytes);

                        let completion = match device.handle_setup(setup) {
                            UsbOutResult::Ack => CompletionCode::Success,
                            UsbOutResult::Nak => {
                                keep_active = true;
                                break;
                            }
                            UsbOutResult::Stall => CompletionCode::StallError,
                            UsbOutResult::Timeout => CompletionCode::UsbTransactionError,
                        };

                        control_td.completion_code = completion;

                        if ring.consume().is_err() {
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;

                        if trb.ioc() || completion != CompletionCode::Success {
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                completion,
                                0,
                            ));
                        }
                    }
                    TrbType::DataStage => {
                        let requested_len = trb.transfer_len() as usize;
                        if requested_len > MAX_CONTROL_DATA_LEN {
                            let completion = CompletionCode::TrbError;
                            if ring.consume().is_err() {
                                keep_active = false;
                                break;
                            }
                            trbs_consumed += 1;
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                completion,
                                0,
                            ));
                            control_td = ControlTdState::default();
                            keep_active = true;
                            break;
                        }

                        let buf_ptr = trb.pointer();
                        let dir_in = (trb.control & Trb::CONTROL_DIR) != 0;

                        let (completion, transferred) = if dir_in {
                            match device.handle_in(0, requested_len) {
                                UsbInResult::Data(mut data) => {
                                    if data.len() > requested_len {
                                        data.truncate(requested_len);
                                    }
                                    mem.write_physical(buf_ptr, &data);
                                    let transferred = data.len();
                                    let completion = if transferred < requested_len {
                                        CompletionCode::ShortPacket
                                    } else {
                                        CompletionCode::Success
                                    };
                                    (completion, transferred)
                                }
                                UsbInResult::Nak => {
                                    keep_active = true;
                                    break;
                                }
                                UsbInResult::Stall => (CompletionCode::StallError, 0),
                                UsbInResult::Timeout => (CompletionCode::UsbTransactionError, 0),
                            }
                        } else {
                            let mut buf = vec![0u8; requested_len];
                            mem.read_physical(buf_ptr, &mut buf);
                            match device.handle_out(0, &buf) {
                                UsbOutResult::Ack => (CompletionCode::Success, requested_len),
                                UsbOutResult::Nak => {
                                    keep_active = true;
                                    break;
                                }
                                UsbOutResult::Stall => (CompletionCode::StallError, 0),
                                UsbOutResult::Timeout => (CompletionCode::UsbTransactionError, 0),
                            }
                        };

                        control_td.data_expected = requested_len;
                        control_td.data_transferred = transferred;
                        control_td.completion_code = completion;

                        if ring.consume().is_err() {
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;

                        // Short Packet is reported as the *TD completion* (StatusStage) in most
                        // control-transfer flows. Only generate an event at the DataStage TRB when
                        // explicitly requested via IOC, or when the transfer failed.
                        if trb.ioc()
                            || (completion != CompletionCode::Success
                                && completion != CompletionCode::ShortPacket)
                        {
                            let residue = requested_len.saturating_sub(transferred);
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                completion,
                                residue,
                            ));
                        }
                    }
                    TrbType::StatusStage => {
                        let dir_in = (trb.control & Trb::CONTROL_DIR) != 0;

                        let status_completion = if dir_in {
                            match device.handle_in(0, 0) {
                                UsbInResult::Data(data) => {
                                    if !data.is_empty() {
                                        CompletionCode::TrbError
                                    } else {
                                        CompletionCode::Success
                                    }
                                }
                                UsbInResult::Nak => {
                                    keep_active = true;
                                    break;
                                }
                                UsbInResult::Stall => CompletionCode::StallError,
                                UsbInResult::Timeout => CompletionCode::UsbTransactionError,
                            }
                        } else {
                            match device.handle_out(0, &[]) {
                                UsbOutResult::Ack => CompletionCode::Success,
                                UsbOutResult::Nak => {
                                    keep_active = true;
                                    break;
                                }
                                UsbOutResult::Stall => CompletionCode::StallError,
                                UsbOutResult::Timeout => CompletionCode::UsbTransactionError,
                            }
                        };

                        let (completion, residue) = if status_completion == CompletionCode::Success {
                            let residue = control_td
                                .data_expected
                                .saturating_sub(control_td.data_transferred);
                            (control_td.completion_code, residue)
                        } else {
                            (status_completion, 0)
                        };

                        if ring.consume().is_err() {
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;

                        if trb.ioc() || completion != CompletionCode::Success {
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                completion,
                                residue,
                            ));
                        }

                        // Reset TD bookkeeping at the end of the control transfer.
                        control_td = ControlTdState::default();
                    }
                    // Unsupported transfer TRBs: consume and ignore so the ring continues.
                    _ => {
                        if ring.consume().is_err() {
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;
                    }
                }
            }
        }

        // Persist the updated ring cursor + control TD bookkeeping.
        if let Some(slot) = self.slots.get_mut(slot_idx) {
            slot.transfer_rings[0] = Some(ring);
        }
        if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
            *state = control_td;
        }
        for ev in events {
            self.post_event(ev);
        }

        if keep_active {
            EndpointOutcome::keep(trbs_consumed)
        } else {
            EndpointOutcome::done(trbs_consumed)
        }
    }

    fn dma_on_run(&mut self, mem: &mut dyn MemoryBus) {
        // Read a dword from CRCR and surface an interrupt. The data itself is ignored; the goal is
        // to touch the memory bus when bus mastering is enabled so the emulator wrapper can gate
        // the access.
        let paddr = self.crcr & !0x3f;
        let mut buf = [0u8; 4];
        mem.read_bytes(paddr, &mut buf);
        self.usbsts |= regs::USBSTS_EINT;
    }
}

fn make_transfer_event_trb(
    slot_id: u8,
    endpoint_id: u8,
    completed_trb_ptr: u64,
    completion_code: CompletionCode,
    residue: usize,
) -> Trb {
    // xHCI reports *residual bytes* (not bytes transferred) in the Transfer Event's
    // Transfer Length field.
    let transfer_len = (residue as u32) & 0x00ff_ffff;
    let status = transfer_len | ((u32::from(completion_code.raw())) << 24);

    let mut ev = Trb::new(completed_trb_ptr & !0x0f, status, 0);
    ev.set_trb_type(TrbType::TransferEvent);
    ev.set_slot_id(slot_id);
    ev.set_endpoint_id(endpoint_id);
    ev
}

fn make_port_status_change_event_trb(port_id: u8) -> Trb {
    // xHCI spec: Port Status Change Event TRB
    // - Parameter bits 24..=31: Port ID
    // - Status bits 24..=31: Completion Code (Success)
    // - Control bits 10..=15: TRB Type
    let mut trb = Trb::new(
        (port_id as u64) << regs::PSC_EVENT_PORT_ID_SHIFT,
        (u32::from(COMPLETION_CODE_SUCCESS)) << Trb::STATUS_COMPLETION_CODE_SHIFT,
        0,
    );
    trb.set_cycle(true);
    trb.set_trb_type(TrbType::PortStatusChangeEvent);
    trb
}
impl IoSnapshot for XhciController {
    const DEVICE_ID: [u8; 4] = *b"XHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(0, 3);

    fn save_state(&self) -> Vec<u8> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_HOST_CONTROLLER_ERROR: u16 = 11;
        const TAG_CRCR: u16 = 3;
        const TAG_PORT_COUNT: u16 = 4;
        const TAG_DCBAAP: u16 = 5;
        const TAG_INTR0_IMAN: u16 = 6;
        const TAG_INTR0_IMOD: u16 = 7;
        const TAG_INTR0_ERSTSZ: u16 = 8;
        const TAG_INTR0_ERSTBA: u16 = 9;
        const TAG_INTR0_ERDP: u16 = 10;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u32(TAG_USBCMD, self.usbcmd);
        w.field_u32(TAG_USBSTS, self.usbsts);
        w.field_bool(TAG_HOST_CONTROLLER_ERROR, self.host_controller_error);
        w.field_u64(TAG_CRCR, self.crcr);
        w.field_u8(TAG_PORT_COUNT, self.port_count);
        w.field_u64(TAG_DCBAAP, self.dcbaap);
        w.field_u32(TAG_INTR0_IMAN, self.interrupter0.iman_raw());
        w.field_u32(TAG_INTR0_IMOD, self.interrupter0.imod_raw());
        w.field_u32(TAG_INTR0_ERSTSZ, self.interrupter0.erstsz_raw());
        w.field_u64(TAG_INTR0_ERSTBA, self.interrupter0.erstba_raw());
        w.field_u64(TAG_INTR0_ERDP, self.interrupter0.erdp_raw());
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_USBCMD: u16 = 1;
        const TAG_USBSTS: u16 = 2;
        const TAG_HOST_CONTROLLER_ERROR: u16 = 11;
        const TAG_CRCR: u16 = 3;
        const TAG_PORT_COUNT: u16 = 4;
        const TAG_DCBAAP: u16 = 5;
        const TAG_INTR0_IMAN: u16 = 6;
        const TAG_INTR0_IMOD: u16 = 7;
        const TAG_INTR0_ERSTSZ: u16 = 8;
        const TAG_INTR0_ERSTBA: u16 = 9;
        const TAG_INTR0_ERDP: u16 = 10;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let port_count = r.u8(TAG_PORT_COUNT)?.unwrap_or(DEFAULT_PORT_COUNT).max(1);
        *self = Self::with_port_count(port_count);

        self.usbcmd = r.u32(TAG_USBCMD)?.unwrap_or(0);
        self.usbsts = r.u32(TAG_USBSTS)?.unwrap_or(0);
        self.host_controller_error = r.bool(TAG_HOST_CONTROLLER_ERROR)?.unwrap_or(false);
        self.crcr = r.u64(TAG_CRCR)?.unwrap_or(0);
        self.dcbaap = r.u64(TAG_DCBAAP)?.unwrap_or(0) & !0x3f;
        self.sync_command_ring_from_crcr();

        if let Some(v) = r.u32(TAG_INTR0_IMAN)? {
            self.interrupter0.restore_iman(v);
        }
        if let Some(v) = r.u32(TAG_INTR0_IMOD)? {
            self.interrupter0.restore_imod(v);
        }
        if let Some(v) = r.u32(TAG_INTR0_ERSTSZ)? {
            self.interrupter0.restore_erstsz(v);
        }
        if let Some(v) = r.u64(TAG_INTR0_ERSTBA)? {
            self.interrupter0.restore_erstba(v);
        }
        if let Some(v) = r.u64(TAG_INTR0_ERDP)? {
            self.interrupter0.restore_erdp(v);
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests;
