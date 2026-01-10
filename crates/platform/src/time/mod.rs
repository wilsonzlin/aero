//! Deterministic virtual time utilities used by emulated devices.
//!
//! # Design
//!
//! This module provides a [`Clock`] (monotonic virtual time) and a [`TimerScheduler`]
//! (one-shot/periodic timers driven by that virtual time).
//!
//! Devices typically use a mix of:
//! - Polling [`Clock::now_ns`] for free-running counters on MMIO/PIO access (e.g. ACPI PM timer).
//! - Handling timer events returned from [`TimerScheduler::advance_to`] to drive interrupt sources
//!   (e.g. PIT/HPET/LAPIC timer).
//!
//! The scheduler in this crate uses **event delivery** rather than storing callbacks.
//! This keeps the timer queue fully serializable for save/restore; callers can keep
//! their own `TimerId -> handler` mapping and re-establish it after restore.

mod clock;
mod math;
mod timers;
mod virtual_time;

pub use clock::{Clock, ClockState};
pub use math::{
    gcd_u64, mul_div_u64_floor, ns_from_ticks_floor, period_from_hz_ns, reduce_fraction,
    ticks_from_ns_floor, NANOS_PER_SEC,
};
pub use timers::{
    TimerError, TimerEvent, TimerId, TimerKind, TimerKindStateRepr, TimerScheduler,
    TimerSchedulerState, TimerState,
};
pub use virtual_time::{VirtualTime, VirtualTimeState};
