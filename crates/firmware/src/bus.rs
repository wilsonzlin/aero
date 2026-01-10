use crate::devices::Devices;

pub trait Bus {
    fn read_u8(&mut self, paddr: u32) -> u8;
    fn write_u8(&mut self, paddr: u32, val: u8);

    /// A20 gate state exposed to firmware.
    ///
    /// When disabled, physical address bit 20 is forced low, aliasing addresses that differ
    /// only by bit 20 (e.g. `0x00000` and `0x1_00000`).
    fn a20_enabled(&self) -> bool;

    fn set_a20_enabled(&mut self, enabled: bool);

    fn io_read_u8(&mut self, port: u16) -> u8;
    fn io_write_u8(&mut self, port: u16, val: u8);

    fn serial_write(&mut self, byte: u8);

    fn read(&mut self, paddr: u32, buf: &mut [u8]) {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.read_u8(paddr.wrapping_add(i as u32));
        }
    }

    fn write(&mut self, paddr: u32, buf: &[u8]) {
        for (i, &b) in buf.iter().enumerate() {
            self.write_u8(paddr.wrapping_add(i as u32), b);
        }
    }

    fn read_u16(&mut self, paddr: u32) -> u16 {
        let lo = self.read_u8(paddr) as u16;
        let hi = self.read_u8(paddr.wrapping_add(1)) as u16;
        lo | (hi << 8)
    }

    fn read_u32(&mut self, paddr: u32) -> u32 {
        let b0 = self.read_u8(paddr) as u32;
        let b1 = self.read_u8(paddr.wrapping_add(1)) as u32;
        let b2 = self.read_u8(paddr.wrapping_add(2)) as u32;
        let b3 = self.read_u8(paddr.wrapping_add(3)) as u32;
        b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
    }

    fn read_u64(&mut self, paddr: u32) -> u64 {
        let lo = self.read_u32(paddr) as u64;
        let hi = self.read_u32(paddr.wrapping_add(4)) as u64;
        lo | (hi << 32)
    }

    fn write_u16(&mut self, paddr: u32, val: u16) {
        self.write_u8(paddr, val as u8);
        self.write_u8(paddr.wrapping_add(1), (val >> 8) as u8);
    }

    fn write_u32(&mut self, paddr: u32, val: u32) {
        self.write_u8(paddr, val as u8);
        self.write_u8(paddr.wrapping_add(1), (val >> 8) as u8);
        self.write_u8(paddr.wrapping_add(2), (val >> 16) as u8);
        self.write_u8(paddr.wrapping_add(3), (val >> 24) as u8);
    }

    fn write_u64(&mut self, paddr: u32, val: u64) {
        self.write_u32(paddr, val as u32);
        self.write_u32(paddr.wrapping_add(4), (val >> 32) as u32);
    }
}

/// Simple in-memory RAM with hard-coded IO/MMIO dispatch to core devices.
#[derive(Debug, Clone)]
pub struct TestBus {
    ram: Vec<u8>,
    serial: Vec<u8>,
    pub devices: Devices,
    a20_enabled: bool,
}

impl TestBus {
    pub fn new(ram_size: usize, devices: Devices) -> Self {
        Self {
            ram: vec![0; ram_size],
            serial: Vec::new(),
            devices,
            a20_enabled: true,
        }
    }

    pub fn ram_size(&self) -> usize {
        self.ram.len()
    }

    pub fn serial_output(&self) -> &[u8] {
        &self.serial
    }

    pub fn clear_serial(&mut self) {
        self.serial.clear();
    }
}

impl Bus for TestBus {
    fn read_u8(&mut self, paddr: u32) -> u8 {
        let paddr = self.filter_a20(paddr);
        let addr = paddr as usize;
        if addr < self.ram.len() {
            return self.ram[addr];
        }

        if let Some(byte) = self.devices.mmio_read_u8(paddr as u64) {
            return byte;
        }

        0
    }

    fn write_u8(&mut self, paddr: u32, val: u8) {
        let paddr = self.filter_a20(paddr);
        let addr = paddr as usize;
        if addr < self.ram.len() {
            self.ram[addr] = val;
            return;
        }

        if self.devices.mmio_write_u8(paddr as u64, val) {
            return;
        }
    }

    fn io_read_u8(&mut self, port: u16) -> u8 {
        self.devices.io_read_u8(port)
    }

    fn io_write_u8(&mut self, port: u16, val: u8) {
        self.devices.io_write_u8(port, val)
    }

    fn serial_write(&mut self, byte: u8) {
        self.serial.push(byte);
    }

    fn a20_enabled(&self) -> bool {
        self.a20_enabled
    }

    fn set_a20_enabled(&mut self, enabled: bool) {
        self.a20_enabled = enabled;
    }
}

impl TestBus {
    #[inline]
    fn filter_a20(&self, paddr: u32) -> u32 {
        if self.a20_enabled {
            paddr
        } else {
            paddr & !(1 << 20)
        }
    }
}
