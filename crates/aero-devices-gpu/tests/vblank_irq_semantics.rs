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
    let mut mem = NoDmaMemory::default();
    let mut cfg = AeroGpuBar0MmioDeviceConfig::default();
    cfg.vblank_hz = Some(60);
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
