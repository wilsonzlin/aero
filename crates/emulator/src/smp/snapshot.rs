//! Snapshot adapter for the minimal SMP machine model.
//!
//! This bridges `emulator::smp::Machine` into the `aero-snapshot` save/restore
//! pipeline so we can validate multi-vCPU snapshots against a small, fully
//! deterministic SMP/APIC model.

use std::collections::HashSet;
use std::io::{Cursor, Read};

use aero_snapshot::{
    CpuState as SnapshotCpuState, DeviceId, DeviceState, DiskOverlayRefs, MmuState, Result,
    SnapshotError, SnapshotMeta, SnapshotSource, SnapshotTarget, VcpuSnapshot,
};

use super::lapic::APIC_REG_ICR_HIGH;
use super::{Machine, VcpuRunState};

const SMP_INTERNAL_VERSION: u16 = 1;

#[derive(Debug)]
struct SmpCpuInternal {
    is_bsp: bool,
    run_state: VcpuRunState,
    sipi_vector: Option<u8>,
    icr_high: u32,
    pending_interrupts: Vec<u8>,
}

fn encode_smp_cpu_internal(machine_cpu: &super::CpuState, apic: &super::LocalApic) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.extend_from_slice(&SMP_INTERNAL_VERSION.to_le_bytes());
    buf.push(machine_cpu.is_bsp as u8);
    buf.push(match machine_cpu.run_state {
        VcpuRunState::Running => 0,
        VcpuRunState::Halted => 1,
        VcpuRunState::WaitForSipi => 2,
    });
    match machine_cpu.sipi_vector {
        Some(v) => {
            buf.push(1);
            buf.push(v);
        }
        None => {
            buf.push(0);
            buf.push(0);
        }
    }
    buf.extend_from_slice(&apic.icr_high().to_le_bytes());
    let pending = apic.pending_interrupts();
    let pending_len: u32 = pending.len().try_into().unwrap_or(u32::MAX);
    buf.extend_from_slice(&pending_len.to_le_bytes());
    buf.extend_from_slice(&pending);
    buf
}

fn decode_smp_cpu_internal(data: &[u8]) -> Result<SmpCpuInternal> {
    let mut r = Cursor::new(data);

    let mut version_bytes = [0u8; 2];
    r.read_exact(&mut version_bytes)?;
    let version = u16::from_le_bytes(version_bytes);
    if version != SMP_INTERNAL_VERSION {
        return Err(SnapshotError::Corrupt(
            "unsupported SMP CPU internal version",
        ));
    }

    let mut b = [0u8; 1];
    r.read_exact(&mut b)?;
    let is_bsp = match b[0] {
        0 => false,
        1 => true,
        _ => return Err(SnapshotError::Corrupt("invalid is_bsp flag")),
    };

    r.read_exact(&mut b)?;
    let run_state = match b[0] {
        0 => VcpuRunState::Running,
        1 => VcpuRunState::Halted,
        2 => VcpuRunState::WaitForSipi,
        _ => return Err(SnapshotError::Corrupt("invalid run state")),
    };

    r.read_exact(&mut b)?;
    let sipi_present = b[0];
    r.read_exact(&mut b)?;
    let sipi_vector = match sipi_present {
        0 => None,
        1 => Some(b[0]),
        _ => return Err(SnapshotError::Corrupt("invalid SIPI presence tag")),
    };

    let mut icr_high_bytes = [0u8; 4];
    r.read_exact(&mut icr_high_bytes)?;
    let icr_high = u32::from_le_bytes(icr_high_bytes);

    let mut pending_len_bytes = [0u8; 4];
    r.read_exact(&mut pending_len_bytes)?;
    let pending_len = u32::from_le_bytes(pending_len_bytes) as usize;

    // Avoid allocating a ridiculous amount of memory on corrupt inputs.
    if pending_len > 1_048_576 {
        return Err(SnapshotError::Corrupt("pending interrupt queue too large"));
    }

    let mut pending_interrupts = vec![0u8; pending_len];
    r.read_exact(&mut pending_interrupts)?;

    Ok(SmpCpuInternal {
        is_bsp,
        run_state,
        sipi_vector,
        icr_high,
        pending_interrupts,
    })
}

fn encode_trampoline(trampoline: Option<super::Trampoline>) -> Vec<u8> {
    let mut buf = Vec::new();
    match trampoline {
        Some(tramp) => {
            buf.push(1);
            buf.extend_from_slice(&tramp.start_paddr.to_le_bytes());
            buf.push(tramp.vector);
            buf.extend_from_slice(&(tramp.code_len as u64).to_le_bytes());
        }
        None => {
            buf.push(0);
        }
    }
    buf
}

fn decode_trampoline(data: &[u8]) -> Result<Option<super::Trampoline>> {
    let mut r = Cursor::new(data);
    let mut tag = [0u8; 1];
    r.read_exact(&mut tag)?;
    match tag[0] {
        0 => Ok(None),
        1 => {
            let mut start_bytes = [0u8; 8];
            r.read_exact(&mut start_bytes)?;
            let start_paddr = u64::from_le_bytes(start_bytes);

            let mut vector = [0u8; 1];
            r.read_exact(&mut vector)?;
            let vector = vector[0];

            let mut len_bytes = [0u8; 8];
            r.read_exact(&mut len_bytes)?;
            let code_len = u64::from_le_bytes(len_bytes) as usize;

            Ok(Some(super::Trampoline {
                start_paddr,
                vector,
                code_len,
            }))
        }
        _ => Err(SnapshotError::Corrupt("invalid trampoline presence tag")),
    }
}

