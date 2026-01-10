use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashMap};

/// A stable identifier for a timer allocated from a [`TimerScheduler`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TimerId(u64);

impl TimerId {
    #[inline]
    pub const fn as_u64(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TimerEvent {
    pub timer_id: TimerId,
    /// The virtual time at which the timer was scheduled to fire.
    pub deadline_ns: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerKind {
    OneShot,
    Periodic { period_ns: u64 },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TimerError {
    UnknownTimer(TimerId),
    InvalidPeriodNs,
}

impl std::fmt::Display for TimerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownTimer(id) => write!(f, "unknown timer id {}", id.as_u64()),
            Self::InvalidPeriodNs => write!(f, "invalid period_ns (must be > 0)"),
        }
    }
}

impl std::error::Error for TimerError {}

#[derive(Clone, Debug)]
struct TimerSlot {
    generation: u64,
    kind: Option<TimerKindState>,
}

#[derive(Clone, Copy, Debug)]
enum TimerKindState {
    OneShot { deadline_ns: u64 },
    Periodic { next_deadline_ns: u64, period_ns: u64 },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct QueueEntry {
    deadline_ns: u64,
    timer_id: TimerId,
    generation: u64,
}

impl Ord for QueueEntry {
    fn cmp(&self, other: &Self) -> Ordering {
        self.deadline_ns
            .cmp(&other.deadline_ns)
            .then_with(|| self.timer_id.cmp(&other.timer_id))
            .then_with(|| self.generation.cmp(&other.generation))
    }
}

impl PartialOrd for QueueEntry {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// A deterministic one-shot/periodic timer scheduler keyed off a virtual timebase.
///
/// The scheduler does not store callbacks; instead [`advance_to`](Self::advance_to)
/// returns a list of [`TimerEvent`]s to be dispatched by the caller.
#[derive(Clone, Debug, Default)]
pub struct TimerScheduler {
    next_timer_id: u64,
    timers: HashMap<TimerId, TimerSlot>,
    queue: BinaryHeap<std::cmp::Reverse<QueueEntry>>,
}

impl TimerScheduler {
    pub fn new() -> Self {
        Self::default()
    }

    /// Allocates a new (initially disarmed) timer.
    pub fn alloc_timer(&mut self) -> TimerId {
        self.next_timer_id = self
            .next_timer_id
            .checked_add(1)
            .expect("timer id overflowed u64::MAX");
        let id = TimerId(self.next_timer_id);
        let old = self.timers.insert(
            id,
            TimerSlot {
                generation: 0,
                kind: None,
            },
        );
        debug_assert!(old.is_none(), "timer id space wrapped and collided");
        id
    }

    /// Arms an existing timer as a one-shot that fires at `deadline_ns`.
    pub fn arm_one_shot(&mut self, timer_id: TimerId, deadline_ns: u64) -> Result<(), TimerError> {
        let slot = self
            .timers
            .get_mut(&timer_id)
            .ok_or(TimerError::UnknownTimer(timer_id))?;

        slot.generation = slot.generation.wrapping_add(1);
        slot.kind = Some(TimerKindState::OneShot { deadline_ns });
        self.queue.push(std::cmp::Reverse(QueueEntry {
            deadline_ns,
            timer_id,
            generation: slot.generation,
        }));
        Ok(())
    }

    /// Arms an existing timer as periodic.
    ///
    /// `first_deadline_ns` is the first firing time, and subsequent firings occur at
    /// `first_deadline_ns + N * period_ns`. The phase is preserved regardless of
    /// how `advance_to()` is chunked.
    pub fn arm_periodic(
        &mut self,
        timer_id: TimerId,
        first_deadline_ns: u64,
        period_ns: u64,
    ) -> Result<(), TimerError> {
        if period_ns == 0 {
            return Err(TimerError::InvalidPeriodNs);
        }
        let slot = self
            .timers
            .get_mut(&timer_id)
            .ok_or(TimerError::UnknownTimer(timer_id))?;

        slot.generation = slot.generation.wrapping_add(1);
        slot.kind = Some(TimerKindState::Periodic {
            next_deadline_ns: first_deadline_ns,
            period_ns,
        });
        self.queue.push(std::cmp::Reverse(QueueEntry {
            deadline_ns: first_deadline_ns,
            timer_id,
            generation: slot.generation,
        }));
        Ok(())
    }

    /// Disarms a timer.
    pub fn disarm(&mut self, timer_id: TimerId) -> Result<(), TimerError> {
        let slot = self
            .timers
            .get_mut(&timer_id)
            .ok_or(TimerError::UnknownTimer(timer_id))?;
        slot.generation = slot.generation.wrapping_add(1);
        slot.kind = None;
        Ok(())
    }

    /// Returns the next deadline (if any) after cleaning up stale queue entries.
    pub fn next_deadline_ns(&mut self) -> Option<u64> {
        self.cleanup_stale_queue_entries();
        self.queue.peek().map(|entry| entry.0.deadline_ns)
    }

