use crate::clock::Clock;
use crate::ioapic::GsiSink;
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

pub const HPET_MMIO_BASE: u64 = 0xFED0_0000;
pub const HPET_MMIO_SIZE: u64 = 0x400;

const REG_GENERAL_CAP_ID: u64 = 0x000;
const REG_GENERAL_CONFIG: u64 = 0x010;
const REG_GENERAL_INT_STATUS: u64 = 0x020;
const REG_MAIN_COUNTER: u64 = 0x0F0;

const REG_TIMER0_BASE: u64 = 0x100;
const TIMER_STRIDE: u64 = 0x20;
const REG_TIMER_CONFIG: u64 = 0x00;
const REG_TIMER_COMPARATOR: u64 = 0x08;
const REG_TIMER_FSB_ROUTE: u64 = 0x10;

const GEN_CONF_ENABLE: u64 = 1 << 0;
const GEN_CONF_LEGACY_ROUTE: u64 = 1 << 1;

/// In ACPI/MADT setups with an interrupt source override (ISO), the legacy PIT IRQ0
/// is commonly mapped to GSI2 in APIC mode (see the MADT emitted by `aero-acpi`).
///
/// HPET "LegacyReplacementRoute" mode routes Timer0 to the legacy timer interrupt
/// and Timer1 to the legacy RTC interrupt; we model that using these GSIs.
const LEGACY_TIMER_GSI: u32 = 2;
const LEGACY_RTC_GSI: u32 = 8;

fn apply_legacy_replacement_route(legacy: bool, timer_index: usize, programmed_route: u32) -> u32 {
    if !legacy {
        return programmed_route;
    }

    match timer_index {
        0 => LEGACY_TIMER_GSI,
        1 => LEGACY_RTC_GSI,
        _ => programmed_route,
    }
}

const TIMER_CFG_INT_LEVEL: u64 = 1 << 1;
const TIMER_CFG_INT_ENABLE: u64 = 1 << 2;
const TIMER_CFG_PERIODIC: u64 = 1 << 3;
const TIMER_CAP_PERIODIC: u64 = 1 << 4;
const TIMER_CAP_SIZE_64: u64 = 1 << 5;
const TIMER_CFG_SETVAL: u64 = 1 << 6;
const TIMER_CFG_32MODE: u64 = 1 << 8;
const TIMER_CFG_INT_ROUTE_SHIFT: u64 = 9;
const TIMER_CFG_INT_ROUTE_MASK: u64 = 0x1F << TIMER_CFG_INT_ROUTE_SHIFT;
const TIMER_CFG_FSB_ENABLE: u64 = 1 << 14;

const TIMER_WRITABLE_MASK: u64 = TIMER_CFG_INT_LEVEL
    | TIMER_CFG_INT_ENABLE
    | TIMER_CFG_PERIODIC
    | TIMER_CFG_SETVAL
    | TIMER_CFG_32MODE
    | TIMER_CFG_INT_ROUTE_MASK
    | TIMER_CFG_FSB_ENABLE;

#[derive(Debug, Clone)]
pub struct HpetCapabilities {
    pub vendor_id: u16,
    pub revision_id: u8,
    pub counter_size_64: bool,
    pub legacy_route_capable: bool,
    pub num_timers: usize,
    pub counter_clk_period_fs: u32,
}

#[derive(Debug, Clone)]
pub struct HpetConfig {
    pub capabilities: HpetCapabilities,
}

#[derive(Debug, Clone)]
struct HpetTimer {
    cap_bits: u64,
    config: u64,
    comparator: u64,
    period: u64,
    fsb_route: u64,
    armed: bool,
    irq_asserted: bool,
}

impl HpetTimer {
    fn new(route_cap_mask: u32, default_route: u8, periodic_capable: bool) -> Self {
        let mut cap_bits = (route_cap_mask as u64) << 32;
        if periodic_capable {
            cap_bits |= TIMER_CAP_PERIODIC;
        }
        cap_bits |= TIMER_CAP_SIZE_64;

        let config =
            ((default_route as u64) << TIMER_CFG_INT_ROUTE_SHIFT) & TIMER_CFG_INT_ROUTE_MASK;

        Self {
            cap_bits,
            config,
            comparator: 0,
            period: 0,
            fsb_route: 0,
            armed: false,
            irq_asserted: false,
        }
    }

