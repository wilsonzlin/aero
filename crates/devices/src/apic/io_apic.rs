use crate::apic::LapicInterruptSink;
use std::sync::Arc;

pub const IOAPIC_MMIO_BASE: u64 = 0xFEC0_0000;
pub const IOAPIC_MMIO_SIZE: u64 = 0x1000;

/// I/O APIC ID (4-bit field in the ID register).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoApicId(pub u8);

impl IoApicId {
    fn as_reg_bits(self) -> u32 {
        u32::from(self.0 & 0x0f) << 24
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TriggerMode {
    Edge,
    Level,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RedirectionEntry {
    vector: u8,
    delivery_mode: u8,
    destination_mode: bool,
    polarity_low: bool,
    trigger_mode: TriggerMode,
    mask: bool,
    destination: u8,
    remote_irr: bool,
}

impl Default for RedirectionEntry {
    fn default() -> Self {
        Self {
            vector: 0,
            delivery_mode: 0,
            destination_mode: false,
            polarity_low: false,
            trigger_mode: TriggerMode::Edge,
            mask: true,
            destination: 0,
            remote_irr: false,
        }
    }
}

impl RedirectionEntry {
    fn read_low(self) -> u32 {
        let mut v = 0u32;
        v |= u32::from(self.vector);
        v |= u32::from(self.delivery_mode & 0x7) << 8;
        v |= u32::from(self.destination_mode) << 11;
        v |= u32::from(self.polarity_low) << 13;
        v |= u32::from(self.remote_irr) << 14;
        v |= match self.trigger_mode {
            TriggerMode::Edge => 0,
            TriggerMode::Level => 1,
        } << 15;
        v |= u32::from(self.mask) << 16;
        v
    }

    fn read_high(self) -> u32 {
        u32::from(self.destination) << 24
    }

    fn write_low(&mut self, v: u32) -> MaskTransition {
        let old_mask = self.mask;

        self.vector = (v & 0xff) as u8;
        self.delivery_mode = ((v >> 8) & 0x7) as u8;
        self.destination_mode = ((v >> 11) & 0x1) != 0;
        self.polarity_low = ((v >> 13) & 0x1) != 0;
        self.trigger_mode = if ((v >> 15) & 0x1) != 0 {
            TriggerMode::Level
        } else {
            TriggerMode::Edge
        };
        self.mask = ((v >> 16) & 0x1) != 0;

        match (old_mask, self.mask) {
            (true, false) => MaskTransition::Unmasked,
            (false, true) => MaskTransition::Masked,
            _ => MaskTransition::Unchanged,
        }
    }

    fn write_high(&mut self, v: u32) {
        self.destination = (v >> 24) as u8;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MaskTransition {
    Unchanged,
    Masked,
    Unmasked,
}

/// Minimal IOAPIC model implementing the MMIO programming interface (`IOREGSEL` + `IOWIN`)
/// and routing interrupts to a [`LapicInterruptSink`].
pub struct IoApic {
    id: IoApicId,
    ioregsel: u8,
    redirection: Vec<RedirectionEntry>,
    irq_level: Vec<bool>,
    lapic: Arc<dyn LapicInterruptSink>,
}

impl IoApic {
    pub const NUM_REDIRECTION_ENTRIES: usize = 24;

    pub fn new(id: IoApicId, lapic: Arc<dyn LapicInterruptSink>) -> Self {
        Self::with_entries(id, lapic, Self::NUM_REDIRECTION_ENTRIES)
    }

    pub fn with_entries(id: IoApicId, lapic: Arc<dyn LapicInterruptSink>, entries: usize) -> Self {
        Self {
            id,
            ioregsel: 0,
            redirection: vec![RedirectionEntry::default(); entries],
            irq_level: vec![false; entries],
            lapic,
        }
    }

    /// Read from the IOAPIC MMIO window. Only 32-bit accesses are supported.
    pub fn mmio_read(&mut self, offset: u64, size: usize) -> u64 {
        if size != 4 {
            return 0;
        }

        let v = match offset {
            0x00 => u32::from(self.ioregsel),
            0x10 => self.read_register(self.ioregsel),
            _ => 0,
        };
        u64::from(v)
    }

    /// Write into the IOAPIC MMIO window. Only 32-bit accesses are supported.
    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        if size != 4 {
            return;
        }

        let v = value as u32;
        match offset {
            0x00 => self.ioregsel = (v & 0xff) as u8,
            0x10 => self.write_register(self.ioregsel, v),
            _ => {}
        }
    }

    fn read_register(&self, reg: u8) -> u32 {
        match reg {
            0x00 => self.id.as_reg_bits(),
            0x01 => {
                let max = (self.redirection.len() - 1) as u32;
                0x11 | (max << 16)
            }
            0x02 => self.id.as_reg_bits(),
            0x10..=0xff => {
                let idx = reg.wrapping_sub(0x10) as usize;
                let entry = idx / 2;
                if entry >= self.redirection.len() {
                    return 0;
                }
                if idx % 2 == 0 {
                    self.redirection[entry].read_low()
                } else {
                    self.redirection[entry].read_high()
                }
            }
            _ => 0,
        }
    }

    fn write_register(&mut self, reg: u8, v: u32) {
        match reg {
            0x00 => self.id = IoApicId(((v >> 24) & 0x0f) as u8),
            0x10..=0xff => {
                let idx = reg.wrapping_sub(0x10) as usize;
                let entry = idx / 2;
                if entry >= self.redirection.len() {
                    return;
                }

                if idx % 2 == 0 {
                    let transition = self.redirection[entry].write_low(v);
                    if transition == MaskTransition::Unmasked {
                        self.maybe_deliver_level(entry as u32);
                    }
                } else {
                    self.redirection[entry].write_high(v);
                }
            }
            _ => {}
        }
    }

    /// Update the input IRQ line level for a given GSI.
    ///
    /// The `level` parameter is the *asserted* state (true = asserted/high, false = deasserted/low),
    /// not an electrical voltage level. This mirrors common virtual interrupt wiring: device models
    /// don't need to care about IOAPIC polarity configuration.
    pub fn set_irq_level(&mut self, gsi: u32, level: bool) {
        let Some(prev_level) = self.irq_level.get_mut(gsi as usize) else {
            return;
        };

        let prev = *prev_level;
        *prev_level = level;

        if gsi as usize >= self.redirection.len() {
            return;
        }
        let entry = self.redirection[gsi as usize];

        if entry.mask {
            return;
        }

        match entry.trigger_mode {
            TriggerMode::Edge => {
                if !prev && level {
                    self.deliver(gsi);
                }
            }
            TriggerMode::Level => {
                if level {
                    self.maybe_deliver_level(gsi);
                } else if let Some(entry) = self.redirection.get_mut(gsi as usize) {
                    // Real hardware clears Remote-IRR on EOI from the LAPIC, but in early
                    // versions of the emulator we may not have a full EOI feedback path.
                    // Clearing on deassert is a pragmatic approximation that:
                    // - prevents storms while the line is held asserted
                    // - allows re-delivery after a deassert/reassert cycle even without EOI
                    entry.remote_irr = false;
                }
            }
        }
    }

    /// Notify the IOAPIC that a LAPIC has issued an EOI for `vector`.
    ///
    /// This is used to model Remote-IRR handling for level-triggered interrupts.
    pub fn notify_eoi(&mut self, vector: u8) {
        let mut pending_redelivery = Vec::new();

        for (gsi, entry) in self.redirection.iter_mut().enumerate() {
            if entry.trigger_mode != TriggerMode::Level {
                continue;
            }
            if !entry.remote_irr {
                continue;
            }
            if entry.vector != vector {
                continue;
            }

            entry.remote_irr = false;

            if self.irq_level[gsi] && !entry.mask {
                pending_redelivery.push(gsi as u32);
            }
        }

        for gsi in pending_redelivery {
            self.deliver(gsi);
        }
    }

    fn maybe_deliver_level(&mut self, gsi: u32) {
        let entry = &mut self.redirection[gsi as usize];
        if entry.trigger_mode != TriggerMode::Level {
            return;
        }
        if entry.mask || entry.remote_irr || !self.irq_level[gsi as usize] {
            return;
        }

        self.deliver(gsi);
    }

    fn deliver(&mut self, gsi: u32) {
        let entry = &mut self.redirection[gsi as usize];

        // Fixed + physical destination routing only for now.
        if entry.delivery_mode != 0 || entry.destination_mode {
            return;
        }

        if entry.trigger_mode == TriggerMode::Level {
            entry.remote_irr = true;
        }

        self.lapic.inject_external_interrupt(entry.vector);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apic::LocalApic;

    fn mk_ioapic() -> (IoApic, Arc<LocalApic>) {
        let lapic = Arc::new(LocalApic::new(0));
        let ioapic = IoApic::new(IoApicId(0), lapic.clone());
        (ioapic, lapic)
    }

    #[test]
    fn mmio_regsel_iowin_id_ver_redtbl() {
        let (mut ioapic, _lapic) = mk_ioapic();

        // Read ID.
        ioapic.mmio_write(0x00, 4, 0x00);
        assert_eq!(ioapic.mmio_read(0x10, 4) as u32, 0x00);

        // Write ID and read back (only bits 24..27 are writable).
        ioapic.mmio_write(0x10, 4, 0x12_34_56_78);
        ioapic.mmio_write(0x00, 4, 0x00);
        assert_eq!(ioapic.mmio_read(0x10, 4) as u32, 0x02_00_00_00);

        // Read VER: version 0x11, max redir entry 23 for 24-entry IOAPIC.
        ioapic.mmio_write(0x00, 4, 0x01);
        assert_eq!(ioapic.mmio_read(0x10, 4) as u32, 0x0017_0011);

        // Program redirection entry 0 (reg 0x10 low, 0x11 high).
        ioapic.mmio_write(0x00, 4, 0x10);
        ioapic.mmio_write(0x10, 4, 0x20); // vector 0x20, unmasked edge by default? mask bit is 0 here.
        ioapic.mmio_write(0x00, 4, 0x10);
        assert_eq!(ioapic.mmio_read(0x10, 4) as u32 & 0xff, 0x20);

        ioapic.mmio_write(0x00, 4, 0x11);
        ioapic.mmio_write(0x10, 4, 0x01 << 24);
        ioapic.mmio_write(0x00, 4, 0x11);
        assert_eq!(ioapic.mmio_read(0x10, 4) as u32, 0x01 << 24);
    }

    #[test]
    fn edge_triggered_delivers_once_per_rising_edge() {
        let (mut ioapic, lapic) = mk_ioapic();

        // Unmask entry 0, vector 0x20, edge triggered (default).
        ioapic.mmio_write(0x00, 4, 0x10);
        ioapic.mmio_write(0x10, 4, 0x20);

        ioapic.set_irq_level(0, true);
        assert_eq!(lapic.pop_pending(), Some(0x20));

        // Still asserted: should not re-deliver.
        ioapic.set_irq_level(0, true);
        assert_eq!(lapic.pop_pending(), None);

        // Deassert then assert again: new edge.
        ioapic.set_irq_level(0, false);
        ioapic.set_irq_level(0, true);
        assert_eq!(lapic.pop_pending(), Some(0x20));
    }

    #[test]
    fn level_triggered_delivers_without_storming() {
        let (mut ioapic, lapic) = mk_ioapic();

        // Entry 1, vector 0x21, level triggered, unmasked.
        ioapic.mmio_write(0x00, 4, 0x12); // low dword of entry 1
        ioapic.mmio_write(0x10, 4, 0x21 | (1 << 15)); // trigger_mode=level

        ioapic.set_irq_level(1, true);
        assert_eq!(lapic.pop_pending(), Some(0x21));

        // Still asserted: no re-delivery without EOI.
        ioapic.set_irq_level(1, true);
        assert_eq!(lapic.pop_pending(), None);

        // Deassert then assert: should deliver again.
        ioapic.set_irq_level(1, false);
        ioapic.set_irq_level(1, true);
        assert_eq!(lapic.pop_pending(), Some(0x21));

        // Now emulate EOI while still asserted; should re-deliver (remote IRR cleared).
        ioapic.notify_eoi(0x21);
        assert_eq!(lapic.pop_pending(), Some(0x21));
    }
}
