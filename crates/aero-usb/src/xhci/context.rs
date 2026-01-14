//! xHCI context structures and parsing helpers (32-byte contexts).
//!
//! xHCI uses in-memory "contexts" to represent device/endpoint state. This module provides raw
//! context wrappers, common field helpers, and safe guest-memory read helpers for the context
//! structures consumed by commands like **Address Device** and **Configure Endpoint**.
//!
//! MVP assumption: `HCCPARAMS1.CSZ = 0`, i.e. **32-byte contexts**.
//! 64-byte contexts are not supported yet.

use alloc::vec::Vec;
use core::fmt;

use crate::MemoryBus;

/// Size of each xHCI context structure in bytes when `HCCPARAMS1.CSZ = 0`.
pub const CONTEXT_SIZE: usize = 32;
/// Number of 32-bit dwords in a 32-byte context.
pub const CONTEXT_DWORDS: usize = 8;

/// Maximum number of contexts in a Device Context (Slot + 31 Endpoints).
pub const DEVICE_CONTEXT_ENTRY_COUNT: usize = 32;
/// Maximum number of contexts in an Input Context (Input Control + Device Context).
pub const INPUT_CONTEXT_ENTRY_COUNT: usize = 33;

/// Upper bound for iterating context indices in flags fields.
///
/// This is fixed by the spec (32 bits) and does **not** depend on guest values.
const MAX_FLAG_BITS: u8 = 32;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContextError {
    InvalidDeviceContextIndex(u8),
    InvalidSlotId(u8),
    AddressOverflow,
}

impl fmt::Display for ContextError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ContextError::InvalidDeviceContextIndex(idx) => {
                write!(f, "invalid device context index {idx}")
            }
            ContextError::InvalidSlotId(id) => write!(f, "invalid xHCI slot ID {id}"),
            ContextError::AddressOverflow => write!(f, "guest physical address overflow"),
        }
    }
}

impl core::error::Error for ContextError {}

fn read_context32_dwords(
    mem: &mut (impl MemoryBus + ?Sized),
    paddr: u64,
) -> [u32; CONTEXT_DWORDS] {
    let mut raw = [0u8; CONTEXT_DWORDS * 4];
    mem.read_bytes(paddr, &mut raw);
    let mut out = [0u32; CONTEXT_DWORDS];
    for (i, dword) in out.iter_mut().enumerate() {
        let off = i * 4;
        *dword = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
    }
    out
}

fn write_context32_dwords(
    mem: &mut (impl MemoryBus + ?Sized),
    paddr: u64,
    dwords: &[u32; CONTEXT_DWORDS],
) {
    let mut raw = [0u8; CONTEXT_DWORDS * 4];
    for (i, dword) in dwords.iter().enumerate() {
        let off = i * 4;
        raw[off..off + 4].copy_from_slice(&dword.to_le_bytes());
    }
    mem.write_physical(paddr, &raw);
}
fn read_u64_le(mem: &mut (impl MemoryBus + ?Sized), paddr: u64) -> u64 {
    mem.read_u64(paddr)
}

fn write_u64_le(mem: &mut (impl MemoryBus + ?Sized), paddr: u64, value: u64) {
    mem.write_physical(paddr, &value.to_le_bytes());
}

/// Iterator over set bits in a 32-bit context flags field.
///
/// This iterator is bounded to 32 iterations (spec-defined), regardless of guest input.
#[derive(Clone, Copy, Debug)]
pub struct ContextFlagIter {
    bits: u32,
    next: u8,
}

impl Iterator for ContextFlagIter {
    type Item = u8;

    fn next(&mut self) -> Option<Self::Item> {
        while self.next < MAX_FLAG_BITS {
            let idx = self.next;
            self.next += 1;
            if (self.bits >> idx) & 1 != 0 {
                return Some(idx);
            }
        }
        None
    }
}

/// Input Control Context (ICC) (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputControlContext {
    dwords: [u32; CONTEXT_DWORDS],
}

impl InputControlContext {
    pub fn read_from(mem: &mut (impl MemoryBus + ?Sized), paddr: u64) -> Self {
        Self {
            dwords: read_context32_dwords(mem, paddr),
        }
    }

    pub fn write_to(&self, mem: &mut (impl MemoryBus + ?Sized), paddr: u64) {
        write_context32_dwords(mem, paddr, &self.dwords);
    }

