use aero_devices::reset_ctrl::RESET_CTRL_RESET_VALUE;
use aero_devices::pci::profile::NVME_CONTROLLER;
use aero_devices::pci::profile::USB_UHCI_PIIX3;
use aero_pc_platform::{PcPlatform, ResetEvent};
use memory::MemoryBus as _;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn read_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.read(0xCFC, 4)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io
        .write(0xCF8, 4, cfg_addr(bus, device, function, offset));
    pc.io.write(0xCFC, 2, u32::from(value));
}

fn read_nvme_bar0_base(pc: &mut PcPlatform) -> u64 {
    let bdf = NVME_CONTROLLER.bdf;
    let bar0_lo = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x10);
    let bar0_hi = read_cfg_u32(pc, bdf.bus, bdf.device, bdf.function, 0x14);
    (u64::from(bar0_hi) << 32) | u64::from(bar0_lo & 0xffff_fff0)
}

#[test]
fn pc_platform_reset_restores_deterministic_power_on_state() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    // Capture an initial piece of PCI state so we can verify it's restored deterministically.
    let uhci_bar4_addr = 0x8000_0000
        | ((USB_UHCI_PIIX3.bdf.bus as u32) << 16)
        | ((USB_UHCI_PIIX3.bdf.device as u32) << 11)
        | ((USB_UHCI_PIIX3.bdf.function as u32) << 8)
        | 0x20;
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    let uhci_bar4_before = pc.io.read(0xCFC, 4);

    // Mutate some state:
    // - Enable A20.
    pc.io.write_u8(0x92, 0x02);
    assert!(pc.chipset.a20().enabled());

    // - Touch the PCI config address latch (0xCF8).
    pc.io.write(0xCF8, 4, 0x8000_0000);
    assert_eq!(pc.io.read(0xCF8, 4), 0x8000_0000);

    // - Relocate UHCI BAR4 to a different base (to ensure PCI resources are reset deterministically).
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    pc.io.write(0xCFC, 4, 0xD000);
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    let uhci_bar4_after = pc.io.read(0xCFC, 4);
    assert_ne!(uhci_bar4_after, uhci_bar4_before);

    // - Queue a reset event.
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);
    assert_eq!(pc.take_reset_events(), vec![ResetEvent::System]);
    pc.io.write_u8(0xCF9, RESET_CTRL_RESET_VALUE);

    // Now reset back to baseline.
    pc.reset();

    // A20 must be disabled.
    assert!(!pc.chipset.a20().enabled());

    // Reset should clear any pending reset events.
    assert!(pc.take_reset_events().is_empty());

    // PCI config address latch should be cleared.
    assert_eq!(pc.io.read(0xCF8, 4), 0);

    // UHCI BAR4 should be restored to its initial BIOS-assigned value.
    pc.io.write(0xCF8, 4, uhci_bar4_addr);
    assert_eq!(pc.io.read(0xCFC, 4), uhci_bar4_before);
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
