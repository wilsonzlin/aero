use super::capabilities::VendorSpecificCapability;
use super::irq_router::{PciIntxRouter, PciIntxRouterConfig};
use super::{
    MsiCapability, MsixCapability, PciBarDefinition, PciBdf, PciBus, PciConfigSpace, PciDevice,
    PciInterruptPin, PciPlatform, PciSubsystemIds,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciClassCode {
    pub base_class: u8,
    pub sub_class: u8,
    pub prog_if: u8,
}

impl PciClassCode {
    pub const fn new(base_class: u8, sub_class: u8, prog_if: u8) -> Self {
        Self {
            base_class,
            sub_class,
            prog_if,
        }
    }

    pub const fn as_u32(self) -> u32 {
        ((self.base_class as u32) << 16) | ((self.sub_class as u32) << 8) | self.prog_if as u32
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciBarKind {
    Io,
    Mem32,
    Mem64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciBarProfile {
    pub index: u8,
    pub kind: PciBarKind,
    pub size: u64,
    pub prefetchable: bool,
}

impl PciBarProfile {
    pub const fn io(index: u8, size: u64) -> Self {
        Self {
            index,
            kind: PciBarKind::Io,
            size,
            prefetchable: false,
        }
    }

    pub const fn mem32(index: u8, size: u64, prefetchable: bool) -> Self {
        Self {
            index,
            kind: PciBarKind::Mem32,
            size,
            prefetchable,
        }
    }

    pub const fn mem64(index: u8, size: u64, prefetchable: bool) -> Self {
        Self {
            index,
            kind: PciBarKind::Mem64,
            size,
            prefetchable,
        }
    }

    pub const fn initial_register_value(self) -> u32 {
        match self.kind {
            PciBarKind::Io => 0x1,
            PciBarKind::Mem32 => {
                if self.prefetchable {
                    0x8
                } else {
                    0
                }
            }
            PciBarKind::Mem64 => {
                let base = 0x4;
                if self.prefetchable {
                    base | 0x8
                } else {
                    base
                }
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PciCapabilityProfile {
    VendorSpecific {
        payload: &'static [u8],
    },
    /// Single-vector MSI capability.
    ///
    /// This is sufficient for simple PCI functions (e.g. AHCI/NVMe controllers) that only need one
    /// interrupt vector.
    Msi {
        is_64bit: bool,
        per_vector_masking: bool,
    },
    /// MSI-X capability.
    Msix {
        table_size: u16,
        table_bar: u8,
        table_offset: u32,
        pba_bar: u8,
        pba_offset: u32,
    },
}

impl PciCapabilityProfile {
    pub fn add_to_config_space(self, config: &mut PciConfigSpace) {
        match self {
            PciCapabilityProfile::VendorSpecific { payload } => {
                config.add_capability(Box::new(VendorSpecificCapability::new(payload.to_vec())));
            }
            PciCapabilityProfile::Msi {
                is_64bit,
                per_vector_masking,
            } => {
                config.add_capability(Box::new(MsiCapability::new_with_config(
                    is_64bit,
                    per_vector_masking,
                )));
            }
            PciCapabilityProfile::Msix {
                table_size,
                table_bar,
                table_offset,
                pba_bar,
                pba_offset,
            } => {
                config.add_capability(Box::new(MsixCapability::new(
                    table_size,
                    table_bar,
                    table_offset,
                    pba_bar,
                    pba_offset,
                )));
            }
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PciDeviceProfile {
    pub name: &'static str,
    pub bdf: PciBdf,
    pub vendor_id: u16,
    pub device_id: u16,
    pub subsystem_vendor_id: u16,
    pub subsystem_id: u16,
    pub revision_id: u8,
    pub class: PciClassCode,
    pub header_type: u8,
    pub interrupt_pin: Option<PciInterruptPin>,
    pub bars: &'static [PciBarProfile],
    pub capabilities: &'static [PciCapabilityProfile],
}

impl PciDeviceProfile {
    pub fn build_config_space(self) -> PciConfigSpace {
        let mut config = PciConfigSpace::new(self.vendor_id, self.device_id);

        config.set_class_code(
            self.class.base_class,
            self.class.sub_class,
            self.class.prog_if,
            self.revision_id,
        );
        config.set_header_type(self.header_type);
        config.set_subsystem_ids(PciSubsystemIds {
            subsystem_vendor_id: self.subsystem_vendor_id,
            subsystem_id: self.subsystem_id,
        });

        for bar in self.bars {
            match bar.kind {
                PciBarKind::Io => config.set_bar_definition(
                    bar.index,
                    PciBarDefinition::Io {
                        size: u32::try_from(bar.size).expect("PCI IO BAR size should fit in u32"),
                    },
                ),
                PciBarKind::Mem32 => config.set_bar_definition(
                    bar.index,
                    PciBarDefinition::Mmio32 {
                        size: u32::try_from(bar.size)
                            .expect("PCI MMIO32 BAR size should fit in u32"),
                        prefetchable: bar.prefetchable,
                    },
                ),
                PciBarKind::Mem64 => config.set_bar_definition(
                    bar.index,
                    PciBarDefinition::Mmio64 {
                        size: bar.size,
                        prefetchable: bar.prefetchable,
                    },
                ),
            }

            let offset = 0x10u16 + u16::from(bar.index) * 4;
            config.write(offset, 4, bar.initial_register_value());
            if bar.kind == PciBarKind::Mem64 {
                config.write(offset + 4, 4, 0);
            }
        }

        for cap in self.capabilities {
            cap.add_to_config_space(&mut config);
        }

        let router = PciIntxRouter::new(PciIntxRouterConfig::default());
        router.configure_device_intx(self.bdf, self.interrupt_pin, &mut config);

        config
    }
}

struct ProfiledPciDevice {
    config: PciConfigSpace,
}

impl ProfiledPciDevice {
    fn new(profile: PciDeviceProfile) -> Self {
        Self {
            config: profile.build_config_space(),
        }
    }
}

impl PciDevice for ProfiledPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

pub const PCI_VENDOR_ID_INTEL: u16 = 0x8086;
pub const PCI_VENDOR_ID_REALTEK: u16 = 0x10ec;
pub const PCI_VENDOR_ID_REDHAT_QEMU: u16 = 0x1b36;
pub const PCI_VENDOR_ID_VIRTIO: u16 = 0x1af4;
/// Project-local PCI vendor ID used by AeroGPU (display controller).
pub const PCI_VENDOR_ID_AERO: u16 = 0xA3A0;

pub const PCI_DEVICE_ID_INTEL_PIIX3_IDE: u16 = 0x7010;
pub const PCI_DEVICE_ID_INTEL_PIIX3_UHCI: u16 = 0x7020;
pub const PCI_DEVICE_ID_INTEL_PIIX3_ISA: u16 = 0x7000;
/// Intel ICH6 EHCI (USB 2.0) controller (commonly supported by Windows in-box drivers).
pub const PCI_DEVICE_ID_INTEL_ICH6_EHCI: u16 = 0x265c;
pub const PCI_DEVICE_ID_INTEL_ICH9_AHCI: u16 = 0x2922;
/// Intel 82801I (ICH9 family) USB2 Enhanced Host Controller (EHCI).
///
/// Windows 7 includes an inbox EHCI driver (`usbehci.sys`) that binds to the EHCI programming
/// interface (class 0x0c0320); using an ICH9 family device ID keeps PCI enumeration consistent with
/// common Q35/ICH9 virtual machines.
///
/// Reference: `lspci` on QEMU `q35` commonly reports `8086:293a` for the first ICH9 EHCI function.
pub const PCI_DEVICE_ID_INTEL_ICH9_EHCI: u16 = 0x293a;
pub const PCI_DEVICE_ID_INTEL_ICH6_HDA: u16 = 0x2668;
pub const PCI_DEVICE_ID_INTEL_E1000_82540EM: u16 = 0x100e;
pub const PCI_DEVICE_ID_REALTEK_RTL8139: u16 = 0x8139;
pub const PCI_DEVICE_ID_QEMU_NVME: u16 = 0x0010;
/// QEMU's "qemu-xhci" PCI device ID (paired with [`PCI_VENDOR_ID_REDHAT_QEMU`]).
///
/// We use QEMU's canonical xHCI PCI identity (`1b36:000d`) so modern guests bind their generic
/// xHCI drivers without requiring vendor-specific packages.
pub const PCI_DEVICE_ID_QEMU_XHCI: u16 = 0x000d;
pub const PCI_DEVICE_ID_AERO_AEROGPU: u16 = 0x0001;

pub const PCI_DEVICE_ID_VIRTIO_NET_TRANSITIONAL: u16 = 0x1000;
pub const PCI_DEVICE_ID_VIRTIO_BLK_TRANSITIONAL: u16 = 0x1001;
pub const PCI_DEVICE_ID_VIRTIO_INPUT_TRANSITIONAL: u16 = 0x1011;
pub const PCI_DEVICE_ID_VIRTIO_SND_TRANSITIONAL: u16 = 0x1018;
pub const PCI_DEVICE_ID_VIRTIO_NET_MODERN: u16 = 0x1041;
pub const PCI_DEVICE_ID_VIRTIO_BLK_MODERN: u16 = 0x1042;
pub const PCI_DEVICE_ID_VIRTIO_INPUT_MODERN: u16 = 0x1052;
pub const PCI_DEVICE_ID_VIRTIO_SND_MODERN: u16 = 0x1059;

pub const IDE_BARS: [PciBarProfile; 5] = [
    PciBarProfile::io(0, 8),
    PciBarProfile::io(1, 4),
    PciBarProfile::io(2, 8),
    PciBarProfile::io(3, 4),
    PciBarProfile::io(4, 16),
];

pub const UHCI_BARS: [PciBarProfile; 1] = [PciBarProfile::io(4, 32)];

/// MMIO BAR window for the canonical EHCI controller.
///
/// EHCI exposes:
/// - capability registers (CAPLENGTH + HCCPARAMS + etc.)
/// - operational registers (USBCMD/USBSTS/...)
/// - per-port registers
///
/// The architectural register set is small (< 0x100 bytes for typical controllers), but we reserve
/// a full 4KiB page so BAR alignment and probing behavior match real hardware.
pub const EHCI_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(0, 0x1000, false)];

/// PCI BAR index used for the xHCI MMIO register window.
pub const XHCI_MMIO_BAR_INDEX: u8 = 0;

/// Size in bytes of the xHCI MMIO register window (BAR0).
///
/// We allocate a 64KiB MMIO BAR, matching the conventional "qemu-xhci" layout and leaving ample
/// room for:
/// - capability registers (CAP),
/// - operational registers (OP),
/// - runtime registers (RT),
/// - doorbell registers (DB),
/// - extended capability structures.
///
/// Keep this in sync with `crate::usb::xhci::XhciPciDevice::MMIO_BAR_SIZE`.
pub const XHCI_MMIO_BAR_SIZE_U32: u32 = 0x1_0000;

/// Size in bytes of the xHCI MMIO register window (BAR0).
pub const XHCI_MMIO_BAR_SIZE: u64 = XHCI_MMIO_BAR_SIZE_U32 as u64;

pub const XHCI_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(
    XHCI_MMIO_BAR_INDEX,
    XHCI_MMIO_BAR_SIZE,
    false,
)];

// -----------------------------------------------------------------------------
// xHCI MSI-X configuration (BAR0-backed MSI-X table + PBA)
// -----------------------------------------------------------------------------
//
// We expose a minimal MSI-X capability for the canonical xHCI controller so modern guests can
// prefer MSI-X over legacy INTx/MSI. The current xHCI model only uses interrupter 0, so one vector
// is sufficient.
//
// Layout:
// - Table: BAR0 + 0x8000, one 16-byte entry (vector 0)
// - PBA:   BAR0 + 0x9000, 8 bytes (bit 0)
pub const XHCI_MSIX_TABLE_SIZE: u16 = 1;
pub const XHCI_MSIX_TABLE_BAR: u8 = XHCI_MMIO_BAR_INDEX;
pub const XHCI_MSIX_TABLE_OFFSET: u32 = 0x8000;
pub const XHCI_MSIX_PBA_BAR: u8 = XHCI_MMIO_BAR_INDEX;
pub const XHCI_MSIX_PBA_OFFSET: u32 = 0x9000;

/// Canonical capabilities exposed by the QEMU-style xHCI profile.
///
/// xHCI uses a single interrupter (vector 0) in the current device model. We expose both MSI and
/// MSI-X so guests can choose their preferred interrupt mechanism.
pub const XHCI_CAPS: [PciCapabilityProfile; 2] = [
    PciCapabilityProfile::Msi {
        is_64bit: true,
        per_vector_masking: true,
    },
    PciCapabilityProfile::Msix {
        table_size: XHCI_MSIX_TABLE_SIZE,
        table_bar: XHCI_MSIX_TABLE_BAR,
        table_offset: XHCI_MSIX_TABLE_OFFSET,
        pba_bar: XHCI_MSIX_PBA_BAR,
        pba_offset: XHCI_MSIX_PBA_OFFSET,
    },
];
/// PCI BAR index used for the AHCI ABAR MMIO window on the Intel ICH9 profile.
pub const AHCI_ABAR_BAR_INDEX: u8 = 5;

/// PCI config space offset of the AHCI ABAR register (BAR5 on Intel ICH9).
pub const AHCI_ABAR_CFG_OFFSET: u8 = 0x10 + AHCI_ABAR_BAR_INDEX * 4;

/// Size in bytes of the AHCI ABAR MMIO window (as a `u32`, for `PciBarDefinition::Mmio32`).
pub const AHCI_ABAR_SIZE_U32: u32 = 0x2000;

/// Size in bytes of the AHCI ABAR MMIO window.
pub const AHCI_ABAR_SIZE: u64 = AHCI_ABAR_SIZE_U32 as u64;

pub const AHCI_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(
    AHCI_ABAR_BAR_INDEX,
    AHCI_ABAR_SIZE,
    false,
)];

/// Canonical capabilities exposed by the Intel ICH9 AHCI profile.
///
/// AHCI uses a single interrupt vector, so a single-vector MSI capability is sufficient.
pub const AHCI_CAPS: [PciCapabilityProfile; 1] = [PciCapabilityProfile::Msi {
    is_64bit: true,
    per_vector_masking: true,
}];

/// PCI BAR index used for the NVMe controller MMIO register window (BAR0).
pub const NVME_BAR0_INDEX: u8 = 0;
/// Size in bytes of the NVMe controller MMIO register window (BAR0).
pub const NVME_BAR0_SIZE: u64 = 0x4000;

pub const NVME_BARS: [PciBarProfile; 1] =
    [PciBarProfile::mem64(NVME_BAR0_INDEX, NVME_BAR0_SIZE, false)];

/// NVMe MSI-X table size (number of vectors) exposed by the canonical NVMe profile.
pub const NVME_MSIX_TABLE_SIZE: u16 = 1;
/// PCI BAR index containing the NVMe MSI-X table.
pub const NVME_MSIX_TABLE_BAR: u8 = NVME_BAR0_INDEX;
/// Byte offset of the NVMe MSI-X table within BAR0.
pub const NVME_MSIX_TABLE_OFFSET: u32 = 0x3000;

const NVME_MSIX_TABLE_ENTRY_SIZE_BYTES: u32 = 16;

const fn nvme_msix_table_bytes(table_size: u16) -> u32 {
    (table_size as u32).saturating_mul(NVME_MSIX_TABLE_ENTRY_SIZE_BYTES)
}

const fn nvme_msix_pba_offset(table_size: u16) -> u32 {
    // Align to 8 bytes as required by MSI-X.
    let end = NVME_MSIX_TABLE_OFFSET.saturating_add(nvme_msix_table_bytes(table_size));
    (end + 7) & !7
}

/// PCI BAR index containing the NVMe MSI-X PBA.
pub const NVME_MSIX_PBA_BAR: u8 = NVME_BAR0_INDEX;
/// Byte offset of the NVMe MSI-X PBA within BAR0.
pub const NVME_MSIX_PBA_OFFSET: u32 = nvme_msix_pba_offset(NVME_MSIX_TABLE_SIZE);

/// Canonical capabilities exposed by the NVMe controller profile.
///
/// Aero's NVMe device model uses a single interrupt vector, so a single-vector MSI capability is
/// sufficient. We also expose a minimal MSI-X table/PBA in BAR0 so modern guests can bind MSI-X
/// capable NVMe drivers and use message-signaled interrupts instead of legacy INTx.
pub const NVME_CAPS: [PciCapabilityProfile; 2] = [
    PciCapabilityProfile::Msi {
        is_64bit: true,
        per_vector_masking: true,
    },
    // MSI-X table/PBA in BAR0:
    // - table: 1 entry at +0x3000
    // - PBA: immediately after the table (8-byte aligned) at +0x3010
    PciCapabilityProfile::Msix {
        table_size: NVME_MSIX_TABLE_SIZE,
        table_bar: NVME_MSIX_TABLE_BAR,
        table_offset: NVME_MSIX_TABLE_OFFSET,
        pba_bar: NVME_MSIX_PBA_BAR,
        pba_offset: NVME_MSIX_PBA_OFFSET,
    },
];

pub const HDA_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(0, 0x4000, false)];

pub const E1000_BARS: [PciBarProfile; 2] = [
    PciBarProfile::mem32(0, 0x20000, false),
    PciBarProfile::io(1, 0x40),
];

pub const RTL8139_BARS: [PciBarProfile; 2] = [
    PciBarProfile::io(0, 0x100),
    PciBarProfile::mem32(1, 0x100, false),
];

/// PCI BAR index used for the virtio-pci MMIO register window (BAR0).
pub const VIRTIO_BAR0_INDEX: u8 = 0;
/// Size in bytes of the virtio-pci MMIO register window (BAR0).
pub const VIRTIO_BAR0_SIZE: u64 = 0x4000;

// NOTE: Keep VIRTIO_BARS in a simple literal array initializer form so CI guardrails can parse it
// deterministically (see scripts/ci/check-windows-virtio-contract.py).
pub const VIRTIO_BARS: [PciBarProfile; 1] = [PciBarProfile::mem64(
    VIRTIO_BAR0_INDEX,
    VIRTIO_BAR0_SIZE,
    false,
)];

/// AeroGPU BAR0 index (MMIO control registers).
pub const AEROGPU_BAR0_INDEX: u8 = 0;
/// AeroGPU BAR1 index (prefetchable VRAM aperture).
pub const AEROGPU_BAR1_VRAM_INDEX: u8 = 1;
/// Size in bytes of the AeroGPU BAR0 register window.
pub const AEROGPU_BAR0_SIZE: u64 = 64 * 1024;
/// Size in bytes of the AeroGPU VRAM aperture exposed via BAR1.
///
/// This window backs the legacy VGA 0xA0000..0xBFFFF alias plus the VBE linear framebuffer and
/// provides headroom for future WDDM-visible VRAM allocations. The canonical PCI profile uses
/// 64MiB, which is large enough for a 32bpp 4K scanout (~32MiB). See
/// `docs/16-aerogpu-vga-vesa-compat.md` for the intended BAR1 layout and the VBE LFB offset rules.
pub const AEROGPU_VRAM_SIZE: u64 = 64 * 1024 * 1024;

pub const AEROGPU_BARS: [PciBarProfile; 2] = [
    // BAR0: 64KiB non-prefetchable MMIO registers.
    PciBarProfile::mem32(AEROGPU_BAR0_INDEX, AEROGPU_BAR0_SIZE, false),
    // BAR1: prefetchable MMIO VRAM aperture.
    PciBarProfile::mem32(AEROGPU_BAR1_VRAM_INDEX, AEROGPU_VRAM_SIZE, true),
];

/// PCI BAR layout for the Bochs/QEMU-compatible VGA "transitional" PCI stub.
///
/// BAR0 exposes the Bochs VBE linear framebuffer (LFB) via a 32-bit MMIO window.
///
/// Note: This is the historical default VRAM/LFB size used by `aero_gpu_vga::VgaDevice`.
pub const VGA_TRANSITIONAL_STUB_BAR0_SIZE: u64 = 16 * 1024 * 1024;
pub const VGA_TRANSITIONAL_STUB_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(
    0,
    VGA_TRANSITIONAL_STUB_BAR0_SIZE,
    false,
)];

/// Virtio BAR0 offset of the "common configuration" structure.
pub const VIRTIO_COMMON_CFG_BAR0_OFFSET: u32 = 0x0000;
/// Virtio BAR0 size of the "common configuration" structure.
pub const VIRTIO_COMMON_CFG_BAR0_SIZE: u32 = 0x0100;

/// Virtio BAR0 offset of the "notify" structure.
pub const VIRTIO_NOTIFY_CFG_BAR0_OFFSET: u32 = 0x1000;
/// Virtio BAR0 size of the "notify" structure.
pub const VIRTIO_NOTIFY_CFG_BAR0_SIZE: u32 = 0x0100;
/// Virtio notify offset multiplier.
pub const VIRTIO_NOTIFY_OFF_MULTIPLIER: u32 = 4;

/// Virtio BAR0 offset of the ISR structure.
pub const VIRTIO_ISR_CFG_BAR0_OFFSET: u32 = 0x2000;
/// Virtio BAR0 size of the ISR structure.
pub const VIRTIO_ISR_CFG_BAR0_SIZE: u32 = 0x0020;

/// Virtio BAR0 offset of the device-specific configuration structure.
pub const VIRTIO_DEVICE_CFG_BAR0_OFFSET: u32 = 0x3000;
/// Virtio BAR0 size of the device-specific configuration structure.
pub const VIRTIO_DEVICE_CFG_BAR0_SIZE: u32 = 0x0100;

// NOTE: Keep these virtio vendor capability payloads in a form that CI guardrails can parse without
// evaluating arbitrary Rust expressions (see scripts/ci/check-windows-virtio-contract.py).
//
// Payload format: virtio_pci_cap minus cap_vndr/cap_next:
//   cap_len, cfg_type, bar, id, padding[2], offset(u32 LE), length(u32 LE), [notify_off_multiplier(u32 LE)]
pub const VIRTIO_CAP_COMMON: [u8; 14] = {
    let off = VIRTIO_COMMON_CFG_BAR0_OFFSET.to_le_bytes();
    let len = VIRTIO_COMMON_CFG_BAR0_SIZE.to_le_bytes();
    [
        16,
        1,
        VIRTIO_BAR0_INDEX,
        0,
        0,
        0,
        off[0],
        off[1],
        off[2],
        off[3],
        len[0],
        len[1],
        len[2],
        len[3],
    ]
};

pub const VIRTIO_CAP_NOTIFY: [u8; 18] = {
    let off = VIRTIO_NOTIFY_CFG_BAR0_OFFSET.to_le_bytes();
    let len = VIRTIO_NOTIFY_CFG_BAR0_SIZE.to_le_bytes();
    let mult = VIRTIO_NOTIFY_OFF_MULTIPLIER.to_le_bytes();
    [
        20,
        2,
        VIRTIO_BAR0_INDEX,
        0,
        0,
        0,
        off[0],
        off[1],
        off[2],
        off[3],
        len[0],
        len[1],
        len[2],
        len[3],
        mult[0],
        mult[1],
        mult[2],
        mult[3],
    ]
};

pub const VIRTIO_CAP_ISR: [u8; 14] = {
    let off = VIRTIO_ISR_CFG_BAR0_OFFSET.to_le_bytes();
    let len = VIRTIO_ISR_CFG_BAR0_SIZE.to_le_bytes();
    [
        16,
        3,
        VIRTIO_BAR0_INDEX,
        0,
        0,
        0,
        off[0],
        off[1],
        off[2],
        off[3],
        len[0],
        len[1],
        len[2],
        len[3],
    ]
};

pub const VIRTIO_CAP_DEVICE: [u8; 14] = {
    let off = VIRTIO_DEVICE_CFG_BAR0_OFFSET.to_le_bytes();
    let len = VIRTIO_DEVICE_CFG_BAR0_SIZE.to_le_bytes();
    [
        16,
        4,
        VIRTIO_BAR0_INDEX,
        0,
        0,
        0,
        off[0],
        off[1],
        off[2],
        off[3],
        len[0],
        len[1],
        len[2],
        len[3],
    ]
};

pub const VIRTIO_VENDOR_CAPS: [PciCapabilityProfile; 4] = [
    PciCapabilityProfile::VendorSpecific {
        payload: &VIRTIO_CAP_COMMON,
    },
    PciCapabilityProfile::VendorSpecific {
        payload: &VIRTIO_CAP_NOTIFY,
    },
    PciCapabilityProfile::VendorSpecific {
        payload: &VIRTIO_CAP_ISR,
    },
    PciCapabilityProfile::VendorSpecific {
        payload: &VIRTIO_CAP_DEVICE,
    },
];

/// BAR0 offset within the virtio-pci MMIO window where the MSI-X table starts.
///
/// This offset is chosen to live immediately after the 0x100-byte device-specific config window
/// (0x3000..=0x30ff) used by the canonical virtio-pci capability payloads above.
pub const VIRTIO_MSIX_TABLE_BAR0_OFFSET: u32 =
    VIRTIO_DEVICE_CFG_BAR0_OFFSET + VIRTIO_DEVICE_CFG_BAR0_SIZE;

const VIRTIO_MSIX_TABLE_ENTRY_SIZE_BYTES: u32 = 16;

const fn virtio_msix_table_bytes(table_size: u16) -> u32 {
    (table_size as u32).saturating_mul(VIRTIO_MSIX_TABLE_ENTRY_SIZE_BYTES)
}

pub const fn virtio_msix_pba_offset(table_size: u16) -> u32 {
    // Align to 8 bytes as required by MSI-X.
    let end = VIRTIO_MSIX_TABLE_BAR0_OFFSET.saturating_add(virtio_msix_table_bytes(table_size));
    (end + 7) & !7
}

const fn virtio_msix_pba_bytes(table_size: u16) -> u32 {
    // One bit per vector, packed into u64 words.
    ((table_size as u32).saturating_add(63) / 64).saturating_mul(8)
}

const fn virtio_msix_end_offset(table_size: u16) -> u32 {
    virtio_msix_pba_offset(table_size).saturating_add(virtio_msix_pba_bytes(table_size))
}

/// Builds the canonical MSI-X capability profile for a virtio-pci device.
///
/// Virtio uses one vector for config change notifications plus one vector per virtqueue.
///
/// # Panics
///
/// Panics if the derived MSI-X table/PBA layout does not fit within the provided BAR0 size.
pub fn virtio_msix_capability_profile(num_queues: usize, bar0_size: u64) -> PciCapabilityProfile {
    let table_size = u16::try_from(num_queues.saturating_add(1)).unwrap_or(u16::MAX);
    let msix_end = u64::from(virtio_msix_end_offset(table_size));
    assert!(
        msix_end <= bar0_size,
        "virtio-pci BAR0 too small for MSI-X table: end=0x{msix_end:x} bar0_size=0x{bar0_size:x}"
    );
    virtio_msix_capability_profile_for_table_size(table_size)
}

pub const fn virtio_msix_capability_profile_for_table_size(
    table_size: u16,
) -> PciCapabilityProfile {
    PciCapabilityProfile::Msix {
        table_size,
        table_bar: VIRTIO_BAR0_INDEX,
        table_offset: VIRTIO_MSIX_TABLE_BAR0_OFFSET,
        pba_bar: VIRTIO_BAR0_INDEX,
        pba_offset: virtio_msix_pba_offset(table_size),
    }
}

pub const VIRTIO_NET_CAPS: [PciCapabilityProfile; 5] = [
    VIRTIO_VENDOR_CAPS[0],
    VIRTIO_VENDOR_CAPS[1],
    VIRTIO_VENDOR_CAPS[2],
    VIRTIO_VENDOR_CAPS[3],
    // virtio-net has 2 virtqueues (RX/TX) + 1 config vector.
    virtio_msix_capability_profile_for_table_size(3),
];

pub const VIRTIO_BLK_CAPS: [PciCapabilityProfile; 5] = [
    VIRTIO_VENDOR_CAPS[0],
    VIRTIO_VENDOR_CAPS[1],
    VIRTIO_VENDOR_CAPS[2],
    VIRTIO_VENDOR_CAPS[3],
    // virtio-blk has 1 virtqueue + 1 config vector.
    virtio_msix_capability_profile_for_table_size(2),
];

pub const VIRTIO_INPUT_CAPS: [PciCapabilityProfile; 5] = [
    VIRTIO_VENDOR_CAPS[0],
    VIRTIO_VENDOR_CAPS[1],
    VIRTIO_VENDOR_CAPS[2],
    VIRTIO_VENDOR_CAPS[3],
    // virtio-input has 2 virtqueues (event/status) + 1 config vector.
    virtio_msix_capability_profile_for_table_size(3),
];

pub const VIRTIO_SND_CAPS: [PciCapabilityProfile; 5] = [
    VIRTIO_VENDOR_CAPS[0],
    VIRTIO_VENDOR_CAPS[1],
    VIRTIO_VENDOR_CAPS[2],
    VIRTIO_VENDOR_CAPS[3],
    // virtio-snd has 4 virtqueues + 1 config vector.
    virtio_msix_capability_profile_for_table_size(5),
];

pub const IDE_PIIX3: PciDeviceProfile = PciDeviceProfile {
    name: "piix3-ide",
    bdf: PciBdf::new(0, 1, 1),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_PIIX3_IDE,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    // PIIX3 uses a programming interface of 0x8A:
    // - bus master DMA present (bit 7)
    // - both channels in legacy-compat mode but programmable (bits 1 and 3)
    class: PciClassCode::new(0x01, 0x01, 0x8a),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &IDE_BARS,
    capabilities: &[],
};

pub const ISA_PIIX3: PciDeviceProfile = PciDeviceProfile {
    name: "piix3-isa",
    bdf: PciBdf::new(0, 1, 0),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_PIIX3_ISA,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    class: PciClassCode::new(0x06, 0x01, 0x00),
    // Mark as multi-function so OS enumeration discovers the IDE/UHCI functions.
    header_type: 0x80,
    interrupt_pin: None,
    bars: &[],
    capabilities: &[],
};

pub const USB_UHCI_PIIX3: PciDeviceProfile = PciDeviceProfile {
    name: "piix3-uhci",
    bdf: PciBdf::new(0, 1, 2),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_PIIX3_UHCI,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    class: PciClassCode::new(0x0c, 0x03, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &UHCI_BARS,
    capabilities: &[],
};

pub const USB_EHCI_ICH9: PciDeviceProfile = PciDeviceProfile {
    name: "ich9-ehci",
    // Use a dedicated device number so it does not collide with other canonical functions and can
    // be moved independently of the PIIX3 multifunction slot used for legacy UHCI.
    bdf: PciBdf::new(0, 0x12, 0),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_ICH9_EHCI,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    // Serial bus / USB / EHCI.
    class: PciClassCode::new(0x0c, 0x03, 0x20),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &EHCI_BARS,
    capabilities: &[],
};

/// PCI identity/profile for an xHCI (USB 3.x) controller.
///
/// Identity choice:
/// - **VID/DID**: `1b36:000d` ("qemu-xhci"), a widely-recognized QEMU virtual device ID.
/// - **Class code**: `0x0c0330` (serial bus / USB / xHCI).
///
/// Most modern guest OSes (Linux, Windows 8+) bind their generic xHCI driver primarily based on
/// the class code, not a specific vendor/device ID, so using QEMU's ID is a safe and
/// well-understood default. (Windows 7 has no inbox xHCI driver; guests that need USB 3.x support
/// must provide a driver.)
pub const USB_XHCI_QEMU: PciDeviceProfile = PciDeviceProfile {
    name: "qemu-xhci",
    // Stable canonical BDF chosen to avoid conflicts with chipset functions (00:01.x) and other
    // canonical PCI functions. Keep this stable so PCI config snapshots restore deterministically.
    bdf: PciBdf::new(0, 0x0d, 0),
    vendor_id: PCI_VENDOR_ID_REDHAT_QEMU,
    device_id: PCI_DEVICE_ID_QEMU_XHCI,
    subsystem_vendor_id: PCI_VENDOR_ID_REDHAT_QEMU,
    subsystem_id: PCI_DEVICE_ID_QEMU_XHCI,
    revision_id: 0x01,
    class: PciClassCode::new(0x0c, 0x03, 0x30),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &XHCI_BARS,
    capabilities: &XHCI_CAPS,
};

pub const SATA_AHCI_ICH9: PciDeviceProfile = PciDeviceProfile {
    name: "ich9-ahci",
    bdf: PciBdf::new(0, 2, 0),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_ICH9_AHCI,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    class: PciClassCode::new(0x01, 0x06, 0x01),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &AHCI_BARS,
    capabilities: &AHCI_CAPS,
};

pub const NVME_CONTROLLER: PciDeviceProfile = PciDeviceProfile {
    name: "nvme",
    bdf: PciBdf::new(0, 3, 0),
    vendor_id: PCI_VENDOR_ID_REDHAT_QEMU,
    device_id: PCI_DEVICE_ID_QEMU_NVME,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    class: PciClassCode::new(0x01, 0x08, 0x02),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &NVME_BARS,
    capabilities: &NVME_CAPS,
};

pub const HDA_ICH6: PciDeviceProfile = PciDeviceProfile {
    name: "ich6-hda",
    bdf: PciBdf::new(0, 4, 0),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_ICH6_HDA,
    subsystem_vendor_id: PCI_VENDOR_ID_INTEL,
    subsystem_id: PCI_DEVICE_ID_INTEL_ICH6_HDA,
    revision_id: 1,
    class: PciClassCode::new(0x04, 0x03, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &HDA_BARS,
    capabilities: &[],
};

pub const NIC_E1000_82540EM: PciDeviceProfile = PciDeviceProfile {
    name: "e1000-82540em",
    bdf: PciBdf::new(0, 5, 0),
    vendor_id: PCI_VENDOR_ID_INTEL,
    device_id: PCI_DEVICE_ID_INTEL_E1000_82540EM,
    subsystem_vendor_id: PCI_VENDOR_ID_INTEL,
    subsystem_id: PCI_DEVICE_ID_INTEL_E1000_82540EM,
    revision_id: 0,
    class: PciClassCode::new(0x02, 0x00, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &E1000_BARS,
    capabilities: &[],
};

pub const NIC_RTL8139: PciDeviceProfile = PciDeviceProfile {
    name: "rtl8139",
    bdf: PciBdf::new(0, 6, 0),
    vendor_id: PCI_VENDOR_ID_REALTEK,
    device_id: PCI_DEVICE_ID_REALTEK_RTL8139,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    class: PciClassCode::new(0x02, 0x00, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &RTL8139_BARS,
    capabilities: &[],
};

/// Bochs/QEMU-compatible VGA PCI identity used as a transitional compatibility shim.
///
/// This profile represents the historical Bochs/QEMU "Standard VGA" PCI stub (`1234:1111`) that
/// some firmware/OS implementations use to discover a VGA-compatible controller and map the Bochs
/// VBE linear framebuffer (LFB) via PCI BAR0.
///
/// Note: The canonical `aero_machine::Machine` exposes this device only when the standalone legacy
/// VGA/VBE boot display path is enabled (`MachineConfig::enable_vga=true`, `enable_aerogpu=false`)
/// and the PC platform is enabled (`enable_pc_platform=true`). In that configuration, the VBE LFB
/// is routed through the stub's BAR0 inside the PCI MMIO window (BAR base assigned by BIOS POST /
/// the PCI allocator, and may be relocated when other PCI devices are present).
///
/// This stub is intentionally absent when AeroGPU is enabled to avoid exposing two VGA-class PCI
/// display controllers to the guest (which can confuse Windows driver binding).
///
/// It is *not* AeroGPU. The canonical AeroGPU contract is [`AEROGPU`].
pub const VGA_TRANSITIONAL_STUB: PciDeviceProfile = PciDeviceProfile {
    name: "vga-transitional-stub",
    // Historical fixed BDF used by Aero's transitional VGA PCI stub (`00:0c.0`).
    //
    // The canonical `aero_machine::Machine` exposes this PCI function only in `enable_vga=true` +
    // `enable_pc_platform=true` mode (see above). When `enable_aerogpu=true`, the stub is absent.
    bdf: PciBdf::new(0, 0x0c, 0),
    // Bochs/QEMU "Standard VGA" IDs.
    vendor_id: 0x1234,
    device_id: 0x1111,
    subsystem_vendor_id: 0,
    subsystem_id: 0,
    revision_id: 0,
    // VGA-compatible display controller.
    class: PciClassCode::new(0x03, 0x00, 0x00),
    header_type: 0x00,
    interrupt_pin: None,
    bars: &VGA_TRANSITIONAL_STUB_BARS,
    capabilities: &[],
};

/// AeroGPU display controller (canonical Windows device contract).
///
/// Note: this is the canonical PCI identity/profile definition (IDs/class/BAR layout).
///
/// In the canonical `aero_machine::Machine`, `MachineConfig::enable_aerogpu=true` wires an MVP
/// device model behind this identity:
/// - BAR0: AeroGPU MMIO registers (ring + doorbell + fence + scanout/cursor register surface)
/// - BAR1: a host-backed VRAM aperture:
///   - the legacy VGA window aliases into `VRAM[0..0x20000)` (128KiB)
///   - the first 256KiB is reserved for legacy VGA planar storage (4 × 64KiB planes)
///   - the VBE linear framebuffer begins at `VRAM[0x40000..]` (`PhysBasePtr = BAR1_BASE + 0x40000`,
///     `AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES`)
///
/// Boot display in the canonical machine can still be provided by the standalone `aero_gpu_vga`
/// VGA/VBE device model when `MachineConfig::enable_vga=true` (and `enable_aerogpu=false`).
/// The VBE linear framebuffer (LFB) is exposed at the configured LFB base (historically defaulting
/// to `aero_gpu_vga::SVGA_LFB_BASE` / `0xE000_0000`).
///
/// When `enable_pc_platform=true`, the canonical machine exposes the Bochs/QEMU-compatible
/// “Standard VGA” PCI stub [`VGA_TRANSITIONAL_STUB`] and routes the LFB through its BAR0 inside the
/// PCI MMIO window (BAR base assigned by BIOS POST / the PCI allocator, and may be relocated when
/// other PCI devices are present). When `enable_pc_platform=false`, the LFB is mapped directly as a
/// fixed MMIO window at the configured physical address.
pub const AEROGPU: PciDeviceProfile = PciDeviceProfile {
    name: "aerogpu",
    bdf: PciBdf::new(0, 7, 0),
    vendor_id: PCI_VENDOR_ID_AERO,
    device_id: PCI_DEVICE_ID_AERO_AEROGPU,
    subsystem_vendor_id: PCI_VENDOR_ID_AERO,
    subsystem_id: 0x0001,
    revision_id: 0,
    // VGA-compatible display controller.
    class: PciClassCode::new(0x03, 0x00, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &AEROGPU_BARS,
    capabilities: &[],
};

pub const VIRTIO_NET: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-net",
    bdf: PciBdf::new(0, 8, 0),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_NET_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 1,
    revision_id: 1,
    class: PciClassCode::new(0x02, 0x00, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_NET_CAPS,
};

pub const VIRTIO_BLK: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-blk",
    bdf: PciBdf::new(0, 9, 0),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_BLK_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 2,
    revision_id: 1,
    class: PciClassCode::new(0x01, 0x00, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_BLK_CAPS,
};

pub const VIRTIO_INPUT_KEYBOARD: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-input-keyboard",
    bdf: PciBdf::new(0, 10, 0),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_INPUT_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 0x0010,
    revision_id: 1,
    class: PciClassCode::new(0x09, 0x80, 0x00),
    header_type: 0x80,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_INPUT_CAPS,
};

pub const VIRTIO_INPUT: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-input (deprecated: use VIRTIO_INPUT_KEYBOARD/VIRTIO_INPUT_MOUSE)",
    bdf: PciBdf::new(0, 10, 0),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_INPUT_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 0x0010,
    revision_id: 1,
    class: PciClassCode::new(0x09, 0x80, 0x00),
    header_type: 0x80,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_INPUT_CAPS,
};

pub const VIRTIO_INPUT_MOUSE: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-input-mouse",
    bdf: PciBdf::new(0, 10, 1),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_INPUT_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 0x0011,
    revision_id: 1,
    class: PciClassCode::new(0x09, 0x80, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_INPUT_CAPS,
};

/// Optional virtio-input absolute pointer function (tablet).
///
/// See `docs/windows7-virtio-driver-contract.md` §3.3: function 2 is an optional EV_ABS tablet that
/// shares the same Vendor/Device ID as the keyboard/mouse functions but uses subsystem ID 0x0012.
pub const VIRTIO_INPUT_TABLET: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-input-tablet",
    bdf: PciBdf::new(0, 10, 2),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_INPUT_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 0x0012,
    revision_id: 1,
    class: PciClassCode::new(0x09, 0x80, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_INPUT_CAPS,
};

pub const VIRTIO_SND: PciDeviceProfile = PciDeviceProfile {
    name: "virtio-snd",
    bdf: PciBdf::new(0, 11, 0),
    vendor_id: PCI_VENDOR_ID_VIRTIO,
    device_id: PCI_DEVICE_ID_VIRTIO_SND_MODERN,
    subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
    subsystem_id: 25,
    revision_id: 1,
    // Report as a generic multimedia audio device rather than HDA (0x0403), since the
    // virtio-snd programming model is not compatible with Intel HD Audio drivers.
    class: PciClassCode::new(0x04, 0x01, 0x00),
    header_type: 0x00,
    interrupt_pin: Some(PciInterruptPin::IntA),
    bars: &VIRTIO_BARS,
    capabilities: &VIRTIO_SND_CAPS,
};

pub const CANONICAL_IO_DEVICES: &[PciDeviceProfile] = &[
    ISA_PIIX3,
    IDE_PIIX3,
    USB_UHCI_PIIX3,
    USB_EHCI_ICH9,
    SATA_AHCI_ICH9,
    NVME_CONTROLLER,
    HDA_ICH6,
    NIC_E1000_82540EM,
    NIC_RTL8139,
    AEROGPU,
    VIRTIO_NET,
    VIRTIO_BLK,
    VIRTIO_INPUT_KEYBOARD,
    VIRTIO_INPUT_MOUSE,
    VIRTIO_SND,
];

/// Builds a PCI bus containing the minimal chipset devices (host bridge + ISA/LPC bridge) plus the
/// canonical IO device set used by Aero's Windows driver contract.
pub fn build_canonical_io_bus() -> PciBus {
    let mut bus = PciPlatform::build_bus();
    for profile in CANONICAL_IO_DEVICES {
        if profile.bdf == USB_UHCI_PIIX3.bdf {
            bus.add_device(
                profile.bdf,
                Box::new(crate::usb::uhci::UhciPciDevice::default()),
            );
        } else if profile.bdf == USB_EHCI_ICH9.bdf {
            bus.add_device(
                profile.bdf,
                Box::new(crate::usb::ehci::EhciPciDevice::default()),
            );
        } else {
            bus.add_device(profile.bdf, Box::new(ProfiledPciDevice::new(*profile)));
        }
    }
    bus
}

pub fn pci_dump(profiles: &[PciDeviceProfile]) -> String {
    let mut rows: Vec<PciDeviceProfile> = profiles.to_vec();
    rows.sort_by_key(|p| (p.bdf.bus, p.bdf.device, p.bdf.function));

    let router = PciIntxRouter::new(PciIntxRouterConfig::default());
    let mut out = String::new();
    for p in rows {
        let (pin, line) = match p.interrupt_pin {
            Some(pin) => (
                pin.to_config_u8(),
                u8::try_from(router.gsi_for_intx(p.bdf, pin)).unwrap_or(0xff),
            ),
            None => (0, 0xff),
        };

        use core::fmt::Write as _;
        let _ = writeln!(
            out,
            "{:02x}:{:02x}.{} {:04x}:{:04x} class {:06x} intpin {} intline {:02x}",
            p.bdf.bus,
            p.bdf.device,
            p.bdf.function,
            p.vendor_id,
            p.device_id,
            p.class.as_u32(),
            pin,
            line
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pci::msi::PCI_CAP_ID_MSI;
    use crate::pci::msix::PCI_CAP_ID_MSIX;

    const TEST_CAPABILITIES: [PciCapabilityProfile; 2] = [
        PciCapabilityProfile::Msi {
            is_64bit: false,
            per_vector_masking: false,
        },
        PciCapabilityProfile::Msix {
            table_size: 4,
            table_bar: 2,
            table_offset: 0x1230,
            pba_bar: 2,
            pba_offset: 0x1300,
        },
    ];

    #[test]
    fn build_config_space_adds_msi_and_msix_capabilities_from_profile() {
        let profile = PciDeviceProfile {
            name: "test",
            bdf: PciBdf::new(0, 0, 0),
            vendor_id: 0x1234,
            device_id: 0x5678,
            subsystem_vendor_id: 0,
            subsystem_id: 0,
            revision_id: 0,
            class: PciClassCode::new(0, 0, 0),
            header_type: 0x00,
            interrupt_pin: None,
            bars: &[],
            capabilities: &TEST_CAPABILITIES,
        };

        let mut cfg = profile.build_config_space();

        let caps = cfg.capability_list();
        assert_eq!(caps.len(), 2);
        assert_eq!(caps[0].id, PCI_CAP_ID_MSI);
        assert_eq!(caps[0].offset, 0x40);
        assert_eq!(caps[1].id, PCI_CAP_ID_MSIX);
        assert_eq!(caps[1].offset, 0x4c);

        let msi_off = cfg.find_capability(PCI_CAP_ID_MSI).unwrap() as u16;
        let msix_off = cfg.find_capability(PCI_CAP_ID_MSIX).unwrap() as u16;

        // MSI capability bytes match the profile fields (32-bit, no per-vector masking).
        assert_eq!(cfg.read(msi_off, 1) as u8, PCI_CAP_ID_MSI);
        assert_eq!(cfg.read(msi_off + 1, 1) as u8, msix_off as u8);
        let msi_ctrl = cfg.read(msi_off + 0x02, 2) as u16;
        assert_eq!(msi_ctrl & 0x0001, 0, "MSI should start disabled");
        assert_eq!(
            msi_ctrl & (1 << 7),
            0,
            "MSI 64-bit flag should match profile"
        );
        assert_eq!(
            msi_ctrl & (1 << 8),
            0,
            "MSI per-vector masking flag should match profile"
        );

        // MSI-X capability bytes match the profile fields.
        assert_eq!(cfg.read(msix_off, 1) as u8, PCI_CAP_ID_MSIX);
        assert_eq!(cfg.read(msix_off + 1, 1) as u8, 0);

        let msix_ctrl = cfg.read(msix_off + 0x02, 2) as u16;
        // Table size is encoded as N-1 in bits 0..=10.
        assert_eq!(msix_ctrl & 0x07ff, 3);

        let table = cfg.read(msix_off + 0x04, 4);
        assert_eq!(table, 0x1230 | 2);
        let pba = cfg.read(msix_off + 0x08, 4);
        assert_eq!(pba, 0x1300 | 2);
    }

    #[test]
    fn ehci_profile_class_and_bar_layout() {
        assert_eq!(
            USB_EHCI_ICH9.class.as_u32(),
            0x0c0320,
            "EHCI class code must be 0x0c0320"
        );
        assert_eq!(USB_EHCI_ICH9.bars, &EHCI_BARS);
        assert_eq!(EHCI_BARS.len(), 1);
        assert_eq!(EHCI_BARS[0].index, 0);
        assert_eq!(EHCI_BARS[0].kind, PciBarKind::Mem32);
        assert_eq!(EHCI_BARS[0].size, 0x1000);
        assert!(!EHCI_BARS[0].prefetchable);
    }
}
