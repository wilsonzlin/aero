use std::cell::RefCell;
use std::rc::Rc;

use aero_acpi::{AcpiConfig, AcpiPlacement, AcpiTables};
use aero_devices::acpi_pm::{
    register_acpi_pm, AcpiPmCallbacks, AcpiPmConfig, AcpiPmIo, PM1_STS_PWRBTN,
};
use aero_devices::clock::ManualClock;
use aero_devices::irq::PlatformIrqLine;
use aero_platform::interrupts::{InterruptController, PlatformInterruptMode, PlatformInterrupts};
use aero_platform::io::IoPortBus;

fn read_u16_le(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes(bytes[offset..offset + 2].try_into().unwrap())
}

fn read_u32_le(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap())
}

#[derive(Debug, Clone, Copy)]
struct FadtInfo {
    sci_int: u16,
    smi_cmd_port: u16,
    acpi_enable_cmd: u8,
    acpi_disable_cmd: u8,
    pm1a_evt_blk: u16,
    pm1a_cnt_blk: u16,
    pm_tmr_blk: u16,
    gpe0_blk: u16,
    gpe0_blk_len: u8,
}

fn parse_fadt(fadt: &[u8]) -> FadtInfo {
    // FADT offsets are based on the ACPI 2.0+ fixed-header format we emit in `aero-acpi`.
    // See `aero_acpi::tables::build_fadt`.
    let sci_int = read_u16_le(fadt, 46);
    let smi_cmd_port = read_u32_le(fadt, 48) as u16;
    let acpi_enable_cmd = fadt[52];
    let acpi_disable_cmd = fadt[53];
    let pm1a_evt_blk = read_u32_le(fadt, 56) as u16;
    let pm1a_cnt_blk = read_u32_le(fadt, 64) as u16;
    let pm_tmr_blk = read_u32_le(fadt, 76) as u16;
    let gpe0_blk = read_u32_le(fadt, 80) as u16;
    let gpe0_blk_len = fadt[92];

    FadtInfo {
        sci_int,
        smi_cmd_port,
        acpi_enable_cmd,
        acpi_disable_cmd,
        pm1a_evt_blk,
        pm1a_cnt_blk,
        pm_tmr_blk,
        gpe0_blk,
        gpe0_blk_len,
    }
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

fn ioapic_write_reg(ints: &mut PlatformInterrupts, reg: u8, value: u32) {
    ints.ioapic_mmio_write(0x00, reg as u32);
    ints.ioapic_mmio_write(0x10, value);
}

fn ioapic_read_reg(ints: &mut PlatformInterrupts, reg: u8) -> u32 {
    ints.ioapic_mmio_write(0x00, reg as u32);
    ints.ioapic_mmio_read(0x10)
}

fn program_ioapic_redirection_from_iso_flags(
    ints: &mut PlatformInterrupts,
    gsi: u32,
    vector: u8,
    iso_flags: u16,
) {
    assert!(
        gsi <= (u8::MAX / 2) as u32,
        "GSI too large for IOAPIC register math"
    );
    let redir_low_index = 0x10u8 + (2 * gsi as u8);
    let redir_high_index = redir_low_index + 1;

    // Low dword:
    // - vector in bits 0..7
    // - bit13 = 1 => active-low
    // - bit15 = 1 => level-triggered
    // - bit16 = 0 => unmasked
    let mut low = vector as u32;

    // ACPI MADT ISO flags use the "MPS INTI" encoding (2-bit polarity + 2-bit trigger).
    let polarity = iso_flags & 0b11;
    let trigger = (iso_flags >> 2) & 0b11;

    if polarity == 0b11 {
        low |= 1 << 13;
    }
    if trigger == 0b11 {
        low |= 1 << 15;
    }

    ioapic_write_reg(ints, redir_low_index, low);
    ioapic_write_reg(ints, redir_high_index, 0); // destination APIC ID 0
}

fn ioapic_remote_irr_set(ints: &mut PlatformInterrupts, gsi: u32) -> bool {
    let redir_low_index = 0x10u8 + (2 * gsi as u8);
    let low = ioapic_read_reg(ints, redir_low_index);
    (low & (1 << 14)) != 0
}

#[test]
fn acpi_pm_sci_apic_mode_delivers_ioapic_vector_and_respects_remote_irr() {
    // Build a "firmware" ACPI table set, then use it as the source of truth for
    // the guest-visible fixed-function register addresses.
    let tables = AcpiTables::build(&AcpiConfig::default(), AcpiPlacement::default());
    let fadt = parse_fadt(tables.fadt.as_slice());

    // Windows 7 is extremely sensitive to SCI correctness; these values are a
    // deliberate ABI between firmware tables and device models.
    assert_eq!(fadt.sci_int, 9, "FADT SCI_INT must remain IRQ9/GSI9");
    assert_eq!(fadt.smi_cmd_port, 0x00B2, "FADT SMI_CMD port changed");
    assert_eq!(
        fadt.acpi_enable_cmd, 0xA0,
        "FADT ACPI_ENABLE command changed"
    );
    assert_eq!(
        fadt.acpi_disable_cmd, 0xA1,
        "FADT ACPI_DISABLE command changed"
    );
    assert_eq!(fadt.pm1a_evt_blk, 0x0400, "FADT PM1a_EVT_BLK port changed");
    assert_eq!(fadt.pm1a_cnt_blk, 0x0404, "FADT PM1a_CNT_BLK port changed");
    assert_eq!(fadt.pm_tmr_blk, 0x0408, "FADT PM_TMR_BLK port changed");
    assert_eq!(fadt.gpe0_blk, 0x0420, "FADT GPE0_BLK port changed");
    assert_eq!(fadt.gpe0_blk_len, 0x08, "FADT GPE0_BLK_LEN changed");

    let default_pm_cfg = AcpiPmConfig::default();
    assert_eq!(
        default_pm_cfg.smi_cmd_port, fadt.smi_cmd_port,
        "AcpiPmConfig::default() must match FADT SMI_CMD"
    );
    assert_eq!(
        default_pm_cfg.acpi_enable_cmd, fadt.acpi_enable_cmd,
        "AcpiPmConfig::default() must match FADT ACPI_ENABLE"
    );
    assert_eq!(
        default_pm_cfg.acpi_disable_cmd, fadt.acpi_disable_cmd,
        "AcpiPmConfig::default() must match FADT ACPI_DISABLE"
    );
    assert_eq!(
        default_pm_cfg.pm1a_evt_blk, fadt.pm1a_evt_blk,
        "AcpiPmConfig::default() must match FADT PM1a_EVT_BLK"
    );
    assert_eq!(
        default_pm_cfg.pm1a_cnt_blk, fadt.pm1a_cnt_blk,
        "AcpiPmConfig::default() must match FADT PM1a_CNT_BLK"
    );
    assert_eq!(
        default_pm_cfg.pm_tmr_blk, fadt.pm_tmr_blk,
        "AcpiPmConfig::default() must match FADT PM_TMR_BLK"
    );
    assert_eq!(
        default_pm_cfg.gpe0_blk, fadt.gpe0_blk,
        "AcpiPmConfig::default() must match FADT GPE0_BLK"
    );
    assert_eq!(
        default_pm_cfg.gpe0_blk_len, fadt.gpe0_blk_len,
        "AcpiPmConfig::default() must match FADT GPE0_BLK_LEN"
    );

    // Ensure the firmware's MADT publishes SCI with the expected polarity/trigger.
    let sci_irq = fadt.sci_int as u8;
    let sci_iso = parse_madt_iso_for_source_irq(tables.madt.as_slice(), sci_irq)
        .expect("missing MADT ISO for SCI");
    assert_eq!(sci_iso.bus, 0, "SCI ISO must be for ISA bus 0");
    assert_eq!(
        sci_iso.gsi,
        u32::from(sci_irq),
        "SCI ISO must route IRQ9 to GSI9 (identity mapping)"
    );
    assert_eq!(
        sci_iso.flags, 0x000F,
        "SCI ISO flags must remain active-low + level-triggered"
    );

    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.set_mode(PlatformInterruptMode::Apic);
        program_ioapic_redirection_from_iso_flags(
            &mut ints,
            u32::from(sci_irq),
            0x60,
            sci_iso.flags,
        );
    }

    let callbacks = AcpiPmCallbacks {
        sci_irq: Box::new(PlatformIrqLine::isa(interrupts.clone(), sci_irq)),
        request_power_off: None,
    };

    let clock = ManualClock::new();
    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
        AcpiPmConfig {
            pm1a_evt_blk: fadt.pm1a_evt_blk,
            pm1a_cnt_blk: fadt.pm1a_cnt_blk,
            pm_tmr_blk: fadt.pm_tmr_blk,
            gpe0_blk: fadt.gpe0_blk,
            gpe0_blk_len: fadt.gpe0_blk_len,
            smi_cmd_port: fadt.smi_cmd_port,
            acpi_enable_cmd: fadt.acpi_enable_cmd,
            acpi_disable_cmd: fadt.acpi_disable_cmd,
            start_enabled: false,
        },
        callbacks,
        clock,
    )));

    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    // Enable PWRBTN in PM1_EN so that triggering the status bit can assert SCI.
    bus.write(fadt.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));

    // Guest performs standard ACPI enable handshake (write ACPI_ENABLE to SMI_CMD).
    bus.write(fadt.smi_cmd_port, 1, u32::from(fadt.acpi_enable_cmd));
    assert!(pm.borrow().is_acpi_enabled());
    assert!(
        !pm.borrow().sci_level(),
        "SCI should remain deasserted without a pending event"
    );
    assert_eq!(interrupts.borrow().get_pending(), None);

    // Trigger a PM1 event (power button) -> SCI level asserts -> IOAPIC delivers a vector.
    pm.borrow_mut().trigger_power_button();
    assert!(
        pm.borrow().sci_level(),
        "PM1 event should assert SCI once SCI_EN is set"
    );
    assert_eq!(interrupts.borrow().get_pending(), Some(0x60));

    {
        let mut ints = interrupts.borrow_mut();
        assert!(
            ioapic_remote_irr_set(&mut ints, u32::from(sci_irq)),
            "Level-triggered SCI must set IOAPIC Remote-IRR once delivered"
        );

        // Acknowledge should move the vector into service and clear the LAPIC pending bit.
        ints.acknowledge(0x60);
        assert_eq!(ints.get_pending(), None);

        // Remote-IRR should remain set until EOI (prevents interrupt storming).
        assert!(
            ioapic_remote_irr_set(&mut ints, u32::from(sci_irq)),
            "Remote-IRR should remain set until EOI"
        );
    }

    // Typical ACPI handler clears the event status before EOI, deasserting SCI.
    bus.write(fadt.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    assert!(
        !pm.borrow().sci_level(),
        "Clearing PM1_STS should deassert SCI"
    );

    {
        let mut ints = interrupts.borrow_mut();
        assert!(
            ioapic_remote_irr_set(&mut ints, u32::from(sci_irq)),
            "Remote-IRR should still be set until EOI, even if SCI is deasserted"
        );

        ints.eoi(0x60);
        assert_eq!(ints.get_pending(), None);
        assert!(
            !ioapic_remote_irr_set(&mut ints, u32::from(sci_irq)),
            "EOI should clear Remote-IRR once SCI is deasserted"
        );
    }

    // Verify that another event can re-assert SCI and be delivered again.
    pm.borrow_mut().trigger_power_button();
    assert_eq!(interrupts.borrow().get_pending(), Some(0x60));
}

