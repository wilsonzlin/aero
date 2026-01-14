use aero_devices::pci::PciBdf;
use aero_gpu_vga::DisplayOutput;
use aero_machine::{Machine, MachineConfig};

#[test]
fn vga_pci_stub_enumerates_and_bar0_sizes_correctly() {
    let cfg = MachineConfig {
        ram_size_bytes: 2 * 1024 * 1024,
        enable_pc_platform: true,
        enable_vga: true,
        // Keep this test stable if AeroGPU becomes enabled-by-default in the canonical presets.
        enable_aerogpu: false,
        // Keep the machine minimal for the PCI stub check.
        enable_serial: false,
        enable_i8042: false,
        enable_a20_gate: false,
        enable_reset_ctrl: false,
        enable_e1000: false,
        ..Default::default()
    };
    let mut m = Machine::new(cfg).unwrap();

    // The canonical machine may expose a transitional VGA/VBE PCI stub at this fixed BDF:
    // `00:0c.0` (see `docs/pci-device-compatibility.md`). Phase 2 removes it when AeroGPU owns
    // legacy VGA/VBE, so treat absence as a no-op for this test.
    let bdf = PciBdf::new(0, 0x0c, 0);
    {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();
        let vendor_id = bus.read_config(bdf, 0x00, 2) as u16;
        if vendor_id == 0xFFFF {
            return;
        }
    }

    let vga = m.vga().expect("VGA enabled");
    let (expected_lfb_base, expected_vram_size) = {
        let vga = vga.borrow();
        (vga.lfb_base(), vga.vram_size())
    };

    let bar0_base = {
        let pci_cfg = m.pci_config_ports().expect("pc platform enabled");
        let mut pci_cfg = pci_cfg.borrow_mut();
        let bus = pci_cfg.bus_mut();

        let vendor_id = bus.read_config(bdf, 0x00, 2) as u16;
        let device_id = bus.read_config(bdf, 0x02, 2) as u16;
        assert_eq!(vendor_id, 0x1234);
        assert_eq!(device_id, 0x1111);

        let class_reg = bus.read_config(bdf, 0x08, 4);
        let class_code = ((class_reg >> 24) & 0xFF) as u8;
        let subclass = ((class_reg >> 16) & 0xFF) as u8;
        assert_eq!(class_code, 0x03);
        assert_eq!(subclass, 0x00);

        let header_type = bus.read_config(bdf, 0x0E, 1) as u8;
        assert_eq!(header_type, 0x00);

        let bar0 = bus.read_config(bdf, 0x10, 4);
        let bar0_base = bar0 & 0xFFFF_FFF0;
        assert_eq!(bar0_base, expected_lfb_base);

        // BAR sizing probe: write all 1s then read back the size mask.
        bus.write_config(bdf, 0x10, 4, 0xFFFF_FFFF);
        let mask = bus.read_config(bdf, 0x10, 4);
        let size = u32::try_from(expected_vram_size).expect("VRAM size fits in u32");
        let expected_mask = !(size.saturating_sub(1)) & 0xFFFF_FFF0;
        assert_eq!(mask & 0xFFFF_FFF0, expected_mask);

        // Restore the original BAR base after probing so subsequent MMIO accesses are routed
        // correctly by the PCI MMIO window.
        bus.write_config(bdf, 0x10, 4, bar0);

        // Ensure MEM decoding is enabled for the MMIO router path (BIOS post normally does this,
        // but tests should be robust to future changes).
        let command = bus.read_config(bdf, 0x04, 2) as u16;
        if (command & 0x2) == 0 {
            bus.write_config(bdf, 0x04, 2, u32::from(command | 0x2));
        }

        u64::from(bar0_base)
    };

    // Program a VBE linear mode and confirm MMIO writes via BAR0 affect the visible framebuffer
    // (via the PCI MMIO router).
    m.io_write(0x01CE, 2, 0x0001);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0002);
    m.io_write(0x01CF, 2, 64);
    m.io_write(0x01CE, 2, 0x0003);
    m.io_write(0x01CF, 2, 32);
    m.io_write(0x01CE, 2, 0x0004);
    m.io_write(0x01CF, 2, 0x0041);

    // Write a red pixel at (0,0) in BGRX format via *machine memory* at the BAR0 base.
    m.write_physical_u8(bar0_base, 0x00); // B
    m.write_physical_u8(bar0_base + 1, 0x00); // G
    m.write_physical_u8(bar0_base + 2, 0xFF); // R
    m.write_physical_u8(bar0_base + 3, 0x00); // X

    let mut vga = vga.borrow_mut();
    vga.present();
    assert_eq!(vga.get_resolution(), (64, 64));
    assert_eq!(vga.get_framebuffer()[0], 0xFF00_00FF);
}
