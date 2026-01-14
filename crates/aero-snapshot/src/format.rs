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
    /// Multi-vCPU MMU state. Newer snapshots may use this instead of `MMU` when multiple CPUs are
    /// present.
    pub const MMUS: SectionId = SectionId(8);

    pub fn name(self) -> Option<&'static str> {
        match self {
            SectionId::META => Some("META"),
            SectionId::CPU => Some("CPU"),
            SectionId::MMU => Some("MMU"),
            SectionId::DEVICES => Some("DEVICES"),
            SectionId::DISKS => Some("DISKS"),
            SectionId::RAM => Some("RAM"),
            SectionId::CPUS => Some("CPUS"),
            SectionId::MMUS => Some("MMUS"),
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
    /// PCI core device state (legacy/compat).
    ///
    /// Canonical full-system snapshots store PCI core state as **split** `DEVICES` entries using
    /// distinct outer IDs:
    /// - [`DeviceId::PCI_CFG`] for `PciConfigPorts` (`PCPT`)
    /// - [`DeviceId::PCI_INTX_ROUTER`] for `PciIntxRouter` (`INTX`)
    ///
    /// This avoids `aero-snapshot`'s `DEVICES` uniqueness constraint on `(DeviceId, version, flags)`
    /// because both `PCPT` and `INTX` are currently `SnapshotVersion (1.0)`.
    ///
    /// Older snapshots may store PCI core state under this historical ID, either as:
    /// - a combined `PciCoreSnapshot` wrapper (`PCIC`) containing both `PCPT` + `INTX`, or
    /// - a single `PCPT` or `INTX` payload.
    pub const PCI: DeviceId = DeviceId(5);
    /// Storage controller device state (AHCI / virtio-blk / NVMe / ...).
    ///
    /// Snapshot adapters should store disk controller state as a **single** [`DeviceId::DISK_CONTROLLER`]
    /// entry whose payload is an `aero-io-snapshot` TLV blob with inner 4CC `DSKC`.
    ///
    /// The wrapper can contain multiple nested controller io-snapshots keyed by a packed PCI BDF
    /// (`u16`) in the standard PCI config-address layout:
    /// `(bus << 8) | (device << 3) | function`.
    ///
    /// For deterministic encoding, nested controller entries should be written in ascending BDF
    /// order.
    ///
    /// Example nested controllers include `AHCP` (AHCI PCI), `VPCI` (virtio-pci), and `NVMP` (NVMe
    /// PCI).
    ///
    /// This wrapper exists to avoid `aero-snapshot`'s `DEVICES` uniqueness constraint on
    /// `(DeviceId, version, flags)` when multiple controllers are present. By convention,
    /// `DeviceState.version/flags` mirror the inner `aero-io-snapshot` device `SnapshotVersion
    /// (major, minor)`, and many storage controllers start at `SnapshotVersion (1.0)` (e.g. AHCI
    /// `AHCP` and virtio-pci `VPCI`). Storing those as two separate outer `DeviceId::DISK_CONTROLLER`
    /// entries would collide as `(DeviceId::DISK_CONTROLLER, 1, 0)`.
    ///
    /// Restore code should ignore unknown/extra controller entries if the target machine lacks that
    /// controller.
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
    /// PCI config mechanism #1 ports (`0xCF8/0xCFC`) and PCI bus config-space state.
    ///
    /// Canonical full-system snapshots store PCI config-state using this ID (inner `PCPT`).
    ///
    /// Using [`DeviceId::PCI_CFG`] (instead of the historical [`DeviceId::PCI`]) avoids collisions
    /// with the separate [`DeviceId::PCI_INTX_ROUTER`] entry.
    pub const PCI_CFG: DeviceId = DeviceId(14);
    /// PCI INTx (INTA#-INTD#) routing state (asserted levels/refcounts).
    ///
    /// Canonical full-system snapshots store PCI INTx routing using this ID (inner `INTX`).
    ///
    /// Using [`DeviceId::PCI_INTX_ROUTER`] (instead of the historical [`DeviceId::PCI`]) avoids
    /// collisions with the separate [`DeviceId::PCI_CFG`] entry.
    pub const PCI_INTX_ROUTER: DeviceId = DeviceId(15);
    /// Backward compatible alias for [`DeviceId::PCI_INTX_ROUTER`].
    pub const PCI_INTX: DeviceId = DeviceId::PCI_INTX_ROUTER;
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
    /// Guest-visible virtio-snd (virtio-pci) audio device state.
    ///
    /// This ID is used by the worker-based web runtime when virtio-snd is present as either:
    /// - the active audio device (in WASM builds that omit HDA), or
    /// - an additional enumerated PCI function alongside HDA.
    ///
    /// See `docs/16-snapshots.md` and `web/src/workers/vm_snapshot_wasm.ts` for the stable mapping
    /// to the string kind `audio.virtio_snd`.
    pub const VIRTIO_SND: DeviceId = DeviceId(22);
    /// Guest-visible virtio-net (virtio-pci) NIC device state.
    ///
    /// This ID is used by the canonical `aero_machine::Machine` when `enable_virtio_net` is set so
    /// virtio-pci transport state (virtqueues + pending interrupts) can be snapshotted/restored
    /// deterministically.
    pub const VIRTIO_NET: DeviceId = DeviceId(23);
    /// Guest-visible virtio-input (virtio-pci) multi-function device state (keyboard + mouse).
    ///
    /// Canonical snapshots historically stored both PCI functions under this single outer ID as an
    /// `aero-io-snapshot` TLV wrapper (`VINP`) containing two nested virtio-pci (`VPCI`) snapshots:
    /// one for the keyboard function (`00:0A.0`) and one for the mouse function (`00:0A.1`).
    ///
    /// Newer snapshots may store the functions separately under [`DeviceId::VIRTIO_INPUT_KEYBOARD`]
    /// and [`DeviceId::VIRTIO_INPUT_MOUSE`].
    pub const VIRTIO_INPUT: DeviceId = DeviceId(24);
    /// AeroGPU PCI device state (BAR0 regs + VRAM + scanout handoff latch).
    ///
    /// See `docs/16-snapshots.md` and `web/src/workers/vm_snapshot_wasm.ts` for the stable mapping
    /// to the string kind `gpu.aerogpu`.
    pub const AEROGPU: DeviceId = DeviceId(25);
    /// Guest-visible virtio-input (virtio-pci) keyboard function state (PCI `00:0A.0`).
    pub const VIRTIO_INPUT_KEYBOARD: DeviceId = DeviceId(26);
    /// Guest-visible virtio-input (virtio-pci) mouse function state (PCI `00:0A.1`).
    pub const VIRTIO_INPUT_MOUSE: DeviceId = DeviceId(27);
    /// Guest-visible GPU VRAM / BAR1 contents (web runtime).
    ///
    /// The worker-based web runtime may allocate a `SharedArrayBuffer` backing store for the GPU's
    /// BAR1 scanout/VRAM window. Since this memory is guest-visible, it must be included in VM
    /// snapshots to ensure deterministic restore (and to avoid restoring older snapshots with
    /// stale VRAM contents from a later run).
    ///
    /// See `docs/16-snapshots.md` and `web/src/workers/vm_snapshot_wasm.ts` for the stable mapping
    /// to the string kind `gpu.vram`.
    pub const GPU_VRAM: DeviceId = DeviceId(28);
    /// Guest-visible virtio-input (virtio-pci) tablet function state (PCI `00:0A.2`).
    pub const VIRTIO_INPUT_TABLET: DeviceId = DeviceId(29);

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
            DeviceId::PCI_INTX_ROUTER => Some("PCI_INTX_ROUTER"),
            DeviceId::ACPI_PM => Some("ACPI_PM"),
            DeviceId::HPET => Some("HPET"),
            DeviceId::HDA => Some("HDA"),
            DeviceId::E1000 => Some("E1000"),
            DeviceId::NET_STACK => Some("NET_STACK"),
            DeviceId::PLATFORM_INTERRUPTS => Some("PLATFORM_INTERRUPTS"),
            DeviceId::VIRTIO_SND => Some("VIRTIO_SND"),
            DeviceId::VIRTIO_NET => Some("VIRTIO_NET"),
            DeviceId::VIRTIO_INPUT => Some("VIRTIO_INPUT"),
            DeviceId::AEROGPU => Some("AEROGPU"),
            DeviceId::VIRTIO_INPUT_KEYBOARD => Some("VIRTIO_INPUT_KEYBOARD"),
            DeviceId::VIRTIO_INPUT_MOUSE => Some("VIRTIO_INPUT_MOUSE"),
            DeviceId::VIRTIO_INPUT_TABLET => Some("VIRTIO_INPUT_TABLET"),
            DeviceId::GPU_VRAM => Some("GPU_VRAM"),
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
