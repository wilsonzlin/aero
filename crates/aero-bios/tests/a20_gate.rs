use std::cell::{Cell, RefCell};
use std::rc::Rc;

use aero_bios::firmware::{A20Gate, BlockDevice, DiskError, Memory, NullKeyboard};
use aero_bios::{Bios, BiosConfig, RealModeCpu};

struct VecMemory {
    bytes: Vec<u8>,
}

impl VecMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

impl Memory for VecMemory {
    fn read_u8(&self, paddr: u32) -> u8 {
        self.bytes[paddr as usize]
    }

    fn read_u16(&self, paddr: u32) -> u16 {
        let lo = self.read_u8(paddr) as u16;
        let hi = self.read_u8(paddr + 1) as u16;
        lo | (hi << 8)
    }

    fn read_u32(&self, paddr: u32) -> u32 {
        let b0 = self.read_u8(paddr) as u32;
        let b1 = self.read_u8(paddr + 1) as u32;
        let b2 = self.read_u8(paddr + 2) as u32;
        let b3 = self.read_u8(paddr + 3) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn write_u8(&mut self, paddr: u32, v: u8) {
        self.bytes[paddr as usize] = v;
    }

    fn write_u16(&mut self, paddr: u32, v: u16) {
        self.write_u8(paddr, v as u8);
        self.write_u8(paddr + 1, (v >> 8) as u8);
    }

    fn write_u32(&mut self, paddr: u32, v: u32) {
        self.write_u8(paddr, v as u8);
        self.write_u8(paddr + 1, (v >> 8) as u8);
        self.write_u8(paddr + 2, (v >> 16) as u8);
        self.write_u8(paddr + 3, (v >> 24) as u8);
    }
}

struct BootSectorDisk {
    sector: [u8; 512],
}

impl BootSectorDisk {
    fn new() -> Self {
        let mut sector = [0u8; 512];
        sector[510] = 0x55;
        sector[511] = 0xAA;
        Self { sector }
    }
}

impl BlockDevice for BootSectorDisk {
    fn read_sector(&mut self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        if lba != 0 {
            return Err(DiskError::OutOfRange);
        }
        buf512.copy_from_slice(&self.sector);
        Ok(())
    }

    fn write_sector(&mut self, _lba: u64, _buf512: &[u8; 512]) -> Result<(), DiskError> {
        Err(DiskError::ReadOnly)
    }

    fn sector_count(&self) -> u64 {
        1
    }

    fn is_read_only(&self) -> bool {
        true
    }
}

#[derive(Clone)]
struct TestA20Gate {
    enabled: Rc<Cell<bool>>,
    events: Rc<RefCell<Vec<bool>>>,
}

impl TestA20Gate {
    fn new() -> (Self, Rc<Cell<bool>>, Rc<RefCell<Vec<bool>>>) {
        let enabled = Rc::new(Cell::new(false));
        let events = Rc::new(RefCell::new(Vec::new()));
        (
            Self {
                enabled: enabled.clone(),
                events: events.clone(),
            },
            enabled,
            events,
        )
    }
}

impl A20Gate for TestA20Gate {
    fn a20_enabled(&self) -> bool {
        self.enabled.get()
    }

    fn set_a20_enabled(&mut self, enabled: bool) {
        self.enabled.set(enabled);
        self.events.borrow_mut().push(enabled);
    }
}

#[test]
fn bios_post_enables_a20_via_gate_hook() {
    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let (gate, enabled, events) = TestA20Gate::new();
    bios.set_a20_gate(Box::new(gate));

    let mut cpu = RealModeCpu::default();
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut disk = BootSectorDisk::new();

    bios.post(&mut cpu, &mut mem, &mut disk);

    assert!(enabled.get(), "POST should enable A20");
    assert_eq!(events.borrow().as_slice(), &[true]);
}

#[test]
fn int15_a20_services_route_through_gate_hook() {
    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let (gate, enabled, events) = TestA20Gate::new();
    bios.set_a20_gate(Box::new(gate));

    let mut cpu = RealModeCpu::default();
    let mut mem = VecMemory::new(2 * 1024 * 1024);
    let mut disk = BootSectorDisk::new();
    let mut kbd = NullKeyboard;

    cpu.set_ax(0x2402);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert_eq!(cpu.al(), 0);

    cpu.set_ax(0x2401);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert!(enabled.get());

    cpu.set_ax(0x2400);
    bios.handle_interrupt(0x15, &mut cpu, &mut mem, &mut disk, &mut kbd);
    assert!(!cpu.cf());
    assert!(!enabled.get());

    assert_eq!(events.borrow().as_slice(), &[true, false]);
}

