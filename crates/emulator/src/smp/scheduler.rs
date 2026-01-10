//! Deterministic time-sliced scheduling for multiple vCPUs.
//!
//! In the browser we can either map vCPUs to Web Workers (when threads are
//! available) or run multiple vCPUs inside a single worker using a deterministic
//! scheduler. This module models the latter: a round-robin scheduler where each
//! "tick" executes one slice of one vCPU.

use super::cpu::VcpuRunState;
use super::machine::Machine;

pub trait Guest {
    fn on_tick(&mut self, cpu: usize, machine: &mut Machine);
    fn on_interrupt(&mut self, cpu: usize, vector: u8, machine: &mut Machine);
}

/// A simple deterministic round-robin scheduler.
#[derive(Debug, Clone)]
pub struct DeterministicScheduler {
    next_cpu: usize,
}

impl DeterministicScheduler {
    pub fn new() -> Self {
        Self { next_cpu: 0 }
    }

    /// Run the machine for a fixed number of scheduler ticks.
    pub fn run_for_ticks(&mut self, machine: &mut Machine, guest: &mut impl Guest, ticks: u64) {
        let cpu_count = machine.cpu_count();
        for _ in 0..ticks {
            let cpu = self.next_cpu;
            self.next_cpu = (self.next_cpu + 1) % cpu_count;

            // Deliver at most one pending interrupt per tick to keep ordering deterministic.
            let pending = machine.pop_pending_interrupt(cpu);
            if let Some(vector) = pending {
                let run_state = machine.cpus[cpu].cpu.run_state;
                if run_state == VcpuRunState::Halted {
                    machine.cpus[cpu].cpu.run_state = VcpuRunState::Running;
                }
                guest.on_interrupt(cpu, vector, machine);
            }

            if machine.cpus[cpu].cpu.run_state == VcpuRunState::Running {
                guest.on_tick(cpu, machine);
            }
        }
    }
}
