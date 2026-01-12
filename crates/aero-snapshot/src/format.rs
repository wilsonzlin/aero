pub const SNAPSHOT_MAGIC: &[u8; 8] = b"AEROSNAP";
pub const SNAPSHOT_VERSION_V1: u16 = 1;
pub const SNAPSHOT_ENDIANNESS_LITTLE: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SectionId(pub u32);

impl SectionId {
    pub const META: SectionId = SectionId(1);
    pub const CPU: SectionId = SectionId(2);
    pub const MMU: SectionId = SectionId(3);
    pub const DEVICES: SectionId = SectionId(4);
    pub const DISKS: SectionId = SectionId(5);
    pub const RAM: SectionId = SectionId(6);
    /// Multi-vCPU CPU state. Newer snapshots may use this instead of `CPU`.
    pub const CPUS: SectionId = SectionId(7);

    pub fn name(self) -> Option<&'static str> {
        match self {
            SectionId::META => Some("META"),
            SectionId::CPU => Some("CPU"),
            SectionId::MMU => Some("MMU"),
            SectionId::DEVICES => Some("DEVICES"),
            SectionId::DISKS => Some("DISKS"),
            SectionId::RAM => Some("RAM"),
            SectionId::CPUS => Some("CPUS"),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct DeviceId(pub u32);

impl DeviceId {
    pub const PIC: DeviceId = DeviceId(1);
    pub const APIC: DeviceId = DeviceId(2);
    pub const PIT: DeviceId = DeviceId(3);
    pub const RTC: DeviceId = DeviceId(4);
    /// PCI core device state.
    ///
    /// Snapshot adapters typically store PCI core state as a **single** [`DeviceId::PCI`] entry
    /// whose payload is the `aero-io-snapshot` blob produced by
    /// `aero_devices::pci::PciCoreSnapshot` (inner 4CC `PCIC`).
    ///
    /// The wrapper nests both:
    /// - PCI config ports + config-space/BAR state (`aero_devices::pci::PciConfigPorts`, inner `PCPT`)
    /// - PCI INTx routing + asserted levels (`aero_devices::pci::PciIntxRouter`, inner `INTX`)
    ///
    /// This avoids `aero-snapshot`'s `DEVICES` uniqueness constraint on `(DeviceId, version, flags)`
    /// while still capturing both sub-snapshots. See [`DeviceId::PCI_CFG`] and
    /// [`DeviceId::PCI_INTX`] for the optional split-out entries.
    pub const PCI: DeviceId = DeviceId(5);
    pub const DISK_CONTROLLER: DeviceId = DeviceId(6);
    pub const VGA: DeviceId = DeviceId(7);
    pub const SERIAL: DeviceId = DeviceId(8);
    /// Non-architectural CPU state (e.g. pending interrupts) that isn't part of the `CPU` section.
    pub const CPU_INTERNAL: DeviceId = DeviceId(9);
    /// Firmware/BIOS state for minimal VM configurations.
    pub const BIOS: DeviceId = DeviceId(10);
    /// Memory bus/host glue state (A20 gate, ROM ranges, etc.).
    pub const MEMORY: DeviceId = DeviceId(11);
    /// Guest-visible USB controller/runtime state.
    pub const USB: DeviceId = DeviceId(12);
    /// i8042 / PS/2 controller state (keyboard + mouse).
    pub const I8042: DeviceId = DeviceId(13);
    /// Optional split-out PCI config mechanism #1 ports (`0xCF8/0xCFC`) and PCI bus config-space
    /// state.
    ///
    /// Prefer [`DeviceId::PCI`] for new snapshot adapters. This ID exists for backward
    /// compatibility with older snapshots that stored PCI state as multiple device entries.
    pub const PCI_CFG: DeviceId = DeviceId(14);
    /// Optional split-out PCI INTx (INTA#-INTD#) routing state (asserted levels/refcounts).
    ///
    /// Prefer [`DeviceId::PCI`] for new snapshot adapters. This ID exists for backward
    /// compatibility with older snapshots that stored PCI state as multiple device entries.
    pub const PCI_INTX: DeviceId = DeviceId(15);
    /// ACPI fixed-feature power management I/O state (PM1/GPE/SMI_CMD).
    pub const ACPI_PM: DeviceId = DeviceId(16);
    /// High Precision Event Timer (HPET) state.
    pub const HPET: DeviceId = DeviceId(17);
    /// Guest-visible HD Audio (HDA) controller/runtime state.
    pub const HDA: DeviceId = DeviceId(18);
    /// Intel E1000 (82540EM-ish) NIC model state.
    pub const E1000: DeviceId = DeviceId(19);
    /// User-space network stack/backend state (TCP/IP, DHCP, NAT, proxy bookkeeping).
    pub const NET_STACK: DeviceId = DeviceId(20);
    /// Platform interrupt controller/routing state (`PlatformInterrupts`: PIC + LAPIC/IOAPIC +
    /// IMCR routing).
    pub const PLATFORM_INTERRUPTS: DeviceId = DeviceId(21);

    pub fn name(self) -> Option<&'static str> {
        match self {
            DeviceId::PIC => Some("PIC"),
            DeviceId::APIC => Some("APIC"),
            DeviceId::PIT => Some("PIT"),
            DeviceId::RTC => Some("RTC"),
            DeviceId::PCI => Some("PCI"),
            DeviceId::DISK_CONTROLLER => Some("DISK_CONTROLLER"),
            DeviceId::VGA => Some("VGA"),
            DeviceId::SERIAL => Some("SERIAL"),
            DeviceId::CPU_INTERNAL => Some("CPU_INTERNAL"),
            DeviceId::BIOS => Some("BIOS"),
            DeviceId::MEMORY => Some("MEMORY"),
            DeviceId::USB => Some("USB"),
            DeviceId::I8042 => Some("I8042"),
            DeviceId::PCI_CFG => Some("PCI_CFG"),
            DeviceId::PCI_INTX => Some("PCI_INTX"),
            DeviceId::ACPI_PM => Some("ACPI_PM"),
            DeviceId::HPET => Some("HPET"),
            DeviceId::HDA => Some("HDA"),
            DeviceId::E1000 => Some("E1000"),
            DeviceId::NET_STACK => Some("NET_STACK"),
            DeviceId::PLATFORM_INTERRUPTS => Some("PLATFORM_INTERRUPTS"),
            _ => None,
        }
    }
}

impl core::fmt::Display for SectionId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(name) = self.name() {
            write!(f, "{name}({})", self.0)
        } else {
            write!(f, "SectionId({})", self.0)
        }
    }
}

impl core::fmt::Display for DeviceId {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        if let Some(name) = self.name() {
            write!(f, "{name}({})", self.0)
        } else {
            write!(f, "DeviceId({})", self.0)
        }
    }
}
