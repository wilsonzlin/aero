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
//! - a small DMA read on the rising edge of `USBCMD.RUN` (to validate PCI BME gating in the wrapper;
//!   may be deferred until the next tick if DMA isn't available during the MMIO write)
//! - interrupter 0 event ring delivery (ERST/ERDP/IMAN) with a bounded pre-ERST pending queue
//! - a level-triggered `irq_level()` surface (to validate PCI INTx disable gating)
//! - DCBAAP register storage + controller-local slot allocation (Enable Slot scaffolding)
//! - a minimal runtime interrupter 0 register block + guest event ring producer (ERST-based)
//! - snapshot/restore support (including basic USB topology) for VM save/restore
//!
//! The controller also exposes doorbells and bounded command ring/transfer execution, but full xHCI
//! semantics (complete command/transfer state machines, SuperSpeed, multi-interrupter MSI/MSI-X,
//! etc) remain future work.
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
//!
//! ## Snapshot/restore
//!
//! [`XhciController`] implements [`aero_io_snapshot::io::state::IoSnapshot`] using the canonical
//! `aero-io-snapshot` TLV encoding so it can participate in VM snapshot/restore deterministically.
//! The snapshot persists controller-local state needed to resume execution:
//! - MMIO-visible registers (`USBCMD`, `USBSTS`, `CRCR`, `DCBAAP`, ...).
//! - Root-port state machines (connect/change bits, reset timers, attached device trees).
//! - Pending event TRBs queued on the host side (until a full guest event ring exists).
//! - Slot state (slot->port mapping, device context pointers, and transfer ring cursors).
//!
//! Attached devices are snapshotted via nested `ADEV` snapshots so restores can reconstruct missing
//! device instances without requiring the host to pre-attach devices.
//!
//! ## WASM / host async restore safety
//!
//! Some USB device models (e.g. WebUSB passthrough) keep host-side asynchronous state (queued
//! actions backed by JS Promises or external handles). That host-side state cannot be resumed after
//! restoring a VM snapshot. Host integrations should traverse the restored USB topology and clear
//! any such in-flight state (for WebUSB this is
//! [`crate::UsbWebUsbPassthroughDevice::reset_host_state_for_restore`]) before resuming execution.

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
mod snapshot;

pub use regs::{PORTSC_CCS, PORTSC_CSC, PORTSC_PEC, PORTSC_PED, PORTSC_PR, PORTSC_PRC};

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::fmt;

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
const MAX_PENDING_EVENTS: usize = 1024;
const COMPLETION_CODE_SUCCESS: u8 = 1;
const MAX_TRBS_PER_TICK: usize = 256;
const RING_STEP_BUDGET: usize = 64;
const MAX_CONTROL_DATA_LEN: usize = 64 * 1024;
/// Maximum number of command TRBs processed per `process_command_ring` call.
///
/// This keeps command processing bounded even if a host integration accidentally calls the method
/// with an extremely large `max_trbs` value.
const MAX_COMMAND_TRBS_PER_CALL: usize = 256;
const TRB_CTRL_IDT: u32 = 1 << 6;

/// Maximum number of event TRBs written into the guest event ring per controller tick.
pub const EVENT_ENQUEUE_BUDGET_PER_TICK: usize = 64;

const COMMAND_RING_STEP_BUDGET: usize = RING_STEP_BUDGET;

/// Deterministic per-frame work budgets for xHCI stepping.
///
/// xHCI is guest-driven: guests control doorbells and TRB rings. A buggy or malicious guest can
/// craft rings that would otherwise cause unbounded work (ring walking, repeated NAK polling,
/// doorbell storms) in a single 1ms tick.
///
/// Aero's controller model therefore exposes a deterministic, unit-based budget model. Budgets are
/// expressed in guest-visible work (TRBs, doorbells, ring-walk steps), not host CPU time.
pub mod budget {
    /// Maximum number of endpoint doorbells/activations serviced per 1ms frame.
    pub const MAX_DOORBELLS_PER_FRAME: usize = super::MAX_TRBS_PER_TICK;

    /// Maximum number of command ring TRBs processed per 1ms frame.
    ///
    /// This budget counts *commands* (non-Link TRBs returned by the command ring cursor). Link TRBs
    /// are bounded separately by [`MAX_RING_POLL_STEPS_PER_FRAME`] and the per-poll step budget
    /// passed to [`super::RingCursor::poll`].
    pub const MAX_COMMAND_TRBS_PER_FRAME: usize = 64;

    /// Maximum number of transfer ring TRBs consumed per 1ms frame.
    pub const MAX_TRANSFER_TRBS_PER_FRAME: usize = super::MAX_TRBS_PER_TICK;

    /// Maximum number of event TRBs enqueued into the guest event ring per 1ms frame.
    pub const MAX_EVENT_TRBS_PER_FRAME: usize = super::EVENT_ENQUEUE_BUDGET_PER_TICK;

    /// Maximum number of ring-walk steps (TRB fetches from guest memory) performed per 1ms frame.
    ///
    /// This is a controller-wide backstop against pathological ring layouts (e.g. long link chains).
    pub const MAX_RING_POLL_STEPS_PER_FRAME: usize = 1024;
}

/// Work counters emitted by [`XhciController::step_1ms`] (primarily for tests and instrumentation).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TickWork {
    pub doorbells_serviced: usize,
    pub command_trbs_processed: usize,
    pub transfer_trbs_consumed: usize,
    pub event_trbs_written: usize,
    pub ring_poll_steps: usize,
}

use self::context::{
    Dcbaa, DeviceContext32, EndpointContext, InputControlContext, SlotContext, CONTEXT_SIZE,
    SLOT_STATE_ADDRESSED, SLOT_STATE_CONFIGURED, SLOT_STATE_DEFAULT,
};
use self::ring::{RingCursor, RingError, RingPoll};

/// A `UsbDeviceModel` proxy that forwards calls to a device model owned elsewhere.
///
/// # Safety
///
/// This is used to let [`transfer::XhciTransferExecutor`] operate on device models stored inside
/// the controller's root hub/slot topology without moving them out of the hub data structures.
///
/// The executor is single-threaded and all access is routed through the controller, so it is up to
/// the controller implementation to ensure the pointer remains valid (e.g. by dropping the
/// executor when a device is detached) and that it does not create overlapping mutable borrows of
/// the underlying model.
struct UsbDeviceModelPtr {
    ptr: *mut dyn UsbDeviceModel,
}

impl UsbDeviceModelPtr {
    fn new(ptr: *mut dyn UsbDeviceModel) -> Self {
        Self { ptr }
    }

    #[inline]
    unsafe fn model(&self) -> &dyn UsbDeviceModel {
        &*self.ptr
    }

    #[inline]
    unsafe fn model_mut(&mut self) -> &mut dyn UsbDeviceModel {
        &mut *self.ptr
    }
}

impl UsbDeviceModel for UsbDeviceModelPtr {
    fn speed(&self) -> crate::UsbSpeed {
        unsafe { self.model().speed() }
    }

    fn reset_host_state_for_restore(&mut self) {
        unsafe { self.model_mut().reset_host_state_for_restore() }
    }

    fn reset(&mut self) {
        unsafe { self.model_mut().reset() }
    }

    fn cancel_control_transfer(&mut self) {
        unsafe { self.model_mut().cancel_control_transfer() }
    }

    fn handle_control_request(
        &mut self,
        setup: crate::SetupPacket,
        data_stage: Option<&[u8]>,
    ) -> crate::ControlResponse {
        unsafe { self.model_mut().handle_control_request(setup, data_stage) }
    }

    fn handle_in_transfer(&mut self, ep: u8, max_len: usize) -> crate::UsbInResult {
        unsafe { self.model_mut().handle_in_transfer(ep, max_len) }
    }

    fn handle_out_transfer(&mut self, ep: u8, data: &[u8]) -> crate::UsbOutResult {
        unsafe { self.model_mut().handle_out_transfer(ep, data) }
    }

    fn as_hub(&self) -> Option<&dyn crate::hub::UsbHub> {
        unsafe { self.model().as_hub() }
    }

    fn as_hub_mut(&mut self) -> Option<&mut dyn crate::hub::UsbHub> {
        unsafe { self.model_mut().as_hub_mut() }
    }

    fn tick_1ms(&mut self) {
        unsafe { self.model_mut().tick_1ms() }
    }

    fn set_suspended(&mut self, suspended: bool) {
        unsafe { self.model_mut().set_suspended(suspended) }
    }

    fn poll_remote_wakeup(&mut self) -> bool {
        unsafe { self.model_mut().poll_remote_wakeup() }
    }

    fn child_device_mut_for_address(
        &mut self,
        address: u8,
    ) -> Option<&mut crate::device::AttachedUsbDevice> {
        unsafe { self.model_mut().child_device_mut_for_address(address) }
    }
}

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
#[derive(Debug, Clone, Default)]
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
    /// When `Some`, a control TD is currently in-flight and this cursor points at the first TRB of
    /// the TD (the Setup Stage TRB).
    ///
    /// xHCI's architectural TR Dequeue Pointer is only advanced once the TD completes, so we keep
    /// the committed endpoint ring cursor pinned here while we process the Data/Status stages.
    td_start: Option<RingCursor>,
    /// Current internal dequeue cursor within the in-flight control TD.
    ///
    /// Unlike `td_start`, this cursor is allowed to advance between Setup/Data/Status TRBs so the
    /// controller can make forward progress. On NAK, this cursor is left pointing at the TRB that
    /// should be retried.
    td_cursor: Option<RingCursor>,
    data_expected: usize,
    data_transferred: usize,
    completion_code: CompletionCode,
}

