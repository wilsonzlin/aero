use super::pic::Pic8259;
use crate::io::{IoPortBus, PortIoDevice};
use aero_interrupts::apic::{IoApic, IoApicId, LapicInterruptSink, LocalApic};
use aero_interrupts::clock::Clock;
use aero_interrupts::pic8259::{MASTER_DATA, SLAVE_DATA};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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

pub const IMCR_SELECT_PORT: u16 = 0x22;
pub const IMCR_DATA_PORT: u16 = 0x23;
pub const IMCR_INDEX: u8 = 0x70;

pub trait InterruptController {
    fn get_pending(&self) -> Option<u8>;
    fn acknowledge(&mut self, vector: u8);
    fn eoi(&mut self, vector: u8);
}

#[derive(Debug, Default)]
struct AtomicClock {
    now_ns: AtomicU64,
}

impl AtomicClock {
    fn advance_ns(&self, delta_ns: u64) {
        self.now_ns.fetch_add(delta_ns, Ordering::SeqCst);
    }

    fn set_now_ns(&self, now_ns: u64) {
        self.now_ns.store(now_ns, Ordering::SeqCst);
    }
}

impl Clock for AtomicClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.load(Ordering::SeqCst)
    }
}

struct RoutedLapicSink {
    lapic: Arc<LocalApic>,
    apic_enabled: Arc<AtomicBool>,
}

impl LapicInterruptSink for RoutedLapicSink {
    fn apic_id(&self) -> u8 {
        self.lapic.apic_id()
    }

    fn inject_external_interrupt(&self, vector: u8) {
        if self.apic_enabled.load(Ordering::SeqCst) {
            self.lapic.inject_fixed_interrupt(vector);
        }
    }
}

#[derive(Clone)]
pub struct PlatformInterrupts {
    mode: PlatformInterruptMode,
    isa_irq_to_gsi: [u32; 16],
    gsi_level: Vec<bool>,

    pic: Pic8259,
    ioapic: Arc<Mutex<IoApic>>,
    lapic: Arc<LocalApic>,
    lapic_clock: Arc<AtomicClock>,
    apic_enabled: Arc<AtomicBool>,

    imcr_select: u8,
    imcr: u8,
}

impl Default for PlatformInterrupts {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PlatformInterrupts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlatformInterrupts")
            .field("mode", &self.mode)
            .field("isa_irq_to_gsi", &self.isa_irq_to_gsi)
            .field("gsi_level", &self.gsi_level)
            .field("pic", &self.pic)
            .field("imcr_select", &self.imcr_select)
            .field("imcr", &self.imcr)
            .finish_non_exhaustive()
    }
}

pub type SharedPlatformInterrupts = Rc<RefCell<PlatformInterrupts>>;

struct ImcrPort {
    interrupts: SharedPlatformInterrupts,
    port: u16,
}

impl ImcrPort {
    fn new(interrupts: SharedPlatformInterrupts, port: u16) -> Self {
        Self { interrupts, port }
    }
}

impl PortIoDevice for ImcrPort {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        let value = self.interrupts.borrow().imcr_port_read_u8(port) as u32;
        match size {
            1 => value,
            2 => value | (value << 8),
            4 => value | (value << 8) | (value << 16) | (value << 24),
            _ => value,
        }
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        let mut interrupts = self.interrupts.borrow_mut();
        match size {
            1 => interrupts.imcr_port_write(port, value as u8),
            2 | 4 => {
                for i in 0..(size as usize) {
                    interrupts.imcr_port_write(port, (value >> (i * 8)) as u8);
                }
            }
            _ => interrupts.imcr_port_write(port, value as u8),
        }
    }
}

