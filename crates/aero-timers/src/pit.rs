use aero_time::{Interrupt, InterruptSink, TimerId, TimerQueue};

use crate::DeviceTimer;

pub const PIT_INPUT_HZ: u64 = 1_193_182;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PitMode {
    InterruptOnTerminalCount = 0,
    RateGenerator = 2,
    SquareWaveGenerator = 3,
}

#[derive(Debug)]
struct PitChannel {
    mode: PitMode,
    reload: u16,
    pending_low: Option<u8>,
    periodic: bool,
    next_tick: Option<u64>,
    period_ticks: u64,
    timer_id: Option<TimerId>,
}

impl PitChannel {
    fn new() -> Self {
        Self {
            mode: PitMode::RateGenerator,
            reload: 0,
            pending_low: None,
            periodic: true,
            next_tick: None,
            period_ticks: 0,
            timer_id: None,
        }
    }

    fn reload_ticks(&self) -> u64 {
        match self.reload {
            0 => 65_536,
            v => v as u64,
        }
    }
}

#[derive(Debug)]
pub struct Pit {
    ch0: PitChannel,
}

impl Pit {
    pub fn new() -> Self {
        Self {
            ch0: PitChannel::new(),
        }
    }

    fn tick_from_ns(guest_ns: u64) -> u64 {
        ((guest_ns as u128) * (PIT_INPUT_HZ as u128) / 1_000_000_000u128) as u64
    }

    fn ns_from_tick_ceil(tick: u64) -> u64 {
        let numer = (tick as u128) * 1_000_000_000u128;
        let denom = PIT_INPUT_HZ as u128;
        ((numer + denom - 1) / denom) as u64
    }

    /// Write to the PIT command register (port 0x43).
    pub fn write_command(&mut self, value: u8) {
        let channel = value >> 6;
        if channel != 0 {
            return;
        }

        let access = (value >> 4) & 0b11;
        if access != 0b11 {
            return;
        }

        let mut mode = (value >> 1) & 0b111;
        if mode >= 6 {
            mode &= 0b11;
        }
        self.ch0.mode = match mode {
            0 => PitMode::InterruptOnTerminalCount,
            2 => PitMode::RateGenerator,
            3 => PitMode::SquareWaveGenerator,
            _ => PitMode::RateGenerator,
        };
        self.ch0.periodic = matches!(
            self.ch0.mode,
            PitMode::RateGenerator | PitMode::SquareWaveGenerator
        );
        self.ch0.pending_low = None;
    }

    /// Write to a channel data port (0x40 for channel 0).
    pub fn write_channel0_data(
        &mut self,
        value: u8,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        match self.ch0.pending_low {
            None => {
                self.ch0.pending_low = Some(value);
            }
            Some(low) => {
                self.ch0.pending_low = None;
                self.ch0.reload = u16::from_le_bytes([low, value]);
                self.program_channel0(guest_now_ns, queue);
            }
        }
    }

    fn program_channel0(&mut self, guest_now_ns: u64, queue: &mut TimerQueue<DeviceTimer>) {
        if let Some(id) = self.ch0.timer_id.take() {
            queue.cancel(id);
        }

        let reload_ticks = self.ch0.reload_ticks();
        self.ch0.period_ticks = reload_ticks;

        let now_tick = Self::tick_from_ns(guest_now_ns);
        let next_tick = now_tick.saturating_add(reload_ticks);
        self.ch0.next_tick = Some(next_tick);
        let deadline_ns = Self::ns_from_tick_ceil(next_tick);
        self.ch0.timer_id = Some(queue.schedule(deadline_ns, DeviceTimer::PitChannel0));
    }

    pub fn handle_timer_event(
        &mut self,
        at_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
        sink: &mut dyn InterruptSink,
    ) {
        sink.raise(Interrupt::Irq(0), at_ns);

        let periodic = self.ch0.periodic;
        let period_ticks = self.ch0.period_ticks;
        let Some(next_tick) = self.ch0.next_tick else {
            return;
        };

        if !periodic || period_ticks == 0 {
            self.ch0.timer_id = None;
            self.ch0.next_tick = None;
            return;
        }

        let new_tick = next_tick.saturating_add(period_ticks);
        self.ch0.next_tick = Some(new_tick);
        let deadline_ns = Self::ns_from_tick_ceil(new_tick);
        self.ch0.timer_id = Some(queue.schedule(deadline_ns, DeviceTimer::PitChannel0));
    }
}

impl Default for Pit {
    fn default() -> Self {
        Self::new()
    }
}
