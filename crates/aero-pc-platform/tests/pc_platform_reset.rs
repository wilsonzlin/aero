use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use aero_devices::a20_gate::A20_GATE_PORT;
use aero_devices::pci::profile::{
    AHCI_ABAR_CFG_OFFSET, IDE_PIIX3, NVME_CONTROLLER, SATA_AHCI_ICH9, USB_UHCI_PIIX3, VIRTIO_BLK,
};
use aero_devices::pci::{
    PciBdf, PciConfigSpace, PciDevice, PciInterruptPin, PciIntxRouterConfig, PCI_CFG_ADDR_PORT,
    PCI_CFG_DATA_PORT,
};
use aero_devices::reset_ctrl::{RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE};
use aero_devices_storage::ata::AtaDrive;
use aero_devices_storage::ata::ATA_CMD_READ_DMA_EXT;
use aero_devices_storage::pci_ide::PRIMARY_PORTS;
use aero_pc_platform::{PcPlatform, PcPlatformConfig, ResetEvent};
use aero_pci_routing::irq_line_for_intx;
use aero_storage::{MemBackend, RawDisk, VirtualDisk, SECTOR_SIZE};
use memory::MemoryBus as _;

struct DropDetectDisk {
    inner: RawDisk<MemBackend>,
    dropped: Arc<AtomicBool>,
}

struct DummyPciConfigDevice {
    config: PciConfigSpace,
}

impl DummyPciConfigDevice {
    fn new(vendor_id: u16, device_id: u16) -> Self {
        Self {
            config: PciConfigSpace::new(vendor_id, device_id),
        }
    }
}

impl PciDevice for DummyPciConfigDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
    // Intentionally do *not* override `reset`: we want the default `PciDevice::reset` behavior
    // (clear COMMAND only) so config-space interrupt line/pin writes persist unless the platform
    // explicitly reprograms them on reset.
}

impl Drop for DropDetectDisk {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl VirtualDisk for DropDetectDisk {
    fn capacity_bytes(&self) -> u64 {
        self.inner.capacity_bytes()
    }

    fn read_at(&mut self, offset: u64, buf: &mut [u8]) -> aero_storage::Result<()> {
        self.inner.read_at(offset, buf)
    }

    fn write_at(&mut self, offset: u64, buf: &[u8]) -> aero_storage::Result<()> {
        self.inner.write_at(offset, buf)
    }

    fn flush(&mut self) -> aero_storage::Result<()> {
        self.inner.flush()
    }
}

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.read(PCI_CFG_DATA_PORT, 4)
}

fn read_cfg_u8(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u8 {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    let port = PCI_CFG_DATA_PORT + u16::from(offset & 3);
    pc.io.read(port, 1) as u8
}

fn read_io_bar_base(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, bar: u8) -> u16 {
    let off = 0x10 + bar * 4;
    let val = read_cfg_u32(pc, bus, device, function, off);
    u16::try_from(val & 0xFFFF_FFFC).unwrap()
}

fn write_cfg_u8(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u8) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    let port = PCI_CFG_DATA_PORT + u16::from(offset & 3);
    pc.io.write(port, 1, u32::from(value));
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

fn read_ahci_bar5_base(pc: &mut PcPlatform) -> u64 {
    let bdf = SATA_AHCI_ICH9.bdf;
    let bar5 = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, AHCI_ABAR_CFG_OFFSET);
    u64::from(bar5 & 0xffff_fff0)
}

fn read_nvme_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

fn read_virtio_blk_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = VIRTIO_BLK.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

// -------------------------------------------------------------------------
// Minimal AHCI DMA helpers (port 0)
// -------------------------------------------------------------------------

