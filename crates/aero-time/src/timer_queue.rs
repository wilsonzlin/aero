use std::cmp::Ordering;
use std::collections::{BinaryHeap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TimerId(u64);

impl TimerId {
    fn next(next_id: &mut u64) -> Self {
        let id = *next_id;
        *next_id = next_id.wrapping_add(1);
        TimerId(id)
    }

    pub fn raw(self) -> u64 {
        self.0
    }
}

#[derive(Debug, Clone)]
pub struct TimerEvent<T> {
    pub id: TimerId,
    pub deadline_ns: u64,
    pub payload: T,
}

#[derive(Debug)]
struct Scheduled<T> {
    id: TimerId,
    deadline_ns: u64,
    payload: T,
}

impl<T> PartialEq for Scheduled<T> {
    fn eq(&self, other: &Self) -> bool {
        self.deadline_ns == other.deadline_ns && self.id == other.id
    }
}

impl<T> Eq for Scheduled<T> {}

impl<T> Ord for Scheduled<T> {
    fn cmp(&self, other: &Self) -> Ordering {
        other
            .deadline_ns
            .cmp(&self.deadline_ns)
            .then_with(|| other.id.0.cmp(&self.id.0))
    }
}

impl<T> PartialOrd for Scheduled<T> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

#[derive(Debug)]
pub struct TimerQueue<T> {
    next_id: u64,
    heap: BinaryHeap<Scheduled<T>>,
    canceled: HashSet<TimerId>,
}

impl<T> TimerQueue<T> {
    pub fn new() -> Self {
        Self {
            next_id: 0,
            heap: BinaryHeap::new(),
            canceled: HashSet::new(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    pub fn schedule(&mut self, deadline_ns: u64, payload: T) -> TimerId {
        let id = TimerId::next(&mut self.next_id);
        self.heap.push(Scheduled {
            id,
            deadline_ns,
            payload,
        });
        id
    }

    pub fn cancel(&mut self, id: TimerId) {
        self.canceled.insert(id);
    }

    fn prune_canceled(&mut self) {
        while let Some(top) = self.heap.peek() {
            if self.canceled.remove(&top.id) {
                self.heap.pop();
                continue;
            }
            break;
        }
    }

    pub fn next_deadline_ns(&mut self) -> Option<u64> {
        self.prune_canceled();
        self.heap.peek().map(|e| e.deadline_ns)
    }

    pub fn pop_due(&mut self, now_ns: u64) -> Option<TimerEvent<T>> {
        self.prune_canceled();
        let top = self.heap.peek()?;
        if top.deadline_ns > now_ns {
            return None;
        }
        let ev = self.heap.pop()?;
        Some(TimerEvent {
            id: ev.id,
            deadline_ns: ev.deadline_ns,
            payload: ev.payload,
        })
    }
}

impl<T> Default for TimerQueue<T> {
    fn default() -> Self {
        Self::new()
    }
}
