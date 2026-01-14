//! Transfer Request Block (TRB) encoding/decoding.
//!
//! xHCI uses a common 16-byte TRB format for commands, transfers, and events. This module provides
//! a raw `Trb` representation along with helpers for extracting common fields (cycle bit, TRB type,
//! slot id, endpoint id, etc).

use core::fmt;

use crate::MemoryBus;

/// Size of a TRB in bytes.
pub const TRB_LEN: usize = 16;

/// xHCI completion codes (Completion Code field in Event TRBs, bits 24..=31 of DW2).
///
/// Values are defined by the xHCI specification and are shared across Command Completion and
/// Transfer Event TRBs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CompletionCode {
    /// Invalid completion code (0).
    ///
    /// This is not a valid completion outcome for architectural TRBs, but is the reserved "invalid"
    /// value in the xHCI specification.
    Invalid = 0,
    Success = 1,
    UsbTransactionError = 4,
    TrbError = 5,
    StallError = 6,
    NoSlotsAvailableError = 9,
    SlotNotEnabledError = 11,
    EndpointNotEnabledError = 12,
    ShortPacket = 13,
    ParameterError = 17,
    ContextStateError = 19,
}

impl CompletionCode {
    pub const fn as_u8(self) -> u8 {
        self as u8
    }

    #[inline]
    pub const fn raw(self) -> u8 {
        self as u8
    }
}

impl fmt::Display for CompletionCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}({})", self, *self as u8)
    }
}

/// Raw xHCI Transfer Request Block (TRB).
///
/// The canonical wire format is 16 bytes:
/// - parameter: 64-bit (DW0-DW1)
/// - status: 32-bit (DW2)
/// - control: 32-bit (DW3)
///
/// This type intentionally preserves unknown TRB types: callers can always inspect `control`
/// directly.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Trb {
    pub parameter: u64,
    pub status: u32,
    pub control: u32,
}

impl Trb {
    /// TRB cycle bit (C) in the control dword.
    pub const CONTROL_CYCLE_BIT: u32 = 1 << 0;
    /// Link TRB "toggle cycle" bit (TC) in the control dword.
    pub const CONTROL_LINK_TOGGLE_CYCLE: u32 = 1 << 1;
    /// TRB chain bit (CH) in the control dword.
    pub const CONTROL_CHAIN_BIT: u32 = 1 << 4;
    /// TRB interrupt-on-completion bit (IOC) in the control dword.
    pub const CONTROL_IOC_BIT: u32 = 1 << 5;

    /// TRB type field location in the control dword.
    pub const CONTROL_TRB_TYPE_SHIFT: u32 = 10;
    pub const CONTROL_TRB_TYPE_MASK: u32 = 0x3f << Self::CONTROL_TRB_TYPE_SHIFT;

    /// Address Device Command TRB "Block Set Address Request" (BSR) bit.
    ///
    /// Reference: xHCI 1.2 §6.4.3.4 "Address Device Command TRB".
    pub const CONTROL_ADDRESS_DEVICE_BSR_BIT: u32 = 1 << 9;

    /// Configure Endpoint Command TRB "Deconfigure" bit.
    ///
    /// Reference: xHCI 1.2 §6.4.3.5 "Configure Endpoint Command TRB".
    pub const CONTROL_CONFIGURE_ENDPOINT_DECONFIGURE_BIT: u32 = 1 << 9;

    /// Event/command slot ID field (bits 24..=31).
    pub const CONTROL_SLOT_ID_SHIFT: u32 = 24;
    pub const CONTROL_SLOT_ID_MASK: u32 = 0xff << Self::CONTROL_SLOT_ID_SHIFT;

    /// Event/command endpoint ID field (bits 16..=20).
    pub const CONTROL_ENDPOINT_ID_SHIFT: u32 = 16;
    pub const CONTROL_ENDPOINT_ID_MASK: u32 = 0x1f << Self::CONTROL_ENDPOINT_ID_SHIFT;

    /// Completion code in the status dword (bits 24..=31) for event TRBs.
    pub const STATUS_COMPLETION_CODE_SHIFT: u32 = 24;
    pub const STATUS_COMPLETION_CODE_MASK: u32 = 0xff << Self::STATUS_COMPLETION_CODE_SHIFT;
    /// Transfer length field (bits 0..=16) for transfer TRBs (e.g. Normal TRB).
    pub const STATUS_TRANSFER_LEN_MASK: u32 = 0x1ffff;

