use std::ops::Range;

use aero_cpu_core::interp::tier0::exec::StepExit;
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::state::{gpr, CpuMode, CpuState as CoreState};
use aero_cpu_core::{CpuBus, CpuCore, Exception};

use crate::corpus::TestCase;
use crate::{CpuState, ExecOutcome, Fault};

pub struct AeroBackend {
    cfg: Tier0Config,
    mem_fault_signal: i32,
}

impl AeroBackend {
    pub fn new(mem_fault_signal: i32) -> Self {
        Self {
            cfg: Tier0Config::default(),
            mem_fault_signal,
        }
    }

    pub fn execute(&mut self, case: &TestCase) -> ExecOutcome {
        #[cfg(feature = "qemu-reference")]
        if matches!(
            case.template.kind,
            crate::corpus::TemplateKind::RealModeFarJump
        ) {
            return self.execute_real_mode(case);
        }

        let mut bus = ConformanceBus {
            base: case.mem_base,
            mem: case.memory.clone(),
        };

        let mut cpu = CpuCore::new(CpuMode::Long);
        import_state(&case.init, &mut cpu.state);

        let step = aero_cpu_core::interp::tier0::exec::step_with_config(
            &self.cfg,
            &mut cpu.state,
            &mut bus,
        );

        let fault = match step {
            Ok(StepExit::Assist { .. }) => Some(Fault::Unsupported("tier-0 assist exit")),
            Ok(_) => None,
            Err(e) => Some(map_exception(e, self.mem_fault_signal)),
        };
        let state = export_state(&cpu.state);
        let memory = bus.mem;

        ExecOutcome {
            state,
            memory,
            fault,
        }
    }

    #[cfg(feature = "qemu-reference")]
    fn execute_real_mode(&mut self, case: &TestCase) -> ExecOutcome {
        // The QEMU reference harness executes a 16-bit snippet starting at 0x0700 and returns to
        // its caller via `ret`. Tier-0 stops at branch boundaries, so we step until the snippet
        // returns to a synthetic address.
        const RETURN_IP: u16 = 0x0000;
        const STACK_SP: u16 = 0x8FFE;

        let mut bus = ConformanceBus {
            base: case.mem_base,
            mem: case.memory.clone(),
        };

        // Seed a synthetic return address on the stack so the snippet's `ret` has somewhere to go.
        let ret_addr = (STACK_SP as u64)
            .checked_sub(case.mem_base)
            .and_then(|v| usize::try_from(v).ok());
        if let Some(ret_off) = ret_addr {
            if ret_off + 2 <= bus.mem.len() {
                bus.mem[ret_off..ret_off + 2].copy_from_slice(&RETURN_IP.to_le_bytes());
            }
        }

        let mut cpu = CpuCore::new(CpuMode::Real);
        import_state(&case.init, &mut cpu.state);

        // Real-mode flat segments.
        cpu.state.segments.cs = Default::default();
        cpu.state.segments.ds = Default::default();
        cpu.state.segments.es = Default::default();
        cpu.state.segments.ss = Default::default();
        cpu.state.segments.cs.limit = 0xFFFF;
        cpu.state.segments.ds.limit = 0xFFFF;
        cpu.state.segments.es.limit = 0xFFFF;
        cpu.state.segments.ss.limit = 0xFFFF;

        // Stack pointer for the `ret` at the end of the snippet.
        cpu.state.write_gpr64(gpr::RSP, STACK_SP as u64);

        let mut fault = None;
        for _ in 0..16 {
            if cpu.state.rip() as u16 == RETURN_IP {
                break;
            }

            let step = aero_cpu_core::interp::tier0::exec::step_with_config(
                &self.cfg,
                &mut cpu.state,
                &mut bus,
            );
            match step {
                Ok(StepExit::Assist { .. }) => {
                    fault = Some(Fault::Unsupported("tier-0 assist exit"));
                    break;
                }
                Ok(_) => {}
                Err(e) => {
                    fault = Some(map_exception(e, self.mem_fault_signal));
                    break;
                }
            }
        }

        if fault.is_none() && cpu.state.rip() as u16 != RETURN_IP {
            fault = Some(Fault::Unsupported("tier-0 real-mode snippet did not return"));
        }

        let mut state = export_state(&cpu.state);
        // The QEMU harness does not report IP/CS directly; match the host backend convention so we
        // have a stable comparison value.
        state.rip = case.init.rip.wrapping_add(case.template.bytes.len() as u64);

        let mem_hash = fnv1a_hash_256(&bus.mem);

        ExecOutcome {
            state,
            memory: mem_hash.to_le_bytes().to_vec(),
            fault,
        }
    }
}

fn map_exception(exception: Exception, mem_fault_signal: i32) -> Fault {
    match exception {
        Exception::InvalidOpcode => Fault::Signal(libc::SIGILL),
        Exception::MemoryFault => Fault::Signal(mem_fault_signal),
        Exception::DivideError => Fault::Signal(libc::SIGFPE),
        Exception::Unimplemented(name) => Fault::Unsupported(name),
        Exception::GeneralProtection(_) => Fault::Unsupported("#GP"),
        Exception::PageFault { .. } => Fault::Unsupported("#PF"),
        Exception::SegmentNotPresent(_) => Fault::Unsupported("#NP"),
        Exception::StackSegment(_) => Fault::Unsupported("#SS"),
        Exception::InvalidTss(_) => Fault::Unsupported("#TS"),
        Exception::DeviceNotAvailable => Fault::Unsupported("#NM"),
        Exception::X87Fpu => Fault::Unsupported("#MF"),
        Exception::SimdFloatingPointException => Fault::Unsupported("#XM"),
    }
}

