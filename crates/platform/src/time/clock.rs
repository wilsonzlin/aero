#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Clock {
    now_ns: u64,
}

impl Clock {
    pub const fn new() -> Self {
        Self { now_ns: 0 }
    }

    /// Returns the current monotonic virtual time, in nanoseconds.
    #[inline]
    pub const fn now_ns(&self) -> u64 {
        self.now_ns
    }

    /// Advances the clock by `ns` nanoseconds.
    ///
    /// # Panics
    ///
    /// Panics if advancing would overflow `u64`. (`u64` nanoseconds is ~584 years.)
    #[inline]
    pub fn advance(&mut self, ns: u64) {
        self.now_ns = self
            .now_ns
            .checked_add(ns)
            .expect("virtual clock overflowed u64::MAX");
    }

    /// Sets the current time, intended for save/restore.
    ///
    /// This may move time backwards; callers must ensure the timer scheduler is
    /// restored to a consistent snapshot.
    #[inline]
    pub fn set_now_ns(&mut self, now_ns: u64) {
        self.now_ns = now_ns;
    }

    #[inline]
    pub const fn save_state(&self) -> ClockState {
        ClockState { now_ns: self.now_ns }
    }

    #[inline]
    pub fn restore_state(&mut self, state: ClockState) {
        self.now_ns = state.now_ns;
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ClockState {
    pub now_ns: u64,
}

