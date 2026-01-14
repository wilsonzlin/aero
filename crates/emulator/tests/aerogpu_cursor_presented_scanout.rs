use std::collections::HashMap;

use emulator::devices::aerogpu_regs::mmio;
use emulator::devices::aerogpu_scanout::AeroGpuFormat;
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::gpu_worker::aerogpu_backend::{
    AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    AeroGpuCommandBackend,
};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::MemoryBus;

#[derive(Clone, Debug, Default)]
struct MapMemory {
    bytes: HashMap<u64, u8>,
}

impl MapMemory {
    fn write_bytes(&mut self, paddr: u64, data: &[u8]) {
        for (i, b) in data.iter().copied().enumerate() {
            self.bytes.insert(paddr + i as u64, b);
        }
    }
}

impl MemoryBus for MapMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        for (i, dst) in buf.iter_mut().enumerate() {
            *dst = *self.bytes.get(&(paddr + i as u64)).unwrap_or(&0);
        }
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        self.write_bytes(paddr, buf);
    }
}

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
    let mut mem = MapMemory::default();

    // 2x2 blue background.
    let blue = [0u8, 0, 255, 255];
    let scanout_rgba = blue.repeat(4);

    let cfg = AeroGpuDeviceConfig {
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
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

    // Cursor should blend over pixel (0,0): red@50% over blue => (128,0,127).
    assert_eq!(&rgba8[0..4], &[128, 0, 127, 255]);

    // Remaining pixels should remain blue.
    assert_eq!(&rgba8[4..8], &blue);
    assert_eq!(&rgba8[8..12], &blue);
    assert_eq!(&rgba8[12..16], &blue);
}

#[test]
fn presented_scanout_cursor_overlay_requires_bus_mastering() {
    let mut mem = MapMemory::default();

    // 2x2 blue background.
    let blue = [0u8, 0, 255, 255];
    let scanout_rgba = blue.repeat(4);

    let cfg = AeroGpuDeviceConfig {
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
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

#[test]
fn presented_scanout_cursor_fb_gpa_updates_deferred_until_hi_written() {
    let mut mem = MapMemory::default();

    // 1x1 blue background.
    let blue = [0u8, 0, 255, 255];
    let scanout_rgba = blue.repeat(1);

    let cfg = AeroGpuDeviceConfig {
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    // Enable PCI MMIO decode + bus mastering so cursor DMA is allowed.
    dev.config_write(0x04, 2, (1 << 1) | (1 << 2));
    dev.set_backend(Box::new(StaticScanoutBackend {
        scanout: AeroGpuBackendScanout {
            width: 1,
            height: 1,
            rgba8: scanout_rgba,
        },
    }));

    // Cursor framebuffer 0: 1x1 red pixel @ 50% alpha (BGRA), above 4GiB so HI dword is non-zero.
    let cursor_fb0 = 0x0000_0001_0000_0100u64;
    mem.write_physical(cursor_fb0, &[0, 0, 255, 128]);

    // Cursor framebuffer 1: 1x1 green pixel @ 50% alpha (BGRA), also above 4GiB.
    let cursor_fb1 = 0x0000_0002_0000_0200u64;
    mem.write_physical(cursor_fb1, &[0, 255, 0, 128]);

    // Program cursor registers for fb0.
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, cursor_fb0 as u32);
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FB_GPA_HI,
        4,
        (cursor_fb0 >> 32) as u32,
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
    // Red@50% over blue => (128,0,127).
    assert_eq!(&rgba8[0..4], &[128, 0, 127, 255]);

    // Start a cursor framebuffer address update by writing LO only. This must not expose a torn
    // address to the cursor readback path; cursor should still read from fb0.
    dev.mmio_write(&mut mem, mmio::CURSOR_FB_GPA_LO, 4, cursor_fb1 as u32);
    let (_w, _h, rgba8_after_lo) = dev
        .read_presented_scanout_rgba8(&mut mem, 0)
        .expect("scanout should be readable");
    assert_eq!(&rgba8_after_lo[0..4], &[128, 0, 127, 255]);

    // Commit the address update by writing HI. Cursor should now read from fb1.
    dev.mmio_write(
        &mut mem,
        mmio::CURSOR_FB_GPA_HI,
        4,
        (cursor_fb1 >> 32) as u32,
    );
    let (_w, _h, rgba8_after_hi) = dev
        .read_presented_scanout_rgba8(&mut mem, 0)
        .expect("scanout should be readable");
    // Green@50% over blue => (0,128,127).
    assert_eq!(&rgba8_after_hi[0..4], &[0, 128, 127, 255]);
}
