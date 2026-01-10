use aero_time::{Interrupt, InterruptSink, TimerId, TimerQueue};

use crate::DeviceTimer;

pub const DEFAULT_HPET_FREQ_HZ: u64 = 10_000_000;

#[derive(Debug, Clone, Copy)]
pub struct HpetTimerConfig {
    pub enabled: bool,
    pub periodic: bool,
    pub period_ticks: u64,
    pub irq: u8,
}

impl Default for HpetTimerConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            periodic: false,
            period_ticks: 0,
            irq: 2,
        }
    }
}

#[derive(Debug)]
struct HpetTimer {
    cfg: HpetTimerConfig,
    comparator: u64,
    timer_id: Option<TimerId>,
}

#[derive(Debug)]
pub struct Hpet {
    freq_hz: u64,
    enabled: bool,
    counter_base: u64,
    counter_anchor_ns: u64,
    timer0: HpetTimer,
}

impl Hpet {
    pub fn new(freq_hz: u64) -> Self {
        Self {
            freq_hz,
            enabled: false,
            counter_base: 0,
            counter_anchor_ns: 0,
            timer0: HpetTimer {
                cfg: HpetTimerConfig::default(),
                comparator: 0,
                timer_id: None,
            },
        }
    }

    fn ticks_from_ns(&self, guest_ns: u64) -> u64 {
        ((guest_ns as u128) * (self.freq_hz as u128) / 1_000_000_000u128) as u64
    }

    fn ns_from_ticks_ceil(&self, ticks: u64) -> u64 {
        let numer = (ticks as u128) * 1_000_000_000u128;
        let denom = self.freq_hz as u128;
        ((numer + denom - 1) / denom) as u64
    }

    fn counter_ticks(&self, guest_now_ns: u64) -> u64 {
        if !self.enabled {
            return self.counter_base;
        }
        let delta_ns = guest_now_ns.saturating_sub(self.counter_anchor_ns);
        let delta_ticks = self.ticks_from_ns(delta_ns);
        self.counter_base.wrapping_add(delta_ticks)
    }

    fn reschedule_timer0(&mut self, guest_now_ns: u64, queue: &mut TimerQueue<DeviceTimer>) {
        if let Some(id) = self.timer0.timer_id.take() {
            queue.cancel(id);
        }
        if !self.enabled || !self.timer0.cfg.enabled {
            return;
        }

        let now_counter = self.counter_ticks(guest_now_ns);
        let target = self.timer0.comparator;
        let deadline_ns = if target <= now_counter || target < self.counter_base {
            guest_now_ns
        } else {
            let delta_ticks = target - self.counter_base;
            let delta_ns = self.ns_from_ticks_ceil(delta_ticks);
            self.counter_anchor_ns.saturating_add(delta_ns)
        };
        self.timer0.timer_id = Some(queue.schedule(deadline_ns, DeviceTimer::HpetTimer0));
    }

    pub fn set_enabled(
        &mut self,
        enabled: bool,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        if enabled == self.enabled {
            return;
        }

        if enabled {
            self.counter_anchor_ns = guest_now_ns;
            self.enabled = true;
            self.reschedule_timer0(guest_now_ns, queue);
        } else {
            self.counter_base = self.counter_ticks(guest_now_ns);
            self.enabled = false;
            if let Some(id) = self.timer0.timer_id.take() {
                queue.cancel(id);
            }
        }
    }

    pub fn main_counter(&self, guest_now_ns: u64) -> u64 {
        self.counter_ticks(guest_now_ns)
    }

    pub fn set_main_counter(
        &mut self,
        guest_now_ns: u64,
        value: u64,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        self.counter_base = value;
        self.counter_anchor_ns = guest_now_ns;
        self.reschedule_timer0(guest_now_ns, queue);
    }

    pub fn configure_timer0(
        &mut self,
        cfg: HpetTimerConfig,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        self.timer0.cfg = cfg;
        self.reschedule_timer0(guest_now_ns, queue);
    }

    pub fn set_timer0_comparator(
        &mut self,
        comparator: u64,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
    ) {
        self.timer0.comparator = comparator;
        self.reschedule_timer0(guest_now_ns, queue);
    }

    pub fn handle_timer0_event(
        &mut self,
        at_ns: u64,
        guest_now_ns: u64,
        queue: &mut TimerQueue<DeviceTimer>,
        sink: &mut dyn InterruptSink,
    ) {
        if !self.enabled || !self.timer0.cfg.enabled {
            return;
        }
        sink.raise(Interrupt::Irq(self.timer0.cfg.irq), at_ns);

        if self.timer0.cfg.periodic && self.timer0.cfg.period_ticks != 0 {
            self.timer0.comparator = self
                .timer0
                .comparator
                .wrapping_add(self.timer0.cfg.period_ticks);
            self.reschedule_timer0(guest_now_ns, queue);
        }
    }
}

impl Default for Hpet {
    fn default() -> Self {
        Self::new(DEFAULT_HPET_FREQ_HZ)
    }
}
