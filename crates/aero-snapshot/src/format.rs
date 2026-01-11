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
}