const AHCI_HBA_GHC: u64 = 0x04;
const AHCI_PORT_BASE: u64 = 0x100;
const AHCI_PORT_REG_CLB: u64 = 0x00;
const AHCI_PORT_REG_CLBU: u64 = 0x04;
const AHCI_PORT_REG_FB: u64 = 0x08;
const AHCI_PORT_REG_FBU: u64 = 0x0C;
const AHCI_PORT_REG_IE: u64 = 0x14;
const AHCI_PORT_REG_CMD: u64 = 0x18;
const AHCI_PORT_REG_CI: u64 = 0x38;

const AHCI_GHC_IE: u32 = 1 << 1;
const AHCI_GHC_AE: u32 = 1 << 31;

const AHCI_PORT_CMD_ST: u32 = 1 << 0;
const AHCI_PORT_CMD_FRE: u32 = 1 << 4;

const AHCI_PORT_IS_DHRS: u32 = 1 << 0;

fn ahci_write_cmd_header(
    pc: &mut PcPlatform,
    clb: u64,
    slot: usize,
    ctba: u64,
    prdtl: u16,
    write: bool,
) {
    let cfl = 5u32;
    let w = if write { 1u32 << 6 } else { 0 };
    let flags = cfl | w | ((prdtl as u32) << 16);
    let addr = clb + (slot as u64) * 32;
    pc.memory.write_u32(addr, flags);
    pc.memory.write_u32(addr + 4, 0); // PRDBC
    pc.memory.write_u32(addr + 8, ctba as u32);
    pc.memory.write_u32(addr + 12, (ctba >> 32) as u32);
}

fn ahci_write_prdt(pc: &mut PcPlatform, ctba: u64, entry: usize, dba: u64, dbc: u32) {
    let addr = ctba + 0x80 + (entry as u64) * 16;
    pc.memory.write_u32(addr, dba as u32);
    pc.memory.write_u32(addr + 4, (dba >> 32) as u32);
    pc.memory.write_u32(addr + 8, 0);
    // DBC field stores byte_count-1 in bits 0..21.
    pc.memory.write_u32(addr + 12, (dbc - 1) & 0x003F_FFFF);
}

fn ahci_write_cfis(pc: &mut PcPlatform, ctba: u64, command: u8, lba: u64, count: u16) {
    let mut cfis = [0u8; 64];
    cfis[0] = 0x27;
    cfis[1] = 0x80;
    cfis[2] = command;
    cfis[7] = 0x40; // LBA mode

    cfis[4] = (lba & 0xFF) as u8;
    cfis[5] = ((lba >> 8) & 0xFF) as u8;
    cfis[6] = ((lba >> 16) & 0xFF) as u8;
    cfis[8] = ((lba >> 24) & 0xFF) as u8;
    cfis[9] = ((lba >> 32) & 0xFF) as u8;
    cfis[10] = ((lba >> 40) & 0xFF) as u8;

    cfis[12] = (count & 0xFF) as u8;
    cfis[13] = (count >> 8) as u8;

    pc.memory.write_physical(ctba, &cfis);
}