    pub fn dword(&self, index: usize) -> u32 {
        self.dwords.get(index).copied().unwrap_or(0)
    }

    pub fn set_dword(&mut self, index: usize, value: u32) {
        if let Some(dw) = self.dwords.get_mut(index) {
            *dw = value;
        }
    }

    /// Drop Context Flags field (DW0).
    pub fn drop_flags(&self) -> u32 {
        self.dwords[0]
    }

    pub fn set_drop_flags(&mut self, value: u32) {
        self.dwords[0] = value;
    }

    /// Add Context Flags field (DW1).
    pub fn add_flags(&self) -> u32 {
        self.dwords[1]
    }

    pub fn set_add_flags(&mut self, value: u32) {
        self.dwords[1] = value;
    }

    /// Returns `true` if the bit for the given Device Context index is set in Drop Context Flags.
    ///
    /// * `device_context_index = 0` is Slot Context.
    /// * `device_context_index = 1..=31` are Endpoint Contexts.
    pub fn drop_context(&self, device_context_index: u8) -> bool {
        if device_context_index >= MAX_FLAG_BITS {
            return false;
        }
        (self.drop_flags() >> device_context_index) & 1 != 0
    }

    /// Returns `true` if the bit for the given Device Context index is set in Add Context Flags.
    ///
    /// * `device_context_index = 0` is Slot Context.
    /// * `device_context_index = 1..=31` are Endpoint Contexts.
    pub fn add_context(&self, device_context_index: u8) -> bool {
        if device_context_index >= MAX_FLAG_BITS {
            return false;
        }
        (self.add_flags() >> device_context_index) & 1 != 0
    }

    pub fn added_indices(&self) -> ContextFlagIter {
        ContextFlagIter {
            bits: self.add_flags(),
            next: 0,
        }
    }

    pub fn dropped_indices(&self) -> ContextFlagIter {
        ContextFlagIter {
            bits: self.drop_flags(),
            next: 0,
        }
    }
}

/// Slot Context (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotContext {
    dwords: [u32; CONTEXT_DWORDS],
}

impl SlotContext {
    pub fn read_from(mem: &mut (impl MemoryBus + ?Sized), paddr: u64) -> Self {
        Self {
            dwords: read_context32_dwords(mem, paddr),
        }
    }

    pub fn write_to(&self, mem: &mut (impl MemoryBus + ?Sized), paddr: u64) {
        write_context32_dwords(mem, paddr, &self.dwords);
    }

    pub fn dword(&self, index: usize) -> u32 {
        self.dwords.get(index).copied().unwrap_or(0)
    }

    pub fn set_dword(&mut self, index: usize, value: u32) {
        if let Some(dw) = self.dwords.get_mut(index) {
            *dw = value;
        }
    }

    /// Route String field (DW0 bits 0..=19).
    pub fn route_string(&self) -> u32 {
        self.dwords[0] & 0x000f_ffff
    }

    pub fn set_route_string(&mut self, route: u32) {
        self.dwords[0] = (self.dwords[0] & !0x000f_ffff) | (route & 0x000f_ffff);
    }

    /// Speed field (DW0 bits 20..=23).
    pub fn speed(&self) -> u8 {
        ((self.dwords[0] >> 20) & 0x0f) as u8
    }

    pub fn set_speed(&mut self, speed: u8) {
        let speed = (speed as u32) & 0x0f;
        self.dwords[0] = (self.dwords[0] & !(0x0f << 20)) | (speed << 20);
    }

    /// Context Entries field (DW0 bits 27..=31).
    pub fn context_entries(&self) -> u8 {
        ((self.dwords[0] >> 27) & 0x1f) as u8
    }

    pub fn set_context_entries(&mut self, entries: u8) {
        let entries = (entries as u32) & 0x1f;
        self.dwords[0] = (self.dwords[0] & !(0x1f << 27)) | (entries << 27);
    }

    /// Parses the Route String field into a validated [`XhciRouteString`].
    pub fn parsed_route_string(&self) -> Result<XhciRouteString, XhciRouteStringError> {
        XhciRouteString::from_raw(self.route_string())
    }

    /// Sets the Route String field from downstream hub ports ordered from root-to-device.
    pub fn set_route_string_from_root_ports(
        &mut self,
        ports: &[u8],
    ) -> Result<(), XhciRouteStringError> {
        let route = XhciRouteString::encode_from_root(ports)?;
        self.set_route_string(route.raw());
        Ok(())
    }

