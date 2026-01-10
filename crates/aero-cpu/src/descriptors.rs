#![forbid(unsafe_code)]

use core::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DescriptorTableReg {
    pub base: u64,
    pub limit: u16,
}

impl DescriptorTableReg {
    pub fn contains(&self, offset: u64, bytes: u64) -> bool {
        let end = offset.saturating_add(bytes.saturating_sub(1));
        end <= self.limit as u64
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentDescriptor {
    pub base: u64,
    pub limit: u32,
    pub access: u8,
    pub flags: u8, // [3:0] = AVL/L/DB/G
}

impl SegmentDescriptor {
    pub fn descriptor_type(&self) -> bool {
        self.access & 0x10 != 0
    }

    pub fn is_present(&self) -> bool {
        self.access & 0x80 != 0
    }

    pub fn dpl(&self) -> u8 {
        (self.access >> 5) & 0x3
    }

    pub fn is_code(&self) -> bool {
        self.descriptor_type() && (self.access & 0x08 != 0)
    }

    pub fn is_data(&self) -> bool {
        self.descriptor_type() && (self.access & 0x08 == 0)
    }

    pub fn code_readable(&self) -> bool {
        self.is_code() && (self.access & 0x02 != 0)
    }

    pub fn data_writable(&self) -> bool {
        self.is_data() && (self.access & 0x02 != 0)
    }

    pub fn code_conforming(&self) -> bool {
        self.is_code() && (self.access & 0x04 != 0)
    }

    pub fn long(&self) -> bool {
        self.flags & 0b0010 != 0
    }

    pub fn default_operand_size_32(&self) -> bool {
        self.flags & 0b0100 != 0
    }

    pub fn granularity_4k(&self) -> bool {
        self.flags & 0b1000 != 0
    }

    pub fn effective_limit(&self) -> u32 {
        if self.granularity_4k() {
            (self.limit << 12) | 0xFFF
        } else {
            self.limit
        }
    }
}

pub fn parse_segment_descriptor(bytes: [u8; 8]) -> SegmentDescriptor {
    let limit_low = u16::from_le_bytes([bytes[0], bytes[1]]) as u32;
    let base_low = u16::from_le_bytes([bytes[2], bytes[3]]) as u32;
    let base_mid = bytes[4] as u32;
    let access = bytes[5];
    let limit_high = (bytes[6] & 0x0F) as u32;
    let flags = bytes[6] >> 4;
    let base_high = bytes[7] as u32;

    let base = (base_low | (base_mid << 16) | (base_high << 24)) as u64;
    let limit = limit_low | (limit_high << 16);

    SegmentDescriptor {
        base,
        limit,
        access,
        flags,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemDescriptor {
    pub base: u64,
    pub limit: u32,
    pub access: u8,
    pub flags: u8,
}

impl SystemDescriptor {
    pub fn is_present(&self) -> bool {
        self.access & 0x80 != 0
    }

    pub fn dpl(&self) -> u8 {
        (self.access >> 5) & 0x3
    }

    pub fn system_type(&self) -> u8 {
        self.access & 0x0F
    }

    pub fn granularity_4k(&self) -> bool {
        self.flags & 0b1000 != 0
    }

    pub fn effective_limit(&self) -> u32 {
        if self.granularity_4k() {
            (self.limit << 12) | 0xFFF
        } else {
            self.limit
        }
    }
}

pub fn parse_system_descriptor(bytes: [u8; 8]) -> SystemDescriptor {
    let seg = parse_segment_descriptor(bytes);
    SystemDescriptor {
        base: seg.base,
        limit: seg.limit,
        access: seg.access,
        flags: seg.flags,
    }
}

pub fn parse_system_descriptor_64(bytes: [u8; 16]) -> SystemDescriptor {
    let low: [u8; 8] = bytes[0..8].try_into().expect("slice length verified");
    let high: [u8; 8] = bytes[8..16].try_into().expect("slice length verified");

    let mut desc = parse_system_descriptor(low);
    let base_high = u32::from_le_bytes([high[0], high[1], high[2], high[3]]) as u64;
    desc.base |= base_high << 32;
    desc
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateType {
    Interrupt,
    Trap,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateSize {
    Bits16,
    Bits32,
    Bits64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdtGateDescriptor {
    pub offset: u64,
    pub selector: u16,
    pub ist: u8,
    pub gate_type: GateType,
    pub dpl: u8,
    pub present: bool,
    pub size: GateSize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateDescriptorError {
    InvalidType(u8),
}

impl fmt::Display for GateDescriptorError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            GateDescriptorError::InvalidType(ty) => write!(f, "invalid gate type {ty:#x}"),
        }
    }
}

impl std::error::Error for GateDescriptorError {}

pub fn parse_idt_gate_descriptor_32(
    bytes: [u8; 8],
) -> Result<IdtGateDescriptor, GateDescriptorError> {
    let offset_low = u16::from_le_bytes([bytes[0], bytes[1]]) as u32;
    let selector = u16::from_le_bytes([bytes[2], bytes[3]]);
    let type_attr = bytes[5];
    let offset_high = u16::from_le_bytes([bytes[6], bytes[7]]) as u32;

    let offset = (offset_low | (offset_high << 16)) as u64;
    let present = type_attr & 0x80 != 0;
    let dpl = (type_attr >> 5) & 0x3;
    let gate_type = match type_attr & 0x0F {
        0x6 => (GateType::Interrupt, GateSize::Bits16),
        0x7 => (GateType::Trap, GateSize::Bits16),
        0xE => (GateType::Interrupt, GateSize::Bits32),
        0xF => (GateType::Trap, GateSize::Bits32),
        other => return Err(GateDescriptorError::InvalidType(other)),
    };

    Ok(IdtGateDescriptor {
        offset,
        selector,
        ist: 0,
        gate_type: gate_type.0,
        dpl,
        present,
        size: gate_type.1,
    })
}

pub fn parse_idt_gate_descriptor_64(
    bytes: [u8; 16],
) -> Result<IdtGateDescriptor, GateDescriptorError> {
    let offset_low = u16::from_le_bytes([bytes[0], bytes[1]]) as u64;
    let selector = u16::from_le_bytes([bytes[2], bytes[3]]);
    let ist = bytes[4] & 0x7;
    let type_attr = bytes[5];
    let offset_mid = u16::from_le_bytes([bytes[6], bytes[7]]) as u64;
    let offset_high = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as u64;

    let offset = offset_low | (offset_mid << 16) | (offset_high << 32);
    let present = type_attr & 0x80 != 0;
    let dpl = (type_attr >> 5) & 0x3;
    let gate_type = match type_attr & 0x0F {
        0xE => GateType::Interrupt,
        0xF => GateType::Trap,
        other => return Err(GateDescriptorError::InvalidType(other)),
    };

    Ok(IdtGateDescriptor {
        offset,
        selector,
        ist,
        gate_type,
        dpl,
        present,
        size: GateSize::Bits64,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RealModeIdtEntry {
    pub offset: u16,
    pub segment: u16,
}

pub fn parse_real_mode_idt_entry(bytes: [u8; 4]) -> RealModeIdtEntry {
    let offset = u16::from_le_bytes([bytes[0], bytes[1]]);
    let segment = u16::from_le_bytes([bytes[2], bytes[3]]);
    RealModeIdtEntry { offset, segment }
}
