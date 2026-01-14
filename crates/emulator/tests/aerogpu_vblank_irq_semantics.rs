use emulator::devices::aerogpu_regs::{irq_bits, mmio};
use emulator::devices::pci::aerogpu::{AeroGpuDeviceConfig, AeroGpuPciDevice};
use emulator::io::pci::{MmioDevice, PciDevice};
use memory::MemoryBus;

#[derive(Clone, Debug, Default)]
struct NoDmaMemory;

impl MemoryBus for NoDmaMemory {
    fn read_physical(&mut self, _paddr: u64, _buf: &mut [u8]) {
        panic!("unexpected DMA read while bus mastering disabled");
    }

    fn write_physical(&mut self, _paddr: u64, _buf: &[u8]) {
        panic!("unexpected DMA write while bus mastering disabled");
    }
}

fn enable_mmio_decode_only(dev: &mut AeroGpuPciDevice) {
    // Enable PCI MMIO decoding (COMMAND.MEM=1) but keep bus mastering disabled so `tick` and
    // the IRQ-enable catch-up path cannot DMA into our dummy memory bus.
    dev.config_write(0x04, 2, 1 << 1);
}

#[test]
fn vblank_irq_status_not_latched_while_masked_or_on_reenable() {
    let mut mem = NoDmaMemory;
    // Keep the interval comfortably above typical test runtime jitter so the "not immediately"
    // assertions don't become timing-sensitive.
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(60),
        // Keep VRAM small for tests (the device allocates BAR1 VRAM backing).
        vram_size_bytes: 2 * 1024 * 1024,
        ..Default::default()
    };
    let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
    enable_mmio_decode_only(&mut dev);

    // Enable scanout to start the free-running vblank clock.
    dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

    let period_ns = u64::from(dev.regs.scanout0_vblank_period_ns);
    assert_ne!(period_ns, 0, "test requires vblank pacing to be enabled");

    // Arm the scheduler with a next-vblank deadline that is already in the past. This simulates
    // the device not being ticked for a while (host stall / guest paused).
    let now = period_ns * 10;
    dev.tick(&mut mem, now - period_ns * 3);

    // Enable vblank IRQ delivery. The device must catch up its vblank clock *before* enabling
    // IRQ latching so that old vblanks while masked do not immediately appear as a pending IRQ.
    dev.mmio_write(&mut mem, mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK);

    // The IRQ status bit must not be set until the next vblank edge *after* the enable.
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);

    // An immediate tick at the current time should *not* produce a pending IRQ (the next vblank
    // deadline should be in the future).
    let after_enable = now;
    dev.tick(&mut mem, after_enable);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(
        dev.regs.scanout0_vblank_seq > 0,
        "vblank counters must advance even when IRQ delivery is masked"
    );

    // The first tick that crosses the next vblank edge should latch the IRQ status bit.
    dev.tick(&mut mem, after_enable + period_ns);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
}
