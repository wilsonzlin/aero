//! Native-only PC machine integration built on [`aero_pc_platform::PcPlatform`].
//!
//! This is an incremental step toward folding the PCI-capable PC platform into
//! the canonical `aero-machine` run loop.
//!
//! Unlike [`crate::Machine`], this wiring currently does *not* include BIOS POST
//! or INT dispatch; it is primarily intended for platform/CPU integration tests.
#![cfg(not(target_arch = "wasm32"))]

use aero_cpu_core::assist::AssistContext;
use aero_cpu_core::interp::tier0::exec::{run_batch_cpu_core_with_assists, BatchExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::interrupts::InterruptController as _;
use aero_cpu_core::state::{CpuMode, CpuState};
use aero_cpu_core::{Exception, CpuCore};
use aero_pc_platform::{PcCpuBus, PcPlatform, ResetEvent};
use aero_platform::reset::ResetKind;

use crate::RunExit;

/// PCI-capable PC machine (CPU + [`PcPlatform`]) with a Tier-0 `run_slice` loop.
///
/// The key feature is that the run loop actively polls the platform interrupt
/// controller (PIC/IOAPIC+LAPIC) and injects pending vectors into
/// `cpu.pending.external_interrupts` so the Tier-0 dispatcher can deliver them.
pub struct PcMachine {
    pub cpu: CpuCore,
    pub bus: PcCpuBus,
    assist: AssistContext,
}

impl PcMachine {
    /// Construct a new PC machine with `ram_size_bytes` of guest RAM.
    pub fn new(ram_size_bytes: usize) -> Self {
        let platform = PcPlatform::new(ram_size_bytes);
        Self {
            cpu: CpuCore::new(CpuMode::Real),
            bus: PcCpuBus::new(platform),
            assist: AssistContext::default(),
        }
    }

    /// Returns the current CPU state.
    pub fn cpu(&self) -> &CpuState {
        &self.cpu.state
    }

    /// Mutable access to the current CPU state (debug/testing only).
    pub fn cpu_mut(&mut self) -> &mut CpuState {
        &mut self.cpu.state
    }

    fn take_reset_kind(&mut self) -> Option<ResetKind> {
        // Preserve ordering, but only surface a single event per slice.
        self.bus
            .platform
            .take_reset_events()
            .into_iter()
            .next()
            .map(|ev| match ev {
                ResetEvent::Cpu => ResetKind::Cpu,
                ResetEvent::System => ResetKind::System,
            })
    }

    fn poll_and_queue_one_external_interrupt(&mut self) {
        // Avoid unbounded growth of the external interrupt FIFO if the guest has
        // IF=0, interrupts are inhibited, etc.
        //
        // Also avoids tight polling loops when a level-triggered interrupt line
        // stays asserted.
        const MAX_QUEUED_EXTERNAL_INTERRUPTS: usize = 1;
        if self.cpu.pending.external_interrupts.len() >= MAX_QUEUED_EXTERNAL_INTERRUPTS {
            return;
        }

        let mut ctrl = self.bus.interrupt_controller();
        if let Some(vector) = ctrl.poll_interrupt() {
            self.cpu.pending.inject_external_interrupt(vector);
        }
    }

    /// Run the CPU for at most `max_insts` guest instructions.
    pub fn run_slice(&mut self, max_insts: u64) -> RunExit {
        let mut executed = 0u64;
        let cfg = Tier0Config::from_cpuid(&self.assist.features);

        while executed < max_insts {
            if let Some(kind) = self.take_reset_kind() {
                return RunExit::ResetRequested { kind, executed };
            }

            // Keep the CPU's A20 view coherent with the chipset latch.
            self.cpu.state.a20_enabled = self.bus.platform.chipset.a20().enabled();

            // Device-level polling: propagate PCI INTx line levels into the
            // platform interrupt controller via routing.
            self.bus.platform.poll_pci_intx_lines();

            // Poll the platform interrupt controller (PIC/IOAPIC+LAPIC) and
            // inject at most one vector into the CPU's external interrupt FIFO.
            self.poll_and_queue_one_external_interrupt();

            let remaining = max_insts - executed;
            let batch = run_batch_cpu_core_with_assists(
                &cfg,
                &mut self.assist,
                &mut self.cpu,
                &mut self.bus,
                remaining,
            );
            executed = executed.saturating_add(batch.executed);

            match batch.exit {
                BatchExit::Completed => return RunExit::Completed { executed },
                BatchExit::Branch => continue,
                BatchExit::Halted => return RunExit::Halted { executed },
                BatchExit::BiosInterrupt(_vector) => {
                    return RunExit::Exception {
                        exception: Exception::Unimplemented("pc machine has no BIOS INT handler"),
                        executed,
                    };
                }
                BatchExit::Assist(reason) => return RunExit::Assist { reason, executed },
                BatchExit::Exception(exception) => {
                    return RunExit::Exception {
                        exception,
                        executed,
                    };
                }
                BatchExit::CpuExit(exit) => return RunExit::CpuExit { exit, executed },
            }
        }

        RunExit::Completed { executed }
    }
}