#[test]
fn pc_platform_reset_restores_deterministic_power_on_state() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Capture initial PCI state so we can verify it's restored deterministically.
    let bar5_base_before = read_ahci_bar5_base(&mut pc);
    let uhci_bdf = USB_UHCI_PIIX3.bdf;
    let uhci_bar4_before = read_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
    );

    // Mutate some state:
    // - Enable A20.
    pc.io.write_u8(A20_GATE_PORT, 0x02);
    assert!(pc.chipset.a20().enabled());

    // - Touch the PCI config address latch (PCI config mechanism #1).
    pc.io.write(PCI_CFG_ADDR_PORT, 4, 0x8000_0000);
    assert_eq!(pc.io.read(PCI_CFG_ADDR_PORT, 4), 0x8000_0000);

    // - Relocate UHCI BAR4 to a different base (to ensure PCI resources are reset deterministically).
    write_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
        0xD000,
    );
    let uhci_bar4_after = read_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
    );
    assert_ne!(uhci_bar4_after, uhci_bar4_before);

    // - Queue a reset event.
    pc.io.write_u8(RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);
    pc.io.write_u8(RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE);

    // - Disable PCI memory decoding for AHCI and move BAR5.
    let bdf = SATA_AHCI_ICH9.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0);
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        AHCI_ABAR_CFG_OFFSET,
        (bar5_base_before + 0x10_0000) as u32,
    );

    // Now reset back to baseline.
    pc.reset();

    // A20 must be disabled.
    assert!(!pc.chipset.a20().enabled());

    // Reset should clear any pending reset events.
    assert!(pc.take_reset_events().is_empty());

    // PCI config address latch should be cleared.
    assert_eq!(pc.io.read(PCI_CFG_ADDR_PORT, 4), 0);

    // UHCI BAR4 should be restored to its initial BIOS-assigned value.
    let uhci_bar4_after_reset = read_cfg_u32(
        &mut pc,
        uhci_bdf.bus,
        uhci_bdf.device,
        uhci_bdf.function,
        0x20,
    );
    assert_eq!(uhci_bar4_after_reset, uhci_bar4_before);

    // BIOS POST should deterministically reassign AHCI BAR5 to its original base.
    let bar5_base_after = read_ahci_bar5_base(&mut pc);
    assert_eq!(bar5_base_after, bar5_base_before);

    // Enable A20 so the AHCI MMIO base doesn't alias across the 1MiB boundary (A20 gate).
    pc.io.write_u8(A20_GATE_PORT, 0x02);

    // AHCI CAP register must be readable again after reset (i.e. MMIO decoding was restored).
    let cap = pc.memory.read_u32(bar5_base_after);
    assert_ne!(cap, 0xFFFF_FFFF);
    assert_ne!(cap & 0x8000_0000, 0);
}