    /// Advances the scheduler to `now_ns`, returning all timer events that become
    /// due at or before `now_ns`.
    ///
    /// The returned events are in deterministic order: `(deadline_ns, timer_id)`.
    pub fn advance_to(&mut self, now_ns: u64) -> Vec<TimerEvent> {
        let mut events = Vec::new();

        loop {
            let Some(std::cmp::Reverse(entry)) = self.queue.peek().copied() else {
                break;
            };

            if entry.deadline_ns > now_ns {
                break;
            }

            // Consume entry before dispatching.
            self.queue.pop();

            let Some(slot) = self.timers.get_mut(&entry.timer_id) else {
                // Timer was removed (not currently supported), or state mismatch. Skip.
                continue;
            };

            if slot.generation != entry.generation {
                continue;
            }

            let Some(kind) = slot.kind else {
                continue;
            };

            match kind {
                TimerKindState::OneShot { deadline_ns } => {
                    debug_assert_eq!(deadline_ns, entry.deadline_ns);
                    events.push(TimerEvent {
                        timer_id: entry.timer_id,
                        deadline_ns,
                    });
                    // Disarm after firing.
                    slot.generation = slot.generation.wrapping_add(1);
                    slot.kind = None;
                }
                TimerKindState::Periodic {
                    next_deadline_ns,
                    period_ns,
                } => {
                    debug_assert_eq!(next_deadline_ns, entry.deadline_ns);
                    events.push(TimerEvent {
                        timer_id: entry.timer_id,
                        deadline_ns: next_deadline_ns,
                    });

                    let new_deadline = next_deadline_ns
                        .checked_add(period_ns)
                        .expect("timer deadline overflowed u64::MAX");

                    slot.kind = Some(TimerKindState::Periodic {
                        next_deadline_ns: new_deadline,
                        period_ns,
                    });
                    // Same generation: this is the same "arming", just advancing phase.
                    self.queue.push(std::cmp::Reverse(QueueEntry {
                        deadline_ns: new_deadline,
                        timer_id: entry.timer_id,
                        generation: slot.generation,
                    }));
                }
            }
        }

        events
    }

    pub fn save_state(&self) -> TimerSchedulerState {
        let mut timers: Vec<TimerState> = self
            .timers
            .iter()
            .map(|(&timer_id, slot)| TimerState {
                timer_id,
                kind: match slot.kind {
                    None => None,
                    Some(TimerKindState::OneShot { deadline_ns }) => Some(TimerKindStateRepr::OneShot {
                        deadline_ns,
                    }),
                    Some(TimerKindState::Periodic {
                        next_deadline_ns,
                        period_ns,
                    }) => Some(TimerKindStateRepr::Periodic {
                        next_deadline_ns,
                        period_ns,
                    }),
                },
            })
            .collect();
        timers.sort_by_key(|t| t.timer_id);

        TimerSchedulerState {
            next_timer_id: self.next_timer_id,
            timers,
        }
    }

    pub fn restore_state(state: TimerSchedulerState) -> Self {
        let mut scheduler = Self {
            next_timer_id: state.next_timer_id,
            timers: HashMap::with_capacity(state.timers.len()),
            queue: BinaryHeap::new(),
        };

        for timer in state.timers {
            let kind_state = match timer.kind {
                None => None,
                Some(TimerKindStateRepr::OneShot { deadline_ns }) => {
                    scheduler.queue.push(std::cmp::Reverse(QueueEntry {
                        deadline_ns,
                        timer_id: timer.timer_id,
                        generation: 0,
                    }));
                    Some(TimerKindState::OneShot { deadline_ns })
                }
                Some(TimerKindStateRepr::Periodic {
                    next_deadline_ns,
                    period_ns,
                }) => {
                    scheduler.queue.push(std::cmp::Reverse(QueueEntry {
                        deadline_ns: next_deadline_ns,
                        timer_id: timer.timer_id,
                        generation: 0,
                    }));
                    Some(TimerKindState::Periodic {
                        next_deadline_ns,
                        period_ns,
                    })
                }
            };

            scheduler.timers.insert(
                timer.timer_id,
                TimerSlot {
                    generation: 0,
                    kind: kind_state,
                },
            );
        }

        scheduler
    }

