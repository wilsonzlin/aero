//! Intel 8254 Programmable Interval Timer (PIT) model.
//!
//! This implementation focuses on the subset required for PC BIOS / OS bringup:
//! - Channels 0-2 on ports 0x40-0x43.
//! - Channel 0 modes 2 (rate generator) and 3 (square wave).
//! - Lobyte/hibyte sequencing, count latching, and a simplified read-back command.
//!
//! Timing is deterministic: time progresses only via [`Pit8254::advance_ns`], which
//! converts nanoseconds into PIT input clock ticks (1.193182 MHz).

use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_platform::interrupts::{InterruptInput, PlatformInterrupts};
use aero_platform::io::{IoPortBus, PortIoDevice};
use core::fmt;
use std::cell::RefCell;
use std::rc::Rc;

pub const PIT_CH0: u16 = 0x40;
pub const PIT_CH1: u16 = 0x41;
pub const PIT_CH2: u16 = 0x42;
pub const PIT_CMD: u16 = 0x43;

/// PIT input clock frequency (Hz).
pub const PIT_HZ: u64 = 1_193_182;

const NS_PER_SEC: u128 = 1_000_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AccessMode {
    LatchCount,
    LobyteOnly,
    HibyteOnly,
    LobyteHibyte,
}

impl AccessMode {
    fn from_bits(bits: u8) -> Self {
        match bits & 0b11 {
            0b00 => AccessMode::LatchCount,
            0b01 => AccessMode::LobyteOnly,
            0b10 => AccessMode::HibyteOnly,
            0b11 => AccessMode::LobyteHibyte,
            _ => unreachable!(),
        }
    }