#[test]
fn pc_platform_reset_restores_pci_intx_interrupt_line_and_pin_registers() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // First validate a built-in device from the default platform topology (AHCI at 00:02.0).
    let pirq_to_gsi = PciIntxRouterConfig::default().pirq_to_gsi;
    assert_eq!(
        pirq_to_gsi,
        [10, 11, 12, 13],
        "default PIRQ->GSI mapping should remain stable for reset determinism tests"
    );
    let ahci_bdf = SATA_AHCI_ICH9.bdf;
    {
        let pin_before = read_cfg_u8(
            &mut pc,
            ahci_bdf.bus,
            ahci_bdf.device,
            ahci_bdf.function,
            0x3d,
        );
        let line_before = read_cfg_u8(
            &mut pc,
            ahci_bdf.bus,
            ahci_bdf.device,
            ahci_bdf.function,
            0x3c,
        );

        let expected_pin = SATA_AHCI_ICH9
            .interrupt_pin
            .expect("AHCI profile should provide an interrupt pin")
            .to_config_u8();
        assert_eq!(pin_before, expected_pin);
        assert!(
            (1..=4).contains(&pin_before),
            "interrupt pin must be in PCI config-space encoding (1=INTA..4=INTD)"
        );
        // Explicitly validate the PCI swizzle against the canonical helper.
        let expected_line = irq_line_for_intx(pirq_to_gsi, ahci_bdf.device, pin_before);
        assert_eq!(line_before, expected_line);

        // Corrupt the fields so reset must restore them.
        write_cfg_u8(
            &mut pc,
            ahci_bdf.bus,
            ahci_bdf.device,
            ahci_bdf.function,
            0x3c,
            0x5a,
        );
        // Interrupt Pin is read-only on real PCI hardware; guest writes must be ignored.
        write_cfg_u8(
            &mut pc,
            ahci_bdf.bus,
            ahci_bdf.device,
            ahci_bdf.function,
            0x3d,
            0x04,
        );
        assert_eq!(
            read_cfg_u8(
                &mut pc,
                ahci_bdf.bus,
                ahci_bdf.device,
                ahci_bdf.function,
                0x3c
            ),
            0x5a
        );
        assert_eq!(
            read_cfg_u8(
                &mut pc,
                ahci_bdf.bus,
                ahci_bdf.device,
                ahci_bdf.function,
                0x3d
            ),
            pin_before
        );
    }

    // Add a dummy PCI endpoint that uses the *default* `PciDevice::reset` implementation (clears
    // COMMAND only). This ensures `PcPlatform::reset` must explicitly reprogram INTx metadata in
    // config space via `PciIntxRouter::configure_device_intx`.
    let expected_pin = PciInterruptPin::IntC;

    let bdf = {
        let mut pci_cfg = pc.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        // Pick a free device number so this test remains robust if the default PC platform adds
        // more devices in the future.
        let device = (0u8..32)
            .find(|&dev| {
                (0u8..8).all(|func| bus.device_config(PciBdf::new(0, dev, func)).is_none())
            })
            .expect("PCI bus is full; cannot allocate test-only dummy device");
        let bdf = PciBdf::new(0, device, 0);

        bus.add_device(bdf, Box::new(DummyPciConfigDevice::new(0x1af4, 0x1000)));
        let cfg = bus
            .device_config_mut(bdf)
            .expect("dummy device config should be accessible");
        pc.pci_intx
            .configure_device_intx(bdf, Some(expected_pin), cfg);
        bdf
    };
    pc.register_pci_intx_source(bdf, expected_pin, |_pc| false);

    let pin_before = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d);
    let line_before = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);

    assert_eq!(pin_before, expected_pin.to_config_u8());
    assert!(
        (1..=4).contains(&pin_before),
        "interrupt pin must be in PCI config-space encoding (1=INTA..4=INTD)"
    );

    // Explicitly validate the PCI swizzle:
    let expected_line = irq_line_for_intx(pirq_to_gsi, bdf.device, pin_before);
    assert_eq!(line_before, expected_line);

    // Smash the guest-visible INTx routing metadata.
    write_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c, 0x5a);
    // Interrupt Pin is read-only on real PCI hardware; guest writes must be ignored.
    write_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d, 0x04);
    assert_eq!(
        read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c),
        0x5a
    );
    assert_eq!(
        read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d),
        pin_before
    );

    pc.reset();

    // AHCI should have been restored back to its deterministic routing.
    {
        let pin_after = read_cfg_u8(
            &mut pc,
            ahci_bdf.bus,
            ahci_bdf.device,
            ahci_bdf.function,
            0x3d,
        );
        let line_after = read_cfg_u8(
            &mut pc,
            ahci_bdf.bus,
            ahci_bdf.device,
            ahci_bdf.function,
            0x3c,
        );

        let expected_pin = SATA_AHCI_ICH9
            .interrupt_pin
            .expect("AHCI profile should provide an interrupt pin")
            .to_config_u8();
        assert_eq!(pin_after, expected_pin);
        assert!(
            (1..=4).contains(&pin_after),
            "interrupt pin must be in PCI config-space encoding (1=INTA..4=INTD)"
        );
        let expected_line = irq_line_for_intx(pirq_to_gsi, ahci_bdf.device, pin_after);
        assert_eq!(line_after, expected_line);
    }

    let pin_after = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d);
    let line_after = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
    assert_eq!(pin_after, expected_pin.to_config_u8());

    let expected_line_after = irq_line_for_intx(pirq_to_gsi, bdf.device, pin_after);
    assert_eq!(line_after, expected_line_after);
}

