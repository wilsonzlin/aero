use aero_time::{Interrupt, InterruptSink, TimerId, TimerQueue, Tsc};

use crate::DeviceTimer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApicTimerMode {
    OneShot,
    Periodic,
    TscDeadline,
}

#[derive(Debug)]
pub struct LocalApicTimer {
    freq_hz: u64,
    divide: u32,
    vector: u8,
    masked: bool,
    mode: ApicTimerMode,
    initial_count: u32,
    start_tick: u64,
    deadline_tick: Option<u64>,
    timer_id: Option<TimerId>,
}

impl LocalApicTimer {
    pub fn new(freq_hz: u64) -> Self {
        Self {
            freq_hz,
            divide: 1,
            vector: 0x20,
            masked: true,
            mode: ApicTimerMode::OneShot,
            initial_count: 0,
            start_tick: 0,
            deadline_tick: None,
            timer_id: None,
        }
    }

    pub fn supports_tsc_deadline(&self) -> bool {
        true
    }

    fn ticks_from_ns(&self, guest_ns: u64) -> u64 {
        ((guest_ns as u128) * (self.freq_hz as u128) / 1_000_000_000u128) as u64
    }

    fn ns_from_ticks_ceil(&self, ticks: u64) -> u64 {
        let numer = (ticks as u128) * 1_000_000_000u128;
        let denom = self.freq_hz as u128;
        ((numer + denom - 1) / denom) as u64
    }

    fn counts_per_period_ticks(&self) -> Option<u64> {
        let counts = self.initial_count as u64;
        if counts == 0 || self.divide == 0 {
            return None;
        }
        Some(counts.saturating_mul(self.divide as u64))
    }

    fn cancel_timer(&mut self, queue: &mut TimerQueue<DeviceTimer>) {
        if let Some(id) = self.timer_id.take() {
            queue.cancel(id);
        }
    }

    fn schedule_deadline_tick(&mut self, deadline_tick: u64, queue: &mut TimerQueue<DeviceTimer>) {
        let deadline_ns = self.ns_from_ticks_ceil(deadline_tick);
        self.deadline_tick = Some(deadline_tick);
        self.timer_id = Some(queue.schedule(deadline_ns, DeviceTimer::LocalApicTimer));
    }

    pub fn set_divide(&mut self, divide: u32) {
        self.divide = divide.max(1);
    }

    pub fn set_vector(&mut self, vector: u8) {
        self.vector = vector;
    }

    pub fn set_masked(&mut self, masked: bool) {
        self.masked = masked;
    }

    pub fn set_mode(&mut self, mode: ApicTimerMode) {
        self.mode = mode;
    }

    pub fn write_initial_count(
        &mut self,
        guest_now_ns: u64,
        initial: u32,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        self.initial_count = initial;
        self.start_tick = self.ticks_from_ns(guest_now_ns);
        self.cancel_timer(queue);

        if self.masked || self.mode == ApicTimerMode::TscDeadline {
            return;
        }

        let Some(period_ticks) = self.counts_per_period_ticks() else {
            return;
        };

        let deadline_tick = self.start_tick.saturating_add(period_ticks);
        self.schedule_deadline_tick(deadline_tick, queue);
    }

    pub fn write_tsc_deadline(
        &mut self,
        guest_now_ns: u64,
        deadline_tsc: u64,
        tsc: &Tsc,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        self.cancel_timer(queue);
        if self.masked || self.mode != ApicTimerMode::TscDeadline {
            return;
        }

        let guest_deadline_ns = tsc.guest_ns_for_tsc(deadline_tsc).unwrap_or(guest_now_ns);
        let deadline_tick = self.ticks_from_ns(guest_deadline_ns);
        self.schedule_deadline_tick(deadline_tick, queue);
    }

    pub fn current_count(&self, guest_now_ns: u64) -> u32 {
        let Some(period_ticks) = self.counts_per_period_ticks() else {
            return 0;
        };
        let now_tick = self.ticks_from_ns(guest_now_ns);
        if now_tick <= self.start_tick {
            return self.initial_count;
        }
        let elapsed_ticks = now_tick - self.start_tick;
        let elapsed_counts = elapsed_ticks / self.divide as u64;

        match self.mode {
            ApicTimerMode::Periodic => {
                let initial = self.initial_count as u64;
                if initial == 0 {
                    return 0;
                }
                let pos_in_period = elapsed_counts % initial;
                (initial - pos_in_period) as u32
            }
            ApicTimerMode::OneShot | ApicTimerMode::TscDeadline => {
                if elapsed_ticks >= period_ticks {
                    0
                } else {
                    let remaining = (period_ticks - elapsed_ticks) / self.divide as u64;
                    remaining.min(self.initial_count as u64) as u32
                }
            }
        }
    }

    pub fn handle_timer_event(
        &mut self,
        at_ns: u64,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
        sink: &mut dyn InterruptSink,
    ) {
        if self.masked {
            self.cancel_timer(queue);
            return;
        }

        sink.raise(Interrupt::Vector(self.vector), at_ns);

        match self.mode {
            ApicTimerMode::Periodic => {
                let Some(period_ticks) = self.counts_per_period_ticks() else {
                    self.cancel_timer(queue);
                    return;
                };
                let new_start = self
                    .deadline_tick
                    .unwrap_or(self.ticks_from_ns(guest_now_ns));
                self.start_tick = new_start;
                let deadline_tick = new_start.saturating_add(period_ticks);
                self.schedule_deadline_tick(deadline_tick, queue);
            }
            ApicTimerMode::OneShot | ApicTimerMode::TscDeadline => {
                self.cancel_timer(queue);
                self.deadline_tick = None;
            }
        }
    }
}

impl Default for LocalApicTimer {
    fn default() -> Self {
        Self::new(1_000_000_000)
    }
}