    fn status_bits(self) -> u8 {
        match self {
            AccessMode::LatchCount => 0b00,
            AccessMode::LobyteOnly => 0b01,
            AccessMode::HibyteOnly => 0b10,
            AccessMode::LobyteHibyte => 0b11,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BytePhase {
    Low,
    High,
}

impl BytePhase {
    // Intentionally no helpers; the PIT's byte sequencing is easier to reason about
    // explicitly at the call site.
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LatchedValue {
    value: u16,
    phase: BytePhase,
}

impl LatchedValue {
    fn new(value: u16) -> Self {
        Self {
            value,
            phase: BytePhase::Low,
        }
    }
}

#[derive(Clone, Copy)]
struct Channel {
    /// Mode number (0-5), with 6/7 aliases folded to 2/3.
    mode: u8,
    bcd: bool,
    access_mode: AccessMode,
    write_phase: BytePhase,
    read_phase: BytePhase,
    write_latch_low: u8,

    /// True if a new mode has been set but a full count has not been written yet.
    null_count: bool,

    /// Reload value, with 0 representing "not yet programmed".
    reload: u32,
    /// Ticks into the current period (0..reload-1).
    phase_ticks: u32,

    latched_count: Option<LatchedValue>,
    latched_status: Option<u8>,
}

impl fmt::Debug for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Channel")
            .field("mode", &self.mode)
            .field("bcd", &self.bcd)
            .field("access_mode", &self.access_mode)
            .field("write_phase", &self.write_phase)
            .field("read_phase", &self.read_phase)
            .field("null_count", &self.null_count)
            .field("reload", &self.reload)
            .field("phase_ticks", &self.phase_ticks)
            .finish()
    }
}

impl Default for Channel {
    fn default() -> Self {
        Self {
            mode: 0,
            bcd: false,
            access_mode: AccessMode::LobyteHibyte,
            write_phase: BytePhase::Low,
            read_phase: BytePhase::Low,
            write_latch_low: 0,
            null_count: true,
            reload: 0,
            phase_ticks: 0,
            latched_count: None,
            latched_status: None,
        }
    }
}

impl Channel {
    fn effective_reload(&self) -> Option<u32> {
        if self.reload == 0 {
            None
        } else {
            Some(self.reload)
        }
    }

    fn set_mode(&mut self, access_mode: AccessMode, mode_bits: u8, bcd: bool) {
        self.access_mode = access_mode;
        self.mode = match mode_bits {
            0..=5 => mode_bits,
            6 => 2,
            7 => 3,
            _ => unreachable!(),
        };
        self.bcd = bcd;
        self.null_count = true;
        self.write_phase = BytePhase::Low;
        self.read_phase = BytePhase::Low;
        self.latched_count = None;
        self.latched_status = None;
        self.phase_ticks = 0;
    }

    fn load_count_raw(&mut self, raw: u16) {
        let divisor = if raw == 0 { 65_536 } else { raw as u32 };
        self.reload = divisor;
        self.phase_ticks = 0;
        self.null_count = false;
    }

    fn current_out(&self) -> bool {
        // OUT behaviour is only approximated for the modes we care about.
        let Some(reload) = self.effective_reload() else {
            return true;
        };

        match self.mode {
            2 => {
                // Mode 2: high for reload-1 ticks, low for 1 tick.
                self.phase_ticks != reload - 1
            }
            3 => {
                // Mode 3: square wave, with high half possibly one tick longer for odd reload.
                let reload = if reload == 1 { 2 } else { reload };
                let high_ticks = reload.div_ceil(2);
                self.phase_ticks < high_ticks
            }
            _ => true,
        }
    }

    fn current_count_raw(&self) -> u16 {
        let Some(reload) = self.effective_reload() else {
            return 0;
        };

        match self.mode {
            2 => {
                let remaining = reload.saturating_sub(self.phase_ticks);
                if remaining == 65_536 {
                    0
                } else {
                    remaining as u16
                }
            }
            3 => {
                let reload = if reload == 1 { 2 } else { reload };
                let high_ticks = reload.div_ceil(2);
                let low_ticks = reload / 2;
                let (ticks_into_half, half_len) = if self.phase_ticks < high_ticks {
                    (self.phase_ticks, high_ticks)
                } else {
                    (self.phase_ticks - high_ticks, low_ticks.max(1))
                };
                let remaining_ticks = half_len.saturating_sub(ticks_into_half);
                let counter = remaining_ticks.saturating_mul(2);
                if counter == 65_536 {
                    0
                } else {
                    counter as u16
                }
            }
            _ => 0,
        }
    }

    fn latch_count(&mut self) {
        // Real hardware ignores additional latch commands until the latched value has
        // been fully read; doing the same avoids surprising guest code that issues
        // redundant latch commands in tight loops.
        if self.latched_count.is_some() {
            return;
        }
        let raw = self.current_count_raw();
        self.latched_count = Some(LatchedValue::new(raw));
        self.read_phase = BytePhase::Low;
    }

    fn latch_status(&mut self) {
        if self.latched_status.is_some() {
            return;
        }
        let out = self.current_out();
        let rw = self.access_mode.status_bits();
        let status = ((out as u8) << 7)
            | ((self.null_count as u8) << 6)
            | (rw << 4)
            | ((self.mode & 0b111) << 1)
            | (self.bcd as u8);
        self.latched_status = Some(status);
        self.read_phase = BytePhase::Low;
    }

    fn advance_ticks(&mut self, ticks: u64) -> u64 {
        let Some(reload) = self.effective_reload() else {
            return 0;
        };

        match self.mode {
            2 | 3 => {
                let reload = if self.mode == 3 && reload == 1 {
                    2
                } else {
                    reload
                };
                let total = self.phase_ticks as u64 + ticks;
                let pulses = total / reload as u64;
                self.phase_ticks = (total % reload as u64) as u32;
                pulses
            }
            _ => 0,
        }
    }

    fn read_data(&mut self) -> u8 {
        if let Some(status) = self.latched_status.take() {
            self.read_phase = BytePhase::Low;
            return status;
        }

        let raw = if let Some(latched) = self.latched_count.as_mut() {
            latched.value
        } else {
            self.current_count_raw()
        };

        let (byte, next_phase) = match self.access_mode {
            AccessMode::LobyteOnly => ((raw & 0x00FF) as u8, BytePhase::Low),
            AccessMode::HibyteOnly => (((raw >> 8) & 0x00FF) as u8, BytePhase::Low),
            AccessMode::LobyteHibyte => match self.read_phase {
                BytePhase::Low => ((raw & 0x00FF) as u8, BytePhase::High),
                BytePhase::High => (((raw >> 8) & 0x00FF) as u8, BytePhase::Low),
            },
            AccessMode::LatchCount => ((raw & 0x00FF) as u8, BytePhase::Low),
        };

        match self.access_mode {
            AccessMode::LobyteHibyte => {
                self.read_phase = next_phase;
                if let Some(latched) = self.latched_count.as_mut() {
                    latched.phase = next_phase;
                    if next_phase == BytePhase::Low {
                        self.latched_count = None;
                    }
                }
            }
            AccessMode::LobyteOnly | AccessMode::HibyteOnly => {
                self.read_phase = BytePhase::Low;
                self.latched_count = None;
            }
            AccessMode::LatchCount => {
                self.read_phase = BytePhase::Low;
                self.latched_count = None;
            }
        }

        byte
    }

    fn write_data(&mut self, val: u8) {
        match self.access_mode {
            AccessMode::LatchCount => {
                // Treat writes as lobyte-only if misprogrammed; this matches the "don't crash"
                // goal and avoids getting stuck.
                self.load_count_raw(val as u16);
            }
            AccessMode::LobyteOnly => {
                let high = (self.reload as u16) & 0xFF00;
                self.load_count_raw(high | (val as u16));
            }
            AccessMode::HibyteOnly => {
                let low = (self.reload as u16) & 0x00FF;
                self.load_count_raw(low | ((val as u16) << 8));
            }
            AccessMode::LobyteHibyte => match self.write_phase {
                BytePhase::Low => {
                    self.write_latch_low = val;
                    self.write_phase = BytePhase::High;
                }
                BytePhase::High => {
                    let raw = u16::from_le_bytes([self.write_latch_low, val]);
                    self.load_count_raw(raw);
                    self.write_phase = BytePhase::Low;
                }
            },
        }
    }
}

/// A deterministic model of the PIT 8254.
#[derive(Default)]
pub struct Pit8254 {
    channels: [Channel; 3],
    ns_remainder: u128,
    irq0_pulses: u64,
    irq0_callback: Option<Box<dyn FnMut() + 'static>>,
}

impl Pit8254 {
    /// Creates a PIT with no IRQ output connected.
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset the PIT back to its power-on state.
    ///
    /// This preserves any host-side IRQ wiring previously connected via
    /// [`Pit8254::connect_irq0`] / [`Pit8254::connect_irq0_to_platform_interrupts`].
    pub fn reset(&mut self) {
        let irq0_callback = self.irq0_callback.take();
        *self = Self::default();
        self.irq0_callback = irq0_callback;
    }

    /// Connect a callback that will be invoked once per IRQ0 pulse.
    pub fn connect_irq0<F>(&mut self, callback: F)
    where
        F: FnMut() + 'static,
    {
        self.irq0_callback = Some(Box::new(callback));
    }

    /// Convenience helper to route PIT IRQ0 pulses to the platform interrupt router.
    ///
    /// This uses [`InterruptInput::IsaIrq(0)`], allowing the router to apply any
    /// MADT interrupt source overrides (ISOs) when operating in IOAPIC mode.
    pub fn connect_irq0_to_platform_interrupts(
        &mut self,
        interrupts: Rc<RefCell<PlatformInterrupts>>,
    ) {
        self.connect_irq0(move || {
            let mut interrupts = interrupts.borrow_mut();
            interrupts.raise_irq(InterruptInput::IsaIrq(0));
            interrupts.lower_irq(InterruptInput::IsaIrq(0));
        });
    }

    /// Drain and return the number of IRQ0 pulses generated since the last call.
    pub fn take_irq0_pulses(&mut self) -> u64 {
        let pulses = self.irq0_pulses;
        self.irq0_pulses = 0;
        pulses
    }

    /// Advance the PIT's timebase by `ns` nanoseconds.
    pub fn advance_ns(&mut self, ns: u64) {
        let total = self.ns_remainder + (ns as u128) * (PIT_HZ as u128);
        let ticks = total / NS_PER_SEC;
        self.ns_remainder = total % NS_PER_SEC;
        self.advance_ticks(ticks as u64);
    }

    /// Advance by a number of PIT input clock ticks.
    pub fn advance_ticks(&mut self, ticks: u64) {
        if ticks == 0 {
            return;
        }

        let pulses = self.channels[0].advance_ticks(ticks);
        if pulses != 0 {
            self.irq0_pulses = self.irq0_pulses.saturating_add(pulses);
            if let Some(cb) = self.irq0_callback.as_mut() {
                for _ in 0..pulses {
                    cb();
                }
            }
        }

        // Channels 1 and 2 may be stubbed; we still advance them so that reads return
        // sensible values when guest code expects them to count.
        self.channels[1].advance_ticks(ticks);
        self.channels[2].advance_ticks(ticks);
    }

    /// Read from an I/O port.
    pub fn port_read(&mut self, port: u16, size: u8) -> u32 {
        match size {
            1 => self.port_read_u8(port) as u32,
            2 => {
                let lo = self.port_read_u8(port) as u32;
                let hi = self.port_read_u8(port) as u32;
                lo | (hi << 8)
            }
            4 => {
                let b0 = self.port_read_u8(port) as u32;
                let b1 = self.port_read_u8(port) as u32;
                let b2 = self.port_read_u8(port) as u32;
                let b3 = self.port_read_u8(port) as u32;
                b0 | (b1 << 8) | (b2 << 16) | (b3 << 24)
            }
            _ => self.port_read_u8(port) as u32,
        }
    }

    /// Write to an I/O port.
    pub fn port_write(&mut self, port: u16, size: u8, val: u32) {
        match size {
            1 => self.port_write_u8(port, val as u8),
            2 => {
                self.port_write_u8(port, (val & 0xFF) as u8);
                self.port_write_u8(port, ((val >> 8) & 0xFF) as u8);
            }
            4 => {
                self.port_write_u8(port, (val & 0xFF) as u8);
                self.port_write_u8(port, ((val >> 8) & 0xFF) as u8);
                self.port_write_u8(port, ((val >> 16) & 0xFF) as u8);
                self.port_write_u8(port, ((val >> 24) & 0xFF) as u8);
            }
            _ => self.port_write_u8(port, val as u8),
        }
    }

    fn port_read_u8(&mut self, port: u16) -> u8 {
        match port {
            PIT_CH0 => self.channels[0].read_data(),
            PIT_CH1 => self.channels[1].read_data(),
            PIT_CH2 => self.channels[2].read_data(),
            PIT_CMD => 0,
            _ => 0xFF,
        }
    }

    fn port_write_u8(&mut self, port: u16, val: u8) {
        match port {
            PIT_CH0 => self.channels[0].write_data(val),
            PIT_CH1 => self.channels[1].write_data(val),
            PIT_CH2 => self.channels[2].write_data(val),
            PIT_CMD => self.write_control(val),
            _ => {}
        }
    }

    fn write_control(&mut self, val: u8) {
        let sel = (val >> 6) & 0b11;
        if sel == 0b11 {
            // Read-back command. Simplified but compatible.
            let latch_count = (val & 0x20) == 0;
            let latch_status = (val & 0x10) == 0;

            for channel in 0..3 {
                let sel_bit = 1u8 << (channel + 1); // bit1=ch0, bit2=ch1, bit3=ch2
                if (val & sel_bit) == 0 {
                    if latch_count {
                        self.channels[channel].latch_count();
                    }
                    if latch_status {
                        self.channels[channel].latch_status();
                    }
                }
            }
            return;
        }

        let channel = sel as usize;
        let access = AccessMode::from_bits((val >> 4) & 0b11);
        if access == AccessMode::LatchCount {
            self.channels[channel].latch_count();
            return;
        }

        let mode_bits = (val >> 1) & 0b111;
        let bcd = (val & 0b1) != 0;
        self.channels[channel].set_mode(access, mode_bits, bcd);
    }
}

impl IoSnapshot for Pit8254 {
    const DEVICE_ID: [u8; 4] = *b"PIT4";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_NS_REMAINDER: u16 = 1;
        const TAG_IRQ0_PULSES: u16 = 2;
        const TAG_CHANNELS: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        // `ns_remainder` is always < 1e9, but keep it as u64 for clarity.
        w.field_u64(TAG_NS_REMAINDER, self.ns_remainder as u64);
        w.field_u64(TAG_IRQ0_PULSES, self.irq0_pulses);

        let mut enc = Encoder::new().u32(self.channels.len() as u32);
        for ch in &self.channels {
            let write_phase = match ch.write_phase {
                BytePhase::Low => 0u8,
                BytePhase::High => 1u8,
            };
            let read_phase = match ch.read_phase {
                BytePhase::Low => 0u8,
                BytePhase::High => 1u8,
            };

            enc = enc
                .u8(ch.mode)
                .bool(ch.bcd)
                .u8(ch.access_mode.status_bits())
                .u8(write_phase)
                .u8(read_phase)
                .u8(ch.write_latch_low)
                .bool(ch.null_count)
                .u32(ch.reload)
                .u32(ch.phase_ticks);

            if let Some(latched) = ch.latched_count {
                let phase = match latched.phase {
                    BytePhase::Low => 0u8,
                    BytePhase::High => 1u8,
                };
                enc = enc.bool(true).u16(latched.value).u8(phase);
            } else {
                enc = enc.bool(false);
            }

            if let Some(status) = ch.latched_status {
                enc = enc.bool(true).u8(status);
            } else {
                enc = enc.bool(false);
            }
        }
        w.field_bytes(TAG_CHANNELS, enc.finish());

        // `irq0_callback` is a host wiring detail; it is intentionally not serialized.
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_NS_REMAINDER: u16 = 1;
        const TAG_IRQ0_PULSES: u16 = 2;
        const TAG_CHANNELS: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Preserve host wiring while resetting to a deterministic baseline.
        let irq0_callback = self.irq0_callback.take();
        *self = Self::default();
        self.irq0_callback = irq0_callback;

        if let Some(ns_rem) = r.u64(TAG_NS_REMAINDER)? {
            self.ns_remainder = ns_rem as u128;
        }
        if let Some(pulses) = r.u64(TAG_IRQ0_PULSES)? {
            self.irq0_pulses = pulses;
        }

        if let Some(buf) = r.bytes(TAG_CHANNELS) {
            let mut d = Decoder::new(buf);
            let count = d.u32()? as usize;
            // The PIT has 3 channels in real hardware, but the snapshot format is forward
            // compatible by encoding the channel count. Reject obviously excessive values to
            // avoid unbounded decode work on malformed snapshots.
            const MAX_SNAPSHOT_CHANNELS: usize = 32;
            if count > MAX_SNAPSHOT_CHANNELS {
                return Err(SnapshotError::InvalidFieldEncoding("channels"));
            }
            for idx in 0..count {
                let mode = d.u8()?;
                let bcd = d.bool()?;
                let access_bits = d.u8()?;
                let write_phase = d.u8()?;
                let read_phase = d.u8()?;
                let write_latch_low = d.u8()?;
                let null_count = d.bool()?;
                let reload = d.u32()?;
                let phase_ticks = d.u32()?;

                let latched_count = if d.bool()? {
                    let value = d.u16()?;
                    let phase = match d.u8()? {
                        1 => BytePhase::High,
                        _ => BytePhase::Low,
                    };
                    Some(LatchedValue { value, phase })
                } else {
                    None
                };

                let latched_status = if d.bool()? { Some(d.u8()?) } else { None };

                if idx < self.channels.len() {
                    let ch = &mut self.channels[idx];
                    ch.mode = mode;
                    ch.bcd = bcd;
                    ch.access_mode = AccessMode::from_bits(access_bits);
                    ch.write_phase = if write_phase == 1 {
                        BytePhase::High
                    } else {
                        BytePhase::Low
                    };
                    ch.read_phase = if read_phase == 1 {
                        BytePhase::High
                    } else {
                        BytePhase::Low
                    };
                    ch.write_latch_low = write_latch_low;
                    ch.null_count = null_count;
                    ch.reload = reload;
                    ch.phase_ticks = phase_ticks;
                    ch.latched_count = latched_count;
                    ch.latched_status = latched_status;
                }
            }
            d.finish()?;
        }

        Ok(())
    }
}

pub type SharedPit8254 = Rc<RefCell<Pit8254>>;

/// I/O-port view of a shared [`Pit8254`].
///
/// `IoPortBus` maps one port to one device instance. A PIT responds to four ports,
/// so the common pattern is to share the PIT behind `Rc<RefCell<_>>` and register
/// four `Pit8254Port` instances.
pub struct Pit8254Port {
    pit: SharedPit8254,
    port: u16,
}

impl Pit8254Port {
    pub fn new(pit: SharedPit8254, port: u16) -> Self {
        Self { pit, port }
    }
}

impl PortIoDevice for Pit8254Port {
    fn read(&mut self, port: u16, size: u8) -> u32 {
        debug_assert_eq!(port, self.port);
        self.pit.borrow_mut().port_read(port, size)
    }