#[test]
fn pc_platform_reset_pci_restores_pci_intx_interrupt_line_and_pin_registers() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Same setup as the full-platform reset test above, but exercise `PcPlatform::reset_pci()`
    // directly so regressions in that helper are also caught.
    let expected_pin = PciInterruptPin::IntC;
    let pirq_to_gsi = PciIntxRouterConfig::default().pirq_to_gsi;
    assert_eq!(
        pirq_to_gsi,
        [10, 11, 12, 13],
        "default PIRQ->GSI mapping should remain stable for reset determinism tests"
    );

    let bdf = {
        let mut pci_cfg = pc.pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let device = (0u8..32)
            .find(|&dev| {
                (0u8..8).all(|func| bus.device_config(PciBdf::new(0, dev, func)).is_none())
            })
            .expect("PCI bus is full; cannot allocate test-only dummy device");
        let bdf = PciBdf::new(0, device, 0);

        bus.add_device(bdf, Box::new(DummyPciConfigDevice::new(0x1af4, 0x1000)));
        let cfg = bus
            .device_config_mut(bdf)
            .expect("dummy device config should be accessible");
        pc.pci_intx
            .configure_device_intx(bdf, Some(expected_pin), cfg);
        bdf
    };
    pc.register_pci_intx_source(bdf, expected_pin, |_pc| false);

    let pin_before = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d);
    let line_before = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
    assert_eq!(pin_before, expected_pin.to_config_u8());
    let expected_line_before = irq_line_for_intx(pirq_to_gsi, bdf.device, pin_before);
    assert_eq!(line_before, expected_line_before);

    // Smash the guest-visible INTx routing metadata.
    write_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c, 0x5a);
    // Interrupt Pin is read-only on real PCI hardware; guest writes must be ignored.
    write_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d, 0x04);
    assert_eq!(
        read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c),
        0x5a
    );
    assert_eq!(
        read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d),
        pin_before
    );

    pc.reset_pci();

    let pin_after = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3d);
    let line_after = read_cfg_u8(&mut pc, bdf.bus, bdf.device, bdf.function, 0x3c);
    assert_eq!(pin_after, expected_pin.to_config_u8());

    let expected_line_after = irq_line_for_intx(pirq_to_gsi, bdf.device, pin_after);
    assert_eq!(line_after, expected_line_after);
}

#[test]
fn pc_platform_reset_resets_nvme_controller_state() {
    let mut pc = PcPlatform::new_with_nvme(2 * 1024 * 1024);
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_base = read_nvme_bar0_base(&mut pc);

    // Enable the controller and mutate a few registers so we can detect that reset cleared them.
    let asq = 0x10000u64;
    let acq = 0x20000u64;

    pc.memory.write_u32(bar0_base + 0x0024, 0x000f_000f); // AQA
    pc.memory.write_u64(bar0_base + 0x0028, asq); // ASQ
    pc.memory.write_u64(bar0_base + 0x0030, acq); // ACQ
    pc.memory.write_u32(bar0_base + 0x0014, 1); // CC.EN
    assert_eq!(pc.memory.read_u32(bar0_base + 0x001c) & 1, 1);

    pc.memory.write_u32(bar0_base + 0x000c, 1); // INTMS
    assert_eq!(pc.memory.read_u32(bar0_base + 0x000c) & 1, 1);

    pc.reset();

    // Re-enable memory decoding in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    let bar0_base_after = read_nvme_bar0_base(&mut pc);

    assert_eq!(
        pc.memory.read_u32(bar0_base_after + 0x0014),
        0,
        "reset should clear NVMe CC register"
    );
    assert_eq!(
        pc.memory.read_u32(bar0_base_after + 0x001c),
        0,
        "reset should clear NVMe CSTS register"
    );
    assert_eq!(
        pc.memory.read_u32(bar0_base_after + 0x000c),
        0,
        "reset should clear NVMe interrupt mask register"
    );
}

#[test]
fn pc_platform_reset_preserves_nvme_disk_backend() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    // Keep this test focused: enable only the NVMe controller.
    let mut pc = PcPlatform::new_with_config_and_nvme_disk(
        2 * 1024 * 1024,
        PcPlatformConfig {
            cpu_count: 1,
            enable_hda: false,
            enable_nvme: true,
            enable_ahci: false,
            enable_ide: false,
            enable_e1000: false,
            mac_addr: None,
            enable_uhci: false,
            enable_ehci: false,
            enable_xhci: false,
            enable_virtio_blk: false,
            enable_virtio_msix: false,
        },
        Box::new(disk),
    );

    pc.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "platform reset dropped the NVMe disk backend"
    );

    // Dropping the platform should drop the disk backend (sanity check).
    drop(pc);
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropping the platform should drop the NVMe disk backend"
    );
}

