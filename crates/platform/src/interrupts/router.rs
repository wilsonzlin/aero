use super::ioapic::{IoApic, IoApicDelivery};
use super::local_apic::LocalApic;
use super::pic::Pic8259;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlatformInterruptMode {
    LegacyPic,
    Apic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptInput {
    IsaIrq(u8),
    Gsi(u32),
}

pub trait InterruptController {
    fn get_pending(&self) -> Option<u8>;
    fn acknowledge(&mut self, vector: u8);
    fn eoi(&mut self, vector: u8);
}

#[derive(Debug, Clone)]
pub struct PlatformInterrupts {
    mode: PlatformInterruptMode,
    isa_irq_to_gsi: [u32; 16],
    pic: Pic8259,
    ioapic: IoApic,
    lapic: LocalApic,
    imcr_select: u8,
    imcr: u8,
}

impl PlatformInterrupts {
    pub fn new() -> Self {
        let mut isa_irq_to_gsi = [0u32; 16];
        for (idx, slot) in isa_irq_to_gsi.iter_mut().enumerate() {
            *slot = idx as u32;
        }

        Self {
            mode: PlatformInterruptMode::LegacyPic,
            isa_irq_to_gsi,
            pic: Pic8259::new(0x08, 0x70),
            ioapic: IoApic::new(24),
            lapic: LocalApic::new(0),
            imcr_select: 0,
            imcr: 0,
        }
    }

    pub fn mode(&self) -> PlatformInterruptMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: PlatformInterruptMode) {
        self.mode = mode;
        if mode == PlatformInterruptMode::Apic {
            self.sync_ioapic();
        }
    }

    pub fn pic(&self) -> &Pic8259 {
        &self.pic
    }

    pub fn pic_mut(&mut self) -> &mut Pic8259 {
        &mut self.pic
    }

    pub fn ioapic(&self) -> &IoApic {
        &self.ioapic
    }

    pub fn ioapic_mut(&mut self) -> &mut IoApic {
        &mut self.ioapic
    }

    pub fn lapic(&self) -> &LocalApic {
        &self.lapic
    }

    pub fn lapic_mut(&mut self) -> &mut LocalApic {
        &mut self.lapic
    }

    pub fn set_isa_irq_override(&mut self, isa_irq: u8, gsi: u32) {
        if isa_irq < 16 {
            self.isa_irq_to_gsi[isa_irq as usize] = gsi;
        }
    }

    pub fn raise_irq(&mut self, input: InterruptInput) {
        match input {
            InterruptInput::IsaIrq(irq) => match self.mode {
                PlatformInterruptMode::LegacyPic => self.pic.raise_irq(irq),
                PlatformInterruptMode::Apic => {
                    let gsi = self
                        .isa_irq_to_gsi
                        .get(irq as usize)
                        .copied()
                        .unwrap_or(irq as u32);
                    self.raise_gsi(gsi);
                }
            },
            InterruptInput::Gsi(gsi) => self.raise_gsi(gsi),
        }
    }

    pub fn lower_irq(&mut self, input: InterruptInput) {
        match input {
            InterruptInput::IsaIrq(irq) => match self.mode {
                PlatformInterruptMode::LegacyPic => self.pic.lower_irq(irq),
                PlatformInterruptMode::Apic => {
                    let gsi = self
                        .isa_irq_to_gsi
                        .get(irq as usize)
                        .copied()
                        .unwrap_or(irq as u32);
                    self.lower_gsi(gsi);
                }
            },
            InterruptInput::Gsi(gsi) => self.lower_gsi(gsi),
        }
    }

    pub fn ioapic_mmio_read(&self, offset: u64) -> u32 {
        self.ioapic.mmio_read(offset)
    }

    pub fn ioapic_mmio_write(&mut self, offset: u64, value: u32) {
        let deliveries = self.ioapic.mmio_write(offset, value);
        self.deliver_ioapic_deliveries(deliveries);
    }

    /// Emulates the Interrupt Mode Configuration Register (IMCR).
    ///
    /// Some chipsets provide the IMCR at ports `0x22`/`0x23` to switch ISA IRQ
    /// delivery between the legacy 8259 PIC and the APIC/IOAPIC path.
    ///
    /// This router uses the same convention as QEMU:
    /// - write `0x70` to port `0x22` to select the IMCR register
    /// - write bit0 to port `0x23` (`0` = PIC, `1` = APIC)
    pub fn imcr_port_write(&mut self, port: u16, value: u8) {
        match port {
            0x22 => self.imcr_select = value,
            0x23 => {
                if self.imcr_select == 0x70 {
                    self.imcr = value & 1;
                    self.set_mode(if self.imcr != 0 {
                        PlatformInterruptMode::Apic
                    } else {
                        PlatformInterruptMode::LegacyPic
                    });
                }
            }
            _ => {}
        }
    }

    fn raise_gsi(&mut self, gsi: u32) {
        let deliveries = self.ioapic.set_line(gsi, true);
        self.deliver_ioapic_deliveries(deliveries);
    }

    fn lower_gsi(&mut self, gsi: u32) {
        let deliveries = self.ioapic.set_line(gsi, false);
        self.deliver_ioapic_deliveries(deliveries);
    }

    fn deliver_ioapic_deliveries(&mut self, deliveries: Vec<IoApicDelivery>) {
        if self.mode != PlatformInterruptMode::Apic {
            return;
        }

        for delivery in deliveries {
            if delivery.dest as u32 != self.lapic.apic_id() {
                continue;
            }

            self.lapic.inject_vector(delivery.vector);
        }
    }

    fn sync_ioapic(&mut self) {
        let deliveries = self.ioapic.sync();
        self.deliver_ioapic_deliveries(deliveries);
    }

    fn lapic_pending_vector(&self) -> Option<u8> {
        for vector in (0u16..=255).rev() {
            let vector = vector as u8;
            if self.lapic.is_pending(vector) {
                return Some(vector);
            }
        }
        None
    }
}

