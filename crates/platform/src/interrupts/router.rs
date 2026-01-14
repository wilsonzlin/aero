use super::pic::Pic8259;
use crate::io::{IoPortBus, PortIoDevice};
use aero_interrupts::apic::{
    DeliveryMode, DestinationShorthand, Icr, IcrNotifier, IoApic, IoApicId, LapicInterruptSink,
    Level, LocalApic,
};
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
    /// Effective asserted level for each platform GSI (after aggregating all sources).
    gsi_level: Vec<bool>,
    /// Baseline line levels restored from snapshots.
    ///
    /// `IoSnapshot::load_state()` restores the interrupt controller complex (PIC/IOAPIC/LAPIC) and
    /// records the last observed electrical line levels (`TAG_GSI_LEVEL`), but that snapshot level
    /// is not attributable to a specific device.
    ///
    /// Device models (e.g. PCI INTx router, HPET) re-drive their own line outputs after restore
    /// using `sync_levels_to_sink()` style fixups. To avoid double-counting those reassertions, we
    /// treat the snapshot line levels as a temporary baseline and aggregate runtime assertions on
    /// top via `gsi_assert_count`.
    ///
    /// Snapshot consumers must call [`PlatformInterrupts::finalize_restore`] once all device line
    /// levels have been re-driven so the baseline can be converted into ref-counted state.
    gsi_restore_baseline: Vec<bool>,
    /// Reference count of active assertions for each GSI (excluding `gsi_restore_baseline`).
    gsi_assert_count: Vec<u32>,
    /// Generation counter used to invalidate cached `PlatformIrqLine` state across reset/restore.
    ///
    /// This is shared with `PlatformIrqLine` so devices can safely cache their last-driven level
    /// without being confused by snapshot restore (which rewinds device state).
    irq_line_generation: Arc<AtomicU64>,

    pic: Pic8259,
    ioapic: Arc<Mutex<IoApic>>,
    lapics: Vec<Arc<LocalApic>>,
    lapic_clock: Arc<AtomicClock>,
    apic_enabled: Arc<AtomicBool>,
    pending_init: Arc<Vec<AtomicBool>>,
    pending_sipi: Arc<Mutex<Vec<Option<u8>>>>,

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
            .field("lapics", &self.lapics.len())
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
        if size == 0 {
            return 0;
        }
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
        if size == 0 {
            return;
        }
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
        Self::new_with_cpu_count(1)
    }

    /// Create a new interrupt routing complex with `cpu_count` LAPICs (APIC IDs `0..cpu_count`).
    ///
    /// The current platform interrupt router is still largely BSP-centric (e.g. IOAPIC routing and
    /// `InterruptController` delivery default to LAPIC0), but we keep the full LAPIC set so SMP
    /// machine configurations can construct a stable topology.
    pub fn new_with_cpu_count(cpu_count: u8) -> Self {
        let cpu_count = cpu_count.max(1);
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
        let apic_enabled = Arc::new(AtomicBool::new(false));
        let pending_init: Arc<Vec<AtomicBool>> = Arc::new(
            (0..cpu_count)
                .map(|_| AtomicBool::new(false))
                .collect::<Vec<_>>(),
        );
        let mut lapics = Vec::with_capacity(cpu_count as usize);
        let mut sinks: Vec<Arc<dyn LapicInterruptSink>> = Vec::with_capacity(cpu_count as usize);
        for apic_id in 0..cpu_count {
            let lapic = Arc::new(LocalApic::with_clock(lapic_clock.clone(), apic_id));
            // Keep LAPICs enabled for platform-level interrupt injection (tests and early bring-up).
            lapic.mmio_write(0xF0, &(0x1FFu32).to_le_bytes());
            sinks.push(Arc::new(RoutedLapicSink {
                lapic: lapic.clone(),
                apic_enabled: apic_enabled.clone(),
            }));
            lapics.push(lapic);
        }
        let pending_sipi = Arc::new(Mutex::new(vec![None; cpu_count as usize]));
        let ioapic = Arc::new(Mutex::new(IoApic::with_lapics(IoApicId(0), sinks)));

        // Wire LAPIC EOI -> IOAPIC Remote-IRR handling.
        for lapic in &lapics {
            let ioapic_for_eoi = ioapic.clone();
            lapic.register_eoi_notifier(Arc::new(move |vector| {
                ioapic_for_eoi.lock().unwrap().notify_eoi(vector);
            }));
        }

        // Register LAPIC ICR notifiers for IPI delivery and INIT/SIPI event queueing.
        //
        // `LocalApic` decodes a completed ICR_LOW write and notifies listeners with a decoded
        // [`Icr`]. This keeps IPI routing out of the LAPIC model itself (it cannot see other CPUs)
        // while allowing the platform interrupt fabric to emulate SMP IPI delivery.
        let lapics_for_ipi = Arc::new(lapics.clone());
        for src_apic_id in 0..cpu_count {
            let pending_init_for_ipi = pending_init.clone();
            let pending_sipi_for_ipi = pending_sipi.clone();
            let lapics_for_ipi = lapics_for_ipi.clone();
            lapics[src_apic_id as usize].register_icr_notifier(Arc::new(move |icr: Icr| {
                PlatformInterrupts::handle_icr_ipi(
                    src_apic_id,
                    icr,
                    lapics_for_ipi.as_ref(),
                    &pending_init_for_ipi,
                    &pending_sipi_for_ipi,
                );
            }));
        }

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
            gsi_restore_baseline: vec![false; num_gsis],
            gsi_assert_count: vec![0; num_gsis],
            irq_line_generation: Arc::new(AtomicU64::new(0)),

            pic,
            ioapic,
            lapics,
            lapic_clock,
            apic_enabled,
            pending_init,
            pending_sipi,

            imcr_select: 0,
            imcr: 0,
        }
    }

    fn handle_icr_ipi(
        src_apic_id: u8,
        icr: Icr,
        lapics: &[Arc<LocalApic>],
        pending_init: &Arc<Vec<AtomicBool>>,
        pending_sipi: &Arc<Mutex<Vec<Option<u8>>>>,
    ) {
        let mut targets: Vec<Arc<LocalApic>> = Vec::new();
        match icr.destination_shorthand {
            DestinationShorthand::None => {
                if let Some(lapic) = lapics
                    .iter()
                    .find(|lapic| lapic.apic_id() == icr.destination)
                    .cloned()
                {
                    targets.push(lapic);
                }
            }
            DestinationShorthand::SelfOnly => {
                if let Some(lapic) = lapics
                    .iter()
                    .find(|lapic| lapic.apic_id() == src_apic_id)
                    .cloned()
                {
                    targets.push(lapic);
                }
            }
            DestinationShorthand::AllIncludingSelf => {
                targets.extend(lapics.iter().cloned());
            }
            DestinationShorthand::AllExcludingSelf => {
                targets.extend(
                    lapics
                        .iter()
                        .filter(|lapic| lapic.apic_id() != src_apic_id)
                        .cloned(),
                );
            }
        }

        match icr.delivery_mode {
            DeliveryMode::Fixed => {
                for lapic in targets {
                    lapic.inject_fixed_interrupt(icr.vector);
                }
            }
            DeliveryMode::Init => {
                // Only INIT with level=assert has reset semantics. INIT deassert is a no-op.
                if icr.level != Level::Assert {
                    return;
                }
                for lapic in targets {
                    let apic_id = lapic.apic_id();
                    if let Some(flag) = pending_init.get(apic_id as usize) {
                        flag.store(true, Ordering::SeqCst);
                    }
                    lapic.reset_state(apic_id);
                    // Keep the LAPIC enabled for platform-level interrupt injection.
                    //
                    // Real hardware clears SVR[8] on reset; our platform keeps it set so IOAPIC/MSI
                    // delivery to an AP works deterministically during early bring-up.
                    Self::enable_lapic_software(lapic.as_ref());
                }
            }
            DeliveryMode::Startup => {
                let mut sipi = pending_sipi.lock().unwrap();
                for lapic in targets {
                    let apic_id = lapic.apic_id() as usize;
                    if let Some(slot) = sipi.get_mut(apic_id) {
                        *slot = Some(icr.vector);
                    }
                }
            }
            _ => {
                // Other delivery modes are currently ignored.
            }
        }
    }

    fn enable_lapic_software(lapic: &LocalApic) {
        // `LocalApic::reset_state` models the power-on SVR value (0xFF with the software-enable
        // bit cleared). At the platform level we keep LAPICs enabled by default so external
        // interrupt injection (IOAPIC/MSI) continues to work after INIT/RESET modelling.
        let mut buf = [0u8; 4];
        lapic.mmio_read(0xF0, &mut buf);
        let mut svr = u32::from_le_bytes(buf);
        svr |= 1 << 8;
        lapic.mmio_write(0xF0, &svr.to_le_bytes());
    }

    /// Like [`InterruptController::get_pending`], but scoped to a specific vCPU.
    pub fn get_pending_for_cpu(&self, cpu: usize) -> Option<u8> {
        match self.mode {
            // Legacy PIC mode only routes interrupts to the bootstrap processor (CPU0).
            PlatformInterruptMode::LegacyPic => {
                if cpu != 0 {
                    return None;
                }
                self.pic.get_pending_vector()
            }
            PlatformInterruptMode::Apic => {
                let lapic = self.lapics.get(cpu)?;
                lapic.get_pending_vector()
            }
        }
    }

    /// Like [`InterruptController::acknowledge`], but scoped to a specific vCPU.
    pub fn acknowledge_for_cpu(&mut self, cpu: usize, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => {
                if cpu != 0 {
                    return;
                }
                self.pic.acknowledge(vector);
            }
            PlatformInterruptMode::Apic => {
                let Some(lapic) = self.lapics.get(cpu) else {
                    return;
                };
                let _ = lapic.ack(vector);
            }
        }
    }

    /// Like [`InterruptController::eoi`], but scoped to a specific vCPU.
    pub fn eoi_for_cpu(&mut self, cpu: usize, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => {
                if cpu != 0 {
                    return;
                }
                self.pic.eoi(vector);
            }
            PlatformInterruptMode::Apic => {
                let _ = vector;
                let Some(lapic) = self.lapics.get(cpu) else {
                    return;
                };
                lapic.eoi();
            }
        }
    }

    /// Reset the interrupt controller complex back to its power-on state.
    pub fn reset(&mut self) {
        // Preserve the shared IRQ line generation counter so existing `PlatformIrqLine` handles
        // observe the reset and invalidate their cached level.
        self.irq_line_generation.fetch_add(1, Ordering::SeqCst);

        // Reset the PC wiring assumptions (ISA IRQ -> GSI mapping) to the defaults published by
        // firmware tables.
        for (idx, slot) in self.isa_irq_to_gsi.iter_mut().enumerate() {
            *slot = idx as u32;
        }
        // Match the MADT Interrupt Source Override (ISO) entries published by firmware:
        // ISA IRQ0 -> GSI2.
        self.isa_irq_to_gsi[0] = 2;

        for flag in self.pending_init.iter() {
            flag.store(false, Ordering::SeqCst);
        }

        // Deterministic LAPIC time starts at 0 on reset.
        self.lapic_clock.set_now_ns(0);

        // Reset LAPIC state in-place so any machine-level wiring (EOI/ICR notifiers, held `Arc`s)
        // survives across resets.
        for (idx, lapic) in self.lapics.iter().enumerate() {
            let apic_id = u8::try_from(idx).unwrap_or(u8::MAX);
            lapic.reset_state(apic_id);
            // Keep LAPICs enabled for platform-level interrupt injection (tests and early bring-up).
            lapic.mmio_write(0xF0, &(0x1FFu32).to_le_bytes());
        }

        // Reset the IOAPIC in-place while preserving the shared `Arc<Mutex<IoApic>>` identity so
        // existing LAPIC EOI notifier closures remain valid.
        self.apic_enabled.store(false, Ordering::SeqCst);
        let mut sinks: Vec<Arc<dyn LapicInterruptSink>> = Vec::with_capacity(self.lapics.len());
        for lapic in &self.lapics {
            sinks.push(Arc::new(RoutedLapicSink {
                lapic: lapic.clone(),
                apic_enabled: self.apic_enabled.clone(),
            }));
        }
        *self.ioapic.lock().unwrap() = IoApic::with_lapics(IoApicId(0), sinks);

        let num_gsis = self.ioapic.lock().unwrap().num_redirection_entries();
        self.gsi_level = vec![false; num_gsis];
        self.gsi_restore_baseline = vec![false; num_gsis];
        self.gsi_assert_count = vec![0; num_gsis];

        // Reset the legacy PIC and mask all IRQ lines by default.
        let mut pic = Pic8259::new(0x08, 0x70);
        pic.port_write_u8(MASTER_DATA, 0xFF);
        pic.port_write_u8(SLAVE_DATA, 0xFF);
        self.pic = pic;

        self.mode = PlatformInterruptMode::LegacyPic;
        self.imcr_select = 0;
        self.imcr = 0;
    }

    pub fn mode(&self) -> PlatformInterruptMode {
        self.mode
    }

    pub fn cpu_count(&self) -> usize {
        self.lapics.len()
    }

    /// Iterate over all LAPICs in the platform.
    ///
    /// This is primarily used by MSI delivery helpers (broadcast and logical destination modes)
    /// without exposing the internal `lapics: Vec<Arc<LocalApic>>` field outside this module.
    pub(crate) fn lapics_iter(&self) -> impl Iterator<Item = &LocalApic> + '_ {
        self.lapics.iter().map(|lapic| lapic.as_ref())
    }

    pub fn lapic(&self, cpu_index: usize) -> &LocalApic {
        self.lapics
            .get(cpu_index)
            .map(|lapic| lapic.as_ref())
            .unwrap_or_else(|| panic!("invalid CPU index {cpu_index}"))
    }

    /// Iterate over all LAPICs in the platform.
    ///
    /// This avoids exposing the internal `lapics: Vec<Arc<LocalApic>>` field directly.
    pub fn lapics_iter(&self) -> impl Iterator<Item = &LocalApic> + '_ {
        self.lapics.iter().map(|lapic| lapic.as_ref())
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
        let gsi = match input {
            InterruptInput::IsaIrq(irq) => self
                .isa_irq_to_gsi
                .get(irq as usize)
                .copied()
                .unwrap_or(irq as u32),
            InterruptInput::Gsi(gsi) => gsi,
        };
        self.update_gsi_assert_count(gsi, true);
    }

    pub fn lower_irq(&mut self, input: InterruptInput) {
        let gsi = match input {
            InterruptInput::IsaIrq(irq) => self
                .isa_irq_to_gsi
                .get(irq as usize)
                .copied()
                .unwrap_or(irq as u32),
            InterruptInput::Gsi(gsi) => gsi,
        };
        self.update_gsi_assert_count(gsi, false);
    }

    /// Returns the current electrical "asserted" level for a given platform GSI.
    ///
    /// This reflects the last value applied via [`PlatformInterrupts::raise_irq`],
    /// [`PlatformInterrupts::lower_irq`], or any `GsiLevelSink` integration
    /// (e.g. `PciIntxRouter`).
    pub fn gsi_level(&self, gsi: u32) -> bool {
        self.gsi_level.get(gsi as usize).copied().unwrap_or(false)
    }

    /// Returns the current IRQ line generation.
    ///
    /// `PlatformIrqLine` uses this to invalidate cached line levels across reset and snapshot
    /// restore.
    pub fn irq_line_generation(&self) -> u64 {
        self.irq_line_generation.load(Ordering::SeqCst)
    }

    /// Finalize a snapshot restore by converting `gsi_restore_baseline` into ref-counted GSI
    /// assertions.
    ///
    /// Snapshot restore loads the platform interrupt controller complex (PIC/IOAPIC/LAPIC) and
    /// restores the last known electrical levels for each GSI, but those levels are not attributable
    /// to individual device models. Restored device state is subsequently re-driven into the sink
    /// via explicit sync steps (e.g. PCI INTx router, HPET).
    ///
    /// This method must be called after all such device sync steps to:
    /// - adopt any still-asserted baseline lines as a single anonymous assertion (so lines restored
    ///   asserted remain asserted even if no device reasserted them), and
    /// - clear the baseline so future deassertions are governed purely by `gsi_assert_count`.
    pub fn finalize_restore(&mut self) {
        for idx in 0..self.gsi_level.len() {
            if *self.gsi_restore_baseline.get(idx).unwrap_or(&false)
                && self.gsi_assert_count.get(idx).copied().unwrap_or(0) == 0
            {
                if let Some(slot) = self.gsi_assert_count.get_mut(idx) {
                    *slot = 1;
                }
            }
        }

        // Clearing the baseline may change the effective line level for GSIs that were asserted in
        // the snapshot but were not claimed by any device model during restore.
        self.gsi_restore_baseline.fill(false);
        for gsi in 0..self.gsi_level.len() {
            let desired = self.gsi_assert_count[gsi] > 0;
            if self.gsi_level[gsi] != desired {
                self.gsi_level[gsi] = desired;
                self.drive_gsi_effective_level(gsi as u32, desired);
            }
        }
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

    pub fn lapic_mmio_read_for_cpu(&self, cpu: usize, offset: u64, data: &mut [u8]) {
        let Some(lapic) = self.lapics.get(cpu) else {
            data.fill(0);
            return;
        };
        lapic.mmio_read(offset, data);
    }

    pub fn lapic_mmio_write_for_cpu(&self, cpu: usize, offset: u64, data: &[u8]) {
        let Some(lapic) = self.lapics.get(cpu) else {
            return;
        };
        lapic.mmio_write(offset, data);
    }

    pub fn lapic_mmio_read(&self, offset: u64, data: &mut [u8]) {
        self.lapic_mmio_read_for_apic(0, offset, data);
    }

    pub fn lapic_mmio_write(&self, offset: u64, data: &[u8]) {
        self.lapic_mmio_write_for_apic(0, offset, data);
    }

    pub fn lapic_mmio_read_for_apic(&self, apic_id: u8, offset: u64, data: &mut [u8]) {
        let Some(lapic) = self.lapic_for_apic_id(apic_id) else {
            data.fill(0);
            return;
        };
        lapic.mmio_read(offset, data);
    }

    pub fn lapic_mmio_write_for_apic(&self, apic_id: u8, offset: u64, data: &[u8]) {
        let Some(lapic) = self.lapic_for_apic_id(apic_id) else {
            return;
        };
        lapic.mmio_write(offset, data);
    }

    /// Returns the LAPIC for `apic_id` if present.
    ///
    /// This returns a cloned [`Arc`] rather than exposing internal references, so callers cannot
    /// mutate the `PlatformInterrupts` LAPIC collection.
    pub fn lapic_by_apic_id(&self, apic_id: u8) -> Option<Arc<LocalApic>> {
        self.lapics
            .iter()
            .find(|lapic| lapic.apic_id() == apic_id)
            .cloned()
    }

    pub fn lapic_by_index(&self, cpu_index: usize) -> Option<Arc<LocalApic>> {
        self.lapics.get(cpu_index).cloned()
    }

    /// Register a callback that is invoked when the guest writes to `ICR_LOW` on the given LAPIC.
    pub fn register_icr_notifier(&self, apic_id: u8, notifier: IcrNotifier) {
        if let Some(lapic) = self.lapic_by_apic_id(apic_id) {
            lapic.register_icr_notifier(notifier);
        }
    }

    pub fn tick(&self, delta_ns: u64) {
        self.lapic_clock.advance_ns(delta_ns);
        for lapic in &self.lapics {
            lapic.poll();
        }
    }

    /// Take and clear the pending INIT-reset flag for `cpu`.
    ///
    /// The platform interrupt fabric marks this flag when it delivers an INIT IPI with
    /// `level=assert`. Consumers (e.g. a VM's vCPU loop) can poll this to reset vCPU state.
    pub fn take_pending_init(&self, cpu: usize) -> bool {
        self.pending_init
            .get(cpu)
            .map(|flag| flag.swap(false, Ordering::SeqCst))
            .unwrap_or(false)
    }

    pub fn take_pending_sipi(&self, cpu: usize) -> Option<u8> {
        let mut pending = self.pending_sipi.lock().unwrap();
        pending.get_mut(cpu).and_then(Option::take)
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

    #[cfg(test)]
    pub(crate) fn lapic_apic_id(&self) -> u8 {
        self.lapics[0].apic_id()
    }
    /// Inject a fixed interrupt into the LAPIC whose `apic_id()` matches `apic_id`.
    ///
    /// If no LAPIC matches, the interrupt is dropped.
    ///
    /// Note: This intentionally bypasses `PlatformInterruptMode` (legacy PIC vs APIC routing). MSI
    /// delivery is an APIC-local concept and should not depend on how legacy ISA IRQs are routed.
    pub(crate) fn inject_fixed_for_apic(&self, apic_id: u8, vector: u8) {
        let Some(lapic) = self.lapic_for_apic_id(apic_id) else {
            return;
        };
        lapic.inject_fixed_interrupt(vector);
    }

    /// Inject a fixed interrupt into all LAPICs ("broadcast").
    ///
    /// Note: This intentionally bypasses `PlatformInterruptMode` (legacy PIC vs APIC routing). MSI
    /// broadcast delivery should reach all CPUs regardless of legacy IRQ routing state.
    pub(crate) fn inject_fixed_broadcast(&self, vector: u8) {
        for lapic in &self.lapics {
            lapic.inject_fixed_interrupt(vector);
        }
    }
    /// Reset a specific LAPIC's internal state back to its power-on baseline.
    ///
    /// This is used by machine-level INIT IPI delivery to reset the target vCPU's local APIC.
    /// If `apic_id` does not correspond to any known LAPIC, this is a no-op.
    pub fn reset_lapic(&self, apic_id: u8) {
        let Some(lapic) = self.lapic_for_apic_id(apic_id) else {
            return;
        };
        lapic.reset_state(apic_id);
        Self::enable_lapic_software(lapic);
    }
    pub fn get_pending_for_apic(&self, apic_id: u8) -> Option<u8> {
        match self.mode {
            PlatformInterruptMode::LegacyPic => {
                if apic_id == 0 {
                    self.pic.get_pending_vector()
                } else {
                    None
                }
            }
            PlatformInterruptMode::Apic => self.lapic_for_apic_id(apic_id)?.get_pending_vector(),
        }
    }

    pub fn acknowledge_for_apic(&mut self, apic_id: u8, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => {
                if apic_id == 0 {
                    self.pic.acknowledge(vector);
                }
            }
            PlatformInterruptMode::Apic => {
                if let Some(lapic) = self.lapic_for_apic_id(apic_id) {
                    let _ = lapic.ack(vector);
                }
            }
        }
    }

    pub fn eoi_for_apic(&mut self, apic_id: u8, vector: u8) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => {
                if apic_id == 0 {
                    self.pic.eoi(vector);
                }
            }
            PlatformInterruptMode::Apic => {
                let _ = vector;
                if let Some(lapic) = self.lapic_for_apic_id(apic_id) {
                    lapic.eoi();
                }
            }
        }
    }

    pub(crate) fn lapic_for_apic_id(&self, apic_id: u8) -> Option<&LocalApic> {
        self.lapics
            .iter()
            .find(|lapic| lapic.apic_id() == apic_id)
            .map(|lapic| lapic.as_ref())
    }

    fn set_gsi_level_internal(&mut self, gsi: u32, level: bool) {
        if let Some(slot) = self.gsi_level.get_mut(gsi as usize) {
            *slot = level;
        }
    }

    fn legacy_pic_irq_for_gsi(&self, gsi: u32) -> Option<u8> {
        if gsi >= 16 {
            return None;
        }

        // Prefer any ISA IRQ that is explicitly mapped to this GSI (via ISO overrides).
        for (irq, mapped) in self.isa_irq_to_gsi.iter().enumerate() {
            if *mapped == gsi {
                return Some(irq as u8);
            }
        }

        // Fall back to identity mapping for GSIs 0-15.
        u8::try_from(gsi).ok()
    }

    fn drive_gsi_effective_level(&mut self, gsi: u32, level: bool) {
        match self.mode {
            PlatformInterruptMode::LegacyPic => {
                if let Some(irq) = self.legacy_pic_irq_for_gsi(gsi) {
                    if level {
                        self.pic.raise_irq(irq);
                    } else {
                        self.pic.lower_irq(irq);
                    }
                }
            }
            PlatformInterruptMode::Apic => {
                self.ioapic.lock().unwrap().set_irq_level(gsi, level);
            }
        }
    }

    fn update_gsi_assert_count(&mut self, gsi: u32, asserted: bool) {
        let idx = gsi as usize;
        let Some(count_slot) = self.gsi_assert_count.get_mut(idx) else {
            return;
        };

        if asserted {
            *count_slot = count_slot.saturating_add(1);
        } else if *count_slot > 0 {
            *count_slot -= 1;
        } else {
            // Unbalanced deassert: ignore. This can happen if a device attempts to deassert a line
            // that it never asserted (e.g. during restore fixups). Maintaining correctness for
            // shared GSIs relies on well-behaved sources only lowering after raising.
            return;
        }

        let baseline = self.gsi_restore_baseline.get(idx).copied().unwrap_or(false);
        let desired_level = baseline || *count_slot > 0;
        let prev = self.gsi_level.get(idx).copied().unwrap_or(false);
        if desired_level == prev {
            return;
        }
        self.set_gsi_level_internal(gsi, desired_level);
        self.drive_gsi_effective_level(gsi, desired_level);
    }
}

