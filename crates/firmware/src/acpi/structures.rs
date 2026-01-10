//! Raw ACPI table structures.
//!
//! All multibyte integers are stored in little-endian form (`to_le()` is used
//! when populating fields).

use core::mem::size_of;

pub const ACPI_HEADER_SIZE: usize = 36;
pub const ACPI_HEADER_CHECKSUM_OFFSET: usize = 9;

pub const RSDP_V2_SIZE: usize = 36;
pub const RSDP_CHECKSUM_LEN_V1: usize = 20;

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct AcpiHeader {
    pub signature: [u8; 4],
    pub length: u32,
    pub revision: u8,
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub oem_table_id: [u8; 8],
    pub oem_revision: u32,
    pub creator_id: u32,
    pub creator_revision: u32,
}

const _: [(); ACPI_HEADER_SIZE] = [(); size_of::<AcpiHeader>()];

#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct RsdpV2 {
    pub signature: [u8; 8], // "RSD PTR "
    pub checksum: u8,
    pub oem_id: [u8; 6],
    pub revision: u8,
    pub rsdt_address: u32,
    // ACPI 2.0+ fields
    pub length: u32,
    pub xsdt_address: u64,
    pub extended_checksum: u8,
    pub reserved: [u8; 3],
}

const _: [(); RSDP_V2_SIZE] = [(); size_of::<RsdpV2>()];

/// ACPI Generic Address Structure (GAS).
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct GenericAddress {
    pub address_space_id: u8,
    pub register_bit_width: u8,
    pub register_bit_offset: u8,
    pub access_size: u8,
    pub address: u64,
}

const _: [(); 12] = [(); size_of::<GenericAddress>()];

/// Firmware ACPI Control Structure (FACS).
///
/// Stored in ACPI NVS memory; referenced by the FADT.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
pub struct Facs {
    pub signature: [u8; 4], // "FACS"
    pub length: u32,
    pub hardware_signature: u32,
    pub firmware_waking_vector: u32,
    pub global_lock: u32,
    pub flags: u32,
    pub x_firmware_waking_vector: u64,
    pub version: u8,
    pub reserved: [u8; 3],
    pub ospm_flags: u32,
    pub reserved2: [u8; 24],
}

const _: [(); 64] = [(); size_of::<Facs>()];

/// Fixed ACPI Description Table (FADT / FACP).
///
/// This struct matches the ACPI 2.0 FADT (length 244 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
#[allow(non_snake_case)]
pub struct Fadt {
    pub header: AcpiHeader,
    pub FirmwareCtrl: u32,
    pub Dsdt: u32,
    pub Reserved0: u8,
    pub PreferredPmProfile: u8,
    pub SciInt: u16,
    pub SmiCmd: u32,
    pub AcpiEnable: u8,
    pub AcpiDisable: u8,
    pub S4BiosReq: u8,
    pub PstateCnt: u8,
    pub Pm1aEvtBlk: u32,
    pub Pm1bEvtBlk: u32,
    pub Pm1aCntBlk: u32,
    pub Pm1bCntBlk: u32,
    pub Pm2CntBlk: u32,
    pub PmTmrBlk: u32,
    pub Gpe0Blk: u32,
    pub Gpe1Blk: u32,
    pub Pm1EvtLen: u8,
    pub Pm1CntLen: u8,
    pub Pm2CntLen: u8,
    pub PmTmrLen: u8,
    pub Gpe0BlkLen: u8,
    pub Gpe1BlkLen: u8,
    pub Gpe1Base: u8,
    pub CstCnt: u8,
    pub PLvl2Lat: u16,
    pub PLvl3Lat: u16,
    pub FlushSize: u16,
    pub FlushStride: u16,
    pub DutyOffset: u8,
    pub DutyWidth: u8,
    pub DayAlrm: u8,
    pub MonAlrm: u8,
    pub Century: u8,
    pub IapcBootArch: u16,
    pub Reserved1: u8,
    pub Flags: u32,
    pub ResetReg: GenericAddress,
    pub ResetValue: u8,
    pub Reserved2: [u8; 3],
    pub X_FirmwareCtrl: u64,
    pub X_Dsdt: u64,
    pub X_Pm1aEvtBlk: GenericAddress,
    pub X_Pm1bEvtBlk: GenericAddress,
    pub X_Pm1aCntBlk: GenericAddress,
    pub X_Pm1bCntBlk: GenericAddress,
    pub X_Pm2CntBlk: GenericAddress,
    pub X_PmTmrBlk: GenericAddress,
    pub X_Gpe0Blk: GenericAddress,
    pub X_Gpe1Blk: GenericAddress,
}

const _: [(); 244] = [(); size_of::<Fadt>()];

/// HPET ACPI table.
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
#[allow(non_snake_case)]
pub struct Hpet {
    pub header: AcpiHeader,
    pub EventTimerBlockId: u32,
    pub BaseAddress: GenericAddress,
    pub HpetNumber: u8,
    pub MinimumTick: u16,
    pub PageProtection: u8,
}

const _: [(); 56] = [(); size_of::<Hpet>()];

pub fn as_bytes<T>(val: &T) -> &[u8] {
    // Safety: All table structs are plain old data with no padding (packed).
    unsafe { core::slice::from_raw_parts(val as *const T as *const u8, size_of::<T>()) }
}

