//! Timer-related device models (PIT/HPET/Local APIC timer).
//!
//! Each device schedules its own deadlines in a shared [`aero_time::TimerQueue`], driven by
//! guest virtual time from [`aero_time::TimeSource`].

mod apic_timer;
mod hpet;
mod pit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceTimer {
    PitChannel0,
    HpetTimer0,
    LocalApicTimer,
}

pub use apic_timer::{ApicTimerMode, LocalApicTimer};
pub use hpet::{Hpet, HpetTimerConfig, DEFAULT_HPET_FREQ_HZ};
pub use pit::{Pit, PitMode, PIT_INPUT_HZ};
