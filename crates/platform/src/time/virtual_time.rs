use super::{Clock, ClockState, TimerEvent, TimerScheduler, TimerSchedulerState};

/// Convenience wrapper that pairs a [`Clock`] with a [`TimerScheduler`].
///
/// This is useful for emulation loops that want a single "time subsystem" object
/// to advance and snapshot for save/restore.
#[derive(Clone, Debug, Default)]
pub struct VirtualTime {
    clock: Clock,
    timers: TimerScheduler,
}

impl VirtualTime {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the current virtual time.
    #[inline]
    pub fn now_ns(&self) -> u64 {
        self.clock.now_ns()
    }

    #[inline]
    pub fn clock(&self) -> &Clock {
        &self.clock
    }

    #[inline]
    pub fn clock_mut(&mut self) -> &mut Clock {
        &mut self.clock
    }

    #[inline]
    pub fn timers(&self) -> &TimerScheduler {
        &self.timers
    }

    #[inline]
    pub fn timers_mut(&mut self) -> &mut TimerScheduler {
        &mut self.timers
    }

    /// Advances the clock and returns all timer events that become due at or
    /// before the new `now_ns()`.
    #[inline]
    pub fn advance(&mut self, ns: u64) -> Vec<TimerEvent> {
        self.clock.advance(ns);
        self.timers.advance_to(self.clock.now_ns())
    }

    /// Returns all timer events due at the current `now_ns()` without advancing
    /// time.
    #[inline]
    pub fn poll_due_events(&mut self) -> Vec<TimerEvent> {
        self.timers.advance_to(self.clock.now_ns())
    }

    pub fn save_state(&self) -> VirtualTimeState {
        VirtualTimeState {
            clock: self.clock.save_state(),
            timers: self.timers.save_state(),
        }
    }

    pub fn restore_state(state: VirtualTimeState) -> Self {
        let mut clock = Clock::new();
        clock.restore_state(state.clock);

        let timers = TimerScheduler::restore_state(state.timers);

        Self { clock, timers }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VirtualTimeState {
    pub clock: ClockState,
    pub timers: TimerSchedulerState,
}

