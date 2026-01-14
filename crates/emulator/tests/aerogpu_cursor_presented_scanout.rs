use emulator::devices::aerogpu_regs::mmio;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission, AeroGpuCommandBackend,
};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::Bus;
use memory::MemoryBus;

#[derive(Clone, Debug)]
struct StaticScanoutBackend {
    scanout: AeroGpuBackendScanout,
}

impl AeroGpuCommandBackend for StaticScanoutBackend {
    fn reset(&mut self) {}

    fn submit(
        &mut self,
        _mem: &mut dyn MemoryBus,
        _submission: AeroGpuBackendSubmission,
    ) -> Result<(), String> {
        Ok(())
    }

    fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
        Vec::new()
    }

    fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
        Some(self.scanout.clone())
    }
}

#[test]
fn presented_scanout_includes_cursor_overlay() {
    let mut mem = Bus::new(0x1000);

    // 2x2 blue background.
    let blue = [0u8, 0, 255, 255];
    let scanout_rgba = blue.repeat(4);

    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vram_size_bytes = 2 * 1024 * 1024;
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    // Enable PCI MMIO decode + bus mastering so the device behaves like a real enumerated endpoint.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.set_backend(Box::new(StaticScanoutBackend {
        scanout: AeroGpuBackendScanout {
            width: 2,
            height: 2,
            rgba8: scanout_rgba,
        },
    }));

    // Cursor framebuffer: 1x1 red pixel at 50% alpha, stored as BGRA.
    let cursor_fb_gpa = 0x100u64;
    mem.write_physical(cursor_fb_gpa, &[0, 0, 255, 128]);

    // Program cursor registers.
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FB_GPA_LO,
        4,
        cursor_fb_gpa as u32,
    );
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FB_GPA_HI,
        4,
        (cursor_fb_gpa >> 32) as u32,
    );
    dev.mmio_write(&mut mem, mmio::CURSOR_PITCH_BYTES, 4, 4);
    dev.mmio_write(&mut mem, mmio::CURSOR_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FORMAT,
        4,
        AeroGpuFormat::B8G8R8A8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::CURSOR_HOT_X, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_HOT_Y, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_X, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_Y, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_ENABLE, 4, 1);

    let (_w, _h, rgba8) = dev
        .read_presented_scanout_rgba8(&mut mem, 0)
        .expect("scanout should be readable");

    // Cursor should blend over pixel (0,0): red@50% over blue => (128,0,127).
    assert_eq!(&rgba8[0..4], &[128, 0, 127, 255]);

    // Remaining pixels should remain blue.
    assert_eq!(&rgba8[4..8], &blue);
    assert_eq!(&rgba8[8..12], &blue);
    assert_eq!(&rgba8[12..16], &blue);
}

#[test]
fn presented_scanout_cursor_overlay_requires_bus_mastering() {
    let mut mem = Bus::new(0x1000);

    // 2x2 blue background.
    let blue = [0u8, 0, 255, 255];
    let scanout_rgba = blue.repeat(4);

    let mut cfg = AeroGpuDeviceConfig::default();
    cfg.vram_size_bytes = 2 * 1024 * 1024;
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    // Enable PCI MMIO decode so we can program cursor regs, but leave bus mastering disabled.
    dev.config_write(0x04, 2, 1 << 1);
    dev.set_backend(Box::new(StaticScanoutBackend {
        scanout: AeroGpuBackendScanout {
            width: 2,
            height: 2,
            rgba8: scanout_rgba,
        },
    }));

    // Cursor framebuffer: 1x1 red pixel at 50% alpha, stored as BGRA.
    let cursor_fb_gpa = 0x100u64;
    mem.write_physical(cursor_fb_gpa, &[0, 0, 255, 128]);

    // Program cursor registers.
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, cursor_fb_gpa as u32);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FB_GPA_HI,
        4,
        (cursor_fb_gpa >> 32) as u32,
    );
    dev.mmio_write(&mut mem, mmio::CURSOR_PITCH_BYTES, 4, 4);
    dev.mmio_write(&mut mem, mmio::CURSOR_WIDTH, 4, 1);
    dev.mmio_write(&mut mem, mmio::CURSOR_HEIGHT, 4, 1);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FORMAT,
        4,
        AeroGpuFormat::B8G8R8A8Unorm as u32,
    );
    dev.mmio_write(&mut mem, mmio::CURSOR_HOT_X, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_HOT_Y, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_X, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_Y, 4, 0);
    dev.mmio_write(&mut mem, mmio::CURSOR_ENABLE, 4, 1);

    let (_w, _h, rgba8) = dev
        .read_presented_scanout_rgba8(&mut mem, 0)
        .expect("scanout should be readable");

    // With COMMAND.BME clear, cursor DMA cannot run, so the presented scanout should remain blue.
    assert_eq!(&rgba8[0..4], &blue);
    assert_eq!(&rgba8[4..8], &blue);
    assert_eq!(&rgba8[8..12], &blue);
    assert_eq!(&rgba8[12..16], &blue);
}
