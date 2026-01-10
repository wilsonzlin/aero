//! A minimal "machine" model: multiple vCPUs, shared memory, and APIC IPI wiring.

use super::cpu::CpuState;
use super::lapic::{DeliveryMode, DestinationShorthand, Icr, Level, LocalApic};

#[derive(Debug, Clone)]
pub struct Vcpu {
    pub cpu: CpuState,
    pub apic: LocalApic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryError {
    OutOfBounds,
    Misaligned,
    InvalidTrampolineAddress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Trampoline {
    pub start_paddr: u64,
    pub vector: u8,
    pub code_len: usize,
}

#[derive(Debug, Clone)]
pub struct Machine {
    pub cpus: Vec<Vcpu>,
    pub memory: Vec<u8>,
    pub trampoline: Option<Trampoline>,
}

impl Machine {
    pub fn new(cpu_count: usize, memory_size: usize) -> Self {
        assert!(cpu_count >= 1, "must have at least one CPU (BSP)");

        let mut cpus = Vec::with_capacity(cpu_count);
        for i in 0..cpu_count {
            let apic_id = i as u8;
            let cpu = if i == 0 {
                CpuState::new_bsp(apic_id)
            } else {
                CpuState::new_ap(apic_id)
            };
            let apic = LocalApic::new(apic_id);
            cpus.push(Vcpu { cpu, apic });
        }

        Self {
            cpus,
            memory: vec![0; memory_size],
            trampoline: None,
        }
    }

    pub fn cpu_count(&self) -> usize {
        self.cpus.len()
    }

    pub fn cpu_index_by_apic_id(&self, apic_id: u8) -> Option<usize> {
        self.cpus.iter().position(|c| c.cpu.apic_id == apic_id)
    }

    pub fn read_local_apic(&self, cpu: usize, offset: u64) -> u32 {
        self.cpus[cpu].apic.read(offset)
    }

    pub fn write_local_apic(&mut self, cpu: usize, offset: u64, value: u32) {
        let maybe_icr = self.cpus[cpu].apic.write(offset, value);
        if let Some(icr) = maybe_icr {
            self.deliver_ipi(cpu, icr);
        }
    }

    fn deliver_ipi(&mut self, sender_cpu: usize, icr: Icr) {
        let dest_cpus = self.resolve_ipi_destinations(sender_cpu, icr);

        match icr.delivery_mode {
            DeliveryMode::Fixed => {
                for cpu in dest_cpus {
                    self.cpus[cpu].apic.push_interrupt(icr.vector);
                }
            }
            DeliveryMode::Init => {
                if icr.level != Level::Assert {
                    // INIT deassert is used in real systems as part of the INIT sequence.
                    // We model only the assert side as it performs the reset.
                    return;
                }

                for cpu in dest_cpus {
                    if cpu == sender_cpu {
                        // Self-INIT is undefined and not used for SMP bring-up.
                        continue;
                    }
                    self.cpus[cpu].cpu.receive_init();
                    // Reset local APIC state for the target while keeping its ID stable.
                    let apic_id = self.cpus[cpu].apic.apic_id;
                    self.cpus[cpu].apic = LocalApic::new(apic_id);
                }
            }
            DeliveryMode::Startup => {
                for cpu in dest_cpus {
                    if cpu == sender_cpu {
                        continue;
                    }
                    self.cpus[cpu].cpu.receive_sipi(icr.vector);
                }
            }
            // Other delivery modes (NMI, SMI, etc.) are not required for the SMP
            // boot and IPI plumbing validated by this crate.
            _ => {}
        }
    }

    fn resolve_ipi_destinations(&self, sender_cpu: usize, icr: Icr) -> Vec<usize> {
        match icr.destination_shorthand {
            DestinationShorthand::SelfOnly => vec![sender_cpu],
            DestinationShorthand::AllIncludingSelf => (0..self.cpus.len()).collect(),
            DestinationShorthand::AllExcludingSelf => {
                (0..self.cpus.len()).filter(|&i| i != sender_cpu).collect()
            }
            DestinationShorthand::None => self
                .cpu_index_by_apic_id(icr.destination)
                .into_iter()
                .collect(),
        }
    }

    pub fn pop_pending_interrupt(&mut self, cpu: usize) -> Option<u8> {
        self.cpus[cpu].apic.pop_interrupt()
    }

    pub fn write_memory(&mut self, paddr: u64, data: &[u8]) -> Result<(), MemoryError> {
        let start = usize::try_from(paddr).map_err(|_| MemoryError::OutOfBounds)?;
        let end = start
            .checked_add(data.len())
            .ok_or(MemoryError::OutOfBounds)?;
        if end > self.memory.len() {
            return Err(MemoryError::OutOfBounds);
        }

        self.memory[start..end].copy_from_slice(data);
        Ok(())
    }

    /// Install the AP startup trampoline at a SIPI-vector address.
    ///
    /// The SIPI vector is an 8-bit 4KiB page number, so the physical address
    /// must be aligned to 4KiB and below 1MiB.
    pub fn install_trampoline(
        &mut self,
        start_paddr: u64,
        code: &[u8],
    ) -> Result<Trampoline, MemoryError> {
        if start_paddr & 0xFFF != 0 {
            return Err(MemoryError::Misaligned);
        }
        if start_paddr >= 0x1_00000 {
            return Err(MemoryError::InvalidTrampolineAddress);
        }

        let vector = ((start_paddr >> 12) & 0xFF) as u8;

        self.write_memory(start_paddr, code)?;
        let tramp = Trampoline {
            start_paddr,
            vector,
            code_len: code.len(),
        };
        self.trampoline = Some(tramp);
        Ok(tramp)
    }
}
