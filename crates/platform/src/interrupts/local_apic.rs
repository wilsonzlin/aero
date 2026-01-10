#[derive(Debug, Clone)]
pub struct LocalApic {
    apic_id: u32,
    irr: [u32; 8],
}

impl LocalApic {
    pub fn new(apic_id: u32) -> Self {
        Self {
            apic_id,
            irr: [0; 8],
        }
    }

    pub fn apic_id(&self) -> u32 {
        self.apic_id
    }

    pub fn inject_vector(&mut self, vector: u8) {
        let (idx, bit) = Self::idx_bit(vector);
        self.irr[idx] |= 1u32 << bit;
    }

    pub fn is_pending(&self, vector: u8) -> bool {
        let (idx, bit) = Self::idx_bit(vector);
        (self.irr[idx] & (1u32 << bit)) != 0
    }

    pub fn take_pending(&mut self) -> Option<u8> {
        for vector in (0u16..=255u16).rev() {
            let vector = vector as u8;
            if self.is_pending(vector) {
                self.clear_vector(vector);
                return Some(vector);
            }
        }

        None
    }

    pub fn acknowledge_vector(&mut self, vector: u8) {
        self.clear_vector(vector);
    }

    pub fn pending_vectors(&self) -> Vec<u8> {
        let mut vectors = Vec::new();
        for vector in 0u16..=255u16 {
            let vector = vector as u8;
            if self.is_pending(vector) {
                vectors.push(vector);
            }
        }
        vectors
    }

    fn clear_vector(&mut self, vector: u8) {
        let (idx, bit) = Self::idx_bit(vector);
        self.irr[idx] &= !(1u32 << bit);
    }

    fn idx_bit(vector: u8) -> (usize, u8) {
        let idx = (vector / 32) as usize;
        let bit = vector % 32;
        (idx, bit)
    }
}
