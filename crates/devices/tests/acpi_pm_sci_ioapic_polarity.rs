use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN,
};
use aero_devices::apic::{IoApic, IoApicId, LocalApic};
use aero_devices::clock::ManualClock;
use aero_devices::irq::IrqLine;
use aero_platform::io::IoPortBus;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[derive(Debug, Clone, Copy)]
struct MadtIso {
    bus: u8,
    gsi: u32,
    flags: u16,
}

fn parse_madt_iso_for_source_irq(madt: &[u8], source_irq: u8) -> Option<MadtIso> {
    let total_len = read_u32_le(madt, 4) as usize;
    let total_len = total_len.min(madt.len());

    // MADT = SDT header (36) + Local APIC address (4) + flags (4) + entries...
    let mut off = 44usize;
    while off + 2 <= total_len {
        let entry_type = madt[off];
        let entry_len = madt[off + 1] as usize;
        if entry_len < 2 || off + entry_len > total_len {
            break;
        }

        // Interrupt Source Override structure (type 2, length 10):
        // u8 type, u8 length, u8 bus, u8 source_irq, u32 gsi, u16 flags.
        if entry_type == 2 && entry_len >= 10 {
            let bus = madt[off + 2];
            let entry_source_irq = madt[off + 3];
            if entry_source_irq == source_irq {
                let gsi = read_u32_le(madt, off + 4);
                let flags = read_u16_le(madt, off + 8);
                return Some(MadtIso { bus, gsi, flags });
            }
        }

        off += entry_len;
    }

    None
}

fn ioapic_write_reg(ioapic: &mut IoApic, reg: u32, value: u32) {
    ioapic.mmio_write(0x00, 4, u64::from(reg));
    ioapic.mmio_write(0x10, 4, u64::from(value));
}

fn ioapic_read_reg(ioapic: &mut IoApic, reg: u32) -> u32 {
    ioapic.mmio_write(0x00, 4, u64::from(reg));
    ioapic.mmio_read(0x10, 4) as u32
}

fn program_ioapic_redirection_from_iso_flags(
    ioapic: &mut IoApic,
    gsi: u32,
    vector: u8,
    iso_flags: u16,
) {
    // ACPI MADT ISO flags use the "MPS INTI" encoding (2-bit polarity + 2-bit trigger).
    let polarity = iso_flags & 0b11;
    let trigger = (iso_flags >> 2) & 0b11;

    let mut low = u32::from(vector);
    // bit13 = input polarity (1 = active low)
    if polarity == 0b11 {
        low |= 1 << 13;
    }
    // bit15 = trigger mode (1 = level)
    if trigger == 0b11 {
        low |= 1 << 15;
    }
    // bit16 = mask (0 = unmasked)

    let redtbl_low = 0x10u32 + gsi * 2;
    let redtbl_high = redtbl_low + 1;

    ioapic_write_reg(ioapic, redtbl_high, 0); // destination APIC ID 0
    ioapic_write_reg(ioapic, redtbl_low, low);
}

#[derive(Clone)]
struct IoApicIrqLine {
    ioapic: Arc<Mutex<IoApic>>,
    gsi: u32,
}

impl IrqLine for IoApicIrqLine {
    fn set_level(&self, level: bool) {
        self.ioapic.lock().unwrap().set_irq_level(self.gsi, level);
    }
}

#[test]
fn acpi_pm_sci_delivers_to_lapic_when_ioapic_polarity_matches_madt_iso() {
    let tables = AcpiTables::build(&AcpiConfig::default(), AcpiPlacement::default());
    let sci_irq = 9u8;

    let iso = parse_madt_iso_for_source_irq(tables.madt.as_slice(), sci_irq)
        .expect("missing MADT ISO for SCI");
    assert_eq!(iso.bus, 0, "SCI ISO must be for ISA bus 0");
    assert_eq!(iso.gsi, u32::from(sci_irq), "SCI ISO must map IRQ9 -> GSI9");
    assert_eq!(
        iso.flags, 0x000F,
        "SCI ISO flags must be active-low + level-triggered (MPS INTI polarity=3, trigger=3)"
    );

    let lapic = Arc::new(LocalApic::new(0));
    // Enable the LAPIC (SVR[8]=1).
    lapic.mmio_write(0xF0, &(1u32 << 8).to_le_bytes());

    let ioapic = Arc::new(Mutex::new(IoApic::new(IoApicId(0), lapic.clone())));
    // Make wiring explicit for the test: SCI is typically active-low on PC platforms.
    ioapic
        .lock()
        .unwrap()
        .set_pin_active_low(u32::from(sci_irq), true);

    // Connect LAPIC EOI to IOAPIC Remote-IRR handling.
    {
        let ioapic_for_eoi = ioapic.clone();
        lapic.register_eoi_notifier(Arc::new(move |vector| {
            ioapic_for_eoi.lock().unwrap().notify_eoi(vector);
        }));
    }

    // Program the IOAPIC entry for SCI according to the MADT ISO flags.
    {
        let mut ioapic = ioapic.lock().unwrap();
        program_ioapic_redirection_from_iso_flags(&mut ioapic, u32::from(sci_irq), 0x60, iso.flags);

        // Sanity check: polarity + trigger bits should reflect ISO flags.
        let low = ioapic_read_reg(&mut ioapic, 0x10u32 + u32::from(sci_irq) * 2);
        assert_ne!(
            low & (1 << 13),
            0,
            "IOAPIC redirection polarity must be active-low"
        );
        assert_ne!(
            low & (1 << 15),
            0,
            "IOAPIC redirection trigger must be level"
        );
        assert_eq!(
            low & (1 << 16),
            0,
            "IOAPIC redirection entry must be unmasked"
        );
    }

    let cfg = AcpiPmConfig::default();
    let callbacks = AcpiPmCallbacks {
        sci_irq: Box::new(IoApicIrqLine {
            ioapic: ioapic.clone(),
            gsi: u32::from(sci_irq),
        }),
        request_power_off: None,
    };

    let clock = ManualClock::new();
    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
        cfg, callbacks, clock,
    )));
    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    // Enable PWRBTN in PM1_EN so that triggering the status bit can assert SCI.
    bus.write(cfg.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    // Enable ACPI (sets SCI_EN).
    bus.write(cfg.smi_cmd_port, 1, u32::from(cfg.acpi_enable_cmd));

    pm.borrow_mut().trigger_power_button();
    assert!(
        pm.borrow().sci_level(),
        "PM1 event should assert SCI once SCI_EN is set"
    );

    // First delivery.
    assert_eq!(lapic.get_pending_vector(), Some(0x60));
    assert!(lapic.ack(0x60));

    // While the line is still asserted, Remote-IRR should prevent storming.
    {
        let mut ioapic = ioapic.lock().unwrap();
        let low = ioapic_read_reg(&mut ioapic, 0x10u32 + u32::from(sci_irq) * 2);
        assert_ne!(
            low & (1 << 14),
            0,
            "Level-triggered SCI must set IOAPIC Remote-IRR once delivered"
        );
    }

    // EOI should clear Remote-IRR and cause immediate re-delivery while SCI is still asserted.
    lapic.eoi();
    assert_eq!(lapic.get_pending_vector(), Some(0x60));

    // Acknowledge the re-delivered interrupt, then clear the event to deassert SCI and ensure
    // EOI does not cause another redelivery.
    assert!(lapic.ack(0x60));
    bus.write(cfg.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    assert!(!pm.borrow().sci_level());
    lapic.eoi();
    assert_eq!(lapic.get_pending_vector(), None);
}