    fn full_config(&self) -> u64 {
        self.cap_bits | self.config
    }

    fn is_periodic(&self) -> bool {
        self.config & TIMER_CFG_PERIODIC != 0
    }

    fn int_enabled(&self) -> bool {
        self.config & TIMER_CFG_INT_ENABLE != 0
    }

    fn is_level_triggered(&self) -> bool {
        self.config & TIMER_CFG_INT_LEVEL != 0
    }

    fn route(&self) -> u32 {
        ((self.config & TIMER_CFG_INT_ROUTE_MASK) >> TIMER_CFG_INT_ROUTE_SHIFT) as u32
    }
}

pub struct Hpet<C: Clock> {
    clock: C,
    config: HpetConfig,

    general_config: u64,
    general_int_status: u64,
    main_counter: u64,

    last_update_ns: u64,
    remainder_fs: u64,

    timers: Vec<HpetTimer>,
}

impl<C: Clock> IoSnapshot for Hpet<C> {
    const DEVICE_ID: [u8; 4] = *b"HPET";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_GENERAL_CONFIG: u16 = 1;
        const TAG_GENERAL_INT_STATUS: u16 = 2;
        const TAG_MAIN_COUNTER: u16 = 3;
        const TAG_REMAINDER_FS: u16 = 4;
        const TAG_TIMERS: u16 = 5;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_u64(TAG_GENERAL_CONFIG, self.general_config);
        w.field_u64(TAG_GENERAL_INT_STATUS, self.general_int_status);
        w.field_u64(TAG_MAIN_COUNTER, self.main_counter);
        w.field_u64(TAG_REMAINDER_FS, self.remainder_fs);

        let mut enc = Encoder::new().u32(self.timers.len() as u32);
        for timer in &self.timers {
            enc = enc
                .u64(timer.config)
                .u64(timer.comparator)
                .u64(timer.period)
                .u64(timer.fsb_route)
                .bool(timer.armed);
        }
        w.field_bytes(TAG_TIMERS, enc.finish());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_GENERAL_CONFIG: u16 = 1;
        const TAG_GENERAL_INT_STATUS: u16 = 2;
        const TAG_MAIN_COUNTER: u16 = 3;
        const TAG_REMAINDER_FS: u16 = 4;
        const TAG_TIMERS: u16 = 5;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        self.general_config = 0;
        self.general_int_status = 0;
        self.main_counter = 0;
        self.remainder_fs = 0;
        self.last_update_ns = self.clock.now_ns();

        for timer in &mut self.timers {
            // Preserve the reset/default route, but clear dynamic state.
            timer.config &= TIMER_CFG_INT_ROUTE_MASK;
            timer.comparator = 0;
            timer.period = 0;
            timer.fsb_route = 0;
            timer.armed = false;
            timer.irq_asserted = false;
        }

        if let Some(cfg) = r.u64(TAG_GENERAL_CONFIG)? {
            self.general_config = cfg & (GEN_CONF_ENABLE | GEN_CONF_LEGACY_ROUTE);
        }
        if let Some(sts) = r.u64(TAG_GENERAL_INT_STATUS)? {
            self.general_int_status = sts;
        }
        if let Some(counter) = r.u64(TAG_MAIN_COUNTER)? {
            self.main_counter = counter;
        }
        if let Some(rem) = r.u64(TAG_REMAINDER_FS)? {
            self.remainder_fs = rem;
        }

