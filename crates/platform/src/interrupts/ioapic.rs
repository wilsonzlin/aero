#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerMode {
    Edge,
    Level,
}

#[derive(Debug, Clone, Copy)]
pub struct IoApicDelivery {
    pub gsi: u32,
    pub vector: u8,
    pub dest: u8,
}

#[derive(Debug, Clone, Copy)]
pub struct IoApicRedirectionEntry {
    pub vector: u8,
    pub dest: u8,
    pub masked: bool,
    pub trigger: TriggerMode,
    remote_irr: bool,
}

impl IoApicRedirectionEntry {
    pub fn fixed(vector: u8, dest: u8) -> Self {
        Self {
            vector,
            dest,
            masked: true,
            trigger: TriggerMode::Edge,
            remote_irr: false,
        }
    }

    fn to_low_dword(self) -> u32 {
        let mut value = self.vector as u32;
        if self.trigger == TriggerMode::Level {
            value |= 1 << 15;
        }
        if self.masked {
            value |= 1 << 16;
        }
        if self.remote_irr {
            value |= 1 << 14;
        }
        value
    }

    fn to_high_dword(self) -> u32 {
        (self.dest as u32) << 24
    }
}

#[derive(Debug, Clone)]
pub struct IoApic {
    select: u8,
    entries: Vec<IoApicRedirectionEntry>,
    line_asserted: Vec<bool>,
}

impl IoApic {
    pub fn new(num_pins: usize) -> Self {
        Self {
            select: 0,
            entries: (0..num_pins)
                .map(|_| IoApicRedirectionEntry::fixed(0, 0))
                .collect(),
            line_asserted: vec![false; num_pins],
        }
    }

    pub fn num_pins(&self) -> usize {
        self.entries.len()
    }

    pub fn entry(&self, gsi: u32) -> Option<IoApicRedirectionEntry> {
        self.entries.get(gsi as usize).copied()
    }

    pub fn set_entry(&mut self, gsi: u32, entry: IoApicRedirectionEntry) -> Vec<IoApicDelivery> {
        let Some(slot) = self.entries.get_mut(gsi as usize) else {
            return Vec::new();
        };

        let prev_masked = slot.masked;
        *slot = IoApicRedirectionEntry {
            remote_irr: slot.remote_irr,
            ..entry
        };

        if prev_masked && !slot.masked {
            return self.sync_gsi(gsi);
        }

        Vec::new()
    }

    pub fn set_line(&mut self, gsi: u32, asserted: bool) -> Vec<IoApicDelivery> {
        let idx = gsi as usize;
        if idx >= self.line_asserted.len() {
            return Vec::new();
        }

        let was_asserted = self.line_asserted[idx];
        self.line_asserted[idx] = asserted;
        self.on_line_change(gsi, was_asserted, asserted)
    }

    pub fn sync(&mut self) -> Vec<IoApicDelivery> {
        let mut deliveries = Vec::new();
        for gsi in 0..self.entries.len() as u32 {
            deliveries.extend(self.sync_gsi(gsi));
        }
        deliveries
    }

    pub fn eoi(&mut self, gsi: u32) -> Vec<IoApicDelivery> {
        let Some(entry) = self.entries.get_mut(gsi as usize) else {
            return Vec::new();
        };

        if entry.trigger == TriggerMode::Level {
            entry.remote_irr = false;
        }

        self.sync_gsi(gsi)
    }

    /// Notify the IOAPIC that the CPU issued an EOI for `vector`.
    ///
    /// For level-triggered interrupts, IOAPICs track "Remote-IRR" per redirection entry.
    /// A LAPIC EOI clears Remote-IRR for entries that match the EOI vector, allowing
    /// level-triggered lines that remain asserted to be re-delivered.
    pub fn notify_eoi(&mut self, vector: u8) -> Vec<IoApicDelivery> {
        let mut deliveries = Vec::new();

        for (gsi, entry) in self.entries.iter_mut().enumerate() {
            if entry.trigger != TriggerMode::Level {
                continue;
            }
            if !entry.remote_irr {
                continue;
            }
            if entry.vector != vector {
                continue;
            }

            entry.remote_irr = false;

            if self.line_asserted[gsi] && !entry.masked {
                entry.remote_irr = true;
                deliveries.push(IoApicDelivery {
                    gsi: gsi as u32,
                    vector: entry.vector,
                    dest: entry.dest,
                });
            }
        }

        deliveries
    }

