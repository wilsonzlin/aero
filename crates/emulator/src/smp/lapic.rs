//! Local APIC model focused on IPI delivery (INIT/SIPI/fixed).

use std::collections::VecDeque;

pub const LOCAL_APIC_BASE: u64 = 0xFEE0_0000;

// xAPIC register offsets used by this minimal model.
pub const APIC_REG_ID: u64 = 0x20;
pub const APIC_REG_EOI: u64 = 0xB0;
pub const APIC_REG_ICR_LOW: u64 = 0x300;
pub const APIC_REG_ICR_HIGH: u64 = 0x310;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliveryMode {
    Fixed,
    LowestPriority,
    Smi,
    RemoteRead,
    Nmi,
    Init,
    Startup,
    ExtInt,
    Reserved(u8),
}

impl DeliveryMode {
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x7 {
            0 => Self::Fixed,
            1 => Self::LowestPriority,
            2 => Self::Smi,
            3 => Self::RemoteRead,
            4 => Self::Nmi,
            5 => Self::Init,
            6 => Self::Startup,
            7 => Self::ExtInt,
            other => Self::Reserved(other),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DestinationShorthand {
    None,
    SelfOnly,
    AllIncludingSelf,
    AllExcludingSelf,
}

impl DestinationShorthand {
    pub fn from_bits(bits: u8) -> Self {
        match bits & 0x3 {
            0 => Self::None,
            1 => Self::SelfOnly,
            2 => Self::AllIncludingSelf,
            3 => Self::AllExcludingSelf,
            _ => unreachable!(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Deassert,
    Assert,
}

impl Level {
    pub fn from_bit(bit: bool) -> Self {
        if bit {
            Self::Assert
        } else {
            Self::Deassert
        }
    }
}

/// Decoded ICR send request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Icr {
    pub vector: u8,
    pub delivery_mode: DeliveryMode,
    pub level: Level,
    pub destination_shorthand: DestinationShorthand,
    /// xAPIC physical destination field (APIC ID).
    pub destination: u8,
}

impl Icr {
    pub fn decode(icr_low: u32, icr_high: u32) -> Self {
        let vector = (icr_low & 0xFF) as u8;
        let delivery_mode = DeliveryMode::from_bits(((icr_low >> 8) & 0x7) as u8);
        let level = Level::from_bit(((icr_low >> 14) & 1) != 0);
        let destination_shorthand = DestinationShorthand::from_bits(((icr_low >> 18) & 0x3) as u8);
        let destination = ((icr_high >> 24) & 0xFF) as u8;

        Self {
            vector,
            delivery_mode,
            level,
            destination_shorthand,
            destination,
        }
    }
}

/// Local APIC state for one vCPU.
#[derive(Debug, Clone)]
pub struct LocalApic {
    pub apic_id: u8,

    icr_high: u32,
    pending: VecDeque<u8>,
}

impl LocalApic {
    pub fn new(apic_id: u8) -> Self {
        Self {
            apic_id,
            icr_high: 0,
            pending: VecDeque::new(),
        }
    }

    pub fn push_interrupt(&mut self, vector: u8) {
        self.pending.push_back(vector);
    }

    pub fn pop_interrupt(&mut self) -> Option<u8> {
        self.pending.pop_front()
    }

    pub fn icr_high(&self) -> u32 {
        self.icr_high
    }

    pub fn set_icr_high(&mut self, value: u32) {
        self.icr_high = value;
    }

    pub fn pending_interrupts(&self) -> Vec<u8> {
        self.pending.iter().copied().collect()
    }

    pub fn set_pending_interrupts(&mut self, pending: Vec<u8>) {
        self.pending = pending.into();
    }

    /// Read a 32-bit APIC register.
    pub fn read(&self, offset: u64) -> u32 {
        match offset {
            APIC_REG_ID => (self.apic_id as u32) << 24,
            APIC_REG_ICR_HIGH => self.icr_high,
            _ => 0,
        }
    }

    /// Write a 32-bit APIC register.
    ///
    /// Writing `ICR_LOW` returns a decoded `Icr` to be processed by the machine
    /// (IPI delivery requires access to other CPUs).
    pub fn write(&mut self, offset: u64, value: u32) -> Option<Icr> {
        match offset {
            APIC_REG_EOI => {
                // In a full model this would deassert the in-service vector.
                None
            }
            APIC_REG_ICR_HIGH => {
                self.icr_high = value;
                None
            }
            APIC_REG_ICR_LOW => Some(Icr::decode(value, self.icr_high)),
            _ => None,
        }
    }
}
