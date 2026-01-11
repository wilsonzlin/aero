use crate::apic::LapicInterruptSink;
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
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
    fn interpret_level(self, raw_level: bool) -> bool {
        if self.polarity_low {
            !raw_level
        } else {
            raw_level
        }
    }

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
    /// Physical wiring polarity for each input pin (true = active-low).
    ///
    /// This models the *board wiring* (e.g. typical PC PCI INTx + SCI lines are active-low),
    /// independent of how the guest programs the redirection table's polarity bit.
    pin_active_low: Vec<bool>,
    /// Raw electrical level on each IOAPIC input pin (true = high).
    pin_level: Vec<bool>,
    lapic: Arc<dyn LapicInterruptSink>,
}

impl IoApic {
    pub const NUM_REDIRECTION_ENTRIES: usize = 24;

    pub fn new(id: IoApicId, lapic: Arc<dyn LapicInterruptSink>) -> Self {
        Self::with_entries(id, lapic, Self::NUM_REDIRECTION_ENTRIES)
    }

    pub fn with_entries(id: IoApicId, lapic: Arc<dyn LapicInterruptSink>, entries: usize) -> Self {
        let mut pin_active_low = vec![false; entries];
        for (gsi, active_low) in pin_active_low.iter_mut().enumerate() {
            // Default PC wiring:
            // - ISA IRQs are typically active-high (except SCI via ACPI ISO)
            // - PCI INTx lines are active-low (our default ACPI _PRT routes them to GSIs 10-13)
            *active_low = gsi == 9 || (10..=13).contains(&gsi) || gsi >= 16;
        }
        // Initialise pins to the deasserted electrical level for the assumed wiring.
        let pin_level = pin_active_low.clone();
        Self {
            id,
            ioregsel: 0,
            redirection: vec![RedirectionEntry::default(); entries],
            pin_active_low,
            pin_level,
            lapic,
        }
    }

    pub fn num_redirection_entries(&self) -> usize {
        self.redirection.len()
    }

    pub fn set_pin_active_low(&mut self, gsi: u32, active_low: bool) {
        if let Some(slot) = self.pin_active_low.get_mut(gsi as usize) {
            *slot = active_low;
        }
        if let Some(level) = self.pin_level.get_mut(gsi as usize) {
            // Keep the pin deasserted when adjusting wiring assumptions.
            *level = active_low;
        }
    }

