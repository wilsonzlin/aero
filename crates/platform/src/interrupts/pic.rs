#[derive(Debug, Clone)]
struct PicUnit {
    base_vector: u8,
    irr: u8,
    isr: u8,
    imr: u8,
    line_level: u8,
}

impl PicUnit {
    fn new(base_vector: u8) -> Self {
        Self {
            base_vector,
            irr: 0,
            isr: 0,
            imr: 0,
            line_level: 0,
        }
    }

    fn set_base_vector(&mut self, base_vector: u8) {
        self.base_vector = base_vector;
    }

    fn raise_irq(&mut self, irq: u8) {
        debug_assert!(irq < 8);
        let bit = 1u8 << irq;
        let was_high = (self.line_level & bit) != 0;
        self.line_level |= bit;
        if !was_high {
            self.irr |= bit;
        }
    }

    fn lower_irq(&mut self, irq: u8) {
        debug_assert!(irq < 8);
        self.line_level &= !(1u8 << irq);
    }

    fn set_masked(&mut self, irq: u8, masked: bool) {
        debug_assert!(irq < 8);
        let bit = 1u8 << irq;
        if masked {
            self.imr |= bit;
        } else {
            self.imr &= !bit;
        }
    }

    fn pending_unmasked(&self) -> u8 {
        self.irr & !self.imr
    }

    fn highest_in_service_irq(&self) -> Option<u8> {
        lowest_set_bit(self.isr)
    }

    fn resolve_pending_irq(&self, request: u8) -> Option<u8> {
        let mut request = request;

        if let Some(in_service) = self.highest_in_service_irq() {
            request &= (1u8 << in_service).wrapping_sub(1);
        }

        lowest_set_bit(request)
    }

    fn acknowledge_irq(&mut self, irq: u8) {
        debug_assert!(irq < 8);
        let bit = 1u8 << irq;
        self.irr &= !bit;
        self.isr |= bit;
    }

    fn eoi_irq(&mut self, irq: u8) {
        debug_assert!(irq < 8);
        self.isr &= !(1u8 << irq);
    }
}

#[derive(Debug, Clone)]
pub struct Pic8259 {
    master: PicUnit,
    slave: PicUnit,
}

impl Pic8259 {
    pub fn new(master_base: u8, slave_base: u8) -> Self {
        Self {
            master: PicUnit::new(master_base),
            slave: PicUnit::new(slave_base),
        }
    }

    pub fn set_offsets(&mut self, master_base: u8, slave_base: u8) {
        self.master.set_base_vector(master_base);
        self.slave.set_base_vector(slave_base);
    }

    pub fn offsets(&self) -> (u8, u8) {
        (self.master.base_vector, self.slave.base_vector)
    }

    pub fn set_masked(&mut self, irq: u8, masked: bool) {
        match irq {
            0..=7 => self.master.set_masked(irq, masked),
            8..=15 => self.slave.set_masked(irq - 8, masked),
            _ => {}
        }
    }

    pub fn raise_irq(&mut self, irq: u8) {
        match irq {
            0..=7 => self.master.raise_irq(irq),
            8..=15 => self.slave.raise_irq(irq - 8),
            _ => {}
        }
    }

    pub fn lower_irq(&mut self, irq: u8) {
        match irq {
            0..=7 => self.master.lower_irq(irq),
            8..=15 => self.slave.lower_irq(irq - 8),
            _ => {}
        }
    }

    pub fn get_pending_vector(&self) -> Option<u8> {
        let slave_pending = self.slave.pending_unmasked();
        let mut master_request = self.master.pending_unmasked();
        if slave_pending != 0 && (self.master.imr & (1 << 2)) == 0 {
            master_request |= 1 << 2;
        }

        let master_irq = self.master.resolve_pending_irq(master_request)?;
        if master_irq == 2 && slave_pending != 0 {
            let slave_irq = self.slave.resolve_pending_irq(slave_pending)?;
            return Some(self.slave.base_vector.wrapping_add(slave_irq));
        }

        Some(self.master.base_vector.wrapping_add(master_irq))
    }

    pub fn acknowledge(&mut self, vector: u8) {
        if let Some(irq) = self.vector_to_irq(vector) {
            if irq < 8 {
                self.master.acknowledge_irq(irq);
            } else {
                self.slave.acknowledge_irq(irq - 8);
                self.master.acknowledge_irq(2);
            }
        }
    }

    pub fn eoi(&mut self, vector: u8) {
        if let Some(irq) = self.vector_to_irq(vector) {
            if irq < 8 {
                self.master.eoi_irq(irq);
            } else {
                self.slave.eoi_irq(irq - 8);
                self.master.eoi_irq(2);
            }
        }
    }

    pub fn vector_to_irq(&self, vector: u8) -> Option<u8> {
        let master_base = self.master.base_vector;
        let slave_base = self.slave.base_vector;

        if vector >= master_base && vector < master_base.wrapping_add(8) {
            return Some(vector.wrapping_sub(master_base));
        }
        if vector >= slave_base && vector < slave_base.wrapping_add(8) {
            return Some(8 + vector.wrapping_sub(slave_base));
        }

        None
    }
}

fn lowest_set_bit(bits: u8) -> Option<u8> {
    if bits == 0 {
        return None;
    }

    Some(bits.trailing_zeros() as u8)
}
