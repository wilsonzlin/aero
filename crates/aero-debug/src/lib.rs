use std::collections::{HashMap, HashSet, VecDeque};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PauseReason {
    Manual,
    Breakpoint { rip: u64 },
    SingleStep,
    Watchpoint { id: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRange {
    pub start: u64,
    pub len: u64,
}

impl MemoryRange {
    pub fn end_exclusive(self) -> u64 {
        self.start.saturating_add(self.len)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum WatchpointKind {
    Read,
    Write,
    ReadWrite,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TraceFilter {
    pub include_instructions: bool,
    pub include_port_io: bool,
    pub include_mmio: bool,
    pub include_interrupts: bool,
    pub sample_rate: u32,
}

impl Default for TraceFilter {
    fn default() -> Self {
        Self {
            include_instructions: false,
            include_port_io: true,
            include_mmio: true,
            include_interrupts: true,
            sample_rate: 1,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TraceEvent {
    Instruction { rip: u64, bytes: Vec<u8> },
    PortRead { port: u16, size: u8, value: u32 },
    PortWrite { port: u16, size: u8, value: u32 },
    MmioRead { addr: u64, size: u8, value: u64 },
    MmioWrite { addr: u64, size: u8, value: u64 },
    Interrupt { vector: u8 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunState {
    Running,
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecDecision {
    Continue,
    Pause(PauseReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Watchpoint {
    pub id: u64,
    pub range: MemoryRange,
    pub kind: WatchpointKind,
}

#[derive(Debug)]
pub struct Debugger {
    run_state: RunState,
    breakpoints: HashSet<u64>,
    watchpoints: HashMap<u64, Watchpoint>,
    next_watchpoint_id: u64,
    remaining_single_steps: u32,
}

impl Debugger {
    pub fn new() -> Self {
        Self {
            run_state: RunState::Running,
            breakpoints: HashSet::new(),
            watchpoints: HashMap::new(),
            next_watchpoint_id: 1,
            remaining_single_steps: 0,
        }
    }

    pub fn run_state(&self) -> RunState {
        self.run_state
    }

    pub fn pause(&mut self) {
        self.run_state = RunState::Paused;
        self.remaining_single_steps = 0;
    }

    pub fn resume(&mut self) {
        self.run_state = RunState::Running;
        self.remaining_single_steps = 0;
    }

    pub fn request_single_step(&mut self) {
        self.run_state = RunState::Running;
        self.remaining_single_steps = 1;
    }

    pub fn set_breakpoint(&mut self, rip: u64) {
        self.breakpoints.insert(rip);
    }

    pub fn remove_breakpoint(&mut self, rip: u64) -> bool {
        self.breakpoints.remove(&rip)
    }

    pub fn clear_breakpoints(&mut self) {
        self.breakpoints.clear();
    }

    pub fn add_watchpoint(&mut self, range: MemoryRange, kind: WatchpointKind) -> u64 {
        let id = self.next_watchpoint_id;
        self.next_watchpoint_id = self.next_watchpoint_id.saturating_add(1);

        self.watchpoints.insert(id, Watchpoint { id, range, kind });
        id
    }

    pub fn remove_watchpoint(&mut self, id: u64) -> bool {
        self.watchpoints.remove(&id).is_some()
    }

    pub fn clear_watchpoints(&mut self) {
        self.watchpoints.clear();
    }

    pub fn check_before_exec(&mut self, rip: u64) -> ExecDecision {
        match self.run_state {
            RunState::Paused => ExecDecision::Pause(PauseReason::Manual),
            RunState::Running => {
                if self.breakpoints.contains(&rip) {
                    self.run_state = RunState::Paused;
                    self.remaining_single_steps = 0;
                    ExecDecision::Pause(PauseReason::Breakpoint { rip })
                } else {
                    ExecDecision::Continue
                }
            }
        }
    }

    pub fn check_after_exec(&mut self) -> ExecDecision {
        if self.run_state == RunState::Paused {
            return ExecDecision::Pause(PauseReason::Manual);
        }

        if self.remaining_single_steps == 0 {
            return ExecDecision::Continue;
        }

        self.remaining_single_steps -= 1;
        if self.remaining_single_steps == 0 {
            self.run_state = RunState::Paused;
            ExecDecision::Pause(PauseReason::SingleStep)
        } else {
            ExecDecision::Continue
        }
    }

    pub fn check_watchpoint(
        &mut self,
        addr: u64,
        len: u64,
        access: WatchpointKind,
    ) -> ExecDecision {
        if self.run_state == RunState::Paused {
            return ExecDecision::Pause(PauseReason::Manual);
        }

        let range = MemoryRange { start: addr, len };
        for watch in self.watchpoints.values() {
            if !ranges_overlap(range, watch.range) {
                continue;
            }

            if watchpoint_kind_matches(access, watch.kind) {
                self.run_state = RunState::Paused;
                self.remaining_single_steps = 0;
                return ExecDecision::Pause(PauseReason::Watchpoint { id: watch.id });
            }
        }

        ExecDecision::Continue
    }
}

impl Default for Debugger {
    fn default() -> Self {
        Self::new()
    }
}

fn watchpoint_kind_matches(access: WatchpointKind, configured: WatchpointKind) -> bool {
    match (access, configured) {
        (WatchpointKind::Read, WatchpointKind::Read) => true,
        (WatchpointKind::Write, WatchpointKind::Write) => true,
        (WatchpointKind::Read, WatchpointKind::ReadWrite) => true,
        (WatchpointKind::Write, WatchpointKind::ReadWrite) => true,
        (WatchpointKind::ReadWrite, _) => true,
        _ => false,
    }
}

fn ranges_overlap(a: MemoryRange, b: MemoryRange) -> bool {
    let a_end = a.end_exclusive();
    let b_end = b.end_exclusive();
    a.start < b_end && b.start < a_end
}

#[derive(Debug)]
pub struct Tracer {
    enabled: bool,
    filter: TraceFilter,
    sample_counter: u64,
    max_events: usize,
    events: VecDeque<TraceEvent>,
}

impl Default for Tracer {
    fn default() -> Self {
        Self::new(16 * 1024)
    }
}

impl Tracer {
    pub fn new(max_events: usize) -> Self {
        Self {
            enabled: false,
            filter: TraceFilter::default(),
            sample_counter: 0,
            max_events: max_events.max(1),
            events: VecDeque::new(),
        }
    }

    pub fn enable(&mut self, filter: TraceFilter) {
        self.enabled = true;
        self.filter = filter;
    }

    pub fn disable(&mut self) {
        self.enabled = false;
    }

    pub fn clear(&mut self) {
        self.events.clear();
    }

    pub fn record(&mut self, event: TraceEvent) {
        if !self.enabled {
            return;
        }

        if !self.filter_allows(&event) {
            return;
        }

        if !self.sample_allows() {
            return;
        }

        if self.events.len() == self.max_events {
            self.events.pop_front();
        }
        self.events.push_back(event);
    }

    pub fn drain(&mut self, max: usize) -> Vec<TraceEvent> {
        let mut out = Vec::new();
        let max = max.min(self.events.len());
        for _ in 0..max {
            if let Some(event) = self.events.pop_front() {
                out.push(event);
            }
        }
        out
    }

    pub fn export_json(&self) -> Result<Vec<u8>, serde_json::Error> {
        serde_json::to_vec(&self.events)
    }

    fn filter_allows(&self, event: &TraceEvent) -> bool {
        match event {
            TraceEvent::Instruction { .. } => self.filter.include_instructions,
            TraceEvent::PortRead { .. } | TraceEvent::PortWrite { .. } => {
                self.filter.include_port_io
            }
            TraceEvent::MmioRead { .. } | TraceEvent::MmioWrite { .. } => self.filter.include_mmio,
            TraceEvent::Interrupt { .. } => self.filter.include_interrupts,
        }
    }

    fn sample_allows(&mut self) -> bool {
        let rate = self.filter.sample_rate.max(1);
        if rate == 1 {
            return true;
        }

        self.sample_counter = self.sample_counter.wrapping_add(1);
        self.sample_counter % rate as u64 == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn breakpoint_pauses_before_exec() {
        let mut dbg = Debugger::new();
        dbg.set_breakpoint(0x1000);

        assert_eq!(dbg.check_before_exec(0x0), ExecDecision::Continue);
        assert_eq!(
            dbg.check_before_exec(0x1000),
            ExecDecision::Pause(PauseReason::Breakpoint { rip: 0x1000 })
        );
        assert_eq!(dbg.run_state(), RunState::Paused);
    }

    #[test]
    fn single_step_pauses_after_one_exec() {
        let mut dbg = Debugger::new();
        dbg.pause();
        dbg.request_single_step();

        assert_eq!(dbg.check_before_exec(0x2000), ExecDecision::Continue);
        assert_eq!(
            dbg.check_after_exec(),
            ExecDecision::Pause(PauseReason::SingleStep)
        );
        assert_eq!(dbg.run_state(), RunState::Paused);
    }

    #[test]
    fn tracer_filters_by_type() {
        let mut tracer = Tracer::new(8);
        tracer.enable(TraceFilter {
            include_instructions: false,
            include_port_io: true,
            include_mmio: false,
            include_interrupts: false,
            sample_rate: 1,
        });

        tracer.record(TraceEvent::Instruction {
            rip: 0,
            bytes: vec![0x90],
        });
        tracer.record(TraceEvent::PortWrite {
            port: 0x3F8,
            size: 1,
            value: 0x41,
        });

        let drained = tracer.drain(10);
        assert_eq!(
            drained,
            vec![TraceEvent::PortWrite {
                port: 0x3F8,
                size: 1,
                value: 0x41
            }]
        );
    }
}