#[test]
fn acpi_pm_sci_legacy_pic_mode_raises_irq9_vector() {
    let tables = AcpiTables::build(&AcpiConfig::default(), AcpiPlacement::default());
    let fadt = parse_fadt(tables.fadt.as_slice());

    let sci_irq = fadt.sci_int as u8;
    assert_eq!(sci_irq, 9, "FADT SCI_INT must remain IRQ9");

    let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
    {
        let mut ints = interrupts.borrow_mut();
        ints.pic_mut().set_offsets(0x20, 0x28);
        ints.set_mode(PlatformInterruptMode::LegacyPic);
    }

    let callbacks = AcpiPmCallbacks {
        sci_irq: Box::new(PlatformIrqLine::isa(interrupts.clone(), sci_irq)),
        request_power_off: None,
    };

    let clock = ManualClock::new();
    let pm = Rc::new(RefCell::new(AcpiPmIo::new_with_callbacks_and_clock(
        AcpiPmConfig {
            pm1a_evt_blk: fadt.pm1a_evt_blk,
            pm1a_cnt_blk: fadt.pm1a_cnt_blk,
            pm_tmr_blk: fadt.pm_tmr_blk,
            gpe0_blk: fadt.gpe0_blk,
            gpe0_blk_len: fadt.gpe0_blk_len,
            smi_cmd_port: fadt.smi_cmd_port,
            acpi_enable_cmd: fadt.acpi_enable_cmd,
            acpi_disable_cmd: fadt.acpi_disable_cmd,
            start_enabled: false,
        },
        callbacks,
        clock,
    )));

    let mut bus = IoPortBus::new();
    register_acpi_pm(&mut bus, pm.clone());

    bus.write(fadt.pm1a_evt_blk + 2, 2, u32::from(PM1_STS_PWRBTN));
    bus.write(fadt.smi_cmd_port, 1, u32::from(fadt.acpi_enable_cmd));
    pm.borrow_mut().trigger_power_button();

    assert!(pm.borrow().sci_level());

    // With PIC offsets (0x20, 0x28), IRQ9 arrives as vector 0x29 (slave IRQ1).
    let vector = interrupts
        .borrow()
        .get_pending()
        .expect("missing pending PIC vector for SCI");
    assert_eq!(
        interrupts
            .borrow()
            .pic()
            .vector_to_irq(vector)
            .expect("PIC vector must map back to an IRQ"),
        9,
        "SCI must be routed through PIC as IRQ9"
    );

    {
        let mut ints = interrupts.borrow_mut();
        ints.acknowledge(vector);
    }

    // Clear the event and EOI, then ensure another power button press re-delivers.
    bus.write(fadt.pm1a_evt_blk, 2, u32::from(PM1_STS_PWRBTN));
    assert!(!pm.borrow().sci_level());
    {
        let mut ints = interrupts.borrow_mut();
        ints.eoi(vector);
    }

    pm.borrow_mut().trigger_power_button();
    let vector2 = interrupts
        .borrow()
        .get_pending()
        .expect("missing second PIC vector for SCI");
    assert_eq!(
        interrupts
            .borrow()
            .pic()
            .vector_to_irq(vector2)
            .expect("PIC vector must map back to an IRQ"),
        9
    );
}