impl SnapshotSource for Machine {
    fn snapshot_meta(&mut self) -> SnapshotMeta {
        SnapshotMeta::default()
    }

    fn cpu_state(&self) -> SnapshotCpuState {
        // Legacy single-CPU snapshots map to the BSP by convention.
        let mut cpu = SnapshotCpuState::default();
        cpu.rip = self.cpus[0].cpu.rip;
        cpu
    }

    fn cpu_states(&self) -> Vec<VcpuSnapshot> {
        self.cpus
            .iter()
            .map(|vcpu| {
                let mut cpu = SnapshotCpuState::default();
                cpu.rip = vcpu.cpu.rip;

                VcpuSnapshot {
                    apic_id: vcpu.cpu.apic_id as u32,
                    cpu,
                    internal_state: encode_smp_cpu_internal(&vcpu.cpu, &vcpu.apic),
                }
            })
            .collect()
    }

    fn mmu_state(&self) -> MmuState {
        MmuState::default()
    }

    fn device_states(&self) -> Vec<DeviceState> {
        vec![DeviceState {
            id: DeviceId::BIOS,
            version: 1,
            flags: 0,
            data: encode_trampoline(self.trampoline),
        }]
    }

    fn disk_overlays(&self) -> DiskOverlayRefs {
        DiskOverlayRefs::default()
    }

    fn ram_len(&self) -> usize {
        self.memory.len()
    }

    fn read_ram(&self, offset: u64, buf: &mut [u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(buf.len())
            .ok_or(SnapshotError::Corrupt("ram read overflow"))?;
        if end > self.memory.len() {
            return Err(SnapshotError::Corrupt("ram read out of bounds"));
        }
        buf.copy_from_slice(&self.memory[offset..end]);
        Ok(())
    }

    fn take_dirty_pages(&mut self) -> Option<Vec<u64>> {
        None
    }
}

impl SnapshotTarget for Machine {
    fn restore_cpu_state(&mut self, state: SnapshotCpuState) {
        // Legacy single-CPU snapshots restore into the BSP by convention.
        self.cpus[0].cpu.rip = state.rip;
    }

    fn restore_cpu_states(&mut self, states: Vec<VcpuSnapshot>) -> Result<()> {
        if states.len() != self.cpus.len() {
            return Err(SnapshotError::Corrupt("CPU count mismatch"));
        }

        let mut seen = HashSet::with_capacity(states.len());
        for cpu in states {
            let apic_id: u8 = cpu
                .apic_id
                .try_into()
                .map_err(|_| SnapshotError::Corrupt("APIC ID out of range"))?;
            if !seen.insert(apic_id) {
                return Err(SnapshotError::Corrupt(
                    "duplicate APIC ID in CPU list (apic_id must be unique)",
                ));
            }

            let idx = self
                .cpu_index_by_apic_id(apic_id)
                .ok_or(SnapshotError::Corrupt("unknown APIC ID"))?;

            let internal = decode_smp_cpu_internal(&cpu.internal_state)?;

            let vcpu = &mut self.cpus[idx];
            vcpu.cpu.apic_id = apic_id;
            vcpu.cpu.is_bsp = internal.is_bsp;
            vcpu.cpu.rip = cpu.cpu.rip;
            vcpu.cpu.run_state = internal.run_state;
            vcpu.cpu.sipi_vector = internal.sipi_vector;

            vcpu.apic.apic_id = apic_id;
            vcpu.apic.set_icr_high(internal.icr_high);
            vcpu.apic
                .set_pending_interrupts(internal.pending_interrupts);
        }

        // Sanity: preserve observable ICR register state.
        for (idx, vcpu) in self.cpus.iter().enumerate() {
            let icr_high = vcpu.apic.icr_high();
            let reg = self.read_local_apic(idx, APIC_REG_ICR_HIGH);
            if reg != icr_high {
                return Err(SnapshotError::Corrupt(
                    "local APIC ICR_HIGH mismatch after restore",
                ));
            }
        }

        Ok(())
    }

    fn restore_mmu_state(&mut self, _state: MmuState) {}

    fn restore_device_states(&mut self, states: Vec<DeviceState>) {
        for state in states {
            if state.id == DeviceId::BIOS && state.version == 1 {
                if let Ok(tramp) = decode_trampoline(&state.data) {
                    self.trampoline = tramp;
                }
            }
        }
    }

    fn restore_disk_overlays(&mut self, _overlays: DiskOverlayRefs) {}

    fn ram_len(&self) -> usize {
        self.memory.len()
    }

    fn write_ram(&mut self, offset: u64, data: &[u8]) -> Result<()> {
        let offset: usize = offset
            .try_into()
            .map_err(|_| SnapshotError::Corrupt("ram offset overflow"))?;
        let end = offset
            .checked_add(data.len())
            .ok_or(SnapshotError::Corrupt("ram write overflow"))?;
        if end > self.memory.len() {
            return Err(SnapshotError::Corrupt("ram write out of bounds"));
        }
        self.memory[offset..end].copy_from_slice(data);
        Ok(())
    }
}
