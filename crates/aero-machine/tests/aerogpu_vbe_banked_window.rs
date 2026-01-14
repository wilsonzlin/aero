use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use pretty_assertions::assert_eq;

const BANK_WINDOW_SIZE: u64 = 0x10000;

fn build_int10_vbe_banked_window_boot_sector(
    bank: u16,
    value: u8,
) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // int 10h AX=4F02: Set VBE mode 0x118 (1024x768x32bpp) with LFB requested.
    // mov ax, 0x4F02
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x02, 0x4F]);
    i += 3;
    // mov bx, 0x4118
    sector[i..i + 3].copy_from_slice(&[0xBB, 0x18, 0x41]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // int 10h AX=4F05: Set bank (BH=window=0, BL=subfunction=0, DX=bank).
    // mov ax, 0x4F05
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x05, 0x4F]);
    i += 3;
    // xor bx, bx
    sector[i..i + 2].copy_from_slice(&[0x31, 0xDB]);
    i += 2;
    // mov dx, imm16
    sector[i..i + 3].copy_from_slice(&[0xBA, (bank & 0xFF) as u8, (bank >> 8) as u8]);
    i += 3;
    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // Write `value` through the legacy banked window at A000:0000.
    // mov ax, 0xA000
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x00, 0xA0]);
    i += 3;
    // mov ds, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xD8]);
    i += 2;
    // mov byte ptr [0x0000], imm8
    sector[i..i + 5].copy_from_slice(&[0xC6, 0x06, 0x00, 0x00, value]);
    i += 5;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..200 {
        match m.run_slice(50_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

#[test]
fn aerogpu_legacy_a0000_banked_window_aliases_into_bar1_vbe_lfb() {
    let bank: u16 = 2;
    let value: u8 = 0xAB;
    let boot = build_int10_vbe_banked_window_boot_sector(bank, value);

    let mut m = Machine::new(MachineConfig {
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test deterministic.
        enable_serial: false,
        enable_i8042: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot.to_vec()).unwrap();
    m.reset();
    run_until_halt(&mut m);

    let bar1_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let cfg = pci_cfg
            .bus_mut()
            .device_config(aero_devices::pci::profile::AEROGPU.bdf)
            .expect("AeroGPU PCI function should exist");
        cfg.bar_range(aero_devices::pci::profile::AEROGPU_BAR1_VRAM_INDEX)
            .expect("BAR1 should be programmed")
            .base
    };

    let expected = bar1_base + VBE_LFB_OFFSET as u64 + u64::from(bank) * BANK_WINDOW_SIZE;
    assert_eq!(m.read_physical_u8(expected), value);

    // Ensure the write did not land in the legacy VGA backing region at VRAM offset 0.
    assert_ne!(m.read_physical_u8(bar1_base), value);
}