    /// Root Hub Port Number field (DW1 bits 16..=23).
    pub fn root_hub_port_number(&self) -> u8 {
        ((self.dwords[1] >> 16) & 0xff) as u8
    }

    pub fn set_root_hub_port_number(&mut self, port: u8) {
        self.dwords[1] = (self.dwords[1] & !(0xff << 16)) | ((port as u32) << 16);
    }
}

/// Endpoint type (xHCI Endpoint Context EP Type field).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointType {
    Invalid,
    IsochOut,
    BulkOut,
    InterruptOut,
    Control,
    IsochIn,
    BulkIn,
    InterruptIn,
}

impl EndpointType {
    pub const fn from_raw(raw: u8) -> Self {
        match raw & 0x07 {
            0 => EndpointType::Invalid,
            1 => EndpointType::IsochOut,
            2 => EndpointType::BulkOut,
            3 => EndpointType::InterruptOut,
            4 => EndpointType::Control,
            5 => EndpointType::IsochIn,
            6 => EndpointType::BulkIn,
            7 => EndpointType::InterruptIn,
            _ => EndpointType::Invalid,
        }
    }
}

/// Endpoint state (xHCI Endpoint Context Endpoint State field, bits 2:0 of DWORD0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointState {
    Disabled,
    Running,
    Halted,
    Stopped,
    Error,
    /// Any reserved/unknown state value (3-bit field).
    Reserved(u8),
}

impl EndpointState {
    pub const fn from_raw(raw: u8) -> Self {
        match raw & 0x07 {
            0 => EndpointState::Disabled,
            1 => EndpointState::Running,
            2 => EndpointState::Halted,
            3 => EndpointState::Stopped,
            4 => EndpointState::Error,
            other => EndpointState::Reserved(other),
        }
    }

    pub const fn raw(self) -> u8 {
        match self {
            EndpointState::Disabled => 0,
            EndpointState::Running => 1,
            EndpointState::Halted => 2,
            EndpointState::Stopped => 3,
            EndpointState::Error => 4,
            EndpointState::Reserved(v) => v & 0x07,
        }
    }
}

/// Endpoint Context (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointContext {
    dwords: [u32; CONTEXT_DWORDS],
}

impl EndpointContext {
    pub fn read_from(mem: &mut (impl MemoryBus + ?Sized), paddr: u64) -> Self {
        Self {
            dwords: read_context32_dwords(mem, paddr),
        }
    }

    pub fn write_to(&self, mem: &mut (impl MemoryBus + ?Sized), paddr: u64) {
        write_context32_dwords(mem, paddr, &self.dwords);
    }

    pub fn dword(&self, index: usize) -> u32 {
        self.dwords.get(index).copied().unwrap_or(0)
    }

    pub fn set_dword(&mut self, index: usize, value: u32) {
        if let Some(dw) = self.dwords.get_mut(index) {
            *dw = value;
        }
    }

    /// Endpoint State field (DW0 bits 0..=2).
    pub fn endpoint_state(&self) -> u8 {
        (self.dwords[0] & 0x7) as u8
    }

    pub fn endpoint_state_enum(&self) -> EndpointState {
        EndpointState::from_raw(self.endpoint_state())
    }

    pub fn set_endpoint_state(&mut self, state: u8) {
        let state = (state as u32) & 0x7;
        self.dwords[0] = (self.dwords[0] & !0x7) | state;
    }

    pub fn set_endpoint_state_enum(&mut self, state: EndpointState) {
        self.set_endpoint_state(state.raw());
    }

    /// Interval field (DW0 bits 16..=23).
    pub fn interval(&self) -> u8 {
        ((self.dwords[0] >> 16) & 0xff) as u8
    }

    /// Sets the Interval field (DW0 bits 16..=23).
    pub fn set_interval(&mut self, interval: u8) {
        self.dwords[0] = (self.dwords[0] & !(0xff << 16)) | ((interval as u32) << 16);
    }

    /// Endpoint Type field (DW1 bits 3..=5).
    pub fn endpoint_type_raw(&self) -> u8 {
        ((self.dwords[1] >> 3) & 0x07) as u8
    }

    pub fn endpoint_type(&self) -> EndpointType {
        EndpointType::from_raw(self.endpoint_type_raw())
    }