#[test]
fn pc_platform_reset_preserves_ide_disk_backend() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            cpu_count: 1,
            enable_hda: false,
            enable_nvme: false,
            enable_ahci: false,
            enable_ide: true,
            enable_e1000: false,
            mac_addr: None,
            enable_uhci: false,
            enable_ehci: false,
            enable_xhci: false,
            enable_virtio_blk: false,
            enable_virtio_msix: false,
        },
    );

    pc.attach_ide_primary_master_drive(AtaDrive::new(Box::new(disk)).unwrap());
    pc.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "platform reset dropped the IDE disk backend"
    );

    // Replacing the drive should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    pc.attach_ide_primary_master_drive(AtaDrive::new(Box::new(replacement)).unwrap());
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the IDE drive should drop the previous disk backend"
    );
}

#[test]
fn pc_platform_reset_preserves_ide_iso_backend() {
    let dropped = Arc::new(AtomicBool::new(false));
    // ATAPI uses 2048-byte sectors, so ensure capacity is 2048-aligned.
    let capacity = 16 * SECTOR_SIZE as u64;
    assert_eq!(capacity % 2048, 0);
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    let mut pc = PcPlatform::new_with_config(
        2 * 1024 * 1024,
        PcPlatformConfig {
            cpu_count: 1,
            enable_hda: false,
            enable_nvme: false,
            enable_ahci: false,
            enable_ide: true,
            enable_e1000: false,
            mac_addr: None,
            enable_uhci: false,
            enable_ehci: false,
            enable_xhci: false,
            enable_virtio_blk: false,
            enable_virtio_msix: false,
        },
    );

    pc.attach_ide_secondary_master_iso(Box::new(disk)).unwrap();
    pc.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "platform reset dropped the IDE ISO backend"
    );

    // Replacing the ISO should drop the previous backend (sanity check that it was attached).
    let replacement = RawDisk::create(MemBackend::new(), capacity).unwrap();
    pc.attach_ide_secondary_master_iso(Box::new(replacement))
        .unwrap();
    assert!(
        dropped.load(Ordering::SeqCst),
        "replacing the IDE ISO should drop the previous backend"
    );
}

#[test]
fn pc_platform_reset_resets_ide_controller_state() {
    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    let bdf = IDE_PIIX3.bdf;

    // Attach a disk so status reads are driven by the selected device.
    let disk = RawDisk::create(MemBackend::new(), 8 * SECTOR_SIZE as u64).unwrap();
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Ensure I/O decoding is enabled so legacy ports + BAR4 are accessible.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);

    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0);

    let status_before = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(
        status_before, 0xFF,
        "IDE status should decode with a drive present"
    );

    // Mutate Bus Master IDE registers so we can verify they're cleared by reset.
    pc.io.write(bm_base, 1, 0x09);
    pc.io.write(bm_base + 4, 4, 0x1234_5678);
    assert_eq!(pc.io.read(bm_base, 1), 0x09);
    assert_eq!(pc.io.read(bm_base + 4, 4), 0x1234_5678);

    pc.reset();

    // Re-enable I/O decoding in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    let bm_base_after = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base_after, 0);

    let status_after = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1) as u8;
    assert_ne!(
        status_after, 0xFF,
        "IDE drive presence should survive platform reset"
    );

    assert_eq!(
        pc.io.read(bm_base_after, 1),
        0,
        "Bus Master IDE command register should be cleared on reset"
    );
    assert_eq!(
        pc.io.read(bm_base_after + 4, 4),
        0,
        "Bus Master IDE PRD pointer should be cleared on reset"
    );
}