impl Default for ControlTdState {
    fn default() -> Self {
        Self {
            td_start: None,
            td_cursor: None,
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
    dnctrl: u32,
    crcr: u64,
    dcbaap: u64,
    config: u32,
    slots: Vec<SlotState>,

    // Root hub ports.
    ports: Vec<XhciPort>,

    // Command ring cursor + doorbell kick.
    //
    // Guest software programs CRCR with a dequeue pointer + cycle state and rings doorbell 0 to
    // notify the controller. We keep a small "kick" flag so command processing can continue across
    // subsequent controller ticks without requiring the guest to ring doorbell 0 for every TRB.
    command_ring: Option<RingCursor>,
    cmd_kick: bool,

    // Runtime registers: interrupter 0 + guest event ring delivery.
    interrupter0: InterrupterRegs,
    event_ring: EventRingProducer,
    /// Microframe index (MFINDEX).
    ///
    /// The xHCI spec counts in 125µs microframes. The web/native integrations tick the controller
    /// in 1ms frames, so we advance by 8 microframes per `tick_1ms`.
    mfindex: u32,

    // Host-side event buffering.
    pending_events: VecDeque<Trb>,
    dropped_event_trbs: u64,
    /// Internal controller time in 1ms USB frames.
    ///
    /// This is advanced via [`XhciController::tick_1ms`] and is intended to back time-based
    /// features such as port reset timers and transfer scheduling.
    time_ms: u64,
    /// Last dword read via the tick-driven DMA path.
    ///
    /// This is primarily used by wrapper tests to validate PCI Bus Master Enable (BME) gating.
    last_tick_dma_dword: u32,
    /// Pending "DMA-on-RUN" probe.
    ///
    /// The xHCI model performs a small DMA read from `CRCR` on the rising edge of `USBCMD.RUN` and
    /// uses it to raise a synthetic interrupt. Some integrations (notably the PC platform) route
    /// MMIO via a bus that cannot provide a re-entrant DMA view during the MMIO write itself. To
    /// support those integrations, the controller latches the rising edge and will perform the DMA
    /// read + interrupt on the next tick once DMA is available.
    pending_dma_on_run: bool,

    // --- Endpoint transfer execution (subset) ---
    /// Endpoints with pending work, scheduled in a simple round-robin queue.
    active_endpoints: VecDeque<ActiveEndpoint>,
    /// Coalescing bitmap to avoid unbounded `active_endpoints` growth under doorbell storms.
    active_endpoint_pending: [[bool; 32]; 256],
    ep0_control_td: Vec<ControlTdState>,

    /// Per-slot transfer-ring executors for bulk/interrupt endpoints.
    ///
    /// Each executor holds its own per-endpoint ring state and a lightweight proxy that forwards
    /// USB requests to the device model currently bound to the slot.
    transfer_executors: Vec<Option<transfer::XhciTransferExecutor>>,
}

impl fmt::Debug for XhciController {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active_execs = self
            .transfer_executors
            .iter()
            .filter(|e| e.is_some())
            .count();
        f.debug_struct("XhciController")
            .field("port_count", &self.port_count)
            .field("ext_caps_dwords", &self.ext_caps.len())
            .field("usbcmd", &self.usbcmd)
            .field("usbsts", &self.usbsts)
            .field("host_controller_error", &self.host_controller_error)
            .field("dnctrl", &self.dnctrl)
            .field("crcr", &self.crcr)
            .field("dcbaap", &self.dcbaap)
            .field("config", &self.config)
            .field("slots", &self.slots.len())
            .field("command_ring", &self.command_ring)
            .field("cmd_kick", &self.cmd_kick)
            .field("pending_events", &self.pending_events.len())
            .field("dropped_event_trbs", &self.dropped_event_trbs)
            .field("time_ms", &self.time_ms)
            .field("last_tick_dma_dword", &self.last_tick_dma_dword)
            .field("pending_dma_on_run", &self.pending_dma_on_run)
            .field("interrupter0", &self.interrupter0)
            .field("active_endpoints", &self.active_endpoints.len())
            .field("transfer_executors", &active_execs)
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
        assert!(
            port_count > 0,
            "xHCI controller must expose at least one port"
        );
        const DEFAULT_MAX_SLOTS: usize = regs::MAX_SLOTS as usize;
        let slots: Vec<SlotState> = core::iter::repeat_with(SlotState::default)
            .take(DEFAULT_MAX_SLOTS + 1)
            .collect();
        let slot_count = slots.len();
        let transfer_executors: Vec<Option<transfer::XhciTransferExecutor>> =
            core::iter::repeat_with(|| None).take(slot_count).collect();
        let mut ctrl = Self {
            port_count,
            ext_caps: Vec::new(),
            usbcmd: 0,
            usbsts: 0,
            host_controller_error: false,
            dnctrl: 0,
            crcr: 0,
            dcbaap: 0,
            config: 0,
            slots,
            cmd_kick: false,
            ports: (0..port_count).map(|_| XhciPort::new()).collect(),
            command_ring: None,
            interrupter0: InterrupterRegs::default(),
            event_ring: EventRingProducer::default(),
            mfindex: 0,
            pending_events: VecDeque::new(),
            dropped_event_trbs: 0,
            time_ms: 0,
            last_tick_dma_dword: 0,
            pending_dma_on_run: false,
            active_endpoints: VecDeque::new(),
            active_endpoint_pending: [[false; 32]; 256],
            ep0_control_td: vec![ControlTdState::default(); slot_count],
            transfer_executors,
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
        hub_dev.model_mut().hub_detach_device(leaf_port)?;

        // Any slots routed to the detached device (or a device behind the detached hub) are no
        // longer valid. Clear the binding and drop any transfer executors to avoid holding raw
        // pointers to detached device models.
        let root_port_number = (root_port as u8).saturating_add(1);
        for slot_id in 1..self.slots.len() {
            let (enabled, route_prefix_match) = {
                let slot = &self.slots[slot_id];
                if !slot.enabled {
                    (false, false)
                } else if slot.port_id != Some(root_port_number) {
                    (true, false)
                } else {
                    let route = slot
                        .slot_context
                        .parsed_route_string()
                        .map(|rs| rs.ports_from_root())
                        .unwrap_or_default();
                    (true, route.as_slice().starts_with(rest))
                }
            };
            if !enabled || !route_prefix_match {
                continue;
            }

            self.slots[slot_id].device_attached = false;
            self.transfer_executors[slot_id] = None;
            let slot_id_u8 = slot_id as u8;
            if let Some(state) = self.ep0_control_td.get_mut(slot_id) {
                *state = ControlTdState::default();
            }
            self.clear_slot_pending_endpoints(slot_id_u8);
        }

        Ok(())
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
        self.attach_device(
            root_port,
            Box::new(UsbHubDevice::with_port_count(port_count)),
        );
        Ok(())
    }

    /// Advance the controller by one 1ms USB frame while allowing DMA via `mem`.
    ///
    /// This helper exists for snapshot coverage and some wrapper tests: it advances the 1ms tick
    /// counter/MFINDEX and, when the controller is running and DMA is enabled, performs a minimal
    /// DMA read against the guest-provided CRCR pointer.
    pub fn tick_1ms_with_dma(&mut self, mem: &mut dyn MemoryBus) {
        self.tick_ports_1ms();

        // HCE is a fatal controller error; once latched, the guest must issue a controller reset.
        // Avoid performing any further DMA while the controller is in an error state.
        if self.host_controller_error {
            return;
        }

        // Run any deferred "DMA-on-RUN" probe (used by PCI wrappers to validate BME gating).
        //
        // This is gated on `dma_enabled()` inside `dma_on_run`.
        self.dma_on_run(mem);

        // Minimal DMA touch-point used by wrapper tests: read a dword from CRCR while the controller
        // is running.
        //
        // Gate this on `dma_enabled()` so wrappers can pass an open-bus/no-DMA `MemoryBus` without
        // the controller interpreting those reads as real guest memory.
        if (self.usbcmd & regs::USBCMD_RUN) != 0 && mem.dma_enabled() {
            // CRCR stores flags in the low bits; mask them away before using the pointer as a guest
            // physical address.
            let paddr = self.crcr & regs::CRCR_PTR_MASK;
            if paddr != 0 {
                self.last_tick_dma_dword = mem.read_u32(paddr);
            }
        }
    }

    pub fn mmio_read_u32(&mut self, offset: u64) -> u32 {
        self.mmio_read(offset, 4) as u32
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
        self.dcbaap = paddr & regs::DCBAAP_SNAPSHOT_MASK;
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

        // xHCI requires software to set CONFIG.MaxSlotsEn. For bring-up convenience we treat a
        // zero value as "use the architectural maximum" (32 slots).
        let mut max_slots_en = (self.config & 0xff) as usize;
        if max_slots_en == 0 {
            max_slots_en = usize::from(regs::MAX_SLOTS);
        }
        max_slots_en = max_slots_en
            .min(usize::from(regs::MAX_SLOTS))
            .min(self.slots.len().saturating_sub(1));

        let slot_id = match (1u8..)
            .take(max_slots_en)
            .find(|&id| !self.slots[usize::from(id)].enabled)
        {
            Some(id) => id,
            None => {
                return CommandCompletion::failure(CommandCompletionCode::NoSlotsAvailableError)
            }
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
        self.transfer_executors[usize::from(slot_id)] = None;
        self.clear_slot_pending_endpoints(slot_id);

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
        self.transfer_executors[idx] = None;
        if let Some(state) = self.ep0_control_td.get_mut(idx) {
            *state = ControlTdState::default();
        }
        self.clear_slot_pending_endpoints(slot_id);
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
    pub fn process_command_ring<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        max_trbs: usize,
    ) -> bool {
        if !mem.dma_enabled() {
            return true;
        }
        if self.host_controller_error {
            return true;
        }
        // Command-ring execution is gated on `USBCMD.RUN` (see xHCI spec). Keep the host-side test
        // harness consistent with the MMIO-driven command processor.
        if (self.usbcmd & regs::USBCMD_RUN) == 0 {
            return false;
        }
        let Some(mut cursor) = self.command_ring else {
            return true;
        };

        let max_trbs = max_trbs.min(MAX_COMMAND_TRBS_PER_CALL);
        for _ in 0..max_trbs {
            match cursor.poll(mem, COMMAND_RING_STEP_BUDGET) {
                RingPoll::Ready(item) => self.handle_command(mem, item.paddr, item.trb),
                RingPoll::NotReady => {
                    self.command_ring = Some(cursor);
                    return true;
                }
                RingPoll::Err(_) => {
                    // Malformed guest ring pointers/TRBs (e.g. Link TRB loops) should surface as a
                    // sticky host controller error so we stop processing further commands.
                    self.host_controller_error = true;
                    self.command_ring = Some(cursor);
                    return true;
                }
            }
        }

        self.command_ring = Some(cursor);
        false
    }

    fn process_command_ring_budgeted<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        max_trbs: usize,
        ring_poll_budget: &mut usize,
        work: &mut TickWork,
    ) -> bool {
        let Some(mut cursor) = self.command_ring else {
            return true;
        };

        for _ in 0..max_trbs {
            // If we cannot buffer another completion event, stop processing commands so we never
            // consume a command TRB without being able to report completion.
            if self.pending_events.len() >= MAX_PENDING_EVENTS {
                break;
            }
            if *ring_poll_budget == 0 {
                break;
            }

            let step_budget = (*ring_poll_budget).min(COMMAND_RING_STEP_BUDGET);
            let (poll, steps_used) = cursor.poll_counted(mem, step_budget);
            *ring_poll_budget = ring_poll_budget.saturating_sub(steps_used);
            work.ring_poll_steps = work.ring_poll_steps.saturating_add(steps_used);

            match poll {
                RingPoll::Ready(item) => {
                    self.handle_command(mem, item.paddr, item.trb);
                    work.command_trbs_processed = work.command_trbs_processed.saturating_add(1);
                }
                RingPoll::NotReady => {
                    self.command_ring = Some(cursor);
                    return true;
                }
                RingPoll::Err(RingError::StepBudgetExceeded) => {
                    // Malformed ring (or insufficient remaining ring-walk budget). Leave the ring
                    // "active" so the guest can recover or we can retry on a future tick.
                    break;
                }
                RingPoll::Err(_) => {
                    // Fatal ring error (bad link target, address overflow). Stop processing and
                    // surface a sticky host controller error bit.
                    self.host_controller_error = true;
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
            TrbType::EvaluateContextCommand => self.cmd_evaluate_context(mem, cmd_paddr, trb),
            TrbType::ConfigureEndpointCommand => self.cmd_configure_endpoint(mem, cmd_paddr, trb),
            TrbType::StopEndpointCommand => self.cmd_stop_endpoint(mem, cmd_paddr, trb),
            TrbType::ResetEndpointCommand => self.cmd_reset_endpoint(mem, cmd_paddr, trb),
            TrbType::SetTrDequeuePointerCommand => {
                self.cmd_set_tr_dequeue_pointer(mem, cmd_paddr, trb)
            }
            TrbType::NoOpCommand => self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::Success,
                trb.slot_id(),
            ),
            _ => self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::TrbError,
                trb.slot_id(),
            ),
        }
    }

    fn cmd_enable_slot<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64) {
        let result = self.enable_slot(mem);
        let (code, slot_id) = match result.completion_code {
            CommandCompletionCode::Success => (CompletionCode::Success, result.slot_id),
            CommandCompletionCode::ContextStateError => (CompletionCode::ContextStateError, 0),
            CommandCompletionCode::ParameterError => (CompletionCode::ParameterError, 0),
            CommandCompletionCode::NoSlotsAvailableError => {
                (CompletionCode::NoSlotsAvailableError, 0)
            }
            CommandCompletionCode::Unknown(_) => (CompletionCode::TrbError, 0),
        };
        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn cmd_disable_slot<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        let slot_id = trb.slot_id();
        let code = self.disable_slot(mem, slot_id);
        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn read_device_context_ptr_for_endpoint_cmd<M: MemoryBus + ?Sized>(
        &self,
        mem: &mut M,
        slot_id: u8,
    ) -> Result<u64, CompletionCode> {
        let slot_idx = usize::from(slot_id);
        if slot_id == 0 || slot_idx >= self.slots.len() || !self.slots[slot_idx].enabled {
            return Err(CompletionCode::SlotNotEnabledError);
        }

        let Some(dcbaa_entry) = self.dcbaap_entry_paddr(slot_id) else {
            return Err(CompletionCode::ContextStateError);
        };
        let dev_ctx_raw = mem.read_u64(dcbaa_entry);
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            return Err(CompletionCode::ContextStateError);
        }
        Ok(dev_ctx_ptr)
    }

    fn endpoint_context_ptr_for_cmd<M: MemoryBus + ?Sized>(
        &self,
        mem: &mut M,
        slot_id: u8,
        endpoint_id: u8,
    ) -> Result<(u64, u64), CompletionCode> {
        if endpoint_id == 0 || endpoint_id > 31 {
            return Err(CompletionCode::ParameterError);
        }
        let dev_ctx_ptr = self.read_device_context_ptr_for_endpoint_cmd(mem, slot_id)?;
        let offset = u64::from(endpoint_id) * CONTEXT_SIZE as u64;
        let ep_ctx_ptr = dev_ctx_ptr
            .checked_add(offset)
            .ok_or(CompletionCode::ParameterError)?;
        Ok((dev_ctx_ptr, ep_ctx_ptr))
    }

    fn cmd_stop_endpoint<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        const EP_STATE_STOPPED: u8 = 3;

        let slot_id = trb.slot_id();
        let endpoint_id = trb.endpoint_id();

        let code = match self.endpoint_context_ptr_for_cmd(mem, slot_id, endpoint_id) {
            Ok((dev_ctx_ptr, ep_ctx_ptr)) => {
                let mut ep_ctx = EndpointContext::read_from(mem, ep_ctx_ptr);
                if ep_ctx.endpoint_state() == 0 {
                    CompletionCode::EndpointNotEnabledError
                } else {
                    ep_ctx.set_endpoint_state(EP_STATE_STOPPED);
                    ep_ctx.write_to(mem, ep_ctx_ptr);

                    let slot_idx = usize::from(slot_id);
                    let slot = &mut self.slots[slot_idx];
                    slot.endpoint_contexts[usize::from(endpoint_id - 1)] = ep_ctx;
                    slot.device_context_ptr = dev_ctx_ptr;

                    CompletionCode::Success
                }
            }
            Err(code) => code,
        };
        if code == CompletionCode::Success {
            // If the endpoint was previously queued for execution (e.g. because the transfer ring had
            // more TRBs ready), stop should unschedule it immediately so it does not consume
            // per-tick doorbell budget while stopped.
            self.clear_endpoint_pending(slot_id, endpoint_id);
        }

        // xHCI completion events retain the original slot ID for endpoint commands.
        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn cmd_reset_endpoint<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        const EP_STATE_RUNNING: u8 = 1;

        let slot_id = trb.slot_id();
        let endpoint_id = trb.endpoint_id();

        let code = match self.endpoint_context_ptr_for_cmd(mem, slot_id, endpoint_id) {
            Ok((dev_ctx_ptr, ep_ctx_ptr)) => {
                let mut ep_ctx = EndpointContext::read_from(mem, ep_ctx_ptr);
                if ep_ctx.endpoint_state() == 0 {
                    CompletionCode::EndpointNotEnabledError
                } else {
                    ep_ctx.set_endpoint_state(EP_STATE_RUNNING);
                    ep_ctx.write_to(mem, ep_ctx_ptr);

                    let slot_idx = usize::from(slot_id);
                    let slot = &mut self.slots[slot_idx];
                    slot.endpoint_contexts[usize::from(endpoint_id - 1)] = ep_ctx;
                    slot.device_context_ptr = dev_ctx_ptr;

                    if endpoint_id == 1 {
                        if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
                            *state = ControlTdState::default();
                        }
                    }

                    // Clear any transfer-executor halted state for this endpoint so new doorbells can run.
                    if let Some(ep_addr) = Self::ep_addr_from_endpoint_id(endpoint_id) {
                        if let Some(slot_exec) = self.transfer_executors.get_mut(slot_idx) {
                            if let Some(mut exec) = slot_exec.take() {
                                exec.reset_endpoint(ep_addr);
                                *slot_exec = Some(exec);
                            }
                        }
                    }

                    CompletionCode::Success
                }
            }
            Err(code) => code,
        };

        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn cmd_set_tr_dequeue_pointer<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        cmd_paddr: u64,
        trb: Trb,
    ) {
        let slot_id = trb.slot_id();
        let endpoint_id = trb.endpoint_id();

        let code = match self.endpoint_context_ptr_for_cmd(mem, slot_id, endpoint_id) {
            Ok((dev_ctx_ptr, ep_ctx_ptr)) => {
                // Bits 1..=3 are reserved in the parameter field (bit0 is DCS).
                if (trb.parameter & 0x0e) != 0 {
                    CompletionCode::ParameterError
                } else if ((trb.status >> 16) & 0xffff) != 0 {
                    // Stream ID in DW2 bits 16..=31. Streams are not supported by this model.
                    CompletionCode::ParameterError
                } else {
                    let ptr = trb.parameter & !0x0f;
                    let dcs = (trb.parameter & 0x01) != 0;
                    if ptr == 0 {
                        return self.queue_command_completion_event(
                            cmd_paddr,
                            CompletionCode::ParameterError,
                            slot_id,
                        );
                    }

                    let mut ep_ctx = EndpointContext::read_from(mem, ep_ctx_ptr);
                    if ep_ctx.endpoint_state() == 0 {
                        CompletionCode::EndpointNotEnabledError
                    } else {
                        ep_ctx.set_tr_dequeue_pointer(ptr, dcs);
                        ep_ctx.write_to(mem, ep_ctx_ptr);

                        let slot_idx = usize::from(slot_id);
                        let slot = &mut self.slots[slot_idx];
                        slot.endpoint_contexts[usize::from(endpoint_id - 1)] = ep_ctx;
                        slot.transfer_rings[usize::from(endpoint_id - 1)] =
                            Some(RingCursor::new(ptr, dcs));
                        slot.device_context_ptr = dev_ctx_ptr;

                        if endpoint_id == 1 {
                            if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
                                *state = ControlTdState::default();
                            }
                        }

                        CompletionCode::Success
                    }
                }
            }
            Err(code) => code,
        };

        self.queue_command_completion_event(cmd_paddr, code, slot_id);
    }

    fn cmd_address_device<M: MemoryBus + ?Sized>(&mut self, mem: &mut M, cmd_paddr: u64, trb: Trb) {
        const CONTROL_BSR: u32 = 1 << 9;
        // Slot Context Slot State field (DW3 bits 27..=31) is xHC-owned.
        const SLOT_STATE_MASK_DWORD3: u32 = 0xf800_0000;
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

        let Some(slot_ctx_addr) = input_ctx_ptr.checked_add(CONTEXT_SIZE as u64) else {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        };
        let mut slot_ctx = SlotContext::read_from(mem, slot_ctx_addr);
        let Some(ep0_ctx_addr) = input_ctx_ptr.checked_add((2 * CONTEXT_SIZE) as u64) else {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        };
        let mut ep0_ctx = EndpointContext::read_from(mem, ep0_ctx_addr);

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
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            }
        };

        let Some(dcbaa_entry) = self.dcbaap_entry_paddr(slot_id) else {
            self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::ContextStateError,
                slot_id,
            );
            return;
        };
        let dev_ctx_raw = mem.read_u64(dcbaa_entry);
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::ContextStateError,
                slot_id,
            );
            return;
        }

        // Only issue SET_ADDRESS after we know the command has a valid output Device Context
        // pointer. If the command fails due to controller state (missing DCBAAP entry, invalid
        // pointer, etc.) the device must not observe any side effects.
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
                        UsbOutResult::Nak | UsbOutResult::Timeout => {
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
                    }

                    match dev.handle_in(0, 0) {
                        UsbInResult::Data(data) if data.is_empty() => {}
                        UsbInResult::Nak | UsbInResult::Timeout => {
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
        // xHCI assigns a device address (USB Device Address field) independent of whether it issues
        // the SET_ADDRESS request (BSR=1 blocks the request but does not block the assignment).
        //
        // This model uses the slot ID as the assigned address.
        slot_ctx.set_usb_device_address(slot_id);
        // Preserve the xHC-owned Slot State field from the existing output Slot Context so guests
        // cannot write arbitrary slot state values via the Address Device input context.
        let out_slot = SlotContext::read_from(mem, dev_ctx_ptr);
        let merged_dw3 = (slot_ctx.dword(3) & !SLOT_STATE_MASK_DWORD3)
            | (out_slot.dword(3) & SLOT_STATE_MASK_DWORD3);
        slot_ctx.set_dword(3, merged_dw3);
        // Address Device transitions the slot to Default/Addressed (xHCI 1.2 §4.6.5). Model the
        // BSR=1 variant as leaving the device in the Default state.
        slot_ctx.set_slot_state(if bsr {
            SLOT_STATE_DEFAULT
        } else {
            SLOT_STATE_ADDRESSED
        });

        // Endpoint state is controller-owned. Preserve the existing state if the output context
        // already has one, otherwise set the endpoint to Running.
        let Some(prev_ep0_addr) = dev_ctx_ptr.checked_add(CONTEXT_SIZE as u64) else {
            self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::ContextStateError,
                slot_id,
            );
            return;
        };
        let prev_ep0 = EndpointContext::read_from(mem, prev_ep0_addr);
        let prev_state = prev_ep0.endpoint_state();
        ep0_ctx.set_endpoint_state(if prev_state == 0 { 1 } else { prev_state });

        // Mirror contexts to the output Device Context.
        slot_ctx.write_to(mem, dev_ctx_ptr);
        let Some(out_ep0_addr) = dev_ctx_ptr.checked_add(CONTEXT_SIZE as u64) else {
            self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::ContextStateError,
                slot_id,
            );
            return;
        };
        ep0_ctx.write_to(mem, out_ep0_addr);

        let slot_state = &mut self.slots[slot_idx];
        slot_state.port_id = Some(port_id);
        slot_state.device_attached = true;
        slot_state.device_context_ptr = dev_ctx_ptr;
        slot_state.slot_context = slot_ctx;
        slot_state.endpoint_contexts[0] = ep0_ctx;
        slot_state.transfer_rings[0] =
            Some(RingCursor::new(ep0_ctx.tr_dequeue_pointer(), ep0_ctx.dcs()));
        if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
            *state = ControlTdState::default();
        }

        self.queue_command_completion_event(cmd_paddr, CompletionCode::Success, slot_id);
    }

    fn cmd_evaluate_context<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        cmd_paddr: u64,
        trb: Trb,
    ) {
        // Slot Context contains several xHC-owned fields that software must not modify:
        // - DW0 bits 0..=19: Route String (used by the controller to bind slot -> topology)
        // - DW0 bits 20..=23: Speed
        // - DW1 bits 16..=23: Root Hub Port Number (used by the controller to bind slot -> topology)
        // - DW3 bits 0..=7: USB Device Address
        // - DW3 bits 27..=31: Slot State
        //
        // Preserve those fields from the existing output Slot Context so Evaluate Context cannot
        // accidentally clear the assigned device address or speed.
        const SLOT_ROUTE_STRING_MASK_DWORD0: u32 = 0x000f_ffff;
        const SLOT_SPEED_MASK_DWORD0: u32 = 0x00f0_0000;
        const SLOT_ROOT_HUB_PORT_MASK_DWORD1: u32 = 0x00ff_0000;
        const SLOT_STATE_ADDR_MASK_DWORD3: u32 = 0xf800_00ff;

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

        let icc = InputControlContext::read_from(mem, input_ctx_ptr);
        if icc.drop_flags() != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        // MVP: only support updating Slot Context (bit0) and/or EP0 (bit1).
        if !icc.add_context(1) {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }
        let add = icc.add_flags();
        let supported = (1u32 << 0) | (1u32 << 1);
        if (add & !supported) != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(CommandCompletionCode::ParameterError) => {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            }
            Err(_) => {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ContextStateError,
                    slot_id,
                );
                return;
            }
        };

        if icc.add_context(0) {
            let Some(slot_ctx_addr) = input_ctx_ptr.checked_add(CONTEXT_SIZE as u64) else {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            };
            let mut slot_ctx = SlotContext::read_from(mem, slot_ctx_addr);
            let mut out_slot = SlotContext::read_from(mem, dev_ctx_ptr);
            if out_slot.root_hub_port_number() == 0 {
                out_slot = self.slots[slot_idx].slot_context;
            }

            let merged_dw0 = (slot_ctx.dword(0)
                & !(SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0))
                | (out_slot.dword(0) & (SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0));
            slot_ctx.set_dword(0, merged_dw0);

            let merged_dw1 = (slot_ctx.dword(1) & !SLOT_ROOT_HUB_PORT_MASK_DWORD1)
                | (out_slot.dword(1) & SLOT_ROOT_HUB_PORT_MASK_DWORD1);
            slot_ctx.set_dword(1, merged_dw1);

            let merged_dw3 = (slot_ctx.dword(3) & !SLOT_STATE_ADDR_MASK_DWORD3)
                | (out_slot.dword(3) & SLOT_STATE_ADDR_MASK_DWORD3);
            slot_ctx.set_dword(3, merged_dw3);
            slot_ctx.write_to(mem, dev_ctx_ptr);
            let slot_state = &mut self.slots[slot_idx];
            slot_state.slot_context = slot_ctx;
            slot_state.device_context_ptr = dev_ctx_ptr;
        }

        // MVP: update the EP0 interval/max-packet-size and TR Dequeue Pointer fields. Preserve
        // endpoint state and other xHC-owned fields in the output context.
        let Some(in_ep0_addr) = input_ctx_ptr.checked_add((2 * CONTEXT_SIZE) as u64) else {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        };
        let in_ep0 = EndpointContext::read_from(mem, in_ep0_addr);
        let Some(ep0_ctx_addr) = dev_ctx_ptr.checked_add(CONTEXT_SIZE as u64) else {
            self.queue_command_completion_event(
                cmd_paddr,
                CompletionCode::ContextStateError,
                slot_id,
            );
            return;
        };
        let mut ep0_ctx = EndpointContext::read_from(mem, ep0_ctx_addr);
        ep0_ctx.set_interval(in_ep0.interval());
        ep0_ctx.set_max_packet_size(in_ep0.max_packet_size());
        ep0_ctx.set_tr_dequeue_pointer_raw(in_ep0.tr_dequeue_pointer_raw());
        ep0_ctx.write_to(mem, ep0_ctx_addr);

        let slot_state = &mut self.slots[slot_idx];
        slot_state.endpoint_contexts[0] = ep0_ctx;
        slot_state.transfer_rings[0] =
            Some(RingCursor::new(ep0_ctx.tr_dequeue_pointer(), ep0_ctx.dcs()));
        slot_state.device_context_ptr = dev_ctx_ptr;
        if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
            *state = ControlTdState::default();
        }

        self.queue_command_completion_event(cmd_paddr, CompletionCode::Success, slot_id);
    }

    fn cmd_configure_endpoint<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
        cmd_paddr: u64,
        trb: Trb,
    ) {
        // Slot Context contains several xHC-owned fields that software must not modify:
        // - DW0 bits 0..=19: Route String (used by the controller to bind slot -> topology)
        // - DW0 bits 20..=23: Speed
        // - DW1 bits 16..=23: Root Hub Port Number (used by the controller to bind slot -> topology)
        // - DW3 bits 0..=7: USB Device Address
        // - DW3 bits 27..=31: Slot State
        //
        // Preserve those fields from controller-local state so Configure Endpoint does not
        // accidentally clear the slot's topology binding. This is especially important in unit
        // tests that use the host-side `address_device()` harness (which does not write the output
        // Slot Context into guest memory).
        const SLOT_ROUTE_STRING_MASK_DWORD0: u32 = 0x000f_ffff;
        const SLOT_SPEED_MASK_DWORD0: u32 = 0x00f0_0000;
        const SLOT_ROOT_HUB_PORT_MASK_DWORD1: u32 = 0x00ff_0000;
        const SLOT_STATE_ADDR_MASK_DWORD3: u32 = 0xf800_00ff;

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

        let icc = InputControlContext::read_from(mem, input_ctx_ptr);
        let drop_flags = icc.drop_flags();
        let add_flags = icc.add_flags();
        let deconfigure = trb.configure_endpoint_deconfigure();

        if !deconfigure {
            // Configure Endpoint supports both dropping and adding contexts. Reject commands that
            // do nothing so we don't treat malformed guest programming as success.
            if drop_flags == 0 && add_flags == 0 {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            }
        }

        let dev_ctx_ptr = match self.read_device_context_ptr(mem, slot_id) {
            Ok(ptr) => ptr,
            Err(CommandCompletionCode::ParameterError) => {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            }
            Err(_) => {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ContextStateError,
                    slot_id,
                );
                return;
            }
        };

        if deconfigure {
            // Deconfigure mode (xHCI 1.2 §6.4.3.5): disable all non-EP0 endpoints.
            //
            // Minimal semantics:
            // - Clear Endpoint Contexts for DCI 2..=31 in guest memory (Disabled state).
            // - Clear controller-local ring cursors and shadow endpoint contexts.
            // - Update Slot Context Context Entries to 1 (EP0 only).
            // - Drop any transfer-executor state so stale endpoints cannot poll.
            for endpoint_id in 2u8..=31 {
                let off = u64::from(endpoint_id) * (CONTEXT_SIZE as u64);
                let Some(out_addr) = dev_ctx_ptr.checked_add(off) else {
                    self.queue_command_completion_event(
                        cmd_paddr,
                        CompletionCode::ContextStateError,
                        slot_id,
                    );
                    return;
                };
                EndpointContext::default().write_to(mem, out_addr);
                let idx = usize::from(endpoint_id - 1);
                let slot_state = &mut self.slots[slot_idx];
                slot_state.endpoint_contexts[idx] = EndpointContext::default();
                slot_state.transfer_rings[idx] = None;
                slot_state.device_context_ptr = dev_ctx_ptr;
            }

            // Reflect EP0-only configuration in the Slot Context.
            let preserved = self.slots[slot_idx].slot_context;
            let mut slot_ctx = SlotContext::read_from(mem, dev_ctx_ptr);
            // Some harnesses bind slots via the host-side `address_device()` helper without writing
            // an output Device Context into guest memory. If the Slot Context in guest memory is
            // still zeroed, preserve the controller-local topology binding fields so the slot
            // remains routable after deconfigure.
            if slot_ctx.root_hub_port_number() == 0 {
                let merged_dw0 = (slot_ctx.dword(0)
                    & !(SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0))
                    | (preserved.dword(0)
                        & (SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0));
                slot_ctx.set_dword(0, merged_dw0);

                let merged_dw1 = (slot_ctx.dword(1) & !SLOT_ROOT_HUB_PORT_MASK_DWORD1)
                    | (preserved.dword(1) & SLOT_ROOT_HUB_PORT_MASK_DWORD1);
                slot_ctx.set_dword(1, merged_dw1);

                let merged_dw3 = (slot_ctx.dword(3) & !SLOT_STATE_ADDR_MASK_DWORD3)
                    | (preserved.dword(3) & SLOT_STATE_ADDR_MASK_DWORD3);
                slot_ctx.set_dword(3, merged_dw3);
            }
            slot_ctx.set_context_entries(1);
            // Deconfigure returns the slot to the Addressed state (xHCI 1.2 §6.4.3.5).
            slot_ctx.set_slot_state(SLOT_STATE_ADDRESSED);
            slot_ctx.write_to(mem, dev_ctx_ptr);
            {
                let slot_state = &mut self.slots[slot_idx];
                slot_state.slot_context = slot_ctx;
                slot_state.device_context_ptr = dev_ctx_ptr;
            }

            // Drop any cached endpoint execution state.
            if let Some(exec) = self.transfer_executors.get_mut(slot_idx) {
                *exec = None;
            }
            // Remove any non-EP0 endpoints from the active list so we stop polling immediately.
            self.active_endpoints
                .retain(|ep| ep.slot_id != slot_id || ep.endpoint_id == 1);
            // Keep the coalescing bitmap consistent with the queue. If we drop endpoints from the
            // queue without clearing their pending bits, future doorbells would be ignored.
            if slot_idx < self.active_endpoint_pending.len() {
                for endpoint_id in 2u8..=31 {
                    let ep_idx = endpoint_id as usize;
                    if ep_idx < 32 {
                        self.active_endpoint_pending[slot_idx][ep_idx] = false;
                    }
                }
            }

            self.queue_command_completion_event(cmd_paddr, CompletionCode::Success, slot_id);
            return;
        }

        // Slot Context / EP0 are not supported drop targets in this model.
        const DROP_DISALLOWED: u32 = (1u32 << 0) | (1u32 << 1);
        if (drop_flags & DROP_DISALLOWED) != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        // Reject contradictory add+drop for the same context.
        if (add_flags & drop_flags) != 0 {
            self.queue_command_completion_event(cmd_paddr, CompletionCode::ParameterError, slot_id);
            return;
        }

        // Slot Context (bit0) is optional.
        if icc.add_context(0) {
            let preserved = self.slots[slot_idx].slot_context;
            let Some(slot_ctx_addr) = input_ctx_ptr.checked_add(CONTEXT_SIZE as u64) else {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            };
            let mut slot_ctx = SlotContext::read_from(mem, slot_ctx_addr);
            let mut out_slot = SlotContext::read_from(mem, dev_ctx_ptr);
            if out_slot.root_hub_port_number() == 0 {
                out_slot = preserved;
            }

            let merged_dw0 = (slot_ctx.dword(0)
                & !(SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0))
                | (out_slot.dword(0) & (SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0));
            slot_ctx.set_dword(0, merged_dw0);

            let merged_dw1 = (slot_ctx.dword(1) & !SLOT_ROOT_HUB_PORT_MASK_DWORD1)
                | (out_slot.dword(1) & SLOT_ROOT_HUB_PORT_MASK_DWORD1);
            slot_ctx.set_dword(1, merged_dw1);

            let merged_dw3 = (slot_ctx.dword(3) & !SLOT_STATE_ADDR_MASK_DWORD3)
                | (out_slot.dword(3) & SLOT_STATE_ADDR_MASK_DWORD3);
            slot_ctx.set_dword(3, merged_dw3);

            slot_ctx.write_to(mem, dev_ctx_ptr);
            let slot_state = &mut self.slots[slot_idx];
            slot_state.slot_context = slot_ctx;
            slot_state.device_context_ptr = dev_ctx_ptr;
        }

        // Clear any dropped endpoint contexts first.
        if drop_flags != 0 {
            for endpoint_id in 2u8..=31 {
                if !icc.drop_context(endpoint_id) {
                    continue;
                }
                let off = u64::from(endpoint_id) * (CONTEXT_SIZE as u64);
                let Some(out_addr) = dev_ctx_ptr.checked_add(off) else {
                    self.queue_command_completion_event(
                        cmd_paddr,
                        CompletionCode::ContextStateError,
                        slot_id,
                    );
                    return;
                };
                EndpointContext::default().write_to(mem, out_addr);
                let idx = usize::from(endpoint_id - 1);
                let slot_state = &mut self.slots[slot_idx];
                slot_state.endpoint_contexts[idx] = EndpointContext::default();
                slot_state.transfer_rings[idx] = None;
                slot_state.device_context_ptr = dev_ctx_ptr;
            }

            // Drop any cached endpoint execution state since the executor cannot remove endpoints
            // individually.
            if let Some(exec) = self.transfer_executors.get_mut(slot_idx) {
                *exec = None;
            }
            self.active_endpoints
                .retain(|ep| ep.slot_id != slot_id || !icc.drop_context(ep.endpoint_id));
            // Keep the coalescing bitmap consistent with the queue so dropped endpoints can be
            // re-doorbelled later.
            if slot_idx < self.active_endpoint_pending.len() {
                for endpoint_id in 2u8..=31 {
                    if !icc.drop_context(endpoint_id) {
                        continue;
                    }
                    let ep_idx = endpoint_id as usize;
                    if ep_idx < 32 {
                        self.active_endpoint_pending[slot_idx][ep_idx] = false;
                    }
                }
            }
        }

        // Configure all added endpoint contexts.
        for endpoint_id in 1u8..=31 {
            if !icc.add_context(endpoint_id) {
                continue;
            }

            // Input context layout: [ICC][Slot][EP0][EP1 OUT][EP1 IN]...
            let input_off = (endpoint_id as u64 + 1) * CONTEXT_SIZE as u64;
            let Some(input_addr) = input_ctx_ptr.checked_add(input_off) else {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ParameterError,
                    slot_id,
                );
                return;
            };
            let mut ep_ctx = EndpointContext::read_from(mem, input_addr);

            // Endpoint state is controller-owned. Preserve an existing non-zero state (e.g.
            // Stopped/Halted) so Configure Endpoint does not implicitly clear error conditions.
            let out_off = u64::from(endpoint_id) * (CONTEXT_SIZE as u64);
            let Some(out_addr) = dev_ctx_ptr.checked_add(out_off) else {
                self.queue_command_completion_event(
                    cmd_paddr,
                    CompletionCode::ContextStateError,
                    slot_id,
                );
                return;
            };
            let prev = EndpointContext::read_from(mem, out_addr);
            let prev_state = prev.endpoint_state();
            ep_ctx.set_endpoint_state(if prev_state == 0 { 1 } else { prev_state });

            ep_ctx.write_to(mem, out_addr);

            let slot_state = &mut self.slots[slot_idx];
            let idx = usize::from(endpoint_id - 1);
            slot_state.endpoint_contexts[idx] = ep_ctx;
            slot_state.transfer_rings[idx] =
                Some(RingCursor::new(ep_ctx.tr_dequeue_pointer(), ep_ctx.dcs()));
            slot_state.device_context_ptr = dev_ctx_ptr;
            if endpoint_id == 1 {
                if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
                    *state = ControlTdState::default();
                }
            }
        }

        // Configure Endpoint transitions the slot to the Configured state (xHCI 1.2 §6.4.3.5).
        let preserved = self.slots[slot_idx].slot_context;
        let mut slot_ctx = SlotContext::read_from(mem, dev_ctx_ptr);
        // Like the deconfigure path, avoid clobbering controller-local topology state if the guest
        // Slot Context has not been initialised in memory (common in host-side harnesses that call
        // `address_device()` directly).
        if slot_ctx.root_hub_port_number() == 0 {
            let merged_dw0 = (slot_ctx.dword(0)
                & !(SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0))
                | (preserved.dword(0) & (SLOT_ROUTE_STRING_MASK_DWORD0 | SLOT_SPEED_MASK_DWORD0));
            slot_ctx.set_dword(0, merged_dw0);

            let merged_dw1 = (slot_ctx.dword(1) & !SLOT_ROOT_HUB_PORT_MASK_DWORD1)
                | (preserved.dword(1) & SLOT_ROOT_HUB_PORT_MASK_DWORD1);
            slot_ctx.set_dword(1, merged_dw1);

            let merged_dw3 = (slot_ctx.dword(3) & !SLOT_STATE_ADDR_MASK_DWORD3)
                | (preserved.dword(3) & SLOT_STATE_ADDR_MASK_DWORD3);
            slot_ctx.set_dword(3, merged_dw3);
        }
        slot_ctx.set_slot_state(SLOT_STATE_CONFIGURED);
        slot_ctx.write_to(mem, dev_ctx_ptr);
        self.slots[slot_idx].slot_context = slot_ctx;
        self.slots[slot_idx].device_context_ptr = dev_ctx_ptr;

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

        // Resolve the bound device so we can derive xHC-owned Slot Context fields (speed, address,
        // slot state).
        let expected_speed = match self.find_device_by_topology(root_port, &route) {
            Some(dev) => port::port_speed_id(dev.speed()),
            None => return CommandCompletion::failure(CommandCompletionCode::ContextStateError),
        };

        let mut slot_ctx = slot_ctx;
        // Mirror controller-owned Slot Context fields to better match Address Device semantics.
        slot_ctx.set_speed(expected_speed);
        slot_ctx.set_usb_device_address(slot_id);
        slot_ctx.set_slot_state(SLOT_STATE_ADDRESSED);

        {
            let slot = &mut self.slots[idx];
            slot.port_id = Some(root_port);
            slot.device_attached = true;
            slot.slot_context = slot_ctx;
        }
        // The slot's bound device may have changed; drop any existing transfer executor so it is
        // recreated with a fresh device-model pointer on the next doorbell.
        self.transfer_executors[idx] = None;

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
        let completion = self.address_device(slot_id, slot_ctx);
        if completion.completion_code == CommandCompletionCode::Success {
            let idx = usize::from(slot_id);
            if let Some(slot) = self.slots.get_mut(idx) {
                slot.slot_context.set_slot_state(SLOT_STATE_CONFIGURED);
            }
        }
        completion
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

    fn read_device_context_ptr<M: MemoryBus + ?Sized>(
        &self,
        mem: &mut M,
        slot_id: u8,
    ) -> Result<u64, CommandCompletionCode> {
        let dcbaap = self
            .dcbaap()
            .ok_or(CommandCompletionCode::ContextStateError)?;
        let dcbaa = Dcbaa::new(dcbaap);
        let dev_ctx_raw = dcbaa
            .read_device_context_ptr(mem, slot_id)
            .map_err(|_| CommandCompletionCode::ParameterError)?;
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            return Err(CommandCompletionCode::ContextStateError);
        }
        Ok(dev_ctx_ptr)
    }

    /// Stop an endpoint (MVP semantics).
    ///
    /// Updates the Endpoint Context Endpoint State field to `Stopped (3)` and preserves all other
    /// fields. If the device context pointer is missing, returns `ContextStateError`.
    pub fn stop_endpoint<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
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

        // Clear any transfer-executor halted state for this endpoint so new doorbells can run.
        if let Some(ep_addr) = Self::ep_addr_from_endpoint_id(endpoint_id) {
            if let Some(slot_exec) = self.transfer_executors.get_mut(slot_idx) {
                if let Some(mut exec) = slot_exec.take() {
                    exec.reset_endpoint(ep_addr);
                    *slot_exec = Some(exec);
                }
            }
        }

        // Stop should immediately unschedule the endpoint if it was previously queued (e.g. because
        // the transfer ring had more TRBs ready). This prevents stopped endpoints from consuming
        // per-tick doorbell budget.
        self.clear_endpoint_pending(slot_id, endpoint_id);

        CommandCompletion::success(slot_id)
    }

    /// Reset an endpoint (MVP semantics).
    ///
    /// Clears a halted/stopped endpoint and allows transfers again by setting the Endpoint State to
    /// `Running (1)`.
    pub fn reset_endpoint<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
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
        if endpoint_id == 1 {
            if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
                *state = ControlTdState::default();
            }
        }

        // Clear any transfer-executor halted state for this endpoint so new doorbells can run.
        if let Some(ep_addr) = Self::ep_addr_from_endpoint_id(endpoint_id) {
            if let Some(slot_exec) = self.transfer_executors.get_mut(slot_idx) {
                if let Some(mut exec) = slot_exec.take() {
                    exec.reset_endpoint(ep_addr);
                    *slot_exec = Some(exec);
                }
            }
        }

        CommandCompletion::success(slot_id)
    }

    /// Set Transfer Ring Dequeue Pointer (MVP semantics).
    ///
    /// Updates the Endpoint Context TR Dequeue Pointer and internal transfer ring cursor state.
    pub fn set_tr_dequeue_pointer<M: MemoryBus + ?Sized>(
        &mut self,
        mem: &mut M,
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
        if endpoint_id == 1 {
            if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
                *state = ControlTdState::default();
            }
        }

        CommandCompletion::success(slot_id)
    }

    fn sync_command_ring_from_crcr(&mut self) {
        // CRCR bits 63:6 contain the ring pointer; bits 3:0 contain flags (RCS/CS/CA/CRR).
        // Preserve guest-writable flag bits while masking the pointer to the required alignment.
        // CRCR.CRR (bit 3) is read-only and is therefore forced to 0 in the stored register value.
        let flags = self.crcr & 0x07;
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
            let mut flags = self.crcr & 0x06;
            if ring.cycle_state() {
                flags |= 0x1;
            }
            self.crcr = ptr | flags;
        }
    }

    fn ring_doorbell0(&mut self) {
        self.cmd_kick = true;
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
    pub fn set_endpoint_ring(
        &mut self,
        slot_id: u8,
        endpoint_id: u8,
        dequeue_ptr: u64,
        cycle: bool,
    ) {
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

    fn clear_slot_pending_endpoints(&mut self, slot_id: u8) {
        let idx = usize::from(slot_id);
        if idx < self.active_endpoint_pending.len() {
            self.active_endpoint_pending[idx] = [false; 32];
        }
        self.active_endpoints.retain(|ep| ep.slot_id != slot_id);
    }

    fn clear_endpoint_pending(&mut self, slot_id: u8, endpoint_id: u8) {
        if slot_id == 0 || endpoint_id == 0 || endpoint_id > 31 {
            return;
        }
        let slot_idx = usize::from(slot_id);
        let ep_idx = endpoint_id as usize;
        if slot_idx < self.active_endpoint_pending.len() && ep_idx < 32 {
            self.active_endpoint_pending[slot_idx][ep_idx] = false;
        }
        self.active_endpoints
            .retain(|ep| ep.slot_id != slot_id || ep.endpoint_id != endpoint_id);
    }

    /// Handle a device endpoint doorbell write.
    ///
    /// `target` corresponds to the doorbell register index (slot id). For non-zero targets, the
    /// low 8 bits of `value` contain the endpoint ID (DCI).
    pub fn write_doorbell(&mut self, target: u8, value: u32) {
        // After Host Controller Error is latched, the controller is considered halted until reset.
        // Ignore further doorbells so a guest cannot keep queueing work for rings we will not
        // execute.
        if self.host_controller_error {
            return;
        }
        if target == 0 {
            // Doorbell 0 rings the command ring (command TRBs).
            // The low bits of `value` are reserved for doorbell 0, so ignore it.
            self.ring_doorbell0();
            return;
        }
        let endpoint_id = (value & 0xff) as u8;
        self.ring_doorbell(target, endpoint_id);
    }

    /// Ring a device endpoint doorbell.
    ///
    /// This marks the endpoint as active. [`XhciController::tick`] will process pending work.
    pub fn ring_doorbell(&mut self, slot_id: u8, endpoint_id: u8) {
        if self.host_controller_error {
            return;
        }
        if slot_id == 0 {
            return;
        }
        // Doorbell target values outside 1..=31 are reserved by xHCI. Ignore them rather than
        // masking so a guest cannot accidentally (or maliciously) alias an invalid target onto a
        // real endpoint ID.
        if endpoint_id == 0 || endpoint_id > 31 {
            return;
        }

        let slot_idx = usize::from(slot_id);
        if slot_idx >= self.slots.len() {
            return;
        }

        let slot = &self.slots[slot_idx];
        if !slot.enabled || !slot.device_attached {
            return;
        }

        // Ignore doorbells for halted/stopped endpoints so guests cannot keep re-queueing work for a
        // ring we have already faulted.
        const EP_STATE_HALTED: u8 = 2;
        const EP_STATE_STOPPED: u8 = 3;
        let idx = usize::from(endpoint_id.saturating_sub(1));
        if let Some(ctx) = slot.endpoint_contexts.get(idx) {
            let state = ctx.endpoint_state();
            if state == EP_STATE_HALTED || state == EP_STATE_STOPPED {
                return;
            }
        }

        let ep_idx = endpoint_id as usize;
        if self.active_endpoint_pending[slot_idx][ep_idx] {
            return;
        }
        self.active_endpoint_pending[slot_idx][ep_idx] = true;
        self.active_endpoints.push_back(ActiveEndpoint {
            slot_id,
            endpoint_id,
        });
    }

    /// Process active endpoints.
    ///
    /// This is intentionally bounded to avoid guest-induced hangs (e.g. malformed transfer rings).
    pub fn tick(&mut self, mem: &mut dyn MemoryBus) {
        if !mem.dma_enabled() {
            return;
        }
        if (self.usbcmd & regs::USBCMD_RUN) == 0 {
            return;
        }
        if self.host_controller_error {
            return;
        }
        // Transfer execution is gated on `USBCMD.RUN` (see xHCI spec). Guests may ring endpoint
        // doorbells while the controller is stopped; those doorbells should be remembered, but no
        // DMA or ring progress should occur until the controller is running again.
        if (self.usbcmd & regs::USBCMD_RUN) == 0 {
            return;
        }

        let mut work = TickWork::default();
        let mut ring_poll_budget = budget::MAX_RING_POLL_STEPS_PER_FRAME;
        self.tick_transfer_rings_budgeted(
            mem,
            budget::MAX_TRANSFER_TRBS_PER_FRAME,
            budget::MAX_DOORBELLS_PER_FRAME,
            &mut ring_poll_budget,
            &mut work,
        );
    }

    fn tick_transfer_rings_budgeted(
        &mut self,
        mem: &mut dyn MemoryBus,
        max_trbs: usize,
        max_doorbells: usize,
        ring_poll_budget: &mut usize,
        work: &mut TickWork,
    ) {
        if self.host_controller_error {
            return;
        }
        // Endpoint transfers only make progress while the controller is running.
        //
        // Guests can ring endpoint doorbells while RUN=0; we keep the coalesced
        // `active_endpoints` queue intact so those doorbells can be serviced once the guest
        // sets USBCMD.RUN again.
        if (self.usbcmd & regs::USBCMD_RUN) == 0 {
            return;
        }

        let mut trb_budget = max_trbs;
        // Process each currently-active endpoint at most once per tick. This ensures deterministic
        // work bounds and prevents a single always-ready endpoint from consuming the entire budget
        // (or running multiple TDs) in one frame.
        let max_endpoints = self.active_endpoints.len().min(max_doorbells);

        for _ in 0..max_endpoints {
            if trb_budget == 0 || *ring_poll_budget == 0 {
                break;
            }
            let Some(ep) = self.active_endpoints.pop_front() else {
                break;
            };

            work.doorbells_serviced += 1;

            let slot_idx = usize::from(ep.slot_id);
            let endpoint_idx = ep.endpoint_id as usize;

            let outcome = self.process_endpoint(
                mem,
                ep.slot_id,
                ep.endpoint_id,
                trb_budget,
                ring_poll_budget,
                work,
            );
            work.transfer_trbs_consumed = work
                .transfer_trbs_consumed
                .saturating_add(outcome.trbs_consumed);
            trb_budget = trb_budget.saturating_sub(outcome.trbs_consumed);

            if outcome.keep_active {
                // Keep the endpoint scheduled for a future tick.
                self.active_endpoints.push_back(ep);
            } else if slot_idx < self.active_endpoint_pending.len() && endpoint_idx < 32 {
                self.active_endpoint_pending[slot_idx][endpoint_idx] = false;
            }
        }
    }

    /// Returns whether the IRQ line for interrupter 0 should be asserted.
    pub fn irq_level(&self) -> bool {
        self.interrupter0.interrupt_enable() && self.interrupter0.interrupt_pending()
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
        let _ = self.service_event_ring_budgeted(mem, EVENT_ENQUEUE_BUDGET_PER_TICK);
    }

    fn service_event_ring_budgeted(&mut self, mem: &mut dyn MemoryBus, budget: usize) -> usize {
        if !mem.dma_enabled() {
            return 0;
        }
        if self.host_controller_error {
            return 0;
        }
        let mut written = 0usize;
        self.event_ring.refresh(mem, &self.interrupter0);

        for _ in 0..budget {
            let Some(&trb) = self.pending_events.front() else {
                break;
            };

            match self.event_ring.try_enqueue(mem, &self.interrupter0, trb) {
                Ok(()) => {
                    self.pending_events.pop_front();
                    self.interrupter0.set_interrupt_pending(true);
                    written += 1;
                }
                Err(event_ring::EnqueueError::NotConfigured)
                | Err(event_ring::EnqueueError::RingFull) => break,
                Err(event_ring::EnqueueError::InvalidConfig) => {
                    // Malformed guest configuration (e.g. ERST points out of bounds) should not
                    // panic; instead surface Host Controller Error as a sticky flag.
                    self.host_controller_error = true;
                    break;
                }
            }
        }

        written
    }

    pub fn dropped_event_trbs(&self) -> u64 {
        self.dropped_event_trbs
    }

    /// Returns the device currently attached to the specified root hub port (0-based).
    ///
    /// This accessor ignores port enable state and is intended for host-side topology management
    /// and tests.
    pub fn port_device(&self, port: usize) -> Option<&AttachedUsbDevice> {
        self.ports.get(port)?.device()
    }

    /// Mutable variant of [`XhciController::port_device`].
    pub fn port_device_mut(&mut self, port: usize) -> Option<&mut AttachedUsbDevice> {
        self.ports.get_mut(port)?.device_mut()
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

    fn tick_ports_1ms(&mut self) {
        self.mfindex = self.mfindex.wrapping_add(8) & 0x3fff;
        self.time_ms = self.time_ms.wrapping_add(1);

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

    /// Advances controller internal time by 1ms without performing any DMA.
    ///
    /// This is useful for integrations that want port timers/MFINDEX to advance even when PCI Bus
    /// Master Enable is disabled.
    pub fn tick_1ms_no_dma(&mut self) {
        self.tick_ports_1ms();
    }

    /// Advances controller internal time by 1ms and performs a bounded amount of controller work.
    ///
    /// This method performs DMA into guest memory (transfer buffers + event ring). Callers should
    /// gate this on PCI Bus Master Enable.
    pub fn tick_1ms(&mut self, mem: &mut dyn MemoryBus) {
        let _ = self.step_1ms(mem);
    }

    /// Advance the controller by one 1ms frame with deterministic internal work budgets.
    ///
    /// This processes (in order):
    /// - port timers,
    /// - the command ring (if doorbell 0 was rung),
    /// - active transfer endpoints,
    /// - and event ring delivery.
    ///
    /// All ring walking is bounded by per-frame budgets so a guest cannot force unbounded work in a
    /// single tick.
    pub fn step_1ms(&mut self, mem: &mut dyn MemoryBus) -> TickWork {
        let mut work = TickWork::default();
        let mut ring_poll_budget = budget::MAX_RING_POLL_STEPS_PER_FRAME;

        // Always advance the controller time base.
        self.tick_1ms_with_dma(mem);

        if !mem.dma_enabled() {
            return work;
        }

        // Command ring processing is gated on `USBCMD.RUN` (a halted controller does not execute
        // commands, even if the guest rings doorbell 0).
        if self.cmd_kick && (self.usbcmd & regs::USBCMD_RUN) != 0 && !self.host_controller_error {
            let ring_empty = self.process_command_ring_budgeted(
                mem,
                budget::MAX_COMMAND_TRBS_PER_FRAME,
                &mut ring_poll_budget,
                &mut work,
            );
            self.sync_crcr_from_command_ring();
            if ring_empty {
                self.cmd_kick = false;
            }
        } else if self.host_controller_error {
            self.cmd_kick = false;
        }

        // Transfer-ring execution is gated on `USBCMD.RUN` (a halted controller does not execute
        // transfers, even if the guest rings endpoint doorbells).
        if (self.usbcmd & regs::USBCMD_RUN) != 0 {
            self.tick_transfer_rings_budgeted(
                mem,
                budget::MAX_TRANSFER_TRBS_PER_FRAME,
                budget::MAX_DOORBELLS_PER_FRAME,
                &mut ring_poll_budget,
                &mut work,
            );
        }

        work.event_trbs_written =
            self.service_event_ring_budgeted(mem, budget::MAX_EVENT_TRBS_PER_FRAME);
        work
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
        self.tick_1ms(mem);
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
        let mut detached_slots: Vec<u8> = Vec::new();
        for (slot_id, slot) in self.slots.iter_mut().enumerate().skip(1) {
            if slot.enabled && slot.port_id == Some(port_id) {
                slot.device_attached = false;
                detached_slots.push(slot_id as u8);
            }
        }
        for slot_id in detached_slots {
            let idx = usize::from(slot_id);
            if idx < self.transfer_executors.len() {
                self.transfer_executors[idx] = None;
            }
            if let Some(state) = self.ep0_control_td.get_mut(idx) {
                *state = ControlTdState::default();
            }
            self.clear_slot_pending_endpoints(slot_id);
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
            | (supported_protocol_offset_dwords << 8)
            | regs::USBLEGSUP_OS_OWNED;
        caps.push(usb_legsup);
        caps.push(0);

        // Supported Protocol Capability for USB 2.0.
        //
        // The roothub port range is 1-based, so we expose all ports as a single USB 2.0 range.
        let psic = 3u8; // low/full/high-speed entries.
        let header0 = (regs::EXT_CAP_ID_SUPPORTED_PROTOCOL as u32)
            // Next pointer: 0 => end of list.
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
            (psic as u32) | ((regs::USB2_PROTOCOL_SLOT_TYPE as u32) << 8) | ((psio as u32) << 16),
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

    fn usbsts_read(&self) -> u32 {
        // Keep derived bits out of the stored `usbsts` field.
        let mut v = self.usbsts & !(regs::USBSTS_EINT | regs::USBSTS_HCH | regs::USBSTS_HCE);

        // HCHalted reports the controller's execution state. RUN=0 => halted, but a fatal Host
        // Controller Error can also halt execution while RUN remains set.
        if (self.usbcmd & regs::USBCMD_RUN) == 0 || self.host_controller_error {
            v |= regs::USBSTS_HCH;
        }

        // Reflect interrupter pending in USBSTS.EINT for drivers.
        if self.interrupter0.interrupt_pending() {
            v |= regs::USBSTS_EINT;
        }

        if self.host_controller_error {
            v |= regs::USBSTS_HCE;
        }

        // xHCI reserved bits read as 0; ensure any host/test-provided values do not leak into the
        // guest-visible register surface.
        v & regs::USBSTS_SNAPSHOT_MASK
    }

    fn mmio_read_u8(&self, offset: u64) -> u8 {
        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let port_regs_base = regs::REG_USBCMD + regs::port::PORTREGS_BASE;
        let port_regs_end =
            port_regs_base + u64::from(self.port_count) * regs::port::PORTREGS_STRIDE;
        let value32 = match aligned {
            off if off >= port_regs_base && off < port_regs_end => {
                let rel = off - port_regs_base;
                let port = (rel / regs::port::PORTREGS_STRIDE) as usize;
                let reg_off = rel % regs::port::PORTREGS_STRIDE;
                match reg_off {
                    regs::port::PORTSC => {
                        self.ports.get(port).map(|p| p.read_portsc()).unwrap_or(0)
                    }
                    _ => 0,
                }
            }
            regs::REG_CAPLENGTH_HCIVERSION => regs::CAPLENGTH_HCIVERSION,
            regs::REG_HCSPARAMS1 => {
                // HCSPARAMS1: MaxSlots (7:0), MaxIntrs (18:8), MaxPorts (31:24).
                let max_slots = u32::from(regs::MAX_SLOTS);
                let max_intrs = 1u32;
                let max_ports = self.port_count as u32;
                (max_slots & 0xff) | ((max_intrs & 0x7ff) << 8) | ((max_ports & 0xff) << 24)
            }
            regs::REG_HCSPARAMS2 => 0,
            regs::REG_HCSPARAMS3 => 0,
            regs::REG_HCCPARAMS1 => {
                // HCCPARAMS1.xECP: offset (in DWORDs) to the xHCI Extended Capabilities list.
                let xecp_dwords = (regs::EXT_CAPS_OFFSET_BYTES / 4) & 0xffff;
                // CSZ=0 => 32-byte contexts (MVP).
                (xecp_dwords << 16) & !regs::HCCPARAMS1_CSZ_64B
            }
            regs::REG_DBOFF => regs::DBOFF_VALUE,
            regs::REG_RTSOFF => regs::RTSOFF_VALUE,
            regs::REG_HCCPARAMS2 => 0,
            off if off >= regs::EXT_CAPS_OFFSET_BYTES as u64
                && off
                    < regs::EXT_CAPS_OFFSET_BYTES as u64
                        + (self.ext_caps.len().saturating_mul(4) as u64) =>
            {
                let idx = (off - regs::EXT_CAPS_OFFSET_BYTES as u64) / 4;
                self.ext_caps.get(idx as usize).copied().unwrap_or(0)
            }

            regs::REG_USBCMD => self.usbcmd,
            regs::REG_USBSTS => self.usbsts_read(),
            regs::REG_PAGESIZE => regs::PAGESIZE_4K,
            regs::REG_DNCTRL => self.dnctrl,
            regs::REG_CRCR_LO => (self.crcr & 0xffff_ffff) as u32,
            regs::REG_CRCR_HI => (self.crcr >> 32) as u32,
            regs::REG_DCBAAP_LO => (self.dcbaap & 0xffff_ffff) as u32,
            regs::REG_DCBAAP_HI => (self.dcbaap >> 32) as u32,
            regs::REG_CONFIG => self.config,

            // Runtime registers.
            regs::REG_MFINDEX => self.mfindex & 0x3fff,
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
    pub fn mmio_read(&mut self, offset: u64, size: usize) -> u64 {
        // Treat invalid/out-of-range reads as open bus.
        let open_bus = all_ones(size);
        if !matches!(size, 1 | 2 | 4 | 8) {
            return open_bus;
        }
        let Some(end) = offset.checked_add(size as u64) else {
            return open_bus;
        };
        if end > u64::from(Self::MMIO_SIZE) {
            return open_bus;
        }

        // Read per-byte so unaligned/cross-dword reads behave like normal little-endian memory.
        // This is more robust against guests doing odd-sized or misaligned accesses.
        let mut out = 0u64;
        for i in 0..size {
            let Some(off) = offset.checked_add(i as u64) else {
                break;
            };
            let byte = self.mmio_read_u8(off);
            out |= (byte as u64) << (i * 8);
        }

        out & open_bus
    }

    /// Write to the controller's MMIO register space.
    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        if !matches!(size, 1 | 2 | 4 | 8) {
            return;
        }
        let Some(end) = offset.checked_add(size as u64) else {
            return;
        };
        if end > u64::from(Self::MMIO_SIZE) {
            return;
        }

        // Handle 64-bit accesses explicitly. Prefer splitting into two natural dword writes when
        // aligned so side effects are closer to what real hardware would expose.
        if size == 8 {
            if (offset & 3) == 0 {
                self.mmio_write(offset, 4, value & 0xffff_ffff);
                self.mmio_write(offset + 4, 4, value >> 32);
            } else {
                for i in 0..8usize {
                    let Some(off) = offset.checked_add(i as u64) else {
                        break;
                    };
                    let byte = (value >> (i * 8)) & 0xff;
                    self.mmio_write(off, 1, byte);
                }
            }
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
                let byte = (value >> (i * 8)) & 0xff;
                self.mmio_write(off, 1, byte);
            }
            return;
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let value_u32 = value as u32;
        let (mask, value_shifted) = match size {
            1 => (0xffu32 << shift, (value_u32 & 0xff) << shift),
            2 => (0xffffu32 << shift, (value_u32 & 0xffff) << shift),
            4 => (u32::MAX, value_u32),
            _ => return,
        };

        let portregs_base = regs::REG_USBCMD + regs::port::PORTREGS_BASE;
        let portregs_end = portregs_base + u64::from(self.port_count) * regs::port::PORTREGS_STRIDE;

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
                let target =
                    ((off - doorbell_base) / u64::from(regs::doorbell::DOORBELL_STRIDE)) as u8;
                let write_val = merge(0);
                self.write_doorbell(target, write_val);
            }
            regs::REG_USBCMD => {
                let prev = self.usbcmd;
                let next = merge(self.usbcmd);

                // Host Controller Reset (HCRST) is write-1-to-reset and is self-clearing.
                if (next & regs::USBCMD_HCRST) != 0 {
                    self.reset_controller();
                    return;
                }

                // Ignore HCRST bit (it is not persistent).
                self.usbcmd = next & !regs::USBCMD_HCRST;

                // On the rising edge of RUN, schedule a small DMA read (performed by tick_1ms) to
                // validate PCI Bus Master Enable (BME) gating in the wrapper.
                let was_running = (prev & regs::USBCMD_RUN) != 0;
                let now_running = (self.usbcmd & regs::USBCMD_RUN) != 0;
                if was_running && !now_running {
                    // Dropping RUN cancels any deferred DMA-on-RUN probe.
                    self.pending_dma_on_run = false;
                } else if !was_running && now_running {
                    // Latch the rising edge. The probe will be executed by `tick_1ms_with_dma` once
                    // DMA is available.
                    self.pending_dma_on_run = true;
                }
            }
            regs::REG_USBSTS => {
                // Treat USBSTS as RW1C. Writing 1 clears the bit.
                let write_val = merge(0);
                if (write_val & regs::USBSTS_EINT) != 0 {
                    // Allow acknowledging event interrupts via USBSTS.EINT by also clearing
                    // Interrupter 0's pending bit (IMAN.IP). This is a minimal model of the xHCI
                    // "summary" interrupt status bit.
                    self.interrupter0.set_interrupt_pending(false);
                }
                self.usbsts &= !write_val;
            }
            regs::REG_DNCTRL => {
                self.dnctrl = merge(self.dnctrl);
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
                self.dcbaap &= regs::DCBAAP_SNAPSHOT_MASK;
            }
            regs::REG_DCBAAP_HI => {
                let hi = merge((self.dcbaap >> 32) as u32) as u64;
                self.dcbaap = (self.dcbaap & 0x0000_0000_ffff_ffff) | (hi << 32);
                self.dcbaap &= regs::DCBAAP_SNAPSHOT_MASK;
            }
            regs::REG_CONFIG => {
                let mut v = merge(self.config);
                // xHCI spec: CONFIG bits 7:0 = MaxSlotsEn, bits 9:8 are used for optional features.
                // Clamp MaxSlotsEn so the value remains self-consistent with HCSPARAMS1.MaxSlots.
                v &= 0x3ff;
                let max_slots_en = (v & 0xff) as u8;
                let max_slots_en = max_slots_en.min(regs::MAX_SLOTS);
                self.config = (v & !0xff) | u32::from(max_slots_en);
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
            _ => {}
        }
    }

    fn reset_controller(&mut self) {
        self.usbcmd = 0;
        self.usbsts = 0;
        self.host_controller_error = false;
        self.dnctrl = 0;
        self.crcr = 0;
        self.dcbaap = 0;
        self.config = 0;
        self.command_ring = None;
        self.cmd_kick = false;
        self.mfindex = 0;
        self.time_ms = 0;
        self.last_tick_dma_dword = 0;
        self.pending_dma_on_run = false;

        for slot in self.slots.iter_mut() {
            *slot = SlotState::default();
        }
        for td in self.ep0_control_td.iter_mut() {
            *td = ControlTdState::default();
        }
        for exec in self.transfer_executors.iter_mut() {
            *exec = None;
        }

        for port in self.ports.iter_mut() {
            port.host_controller_reset();
        }

        self.interrupter0 = InterrupterRegs::default();
        self.event_ring = EventRingProducer::default();
        self.pending_events.clear();
        self.dropped_event_trbs = 0;
        self.active_endpoints.clear();
        self.active_endpoint_pending = [[false; 32]; 256];
    }

    fn queue_port_status_change_event(&mut self, port: usize) {
        let port_id = (port + 1) as u8;
        self.post_event(make_port_status_change_event_trb(port_id));
    }

    fn process_endpoint(
        &mut self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
        trb_budget: usize,
        ring_poll_budget: &mut usize,
        work: &mut TickWork,
    ) -> EndpointOutcome {
        let slot_idx = usize::from(slot_id);
        let Some(slot) = self.slots.get(slot_idx) else {
            return EndpointOutcome::idle();
        };
        if !slot.enabled || !slot.device_attached {
            return EndpointOutcome::idle();
        }

        // Stop/Reset Endpoint commands update the Endpoint Context "Endpoint State" field. Enforce
        // basic doorbell gating semantics: only Running endpoints execute transfers. If the Endpoint
        // Context cannot be read (e.g. test harnesses that only configure controller-local ring
        // cursors), fall back to the legacy behavior and allow progress.
        // Always respect controller-local shadow halt/stop state. This protects against malformed
        // snapshots (or hostile guests) that might otherwise leave a halted endpoint queued in
        // `active_endpoints` while the guest Device Context advertises a running state.
        const EP_STATE_HALTED: u8 = 2;
        const EP_STATE_STOPPED: u8 = 3;
        let idx = usize::from(endpoint_id.saturating_sub(1));
        if let Some(ctx) = slot.endpoint_contexts.get(idx) {
            let state = ctx.endpoint_state();
            if state == EP_STATE_HALTED || state == EP_STATE_STOPPED {
                return EndpointOutcome::idle();
            }
        }

        // Determine whether the guest has installed a Device Context pointer for this slot.
        //
        // Even if the pointer is malformed (misaligned), treat it as "present" so we do not fall
        // back to controller-local ring cursors (which could otherwise cause DMA based on stale
        // snapshot state).
        //
        // Also treat the slot as having a guest context if we've previously observed a non-zero
        // Device Context pointer for it. This prevents a guest (or malformed snapshot) from
        // clearing DCBAA[slot] back to 0 and re-enabling DMA via controller-local shadow ring
        // cursors.
        let guest_ctx_present = self
            .read_device_context_ptr_raw(mem, slot_id)
            .is_some_and(|raw| raw != 0)
            || slot.device_context_ptr != 0;

        let guest_endpoint_state = self.read_endpoint_state_from_context(mem, slot_id, endpoint_id);
        if let Some(state) = guest_endpoint_state {
            if !matches!(state, context::EndpointState::Running) {
                return EndpointOutcome::idle();
            }
        } else if guest_ctx_present {
            // If the guest has installed a Device Context pointer for this slot but the controller
            // cannot read the Endpoint Context (e.g. DCBAA entry is misaligned), do not fall back to
            // controller-local shadow state. This prevents DMA based on stale ring cursors when the
            // guest context becomes invalid while an endpoint is already scheduled.
            return EndpointOutcome::idle();
        }

        // Bulk/interrupt endpoints are delegated to `transfer::XhciTransferExecutor` and execute at
        // most one TD per controller tick.
        if endpoint_id != 1 {
            let Some(ep_addr) = Self::ep_addr_from_endpoint_id(endpoint_id) else {
                return EndpointOutcome::idle();
            };

            // Lazily create a transfer executor for the slot, pointing at the currently attached
            // device model.
            if self
                .transfer_executors
                .get(slot_idx)
                .and_then(|e| e.as_ref())
                .is_none()
            {
                let dev_ptr = match self.slot_device_mut(slot_id) {
                    Some(dev) => dev.model_mut() as *mut dyn UsbDeviceModel,
                    None => return EndpointOutcome::idle(),
                };
                self.transfer_executors[slot_idx] = Some(transfer::XhciTransferExecutor::new(
                    Box::new(UsbDeviceModelPtr::new(dev_ptr)),
                ));
            }

            let Some(mut exec) = self
                .transfer_executors
                .get_mut(slot_idx)
                .and_then(|e| e.take())
            else {
                return EndpointOutcome::idle();
            };

            let mapped = self.ensure_endpoint_mapped_from_context(
                mem,
                &mut exec,
                slot_id,
                endpoint_id,
                ep_addr,
                guest_ctx_present,
            );
            if !mapped {
                self.transfer_executors[slot_idx] = Some(exec);
                return EndpointOutcome::idle();
            }

            let before = exec
                .endpoint_state(ep_addr)
                .map(|st| (st.ring.dequeue_ptr, st.ring.cycle));

            let step_budget = *ring_poll_budget;
            let poll_work = exec.poll_endpoint_counted(mem, ep_addr, step_budget);
            *ring_poll_budget = ring_poll_budget.saturating_sub(poll_work.ring_poll_steps);
            work.ring_poll_steps = work
                .ring_poll_steps
                .saturating_add(poll_work.ring_poll_steps);

            let after = exec
                .endpoint_state(ep_addr)
                .map(|st| (st.ring.dequeue_ptr, st.ring.cycle));

            // If the dequeue pointer advanced, reflect it back into the Endpoint Context TR Dequeue
            // Pointer field so guests that inspect the Device Context observe progress.
            let trbs_consumed = poll_work.trbs_consumed;
            if let (Some((before_ptr, before_cycle)), Some((after_ptr, after_cycle))) =
                (before, after)
            {
                if before_ptr != after_ptr || before_cycle != after_cycle {
                    self.write_endpoint_dequeue_to_context(
                        mem,
                        slot_id,
                        endpoint_id,
                        after_ptr,
                        after_cycle,
                    );
                    // Keep controller-local ring cursor state in sync.
                    let endpoint_idx = usize::from(endpoint_id - 1);
                    self.slots[slot_idx].transfer_rings[endpoint_idx] =
                        Some(RingCursor::new(after_ptr, after_cycle));
                    self.slots[slot_idx].endpoint_contexts[endpoint_idx]
                        .set_tr_dequeue_pointer(after_ptr, after_cycle);
                }
            }

            // When a transfer results in an endpoint halt (STALL/TRB error), reflect that into the
            // guest Endpoint Context so software can observe the halted state and issue Reset
            // Endpoint. Also update the controller-local shadow context so snapshot/restore does
            // not lose the halted state (transfer executors are rebuilt on demand).
            if exec.endpoint_state(ep_addr).is_some_and(|st| st.halted) {
                self.write_endpoint_state_to_context(
                    mem,
                    slot_id,
                    endpoint_id,
                    context::EndpointState::Halted,
                );
            }
            // Drain and emit transfer events.
            let events = exec.take_events();
            for ev in events {
                let residual = ev.residual & 0x00ff_ffff;
                let status = residual | (u32::from(ev.completion_code.as_u8()) << 24);
                let mut trb = if let Some(event_data) = ev.event_data {
                    // xHCI spec: when an Event Data TRB terminates the TD, the Transfer Event TRB
                    // sets ED=1 and copies the Event Data TRB `parameter` value into the Transfer
                    // Event TRB parameter field (instead of a TRB pointer).
                    let mut trb = Trb::new(event_data, status, 0);
                    trb.control |= Trb::CONTROL_EVENT_DATA_BIT;
                    trb
                } else {
                    Trb::new(ev.trb_ptr & !0x0f, status, 0)
                };
                trb.set_trb_type(TrbType::TransferEvent);
                trb.set_slot_id(slot_id);
                trb.set_endpoint_id(Self::endpoint_id_from_ep_addr(ev.ep_addr));
                self.post_event(trb);
            }

            // Keep the endpoint active if the next TRB is ready (or we're waiting on an inflight
            // device completion).
            let keep_active = exec.endpoint_state(ep_addr).is_some_and(|st| {
                if st.halted {
                    return false;
                }
                if *ring_poll_budget == 0 {
                    // Global ring-walk budget exhausted; conservatively keep the endpoint active so
                    // we can retry on a future tick.
                    return true;
                }
                // Count the readiness probe as a ring-walk step.
                *ring_poll_budget = ring_poll_budget.saturating_sub(1);
                work.ring_poll_steps = work.ring_poll_steps.saturating_add(1);
                Trb::read_from(mem, st.ring.dequeue_ptr).cycle() == st.ring.cycle
            });

            self.transfer_executors[slot_idx] = Some(exec);

            return if keep_active {
                EndpointOutcome::keep(trbs_consumed)
            } else {
                EndpointOutcome::done(trbs_consumed)
            };
        }

        // If the guest has configured an Endpoint Context for EP0, validate its TR Dequeue Pointer
        // before polling. This ensures we don't DMA based on controller-local ring cursors when the
        // guest context becomes invalid (or encodes reserved TRDP bits).
        if guest_ctx_present
            && self
                .read_endpoint_dequeue_from_context(mem, slot_id, endpoint_id)
                .is_none()
        {
            return EndpointOutcome::idle();
        }

        // If an endpoint is Stopped/Halted, ignore doorbells and do not touch its transfer ring.
        // (This also prevents retrying a known-malformed ring forever.)
        let ep_state = slot.endpoint_contexts[0].endpoint_state();
        if ep_state == EP_STATE_HALTED || ep_state == EP_STATE_STOPPED {
            return EndpointOutcome::idle();
        }

        let Some(committed_ring) = slot.transfer_rings[0] else {
            return EndpointOutcome::idle();
        };

        let mut control_td = self
            .ep0_control_td
            .get(slot_idx)
            .copied()
            .unwrap_or_default();

        // Use the in-flight cursor when a control TD is partially completed (e.g. DATA/STATUS stage
        // is NAKed). Otherwise start from the committed dequeue pointer.
        let mut ring = control_td.td_cursor.unwrap_or(committed_ring);

        let mut events: Vec<Trb> = Vec::new();
        let mut trbs_consumed = 0usize;
        let mut keep_active = false;
        let mut halt_endpoint = false;

        {
            let Some(device) = self.slot_device_mut(slot_id) else {
                return EndpointOutcome::idle();
            };

            while trbs_consumed < trb_budget {
                if *ring_poll_budget == 0 {
                    // Global ring-walk budget exhausted; defer remaining work to a future tick.
                    keep_active = true;
                    break;
                }

                let step_budget = (*ring_poll_budget).min(RING_STEP_BUDGET);
                let (poll, steps_used) = ring.peek_counted(mem, step_budget);
                *ring_poll_budget = ring_poll_budget.saturating_sub(steps_used);
                work.ring_poll_steps = work.ring_poll_steps.saturating_add(steps_used);

                let item = match poll {
                    RingPoll::Ready(item) => item,
                    RingPoll::NotReady => {
                        // If a TD is already in progress, keep the endpoint active so we continue
                        // polling until the guest finishes writing the remaining stage TRBs.
                        keep_active = control_td.td_start.is_some();
                        break;
                    }
                    RingPoll::Err(RingError::StepBudgetExceeded) => {
                        // Either the ring is malformed (link loop) or we hit the remaining
                        // controller-wide ring-walk budget. In either case, keep the endpoint
                        // active and retry on a future tick.
                        keep_active = true;
                        break;
                    }
                    RingPoll::Err(_) => {
                        // Malformed ring (e.g. Link TRB loop). Halt the endpoint so we don't spend
                        // `RING_STEP_BUDGET` work on the same broken pointer forever.
                        halt_endpoint = true;
                        keep_active = false;
                        events.push(make_transfer_event_trb(
                            slot_id,
                            endpoint_id,
                            ring.dequeue_ptr(),
                            CompletionCode::TrbError,
                            0,
                        ));
                        control_td = ControlTdState::default();
                        break;
                    }
                };

                let trb = item.trb;
                let trb_paddr = item.paddr;

                match trb.trb_type() {
                    TrbType::SetupStage => {
                        control_td = ControlTdState::default();
                        // Pin the committed dequeue pointer at the start of the TD until the
                        // Status Stage completes.
                        control_td.td_start = Some(ring);

                        let setup_bytes = trb.parameter.to_le_bytes();
                        let setup = SetupPacket::from_bytes(setup_bytes);

                        let completion = match device.handle_setup(setup) {
                            UsbOutResult::Ack => CompletionCode::Success,
                            UsbOutResult::Nak => {
                                keep_active = true;
                                // Do not advance the transfer ring. Retry the setup stage later.
                                control_td.td_cursor = Some(ring);
                                break;
                            }
                            UsbOutResult::Stall => CompletionCode::StallError,
                            UsbOutResult::Timeout => CompletionCode::UsbTransactionError,
                        };

                        control_td.completion_code = completion;

                        if ring.consume().is_err() {
                            halt_endpoint = true;
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                CompletionCode::TrbError,
                                0,
                            ));
                            control_td = ControlTdState::default();
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;
                        control_td.td_cursor = Some(ring);

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
                                halt_endpoint = true;
                                events.push(make_transfer_event_trb(
                                    slot_id,
                                    endpoint_id,
                                    trb_paddr,
                                    CompletionCode::TrbError,
                                    0,
                                ));
                                control_td = ControlTdState::default();
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

                        let dir_in = trb.dir_in();
                        let idt = (trb.control & TRB_CTRL_IDT) != 0;
                        if idt && requested_len > 8 {
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

                        let (completion, transferred) = if dir_in {
                            match device.handle_in(0, requested_len) {
                                UsbInResult::Data(mut data) => {
                                    if data.len() > requested_len {
                                        data.truncate(requested_len);
                                    }
                                    let transferred = data.len();
                                    if idt {
                                        // Immediate data (IDT=1): write the response bytes into the
                                        // DataStage TRB parameter field in guest memory.
                                        let mut imm = [0u8; 8];
                                        imm[..transferred].copy_from_slice(&data);
                                        let mut updated = trb;
                                        updated.parameter = u64::from_le_bytes(imm);
                                        updated.write_to(mem, trb_paddr);
                                    } else {
                                        let buf_ptr = trb.parameter;
                                        mem.write_physical(buf_ptr, &data);
                                    }
                                    let completion = if transferred < requested_len {
                                        CompletionCode::ShortPacket
                                    } else {
                                        CompletionCode::Success
                                    };
                                    (completion, transferred)
                                }
                                UsbInResult::Nak => {
                                    keep_active = true;
                                    control_td.td_cursor = Some(ring);
                                    break;
                                }
                                UsbInResult::Stall => (CompletionCode::StallError, 0),
                                UsbInResult::Timeout => (CompletionCode::UsbTransactionError, 0),
                            }
                        } else if idt {
                            let imm = trb.parameter.to_le_bytes();
                            match device.handle_out(0, &imm[..requested_len]) {
                                UsbOutResult::Ack => (CompletionCode::Success, requested_len),
                                UsbOutResult::Nak => {
                                    keep_active = true;
                                    control_td.td_cursor = Some(ring);
                                    break;
                                }
                                UsbOutResult::Stall => (CompletionCode::StallError, 0),
                                UsbOutResult::Timeout => (CompletionCode::UsbTransactionError, 0),
                            }
                        } else {
                            let buf_ptr = trb.parameter;
                            let mut buf = vec![0u8; requested_len];
                            mem.read_physical(buf_ptr, &mut buf);
                            match device.handle_out(0, &buf) {
                                UsbOutResult::Ack => (CompletionCode::Success, requested_len),
                                UsbOutResult::Nak => {
                                    keep_active = true;
                                    control_td.td_cursor = Some(ring);
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
                            halt_endpoint = true;
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                CompletionCode::TrbError,
                                0,
                            ));
                            control_td = ControlTdState::default();
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;
                        control_td.td_cursor = Some(ring);

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
                        let dir_in = trb.dir_in();

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
                                    control_td.td_cursor = Some(ring);
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
                                    control_td.td_cursor = Some(ring);
                                    break;
                                }
                                UsbOutResult::Stall => CompletionCode::StallError,
                                UsbOutResult::Timeout => CompletionCode::UsbTransactionError,
                            }
                        };

                        let (completion, residue) = if status_completion == CompletionCode::Success
                        {
                            let residue = control_td
                                .data_expected
                                .saturating_sub(control_td.data_transferred);
                            (control_td.completion_code, residue)
                        } else {
                            (status_completion, 0)
                        };

                        if ring.consume().is_err() {
                            halt_endpoint = true;
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                CompletionCode::TrbError,
                                0,
                            ));
                            control_td = ControlTdState::default();
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
                            halt_endpoint = true;
                            events.push(make_transfer_event_trb(
                                slot_id,
                                endpoint_id,
                                trb_paddr,
                                CompletionCode::TrbError,
                                0,
                            ));
                            control_td = ControlTdState::default();
                            keep_active = false;
                            break;
                        }
                        trbs_consumed += 1;
                    }
                }
            }
        }

        // Persist the updated ring cursor + control TD bookkeeping.
        //
        // xHCI dequeue-pointer semantics:
        // - While a control TD is in-flight, the architectural TR Dequeue Pointer is pinned at the
        //   TD start (`td_start`).
        // - The internal cursor (`td_cursor`) advances as we process stage TRBs, and is used for
        //   retries after NAK without duplicating work.
        // - Once the Status Stage completes, `control_td` is reset to its default (no TD in
        //   progress) and we commit the current cursor (`ring`) as the new dequeue pointer.
        let new_committed_ring = control_td.td_start.unwrap_or(ring);
        let committed_changed = new_committed_ring != committed_ring;
        if let Some(slot) = self.slots.get_mut(slot_idx) {
            slot.transfer_rings[0] = Some(new_committed_ring);
            if committed_changed {
                slot.endpoint_contexts[0].set_tr_dequeue_pointer(
                    new_committed_ring.dequeue_ptr(),
                    new_committed_ring.cycle_state(),
                );
            }
        }
        if committed_changed {
            self.write_endpoint_dequeue_to_context(
                mem,
                slot_id,
                endpoint_id,
                new_committed_ring.dequeue_ptr(),
                new_committed_ring.cycle_state(),
            );
        }
        if halt_endpoint {
            // Keep the guest endpoint context and controller-local shadow state in sync. Use the
            // shared helper so we don't overwrite unrelated fields (e.g. TR Dequeue Pointer) when
            // snapshot/restore or test harnesses mutate the guest context directly.
            self.write_endpoint_state_to_context(
                mem,
                slot_id,
                endpoint_id,
                context::EndpointState::Halted,
            );
        }
        if let Some(state) = self.ep0_control_td.get_mut(slot_idx) {
            if control_td.td_start.is_some() {
                control_td.td_cursor = Some(ring);
            } else {
                control_td.td_cursor = None;
            }
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

    fn ep_addr_from_endpoint_id(endpoint_id: u8) -> Option<u8> {
        // Endpoint ID 0 is reserved, endpoint ID 1 is EP0 (not supported by the transfer executor).
        if !(2..=31).contains(&endpoint_id) {
            return None;
        }
        let ep_num = endpoint_id / 2;
        let is_in = (endpoint_id & 1) != 0;
        Some(ep_num | if is_in { 0x80 } else { 0x00 })
    }

    fn endpoint_id_from_ep_addr(ep_addr: u8) -> u8 {
        let ep_num = ep_addr & 0x0f;
        if ep_num == 0 {
            return 1;
        }
        let is_in = (ep_addr & 0x80) != 0;
        ep_num
            .saturating_mul(2)
            .saturating_add(if is_in { 1 } else { 0 })
    }

    fn ensure_endpoint_mapped_from_context(
        &mut self,
        mem: &mut dyn MemoryBus,
        exec: &mut transfer::XhciTransferExecutor,
        slot_id: u8,
        endpoint_id: u8,
        ep_addr: u8,
        guest_ctx_present: bool,
    ) -> bool {
        let (dequeue_ptr, cycle) =
            match self.read_endpoint_dequeue_from_context(mem, slot_id, endpoint_id) {
                Some(v) => v,
                None => {
                    // If the guest has configured a valid Device Context for this slot, do not fall
                    // back to controller-local ring cursors. `read_endpoint_dequeue_from_context`
                    // returning `None` in this case implies the guest Endpoint Context is either
                    // malformed (missing TRDP) or describes an unsupported endpoint type.
                    if guest_ctx_present {
                        return false;
                    }
                    // Test/harness helpers like `set_endpoint_ring()` configure controller-local ring
                    // cursors without populating full Endpoint Context state in guest memory. Fall back
                    // to those cursors so deterministic `tick_1ms` polling can still make progress.
                    let slot_idx = usize::from(slot_id);
                    let ring_idx = usize::from(endpoint_id.saturating_sub(1));
                    let Some(ring) = self
                        .slots
                        .get(slot_idx)
                        .and_then(|slot| slot.transfer_rings.get(ring_idx))
                        .copied()
                        .flatten()
                    else {
                        return false;
                    };
                    (ring.dequeue_ptr(), ring.cycle_state())
                }
            };

        if let Some(st) = exec.endpoint_state_mut(ep_addr) {
            st.ring.dequeue_ptr = dequeue_ptr;
            st.ring.cycle = cycle;
        } else {
            exec.add_endpoint_with_cycle(ep_addr, dequeue_ptr, cycle);
        }
        true
    }

    fn read_device_context_ptr_raw(&self, mem: &mut dyn MemoryBus, slot_id: u8) -> Option<u64> {
        if slot_id == 0 {
            return None;
        }
        if self.dcbaap == 0 {
            return None;
        }
        let dcbaa = context::Dcbaa::new(self.dcbaap);
        dcbaa.read_device_context_ptr(mem, slot_id).ok()
    }

    fn read_endpoint_dequeue_from_context(
        &self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
    ) -> Option<(u64, bool)> {
        if slot_id == 0 {
            return None;
        }
        if endpoint_id == 0 || endpoint_id > 31 {
            return None;
        }
        if self.dcbaap == 0 {
            return None;
        }
        let dev_ctx_raw = self.read_device_context_ptr_raw(mem, slot_id)?;
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            return None;
        }
        let dev_ctx = context::DeviceContext32::new(dev_ctx_ptr);
        let ep_ctx = dev_ctx.endpoint_context(mem, endpoint_id).ok()?;
        match ep_ctx.endpoint_type() {
            context::EndpointType::BulkIn
            | context::EndpointType::BulkOut
            | context::EndpointType::InterruptIn
            | context::EndpointType::InterruptOut => {}
            context::EndpointType::Control | context::EndpointType::Invalid if endpoint_id == 1 => {
            }
            _ => return None,
        }
        // TR Dequeue Pointer is 16-byte aligned with DCS in bit0. Bits 1..=3 are reserved; treat
        // them as invalid rather than masking them away so a malformed guest context cannot alias a
        // different aligned pointer.
        let raw = ep_ctx.tr_dequeue_pointer_raw();
        if (raw & 0x0e) != 0 {
            return None;
        }
        let dequeue_ptr = raw & !0x0f;
        if dequeue_ptr == 0 {
            return None;
        }
        Some((dequeue_ptr, (raw & 0x01) != 0))
    }

    fn read_endpoint_state_from_context(
        &self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
    ) -> Option<context::EndpointState> {
        if slot_id == 0 {
            return None;
        }
        if endpoint_id == 0 || endpoint_id > 31 {
            return None;
        }
        if self.dcbaap == 0 {
            return None;
        }
        let dev_ctx_raw = self.read_device_context_ptr_raw(mem, slot_id)?;
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            return None;
        }
        let dev_ctx = context::DeviceContext32::new(dev_ctx_ptr);
        let ep_ctx = dev_ctx.endpoint_context(mem, endpoint_id).ok()?;
        Some(ep_ctx.endpoint_state_enum())
    }

    fn write_endpoint_dequeue_to_context(
        &self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
        dequeue_ptr: u64,
        cycle: bool,
    ) {
        if slot_id == 0 {
            return;
        }
        if endpoint_id == 0 || endpoint_id > 31 {
            return;
        }
        if self.dcbaap == 0 {
            return;
        }
        let Some(dev_ctx_raw) = self.read_device_context_ptr_raw(mem, slot_id) else {
            return;
        };
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            return;
        }

        let ctx_base = match dev_ctx_ptr
            .checked_add(u64::from(endpoint_id).saturating_mul(context::CONTEXT_SIZE as u64))
        {
            Some(v) => v,
            None => return,
        };

        let tr_dequeue_raw = (dequeue_ptr & !0x0f) | u64::from(cycle as u8);
        let lo_addr = match ctx_base.checked_add(8) {
            Some(v) => v,
            None => return,
        };
        let hi_addr = match ctx_base.checked_add(12) {
            Some(v) => v,
            None => return,
        };
        mem.write_u32(lo_addr, tr_dequeue_raw as u32);
        mem.write_u32(hi_addr, (tr_dequeue_raw >> 32) as u32);
    }

    fn write_endpoint_state_to_context(
        &mut self,
        mem: &mut dyn MemoryBus,
        slot_id: u8,
        endpoint_id: u8,
        state: context::EndpointState,
    ) -> bool {
        if slot_id == 0 {
            return false;
        }
        if endpoint_id == 0 || endpoint_id > 31 {
            return false;
        }

        let dev_ctx_raw = if self.dcbaap == 0 {
            None
        } else {
            self.read_device_context_ptr_raw(mem, slot_id)
        };

        // Always update the controller-local shadow Endpoint Context so doorbell gating and
        // snapshot/restore preserve the halted state even if the guest Device Context is absent.
        let slot_idx = usize::from(slot_id);
        let Some(slot) = self.slots.get_mut(slot_idx) else {
            return false;
        };
        slot.endpoint_contexts[usize::from(endpoint_id - 1)].set_endpoint_state_enum(state);

        let Some(dev_ctx_raw) = dev_ctx_raw else {
            return false;
        };
        let dev_ctx_ptr = dev_ctx_raw & !0x3f;
        if dev_ctx_ptr == 0 || (dev_ctx_raw & 0x3f) != 0 {
            return false;
        }

        let ctx_base = match dev_ctx_ptr
            .checked_add(u64::from(endpoint_id).saturating_mul(context::CONTEXT_SIZE as u64))
        {
            Some(v) => v,
            None => return false,
        };

        // Endpoint state bits live in DW0 bits 2:0. Update just that dword in guest memory so we do
        // not clobber TR Dequeue Pointer writes performed by the transfer engine.
        let dw0 = mem.read_u32(ctx_base);
        let new_dw0 = (dw0 & !0x7) | (u32::from(state.raw()) & 0x7);
        mem.write_u32(ctx_base, new_dw0);

        // Record the Device Context pointer for snapshot/restore.
        slot.device_context_ptr = dev_ctx_ptr;
        true
    }
    fn dma_on_run(&mut self, mem: &mut dyn MemoryBus) {
        if !self.pending_dma_on_run {
            return;
        }
        if self.host_controller_error {
            // Once HCE is latched, the guest must reset the controller. Avoid any further DMA,
            // including the DMA-on-RUN probe used by wrapper tests.
            self.pending_dma_on_run = false;
            return;
        }
        if !mem.dma_enabled() {
            return;
        }

        // Read a dword from CRCR and surface an interrupt. The data itself is ignored; the goal is
        // to touch the memory bus when bus mastering is enabled so wrappers can gate the access.
        let paddr = self.crcr & !0x3f;
        let mut buf = [0u8; 4];
        mem.read_bytes(paddr, &mut buf);
        self.interrupter0.set_interrupt_pending(true);
        self.pending_dma_on_run = false;
    }

    /// Traverse the attached USB topology and reset any host-side asynchronous state that cannot
    /// survive snapshot/restore (e.g. WebUSB JS Promise bookkeeping).
    ///
    /// This is intended to be called by host integrations after restoring a VM snapshot. It does
    /// not modify guest-visible USB state.
    pub fn reset_host_state_for_restore(&mut self) {
        for port in &mut self.ports {
            if let Some(dev) = port.device_mut() {
                dev.reset_host_state_for_restore();
            }
        }
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
fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}

#[cfg(test)]
mod tests;
