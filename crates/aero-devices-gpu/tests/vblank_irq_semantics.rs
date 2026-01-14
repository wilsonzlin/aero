use aero_devices_gpu::device::{AeroGpuBar0MmioDevice, AeroGpuBar0MmioDeviceConfig};
use aero_devices_gpu::regs::{irq_bits, mmio};
use memory::MemoryBus;

#[derive(Clone, Debug, Default)]
struct NoDmaMemory;

impl MemoryBus for NoDmaMemory {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read while DMA is disabled");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write while DMA is disabled");
    }
}

#[test]
fn vblank_irq_status_not_latched_while_masked_or_on_reenable() {
    let mut mem = NoDmaMemory;
    let cfg = AeroGpuBar0MmioDeviceConfig {
        vblank_hz: Some(60),
        ..Default::default()
    };
    let mut dev = AeroGpuBar0MmioDevice::new(cfg);

    // Enable scanout to start the free-running vblank clock.
    let t0_ns = 0u64;
    dev.mmio_write_dword(&mut mem, t0_ns, false, mmio::SCANOUT0_ENABLE, 1);

    let period_ns = dev.regs.scanout0_vblank_period_ns;
    assert_ne!(period_ns, 0, "test requires vblank pacing to be enabled");
    let period_ns = period_ns as u64;

    // Prime the vblank scheduler so `next_vblank` is defined, then let simulated time advance
    // across several vblank edges without ticking the device.
    dev.tick(&mut mem, t0_ns, false);

    let enable_time_ns = t0_ns + period_ns * 3;

    // Enable vblank IRQ delivery. The device must catch up its vblank clock *before* enabling
    // IRQ latching so that old vblanks while masked do not immediately appear as a pending IRQ.
    dev.mmio_write_dword(
        &mut mem,
        enable_time_ns,
        false,
        mmio::IRQ_ENABLE,
        irq_bits::SCANOUT_VBLANK,
    );

    // Even though the vblank counter advanced during catch-up, the IRQ status bit must not be
    // set until the next vblank edge *after* the enable.
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(
        dev.regs.scanout0_vblank_seq > 0,
        "vblank counters must advance even when IRQ delivery is masked"
    );
    assert!(!dev.irq_level());

    // An immediate tick at the enable time should *not* produce a pending IRQ (the next vblank
    // deadline should be in the future).
    dev.tick(&mut mem, enable_time_ns, false);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // The first tick that crosses the next vblank edge should latch the IRQ status bit.
    dev.tick(&mut mem, enable_time_ns + period_ns, false);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}

#[test]
fn scanout_fb_gpa_updates_are_atomic_for_readback() {
    let mut mem = NoDmaMemory;
    let cfg = AeroGpuBar0MmioDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };
    let mut dev = AeroGpuBar0MmioDevice::new(cfg);

    // Start from a stable committed framebuffer address.
    let fb0 = 0x1111_2222_3333_4444u64;
    dev.mmio_write_dword(&mut mem, 0, false, mmio::SCANOUT0_FB_GPA_LO, fb0 as u32);
    dev.mmio_write_dword(
        &mut mem,
        0,
        false,
        mmio::SCANOUT0_FB_GPA_HI,
        (fb0 >> 32) as u32,
    );
    assert_eq!(dev.regs.scanout0.fb_gpa, fb0);
    assert_eq!(dev.mmio_read_dword(mmio::SCANOUT0_FB_GPA_LO), fb0 as u32);
    assert_eq!(
        dev.mmio_read_dword(mmio::SCANOUT0_FB_GPA_HI),
        (fb0 >> 32) as u32
    );

    // LO-only write must not immediately commit the full 64-bit GPA. Reads should still be able to
    // observe the pending LO dword (read-your-writes), while the internal committed GPA remains
    // stable until the HI commit.
    let fb1 = 0x9999_AAAA_BBBB_CCCDu64;
    dev.mmio_write_dword(&mut mem, 0, false, mmio::SCANOUT0_FB_GPA_LO, fb1 as u32);
    assert_eq!(dev.regs.scanout0.fb_gpa, fb0);
    assert_eq!(dev.mmio_read_dword(mmio::SCANOUT0_FB_GPA_LO), fb1 as u32);
    assert_eq!(
        dev.mmio_read_dword(mmio::SCANOUT0_FB_GPA_HI),
        (fb0 >> 32) as u32
    );

    dev.mmio_write_dword(
        &mut mem,
        0,
        false,
        mmio::SCANOUT0_FB_GPA_HI,
        (fb1 >> 32) as u32,
    );
    assert_eq!(dev.regs.scanout0.fb_gpa, fb1);
    assert_eq!(dev.mmio_read_dword(mmio::SCANOUT0_FB_GPA_LO), fb1 as u32);
    assert_eq!(
        dev.mmio_read_dword(mmio::SCANOUT0_FB_GPA_HI),
        (fb1 >> 32) as u32
    );
}

#[test]
fn cursor_fb_gpa_updates_are_atomic_for_readback() {
    let mut mem = NoDmaMemory;
    let cfg = AeroGpuBar0MmioDeviceConfig {
        vblank_hz: None,
        ..Default::default()
    };
    let mut dev = AeroGpuBar0MmioDevice::new(cfg);

    // Start from a stable committed framebuffer address.
    let fb0 = 0x5555_6666_7777_8888u64;
    dev.mmio_write_dword(&mut mem, 0, false, mmio::CURSOR_FB_GPA_LO, fb0 as u32);
    dev.mmio_write_dword(
        &mut mem,
        0,
        false,
        mmio::CURSOR_FB_GPA_HI,
        (fb0 >> 32) as u32,
    );
    assert_eq!(dev.regs.cursor.fb_gpa, fb0);
    assert_eq!(dev.mmio_read_dword(mmio::CURSOR_FB_GPA_LO), fb0 as u32);
    assert_eq!(
        dev.mmio_read_dword(mmio::CURSOR_FB_GPA_HI),
        (fb0 >> 32) as u32
    );

    // LO-only write must not immediately commit the full 64-bit GPA. Reads should still be able to
    // observe the pending LO dword (read-your-writes), while the internal committed GPA remains
    // stable until the HI commit.
    let fb1 = 0x0123_4567_89AB_CDEFu64;
    dev.mmio_write_dword(&mut mem, 0, false, mmio::CURSOR_FB_GPA_LO, fb1 as u32);
    assert_eq!(dev.regs.cursor.fb_gpa, fb0);
    assert_eq!(dev.mmio_read_dword(mmio::CURSOR_FB_GPA_LO), fb1 as u32);
    assert_eq!(
        dev.mmio_read_dword(mmio::CURSOR_FB_GPA_HI),
        (fb0 >> 32) as u32
    );

    dev.mmio_write_dword(
        &mut mem,
        0,
        false,
        mmio::CURSOR_FB_GPA_HI,
        (fb1 >> 32) as u32,
    );
    assert_eq!(dev.regs.cursor.fb_gpa, fb1);
    assert_eq!(dev.mmio_read_dword(mmio::CURSOR_FB_GPA_LO), fb1 as u32);
    assert_eq!(
        dev.mmio_read_dword(mmio::CURSOR_FB_GPA_HI),
        (fb1 >> 32) as u32
    );
}