impl PlatformInterrupts {
    pub fn new() -> Self {
        let mut isa_irq_to_gsi = [0u32; 16];
        for (idx, slot) in isa_irq_to_gsi.iter_mut().enumerate() {
            *slot = idx as u32;
        }

        // Match the MADT Interrupt Source Override (ISO) entries published by firmware.
        //
        // The ACPI tables (emitted by `aero-acpi`) publish ISA IRQ0 -> GSI2 (the legacy PIT
        // interrupt). Windows and other ACPI/APIC OSes will program the IOAPIC expecting that
        // mapping.
        isa_irq_to_gsi[0] = 2;

        let lapic_clock = Arc::new(AtomicClock::default());
        let lapic = Arc::new(LocalApic::with_clock(lapic_clock.clone(), 0));
        // Keep the LAPIC enabled for platform-level interrupt injection (tests and early bring-up).
        lapic.mmio_write(0xF0, &(0x1FFu32).to_le_bytes());

        let apic_enabled = Arc::new(AtomicBool::new(false));
        let sink: Arc<dyn LapicInterruptSink> = Arc::new(RoutedLapicSink {
            lapic: lapic.clone(),
            apic_enabled: apic_enabled.clone(),
        });

        let ioapic = Arc::new(Mutex::new(IoApic::new(IoApicId(0), sink)));

        // Wire LAPIC EOI -> IOAPIC Remote-IRR handling.
        let ioapic_for_eoi = ioapic.clone();
        lapic.register_eoi_notifier(Arc::new(move |vector| {
            ioapic_for_eoi.lock().unwrap().notify_eoi(vector);
        }));

        let num_gsis = ioapic.lock().unwrap().num_redirection_entries();

        // `Pic8259::new` programs vector offsets using the standard legacy init sequence.
        // The 8259A clears IMR during initialization (enabling all IRQ lines), which is not a
        // great power-on default for our platform: once the CPU enables IF after BIOS POST,
        // periodic timers (PIT/HPET) could start delivering interrupts before the guest has
        // installed real handlers, vectoring into BIOS default `HLT;IRET` stubs.
        //
        // Mask all IRQs by default; guest software can explicitly unmask lines as needed.
        let mut pic = Pic8259::new(0x08, 0x70);
        pic.port_write_u8(MASTER_DATA, 0xFF);
        pic.port_write_u8(SLAVE_DATA, 0xFF);

        Self {
            mode: PlatformInterruptMode::LegacyPic,
            isa_irq_to_gsi,
            gsi_level: vec![false; num_gsis],

            pic,
            ioapic,
            lapic,
            lapic_clock,
            apic_enabled,

            imcr_select: 0,
            imcr: 0,
        }
    }

    /// Reset the interrupt controller complex back to its power-on state.
    pub fn reset(&mut self) {
        *self = Self::new();
    }

    pub fn mode(&self) -> PlatformInterruptMode {
        self.mode
    }

    pub fn set_mode(&mut self, mode: PlatformInterruptMode) {
        let prev = self.mode;
        self.mode = mode;

        self.apic_enabled
            .store(mode == PlatformInterruptMode::Apic, Ordering::SeqCst);

        if prev != PlatformInterruptMode::Apic && mode == PlatformInterruptMode::Apic {
            // If the IOAPIC has been programmed while still routing through the legacy
            // PIC (or has seen input levels change without delivery), Remote-IRR may not
            // represent a real in-service interrupt. Reset it before syncing asserted
            // level-triggered lines into the LAPIC.
            let mut ioapic = self.ioapic.lock().unwrap();
            ioapic.clear_remote_irr();
            for (gsi, level) in self.gsi_level.iter().enumerate() {
                ioapic.set_irq_level(gsi as u32, *level);
            }
        }
    }

    pub fn pic(&self) -> &Pic8259 {
        &self.pic
    }

    pub fn pic_mut(&mut self) -> &mut Pic8259 {
        &mut self.pic
    }

    pub fn set_isa_irq_override(&mut self, isa_irq: u8, gsi: u32) {
        if isa_irq < 16 {
            self.isa_irq_to_gsi[isa_irq as usize] = gsi;
        }
    }