    /// Clears Remote-IRR for all redirection entries.
    ///
    /// Remote-IRR is only meaningful once the IOAPIC has delivered a level-triggered interrupt
    /// to a LAPIC. If the guest programs the IOAPIC while the platform is still routing
    /// interrupts through the legacy PIC (or delivery is otherwise suppressed), Remote-IRR may
    /// not reflect a real in-service interrupt; callers can clear it when switching to APIC
    /// delivery.
    pub fn clear_remote_irr(&mut self) {
        for entry in &mut self.redirection {
            entry.remote_irr = false;
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
    /// The `level` parameter is the *asserted* state from the device's perspective.
    ///
    /// Internally, the IOAPIC converts this to a raw electrical pin level using a fixed per-pin
    /// polarity (`pin_active_low`), and then applies the *guest-programmable* redirection table
    /// polarity bit when deciding whether the interrupt is asserted (mirroring real hardware).
    pub fn set_irq_level(&mut self, gsi: u32, level: bool) {
        let idx = gsi as usize;
        let Some(prev_level) = self.pin_level.get_mut(idx) else {
            return;
        };

        let prev_raw = *prev_level;
        let active_low = *self.pin_active_low.get(idx).unwrap_or(&false);
        let new_raw = level != active_low;
        *prev_level = new_raw;

        if idx >= self.redirection.len() {
            return;
        }
        let entry = self.redirection[idx];

        let prev = entry.interpret_level(prev_raw);
        let asserted = entry.interpret_level(new_raw);

        match entry.trigger_mode {
            TriggerMode::Edge => {
                if entry.mask {
                    return;
                }
                if !prev && asserted {
                    self.deliver(gsi);
                }
            }
            TriggerMode::Level => {
                if entry.mask {
                    return;
                }
                if asserted {
                    self.maybe_deliver_level(gsi);
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

            if entry.interpret_level(self.pin_level[gsi]) && !entry.mask {
                pending_redelivery.push(gsi as u32);
            }
        }

        for gsi in pending_redelivery {
            self.deliver(gsi);
        }
    }

    fn maybe_deliver_level(&mut self, gsi: u32) {
        let idx = gsi as usize;
        let entry = self.redirection[idx];
        if entry.trigger_mode != TriggerMode::Level {
            return;
        }
        if entry.mask || entry.remote_irr || !entry.interpret_level(self.pin_level[idx]) {
            return;
        }

        self.deliver(gsi);
    }

    /// Scans for asserted level-triggered lines and delivers interrupts that are not blocked by
    /// Remote-IRR.
    pub fn sync_level_triggered(&mut self) {
        for gsi in 0..self.redirection.len() {
            self.maybe_deliver_level(gsi as u32);
        }
    }

    fn deliver(&mut self, gsi: u32) {
        let entry = &mut self.redirection[gsi as usize];

        // Fixed + physical destination routing only for now.
        if entry.delivery_mode != 0 || entry.destination_mode {
            return;
        }

        if entry.destination != self.lapic.apic_id() {
            return;
        }

        if entry.trigger_mode == TriggerMode::Level {
            entry.remote_irr = true;
        }

        self.lapic.inject_external_interrupt(entry.vector);
    }
}

impl IoSnapshot for IoApic {
    const DEVICE_ID: [u8; 4] = *b"IOAP";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_ID: u16 = 1;
        const TAG_IOREGSEL: u16 = 2;
        const TAG_REDIRECTION: u16 = 3;
        const TAG_PIN_ACTIVE_LOW: u16 = 4;
        const TAG_PIN_LEVEL: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u8(TAG_ID, self.id.0 & 0x0f);
        w.field_u8(TAG_IOREGSEL, self.ioregsel);

        let mut redir = Encoder::new().u32(self.redirection.len() as u32);
        for entry in &self.redirection {
            let trigger = match entry.trigger_mode {
                TriggerMode::Edge => 0u8,
                TriggerMode::Level => 1u8,
            };
            redir = redir
                .u8(entry.vector)
                .u8(entry.delivery_mode)
                .bool(entry.destination_mode)
                .bool(entry.polarity_low)
                .u8(trigger)
                .bool(entry.mask)
                .u8(entry.destination)
                .bool(entry.remote_irr);
        }
        w.field_bytes(TAG_REDIRECTION, redir.finish());

        let pin_active_low: Vec<u8> = self
            .pin_active_low
            .iter()
            .map(|val| if *val { 1 } else { 0 })
            .collect();
        w.field_bytes(TAG_PIN_ACTIVE_LOW, Encoder::new().vec_u8(&pin_active_low).finish());

        let pin_level: Vec<u8> = self.pin_level.iter().map(|val| if *val { 1 } else { 0 }).collect();
        w.field_bytes(TAG_PIN_LEVEL, Encoder::new().vec_u8(&pin_level).finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_ID: u16 = 1;
        const TAG_IOREGSEL: u16 = 2;
        const TAG_REDIRECTION: u16 = 3;
        const TAG_PIN_ACTIVE_LOW: u16 = 4;
        const TAG_PIN_LEVEL: u16 = 5;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(id) = r.u8(TAG_ID)? {
            self.id = IoApicId(id & 0x0f);
        }
        if let Some(sel) = r.u8(TAG_IOREGSEL)? {
            self.ioregsel = sel;
        }

        if let Some(buf) = r.bytes(TAG_REDIRECTION) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count == 0 {
                return Err(SnapshotError::InvalidFieldEncoding("ioapic redirection count"));
            }
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let vector = d.u8()?;
                let delivery_mode = d.u8()? & 0x7;
                let destination_mode = d.bool()?;
                let polarity_low = d.bool()?;
                let trigger_mode = match d.u8()? {
                    0 => TriggerMode::Edge,
                    1 => TriggerMode::Level,
                    _ => return Err(SnapshotError::InvalidFieldEncoding("ioapic trigger_mode")),
                };
                let mask = d.bool()?;
                let destination = d.u8()?;
                let remote_irr = d.bool()?;
                entries.push(RedirectionEntry {
                    vector,
                    delivery_mode,
                    destination_mode,
                    polarity_low,
                    trigger_mode,
                    mask,
                    destination,
                    remote_irr,
                });
            }
            d.finish()?;
            self.redirection = entries;
        }

        if let Some(buf) = r.bytes(TAG_PIN_ACTIVE_LOW) {
            let mut d = Decoder::new(buf);
            let values = d.vec_u8()?;
            d.finish()?;
            self.pin_active_low = values
                .into_iter()
                .map(|v| match v {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(SnapshotError::InvalidFieldEncoding("ioapic pin_active_low")),
                })
                .collect::<SnapshotResult<Vec<bool>>>()?;
        }

        if let Some(buf) = r.bytes(TAG_PIN_LEVEL) {
            let mut d = Decoder::new(buf);
            let values = d.vec_u8()?;
            d.finish()?;
            self.pin_level = values
                .into_iter()
                .map(|v| match v {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(SnapshotError::InvalidFieldEncoding("ioapic pin_level")),
                })
                .collect::<SnapshotResult<Vec<bool>>>()?;
        }

        let pins = self.redirection.len();
        let old_level_len = self.pin_level.len();
        self.pin_active_low.resize(pins, false);
        self.pin_level.resize(pins, false);
        for idx in old_level_len..pins {
            // Default new pins to the deasserted electrical level for their wiring.
            self.pin_level[idx] = self.pin_active_low[idx];
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::apic::LocalApic;

    fn mk_ioapic() -> (IoApic, Arc<LocalApic>) {
        let lapic = Arc::new(LocalApic::new(0));
        // Enable LAPIC with a valid spurious interrupt vector (0xFF) so injected
        // interrupts are accepted.
        lapic.mmio_write(0xF0, &(0x1FFu32).to_le_bytes());
        let ioapic = IoApic::new(IoApicId(0), lapic.clone());
        (ioapic, lapic)
    }

    fn service_next(lapic: &LocalApic) -> Option<u8> {
        let vector = lapic.get_pending_vector()?;
        assert!(lapic.ack(vector));
        lapic.eoi();
        Some(vector)
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
        ioapic.mmio_write(0x10, 4, 0x20); // vector 0x20, mask bit is 0 here.
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
        assert_eq!(service_next(&lapic), Some(0x20));

        // Still asserted: should not re-deliver.
        ioapic.set_irq_level(0, true);
        assert_eq!(service_next(&lapic), None);

        // Deassert then assert again: new edge.
        ioapic.set_irq_level(0, false);
        ioapic.set_irq_level(0, true);
        assert_eq!(service_next(&lapic), Some(0x20));
    }

    #[test]
    fn level_triggered_delivers_without_storming() {
        let (mut ioapic, lapic) = mk_ioapic();

        // Entry 1, vector 0x21, level triggered, unmasked.
        ioapic.mmio_write(0x00, 4, 0x12); // low dword of entry 1
        ioapic.mmio_write(0x10, 4, 0x21 | (1 << 15)); // trigger_mode=level

        ioapic.set_irq_level(1, true);
        assert_eq!(service_next(&lapic), Some(0x21));

        // Still asserted: no re-delivery without EOI.
        ioapic.set_irq_level(1, true);
        assert_eq!(service_next(&lapic), None);

        // Deassert then assert again: the IOAPIC still considers the interrupt in service
        // (Remote-IRR remains set) so it must *not* re-deliver until the LAPIC EOI is observed.
        ioapic.set_irq_level(1, false);
        ioapic.set_irq_level(1, true);
        assert_eq!(service_next(&lapic), None);

        // Now emulate EOI while still asserted; should re-deliver (remote IRR cleared).
        ioapic.notify_eoi(0x21);
        assert_eq!(service_next(&lapic), Some(0x21));

        // If the line is deasserted before EOI, the EOI should clear Remote-IRR but not
        // re-deliver until the line asserts again.
        ioapic.set_irq_level(1, false);
        ioapic.notify_eoi(0x21);
        assert_eq!(service_next(&lapic), None);

        ioapic.set_irq_level(1, true);
        assert_eq!(service_next(&lapic), Some(0x21));
    }
}