        if let Some(buf) = r.bytes(TAG_TIMERS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            for idx in 0..count {
                let config = d.u64()?;
                let comparator = d.u64()?;
                let period = d.u64()?;
                let fsb_route = d.u64()?;
                let armed = d.bool()?;

                if idx < self.timers.len() {
                    let timer = &mut self.timers[idx];
                    timer.config = config & TIMER_WRITABLE_MASK & !TIMER_CFG_FSB_ENABLE;
                    timer.comparator = comparator;
                    timer.period = if timer.is_periodic() { period } else { 0 };
                    timer.fsb_route = fsb_route;
                    timer.armed = armed;

                    // `irq_asserted` is not snapshotted: it is a runtime handshake with the
                    // interrupt sink. The first `poll()` after restore (or an explicit
                    // `sync_levels_to_sink()`) reasserts lines based on `general_int_status`
                    // and timer configuration.
                    timer.irq_asserted = false;
                }
            }
            d.finish()?;
        }

        Ok(())
    }
}

impl<C: Clock> Hpet<C> {
    pub fn new(clock: C, config: HpetConfig) -> Self {
        let now_ns = clock.now_ns();
        let route_cap_mask: u32 = 0x00ff_ffff;

        let mut timers = Vec::new();
        for timer_idx in 0..config.capabilities.num_timers {
            let default_route = match timer_idx {
                0 => 2,
                1 => 8,
                _ => 10,
            };
            timers.push(HpetTimer::new(route_cap_mask, default_route, true));
        }

        Self {
            clock,
            config,
            general_config: 0,
            general_int_status: 0,
            main_counter: 0,
            last_update_ns: now_ns,
            remainder_fs: 0,
            timers,
        }
    }

    pub fn new_default(clock: C) -> Self {
        Self::new(
            clock,
            HpetConfig {
                capabilities: HpetCapabilities {
                    vendor_id: 0x8086,
                    revision_id: 1,
                    counter_size_64: true,
                    legacy_route_capable: true,
                    num_timers: 3,
                    counter_clk_period_fs: 100_000_000,
                },
            },
        )
    }

    pub fn poll(&mut self, sink: &mut impl GsiSink) {
        self.update_main_counter();
        self.service_timers(sink);
    }

    /// Synchronizes the current level-triggered IRQ line levels into `sink`.
    ///
    /// This is primarily intended for snapshot restore flows: [`IoSnapshot::load_state()`]
    /// restores interrupt status bits and timer configuration, but it cannot access the
    /// platform interrupt sink. Callers should invoke this after restoring both the HPET
    /// and the interrupt controller to re-drive any pending level-triggered interrupts
    /// without advancing the HPET counter.
    pub fn sync_levels_to_sink(&mut self, sink: &mut impl GsiSink) {
        let legacy = self.general_config & GEN_CONF_LEGACY_ROUTE != 0;
        let enabled = self.enabled();

        for (idx, timer) in self.timers.iter_mut().enumerate() {
            let status_bit = 1u64 << idx;
            let gsi = apply_legacy_replacement_route(legacy, idx, timer.route());

            let pending = self.general_int_status & status_bit != 0;
            let should_assert =
                enabled && pending && timer.is_level_triggered() && timer.int_enabled();

            if should_assert {
                sink.raise_gsi(gsi);
                timer.irq_asserted = true;
            } else {
                sink.lower_gsi(gsi);
                timer.irq_asserted = false;
            }
        }
    }

    pub fn mmio_read(&mut self, offset: u64, size: usize, sink: &mut impl GsiSink) -> u64 {
        assert!(size == 1 || size == 2 || size == 4 || size == 8);
        assert!(offset < HPET_MMIO_SIZE);
        if size == 8 {
            assert_eq!(offset & 0x7, 0);
        }

        self.poll(sink);

        let aligned = offset & !0x7;
        let shift = (offset - aligned) * 8;
        let reg = self.read_aligned_u64(aligned);
        let mask = if size == 8 {
            u64::MAX
        } else {
            (1u64 << (size * 8)) - 1
        };
        (reg >> shift) & mask
    }