    pub fn raise_irq(&mut self, input: InterruptInput) {
        match input {
            InterruptInput::IsaIrq(irq) => {
                let gsi = self
                    .isa_irq_to_gsi
                    .get(irq as usize)
                    .copied()
                    .unwrap_or(irq as u32);

                self.set_gsi_level(gsi, true);

                match self.mode {
                    PlatformInterruptMode::LegacyPic => {
                        self.pic.raise_irq(irq);
                    }
                    PlatformInterruptMode::Apic => {
                        self.ioapic.lock().unwrap().set_irq_level(gsi, true);
                    }
                }
            }
            InterruptInput::Gsi(gsi) => {
                self.set_gsi_level(gsi, true);
                if self.mode == PlatformInterruptMode::Apic {
                    self.ioapic.lock().unwrap().set_irq_level(gsi, true);
                }
            }
        }
    }

    pub fn lower_irq(&mut self, input: InterruptInput) {
        match input {
            InterruptInput::IsaIrq(irq) => {
                let gsi = self
                    .isa_irq_to_gsi
                    .get(irq as usize)
                    .copied()
                    .unwrap_or(irq as u32);

                self.set_gsi_level(gsi, false);

                match self.mode {
                    PlatformInterruptMode::LegacyPic => {
                        self.pic.lower_irq(irq);
                    }
                    PlatformInterruptMode::Apic => {
                        self.ioapic.lock().unwrap().set_irq_level(gsi, false);
                    }
                }
            }
            InterruptInput::Gsi(gsi) => {
                self.set_gsi_level(gsi, false);
                if self.mode == PlatformInterruptMode::Apic {
                    self.ioapic.lock().unwrap().set_irq_level(gsi, false);
                }
            }
        }
    }

    /// Returns the current electrical "asserted" level for a given platform GSI.
    ///
    /// This reflects the last value applied via [`PlatformInterrupts::raise_irq`],
    /// [`PlatformInterrupts::lower_irq`], or any `GsiLevelSink` integration
    /// (e.g. `PciIntxRouter`).
    pub fn gsi_level(&self, gsi: u32) -> bool {
        self.gsi_level.get(gsi as usize).copied().unwrap_or(false)
    }

    pub fn ioapic_mmio_read(&self, offset: u64) -> u32 {
        self.ioapic.lock().unwrap().mmio_read(offset, 4) as u32
    }

    pub fn ioapic_mmio_write(&mut self, offset: u64, value: u32) {
        let mut ioapic = self.ioapic.lock().unwrap();
        ioapic.mmio_write(offset, 4, u64::from(value));
        if self.mode != PlatformInterruptMode::Apic {
            ioapic.clear_remote_irr();
        }
    }

    pub fn lapic_mmio_read(&self, offset: u64, data: &mut [u8]) {
        self.lapic.mmio_read(offset, data);
    }

    pub fn lapic_mmio_write(&self, offset: u64, data: &[u8]) {
        self.lapic.mmio_write(offset, data);
    }

