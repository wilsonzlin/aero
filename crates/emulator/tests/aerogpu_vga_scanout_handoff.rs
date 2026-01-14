use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use aero_gpu_vga::{VBE_DISPI_DATA_PORT, VBE_DISPI_INDEX_PORT};
use aero_shared::scanout_state::{
    ScanoutState, SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_LEGACY_VBE_LFB, SCANOUT_SOURCE_WDDM,
};
use emulator::devices::aerogpu_regs::mmio;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::{MmioDevice as _, PciDevice as _};
use memory::Bus;
use memory::MmioHandler;

struct Bar1VramMmio {
    dev: Rc<RefCell<AeroGpuPciDevice>>,
}

impl MmioHandler for Bar1VramMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.dev.borrow_mut().vram_mmio_read(offset, size)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.dev.borrow_mut().vram_mmio_write(offset, size, value)
    }
}

#[test]
fn scanout_state_hands_off_from_legacy_vbe_to_wddm_and_locks_out_legacy() {
    let bar1_base = 0xE000_0000u32;
    let vram_size = 2 * 1024 * 1024u32;

    let dev = Rc::new(RefCell::new(AeroGpuPciDevice::new(
        AeroGpuDeviceConfig {
            vram_size_bytes: vram_size,
            ..Default::default()
        },
        0,
        bar1_base,
    )));

    let scanout = Arc::new(ScanoutState::new());
    dev.borrow_mut().set_scanout_state(Some(scanout.clone()));

    // Enable PCI I/O + memory decoding:
    // - I/O decoding is required for VGA/VBE port access.
    // - memory decoding is required for BAR1 accesses and BAR0 scanout programming.
    dev.borrow_mut().config_write(0x04, 2, (1 << 0) | (1 << 1));

    // Map BAR1 into a simple physical bus so we can "write pixels to the LFB" via guest physical
    // addresses.
    let mut mem = Bus::new(0);
    mem.map_mmio(
        u64::from(bar1_base),
        u64::from(vram_size),
        Box::new(Bar1VramMmio { dev: dev.clone() }),
    );

    // Power-on/reset state: legacy text.
    let snap0 = scanout.snapshot();
    assert_eq!(snap0.source, SCANOUT_SOURCE_LEGACY_TEXT);

    // Program a Bochs VBE mode (64x64x32bpp, LFB enabled).
    {
        let mut d = dev.borrow_mut();
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 64);
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0002);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 64);
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0003);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 32);
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0004);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 0x0041);
    }

    // Write a red pixel at (0,0) in BGRX format via the BAR1-mapped LFB.
    let lfb_base = dev.borrow().bar1_lfb_base();
    mem.write(
        lfb_base,
        4,
        u64::from(u32::from_le_bytes([0x00, 0x00, 0xFF, 0x00])),
    );

    let snap1 = scanout.snapshot();
    assert_eq!(snap1.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
    assert_eq!(snap1.base_paddr(), lfb_base);
    assert_eq!(snap1.width, 64);
    assert_eq!(snap1.height, 64);
    assert_eq!(snap1.pitch_bytes, 64 * 4);

    // Program the WDDM scanout registers (BAR0) and enable scanout.
    let wddm_fb_gpa = 0x1234_0000u64;
    {
        let mut d = dev.borrow_mut();
        d.mmio_write(&mut mem, mmio::SCANOUT0_FB_GPA_LO, 4, wddm_fb_gpa as u32);
        d.mmio_write(
            &mut mem,
            mmio::SCANOUT0_FB_GPA_HI,
            4,
            (wddm_fb_gpa >> 32) as u32,
        );
        d.mmio_write(&mut mem, mmio::SCANOUT0_WIDTH, 4, 128);
        d.mmio_write(&mut mem, mmio::SCANOUT0_HEIGHT, 4, 96);
        d.mmio_write(&mut mem, mmio::SCANOUT0_PITCH_BYTES, 4, 128 * 4);
        d.mmio_write(
            &mut mem,
            mmio::SCANOUT0_FORMAT,
            4,
            AeroGpuFormat::B8G8R8X8Unorm as u32,
        );
        d.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);
    }

    let snap2 = scanout.snapshot();
    assert_eq!(snap2.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap2.base_paddr(), wddm_fb_gpa);
    assert_eq!(snap2.width, 128);
    assert_eq!(snap2.height, 96);

    // Legacy writes must not steal scanout ownership once WDDM has enabled scanout.
    {
        let mut d = dev.borrow_mut();
        // Attempt to switch to a different legacy VBE mode.
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 800);
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0002);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 600);
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0003);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 32);
        d.vga_port_write(VBE_DISPI_INDEX_PORT, 2, 0x0004);
        d.vga_port_write(VBE_DISPI_DATA_PORT, 2, 0x0041);
    }
    // Attempt to scribble the legacy LFB.
    mem.write(
        lfb_base,
        4,
        u64::from(u32::from_le_bytes([0x00, 0xFF, 0x00, 0x00])),
    );

    let snap3 = scanout.snapshot();
    assert_eq!(snap3.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap3.base_paddr(), wddm_fb_gpa);
    assert_eq!(snap3.generation, snap2.generation);

    // Disabling scanout must blank output but keep WDDM ownership sticky.
    {
        let mut d = dev.borrow_mut();
        d.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 0);
    }
    let snap4 = scanout.snapshot();
    assert_eq!(snap4.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap4.base_paddr(), 0);
    assert_eq!(snap4.width, 0);
    assert_eq!(snap4.height, 0);

    // Legacy writes still must not steal scanout ownership while WDDM is blanked.
    {
        let mut d = dev.borrow_mut();
        // Attempt to switch to a different legacy VBE mode.
        d.vga_port_write(0x01CE, 2, 0x0001);
        d.vga_port_write(0x01CF, 2, 800);
        d.vga_port_write(0x01CE, 2, 0x0002);
        d.vga_port_write(0x01CF, 2, 600);
        d.vga_port_write(0x01CE, 2, 0x0003);
        d.vga_port_write(0x01CF, 2, 32);
        d.vga_port_write(0x01CE, 2, 0x0004);
        d.vga_port_write(0x01CF, 2, 0x0041);
    }
    // Attempt to scribble the legacy LFB.
    mem.write(
        lfb_base,
        4,
        u64::from(u32::from_le_bytes([0x00, 0xFF, 0x00, 0x00])),
    );

    let snap5 = scanout.snapshot();
    assert_eq!(snap5.source, SCANOUT_SOURCE_WDDM);
    assert_eq!(snap5.generation, snap4.generation);
}