    pub fn mmio_write(&mut self, offset: u64, size: usize, value: u64, sink: &mut impl GsiSink) {
        assert!(size == 1 || size == 2 || size == 4 || size == 8);
        assert!(offset < HPET_MMIO_SIZE);
        if size == 8 {
            assert_eq!(offset & 0x7, 0);
        }

        self.poll(sink);

        let aligned = offset & !0x7;
        let shift = (offset - aligned) * 8;
        let mask = if size == 8 {
            u64::MAX
        } else {
            (1u64 << (size * 8)) - 1
        };

        let write_value = (value & mask) << shift;
        let write_mask = mask << shift;
        self.write_aligned_u64(aligned, write_value, write_mask, sink);

        self.service_timers(sink);
    }

    fn capabilities_reg(&self) -> u64 {
        let timers_minus_one = self.config.capabilities.num_timers.saturating_sub(1) as u64;
        let mut reg = self.config.capabilities.revision_id as u64;
        reg |= timers_minus_one << 8;
        if self.config.capabilities.counter_size_64 {
            reg |= 1 << 13;
        }
        if self.config.capabilities.legacy_route_capable {
            reg |= 1 << 15;
        }
        reg |= (self.config.capabilities.vendor_id as u64) << 16;
        reg |= (self.config.capabilities.counter_clk_period_fs as u64) << 32;
        reg
    }

    fn enabled(&self) -> bool {
        self.general_config & GEN_CONF_ENABLE != 0
    }

    fn update_main_counter(&mut self) {
        if !self.enabled() {
            return;
        }

        let now_ns = self.clock.now_ns();
        let elapsed_ns = now_ns.wrapping_sub(self.last_update_ns);
        self.last_update_ns = now_ns;

        let elapsed_fs = (elapsed_ns as u128) * 1_000_000u128 + (self.remainder_fs as u128);
        let period_fs = self.config.capabilities.counter_clk_period_fs as u128;
        if period_fs == 0 {
            return;
        }

        let ticks = (elapsed_fs / period_fs) as u64;
        self.remainder_fs = (elapsed_fs % period_fs) as u64;
        self.main_counter = self.main_counter.wrapping_add(ticks);
    }

    fn service_timers(&mut self, sink: &mut impl GsiSink) {
        if !self.enabled() {
            return;
        }

        for (idx, timer) in self.timers.iter_mut().enumerate() {
            let status_bit = 1u64 << idx;
            let legacy = self.general_config & GEN_CONF_LEGACY_ROUTE != 0;
            let gsi = apply_legacy_replacement_route(legacy, idx, timer.route());

            // Level-triggered IRQs are asserted for as long as the interrupt status bit
            // remains set, independent of whether the comparator is still armed.
            let pending = self.general_int_status & status_bit != 0;
            if timer.is_level_triggered() && timer.int_enabled() {
                match (pending, timer.irq_asserted) {
                    (true, false) => {
                        sink.raise_gsi(gsi);
                        timer.irq_asserted = true;
                    }
                    (false, true) => {
                        sink.lower_gsi(gsi);
                        timer.irq_asserted = false;
                    }
                    _ => {}
                }
            }

            if !timer.int_enabled() {
                continue;
            }
            if !timer.armed {
                continue;
            }

            if self.main_counter < timer.comparator {
                continue;
            }

            let was_pending = self.general_int_status & status_bit != 0;
            self.general_int_status |= status_bit;

            if timer.is_level_triggered() {
                if !timer.irq_asserted {
                    sink.raise_gsi(gsi);
                    timer.irq_asserted = true;
                }
            } else if !was_pending {
                sink.pulse_gsi(gsi);
            }

            if timer.is_periodic() && timer.period != 0 {
                let delta = self.main_counter.wrapping_sub(timer.comparator);
                let skips = delta / timer.period + 1;
                timer.comparator = timer
                    .comparator
                    .wrapping_add(timer.period.wrapping_mul(skips));
            } else {
                timer.armed = false;
            }
        }
    }

