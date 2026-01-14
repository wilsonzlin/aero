use aero_devices::pci::PciDevice;
use aero_devices_gpu::{irq_bits, mmio, AeroGpuDeviceConfig, AeroGpuPciDevice};
use memory::{MemoryBus, MmioHandler};

#[derive(Clone, Debug)]
struct VecMemory {
    data: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            data: vec![0; size],
        }
    }

    fn range(&self, paddr: u64, len: usize) -> std::ops::Range<usize> {
        let start = usize::try_from(paddr).expect("paddr too large");
        let end = start.checked_add(len).expect("address wrap");
        assert!(end <= self.data.len(), "out-of-bounds physical access");
        start..end
    }
}

impl MemoryBus for VecMemory {
    fn read_physical(&mut self, paddr: u64, buf: &mut [u8]) {
        let range = self.range(paddr, buf.len());
        buf.copy_from_slice(&self.data[range]);
    }

    fn write_physical(&mut self, paddr: u64, buf: &[u8]) {
        let range = self.range(paddr, buf.len());
        self.data[range].copy_from_slice(buf);
    }
}

fn new_test_device(cfg: AeroGpuDeviceConfig) -> AeroGpuPciDevice {
    let mut dev = AeroGpuPciDevice::new(cfg);
    // Enable PCI MMIO decode + bus mastering so MMIO and DMA paths behave like a real enumerated
    // device (guests must set COMMAND.MEM/BME before touching BARs).
    dev.config_mut().set_command((1 << 1) | (1 << 2));
    dev
}

#[test]
fn vblank_irq_enable_disable_has_no_stale_interrupt() {
    let cfg = AeroGpuDeviceConfig {
        vblank_hz: Some(10),
        ..Default::default()
    };
    let mut mem = VecMemory::new(0x1000);
    let mut dev = new_test_device(cfg);

    // Enable scanout so vblank ticks run.
    dev.write(mmio::SCANOUT0_ENABLE, 4, 1);

    // Prime the vblank scheduler and advance time across multiple vblank edges with vblank IRQs
    // disabled; vblank ticks must not latch IRQ_STATUS while masked.
    let t0_ns = 0u64;
    let period_ns = dev.regs.scanout0_vblank_period_ns as u64;
    assert_ne!(period_ns, 0, "test requires vblank pacing to be enabled");

    dev.write(mmio::IRQ_ENABLE, 4, 0);
    dev.tick(&mut mem, t0_ns);
    dev.tick(&mut mem, t0_ns + period_ns * 2);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // Enable vblank IRQ delivery: enabling must *not* immediately assert due to an old vblank.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK as u64);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // Tick before the next vblank edge to clear the one-tick enable suppression window.
    dev.tick(&mut mem, t0_ns + period_ns * 2 + period_ns / 2);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // The next *future* vblank should latch IRQ_STATUS and assert INTx.
    dev.tick(&mut mem, t0_ns + period_ns * 3);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());

    // ACK the vblank to clear IRQ_STATUS.
    dev.write(mmio::IRQ_ACK, 4, irq_bits::SCANOUT_VBLANK as u64);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // Disable vblank IRQ delivery; future vblank ticks must not latch IRQ_STATUS.
    dev.write(mmio::IRQ_ENABLE, 4, 0);
    assert_eq!(dev.regs.irq_enable & irq_bits::SCANOUT_VBLANK, 0);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    dev.tick(&mut mem, t0_ns + period_ns * 5);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // Re-enable vblank IRQ delivery; this must not immediately assert due to an old vblank that
    // occurred while disabled.
    dev.write(mmio::IRQ_ENABLE, 4, irq_bits::SCANOUT_VBLANK as u64);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    // Tick before the next vblank edge to clear the enable suppression window, then ensure the
    // following vblank latches IRQ_STATUS.
    dev.tick(&mut mem, t0_ns + period_ns * 5 + period_ns / 2);
    assert_eq!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(!dev.irq_level());

    dev.tick(&mut mem, t0_ns + period_ns * 6);
    assert_ne!(dev.regs.irq_status & irq_bits::SCANOUT_VBLANK, 0);
    assert!(dev.irq_level());
}