    /// Interrupt-on-completion (IOC) bit in the control dword.
    ///
    /// For transfer TRBs this requests a Transfer Event when the associated Transfer Descriptor
    /// completes.
    pub const CONTROL_IOC: u32 = 1 << 5;

    /// Direction bit (DIR) used by DataStage/StatusStage TRBs.
    ///
    /// For DataStage TRBs: `1` indicates an IN stage (device-to-host).
    /// For StatusStage TRBs: `1` indicates an IN status stage.
    pub const CONTROL_DIR: u32 = 1 << 16;

    /// Transfer TRB transfer-length field mask (bits 0..=16 of the status dword).
    pub const STATUS_TRB_TRANSFER_LENGTH_MASK: u32 = 0x1ffff;

    #[inline]
    pub const fn new(parameter: u64, status: u32, control: u32) -> Self {
        Self {
            parameter,
            status,
            control,
        }
    }

    #[inline]
    pub const fn from_dwords(d0: u32, d1: u32, d2: u32, d3: u32) -> Self {
        Self {
            parameter: (d1 as u64) << 32 | (d0 as u64),
            status: d2,
            control: d3,
        }
    }

    #[inline]
    pub const fn dword0(&self) -> u32 {
        self.parameter as u32
    }

    #[inline]
    pub const fn dword1(&self) -> u32 {
        (self.parameter >> 32) as u32
    }

    #[inline]
    pub const fn dword2(&self) -> u32 {
        self.status
    }

    #[inline]
    pub const fn dword3(&self) -> u32 {
        self.control
    }

    #[inline]
    pub fn from_bytes(bytes: [u8; TRB_LEN]) -> Self {
        let d0 = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        let d1 = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
        let d2 = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
        let d3 = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
        Self::from_dwords(d0, d1, d2, d3)
    }

    #[inline]
    pub fn to_bytes(&self) -> [u8; TRB_LEN] {
        let mut out = [0u8; TRB_LEN];
        out[0..4].copy_from_slice(&self.dword0().to_le_bytes());
        out[4..8].copy_from_slice(&self.dword1().to_le_bytes());
        out[8..12].copy_from_slice(&self.dword2().to_le_bytes());
        out[12..16].copy_from_slice(&self.dword3().to_le_bytes());
        out
    }

    #[inline]
    pub fn read_from(mem: &mut (impl MemoryBus + ?Sized), paddr: u64) -> Self {
        let mut buf = [0u8; TRB_LEN];
        mem.read_bytes(paddr, &mut buf);
        Self::from_bytes(buf)
    }

    #[inline]
    pub fn write_to(&self, mem: &mut (impl MemoryBus + ?Sized), paddr: u64) {
        mem.write_bytes(paddr, &self.to_bytes());
    }

    #[inline]
    pub const fn cycle(&self) -> bool {
        (self.control & Self::CONTROL_CYCLE_BIT) != 0
    }

    #[inline]
    pub fn set_cycle(&mut self, on: bool) {
        self.control = (self.control & !Self::CONTROL_CYCLE_BIT) | (on as u32);
    }

    #[inline]
    pub const fn trb_type_raw(&self) -> u8 {
        ((self.control & Self::CONTROL_TRB_TYPE_MASK) >> Self::CONTROL_TRB_TYPE_SHIFT) as u8
    }

    #[inline]
    pub const fn trb_type(&self) -> TrbType {
        TrbType::from_raw(self.trb_type_raw())
    }

    #[inline]
    pub fn set_trb_type_raw(&mut self, raw: u8) {
        let raw = (raw as u32) & 0x3f;
        self.control =
            (self.control & !Self::CONTROL_TRB_TYPE_MASK) | (raw << Self::CONTROL_TRB_TYPE_SHIFT);
    }

    #[inline]
    pub fn set_trb_type(&mut self, ty: TrbType) {
        self.set_trb_type_raw(ty.raw());
    }

    #[inline]
    pub const fn slot_id(&self) -> u8 {
        ((self.control & Self::CONTROL_SLOT_ID_MASK) >> Self::CONTROL_SLOT_ID_SHIFT) as u8
    }

    #[inline]
    pub fn set_slot_id(&mut self, slot_id: u8) {
        self.control = (self.control & !Self::CONTROL_SLOT_ID_MASK)
            | ((slot_id as u32) << Self::CONTROL_SLOT_ID_SHIFT);
    }