    pub fn tick(&self, delta_ns: u64) {
        self.lapic_clock.advance_ns(delta_ns);
        self.lapic.poll();
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
            IMCR_SELECT_PORT => self.imcr_select = value,
            IMCR_DATA_PORT => {
                if self.imcr_select == IMCR_INDEX {
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

    pub fn imcr_port_read_u8(&self, port: u16) -> u8 {
        match port {
            IMCR_SELECT_PORT => self.imcr_select,
            IMCR_DATA_PORT => {
                if self.imcr_select == IMCR_INDEX {
                    self.imcr
                } else {
                    0
                }
            }
            _ => 0,
        }
    }

    pub fn register_imcr_ports(bus: &mut IoPortBus, interrupts: SharedPlatformInterrupts) {
        bus.register(
            IMCR_SELECT_PORT,
            Box::new(ImcrPort::new(interrupts.clone(), IMCR_SELECT_PORT)),
        );
        bus.register(
            IMCR_DATA_PORT,
            Box::new(ImcrPort::new(interrupts, IMCR_DATA_PORT)),
        );
    }

    pub(crate) fn lapic_apic_id(&self) -> u8 {
        self.lapic.apic_id()
    }

    pub(crate) fn lapic_inject_fixed(&self, vector: u8) {
        self.lapic.inject_fixed_interrupt(vector);
    }

    fn set_gsi_level(&mut self, gsi: u32, level: bool) {
        if let Some(slot) = self.gsi_level.get_mut(gsi as usize) {
            *slot = level;
        }
    }
}

impl InterruptController for PlatformInterrupts {
    fn get_pending(&self) -> Option<u8> {
        match self.mode {
            PlatformInterruptMode::LegacyPic => self.pic.get_pending_vector(),
            PlatformInterruptMode::Apic => self.lapic.get_pending_vector(),
        }
    }

    fn acknowledge(&mut self, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => self.pic.acknowledge(vector),
            PlatformInterruptMode::Apic => {
                let _ = self.lapic.ack(vector);
            }
        }
    }

    fn eoi(&mut self, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => self.pic.eoi(vector),
            PlatformInterruptMode::Apic => {
                let _ = vector;
                self.lapic.eoi();
            }
        }
    }
}

impl IoSnapshot for PlatformInterrupts {
    const DEVICE_ID: [u8; 4] = *b"INTR";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_MODE: u16 = 1;
        const TAG_ISA_IRQ_TO_GSI: u16 = 2;
        const TAG_IMCR_SELECT: u16 = 3;
        const TAG_IMCR: u16 = 4;
        const TAG_PIC: u16 = 5;
        const TAG_IOAPIC: u16 = 6;
        const TAG_LAPIC: u16 = 7;
        const TAG_GSI_LEVEL: u16 = 8;
        const TAG_LAPIC_CLOCK_NOW_NS: u16 = 9;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        let mode = match self.mode {
            PlatformInterruptMode::LegacyPic => 0u8,
            PlatformInterruptMode::Apic => 1u8,
        };
        w.field_u8(TAG_MODE, mode);

        let mut enc = Encoder::new();
        for gsi in self.isa_irq_to_gsi {
            enc = enc.u32(gsi);
        }
        w.field_bytes(TAG_ISA_IRQ_TO_GSI, enc.finish());

        w.field_u8(TAG_IMCR_SELECT, self.imcr_select);
        w.field_u8(TAG_IMCR, self.imcr);

        let mut gsi_levels = Vec::with_capacity(self.gsi_level.len());
        for &level in &self.gsi_level {
            gsi_levels.push(if level { 1 } else { 0 });
        }
        w.field_bytes(TAG_GSI_LEVEL, Encoder::new().vec_u8(&gsi_levels).finish());

        w.field_u64(TAG_LAPIC_CLOCK_NOW_NS, self.lapic_clock.now_ns());

        w.field_bytes(TAG_PIC, self.pic.save_state());
        w.field_bytes(TAG_IOAPIC, self.ioapic.lock().unwrap().save_state());
        w.field_bytes(TAG_LAPIC, self.lapic.save_state());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_MODE: u16 = 1;
        const TAG_ISA_IRQ_TO_GSI: u16 = 2;
        const TAG_IMCR_SELECT: u16 = 3;
        const TAG_IMCR: u16 = 4;
        const TAG_PIC: u16 = 5;
        const TAG_IOAPIC: u16 = 6;
        const TAG_LAPIC: u16 = 7;
        const TAG_GSI_LEVEL: u16 = 8;
        const TAG_LAPIC_CLOCK_NOW_NS: u16 = 9;

        const MAX_SNAPSHOT_GSI_LEVELS: usize = 4096;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let mode = match r.u8(TAG_MODE)?.unwrap_or(0) {
            0 => PlatformInterruptMode::LegacyPic,
            1 => PlatformInterruptMode::Apic,
            _ => PlatformInterruptMode::LegacyPic,
        };
        self.mode = mode;
        self.apic_enabled
            .store(mode == PlatformInterruptMode::Apic, Ordering::SeqCst);

        if let Some(buf) = r.bytes(TAG_ISA_IRQ_TO_GSI) {
            let mut d = Decoder::new(buf);
            for slot in &mut self.isa_irq_to_gsi {
                *slot = d.u32()?;
            }
            d.finish()?;
        }

