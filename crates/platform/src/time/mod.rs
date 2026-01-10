//! Deterministic virtual time utilities used by emulated devices.
//!
//! # Design
//!
//! This module provides a [`Clock`] (monotonic virtual time) and a [`TimerScheduler`]
//! (one-shot/periodic timers driven by that virtual time).
//!
//! Devices can either:
//! - Poll [`Clock::now_ns`] on MMIO/PIO accesses; or
//! - Be driven by timer events returned from [`TimerScheduler::advance_to`].
//!
//! The scheduler in this crate uses **event delivery** rather than storing callbacks.
//! This keeps the timer queue fully serializable for save/restore; callers can keep
//! their own `TimerId -> handler` mapping and re-establish it after restore.

mod clock;
mod timers;

pub use clock::{Clock, ClockState};
pub use timers::{
    TimerError, TimerEvent, TimerId, TimerKind, TimerKindStateRepr, TimerScheduler,
    TimerSchedulerState, TimerState,
};