    fn read_aligned_u64(&self, offset: u64) -> u64 {
        match offset {
            REG_GENERAL_CAP_ID => self.capabilities_reg(),
            REG_GENERAL_CONFIG => self.general_config,
            REG_GENERAL_INT_STATUS => self.general_int_status,
            REG_MAIN_COUNTER => self.main_counter,
            _ if offset >= REG_TIMER0_BASE => {
                let timer_idx = ((offset - REG_TIMER0_BASE) / TIMER_STRIDE) as usize;
                let reg = (offset - REG_TIMER0_BASE) % TIMER_STRIDE;

                if timer_idx >= self.timers.len() {
                    return 0;
                }

                match reg {
                    REG_TIMER_CONFIG => self.timers[timer_idx].full_config(),
                    REG_TIMER_COMPARATOR => self.timers[timer_idx].comparator,
                    REG_TIMER_FSB_ROUTE => self.timers[timer_idx].fsb_route,
                    _ => 0,
                }
            }
            _ => 0,
        }
    }

    fn write_aligned_u64(
        &mut self,
        offset: u64,
        value: u64,
        write_mask: u64,
        sink: &mut impl GsiSink,
    ) {
        match offset {
            REG_GENERAL_CONFIG => {
                let before = self.general_config;
                let before_legacy = before & GEN_CONF_LEGACY_ROUTE != 0;
                let mut new = (before & !write_mask) | (value & write_mask);
                new &= GEN_CONF_ENABLE | GEN_CONF_LEGACY_ROUTE;

                let was_enabled = before & GEN_CONF_ENABLE != 0;
                let now_enabled = new & GEN_CONF_ENABLE != 0;
                let after_legacy = new & GEN_CONF_LEGACY_ROUTE != 0;
                let legacy_changed = before_legacy != after_legacy;
                self.general_config = new;

                if !was_enabled && now_enabled {
                    self.last_update_ns = self.clock.now_ns();
                    self.remainder_fs = 0;
                }

                // If HPET is disabled while level-triggered lines are asserted, deassert them to
                // avoid leaving the interrupt controller stuck in an asserted state.
                if was_enabled && !now_enabled {
                    for (timer_idx, timer) in self.timers.iter_mut().enumerate() {
                        if !timer.irq_asserted {
                            continue;
                        }
                        let gsi =
                            apply_legacy_replacement_route(before_legacy, timer_idx, timer.route());
                        sink.lower_gsi(gsi);
                        timer.irq_asserted = false;
                    }
                    return;
                }

                // Legacy replacement changes the effective route of timer0/timer1. If a level
                // interrupt is currently asserted, move it to the new destination.
                if legacy_changed {
                    for (timer_idx, timer) in self.timers.iter_mut().enumerate() {
                        if !timer.irq_asserted {
                            continue;
                        }

                        let before_gsi =
                            apply_legacy_replacement_route(before_legacy, timer_idx, timer.route());
                        let after_gsi =
                            apply_legacy_replacement_route(after_legacy, timer_idx, timer.route());
                        if before_gsi != after_gsi {
                            sink.lower_gsi(before_gsi);
                            sink.raise_gsi(after_gsi);
                        }
                    }
                }
            }
            REG_GENERAL_INT_STATUS => {
                let clear = value & write_mask;
                let before = self.general_int_status;
                self.general_int_status &= !clear;
                let cleared = before & clear;

                for timer_idx in 0..self.timers.len() {
                    let bit = 1u64 << timer_idx;
                    if cleared & bit == 0 {
                        continue;
                    }

                    let (route, is_level, is_asserted) = {
                        let timer = &self.timers[timer_idx];
                        (
                            timer.route(),
                            timer.is_level_triggered(),
                            timer.irq_asserted,
                        )
                    };
                    if is_level && is_asserted {
                        let legacy = self.general_config & GEN_CONF_LEGACY_ROUTE != 0;
                        let gsi = apply_legacy_replacement_route(legacy, timer_idx, route);

                        sink.lower_gsi(gsi);
                        self.timers[timer_idx].irq_asserted = false;
                    }
                }
            }
            REG_MAIN_COUNTER => {
                let new = (self.main_counter & !write_mask) | (value & write_mask);
                self.main_counter = new;
                self.last_update_ns = self.clock.now_ns();
                self.remainder_fs = 0;
            }
            _ if offset >= REG_TIMER0_BASE => {
                let timer_idx = ((offset - REG_TIMER0_BASE) / TIMER_STRIDE) as usize;
                let reg = (offset - REG_TIMER0_BASE) % TIMER_STRIDE;

                if timer_idx >= self.timers.len() {
                    return;
                }

                match reg {
                    REG_TIMER_CONFIG => {
                        let timer = &mut self.timers[timer_idx];
                        let before = timer.config;
                        let before_route = timer.route();
                        let legacy = self.general_config & GEN_CONF_LEGACY_ROUTE != 0;
                        let before_gsi =
                            apply_legacy_replacement_route(legacy, timer_idx, before_route);

                        let mut new = (before & !write_mask) | (value & write_mask);
                        new &= TIMER_WRITABLE_MASK;
                        new &= !TIMER_CFG_FSB_ENABLE;

                        timer.config = (timer.config & !TIMER_WRITABLE_MASK) | new;

                        let after_route = timer.route();
                        let after_gsi =
                            apply_legacy_replacement_route(legacy, timer_idx, after_route);

                        let before_int = before & TIMER_CFG_INT_ENABLE != 0;
                        let after_int = timer.config & TIMER_CFG_INT_ENABLE != 0;
                        let before_level = before & TIMER_CFG_INT_LEVEL != 0;
                        let after_level = timer.is_level_triggered();

                        if timer.irq_asserted {
                            // If a level interrupt is asserted and the guest either disables the
                            // interrupt, or switches the timer to edge-triggered mode, deassert it
                            // immediately.
                            if (before_int && !after_int) || (before_level && !after_level) {
                                sink.lower_gsi(before_gsi);
                                timer.irq_asserted = false;
                            } else if before_gsi != after_gsi {
                                // Route changed while asserted; move the line.
                                sink.lower_gsi(before_gsi);
                                if after_level {
                                    sink.raise_gsi(after_gsi);
                                } else {
                                    timer.irq_asserted = false;
                                }
                            }
                        }

                        if timer.config & TIMER_CFG_PERIODIC == 0 {
                            timer.period = 0;
                        }
                    }
                    REG_TIMER_COMPARATOR => {
                        let timer = &mut self.timers[timer_idx];
                        let new_value = (timer.comparator & !write_mask) | (value & write_mask);

                        if timer.is_periodic() {
                            if (timer.config & TIMER_CFG_SETVAL != 0) || timer.period == 0 {
                                timer.period = new_value;
                                timer.comparator = self.main_counter.wrapping_add(timer.period);
                                timer.config &= !TIMER_CFG_SETVAL;
                            } else {
                                timer.comparator = new_value;
                            }
                        } else {
                            timer.period = 0;
                            timer.comparator = new_value;
                        }
                        timer.armed = true;
                    }
                    REG_TIMER_FSB_ROUTE => {
                        let timer = &mut self.timers[timer_idx];
                        timer.fsb_route = (timer.fsb_route & !write_mask) | (value & write_mask);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// [`memory::Bus`] MMIO adapter for [`Hpet`].
///
/// The core HPET model requires a [`GsiSink`] to deliver timer interrupts. The
/// [`memory::MmioHandler`] trait doesn't carry an interrupt sink parameter, so
/// this wrapper bundles the HPET instance with a sink and forwards reads/writes.
pub struct HpetMmio<C: Clock, S: GsiSink> {
    hpet: Hpet<C>,
    sink: S,
}

impl<C: Clock, S: GsiSink> HpetMmio<C, S> {
    pub fn new(hpet: Hpet<C>, sink: S) -> Self {
        Self { hpet, sink }
    }

    pub fn hpet_mut(&mut self) -> &mut Hpet<C> {
        &mut self.hpet
    }

    pub fn sink_mut(&mut self) -> &mut S {
        &mut self.sink
    }
}

impl<C: Clock, S: GsiSink> memory::MmioHandler for HpetMmio<C, S> {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        self.hpet.mmio_read(offset, size, &mut self.sink)
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        self.hpet.mmio_write(offset, size, value, &mut self.sink);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::ioapic::{GsiEvent, IoApic};

    #[test]
    fn enable_bit_gates_counter_increment() {
        let clock = ManualClock::new();
        let mut ioapic = IoApic::default();
        let mut hpet = Hpet::new_default(clock.clone());

        clock.advance_ns(1_000);
        hpet.poll(&mut ioapic);
        assert_eq!(hpet.mmio_read(REG_MAIN_COUNTER, 8, &mut ioapic), 0);

        hpet.mmio_write(REG_GENERAL_CONFIG, 8, GEN_CONF_ENABLE, &mut ioapic);
        clock.advance_ns(1_000);
        hpet.poll(&mut ioapic);
        assert_eq!(hpet.mmio_read(REG_MAIN_COUNTER, 8, &mut ioapic), 10);

        hpet.mmio_write(REG_GENERAL_CONFIG, 8, 0, &mut ioapic);
        clock.advance_ns(1_000);
        hpet.poll(&mut ioapic);
        assert_eq!(hpet.mmio_read(REG_MAIN_COUNTER, 8, &mut ioapic), 10);
    }

    #[test]
    fn comparator_fires_at_expected_time() {
        let clock = ManualClock::new();
        let mut ioapic = IoApic::default();
        let mut hpet = Hpet::new_default(clock.clone());

        hpet.mmio_write(REG_GENERAL_CONFIG, 8, GEN_CONF_ENABLE, &mut ioapic);
        let timer0_cfg = hpet.mmio_read(REG_TIMER0_BASE + REG_TIMER_CONFIG, 8, &mut ioapic);
        hpet.mmio_write(
            REG_TIMER0_BASE + REG_TIMER_CONFIG,
            8,
            timer0_cfg | TIMER_CFG_INT_ENABLE,
            &mut ioapic,
        );
        hpet.mmio_write(REG_TIMER0_BASE + REG_TIMER_COMPARATOR, 8, 5, &mut ioapic);

        clock.advance_ns(400);
        hpet.poll(&mut ioapic);
        assert!(ioapic.take_events().is_empty());

        clock.advance_ns(100);
        hpet.poll(&mut ioapic);
        assert_eq!(
            ioapic.take_events(),
            vec![GsiEvent::Raise(2), GsiEvent::Lower(2)]
        );

        assert_ne!(
            hpet.mmio_read(REG_GENERAL_INT_STATUS, 8, &mut ioapic) & 1,
            0
        );
    }

    #[test]
    fn interrupt_status_is_write_one_to_clear() {
        let clock = ManualClock::new();
        let mut ioapic = IoApic::default();
        let mut hpet = Hpet::new_default(clock.clone());

        hpet.mmio_write(REG_GENERAL_CONFIG, 8, GEN_CONF_ENABLE, &mut ioapic);
        let timer0_cfg = hpet.mmio_read(REG_TIMER0_BASE + REG_TIMER_CONFIG, 8, &mut ioapic);
        hpet.mmio_write(
            REG_TIMER0_BASE + REG_TIMER_CONFIG,
            8,
            timer0_cfg | TIMER_CFG_INT_ENABLE | TIMER_CFG_INT_LEVEL,
            &mut ioapic,
        );
        hpet.mmio_write(REG_TIMER0_BASE + REG_TIMER_COMPARATOR, 8, 1, &mut ioapic);

        clock.advance_ns(100);
        hpet.poll(&mut ioapic);
        assert!(ioapic.is_asserted(2));
        assert_ne!(
            hpet.mmio_read(REG_GENERAL_INT_STATUS, 8, &mut ioapic) & 1,
            0
        );

        hpet.mmio_write(REG_GENERAL_INT_STATUS, 8, 1, &mut ioapic);
        assert_eq!(
            hpet.mmio_read(REG_GENERAL_INT_STATUS, 8, &mut ioapic) & 1,
            0
        );
        assert!(!ioapic.is_asserted(2));
    }
}
