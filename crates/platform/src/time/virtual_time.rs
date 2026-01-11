use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

use super::{
    Clock, ClockState, TimerEvent, TimerId, TimerKindStateRepr, TimerScheduler,
    TimerSchedulerState, TimerState,
};

// Snapshot format (device id "VTIM", version 1.0)
//
// Top-level TLV tags:
//   Tag 1: u64 now_ns
//   Tag 2: timer scheduler state (custom binary encoding, little-endian)
//
// Timer scheduler encoding (Tag 2 payload):
//   u64 next_timer_id
//   u32 timer_count
//   repeated timer records, in ascending `timer_id` order:
//     u64 timer_id
//     u8  kind_tag:
//           0 = disarmed (None)
//           1 = one-shot
//           2 = periodic (integer ns)
//           3 = periodic (rational ns)
//     kind payload:
//       one-shot:
//         u64 deadline_ns
//       periodic:
//         u64 next_deadline_ns
//         u64 period_ns
//       periodic rational:
//         u64 base_deadline_ns
//         u64 next_index
//         u64 period_num_ns
//         u64 period_denom
//
// This format is deterministic by construction:
// - TLV tags are sorted by `SnapshotWriter`
// - timers are encoded in `TimerSchedulerState.timers` order, which is sorted by `timer_id`.
const TAG_NOW_NS: u16 = 1;
const TAG_TIMER_SCHEDULER: u16 = 2;

const TIMER_KIND_NONE: u8 = 0;
const TIMER_KIND_ONE_SHOT: u8 = 1;
const TIMER_KIND_PERIODIC: u8 = 2;
const TIMER_KIND_PERIODIC_RATIONAL: u8 = 3;

fn encode_timer_scheduler_state(state: &TimerSchedulerState) -> Vec<u8> {
    let timer_count: u32 = state
        .timers
        .len()
        .try_into()
        .expect("timer count exceeded u32::MAX");

    let mut enc = Encoder::new().u64(state.next_timer_id).u32(timer_count);
    for timer in &state.timers {
        enc = enc.u64(timer.timer_id.as_u64());
        match timer.kind {
            None => {
                enc = enc.u8(TIMER_KIND_NONE);
            }
            Some(TimerKindStateRepr::OneShot { deadline_ns }) => {
                enc = enc.u8(TIMER_KIND_ONE_SHOT).u64(deadline_ns);
            }
            Some(TimerKindStateRepr::Periodic {
                next_deadline_ns,
                period_ns,
            }) => {
                enc = enc
                    .u8(TIMER_KIND_PERIODIC)
                    .u64(next_deadline_ns)
                    .u64(period_ns);
            }
            Some(TimerKindStateRepr::PeriodicRational {
                base_deadline_ns,
                next_index,
                period_num_ns,
                period_denom,
            }) => {
                enc = enc
                    .u8(TIMER_KIND_PERIODIC_RATIONAL)
                    .u64(base_deadline_ns)
                    .u64(next_index)
                    .u64(period_num_ns)
                    .u64(period_denom);
            }
        }
    }
    enc.finish()
}

fn decode_timer_scheduler_state(bytes: &[u8]) -> SnapshotResult<TimerSchedulerState> {
    let mut d = Decoder::new(bytes);
    let next_timer_id = d.u64()?;
    let timer_count = d.u32()? as usize;

    let mut timers = Vec::with_capacity(timer_count);
    for _ in 0..timer_count {
        let timer_id = TimerId::from_u64(d.u64()?);
        let kind_tag = d.u8()?;
        let kind = match kind_tag {
            TIMER_KIND_NONE => None,
            TIMER_KIND_ONE_SHOT => Some(TimerKindStateRepr::OneShot {
                deadline_ns: d.u64()?,
            }),
            TIMER_KIND_PERIODIC => {
                let next_deadline_ns = d.u64()?;
                let period_ns = d.u64()?;
                if period_ns == 0 {
                    return Err(SnapshotError::InvalidFieldEncoding("period_ns"));
                }
                Some(TimerKindStateRepr::Periodic {
                    next_deadline_ns,
                    period_ns,
                })
            }
            TIMER_KIND_PERIODIC_RATIONAL => {
                let base_deadline_ns = d.u64()?;
                let next_index = d.u64()?;
                let period_num_ns = d.u64()?;
                let period_denom = d.u64()?;
                if period_num_ns == 0 || period_denom == 0 {
                    return Err(SnapshotError::InvalidFieldEncoding("period_rational"));
                }
                Some(TimerKindStateRepr::PeriodicRational {
                    base_deadline_ns,
                    next_index,
                    period_num_ns,
                    period_denom,
                })
            }
            _ => return Err(SnapshotError::InvalidFieldEncoding("timer_kind")),
        };
        timers.push(TimerState { timer_id, kind });
    }

    d.finish()?;

    Ok(TimerSchedulerState {
        next_timer_id,
        timers,
    })
}

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

impl IoSnapshot for VirtualTime {
    const DEVICE_ID: [u8; 4] = *b"VTIM";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u64(TAG_NOW_NS, self.clock.now_ns());
        w.field_bytes(
            TAG_TIMER_SCHEDULER,
            encode_timer_scheduler_state(&self.timers.save_state()),
        );

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        // Start from a deterministic baseline for forward-compatible snapshots that may omit fields.
        *self = Self::new();

        if let Some(now_ns) = r.u64(TAG_NOW_NS)? {
            self.clock.set_now_ns(now_ns);
        }

        if let Some(buf) = r.bytes(TAG_TIMER_SCHEDULER) {
            self.timers = TimerScheduler::restore_state(decode_timer_scheduler_state(buf)?);
        }

        Ok(())
    }
}