    /// Max Packet Size field (DW1 bits 16..=31).
    pub fn max_packet_size(&self) -> u16 {
        ((self.dwords[1] >> 16) & 0xffff) as u16
    }

    /// Sets the Max Packet Size field (DW1 bits 16..=31).
    pub fn set_max_packet_size(&mut self, max_packet_size: u16) {
        self.dwords[1] =
            (self.dwords[1] & !(0xffff << 16)) | ((max_packet_size as u32) << 16);
    }

    /// TR Dequeue Pointer field (DW2-DW3).
    pub fn tr_dequeue_pointer_raw(&self) -> u64 {
        (self.dwords[3] as u64) << 32 | (self.dwords[2] as u64)
    }

    /// Transfer Ring Dequeue Pointer, masked to 16-byte alignment.
    pub fn tr_dequeue_pointer(&self) -> u64 {
        self.tr_dequeue_pointer_raw() & !0x0f
    }

    /// Dequeue Cycle State (DCS) bit.
    pub fn dcs(&self) -> bool {
        (self.tr_dequeue_pointer_raw() & 0x01) != 0
    }

    /// Set the Transfer Ring Dequeue Pointer + DCS bit.
    pub fn set_tr_dequeue_pointer(&mut self, ptr: u64, dcs: bool) {
        let ptr = ptr & !0x0f;
        let raw = ptr | (dcs as u64);
        self.dwords[2] = raw as u32;
        self.dwords[3] = (raw >> 32) as u32;
    }

    /// Sets the raw TR Dequeue Pointer field (DW2-DW3) verbatim.
    ///
    /// This preserves the DCS bit as well as any reserved low bits that may have been set by the
    /// guest. Prefer [`Self::set_tr_dequeue_pointer`] when constructing a new context.
    pub fn set_tr_dequeue_pointer_raw(&mut self, raw: u64) {
        self.dwords[2] = raw as u32;
        self.dwords[3] = (raw >> 32) as u32;
    }

    /// Lower 32 bits of the raw TR Dequeue Pointer field (DW2).
    pub fn tr_dequeue_ptr_lo(&self) -> u32 {
        self.dwords[2]
    }

    pub fn set_tr_dequeue_ptr_lo(&mut self, value: u32) {
        self.dwords[2] = value;
    }

    pub fn tr_dequeue_ptr_hi(&self) -> u32 {
        self.dwords[3]
    }

    pub fn set_tr_dequeue_ptr_hi(&mut self, value: u32) {
        self.dwords[3] = value;
    }
}

/// Wrapper for an xHCI Input Context in guest memory (32-byte contexts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InputContext32 {
    base: u64,
}

impl InputContext32 {
    pub const fn new(base: u64) -> Self {
        Self { base }
    }

    pub const fn base(&self) -> u64 {
        self.base
    }

    pub fn input_control(&self, mem: &mut (impl MemoryBus + ?Sized)) -> InputControlContext {
        InputControlContext::read_from(mem, self.base)
    }

    pub fn write_input_control(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        ctx: &InputControlContext,
    ) -> Result<(), ContextError> {
        ctx.write_to(mem, self.base);
        Ok(())
    }

    /// Reads the Slot Context (Device Context index 0).
    pub fn slot_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
    ) -> Result<SlotContext, ContextError> {
        let addr = self
            .base
            .checked_add(CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        Ok(SlotContext::read_from(mem, addr))
    }

    /// Writes the Slot Context (Device Context index 0).
    pub fn write_slot_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        ctx: &SlotContext,
    ) -> Result<(), ContextError> {
        let addr = self
            .base
            .checked_add(CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        ctx.write_to(mem, addr);
        Ok(())
    }

    /// Reads an Endpoint Context by Device Context index (`1..=31`).
    ///
    /// The index matches the bit position in the Input Control Context flags:
    ///
    /// * `1` = Endpoint 0
    /// * `2` = Endpoint 1 OUT
    /// * `3` = Endpoint 1 IN
    /// * ...
    pub fn endpoint_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        device_context_index: u8,
    ) -> Result<EndpointContext, ContextError> {
        if !(1..=31).contains(&device_context_index) {
            return Err(ContextError::InvalidDeviceContextIndex(
                device_context_index,
            ));
        }

        // Input Context layout: [Input Control][Slot][EP0][EP1 OUT][EP1 IN]...
        // => add one context to map from Device Context index to Input Context index.
        let input_context_index = device_context_index as u64 + 1;
        let addr = self
            .base
            .checked_add(input_context_index * CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        Ok(EndpointContext::read_from(mem, addr))
    }

    /// Writes an Endpoint Context by Device Context index (`1..=31`).
    pub fn write_endpoint_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        device_context_index: u8,
        ctx: &EndpointContext,
    ) -> Result<(), ContextError> {
        if !(1..=31).contains(&device_context_index) {
            return Err(ContextError::InvalidDeviceContextIndex(device_context_index));
        }

        // Input Context layout: [Input Control][Slot][EP0][EP1 OUT][EP1 IN]...
        // => add one context to map from Device Context index to Input Context index.
        let input_context_index = device_context_index as u64 + 1;
        let addr = self
            .base
            .checked_add(input_context_index * CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        ctx.write_to(mem, addr);
        Ok(())
    }
}

