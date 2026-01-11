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
