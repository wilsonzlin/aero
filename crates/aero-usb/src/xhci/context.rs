//! xHCI context structures and parsing helpers (32-byte contexts).
//!
//! xHCI uses in-memory "contexts" to represent device/endpoint state. This module provides raw
//! context wrappers, common field helpers, and safe guest-memory read helpers for the context
//! structures consumed by commands like **Address Device** and **Configure Endpoint**.
//!
//! MVP assumption: `HCCPARAMS1.CSZ = 0`, i.e. **32-byte contexts**.
//! 64-byte contexts are not supported yet.

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

fn read_context32_dwords(mem: &mut impl MemoryBus, paddr: u64) -> [u32; CONTEXT_DWORDS] {
    let mut raw = [0u8; CONTEXT_DWORDS * 4];
    mem.read_physical(paddr, &mut raw);
    let mut out = [0u32; CONTEXT_DWORDS];
    for (i, dword) in out.iter_mut().enumerate() {
        let off = i * 4;
        *dword = u32::from_le_bytes([raw[off], raw[off + 1], raw[off + 2], raw[off + 3]]);
    }
    out
}

fn read_u64_le(mem: &mut impl MemoryBus, paddr: u64) -> u64 {
    let mut raw = [0u8; 8];
    mem.read_physical(paddr, &mut raw);
    u64::from_le_bytes(raw)
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
    pub fn read_from(mem: &mut impl MemoryBus, paddr: u64) -> Self {
        Self {
            dwords: read_context32_dwords(mem, paddr),
        }
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
    pub fn read_from(mem: &mut impl MemoryBus, paddr: u64) -> Self {
        Self {
            dwords: read_context32_dwords(mem, paddr),
        }
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

    /// Root Hub Port Number field (DW1 bits 0..=7).
    pub fn root_hub_port_number(&self) -> u8 {
        (self.dwords[1] & 0xff) as u8
    }

    pub fn set_root_hub_port_number(&mut self, port: u8) {
        self.dwords[1] = (self.dwords[1] & !0xff) | (port as u32);
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

/// Endpoint Context (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointContext {
    dwords: [u32; CONTEXT_DWORDS],
}

impl EndpointContext {
    pub fn read_from(mem: &mut impl MemoryBus, paddr: u64) -> Self {
        Self {
            dwords: read_context32_dwords(mem, paddr),
        }
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

    pub fn set_endpoint_state(&mut self, state: u8) {
        let state = (state as u32) & 0x7;
        self.dwords[0] = (self.dwords[0] & !0x7) | state;
    }

    /// Interval field (DW0 bits 16..=23).
    pub fn interval(&self) -> u8 {
        ((self.dwords[0] >> 16) & 0xff) as u8
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

    /// Lower 32 bits of the raw TR Dequeue Pointer field (DW2).
    pub fn tr_dequeue_ptr_lo(&self) -> u32 {
        self.dwords[2]
    }

    pub fn set_tr_dequeue_ptr_lo(&mut self, value: u32) {
        self.dwords[2] = value;
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

    pub fn input_control(&self, mem: &mut impl MemoryBus) -> InputControlContext {
        InputControlContext::read_from(mem, self.base)
    }

    /// Reads the Slot Context (Device Context index 0).
    pub fn slot_context(&self, mem: &mut impl MemoryBus) -> Result<SlotContext, ContextError> {
        let addr = self
            .base
            .checked_add(CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        Ok(SlotContext::read_from(mem, addr))
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
        mem: &mut impl MemoryBus,
        device_context_index: u8,
    ) -> Result<EndpointContext, ContextError> {
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
        Ok(EndpointContext::read_from(mem, addr))
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

    pub fn slot_context(&self, mem: &mut impl MemoryBus) -> SlotContext {
        SlotContext::read_from(mem, self.base)
    }

    pub fn endpoint_context(
        &self,
        mem: &mut impl MemoryBus,
        device_context_index: u8,
    ) -> Result<EndpointContext, ContextError> {
        if !(1..=31).contains(&device_context_index) {
            return Err(ContextError::InvalidDeviceContextIndex(device_context_index));
        }

        let addr = self
            .base
            .checked_add(device_context_index as u64 * CONTEXT_SIZE as u64)
            .ok_or(ContextError::AddressOverflow)?;
        Ok(EndpointContext::read_from(mem, addr))
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
        mem: &mut impl MemoryBus,
        slot_id: u8,
    ) -> Result<u64, ContextError> {
        let addr = self.entry_addr(slot_id)?;
        Ok(read_u64_le(mem, addr))
    }
}
