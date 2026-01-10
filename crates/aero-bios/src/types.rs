use core::fmt;

/// Physical base address where the emulator is expected to map the BIOS ROM.
pub const BIOS_BASE: u32 = 0x000F_0000;
pub const BIOS_SIZE: usize = 0x10000; // 64 KiB

/// Physical reset vector address (alias for `F000:FFF0`).
pub const RESET_VECTOR_PHYS: u32 = 0xFFFF_FFF0u32;

pub const FLAG_CF: u32 = 1 << 0;
pub const FLAG_ZF: u32 = 1 << 6;
pub const FLAG_IF: u32 = 1 << 9;

#[derive(Clone, Copy, Default)]
pub struct RealModeCpu {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
    pub esi: u32,
    pub edi: u32,
    pub ebp: u32,
    pub esp: u32,
    pub eip: u32,
    pub eflags: u32,

    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
}

impl fmt::Debug for RealModeCpu {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RealModeCpu")
            .field("eax", &format_args!("{:08x}", self.eax))
            .field("ebx", &format_args!("{:08x}", self.ebx))
            .field("ecx", &format_args!("{:08x}", self.ecx))
            .field("edx", &format_args!("{:08x}", self.edx))
            .field("esi", &format_args!("{:08x}", self.esi))
            .field("edi", &format_args!("{:08x}", self.edi))
            .field("ebp", &format_args!("{:08x}", self.ebp))
            .field("esp", &format_args!("{:08x}", self.esp))
            .field("eip", &format_args!("{:08x}", self.eip))
            .field("eflags", &format_args!("{:08x}", self.eflags))
            .field("cs", &format_args!("{:04x}", self.cs))
            .field("ds", &format_args!("{:04x}", self.ds))
            .field("es", &format_args!("{:04x}", self.es))
            .field("ss", &format_args!("{:04x}", self.ss))
            .finish()
    }
}

impl RealModeCpu {
    pub fn ip(&self) -> u16 {
        self.eip as u16
    }

    pub fn set_ip(&mut self, ip: u16) {
        self.eip = (self.eip & 0xFFFF_0000) | ip as u32;
    }

    pub fn cs_base(&self) -> u32 {
        (self.cs as u32) << 4
    }

    pub fn ds_base(&self) -> u32 {
        (self.ds as u32) << 4
    }

    pub fn es_base(&self) -> u32 {
        (self.es as u32) << 4
    }

    pub fn ss_base(&self) -> u32 {
        (self.ss as u32) << 4
    }

    pub fn ax(&self) -> u16 {
        self.eax as u16
    }

    pub fn set_ax(&mut self, v: u16) {
        self.eax = (self.eax & 0xFFFF_0000) | v as u32;
    }

    pub fn ah(&self) -> u8 {
        (self.eax >> 8) as u8
    }

    pub fn set_ah(&mut self, v: u8) {
        self.eax = (self.eax & 0xFFFF_00FF) | ((v as u32) << 8);
    }

    pub fn al(&self) -> u8 {
        self.eax as u8
    }

    pub fn set_al(&mut self, v: u8) {
        self.eax = (self.eax & 0xFFFF_FF00) | v as u32;
    }

    pub fn bx(&self) -> u16 {
        self.ebx as u16
    }

    pub fn set_bx(&mut self, v: u16) {
        self.ebx = (self.ebx & 0xFFFF_0000) | v as u32;
    }

    pub fn cx(&self) -> u16 {
        self.ecx as u16
    }

    pub fn set_cx(&mut self, v: u16) {
        self.ecx = (self.ecx & 0xFFFF_0000) | v as u32;
    }

    pub fn dx(&self) -> u16 {
        self.edx as u16
    }

    pub fn set_dx(&mut self, v: u16) {
        self.edx = (self.edx & 0xFFFF_0000) | v as u32;
    }

    pub fn dl(&self) -> u8 {
        self.edx as u8
    }

    pub fn set_dl(&mut self, v: u8) {
        self.edx = (self.edx & 0xFFFF_FF00) | v as u32;
    }

    pub fn dh(&self) -> u8 {
        (self.edx >> 8) as u8
    }

    pub fn set_dh(&mut self, v: u8) {
        self.edx = (self.edx & 0xFFFF_00FF) | ((v as u32) << 8);
    }

    pub fn cf(&self) -> bool {
        (self.eflags & FLAG_CF) != 0
    }

    pub fn set_cf(&mut self, v: bool) {
        if v {
            self.eflags |= FLAG_CF;
        } else {
            self.eflags &= !FLAG_CF;
        }
    }

    pub fn zf(&self) -> bool {
        (self.eflags & FLAG_ZF) != 0
    }

    pub fn set_zf(&mut self, v: bool) {
        if v {
            self.eflags |= FLAG_ZF;
        } else {
            self.eflags &= !FLAG_ZF;
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
#[repr(C, packed)]
pub struct E820Entry {
    pub base: u64,
    pub length: u64,
    pub region_type: u32,
    pub extended_attributes: u32,
}

pub const E820_TYPE_RAM: u32 = 1;
pub const E820_TYPE_RESERVED: u32 = 2;
pub const E820_TYPE_ACPI: u32 = 3;
pub const E820_TYPE_NVS: u32 = 4;
pub const E820_TYPE_UNUSABLE: u32 = 5;
