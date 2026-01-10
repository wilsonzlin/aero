use aero_bios::firmware::{BlockDevice, DiskError, Memory, NullKeyboard, PciConfigSpace};
use aero_bios::{Bios, BiosConfig, RealModeCpu};

struct SimpleMemory {
    bytes: Vec<u8>,
}

impl SimpleMemory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
        }
    }
}

impl Memory for SimpleMemory {
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

struct VecDisk {
    bytes: Vec<u8>,
}

impl VecDisk {
    fn new(mut bytes: Vec<u8>) -> Self {
        assert_eq!(bytes.len() % 512, 0);
        if bytes.len() < 512 {
            bytes.resize(512, 0);
        }
        bytes[510] = 0x55;
        bytes[511] = 0xAA;
        Self { bytes }
    }
}

impl BlockDevice for VecDisk {
    fn read_sector(&mut self, lba: u64, buf512: &mut [u8; 512]) -> Result<(), DiskError> {
        let start = usize::try_from(lba * 512).map_err(|_| DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self.bytes.get(start..end).ok_or(DiskError::OutOfRange)?;
        buf512.copy_from_slice(slice);
        Ok(())
    }

    fn write_sector(&mut self, lba: u64, buf512: &[u8; 512]) -> Result<(), DiskError> {
        let start = usize::try_from(lba * 512).map_err(|_| DiskError::OutOfRange)?;
        let end = start.checked_add(512).ok_or(DiskError::OutOfRange)?;
        let slice = self
            .bytes
            .get_mut(start..end)
            .ok_or(DiskError::OutOfRange)?;
        slice.copy_from_slice(buf512);
        Ok(())
    }

    fn sector_count(&self) -> u64 {
        (self.bytes.len() / 512) as u64
    }
}

#[derive(Clone, Debug)]
struct DevCfg {
    id: u32,
    class: u32,
    header: u32,
    reg_3c: u32,
}

struct TestPciCfg {
    devs: Vec<(u8, u8, u8, DevCfg)>,
}

impl TestPciCfg {
    fn new() -> Self {
        Self { devs: Vec::new() }
    }

    fn add_dev(&mut self, bus: u8, device: u8, function: u8, vendor: u16, dev_id: u16, pin: u8) {
        let id = u32::from(dev_id) << 16 | u32::from(vendor);
        let reg_3c = u32::from(pin) << 8;
        self.devs.push((
            bus,
            device,
            function,
            DevCfg {
                id,
                class: 0,
                header: 0,
                reg_3c,
            },
        ));
    }

    fn dev_mut(&mut self, bus: u8, device: u8, function: u8) -> Option<&mut DevCfg> {
        self.devs
            .iter_mut()
            .find_map(|(b, d, f, cfg)| (*b == bus && *d == device && *f == function).then_some(cfg))
    }
}

impl PciConfigSpace for TestPciCfg {
    fn read_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8) -> u32 {
        let Some(cfg) = self.dev_mut(bus, device, function) else {
            return 0xFFFF_FFFF;
        };

        match offset {
            0x00 => cfg.id,
            0x08 => cfg.class,
            0x0C => cfg.header,
            0x3C => cfg.reg_3c,
            _ => 0,
        }
    }

    fn write_config_dword(&mut self, bus: u8, device: u8, function: u8, offset: u8, value: u32) {
        let Some(cfg) = self.dev_mut(bus, device, function) else {
            return;
        };

        if offset == 0x3C {
            cfg.reg_3c = value;
        }
    }
}

#[test]
fn bios_programs_pci_interrupt_line_using_intx_swizzle_and_pirq_map() {
    let mut mem = SimpleMemory::new(2 * 1024 * 1024);
    let mut cpu = RealModeCpu::default();
    let mut disk = VecDisk::new(vec![0; 512]);

    let mut pci = TestPciCfg::new();
    // Bus 0, device 1, function 0, INTA#.
    pci.add_dev(0, 1, 0, 0x1234, 0x5678, 1);
    // Bus 0, device 2, function 0, INTD#.
    pci.add_dev(0, 2, 0, 0x1234, 0x9999, 4);

    let mut bios = Bios::new(BiosConfig {
        enable_acpi: false,
        ..BiosConfig::default()
    });
    let mut kbd = NullKeyboard;

    bios.post_with_devices(&mut cpu, &mut mem, &mut disk, &mut kbd, Some(&mut pci));

    // With PIRQ[A-D] -> [10,11,12,13] and swizzle (pin+device)%4:
    // dev1 INTA (pin=0) -> index 1 -> 11.
    let dev1 = pci.dev_mut(0, 1, 0).unwrap();
    assert_eq!(dev1.reg_3c & 0xFF, 11);
    assert_eq!((dev1.reg_3c >> 8) & 0xFF, 1); // pin preserved

    // dev2 INTD (pin=3) -> index (2+3)%4=1 -> 11.
    let dev2 = pci.dev_mut(0, 2, 0).unwrap();
    assert_eq!(dev2.reg_3c & 0xFF, 11);
    assert_eq!((dev2.reg_3c >> 8) & 0xFF, 4); // pin preserved

    // BIOS bookkeeping should reflect the assigned line too.
    let seen = bios
        .pci_devices()
        .iter()
        .map(|d| (d.bus, d.device, d.function, d.irq_line))
        .collect::<Vec<_>>();
    assert!(seen.contains(&(0, 1, 0, 11)));
    assert!(seen.contains(&(0, 2, 0, 11)));
}