impl InterruptController for PlatformInterrupts {
    fn get_pending(&self) -> Option<u8> {
        self.get_pending_for_cpu(0)
    }

    fn acknowledge(&mut self, vector: u8) {
        self.acknowledge_for_cpu(0, vector);
    }

    fn eoi(&mut self, vector: u8) {
        self.eoi_for_cpu(0, vector);
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
        const TAG_LAPICS: u16 = 10;

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
        if self.lapics.len() == 1 {
            // Preserve the legacy single-LAPIC tag for smaller snapshots and backward
            // compatibility.
            w.field_bytes(TAG_LAPIC, self.lapics[0].save_state());
        } else {
            let mut enc = Encoder::new().u32(self.lapics.len() as u32);
            for lapic in &self.lapics {
                let state = lapic.save_state();
                enc = enc.u64(state.len() as u64).bytes(&state);
            }
            w.field_bytes(TAG_LAPICS, enc.finish());
        }

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
        const TAG_LAPICS: u16 = 10;

        const MAX_SNAPSHOT_GSI_LEVELS: usize = 4096;
        const MAX_SNAPSHOT_LAPICS: usize = 256;
        const MAX_SNAPSHOT_LAPIC_STATE_LEN: usize = 1024 * 1024;

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

        // Reset line-level tracking while keeping the restored PIC/IOAPIC/LAPIC state intact.
        //
        // `TAG_GSI_LEVEL` (decoded below) will repopulate `gsi_restore_baseline`/`gsi_level`.
        self.gsi_level.fill(false);
        self.gsi_restore_baseline.fill(false);
        self.gsi_assert_count.fill(0);
        for flag in self.pending_init.iter() {
            flag.store(false, Ordering::SeqCst);
        }

        // Clear pending INIT/SIPI events on restore.
        self.pending_sipi.lock().unwrap().fill(None);

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
            let decoded: Vec<bool> = levels
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

            // Treat the snapshotted line levels as a temporary baseline. Restored devices will
            // re-drive their own assertions into `gsi_assert_count`; snapshot consumers must call
            // `finalize_restore()` after those sync steps to convert the baseline into ref-counted
            // state.
            self.gsi_restore_baseline = decoded.clone();
            self.gsi_level = decoded;
            self.gsi_assert_count.clear();
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
        if let Some(buf) = r.bytes(TAG_LAPICS) {
            // SMP snapshots: restore LAPIC state by CPU index.
            //
            // This snapshot does not currently store a per-entry APIC ID mapping, so we require
            // `lapic_count` to match the runtime CPU/LAPIC count exactly and restore entries in
            // order.
            let mut d = Decoder::new(buf);
            let lapic_count = d.u32()? as usize;
            if lapic_count == 0 || lapic_count > MAX_SNAPSHOT_LAPICS {
                return Err(
                    aero_io_snapshot::io::state::SnapshotError::InvalidFieldEncoding("lapic_count"),
                );
            }
            if lapic_count != self.lapics.len() {
                return Err(
                    aero_io_snapshot::io::state::SnapshotError::InvalidFieldEncoding(
                        "lapic_count_mismatch",
                    ),
                );
            }

            for idx in 0..lapic_count {
                let entry_len_u64 = d.u64()?;
                let entry_len: usize = entry_len_u64.try_into().map_err(|_| {
                    aero_io_snapshot::io::state::SnapshotError::InvalidFieldEncoding(
                        "lapic_entry_len",
                    )
                })?;
                if entry_len == 0 || entry_len > MAX_SNAPSHOT_LAPIC_STATE_LEN {
                    return Err(
                        aero_io_snapshot::io::state::SnapshotError::InvalidFieldEncoding(
                            "lapic_entry_len",
                        ),
                    );
                }
                let entry = d.bytes(entry_len)?;
                self.lapics[idx].restore_state(entry)?;
            }
            d.finish()?;
        } else if let Some(buf) = r.bytes(TAG_LAPIC) {
            // Backward compatibility: old snapshots contain a single LAPIC.
            self.lapics[0].restore_state(buf)?;
        }

        let num_gsis = self.ioapic.lock().unwrap().num_redirection_entries();
        self.gsi_level.resize(num_gsis, false);
        self.gsi_restore_baseline.resize(num_gsis, false);
        self.gsi_assert_count.resize(num_gsis, 0);

        if self.mode == PlatformInterruptMode::Apic {
            // Re-synchronize asserted level-triggered IOAPIC lines into the LAPIC.
            //
            // This avoids losing interrupts on restore without clearing Remote-IRR; the IOAPIC
            // implementation gates level-triggered delivery on Remote-IRR.
            self.ioapic.lock().unwrap().sync_level_triggered();
        }

        // Invalidate cached `PlatformIrqLine` state. Restored devices will re-drive their own line
        // levels after restore.
        self.irq_line_generation.fetch_add(1, Ordering::SeqCst);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aero_interrupts::apic::{DeliveryMode, DestinationShorthand, Icr, Level};

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
    fn apic_mode_routes_ioapic_to_specific_apic_id_and_ack_eoi_flow() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // GSI1 -> vector 0x40, level-triggered, unmasked, destination APIC ID 1.
        let vector = 0x40u32;
        let low = vector | (1 << 15);
        let high = 1u32 << 24;
        program_ioapic_entry(&mut ints, 1, low, high);

        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending_for_apic(0), None);
        assert_eq!(ints.get_pending_for_apic(1), Some(vector as u8));