impl InterruptController for PlatformInterrupts {
    fn get_pending(&self) -> Option<u8> {
        match self.mode {
            PlatformInterruptMode::LegacyPic => self.pic.get_pending_vector(),
            PlatformInterruptMode::Apic => self.lapic_pending_vector(),
        }
    }

    fn acknowledge(&mut self, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => self.pic.acknowledge(vector),
            PlatformInterruptMode::Apic => {
                self.lapic.acknowledge_vector(vector);
            }
        }
    }

    fn eoi(&mut self, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => self.pic.eoi(vector),
            PlatformInterruptMode::Apic => {
                let deliveries = self.ioapic.notify_eoi(vector);
                self.deliver_ioapic_deliveries(deliveries);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupts::{IoApicRedirectionEntry, TriggerMode};

    #[test]
    fn legacy_pic_irq1_delivers_pic_vector() {
        let mut ints = PlatformInterrupts::new();
        ints.pic_mut().set_offsets(0x20, 0x28);

        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(0x21));

        ints.acknowledge(0x21);
        assert_eq!(ints.get_pending(), None);

        ints.eoi(0x21);

        ints.lower_irq(InterruptInput::IsaIrq(1));
        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(0x21));
    }

    #[test]
    fn apic_mode_routes_isa_irq_via_ioapic_to_lapic() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        let mut entry = IoApicRedirectionEntry::fixed(0x31, 0);
        entry.masked = false;
        ints.ioapic_mut().set_entry(1, entry);

        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(0x31));
        ints.acknowledge(0x31);

        ints.lower_irq(InterruptInput::IsaIrq(1));
        ints.eoi(0x31);

        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(0x31));
    }

    #[test]
    fn level_triggered_ioapic_uses_remote_irr_until_eoi() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        let mut entry = IoApicRedirectionEntry::fixed(0x40, 0);
        entry.masked = false;
        entry.trigger = TriggerMode::Level;
        ints.ioapic_mut().set_entry(1, entry);

        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending(), Some(0x40));
        ints.acknowledge(0x40);
        assert_eq!(ints.get_pending(), None);

        ints.eoi(0x40);
        assert_eq!(ints.get_pending(), Some(0x40));
    }

    #[test]
    fn apic_eoi_clears_remote_irr_for_shared_vectors() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        let vector = 0x50;

        let mut entry1 = IoApicRedirectionEntry::fixed(vector, 0);
        entry1.masked = false;
        entry1.trigger = TriggerMode::Level;
        ints.ioapic_mut().set_entry(1, entry1);

        let mut entry2 = IoApicRedirectionEntry::fixed(vector, 0);
        entry2.masked = false;
        entry2.trigger = TriggerMode::Level;
        ints.ioapic_mut().set_entry(2, entry2);

        ints.raise_irq(InterruptInput::Gsi(1));
        ints.raise_irq(InterruptInput::Gsi(2));
        assert_eq!(ints.get_pending(), Some(vector));

        ints.acknowledge(vector);
        ints.lower_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending(), None);

        ints.eoi(vector);
        assert_eq!(ints.get_pending(), Some(vector));
    }

    #[test]
    fn imcr_switches_between_pic_and_apic_modes() {
        let mut ints = PlatformInterrupts::new();
        assert_eq!(ints.mode(), PlatformInterruptMode::LegacyPic);

        ints.imcr_port_write(0x22, 0x70);
        ints.imcr_port_write(0x23, 0x01);
        assert_eq!(ints.mode(), PlatformInterruptMode::Apic);

        ints.imcr_port_write(0x22, 0x70);
        ints.imcr_port_write(0x23, 0x00);
        assert_eq!(ints.mode(), PlatformInterruptMode::LegacyPic);
    }
}