fn import_state(input: &CpuState, core: &mut CoreState) {
    core.write_gpr64(gpr::RAX, input.rax);
    core.write_gpr64(gpr::RBX, input.rbx);
    core.write_gpr64(gpr::RCX, input.rcx);
    core.write_gpr64(gpr::RDX, input.rdx);
    core.write_gpr64(gpr::RSI, input.rsi);
    core.write_gpr64(gpr::RDI, input.rdi);
    core.write_gpr64(gpr::R8, input.r8);
    core.write_gpr64(gpr::R9, input.r9);
    core.write_gpr64(gpr::R10, input.r10);
    core.write_gpr64(gpr::R11, input.r11);
    core.write_gpr64(gpr::R12, input.r12);
    core.write_gpr64(gpr::R13, input.r13);
    core.write_gpr64(gpr::R14, input.r14);
    core.write_gpr64(gpr::R15, input.r15);
    core.set_rflags(input.rflags);
    core.set_rip(input.rip);
}

fn export_state(core: &CoreState) -> CpuState {
    CpuState {
        rax: core.read_gpr64(gpr::RAX),
        rbx: core.read_gpr64(gpr::RBX),
        rcx: core.read_gpr64(gpr::RCX),
        rdx: core.read_gpr64(gpr::RDX),
        rsi: core.read_gpr64(gpr::RSI),
        rdi: core.read_gpr64(gpr::RDI),
        r8: core.read_gpr64(gpr::R8),
        r9: core.read_gpr64(gpr::R9),
        r10: core.read_gpr64(gpr::R10),
        r11: core.read_gpr64(gpr::R11),
        r12: core.read_gpr64(gpr::R12),
        r13: core.read_gpr64(gpr::R13),
        r14: core.read_gpr64(gpr::R14),
        r15: core.read_gpr64(gpr::R15),
        rflags: core.rflags(),
        rip: core.rip(),
    }
}

#[cfg(feature = "qemu-reference")]
fn fnv1a_hash_256(bytes: &[u8]) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for idx in 0..256usize {
        let b = bytes.get(idx).copied().unwrap_or(0);
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

#[derive(Debug)]
struct ConformanceBus {
    base: u64,
    mem: Vec<u8>,
}

impl ConformanceBus {
    fn range(&self, vaddr: u64, len: usize) -> Result<Range<usize>, Exception> {
        let start = vaddr.checked_sub(self.base).ok_or(Exception::MemoryFault)?;
        let start = usize::try_from(start).map_err(|_| Exception::MemoryFault)?;
        let end = start.checked_add(len).ok_or(Exception::MemoryFault)?;
        if end > self.mem.len() {
            return Err(Exception::MemoryFault);
        }
        Ok(start..end)
    }
}

impl CpuBus for ConformanceBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        let idx = self.range(vaddr, 1)?.start;
        Ok(self.mem[idx])
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        let range = self.range(vaddr, 2)?;
        let bytes: [u8; 2] = self.mem[range].try_into().expect("range length checked");
        Ok(u16::from_le_bytes(bytes))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let range = self.range(vaddr, 4)?;
        let bytes: [u8; 4] = self.mem[range].try_into().expect("range length checked");
        Ok(u32::from_le_bytes(bytes))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let range = self.range(vaddr, 8)?;
        let bytes: [u8; 8] = self.mem[range].try_into().expect("range length checked");
        Ok(u64::from_le_bytes(bytes))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let range = self.range(vaddr, 16)?;
        let bytes: [u8; 16] = self.mem[range].try_into().expect("range length checked");
        Ok(u128::from_le_bytes(bytes))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        let idx = self.range(vaddr, 1)?.start;
        self.mem[idx] = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        let range = self.range(vaddr, 2)?;
        self.mem[range].copy_from_slice(&val.to_le_bytes());
        Ok(())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        let range = self.range(vaddr, 4)?;
        self.mem[range].copy_from_slice(&val.to_le_bytes());
        Ok(())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        let range = self.range(vaddr, 8)?;
        self.mem[range].copy_from_slice(&val.to_le_bytes());
        Ok(())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        let range = self.range(vaddr, 16)?;
        self.mem[range].copy_from_slice(&val.to_le_bytes());
        Ok(())
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.range(vaddr, len).map(|_| ())
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        if len == 0 {
            return Ok(buf);
        }
        let range = self.range(vaddr, len)?;
        buf[..len].copy_from_slice(&self.mem[range]);
        Ok(buf)
    }

    fn io_read(&mut self, _port: u16, _size: u32) -> Result<u64, Exception> {
        Err(Exception::Unimplemented("io"))
    }

    fn io_write(&mut self, _port: u16, _size: u32, _val: u64) -> Result<(), Exception> {
        Err(Exception::Unimplemented("io"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conformance_bus_rejects_port_io() {
        let mut bus = ConformanceBus {
            base: 0x1000,
            mem: vec![0u8; 64],
        };

        assert_eq!(
            bus.io_read(0x3f8, 1).unwrap_err(),
            Exception::Unimplemented("io")
        );
        assert_eq!(
            bus.io_write(0x3f8, 1, 0).unwrap_err(),
            Exception::Unimplemented("io")
        );
    }

    #[test]
    fn conformance_bus_oob_is_memoryfault() {
        let mut bus = ConformanceBus {
            base: 0x1000,
            mem: vec![0u8; 16],
        };

        assert_eq!(bus.read_u8(0x0fff).unwrap_err(), Exception::MemoryFault);
        assert_eq!(bus.read_u8(0x1000 + 16).unwrap_err(), Exception::MemoryFault);
        assert_eq!(bus.fetch(0x1000 + 8, 15).unwrap_err(), Exception::MemoryFault);
    }
}