    fn cleanup_stale_queue_entries(&mut self) {
        loop {
            let Some(std::cmp::Reverse(entry)) = self.queue.peek().copied() else {
                break;
            };

            let Some(slot) = self.timers.get(&entry.timer_id) else {
                self.queue.pop();
                continue;
            };

            if slot.generation != entry.generation {
                self.queue.pop();
                continue;
            }

            let Some(kind) = slot.kind else {
                self.queue.pop();
                continue;
            };

            let active_deadline_ns = match kind {
                TimerKindState::OneShot { deadline_ns } => deadline_ns,
                TimerKindState::Periodic {
                    next_deadline_ns, ..
                } => next_deadline_ns,
            };

            if active_deadline_ns != entry.deadline_ns {
                self.queue.pop();
                continue;
            }

            break;
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimerSchedulerState {
    pub next_timer_id: u64,
    pub timers: Vec<TimerState>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimerState {
    pub timer_id: TimerId,
    pub kind: Option<TimerKindStateRepr>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TimerKindStateRepr {
    OneShot { deadline_ns: u64 },
    Periodic { next_deadline_ns: u64, period_ns: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::time::Clock;

    fn collect_events(mut scheduler: TimerScheduler, mut clock: Clock, steps: &[u64]) -> Vec<TimerEvent> {
        let mut events = Vec::new();
        for &step in steps {
            clock.advance(step);
            events.extend(scheduler.advance_to(clock.now_ns()));
        }
        events
    }

    #[test]
    fn one_shot_fires_at_deadline_boundary() {
        let mut clock = Clock::new();
        let mut sched = TimerScheduler::new();
        let t = sched.alloc_timer();
        sched.arm_one_shot(t, 100).unwrap();

        clock.advance(99);
        assert!(sched.advance_to(clock.now_ns()).is_empty());

        clock.advance(1);
        let events = sched.advance_to(clock.now_ns());
        assert_eq!(
            events,
            vec![TimerEvent {
                timer_id: t,
                deadline_ns: 100
            }]
        );

        clock.advance(1);
        assert!(sched.advance_to(clock.now_ns()).is_empty());
    }

    #[test]
    fn periodic_maintains_phase_across_chunking() {
        let mut clock = Clock::new();
        let mut sched = TimerScheduler::new();
        let t = sched.alloc_timer();
        sched.arm_periodic(t, 10, 5).unwrap();

        // Single-step to 30.
        clock.advance(30);
        let events = sched.advance_to(clock.now_ns());
        let deadlines: Vec<u64> = events.iter().map(|e| e.deadline_ns).collect();
        assert_eq!(deadlines, vec![10, 15, 20, 25, 30]);

        // Next firing should be at 35.
        clock.advance(4);
        assert!(sched.advance_to(clock.now_ns()).is_empty());
        clock.advance(1);
        let events = sched.advance_to(clock.now_ns());
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].deadline_ns, 35);
    }

    #[test]
    fn deterministic_vs_chunked_advance() {
        let mut sched = TimerScheduler::new();
        let t1 = sched.alloc_timer();
        let t2 = sched.alloc_timer();
        sched.arm_periodic(t1, 10, 10).unwrap();
        sched.arm_one_shot(t2, 25).unwrap();

        let events_single = collect_events(sched.clone(), Clock::new(), &[100]);
        let events_chunked = collect_events(sched, Clock::new(), &[7, 3, 11, 79]);
        assert_eq!(events_single, events_chunked);
    }

    #[test]
    fn save_restore_round_trip_preserves_future_events() {
        let mut clock = Clock::new();
        let mut sched = TimerScheduler::new();
        let t = sched.alloc_timer();
        sched.arm_periodic(t, 10, 10).unwrap();

        clock.advance(25);
        let events_before = sched.advance_to(clock.now_ns());
        assert_eq!(events_before.iter().map(|e| e.deadline_ns).collect::<Vec<_>>(), vec![10, 20]);

        let state = sched.save_state();
        let mut restored = TimerScheduler::restore_state(state);

        clock.advance(30); // Now at 55
        let events_after = restored.advance_to(clock.now_ns());
        assert_eq!(
            events_after.iter().map(|e| e.deadline_ns).collect::<Vec<_>>(),
            vec![30, 40, 50]
        );
    }

    #[test]
    fn same_deadline_is_ordered_by_timer_id() {
        let mut clock = Clock::new();
        let mut sched = TimerScheduler::new();

        let t1 = sched.alloc_timer();
        let t2 = sched.alloc_timer();
        sched.arm_one_shot(t2, 10).unwrap();
        sched.arm_one_shot(t1, 10).unwrap();

        clock.advance(10);
        let events = sched.advance_to(clock.now_ns());
        assert_eq!(
            events,
            vec![
                TimerEvent {
                    timer_id: t1,
                    deadline_ns: 10
                },
                TimerEvent {
                    timer_id: t2,
                    deadline_ns: 10
                }
            ]
        );
    }

    #[test]
    fn disarm_cancels_pending_event_and_next_deadline_skips_stale_entries() {
        let mut clock = Clock::new();
        let mut sched = TimerScheduler::new();
        let t = sched.alloc_timer();

        sched.arm_one_shot(t, 10).unwrap();
        sched.disarm(t).unwrap();

        assert_eq!(sched.next_deadline_ns(), None);

        clock.advance(20);
        assert!(sched.advance_to(clock.now_ns()).is_empty());
    }

    #[test]
    fn rearm_overwrites_previous_deadline_without_firing_old_entry() {
        let mut clock = Clock::new();
        let mut sched = TimerScheduler::new();
        let t = sched.alloc_timer();

        sched.arm_one_shot(t, 10).unwrap();
        sched.arm_one_shot(t, 20).unwrap();

        assert_eq!(sched.next_deadline_ns(), Some(20));

        clock.advance(15);
        assert!(sched.advance_to(clock.now_ns()).is_empty());

        clock.advance(5);
        let events = sched.advance_to(clock.now_ns());
        assert_eq!(
            events,
            vec![TimerEvent {
                timer_id: t,
                deadline_ns: 20
            }]
        );
    }
}
