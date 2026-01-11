use aero_interrupts::pic8259::{DualPic8259, MASTER_CMD, MASTER_DATA, SLAVE_CMD, SLAVE_DATA};

/// Thin wrapper around [`DualPic8259`] that preserves the historical
/// `aero_platform::interrupts::Pic8259` API used by the platform interrupt
/// router and its tests.
#[derive(Debug, Clone)]
pub struct Pic8259 {
    inner: DualPic8259,
    master_base: u8,
    slave_base: u8,
}

impl Pic8259 {
    pub fn new(master_base: u8, slave_base: u8) -> Self {
        let mut pic = Self {
            inner: DualPic8259::new(),
            master_base,
            slave_base,
        };
        pic.set_offsets(master_base, slave_base);
        pic
    }

    pub fn set_offsets(&mut self, master_base: u8, slave_base: u8) {
        // Standard legacy PC PIC initialization sequence:
        // - ICW1: init, expect ICW4.
        // - ICW2: vector base.
        // - ICW3: master has slave on IRQ2, slave identity is 2.
        // - ICW4: 8086/88 mode.
        self.inner.port_write_u8(MASTER_CMD, 0x11);
        self.inner.port_write_u8(MASTER_DATA, master_base);
        self.inner.port_write_u8(MASTER_DATA, 0x04);
        self.inner.port_write_u8(MASTER_DATA, 0x01);

        self.inner.port_write_u8(SLAVE_CMD, 0x11);
        self.inner.port_write_u8(SLAVE_DATA, slave_base);
        self.inner.port_write_u8(SLAVE_DATA, 0x02);
        self.inner.port_write_u8(SLAVE_DATA, 0x01);

        self.master_base = master_base & 0xF8;
        self.slave_base = slave_base & 0xF8;
    }

    pub fn offsets(&self) -> (u8, u8) {
        (self.master_base, self.slave_base)
    }

    pub fn set_masked(&mut self, irq: u8, masked: bool) {
        let (port, bit) = match irq {
            0..=7 => (MASTER_DATA, 1u8 << irq),
            8..=15 => (SLAVE_DATA, 1u8 << (irq - 8)),
            _ => return,
        };

        let mut imr = self.inner.port_read_u8(port);
        if masked {
            imr |= bit;
        } else {
            imr &= !bit;
        }
        self.inner.port_write_u8(port, imr);
    }

    pub fn raise_irq(&mut self, irq: u8) {
        self.inner.raise_irq(irq);
    }

    pub fn lower_irq(&mut self, irq: u8) {
        self.inner.lower_irq(irq);
    }

    pub fn get_pending_vector(&self) -> Option<u8> {
        self.inner.get_pending_vector()
    }

    pub fn acknowledge(&mut self, vector: u8) {
        let _ = self.inner.acknowledge(vector);
    }

    pub fn eoi(&mut self, vector: u8) {
        let Some(irq) = self.vector_to_irq(vector) else {
            return;
        };

        if irq < 8 {
            self.inner
                .port_write_u8(MASTER_CMD, 0x60 | (irq & 0x07));
        } else {
            self.inner
                .port_write_u8(SLAVE_CMD, 0x60 | ((irq - 8) & 0x07));
            // Slave IRQs are cascaded through master IRQ2.
            self.inner.port_write_u8(MASTER_CMD, 0x60 | 0x02);
        }
    }

    pub fn vector_to_irq(&self, vector: u8) -> Option<u8> {
        let master_base = self.master_base;
        let slave_base = self.slave_base;

        if vector >= master_base && vector < master_base.wrapping_add(8) {
            return Some(vector.wrapping_sub(master_base));
        }
        if vector >= slave_base && vector < slave_base.wrapping_add(8) {
            return Some(8 + vector.wrapping_sub(slave_base));
        }

        None
    }

    pub fn port_read_u8(&self, port: u16) -> u8 {
        self.inner.port_read_u8(port)
    }

    pub fn port_write_u8(&mut self, port: u16, value: u8) {
        self.inner.port_write_u8(port, value);
    }
}
