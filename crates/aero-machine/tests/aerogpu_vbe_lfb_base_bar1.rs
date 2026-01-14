use aero_devices::pci::profile;
use aero_machine::{Machine, MachineConfig, RunExit, VBE_LFB_OFFSET};
use pretty_assertions::assert_eq;

fn build_vbe_mode_info_boot_sector(mode: u16) -> [u8; aero_storage::SECTOR_SIZE] {
    let mut sector = [0u8; aero_storage::SECTOR_SIZE];
    let mut i = 0usize;

    // xor ax, ax
    sector[i..i + 2].copy_from_slice(&[0x31, 0xC0]);
    i += 2;
    // mov es, ax
    sector[i..i + 2].copy_from_slice(&[0x8E, 0xC0]);
    i += 2;
    // mov di, 0x0500
    sector[i..i + 3].copy_from_slice(&[0xBF, 0x00, 0x05]);
    i += 3;

    // mov ax, 0x4F01 (VBE Get Mode Info)
    sector[i..i + 3].copy_from_slice(&[0xB8, 0x01, 0x4F]);
    i += 3;

    // mov cx, mode
    let [mode_lo, mode_hi] = mode.to_le_bytes();
    sector[i..i + 3].copy_from_slice(&[0xB9, mode_lo, mode_hi]);
    i += 3;

    // int 0x10
    sector[i..i + 2].copy_from_slice(&[0xCD, 0x10]);
    i += 2;

    // hlt
    sector[i] = 0xF4;

    sector[510] = 0x55;
    sector[511] = 0xAA;
    sector
}

fn run_until_halt(m: &mut Machine) {
    for _ in 0..100 {
        match m.run_slice(10_000) {
            RunExit::Halted { .. } => return,
            RunExit::Completed { .. } => continue,
            other => panic!("unexpected exit: {other:?}"),
        }
    }
    panic!("guest did not reach HLT");
}

fn new_deterministic_aerogpu_machine(boot_sector: [u8; aero_storage::SECTOR_SIZE]) -> Machine {
    let mut m = Machine::new(MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_aerogpu: true,
        enable_vga: false,
        // Keep the test output deterministic.
        enable_serial: false,
        enable_i8042: false,
        // Avoid extra legacy port devices that aren't needed for these tests.
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        // Keep the machine minimal.
        enable_e1000: false,
        enable_virtio_net: false,
        ..Default::default()
    })
    .unwrap();

    m.set_disk_image(boot_sector.to_vec()).unwrap();
    m.reset();
    m
}

#[test]
fn aerogpu_bios_vbe_reports_lfb_base_inside_bar1_for_0x115_and_0x160() {
    for (mode, (w, h)) in [(0x115u16, (800u16, 600u16)), (0x160u16, (1280u16, 720u16))] {
        let boot = build_vbe_mode_info_boot_sector(mode);
        let mut m = new_deterministic_aerogpu_machine(boot);

        run_until_halt(&mut m);

        // Resolve the AeroGPU BAR1 base assigned by PCI BIOS POST.
        let bdf = m
            .aerogpu_bdf()
            .expect("AeroGPU should be present when enable_aerogpu=true");
        let bar1_base = m
            .pci_bar_base(bdf, profile::AEROGPU_BAR1_VRAM_INDEX)
            .filter(|&base| base != 0)
            .expect("AeroGPU BAR1 should be assigned by BIOS POST");

        // VBE mode info block was written to 0x0000:0x0500 by INT 10h AX=4F01.
        let phys_base_ptr = m.read_physical_u32(0x0500 + 40);
        let expected = u32::try_from(bar1_base + VBE_LFB_OFFSET as u64)
            .expect("BAR1 base + VBE_LFB_OFFSET should fit in u32");
        assert_eq!(phys_base_ptr, expected);
        assert_eq!(phys_base_ptr, m.vbe_lfb_base_u32());

        // Also sanity-check reported resolution matches the requested mode.
        let got_w = m.read_physical_u16(0x0500 + 18);
        let got_h = m.read_physical_u16(0x0500 + 20);
        assert_eq!((got_w, got_h), (w, h));
    }
}
