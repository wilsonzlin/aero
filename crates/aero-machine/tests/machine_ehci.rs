#![cfg(not(target_arch = "wasm32"))]

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::USB_EHCI_ICH9;
use aero_devices::usb::ehci::{regs, EhciPciDevice, CAPLENGTH, HCIVERSION};
use aero_io_snapshot::io::state::IoSnapshot;
use aero_machine::{Machine, MachineConfig};
use aero_snapshot as snapshot;
use pretty_assertions::assert_eq;

#[test]
fn ehci_pci_function_exists_and_capability_registers_are_mmio_readable() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        // Keep the machine minimal/deterministic for this PCI/MMIO probe.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut m = Machine::new(cfg).unwrap();

    let (class, command, bar0_base) = {
        let pci_cfg = m
            .pci_config_ports()
            .expect("pc platform should expose pci_cfg");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(USB_EHCI_ICH9.bdf)
            .expect("EHCI PCI function should exist");
        (
            cfg.class_code(),
            cfg.command(),
            cfg.bar_range(EhciPciDevice::MMIO_BAR_INDEX)
                .map(|range| range.base)
                .unwrap_or(0),
        )
    };

    assert_eq!(class.class, 0x0c, "base class should be Serial Bus");
    assert_eq!(class.subclass, 0x03, "subclass should be USB");
    assert_eq!(class.prog_if, 0x20, "prog-if should be EHCI");
    assert_ne!(
        command & 0x2,
        0,
        "EHCI MEM decoding should be enabled by BIOS POST"
    );
    assert_ne!(
        bar0_base, 0,
        "EHCI BAR0 base should be assigned by BIOS POST"
    );

    // CAPLENGTH (byte0) + HCIVERSION (u16 at bytes2-3).
    let cap = m.read_physical_u32(bar0_base);
    assert_eq!((cap & 0xFF) as u8, CAPLENGTH);
    assert_eq!((cap >> 16) as u16, HCIVERSION);
}

struct LegacyEhciUsbSnapshotSource {
    machine: Machine,
}

impl snapshot::SnapshotSource for LegacyEhciUsbSnapshotSource {
    fn snapshot_meta(&mut self) -> snapshot::SnapshotMeta {
        snapshot::SnapshotSource::snapshot_meta(&mut self.machine)
    }

    fn cpu_state(&self) -> snapshot::CpuState {
        snapshot::SnapshotSource::cpu_state(&self.machine)
    }

    fn mmu_state(&self) -> snapshot::MmuState {
        snapshot::SnapshotSource::mmu_state(&self.machine)
    }

    fn device_states(&self) -> Vec<snapshot::DeviceState> {
        let mut devices = snapshot::SnapshotSource::device_states(&self.machine);
        let Some(pos) = devices
            .iter()
            .position(|device| device.id == snapshot::DeviceId::USB)
        else {
            return devices;
        };
        let Some(ehci) = self.machine.ehci() else {
            return devices;
        };

        let version = <EhciPciDevice as IoSnapshot>::DEVICE_VERSION;
        devices[pos] = snapshot::DeviceState {
            id: snapshot::DeviceId::USB,
            version: version.major,
            flags: version.minor,
            data: ehci.borrow().save_state(),
        };
        devices
    }

    fn disk_overlays(&self) -> snapshot::DiskOverlayRefs {
        snapshot::SnapshotSource::disk_overlays(&self.machine)
    }

    fn ram_len(&self) -> usize {
        snapshot::SnapshotSource::ram_len(&self.machine)
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> snapshot::Result<()> {
        snapshot::SnapshotSource::read_ram(&self.machine, offset, buf)
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        snapshot::SnapshotSource::take_dirty_pages(&mut self.machine)
    }
}

#[test]
fn ehci_restore_accepts_legacy_deviceid_usb_payload_without_machine_remainder() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_ehci: true,
        enable_uhci: false,
        // Keep the machine minimal/deterministic for this snapshot test.
        enable_vga: false,
        enable_serial: false,
        enable_i8042: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };

    let mut src = LegacyEhciUsbSnapshotSource {
        machine: Machine::new(cfg.clone()).unwrap(),
    };
    // Ensure high MMIO addresses decode correctly (avoid A20 aliasing).
    src.machine.io_write(A20_GATE_PORT, 1, 0x02);

    let bar0_base = src
        .machine
        .pci_bar_base(USB_EHCI_ICH9.bdf, EhciPciDevice::MMIO_BAR_INDEX)
        .expect("EHCI BAR0 should exist");
    assert_ne!(bar0_base, 0);

    // Start the controller (USBCMD.RS) so FRINDEX advances when ticking.
    let usbcmd_before = src.machine.read_physical_u32(bar0_base + regs::REG_USBCMD);
    src.machine
        .write_physical_u32(bar0_base + regs::REG_USBCMD, usbcmd_before | regs::USBCMD_RS);

    let frindex_before = src.machine.read_physical_u32(bar0_base + regs::REG_FRINDEX)
        & regs::FRINDEX_MASK;
    // Advance by 1.5ms so the controller ticks once and the machine accumulates a sub-ms remainder.
    src.machine.tick_platform(1_500_000);
    let frindex_snapshot = src.machine.read_physical_u32(bar0_base + regs::REG_FRINDEX)
        & regs::FRINDEX_MASK;
    assert_eq!(frindex_snapshot, frindex_before.wrapping_add(8) & regs::FRINDEX_MASK);

    // Ensure our snapshot source really emitted the legacy payload (EHCP), not the canonical `USBC`
    // wrapper.
    let usb_state = snapshot::SnapshotSource::device_states(&src)
        .into_iter()
        .find(|d| d.id == snapshot::DeviceId::USB)
        .expect("USB device state should exist");
    assert_eq!(
        usb_state.data.get(8..12),
        Some(b"EHCP".as_slice()),
        "legacy snapshot should store EHCI PCI snapshot directly under DeviceId::USB"
    );

    let mut bytes = std::io::Cursor::new(Vec::new());
    snapshot::save_snapshot(&mut bytes, &mut src, snapshot::SaveOptions::default()).unwrap();
    let bytes = bytes.into_inner();

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&bytes).unwrap();
    restored.io_write(A20_GATE_PORT, 1, 0x02);

    let bar0_base_restored = restored
        .pci_bar_base(USB_EHCI_ICH9.bdf, EhciPciDevice::MMIO_BAR_INDEX)
        .expect("EHCI BAR0 should exist");
    assert_ne!(bar0_base_restored, 0);

    let frindex_after_restore = restored.read_physical_u32(bar0_base_restored + regs::REG_FRINDEX)
        & regs::FRINDEX_MASK;
    assert_eq!(frindex_after_restore, frindex_snapshot);

    // Legacy snapshots did not include the machine's sub-ms tick remainder; we should start from a
    // deterministic default of 0 on restore.
    restored.tick_platform(500_000);
    let frindex_after_half_ms = restored.read_physical_u32(bar0_base_restored + regs::REG_FRINDEX)
        & regs::FRINDEX_MASK;
    assert_eq!(frindex_after_half_ms, frindex_snapshot);

    restored.tick_platform(500_000);
    let frindex_after_full_ms = restored.read_physical_u32(bar0_base_restored + regs::REG_FRINDEX)
        & regs::FRINDEX_MASK;
    assert_eq!(
        frindex_after_full_ms,
        frindex_snapshot.wrapping_add(8) & regs::FRINDEX_MASK
    );
}
