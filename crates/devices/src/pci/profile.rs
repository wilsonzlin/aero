use super::capabilities::VendorSpecificCapability;
use super::irq_router::{PciIntxRouter, PciIntxRouterConfig};
use super::{
    PciBarDefinition, PciBdf, PciBus, PciConfigSpace, PciDevice, PciInterruptPin, PciPlatform,
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
    VendorSpecific { payload: &'static [u8] },
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

        config.write(0x08, 1, u32::from(self.revision_id));
        config.write(0x09, 1, u32::from(self.class.prog_if));
        config.write(0x0a, 1, u32::from(self.class.sub_class));
        config.write(0x0b, 1, u32::from(self.class.base_class));
        config.write(0x0e, 1, u32::from(self.header_type));

        config.write(0x2c, 2, u32::from(self.subsystem_vendor_id));
        config.write(0x2e, 2, u32::from(self.subsystem_id));

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
            match cap {
                PciCapabilityProfile::VendorSpecific { payload } => {
                    config
                        .add_capability(Box::new(VendorSpecificCapability::new(payload.to_vec())));
                }
            }
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
pub const PCI_DEVICE_ID_INTEL_ICH9_AHCI: u16 = 0x2922;
pub const PCI_DEVICE_ID_INTEL_ICH6_HDA: u16 = 0x2668;
pub const PCI_DEVICE_ID_INTEL_E1000_82540EM: u16 = 0x100e;
pub const PCI_DEVICE_ID_REALTEK_RTL8139: u16 = 0x8139;
pub const PCI_DEVICE_ID_QEMU_NVME: u16 = 0x0010;
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

/// PCI BAR index used for the AHCI ABAR MMIO window on the Intel ICH9 profile.
pub const AHCI_ABAR_BAR_INDEX: u8 = 5;

/// Size in bytes of the AHCI ABAR MMIO window (as a `u32`, for `PciBarDefinition::Mmio32`).
pub const AHCI_ABAR_SIZE_U32: u32 = 0x2000;

/// Size in bytes of the AHCI ABAR MMIO window.
pub const AHCI_ABAR_SIZE: u64 = AHCI_ABAR_SIZE_U32 as u64;

pub const AHCI_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(
    AHCI_ABAR_BAR_INDEX,
    AHCI_ABAR_SIZE,
    false,
)];

pub const NVME_BARS: [PciBarProfile; 1] = [PciBarProfile::mem64(0, 0x4000, false)];

pub const HDA_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(0, 0x4000, false)];

pub const E1000_BARS: [PciBarProfile; 2] = [
    PciBarProfile::mem32(0, 0x20000, false),
    PciBarProfile::io(1, 0x40),
];

pub const RTL8139_BARS: [PciBarProfile; 2] = [
    PciBarProfile::io(0, 0x100),
    PciBarProfile::mem32(1, 0x100, false),
];

pub const VIRTIO_BARS: [PciBarProfile; 1] = [PciBarProfile::mem64(0, 0x4000, false)];

pub const AEROGPU_BARS: [PciBarProfile; 1] = [PciBarProfile::mem32(0, 64 * 1024, false)];

pub const VIRTIO_CAP_COMMON: [u8; 14] = [
    16, 1, 0, 0, 0, 0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
];

pub const VIRTIO_CAP_NOTIFY: [u8; 18] = [
    20, 2, 0, 0, 0, 0, 0x00, 0x10, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x04, 0x00, 0x00, 0x00,
];

pub const VIRTIO_CAP_ISR: [u8; 14] = [
    16, 3, 0, 0, 0, 0, 0x00, 0x20, 0x00, 0x00, 0x20, 0x00, 0x00, 0x00,
];

pub const VIRTIO_CAP_DEVICE: [u8; 14] = [
    16, 4, 0, 0, 0, 0, 0x00, 0x30, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
];

pub const VIRTIO_CAPS: [PciCapabilityProfile; 4] = [
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
    capabilities: &[],
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
    capabilities: &[],
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

/// AeroGPU display controller (canonical Windows device contract).
///
/// Note: this is a PCI identity/profile only. The canonical `aero_machine::Machine` does not yet
/// expose the full AeroGPU WDDM device model; today it provides boot display via `aero_gpu_vga` and
/// uses a separate Bochs/QEMU-compatible VGA PCI stub (currently at `00:0c.0`) so the fixed VBE
/// linear framebuffer (LFB) can be routed through the PCI MMIO window.
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
    capabilities: &VIRTIO_CAPS,
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
    capabilities: &VIRTIO_CAPS,
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
    capabilities: &VIRTIO_CAPS,
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
    capabilities: &VIRTIO_CAPS,
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
    capabilities: &VIRTIO_CAPS,
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
    capabilities: &VIRTIO_CAPS,
};

pub const CANONICAL_IO_DEVICES: &[PciDeviceProfile] = &[
    ISA_PIIX3,
    IDE_PIIX3,
    USB_UHCI_PIIX3,
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