#[test]
fn pc_platform_reset_clears_ide_nien_and_allows_irq14_delivery() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ide(2 * 1024 * 1024);
    pc.attach_ide_primary_master_disk(Box::new(disk)).unwrap();

    // Enable legacy I/O decoding so we can program the device control register (nIEN).
    let bdf = IDE_PIIX3.bdf;
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0001);
    let bm_base = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base, 0);

    // Mask IDE interrupts via the device control register.
    pc.io.write(PRIMARY_PORTS.ctrl_base, 1, 0x02);

    pc.reset();

    // Re-enable bus mastering and I/O decode in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0005);
    let bm_base_after = read_io_bar_base(&mut pc, bdf.bus, bdf.device, bdf.function, 4);
    assert_ne!(bm_base_after, 0);

    // Unmask IRQ2 (cascade) and IRQ14 so we can observe primary IDE IRQ delivery via the PIC.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().set_offsets(0x20, 0x28);
        interrupts.pic_mut().set_masked(2, false);
        interrupts.pic_mut().set_masked(14, false);
    }

    // PRD table at 0x1000: one entry, end-of-table, 512 bytes.
    let prd_addr = 0x1000u64;
    let read_buf = 0x2000u64;
    pc.memory.write_u32(prd_addr, read_buf as u32);
    pc.memory.write_u16(prd_addr + 4, SECTOR_SIZE as u16);
    pc.memory.write_u16(prd_addr + 6, 0x8000);
    pc.io.write(bm_base_after + 4, 4, prd_addr as u32);

    // Issue READ DMA (LBA 0, 1 sector) and start bus master.
    pc.io.write(PRIMARY_PORTS.cmd_base + 6, 1, 0xE0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 2, 1, 1);
    pc.io.write(PRIMARY_PORTS.cmd_base + 3, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 4, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 5, 1, 0);
    pc.io.write(PRIMARY_PORTS.cmd_base + 7, 1, 0xC8); // READ DMA
    pc.io.write(bm_base_after, 1, 0x09);

    pc.process_ide();
    pc.poll_pci_intx_lines();

    let mut out = [0u8; 4];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out, b"BOOT");

    let pending = pc
        .interrupts
        .borrow()
        .pic()
        .get_pending_vector()
        .expect("IRQ14 should be pending after reset clears nIEN");
    let irq = pc
        .interrupts
        .borrow()
        .pic()
        .vector_to_irq(pending)
        .expect("pending vector should decode to an IRQ number");
    assert_eq!(irq, 14);

    // Consume and EOI the interrupt so subsequent assertions are not affected by PIC latching.
    {
        let mut interrupts = pc.interrupts.borrow_mut();
        interrupts.pic_mut().acknowledge(pending);
        interrupts.pic_mut().eoi(pending);
    }

    // Clear the IDE device interrupt by reading the status register.
    let _ = pc.io.read(PRIMARY_PORTS.cmd_base + 7, 1);
    pc.poll_pci_intx_lines();
    assert_eq!(pc.interrupts.borrow().pic().get_pending_vector(), None);
}

