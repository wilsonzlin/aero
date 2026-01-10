#[derive(Debug, Clone)]
pub struct Tsc {
    freq_hz: u64,
    invariant: bool,
    aux: u32,
    base_guest_ns: u64,
    base_tsc: u64,
}

impl Tsc {
    pub fn new(freq_hz: u64) -> Self {
        Self {
            freq_hz,
            invariant: true,
            aux: 0,
            base_guest_ns: 0,
            base_tsc: 0,
        }
    }

    pub fn freq_hz(&self) -> u64 {
        self.freq_hz
    }

    pub fn invariant(&self) -> bool {
        self.invariant
    }

    pub fn set_invariant(&mut self, invariant: bool) {
        self.invariant = invariant;
    }

    pub fn aux(&self) -> u32 {
        self.aux
    }

    pub fn set_aux(&mut self, aux: u32) {
        self.aux = aux;
    }

    pub fn read(&self, guest_now_ns: u64) -> u64 {
        let delta_ns = guest_now_ns.saturating_sub(self.base_guest_ns);
        let delta_tsc = ((delta_ns as u128) * (self.freq_hz as u128)) / 1_000_000_000u128;
        self.base_tsc.wrapping_add(delta_tsc as u64)
    }

    pub fn read_rdtscp(&self, guest_now_ns: u64) -> (u64, u32) {
        (self.read(guest_now_ns), self.aux)
    }

    pub fn write(&mut self, guest_now_ns: u64, tsc_value: u64) {
        self.base_guest_ns = guest_now_ns;
        self.base_tsc = tsc_value;
    }

    pub fn guest_ns_for_tsc(&self, target_tsc: u64) -> Option<u64> {
        if target_tsc < self.base_tsc {
            return None;
        }
        let delta_tsc = target_tsc - self.base_tsc;
        if self.freq_hz == 0 {
            return None;
        }
        let numer = (delta_tsc as u128) * 1_000_000_000u128;
        let denom = self.freq_hz as u128;
        let delta_ns = (numer + denom - 1) / denom;
        Some(self.base_guest_ns.saturating_add(delta_ns as u64))
    }
}
