use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub trait HostClock: Send + Sync + 'static {
    /// Monotonic nanoseconds from an arbitrary origin.
    fn now_ns(&self) -> u64;
}

#[derive(Debug)]
pub struct StdHostClock {
    start: Instant,
}

impl StdHostClock {
    pub fn new() -> Self {
        Self {
            start: Instant::now(),
        }
    }
}

impl Default for StdHostClock {
    fn default() -> Self {
        Self::new()
    }
}

impl HostClock for StdHostClock {
    fn now_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
}

#[derive(Debug)]
pub struct FakeHostClock {
    now_ns: AtomicU64,
}

impl FakeHostClock {
    pub fn new(start_ns: u64) -> Self {
        Self {
            now_ns: AtomicU64::new(start_ns),
        }
    }

    pub fn set_ns(&self, now_ns: u64) {
        self.now_ns.store(now_ns, Ordering::SeqCst);
    }

    pub fn advance_ns(&self, delta_ns: u64) {
        self.now_ns.fetch_add(delta_ns, Ordering::SeqCst);
    }
}

impl HostClock for FakeHostClock {
    fn now_ns(&self) -> u64 {
        self.now_ns.load(Ordering::SeqCst)
    }
}

/// Fixed-point guest speed scaling (guest ns per host ns) in Q32.32.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Speed {
    scale_fp: u64,
}

impl Speed {
    pub const ONE: Speed = Speed {
        scale_fp: 1u64 << 32,
    };

    pub fn from_ratio(numer: u32, denom: u32) -> Self {
        assert!(denom != 0);
        let scale_fp = ((numer as u128) << 32) / denom as u128;
        Self {
            scale_fp: scale_fp as u64,
        }
    }

    #[inline]
    pub fn host_delta_to_guest_ns(self, host_delta_ns: u64) -> u64 {
        (((host_delta_ns as u128) * (self.scale_fp as u128)) >> 32) as u64
    }

    #[inline]
    pub fn guest_delta_to_host_ns_ceil(self, guest_delta_ns: u64) -> u64 {
        let numer = (guest_delta_ns as u128) << 32;
        let denom = self.scale_fp as u128;
        if denom == 0 {
            return u64::MAX;
        }
        ((numer + denom - 1) / denom) as u64
    }
}

#[derive(Debug)]
struct TimeSourceState {
    host_anchor_ns: u64,
    guest_anchor_ns: u64,
    speed: Speed,
    paused: bool,
}

/// Thread-safe guest time source derived from a monotonic host clock.
pub struct TimeSource {
    host: Arc<dyn HostClock>,
    state: Mutex<TimeSourceState>,
    last_guest_now_ns: AtomicU64,
}

impl TimeSource {
    pub fn new(host: Arc<dyn HostClock>) -> Self {
        let host_anchor_ns = host.now_ns();
        Self {
            host,
            state: Mutex::new(TimeSourceState {
                host_anchor_ns,
                guest_anchor_ns: 0,
                speed: Speed::ONE,
                paused: false,
            }),
            last_guest_now_ns: AtomicU64::new(0),
        }
    }

    fn raw_now_ns(&self) -> u64 {
        let host_now_ns = self.host.now_ns();
        let state = self.state.lock().expect("time source mutex poisoned");
        if state.paused {
            return state.guest_anchor_ns;
        }
        let host_delta_ns = host_now_ns.saturating_sub(state.host_anchor_ns);
        state
            .guest_anchor_ns
            .saturating_add(state.speed.host_delta_to_guest_ns(host_delta_ns))
    }

    /// Monotonic guest nanoseconds since reset.
    pub fn now_ns(&self) -> u64 {
        let raw = self.raw_now_ns();
        loop {
            let last = self.last_guest_now_ns.load(Ordering::SeqCst);
            if raw <= last {
                return last;
            }
            if self
                .last_guest_now_ns
                .compare_exchange(last, raw, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return raw;
            }
        }
    }

    pub fn speed(&self) -> Speed {
        let state = self.state.lock().expect("time source mutex poisoned");
        state.speed
    }

    pub fn is_paused(&self) -> bool {
        let state = self.state.lock().expect("time source mutex poisoned");
        state.paused
    }

    pub fn pause(&self) {
        let mut state = self.state.lock().expect("time source mutex poisoned");
        if state.paused {
            return;
        }
        let host_now_ns = self.host.now_ns();
        let host_delta_ns = host_now_ns.saturating_sub(state.host_anchor_ns);
        let guest_now_ns = state
            .guest_anchor_ns
            .saturating_add(state.speed.host_delta_to_guest_ns(host_delta_ns));
        state.guest_anchor_ns = guest_now_ns;
        state.paused = true;
    }

    pub fn resume(&self) {
        let mut state = self.state.lock().expect("time source mutex poisoned");
        if !state.paused {
            return;
        }
        state.host_anchor_ns = self.host.now_ns();
        state.paused = false;
    }

    pub fn set_speed(&self, speed: Speed) {
        let mut state = self.state.lock().expect("time source mutex poisoned");
        if state.paused {
            state.speed = speed;
            return;
        }

        let host_now_ns = self.host.now_ns();
        let host_delta_ns = host_now_ns.saturating_sub(state.host_anchor_ns);
        let guest_now_ns = state
            .guest_anchor_ns
            .saturating_add(state.speed.host_delta_to_guest_ns(host_delta_ns));

        state.host_anchor_ns = host_now_ns;
        state.guest_anchor_ns = guest_now_ns;
        state.speed = speed;
    }

    /// Convert a guest deadline into a host sleep duration based on the current speed.
    ///
    /// Returns `None` if the clock is paused (guest time will not progress).
    pub fn host_duration_until_guest_ns(&self, guest_deadline_ns: u64) -> Option<Duration> {
        let host_now_ns = self.host.now_ns();
        let state = self.state.lock().expect("time source mutex poisoned");
        if state.paused {
            return None;
        }
        let host_delta_ns = host_now_ns.saturating_sub(state.host_anchor_ns);
        let guest_now_ns = state
            .guest_anchor_ns
            .saturating_add(state.speed.host_delta_to_guest_ns(host_delta_ns));

        if guest_deadline_ns <= guest_now_ns {
            return Some(Duration::ZERO);
        }

        let guest_delta_ns = guest_deadline_ns - guest_now_ns;
        let host_delta_ns = state.speed.guest_delta_to_host_ns_ceil(guest_delta_ns);
        Some(Duration::from_nanos(host_delta_ns))
    }
}
