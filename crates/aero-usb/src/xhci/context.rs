//! xHCI context structures.
//!
//! xHCI uses in-memory "contexts" to represent device/endpoint state. This file provides raw
//! context wrappers and a few common field helpers. It is not yet a full device-context manager.

/// Size of each xHCI context structure in bytes (slot/endpoint contexts are 32 bytes each).
pub const CONTEXT_SIZE: usize = 32;

/// Input Control Context (ICC) (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct InputControlContext {
    dwords: [u32; 8],
}

impl InputControlContext {
    pub fn dword(&self, index: usize) -> u32 {
        self.dwords.get(index).copied().unwrap_or(0)
    }

    pub fn set_dword(&mut self, index: usize, value: u32) {
        if let Some(dw) = self.dwords.get_mut(index) {
            *dw = value;
        }
    }

    /// `add_flags` field (DW1) indicating which contexts are valid in an Input Context.
    pub fn add_flags(&self) -> u32 {
        self.dwords[1]
    }

    pub fn set_add_flags(&mut self, value: u32) {
        self.dwords[1] = value;
    }

    /// `drop_flags` field (DW0) indicating which contexts should be dropped.
    pub fn drop_flags(&self) -> u32 {
        self.dwords[0]
    }

    pub fn set_drop_flags(&mut self, value: u32) {
        self.dwords[0] = value;
    }
}

/// Slot Context (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SlotContext {
    dwords: [u32; 8],
}

impl SlotContext {
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
}

/// Endpoint Context (32 bytes / 8 dwords).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EndpointContext {
    dwords: [u32; 8],
}

impl EndpointContext {
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

    /// TR Dequeue Pointer (masked) combines DW2-DW3 in a full endpoint context.
    ///
    /// This returns the lower 32 bits from DW2 for now; full helpers will be added when the
    /// controller uses endpoint contexts.
    pub fn tr_dequeue_ptr_lo(&self) -> u32 {
        self.dwords[2]
    }

    pub fn set_tr_dequeue_ptr_lo(&mut self, value: u32) {
        self.dwords[2] = value;
    }
}