/// Wrapper for an xHCI Device Context in guest memory (32-byte contexts).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeviceContext32 {
    base: u64,
}

impl DeviceContext32 {
    pub const fn new(base: u64) -> Self {
        Self { base }
    }

    pub const fn base(&self) -> u64 {
        self.base
    }

    pub fn slot_context(&self, mem: &mut (impl MemoryBus + ?Sized)) -> SlotContext {
        SlotContext::read_from(mem, self.base)
    }

    pub fn write_slot_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        ctx: &SlotContext,
    ) -> Result<(), ContextError> {
        ctx.write_to(mem, self.base);
        Ok(())
    }

    pub fn endpoint_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        device_context_index: u8,
    ) -> Result<EndpointContext, ContextError> {
        if !(1..=31).contains(&device_context_index) {
            return Err(ContextError::InvalidDeviceContextIndex(
                device_context_index,
            ));
        }

        let addr = self
            .base
            .checked_add(device_context_index as u64 * CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        Ok(EndpointContext::read_from(mem, addr))
    }

    pub fn write_endpoint_context(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        device_context_index: u8,
        ctx: &EndpointContext,
    ) -> Result<(), ContextError> {
        if !(1..=31).contains(&device_context_index) {
            return Err(ContextError::InvalidDeviceContextIndex(device_context_index));
        }

        let addr = self
            .base
            .checked_add(device_context_index as u64 * CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        ctx.write_to(mem, addr);
        Ok(())
    }
}

/// xHCI Device Context Base Address Array (DCBAA).
///
/// The DCBAA is an array of 64-bit pointers indexed by Slot ID.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dcbaa {
    base: u64,
}

impl Dcbaa {
    /// Number of 64-bit entries in the DCBAA, per xHCI spec.
    pub const ENTRY_COUNT: usize = 256;

    pub const fn new(base: u64) -> Self {
        Self { base }
    }

    pub const fn base(&self) -> u64 {
        self.base
    }

    fn entry_addr(&self, slot_id: u8) -> Result<u64, ContextError> {
        if slot_id == 0 {
            return Err(ContextError::InvalidSlotId(slot_id));
        }
        // slot_id is u8 => max 255 => offset <= 2040.
        self.base
            .checked_add((slot_id as u64) * 8)
            .ok_or(ContextError::AddressOverflow)
    }

    pub fn read_device_context_ptr(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        slot_id: u8,
    ) -> Result<u64, ContextError> {
        let addr = self.entry_addr(slot_id)?;
        Ok(read_u64_le(mem, addr))
    }

    pub fn write_device_context_ptr(
        &self,
        mem: &mut (impl MemoryBus + ?Sized),
        slot_id: u8,
        device_context_ptr: u64,
    ) -> Result<(), ContextError> {
        let addr = self.entry_addr(slot_id)?;
        // Device Context pointers are 64-byte aligned; mask low bits away.
        write_u64_le(mem, addr, device_context_ptr & !0x3f);
        Ok(())
    }
}

/// Maximum depth encoded by the Slot Context Route String field.
///
/// xHCI encodes up to 5 tiers of hub routing (20 bits, 5 nibbles).
pub const XHCI_ROUTE_STRING_MAX_DEPTH: usize = 5;