    pub fn mmio_read(&self, offset: u64) -> u32 {
        match offset {
            0x00 => self.select as u32,
            0x10 => self.read_register(self.select),
            _ => 0,
        }
    }

    pub fn mmio_write(&mut self, offset: u64, value: u32) -> Vec<IoApicDelivery> {
        match offset {
            0x00 => {
                self.select = (value & 0xFF) as u8;
                Vec::new()
            }
            0x10 => self.write_register(self.select, value),
            _ => Vec::new(),
        }
    }

    fn read_register(&self, register: u8) -> u32 {
        match register {
            0x00 => 0, // IOAPICID: not currently configurable.
            0x01 => {
                let max_redir = self.entries.len().saturating_sub(1) as u32;
                0x11 | (max_redir << 16)
            }
            reg if reg >= 0x10 => {
                let reg = reg - 0x10;
                let entry_index = (reg / 2) as usize;
                let hi = (reg % 2) == 1;
                if let Some(entry) = self.entries.get(entry_index).copied() {
                    if hi {
                        entry.to_high_dword()
                    } else {
                        entry.to_low_dword()
                    }
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    fn write_register(&mut self, register: u8, value: u32) -> Vec<IoApicDelivery> {
        match register {
            reg if reg >= 0x10 => {
                let reg = reg - 0x10;
                let entry_index = (reg / 2) as usize;
                let hi = (reg % 2) == 1;
                if entry_index >= self.entries.len() {
                    return Vec::new();
                }

                if hi {
                    self.entries[entry_index].dest = (value >> 24) as u8;
                    Vec::new()
                } else {
                    let vector = (value & 0xFF) as u8;
                    let trigger = if (value & (1 << 15)) != 0 {
                        TriggerMode::Level
                    } else {
                        TriggerMode::Edge
                    };
                    let masked = (value & (1 << 16)) != 0;

                    let entry = &mut self.entries[entry_index];
                    let prev_masked = entry.masked;
                    entry.vector = vector;
                    entry.trigger = trigger;
                    entry.masked = masked;

                    if prev_masked && !entry.masked {
                        return self.sync_gsi(entry_index as u32);
                    }
                    Vec::new()
                }
            }
            _ => Vec::new(),
        }
    }

    fn on_line_change(
        &mut self,
        gsi: u32,
        was_asserted: bool,
        asserted: bool,
    ) -> Vec<IoApicDelivery> {
        let Some(entry) = self.entries.get_mut(gsi as usize) else {
            return Vec::new();
        };
        if entry.masked {
            return Vec::new();
        }

        match entry.trigger {
            TriggerMode::Edge => {
                if !was_asserted && asserted {
                    return vec![IoApicDelivery {
                        gsi,
                        vector: entry.vector,
                        dest: entry.dest,
                    }];
                }
            }
            TriggerMode::Level => {
                if asserted && !entry.remote_irr {
                    entry.remote_irr = true;
                    return vec![IoApicDelivery {
                        gsi,
                        vector: entry.vector,
                        dest: entry.dest,
                    }];
                }
            }
        }

        Vec::new()
    }

    fn sync_gsi(&mut self, gsi: u32) -> Vec<IoApicDelivery> {
        let idx = gsi as usize;
        if idx >= self.entries.len() {
            return Vec::new();
        }
        let asserted = self.line_asserted[idx];
        let entry = &mut self.entries[idx];

        if entry.masked {
            return Vec::new();
        }

        match entry.trigger {
            TriggerMode::Edge => Vec::new(),
            TriggerMode::Level => {
                if asserted && !entry.remote_irr {
                    entry.remote_irr = true;
                    vec![IoApicDelivery {
                        gsi,
                        vector: entry.vector,
                        dest: entry.dest,
                    }]
                } else {
                    Vec::new()
                }
            }
        }
    }
}