        if let Some(imcr_select) = r.u8(TAG_IMCR_SELECT)? {
            self.imcr_select = imcr_select;
        }
        if let Some(imcr) = r.u8(TAG_IMCR)? {
            self.imcr = imcr & 1;
        }

        if let Some(buf) = r.bytes(TAG_GSI_LEVEL) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            if count > MAX_SNAPSHOT_GSI_LEVELS {
                return Err(
                    aero_io_snapshot::io::state::SnapshotError::InvalidFieldEncoding("gsi_level"),
                );
            }
            let levels = d.bytes(count)?;
            d.finish()?;
            self.gsi_level = levels
                .iter()
                .map(|v| match *v {
                    0 => Ok(false),
                    1 => Ok(true),
                    _ => Err(
                        aero_io_snapshot::io::state::SnapshotError::InvalidFieldEncoding(
                            "gsi_level",
                        ),
                    ),
                })
                .collect::<SnapshotResult<Vec<bool>>>()?;
        }

        if let Some(now) = r.u64(TAG_LAPIC_CLOCK_NOW_NS)? {
            self.lapic_clock.set_now_ns(now);
        }

        if let Some(buf) = r.bytes(TAG_PIC) {
            self.pic.load_state(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_IOAPIC) {
            self.ioapic.lock().unwrap().load_state(buf)?;
        }
        if let Some(buf) = r.bytes(TAG_LAPIC) {
            self.lapic.restore_state(buf)?;
        }

        let num_gsis = self.ioapic.lock().unwrap().num_redirection_entries();
        self.gsi_level.resize(num_gsis, false);

        if self.mode == PlatformInterruptMode::Apic {
            // Re-synchronize asserted level-triggered IOAPIC lines into the LAPIC.
            //
            // This avoids losing interrupts on restore without clearing Remote-IRR; the IOAPIC
            // implementation gates level-triggered delivery on Remote-IRR.
            self.ioapic.lock().unwrap().sync_level_triggered();
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program_ioapic_entry(ints: &mut PlatformInterrupts, gsi: u32, low: u32, high: u32) {
        let redtbl_low = 0x10u32 + gsi * 2;
        let redtbl_high = redtbl_low + 1;
        ints.ioapic_mmio_write(0x00, redtbl_low);
        ints.ioapic_mmio_write(0x10, low);
        ints.ioapic_mmio_write(0x00, redtbl_high);
        ints.ioapic_mmio_write(0x10, high);
    }

    #[test]
    fn imcr_ports_switch_mode_via_io_bus() {
        let interrupts = Rc::new(RefCell::new(PlatformInterrupts::new()));
        let mut bus = IoPortBus::new();
        PlatformInterrupts::register_imcr_ports(&mut bus, interrupts.clone());

        bus.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
        bus.write_u8(IMCR_DATA_PORT, 0x01);
        assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::Apic);

        bus.write_u8(IMCR_SELECT_PORT, IMCR_INDEX);
        bus.write_u8(IMCR_DATA_PORT, 0x00);
        assert_eq!(interrupts.borrow().mode(), PlatformInterruptMode::LegacyPic);
    }

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

        // GSI1 -> vector 0x31, edge-triggered, unmasked.
        program_ioapic_entry(&mut ints, 1, 0x31, 0);

        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(0x31));
        ints.acknowledge(0x31);

        ints.lower_irq(InterruptInput::IsaIrq(1));
        ints.eoi(0x31);

