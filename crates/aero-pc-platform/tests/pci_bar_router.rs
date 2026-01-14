use aero_devices::pci::{
    PciBarDefinition, PciBdf, PciConfigSpace, PciDevice, PciResourceAllocatorConfig,
    PCI_CFG_ADDR_PORT, PCI_CFG_DATA_PORT,
};
use aero_pc_platform::PcPlatform;
use memory::MemoryBus as _;
use memory::MmioHandler;
use std::cell::RefCell;
use std::rc::Rc;

fn cfg_addr(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000
        | ((bus as u32) << 16)
        | ((device as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xFC)
}

fn write_cfg_u16(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 2, u32::from(value));
}

fn write_cfg_u32(pc: &mut PcPlatform, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
    pc.io.write(
        PCI_CFG_ADDR_PORT,
        4,
        cfg_addr(bus, device, function, offset),
    );
    pc.io.write(PCI_CFG_DATA_PORT, 4, value);
}

#[derive(Default)]
struct TestMmioDev {
    last_write: Option<(u64, usize, u64)>,
    mem: Vec<u8>,
}

impl TestMmioDev {
    fn new(size: usize) -> Self {
        Self {
            last_write: None,
            mem: vec![0; size],
        }
    }
}

impl MmioHandler for TestMmioDev {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        let mut buf = [0xffu8; 8];
        let base = offset as usize;
        for (i, dst) in buf.iter_mut().enumerate().take(size.min(8)) {
            if let Some(src) = self.mem.get(base + i) {
                *dst = *src;
            }
        }
        u64::from_le_bytes(buf)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.last_write = Some((offset, size, value));
        let bytes = value.to_le_bytes();
        let base = offset as usize;
        for (i, src) in bytes.iter().enumerate().take(size.min(8)) {
            if let Some(dst) = self.mem.get_mut(base + i) {
                *dst = *src;
            }
        }
    }
}

struct TestPciConfigDev {
    cfg: PciConfigSpace,
}

impl PciDevice for TestPciConfigDev {
    fn config(&self) -> &PciConfigSpace {
        &self.cfg
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.cfg
    }
}

#[test]
fn pci_bar_router_routes_mmio_and_tracks_bar_reprogramming() {
    let mut pc = PcPlatform::new(2 * 1024 * 1024);

    let alloc_cfg = PciResourceAllocatorConfig::default();

    // Pick a BDF that isn't used by the built-in platform devices (00:02.0 AHCI, 00:04.0 HDA,
    // 00:05.0 E1000, etc.) to avoid collisions if additional defaults are enabled.
    // Avoid `00:0c.0`, which is reserved by the historical Bochs/QEMU VGA stub contract.
    let bdf = PciBdf::new(0, 14, 0);

    // Add a PCI config-space function with a single 4KiB MMIO BAR0.
    let mut cfg = PciConfigSpace::new(0x1234, 0x5678);
    cfg.set_bar_definition(
        0,
        PciBarDefinition::Mmio32 {
            size: 0x1000,
            prefetchable: false,
        },
    );
    pc.pci_cfg
        .borrow_mut()
        .bus_mut()
        .add_device(bdf, Box::new(TestPciConfigDev { cfg }));

    // Register a MMIO handler for this BAR with the platform router.
    let dev = Rc::new(RefCell::new(TestMmioDev::new(0x1000)));
    pc.register_pci_mmio_bar_handler(bdf, 0, dev.clone());

    // Pick a base far enough into the MMIO window that it won't collide with the platform's
    // built-in PCI devices (which allocate BARs starting at `mmio_base` during BIOS POST).
    let base = alloc_cfg.mmio_base + 0x0100_0000;
    assert_eq!(base % 0x1000, 0);

    // Program BAR0 and enable memory decoding.
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x10,
        base as u32,
    );
    write_cfg_u16(&mut pc, bdf.bus, bdf.device, bdf.function, 0x04, 0x0002);

    pc.memory.write_u32(base + 0x40, 0xaabb_ccdd);
    assert_eq!(pc.memory.read_u32(base + 0x40), 0xaabb_ccdd);
    assert_eq!(dev.borrow().last_write.map(|w| w.0), Some(0x40));

    // Reprogram BAR0 to a new base within the same MMIO window.
    let new_base = base + 0x1000;
    write_cfg_u32(
        &mut pc,
        bdf.bus,
        bdf.device,
        bdf.function,
        0x10,
        new_base as u32,
    );

    // Old base no longer decodes (open bus).
    assert_eq!(pc.memory.read_u32(base + 0x40), 0xffff_ffff);

    // New base decodes and preserves handler state.
    assert_eq!(pc.memory.read_u32(new_base + 0x40), 0xaabb_ccdd);

    // Writes to the old base should be ignored.
    pc.memory.write_u32(base + 0x40, 0x1122_3344);
    assert_eq!(pc.memory.read_u32(new_base + 0x40), 0xaabb_ccdd);
}
