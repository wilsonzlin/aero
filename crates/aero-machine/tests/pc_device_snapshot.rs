use aero_devices::hpet;
use aero_devices::pci::{PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT};
use aero_devices::pic8259::MASTER_CMD;
use aero_devices::pit8254::{PIT_CH0, PIT_CMD};
use aero_machine::{Machine, MachineConfig};
use pretty_assertions::assert_eq;

fn pci_cfg_addr(bus: u8, dev: u8, func: u8, offset: u8) -> u32 {
    let offset = offset & !0x3;
    0x8000_0000u32
        | ((bus as u32) << 16)
        | ((dev as u32) << 11)
        | ((func as u32) << 8)
        | (offset as u32)
}

fn read_pci_command(m: &mut Machine, bus: u8, dev: u8, func: u8) -> u16 {
    m.io_write(PCI_CFG_ADDR_PORT, 4, pci_cfg_addr(bus, dev, func, 0x04));
    m.io_read(PCI_CFG_DATA_PORT, 2) as u16
}

fn write_pci_command(m: &mut Machine, bus: u8, dev: u8, func: u8, command: u16) {
    m.io_write(PCI_CFG_ADDR_PORT, 4, pci_cfg_addr(bus, dev, func, 0x04));
    m.io_write(PCI_CFG_DATA_PORT, 2, command as u32);
}

fn read_pic_irr(m: &mut Machine) -> u8 {
    // OCW3: read IRR.
    m.io_write(MASTER_CMD, 1, 0x0A);
    m.io_read(MASTER_CMD, 1) as u8
}

fn read_pit_count_ch0(m: &mut Machine) -> u16 {
    // Latch channel 0 count.
    m.io_write(PIT_CMD, 1, 0x00);
    let lo = m.io_read(PIT_CH0, 1) as u8;
    let hi = m.io_read(PIT_CH0, 1) as u8;
    u16::from_le_bytes([lo, hi])
}

#[test]
fn snapshot_restore_preserves_pc_device_state() {
    let cfg = MachineConfig {
        ram_size_bytes: 16 * 1024 * 1024,
        enable_pc_platform: true,
        ..Default::default()
    };

    let mut src = Machine::new(cfg.clone()).unwrap();

    // Enable A20 so high MMIO addresses (HPET) are not affected by address-line masking.
    src.io_write(0x92, 1, 0x02);

    // Enable HPET (General Configuration: enable bit).
    src.write_physical_u64(hpet::HPET_MMIO_BASE + 0x010, 1);

    // Program PIT channel 0 (mode 2, lobyte/hibyte) with a small reload value so it will fire
    // within a short tick interval.
    let reload: u16 = 1000;
    src.io_write(PIT_CMD, 1, 0x34);
    src.io_write(PIT_CH0, 1, u32::from(reload as u8));
    src.io_write(PIT_CH0, 1, u32::from((reload >> 8) as u8));

    // Touch PCI config state (ISA bridge @ 00:1f.0): set command register to a non-zero value.
    write_pci_command(&mut src, 0, 0x1f, 0, 0x0007);

    // Advance enough time to mutate timer/interrupt controller state.
    src.tick(1_000_000); // 1ms

    let snap = src.take_snapshot_full().unwrap();

    let baseline_hpet = src.read_physical_u64(hpet::HPET_MMIO_BASE + 0x0F0);
    let baseline_pit = read_pit_count_ch0(&mut src);
    let baseline_pic_irr = read_pic_irr(&mut src);
    let baseline_pci_cfg_addr = src.io_read(PCI_CFG_ADDR_PORT, 4);
    let baseline_pci_cmd = read_pci_command(&mut src, 0, 0x1f, 0);

    assert!(baseline_hpet > 0, "HPET counter should have advanced");
    assert_ne!(
        baseline_pic_irr & 0x01,
        0,
        "PIT IRQ0 should set PIC IRR bit0"
    );
    assert_eq!(baseline_pci_cmd, 0x0007);

    let mut restored = Machine::new(cfg).unwrap();
    restored.restore_snapshot_bytes(&snap).unwrap();

    assert_eq!(
        restored.read_physical_u64(hpet::HPET_MMIO_BASE + 0x0F0),
        baseline_hpet
    );
    assert_eq!(read_pit_count_ch0(&mut restored), baseline_pit);
    assert_eq!(read_pic_irr(&mut restored), baseline_pic_irr);
    assert_eq!(
        restored.io_read(PCI_CFG_ADDR_PORT, 4),
        baseline_pci_cfg_addr
    );
    assert_eq!(
        read_pci_command(&mut restored, 0, 0x1f, 0),
        baseline_pci_cmd
    );

    // Validate continuity: ticking both machines further should keep them in sync.
    src.tick(500_000);
    restored.tick(500_000);

    assert_eq!(
        restored.read_physical_u64(hpet::HPET_MMIO_BASE + 0x0F0),
        src.read_physical_u64(hpet::HPET_MMIO_BASE + 0x0F0)
    );
    assert_eq!(
        read_pit_count_ch0(&mut restored),
        read_pit_count_ch0(&mut src)
    );
}
