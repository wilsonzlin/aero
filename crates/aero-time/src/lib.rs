//! Guest/host time modelling and timer scheduling primitives.
//!
//! The emulator uses **guest virtual time** (monotonic nanoseconds since reset) as the single
//! source of truth for all timer devices (TSC/PIT/HPET/APIC). In production, guest time is
//! derived from a monotonic host clock (e.g. `performance.now()` on wasm, `Instant` on native),
//! while unit tests can drive the system deterministically via a fake clock.

mod clock;
mod interrupt;
mod timer_queue;
mod tsc;

pub use clock::{FakeHostClock, HostClock, Speed, StdHostClock, TimeSource};
pub use interrupt::{Interrupt, InterruptSink};
pub use timer_queue::{TimerEvent, TimerId, TimerQueue};
pub use tsc::Tsc;