    #[inline]
    pub const fn endpoint_id(&self) -> u8 {
        ((self.control & Self::CONTROL_ENDPOINT_ID_MASK) >> Self::CONTROL_ENDPOINT_ID_SHIFT) as u8
    }

    #[inline]
    pub fn set_endpoint_id(&mut self, endpoint_id: u8) {
        let endpoint_id = (endpoint_id as u32) & 0x1f;
        self.control = (self.control & !Self::CONTROL_ENDPOINT_ID_MASK)
            | (endpoint_id << Self::CONTROL_ENDPOINT_ID_SHIFT);
    }

    /// For Address Device Command TRBs, returns the "Block Set Address Request" (BSR) flag.
    #[inline]
    pub const fn address_device_bsr(&self) -> bool {
        (self.control & Self::CONTROL_ADDRESS_DEVICE_BSR_BIT) != 0
    }

    #[inline]
    pub fn set_address_device_bsr(&mut self, on: bool) {
        self.control = (self.control & !Self::CONTROL_ADDRESS_DEVICE_BSR_BIT) | ((on as u32) << 9);
    }

    /// For Configure Endpoint Command TRBs, returns the "Deconfigure" flag.
    #[inline]
    pub const fn configure_endpoint_deconfigure(&self) -> bool {
        (self.control & Self::CONTROL_CONFIGURE_ENDPOINT_DECONFIGURE_BIT) != 0
    }

    #[inline]
    pub fn set_configure_endpoint_deconfigure(&mut self, on: bool) {
        self.control = (self.control & !Self::CONTROL_CONFIGURE_ENDPOINT_DECONFIGURE_BIT)
            | ((on as u32) << 9);
    }

    /// Interpret the parameter field as a pointer and mask it to 16-byte alignment.
    ///
    /// Many xHCI TRBs store guest physical pointers in bits 4..=63 with low bits reserved for
    /// flags. This helper is suitable for fields like:
    /// - Link TRB segment pointer
    /// - Evaluate Context input context pointer
    /// - Command Completion Event TRB "Command TRB Pointer"
    #[inline]
    pub const fn pointer(&self) -> u64 {
        self.parameter & !0x0f
    }

    /// For Link TRBs, returns the target segment pointer (masked to 16-byte alignment).
    #[inline]
    pub const fn link_segment_ptr(&self) -> u64 {
        self.pointer()
    }

    /// For Link TRBs, returns whether the cycle state should be toggled when following the link.
    #[inline]
    pub const fn link_toggle_cycle(&self) -> bool {
        (self.control & Self::CONTROL_LINK_TOGGLE_CYCLE) != 0
    }

    #[inline]
    pub fn set_link_toggle_cycle(&mut self, on: bool) {
        self.control = (self.control & !Self::CONTROL_LINK_TOGGLE_CYCLE) | ((on as u32) << 1);
    }

    /// For transfer TRBs, returns whether the TRB is chained to the next TRB (CH bit).
    #[inline]
    pub const fn chain(&self) -> bool {
        (self.control & Self::CONTROL_CHAIN_BIT) != 0
    }

    /// For transfer TRBs, returns whether the TRB should generate an event on completion (IOC bit).
    #[inline]
    pub const fn ioc(&self) -> bool {
        (self.control & Self::CONTROL_IOC_BIT) != 0
    }

    /// For transfer TRBs, returns the Transfer Length field.
    #[inline]
    pub const fn transfer_len(&self) -> u32 {
        self.status & Self::STATUS_TRANSFER_LEN_MASK
    }

    /// For event TRBs, returns the completion code field.
    #[inline]
    pub const fn completion_code_raw(&self) -> u8 {
        ((self.status & Self::STATUS_COMPLETION_CODE_MASK) >> Self::STATUS_COMPLETION_CODE_SHIFT)
            as u8
    }

    /// For Setup Stage TRBs, interpret the parameter field as an 8-byte USB SETUP packet.
    #[inline]
    pub fn setup_packet(&self) -> crate::SetupPacket {
        crate::SetupPacket::from_bytes(self.parameter.to_le_bytes())
    }

    /// For Data/Status Stage TRBs, returns whether the transfer direction is IN (device-to-host).
    #[inline]
    pub const fn dir_in(&self) -> bool {
        (self.control & Self::CONTROL_DIR) != 0
    }

