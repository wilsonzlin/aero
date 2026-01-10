//! AHCI register layout and bit definitions.

// HBA registers (offsets from ABAR).
pub const HBA_CAP: u64 = 0x00;
pub const HBA_GHC: u64 = 0x04;
pub const HBA_IS: u64 = 0x08;
pub const HBA_PI: u64 = 0x0C;
pub const HBA_VS: u64 = 0x10;
pub const HBA_CAP2: u64 = 0x24;
pub const HBA_BOHC: u64 = 0x28;
pub const HBA_PORTS_BASE: u64 = 0x100;
pub const HBA_PORT_STRIDE: u64 = 0x80;

// CAP bits.
pub const CAP_NP_MASK: u32 = 0x1f;
pub const CAP_NCS_MASK: u32 = 0x1f << 8;
pub const CAP_S64A: u32 = 1 << 31;

// GHC bits.
pub const GHC_HR: u32 = 1 << 0;
pub const GHC_IE: u32 = 1 << 1;
pub const GHC_AE: u32 = 1 << 31;

// Port register offsets (relative to port base).
pub const PX_CLB: u64 = 0x00;
pub const PX_CLBU: u64 = 0x04;
pub const PX_FB: u64 = 0x08;
pub const PX_FBU: u64 = 0x0C;
pub const PX_IS: u64 = 0x10;
pub const PX_IE: u64 = 0x14;
pub const PX_CMD: u64 = 0x18;
pub const PX_TFD: u64 = 0x20;
pub const PX_SIG: u64 = 0x24;
pub const PX_SSTS: u64 = 0x28;
pub const PX_SCTL: u64 = 0x2C;
pub const PX_SERR: u64 = 0x30;
pub const PX_SACT: u64 = 0x34;
pub const PX_CI: u64 = 0x38;
pub const PX_SNTF: u64 = 0x3C;
pub const PX_FBS: u64 = 0x40;

// PxCMD bits.
pub const PXCMD_ST: u32 = 1 << 0;
pub const PXCMD_SUD: u32 = 1 << 1;
pub const PXCMD_FRE: u32 = 1 << 4;
pub const PXCMD_FR: u32 = 1 << 14;
pub const PXCMD_CR: u32 = 1 << 15;

// PxIS / PxIE bits.
pub const PXIS_DHRS: u32 = 1 << 0;
pub const PXIE_DHRE: u32 = 1 << 0;

// PxTFD bits (Status is low byte, Error is high byte).
pub const ATA_SR_BSY: u8 = 1 << 7;
pub const ATA_SR_DRDY: u8 = 1 << 6;
pub const ATA_SR_DSC: u8 = 1 << 4;
pub const ATA_SR_DRQ: u8 = 1 << 3;
pub const ATA_SR_ERR: u8 = 1 << 0;

// SATA signatures.
pub const SATA_SIG_ATA: u32 = 0x0000_0101;