/// Maximum downstream port number encodable in a Route String nibble.
///
/// Per xHCI spec (Slot Context, Route String), each tier is encoded as a 4-bit port number where
/// `0` means "no more tiers" and valid downstream ports are `1..=15`.
///
/// Reference: xHCI 1.2 §6.2.2 "Slot Context" (Route String field).
pub const XHCI_ROUTE_STRING_MAX_PORT: u8 = 15;

/// Errors returned when decoding or encoding a Slot Context Route String.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum XhciRouteStringError {
    #[error("route string uses bits outside the 20-bit field: 0x{0:x}")]
    OutOfRange(u32),

    #[error("route string has a gap (encountered terminator then later non-zero nibble): 0x{0:x}")]
    NonZeroAfterTerminator(u32),

    #[error("route string depth exceeds {max} tiers (ports={depth})")]
    TooDeep { depth: usize, max: usize },

    #[error("invalid downstream port number in route string: {port} (valid range is 1..={max})")]
    InvalidPort { port: u8, max: u8 },
}

/// A validated xHCI Slot Context Route String.
///
/// The Route String is a 20-bit value made up of up to 5 4-bit nibbles. Each nibble encodes a
/// downstream hub port number (`1..=15`). A `0` nibble terminates the string.
///
/// Note on nibble ordering: the least significant nibble (bits 3:0) is the port number closest to
/// the device. Each successive nibble moves one hub hop closer to the root port.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XhciRouteString(u32);

impl XhciRouteString {
    /// Builds a validated route string from the raw 20-bit field value.
    pub fn from_raw(raw: u32) -> Result<Self, XhciRouteStringError> {
        if raw & !0x000f_ffff != 0 {
            return Err(XhciRouteStringError::OutOfRange(raw));
        }

        // Enforce the invariant that once a 0 nibble is encountered, all higher nibbles must be 0.
        let mut seen_terminator = false;
        for i in 0..XHCI_ROUTE_STRING_MAX_DEPTH {
            let nibble = ((raw >> (4 * i)) & 0x0f) as u8;
            if nibble == 0 {
                seen_terminator = true;
                continue;
            }
            if seen_terminator {
                return Err(XhciRouteStringError::NonZeroAfterTerminator(raw));
            }
            // nibble is nonzero, range is implicitly 1..=15 due to 4-bit field, but keep the check
            // explicit so callers that pass in already-shifted values get a useful error.
            if nibble > XHCI_ROUTE_STRING_MAX_PORT {
                return Err(XhciRouteStringError::InvalidPort {
                    port: nibble,
                    max: XHCI_ROUTE_STRING_MAX_PORT,
                });
            }
        }

        Ok(Self(raw))
    }

    /// Returns the underlying raw 20-bit value.
    pub fn raw(self) -> u32 {
        self.0
    }

    /// Returns the sequence of downstream hub ports starting at the hub directly attached to the
    /// root port.
    pub fn ports_from_root(self) -> Vec<u8> {
        // Nibble 0 is closest-to-device. Collect then reverse to root->device order.
        let mut ports = self.ports_to_root();
        ports.reverse();
        ports
    }

    /// Returns the sequence of downstream hub ports starting at the hub directly attached to the
    /// device (i.e. closest-to-device first).
    pub fn ports_to_root(self) -> Vec<u8> {
        let mut ports = Vec::new();
        for i in 0..XHCI_ROUTE_STRING_MAX_DEPTH {
            let nibble = ((self.0 >> (4 * i)) & 0x0f) as u8;
            if nibble == 0 {
                break;
            }
            ports.push(nibble);
        }
        ports
    }

    /// Encodes a Route String from a list of port numbers ordered from root-to-device.
    pub fn encode_from_root(ports: &[u8]) -> Result<Self, XhciRouteStringError> {
        if ports.len() > XHCI_ROUTE_STRING_MAX_DEPTH {
            return Err(XhciRouteStringError::TooDeep {
                depth: ports.len(),
                max: XHCI_ROUTE_STRING_MAX_DEPTH,
            });
        }

        // Build the Route String by appending port nibbles as we walk from root to device.
        // The last hop ends up in the least significant nibble.
        let mut raw: u32 = 0;
        for &port in ports.iter() {
            if port == 0 || port > XHCI_ROUTE_STRING_MAX_PORT {
                return Err(XhciRouteStringError::InvalidPort {
                    port,
                    max: XHCI_ROUTE_STRING_MAX_PORT,
                });
            }
            raw = (raw << 4) | (port as u32);
        }
        Self::from_raw(raw)
    }
}