    #[inline]
    pub fn set_dir_in(&mut self, on: bool) {
        self.control = (self.control & !Self::CONTROL_DIR) | ((on as u32) << 16);
    }
}

/// TRB type field.
///
/// Unknown/unsupported codes are preserved via [`TrbType::Unknown`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrbType {
    // Transfer TRBs.
    Normal,
    SetupStage,
    DataStage,
    StatusStage,
    Isoch,
    Link,
    EventData,
    NoOp,

    // Command TRBs.
    EnableSlotCommand,
    DisableSlotCommand,
    AddressDeviceCommand,
    ConfigureEndpointCommand,
    EvaluateContextCommand,
    ResetEndpointCommand,
    StopEndpointCommand,
    SetTrDequeuePointerCommand,
    ResetDeviceCommand,
    ForceEventCommand,
    NegotiateBandwidthCommand,
    SetLatencyToleranceValueCommand,
    GetPortBandwidthCommand,
    ForceHeaderCommand,
    NoOpCommand,

    // Event TRBs.
    TransferEvent,
    CommandCompletionEvent,
    PortStatusChangeEvent,
    BandwidthRequestEvent,
    DoorbellEvent,
    HostControllerEvent,
    DeviceNotificationEvent,
    MfindexWrapEvent,

    /// Any unrecognised TRB type code.
    Unknown(u8),
}

impl TrbType {
    #[inline]
    pub const fn from_raw(raw: u8) -> Self {
        match raw {
            1 => Self::Normal,
            2 => Self::SetupStage,
            3 => Self::DataStage,
            4 => Self::StatusStage,
            5 => Self::Isoch,
            6 => Self::Link,
            7 => Self::EventData,
            8 => Self::NoOp,

            9 => Self::EnableSlotCommand,
            10 => Self::DisableSlotCommand,
            11 => Self::AddressDeviceCommand,
            12 => Self::ConfigureEndpointCommand,
            13 => Self::EvaluateContextCommand,
            14 => Self::ResetEndpointCommand,
            15 => Self::StopEndpointCommand,
            16 => Self::SetTrDequeuePointerCommand,
            17 => Self::ResetDeviceCommand,
            18 => Self::ForceEventCommand,
            19 => Self::NegotiateBandwidthCommand,
            20 => Self::SetLatencyToleranceValueCommand,
            21 => Self::GetPortBandwidthCommand,
            22 => Self::ForceHeaderCommand,
            23 => Self::NoOpCommand,

            32 => Self::TransferEvent,
            33 => Self::CommandCompletionEvent,
            34 => Self::PortStatusChangeEvent,
            35 => Self::BandwidthRequestEvent,
            36 => Self::DoorbellEvent,
            37 => Self::HostControllerEvent,
            38 => Self::DeviceNotificationEvent,
            39 => Self::MfindexWrapEvent,

            other => Self::Unknown(other),
        }
    }

    #[inline]
    pub const fn raw(self) -> u8 {
        match self {
            Self::Normal => 1,
            Self::SetupStage => 2,
            Self::DataStage => 3,
            Self::StatusStage => 4,
            Self::Isoch => 5,
            Self::Link => 6,
            Self::EventData => 7,
            Self::NoOp => 8,

            Self::EnableSlotCommand => 9,
            Self::DisableSlotCommand => 10,
            Self::AddressDeviceCommand => 11,
            Self::ConfigureEndpointCommand => 12,
            Self::EvaluateContextCommand => 13,
            Self::ResetEndpointCommand => 14,
            Self::StopEndpointCommand => 15,
            Self::SetTrDequeuePointerCommand => 16,
            Self::ResetDeviceCommand => 17,
            Self::ForceEventCommand => 18,
            Self::NegotiateBandwidthCommand => 19,
            Self::SetLatencyToleranceValueCommand => 20,
            Self::GetPortBandwidthCommand => 21,
            Self::ForceHeaderCommand => 22,
            Self::NoOpCommand => 23,

            Self::TransferEvent => 32,
            Self::CommandCompletionEvent => 33,
            Self::PortStatusChangeEvent => 34,
            Self::BandwidthRequestEvent => 35,
            Self::DoorbellEvent => 36,
            Self::HostControllerEvent => 37,
            Self::DeviceNotificationEvent => 38,
            Self::MfindexWrapEvent => 39,

            Self::Unknown(raw) => raw,
        }
    }
}