    fn write(&mut self, port: u16, size: u8, value: u32) {
        debug_assert_eq!(port, self.port);
        self.pit.borrow_mut().port_write(port, size, value);
    }

    fn reset(&mut self) {
        // Reset the shared PIT back to its power-on state. This is safe to call multiple times
        // (once per port mapping) as the operation is idempotent.
        self.pit.borrow_mut().reset();
    }
}

/// Convenience helper to register the PIT ports on an [`IoPortBus`].
pub fn register_pit8254(bus: &mut IoPortBus, pit: SharedPit8254) {
    bus.register(PIT_CH0, Box::new(Pit8254Port::new(pit.clone(), PIT_CH0)));
    bus.register(PIT_CH1, Box::new(Pit8254Port::new(pit.clone(), PIT_CH1)));
    bus.register(PIT_CH2, Box::new(Pit8254Port::new(pit.clone(), PIT_CH2)));
    bus.register(PIT_CMD, Box::new(Pit8254Port::new(pit, PIT_CMD)));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn program_divisor(pit: &mut Pit8254, mode_cmd: u8, divisor: u16) {
        pit.port_write(PIT_CMD, 1, mode_cmd as u32);
        pit.port_write(PIT_CH0, 1, (divisor & 0xFF) as u32);
        pit.port_write(PIT_CH0, 1, (divisor >> 8) as u32);
    }

    #[test]
    fn mode2_irq0_periodic() {
        let mut pit = Pit8254::new();
        // ch0, lobyte/hibyte, mode2, binary
        program_divisor(&mut pit, 0x34, 4);

        pit.advance_ticks(3);
        assert_eq!(pit.take_irq0_pulses(), 0);

        pit.advance_ticks(1);
        assert_eq!(pit.take_irq0_pulses(), 1);

        pit.advance_ticks(8);
        assert_eq!(pit.take_irq0_pulses(), 2);
    }

    #[test]
    fn mode3_irq0_periodic() {
        let mut pit = Pit8254::new();
        // ch0, lobyte/hibyte, mode3, binary
        program_divisor(&mut pit, 0x36, 5);

        pit.advance_ticks(5);
        assert_eq!(pit.take_irq0_pulses(), 1);

        pit.advance_ticks(10);
        assert_eq!(pit.take_irq0_pulses(), 2);
    }

    #[test]
    fn lobyte_hibyte_sequencing_gates_start() {
        let mut pit = Pit8254::new();
        pit.port_write(PIT_CMD, 1, 0x34); // mode2, lo/hi
        pit.port_write(PIT_CH0, 1, 10); // low byte only so far

        pit.advance_ticks(100);
        assert_eq!(pit.take_irq0_pulses(), 0);

        pit.port_write(PIT_CH0, 1, 0); // high byte -> divisor=10
        pit.advance_ticks(10);
        assert_eq!(pit.take_irq0_pulses(), 1);
    }

    #[test]
    fn latch_count_freezes_value() {
        let mut pit = Pit8254::new();
        program_divisor(&mut pit, 0x34, 10);

        pit.advance_ticks(3);

        // Latch count for channel 0.
        pit.port_write(PIT_CMD, 1, 0x00);

        let lo = pit.port_read(PIT_CH0, 1) as u8;
        let hi = pit.port_read(PIT_CH0, 1) as u8;
        let latched = u16::from_le_bytes([lo, hi]);
        assert_eq!(latched, 7);

        pit.advance_ticks(1);
        let live_lo = pit.port_read(PIT_CH0, 1) as u8;
        let live_hi = pit.port_read(PIT_CH0, 1) as u8;
        let live = u16::from_le_bytes([live_lo, live_hi]);
        assert_eq!(live, 6);
    }

    #[test]
    fn read_back_command_latches_status_then_count() {
        let mut pit = Pit8254::new();
        program_divisor(&mut pit, 0x34, 4);
        pit.advance_ticks(1);

        // Read-back: latch status + count for channel 0.
        // D7-D6=11 (read-back), D5=0 (latch count), D4=0 (latch status),
        // D3=1 (don't select ch2), D2=1 (don't select ch1), D1=0 (select ch0), D0=0.
        pit.port_write(PIT_CMD, 1, 0b1100_1100);

        let status = pit.port_read(PIT_CH0, 1) as u8;
        assert_ne!(status, 0);

        // Next reads return count bytes.
        let lo = pit.port_read(PIT_CH0, 1) as u8;
        let hi = pit.port_read(PIT_CH0, 1) as u8;
        let count = u16::from_le_bytes([lo, hi]);
        assert_eq!(count, 3);
    }

    #[test]
    fn ns_to_ticks_conversion_is_deterministic() {
        let mut pit = Pit8254::new();
        program_divisor(&mut pit, 0x34, 1_193); // ~1kHz

        pit.advance_ns(1_000_000); // 1ms -> ~1193 ticks -> ~1 pulse
        assert_eq!(pit.take_irq0_pulses(), 1);
    }
}