        ints.raise_irq(InterruptInput::IsaIrq(1));
        assert_eq!(ints.get_pending(), Some(0x31));
    }

    #[test]
    fn apic_mode_applies_default_madt_iso_for_irq0() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Firmware publishes ISA IRQ0 -> GSI2.
        program_ioapic_entry(&mut ints, 2, 0x60, 0);

        ints.raise_irq(InterruptInput::IsaIrq(0));
        assert_eq!(ints.get_pending(), Some(0x60));
    }

    #[test]
    fn switching_to_apic_delivers_asserted_level_lines() {
        let mut ints = PlatformInterrupts::new();

        // GSI1 -> vector 0x60, level-triggered, unmasked.
        program_ioapic_entry(&mut ints, 1, 0x60 | (1 << 15), 0);

        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending(), None);

        ints.set_mode(PlatformInterruptMode::Apic);
        assert_eq!(ints.get_pending(), Some(0x60));
    }

    #[test]
    fn level_triggered_ioapic_uses_remote_irr_until_eoi() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        // GSI1 -> vector 0x40, level-triggered, unmasked.
        program_ioapic_entry(&mut ints, 1, 0x40 | (1 << 15), 0);

        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending(), Some(0x40));
        ints.acknowledge(0x40);
        assert_eq!(ints.get_pending(), None);

        ints.eoi(0x40);
        assert_eq!(ints.get_pending(), Some(0x40));
    }

    #[test]
    fn level_triggered_active_low_redelivers_after_eoi_when_line_stays_asserted() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Use GSI9 (SCI) which is active-low wired by default in our board model.
        // Program polarity_low=1 + trigger_mode=level, unmasked, vector 0x55.
        let vector = 0x55u32;
        let low = vector | (1 << 13) | (1 << 15); // polarity_low + level-triggered
        program_ioapic_entry(&mut ints, 9, low, 0);

        // Assert the line and ensure it delivers once.
        ints.raise_irq(InterruptInput::Gsi(9));
        assert_eq!(ints.get_pending(), Some(vector as u8));
        ints.acknowledge(vector as u8);

        // With the line still asserted, Remote-IRR should suppress re-delivery until EOI.
        assert_eq!(ints.get_pending(), None);

        // EOI while still asserted should clear Remote-IRR and cause re-delivery.
        ints.eoi(vector as u8);
        assert_eq!(ints.get_pending(), Some(vector as u8));

        // Deassert, then EOI: should stop.
        ints.acknowledge(vector as u8);
        ints.lower_irq(InterruptInput::Gsi(9));
        ints.eoi(vector as u8);
        assert_eq!(ints.get_pending(), None);
    }

    #[test]
    fn imcr_switches_between_pic_and_apic_modes() {
        let mut ints = PlatformInterrupts::new();
        assert_eq!(ints.mode(), PlatformInterruptMode::LegacyPic);

        ints.imcr_port_write(IMCR_SELECT_PORT, IMCR_INDEX);
        ints.imcr_port_write(IMCR_DATA_PORT, 0x01);
        assert_eq!(ints.mode(), PlatformInterruptMode::Apic);

        ints.imcr_port_write(IMCR_SELECT_PORT, IMCR_INDEX);
        ints.imcr_port_write(IMCR_DATA_PORT, 0x00);
        assert_eq!(ints.mode(), PlatformInterruptMode::LegacyPic);
    }

    #[test]
    fn lapic_timer_one_shot_delivers_after_tick() {
        let mut ints = PlatformInterrupts::new();
        ints.set_mode(PlatformInterruptMode::Apic);

        // Program LAPIC timer:
        // - Divide config = 0xB => divisor 1 (our model treats it as 1ns per tick).
        // - LVT timer = vector 0x40, unmasked, one-shot (default).
        // - Initial count = 10 ticks.
        ints.lapic_mmio_write(0x3E0, &0xBu32.to_le_bytes()); // Divide config
        ints.lapic_mmio_write(0x320, &0x40u32.to_le_bytes()); // LVT Timer
        ints.lapic_mmio_write(0x380, &10u32.to_le_bytes()); // Initial count

        ints.tick(9);
        assert_eq!(ints.get_pending(), None);

        ints.tick(1);
        assert_eq!(ints.get_pending(), Some(0x40));

        ints.acknowledge(0x40);
        ints.eoi(0x40);

        // One-shot timer should not re-fire.
        ints.tick(100);
        assert_eq!(ints.get_pending(), None);
    }
}