#[test]
fn pc_platform_reset_resets_virtio_blk_transport_state_and_preserves_disk_backend() {
    let dropped = Arc::new(AtomicBool::new(false));
    let capacity = 16 * SECTOR_SIZE as u64;
    let disk = DropDetectDisk {
        inner: RawDisk::create(MemBackend::new(), capacity).unwrap(),
        dropped: dropped.clone(),
    };

    let mut pc = PcPlatform::new_with_virtio_blk_disk(2 * 1024 * 1024, Box::new(disk));

    let bdf = VIRTIO_BLK.bdf;
    // Ensure memory decoding is enabled so BAR0 MMIO is accessible.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    let bar0_base = read_virtio_blk_bar0_base(&mut pc);
    assert_ne!(bar0_base, 0);

    // Mutate the virtio device status register (common cfg offset 0x14).
    pc.memory.write_u8(bar0_base + 0x14, 0x04); // VIRTIO_STATUS_DRIVER_OK
    assert_eq!(pc.memory.read_u8(bar0_base + 0x14), 0x04);

    pc.reset();

    assert!(
        !dropped.load(Ordering::SeqCst),
        "platform reset dropped the virtio-blk disk backend"
    );

    // Re-enable memory decoding in case the post-reset BIOS chose a different policy.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);
    let bar0_base_after = read_virtio_blk_bar0_base(&mut pc);
    assert_ne!(bar0_base_after, 0);

    assert_eq!(
        pc.memory.read_u8(bar0_base_after + 0x14),
        0,
        "platform reset should clear virtio device status"
    );

    // Dropping the platform should drop the disk backend (sanity check).
    drop(pc);
    assert!(
        dropped.load(Ordering::SeqCst),
        "dropping the platform should drop the virtio-blk disk backend"
    );
}

#[test]
fn pc_platform_reset_preserves_ahci_attached_disk() {
    let capacity = 8 * SECTOR_SIZE as u64;
    let mut disk = RawDisk::create(MemBackend::new(), capacity).unwrap();
    let mut sector0 = vec![0u8; SECTOR_SIZE];
    sector0[0..4].copy_from_slice(b"BOOT");
    sector0[510] = 0x55;
    sector0[511] = 0xAA;
    disk.write_sectors(0, &sector0).unwrap();

    let mut pc = PcPlatform::new_with_ahci(2 * 1024 * 1024);
    pc.attach_ahci_disk_port0(Box::new(disk)).unwrap();

    // Reset after the disk has been attached; the disk must remain present.
    pc.reset();

    // Enable A20 so high MMIO addresses don't alias across the 1MiB boundary.
    pc.io.write_u8(A20_GATE_PORT, 0x02);

    let bdf = SATA_AHCI_ICH9.bdf;
    // Enable MMIO decoding + DMA.
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0006);
    let bar5_base = read_ahci_bar5_base(&mut pc);
    assert_ne!(bar5_base, 0);

    // Program HBA + port 0 registers.
    let clb = 0x1000u64;
    let fb = 0x2000u64;
    let ctba = 0x3000u64;
    let read_buf = 0x4000u64;

    pc.memory
        .write_u32(bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_CLB, clb as u32);
    pc.memory.write_u32(
        bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_CLBU,
        (clb >> 32) as u32,
    );
    pc.memory
        .write_u32(bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_FB, fb as u32);
    pc.memory.write_u32(
        bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_FBU,
        (fb >> 32) as u32,
    );

    pc.memory
        .write_u32(bar5_base + AHCI_HBA_GHC, AHCI_GHC_AE | AHCI_GHC_IE);
    pc.memory.write_u32(
        bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_IE,
        AHCI_PORT_IS_DHRS,
    );
    pc.memory.write_u32(
        bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_CMD,
        AHCI_PORT_CMD_ST | AHCI_PORT_CMD_FRE,
    );

    // READ DMA EXT for LBA 0, 1 sector.
    ahci_write_cmd_header(&mut pc, clb, 0, ctba, 1, false);
    ahci_write_cfis(&mut pc, ctba, ATA_CMD_READ_DMA_EXT, 0, 1);
    ahci_write_prdt(&mut pc, ctba, 0, read_buf, SECTOR_SIZE as u32);

    pc.memory.write_u32(read_buf, 0);
    pc.memory
        .write_u32(bar5_base + AHCI_PORT_BASE + AHCI_PORT_REG_CI, 1);
    pc.process_ahci();

    let mut out = [0u8; SECTOR_SIZE];
    pc.memory.read_physical(read_buf, &mut out);
    assert_eq!(&out[0..4], b"BOOT");
    assert_eq!(&out[510..512], &[0x55, 0xAA]);
}