        ints.acknowledge_for_apic(1, vector as u8);
        assert_eq!(ints.get_pending_for_apic(1), None);

        // While still asserted, EOI should clear Remote-IRR and re-deliver.
        ints.eoi_for_apic(1, vector as u8);
        assert_eq!(ints.get_pending_for_apic(1), Some(vector as u8));
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
    fn level_triggered_remote_irr_cleared_by_eoi_from_non_bsp_lapic() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // Ensure LAPIC SVR[8] is set for both CPUs so they accept injected interrupts.
        for apic_id in [0u8, 1] {
            ints.lapic_mmio_write_for_apic(apic_id, 0xF0, &0x1FFu32.to_le_bytes());
        }

        // GSI1 -> vector 0x55, level-triggered, unmasked, destination=APIC1.
        let vector = 0x55u8;
        program_ioapic_entry(&mut ints, 1, u32::from(vector) | (1 << 15), 1 << 24);

        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending_for_cpu(1), Some(vector));
        assert_eq!(ints.get_pending_for_cpu(0), None);

        // ACK moves the vector into ISR; the IOAPIC must not redeliver while Remote-IRR is set.
        ints.acknowledge_for_cpu(1, vector);
        assert_eq!(ints.get_pending_for_cpu(1), None);
        assert_eq!(ints.get_pending_for_cpu(0), None);

        // EOI on the non-BSP LAPIC must clear Remote-IRR and trigger redelivery when the line is
        // still asserted.
        ints.eoi_for_cpu(1, vector);
        assert_eq!(ints.get_pending_for_cpu(1), Some(vector));
        assert_eq!(ints.get_pending_for_cpu(0), None);
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

    #[test]
    fn lapic_timer_tick_polls_all_lapics() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // Ensure both LAPICs are software-enabled (SVR[8] = 1).
        ints.lapic_mmio_write_for_apic(0, 0xF0, &0x1FFu32.to_le_bytes());
        ints.lapic_mmio_write_for_apic(1, 0xF0, &0x1FFu32.to_le_bytes());

        // Program LAPIC1 (CPU1) timer:
        // - Divide config = 0xB => divisor 1 (our model treats it as 1ns per tick).
        // - LVT timer = vector 0x55, unmasked, one-shot (default).
        // - Initial count = 10 ticks.
        ints.lapic_mmio_write_for_apic(1, 0x3E0, &0xBu32.to_le_bytes()); // Divide config
        ints.lapic_mmio_write_for_apic(1, 0x320, &0x55u32.to_le_bytes()); // LVT Timer
        ints.lapic_mmio_write_for_apic(1, 0x380, &10u32.to_le_bytes()); // Initial count

        ints.tick(9);
        assert!(!ints.lapics[1].is_pending(0x55));

        ints.tick(1);
        assert!(ints.lapics[1].is_pending(0x55));
        assert_eq!(ints.get_pending_for_apic(1), Some(0x55));

        // Ensure isolation: LAPIC0 should not see LAPIC1's timer vector pending in IRR.
        assert!(!ints.lapics[0].is_pending(0x55));
    }

    #[test]
    fn reset_lapic_clears_only_target_lapic_state() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // Inject distinct fixed interrupts into LAPIC0 and LAPIC1.
        ints.lapics[0].inject_fixed_interrupt(0x40);
        ints.lapics[1].inject_fixed_interrupt(0x41);

        assert!(ints.lapics[0].is_pending(0x40));
        assert!(ints.lapics[1].is_pending(0x41));

        ints.reset_lapic(1);

        // LAPIC1 should have been cleared.
        assert!(!ints.lapics[1].is_pending(0x41));

        // LAPIC0 should be unaffected.
        assert!(ints.lapics[0].is_pending(0x40));
    }

    #[test]
    fn reset_lapic_preserves_ioapic_routing_to_existing_sink_arc() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // Route GSI1 -> vector 0x60 to LAPIC1 (APIC ID 1), edge-triggered, unmasked.
        program_ioapic_entry(&mut ints, 1, 0x60, 1 << 24);

        // First delivery: LAPIC1 should receive it.
        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.lapics[1].get_pending_vector(), Some(0x60));
        assert!(ints.lapics[1].ack(0x60));
        ints.lapics[1].eoi();
        ints.lower_irq(InterruptInput::Gsi(1));

        // Reset LAPIC1's internal state. This should *not* require rebuilding the IOAPIC sink graph.
        ints.reset_lapic(1);
        // LAPIC reset clears SVR[8] (software enable); re-enable it so injected interrupts are
        // accepted.
        ints.lapic_mmio_write_for_apic(1, 0xF0, &(0x1FFu32).to_le_bytes());

        // Second delivery: should still route to LAPIC1 via the original `Arc<LocalApic>` held by
        // the IOAPIC sinks.
        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.lapics[1].get_pending_vector(), Some(0x60));
    }

    #[test]
    fn reset_lapic_disables_lapic_software_enable_bit() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        ints.reset_lapic(1);

        // Reset should restore the power-on SVR value (0xFF with software enable bit clear).
        let mut buf = [0u8; 4];
        ints.lapics[1].mmio_read(0xF0, &mut buf);
        assert_eq!(u32::from_le_bytes(buf), 0xFF);

        // With SVR[8] cleared, injected interrupts are silently dropped.
        let vector = 0x44u8;
        ints.lapics[1].inject_fixed_interrupt(vector);
        assert_eq!(ints.get_pending_for_apic(1), None);

        // Re-enable SVR[8] and ensure injection works again.
        ints.lapics[1].mmio_write(0xF0, &(0x1FFu32).to_le_bytes());
        ints.lapics[1].inject_fixed_interrupt(vector);
        assert_eq!(ints.get_pending_for_apic(1), Some(vector));
    }

    #[test]
    fn init_ipi_reset_keeps_target_lapic_enabled_for_ioapic_delivery() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // Route GSI1 -> vector 0x60 to LAPIC1 (APIC ID 1), edge-triggered, unmasked.
        program_ioapic_entry(&mut ints, 1, 0x60, 1 << 24);

        // Send INIT IPI (level=assert) from CPU0 -> CPU1 via ICR.
        ints.lapic_mmio_write_for_apic(0, 0x310, &((1u32 << 24).to_le_bytes()));
        ints.lapic_mmio_write_for_apic(0, 0x300, &(((5u32 << 8) | (1 << 14)).to_le_bytes()));
        assert!(
            ints.take_pending_init(1),
            "INIT IPI should set the pending-init flag"
        );

        // IOAPIC delivery should still work immediately after INIT reset (no guest SVR write).
        ints.raise_irq(InterruptInput::Gsi(1));
        assert_eq!(ints.get_pending_for_apic(0), None);
        assert_eq!(ints.get_pending_for_apic(1), Some(0x60));
    }

    fn lapic_read_u32(lapic: &LocalApic, offset: u64) -> u32 {
        let mut buf = [0u8; 4];
        lapic.mmio_read(offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    fn lapic_write_u32(lapic: &LocalApic, offset: u64, value: u32) {
        lapic.mmio_write(offset, &value.to_le_bytes());
    }

    #[test]
    fn snapshot_round_trip_preserves_multiple_lapics() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(4);
        ints.set_mode(PlatformInterruptMode::Apic);

        // Mutate each CPU's LAPIC state:
        // - SVR/TPR
        // - ICR_HIGH
        // - a one-shot timer interrupt pending in IRR
        for (cpu, lapic) in ints.lapics.iter().enumerate() {
            let cpu_u32 = cpu as u32;

            // SVR: set software enable + unique spurious vector.
            lapic_write_u32(lapic, 0xF0, (1 << 8) | (0xF0 + cpu_u32));
            // TPR: unique priority class per CPU (low enough not to mask our timer vector).
            lapic_write_u32(lapic, 0x80, cpu_u32 << 4);
            // ICR high: unique destination field.
            lapic_write_u32(lapic, 0x310, cpu_u32 << 24);

            // Program one-shot LAPIC timer (vector 0x40 + cpu).
            lapic_write_u32(lapic, 0x3E0, 0xB); // Divide config: divisor 1
            lapic_write_u32(lapic, 0x320, 0x40 + cpu_u32); // LVT Timer
            lapic_write_u32(lapic, 0x380, 10 + cpu_u32); // Initial count
        }

        // Advance time enough to fire all timers and make the IRR bit visible in snapshots.
        ints.tick(13);

        for cpu in 0..4 {
            let lapic = &ints.lapics[cpu];
            let cpu_u32 = cpu as u32;
            assert_eq!(lapic_read_u32(lapic, 0xF0), (1 << 8) | (0xF0 + cpu_u32));
            assert_eq!(lapic_read_u32(lapic, 0x80), cpu_u32 << 4);
            assert_eq!(lapic_read_u32(lapic, 0x310), cpu_u32 << 24);
            assert!(lapic.is_pending((0x40 + cpu_u32) as u8));
        }

        let bytes = ints.save_state();

        let mut restored = PlatformInterrupts::new_with_cpu_count(4);
        restored.load_state(&bytes).unwrap();
        restored.finalize_restore();

        assert_eq!(restored.lapic_clock.now_ns(), 13);

        for cpu in 0..4 {
            let lapic = &restored.lapics[cpu];
            let cpu_u32 = cpu as u32;
            assert_eq!(lapic_read_u32(lapic, 0xF0), (1 << 8) | (0xF0 + cpu_u32));
            assert_eq!(lapic_read_u32(lapic, 0x80), cpu_u32 << 4);
            assert_eq!(lapic_read_u32(lapic, 0x310), cpu_u32 << 24);
            assert!(lapic.is_pending((0x40 + cpu_u32) as u8));
        }
    }

    #[test]
    fn reset_preserves_lapic_count_and_ids_in_smp_mode() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(4);
        assert_eq!(ints.lapics.len(), 4);
        let initial_ids: Vec<u8> = ints.lapics.iter().map(|lapic| lapic.apic_id()).collect();
        assert_eq!(initial_ids, vec![0, 1, 2, 3]);

        ints.reset();

        assert_eq!(ints.lapics.len(), 4);
        let reset_ids: Vec<u8> = ints.lapics.iter().map(|lapic| lapic.apic_id()).collect();
        assert_eq!(reset_ids, vec![0, 1, 2, 3]);
    }

    #[test]
    fn lapic_icr_notifier_fires_with_decoded_vector_and_destination() {
        let ints = PlatformInterrupts::new_with_cpu_count(2);

        // Unknown APIC IDs should be a no-op.
        ints.register_icr_notifier(5, Arc::new(|_| panic!("unexpected ICR notifier call")));

        let seen: Arc<Mutex<Vec<Icr>>> = Arc::new(Mutex::new(Vec::new()));
        {
            let seen = seen.clone();
            ints.register_icr_notifier(
                0,
                Arc::new(move |icr| {
                    seen.lock().unwrap().push(icr);
                }),
            );
        }

        // Fixed IPI: vector 0x40 -> destination 1, destination shorthand AllExcludingSelf.
        ints.lapic_mmio_write_for_apic(0, 0x310, &(1u32 << 24).to_le_bytes());

        let icr_low = 0x40u32 | (1 << 14) | (3u32 << 18);
        let bytes = icr_low.to_le_bytes();
        ints.lapic_mmio_write_for_apic(0, 0x300, &bytes[0..2]);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![Icr {
                vector: 0x40,
                delivery_mode: DeliveryMode::Fixed,
                destination_mode: false,
                level: Level::Assert,
                destination_shorthand: DestinationShorthand::None,
                destination: 1,
            }]
        );

        ints.lapic_mmio_write_for_apic(0, 0x302, &bytes[2..4]);
        assert_eq!(
            *seen.lock().unwrap(),
            vec![
                Icr {
                    vector: 0x40,
                    delivery_mode: DeliveryMode::Fixed,
                    destination_mode: false,
                    level: Level::Assert,
                    destination_shorthand: DestinationShorthand::None,
                    destination: 1,
                },
                Icr {
                    vector: 0x40,
                    delivery_mode: DeliveryMode::Fixed,
                    destination_mode: false,
                    level: Level::Assert,
                    destination_shorthand: DestinationShorthand::AllExcludingSelf,
                    destination: 1,
                }
            ]
        );
    }

    #[test]
    fn reset_lapic_preserves_icr_notifier_registration() {
        let ints = PlatformInterrupts::new_with_cpu_count(2);

        let seen = Arc::new(Mutex::new(Vec::<aero_interrupts::apic::Icr>::new()));
        let seen_clone = seen.clone();
        ints.register_icr_notifier(
            1,
            Arc::new(move |icr| {
                seen_clone.lock().unwrap().push(icr);
            }),
        );

        // Send fixed IPI vector 0x40 from LAPIC1 -> destination 0.
        ints.lapic_mmio_write_for_apic(1, 0x310, &((0u32 << 24).to_le_bytes()));
        ints.lapic_mmio_write_for_apic(1, 0x300, &0x40u32.to_le_bytes());
        assert_eq!(seen.lock().unwrap().len(), 1);

        // Reset LAPIC1 and ensure the notifier still fires.
        seen.lock().unwrap().clear();
        ints.reset_lapic(1);

        ints.lapic_mmio_write_for_apic(1, 0x310, &((0u32 << 24).to_le_bytes()));
        ints.lapic_mmio_write_for_apic(1, 0x300, &0x41u32.to_le_bytes());
        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].destination, 0);
        assert_eq!(events[0].vector, 0x41);
    }

    #[test]
    fn ioapic_delivers_to_nonzero_destination_apic_id() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // GSI1 -> vector 0x45, edge-triggered, unmasked, destination APIC ID 1.
        let vector = 0x45u32;
        program_ioapic_entry(&mut ints, 1, vector, 1u32 << 24);

        ints.raise_irq(InterruptInput::Gsi(1));

        assert_eq!(ints.lapics[0].get_pending_vector(), None);
        assert_eq!(ints.lapics[1].get_pending_vector(), Some(vector as u8));
        // Acknowledge the interrupt on the destination LAPIC to clear its pending state.
        ints.acknowledge_for_apic(1, vector as u8);
        assert_eq!(ints.lapics[1].get_pending_vector(), None);
    }

    #[test]
    fn init_ipi_deassert_is_ignored_for_pending_init_and_reset_semantics() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        assert!(
            !ints.take_pending_init(1),
            "new PlatformInterrupts should not start with pending INIT state"
        );

        let lapic1 = &ints.lapics[1];

        // Prime LAPIC1 with non-default state that would be wiped by INIT assert/reset.
        lapic_write_u32(lapic1, 0xF0, 0x1FF); // Enable (SVR[8]=1).
        lapic_write_u32(lapic1, 0x80, 0x70); // TPR.
        lapic1.inject_fixed_interrupt(0x80);

        assert!(lapic1.is_pending(0x80));
        assert_eq!(lapic_read_u32(lapic1, 0x80), 0x70);

        // Send INIT deassert from LAPIC0 -> destination 1.
        ints.lapic_mmio_write_for_apic(0, 0x310, &((1u32 << 24).to_le_bytes()));
        ints.lapic_mmio_write_for_apic(0, 0x300, &((5u32 << 8).to_le_bytes()));

        assert!(
            !ints.take_pending_init(1),
            "INIT deassert must not record a pending INIT-reset event"
        );
        assert!(
            lapic1.is_pending(0x80),
            "INIT deassert must not reset target LAPIC state"
        );
        assert_eq!(
            lapic_read_u32(lapic1, 0x80),
            0x70,
            "INIT deassert must not reset LAPIC register state"
        );

        // Sanity: INIT assert should set the pending-init flag and reset the target LAPIC.
        ints.lapic_mmio_write_for_apic(0, 0x310, &((1u32 << 24).to_le_bytes()));
        ints.lapic_mmio_write_for_apic(0, 0x300, &(((5u32 << 8) | (1 << 14)).to_le_bytes()));

        assert!(
            ints.take_pending_init(1),
            "INIT assert should record a pending INIT-reset event"
        );
        assert!(!lapic1.is_pending(0x80));
        assert_eq!(lapic_read_u32(lapic1, 0x80), 0);
    }

    #[test]
    fn lapic_icr_notifier_survives_platform_reset() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);

        let seen = Arc::new(Mutex::new(Vec::<aero_interrupts::apic::Icr>::new()));
        let seen_clone = seen.clone();
        ints.register_icr_notifier(
            0,
            Arc::new(move |icr| {
                seen_clone.lock().unwrap().push(icr);
            }),
        );

        ints.reset();

        // Program destination APIC ID = 1 in ICR_HIGH.
        ints.lapic_mmio_write_for_apic(0, 0x310, &((1u32 << 24).to_le_bytes()));
        // Send fixed IPI vector 0x46.
        ints.lapic_mmio_write_for_apic(0, 0x300, &(0x46u32.to_le_bytes()));

        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].destination, 1);
        assert_eq!(events[0].vector, 0x46);
    }

    fn lapic_write_u32_for_cpu(ints: &PlatformInterrupts, cpu: usize, offset: u64, value: u32) {
        ints.lapic_mmio_write_for_cpu(cpu, offset, &value.to_le_bytes());
    }

    fn lapic_read_u32_for_cpu(ints: &PlatformInterrupts, cpu: usize, offset: u64) -> u32 {
        let mut buf = [0u8; 4];
        ints.lapic_mmio_read_for_cpu(cpu, offset, &mut buf);
        u32::from_le_bytes(buf)
    }

    #[test]
    fn fixed_ipi_injects_vector_into_target_lapic() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // CPU0 sends IPI vector 0x41 to CPU1.
        lapic_write_u32_for_cpu(&ints, 0, 0x310, (1u32) << 24); // ICR high: dest=1
        lapic_write_u32_for_cpu(&ints, 0, 0x300, 0x41); // ICR low: FIXED delivery, vector=0x41

        assert_eq!(ints.get_pending_for_cpu(0), None);
        assert_eq!(ints.get_pending_for_cpu(1), Some(0x41));
    }

    #[test]
    fn init_ipi_records_pending_init_and_resets_destination_lapic() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // LAPICs start enabled by PlatformInterrupts (SVR[8]=1).
        let svr_before = lapic_read_u32_for_cpu(&ints, 1, 0xF0);
        assert_ne!(svr_before & (1 << 8), 0);

        // CPU0 sends INIT (level=assert) to CPU1.
        lapic_write_u32_for_cpu(&ints, 0, 0x310, (1u32) << 24); // dest=1
        lapic_write_u32_for_cpu(&ints, 0, 0x300, (5u32 << 8) | (1u32 << 14)); // INIT + level=assert

        assert!(ints.take_pending_init(1));
        assert!(!ints.take_pending_init(1));

        // Destination LAPIC should be reset (SVR enable bit cleared).
        let svr_after = lapic_read_u32_for_cpu(&ints, 1, 0xF0);
        assert_eq!(svr_after & (1 << 8), 0);
    }

    #[test]
    fn sipi_ipi_records_startup_vector() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(2);
        ints.set_mode(PlatformInterruptMode::Apic);

        // CPU0 sends SIPI with vector 0x08 to CPU1.
        lapic_write_u32_for_cpu(&ints, 0, 0x310, (1u32) << 24); // dest=1
        lapic_write_u32_for_cpu(&ints, 0, 0x300, (6u32 << 8) | 0x08); // STARTUP + vector=0x08

        assert_eq!(ints.take_pending_sipi(1), Some(0x08));
        assert_eq!(ints.take_pending_sipi(1), None);
    }

    fn enable_lapic_svr(ints: &PlatformInterrupts, cpu_count: usize) {
        // The LAPIC model drops injected interrupts while the software enable bit is cleared.
        // Keep this explicit in tests so behaviour doesn't depend on constructor defaults.
        for cpu in 0..cpu_count {
            lapic_write_u32_for_cpu(ints, cpu, 0xF0, 0x1FF);
        }
    }

    fn send_fixed_ipi_shorthand(ints: &PlatformInterrupts, vector: u8, shorthand: u32) {
        // Destination field is ignored for shorthand delivery modes, but real guests still
        // program ICR_HIGH as part of the send sequence.
        lapic_write_u32_for_cpu(ints, 0, 0x310, 0);

        let icr_low = u32::from(vector) | (shorthand << 18);
        lapic_write_u32_for_cpu(ints, 0, 0x300, icr_low);
    }

    #[test]
    fn lapic_ipi_destination_shorthand_self_only() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(4);
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr(&ints, 4);

        // Destination shorthand: SelfOnly = 0b01.
        send_fixed_ipi_shorthand(&ints, 0x40, 0b01);

        assert_eq!(ints.get_pending_for_cpu(0), Some(0x40));
        for cpu in 1..4 {
            assert_eq!(ints.get_pending_for_cpu(cpu), None);
        }
    }

    #[test]
    fn lapic_ipi_destination_shorthand_all_including_self() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(4);
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr(&ints, 4);

        // Destination shorthand: AllIncludingSelf = 0b10.
        send_fixed_ipi_shorthand(&ints, 0x41, 0b10);

        for cpu in 0..4 {
            assert_eq!(ints.get_pending_for_cpu(cpu), Some(0x41));
        }
    }

    #[test]
    fn lapic_ipi_destination_shorthand_all_excluding_self() {
        let mut ints = PlatformInterrupts::new_with_cpu_count(4);
        ints.set_mode(PlatformInterruptMode::Apic);
        enable_lapic_svr(&ints, 4);

        // Destination shorthand: AllExcludingSelf = 0b11.
        send_fixed_ipi_shorthand(&ints, 0x42, 0b11);

        assert_eq!(ints.get_pending_for_cpu(0), None);
        for cpu in 1..4 {
            assert_eq!(ints.get_pending_for_cpu(cpu), Some(0x42));
        }
    }
}
