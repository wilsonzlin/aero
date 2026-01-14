use std::ops::Range;

use aero_cpu_core::interp::tier0::exec::{self, StepExit};
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::state::{gpr, CpuMode, CpuState as CoreState};
use aero_cpu_core::{AssistReason, CpuBus, Exception};
#[cfg(feature = "qemu-reference")]
use aero_cpu_core::CpuCore;

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

        let mut bus = ConformanceBus::new(case.template.bytes, case.mem_base, case.memory.clone());

        let mut cpu = CoreState::new(CpuMode::Long);
        import_state(&case.init, &mut cpu);

        let fault = match exec::step_with_config(&self.cfg, &mut cpu, &mut bus) {
            Ok(exit) => match exit {
                StepExit::Continue | StepExit::ContinueInhibitInterrupts | StepExit::Branch => None,
                StepExit::Halted => Some(Fault::Unsupported("tier0 halted")),
                StepExit::BiosInterrupt(_) => Some(Fault::Unsupported("tier0 bios interrupt")),
                StepExit::Assist { reason, .. } => Some(Fault::Unsupported(match reason {
                    AssistReason::Io => "tier0 assist: io",
                    AssistReason::Privileged => "tier0 assist: privileged",
                    AssistReason::Interrupt => "tier0 assist: interrupt",
                    AssistReason::Cpuid => "tier0 assist: cpuid",
                    AssistReason::Msr => "tier0 assist: msr",
                    AssistReason::Unsupported => "tier0 assist: unsupported",
                })),
            },
            Err(e) => Some(map_tier0_exception(e, self.mem_fault_signal)),
        };

        ExecOutcome {
            state: export_state(&cpu),
            memory: bus.mem,
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

        let mut bus = ConformanceBus::new(case.template.bytes, case.mem_base, case.memory.clone());

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
                    fault = Some(map_tier0_exception(e, self.mem_fault_signal));
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

fn map_tier0_exception(exception: Exception, mem_fault_signal: i32) -> Fault {
    match exception {
        Exception::InvalidOpcode => Fault::Signal(libc::SIGILL),
        Exception::MemoryFault => Fault::Signal(mem_fault_signal),
        Exception::DivideError => Fault::Signal(libc::SIGFPE),
        // Leave the name intact to make mismatches readable (and because it is already stable).
        Exception::Unimplemented(name) => Fault::Unsupported(name),
        Exception::GeneralProtection(_) => Fault::Unsupported("tier0 exception: #GP"),
        Exception::PageFault { .. } => Fault::Unsupported("tier0 exception: #PF"),
        Exception::SegmentNotPresent(_) => Fault::Unsupported("tier0 exception: #NP"),
        Exception::StackSegment(_) => Fault::Unsupported("tier0 exception: #SS"),
        Exception::InvalidTss(_) => Fault::Unsupported("tier0 exception: #TS"),
        Exception::DeviceNotAvailable => Fault::Unsupported("tier0 exception: #NM"),
        Exception::X87Fpu => Fault::Unsupported("tier0 exception: #MF"),
        Exception::SimdFloatingPointException => Fault::Unsupported("tier0 exception: #XM"),
    }
}

fn import_state(input: &CpuState, core: &mut CoreState) {
    core.gpr[gpr::RAX] = input.rax;
    core.gpr[gpr::RBX] = input.rbx;
    core.gpr[gpr::RCX] = input.rcx;
    core.gpr[gpr::RDX] = input.rdx;
    core.gpr[gpr::RSI] = input.rsi;
    core.gpr[gpr::RDI] = input.rdi;
    core.gpr[gpr::R8] = input.r8;
    core.gpr[gpr::R9] = input.r9;
    core.gpr[gpr::R10] = input.r10;
    core.gpr[gpr::R11] = input.r11;
    core.gpr[gpr::R12] = input.r12;
    core.gpr[gpr::R13] = input.r13;
    core.gpr[gpr::R14] = input.r14;
    core.gpr[gpr::R15] = input.r15;
    // Use `set_rflags` to preserve reserved-bit semantics and clear any lazy flags state.
    core.set_rflags(input.rflags);
    core.set_rip(input.rip);
}

fn export_state(core: &CoreState) -> CpuState {
    CpuState {
        rax: core.gpr[gpr::RAX],
        rbx: core.gpr[gpr::RBX],
        rcx: core.gpr[gpr::RCX],
        rdx: core.gpr[gpr::RDX],
        rsi: core.gpr[gpr::RSI],
        rdi: core.gpr[gpr::RDI],
        r8: core.gpr[gpr::R8],
        r9: core.gpr[gpr::R9],
        r10: core.gpr[gpr::R10],
        r11: core.gpr[gpr::R11],
        r12: core.gpr[gpr::R12],
        r13: core.gpr[gpr::R13],
        r14: core.gpr[gpr::R14],
        r15: core.gpr[gpr::R15],
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
    /// Tier-0 instruction fetch buffer, mapped at vaddr 0.
    code: Vec<u8>,
    /// Base virtual address for the `mem` slice; `mem[0]` corresponds to `base`.
    base: u64,
    /// Backing data memory image.
    mem: Vec<u8>,
}

impl ConformanceBus {
    fn new(code: &[u8], base: u64, mem: Vec<u8>) -> Self {
        // Tier-0 may fetch up to 15 bytes at RIP for decoding, even when the instruction itself
        // is shorter. Pad out-of-range bytes with zero.
        let mut padded = vec![0u8; code.len().max(15)];
        padded[..code.len()].copy_from_slice(code);
        Self {
            code: padded,
            base,
            mem,
        }
    }

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
        let mut buf = [0u8; 2];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u16::from_le_bytes(buf))
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        let mut buf = [0u8; 4];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u32::from_le_bytes(buf))
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        let mut buf = [0u8; 8];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u64::from_le_bytes(buf))
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        let mut buf = [0u8; 16];
        self.read_bytes(vaddr, &mut buf)?;
        Ok(u128::from_le_bytes(buf))
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        let idx = self.range(vaddr, 1)?.start;
        self.mem[idx] = val;
        Ok(())
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.write_bytes(vaddr, &val.to_le_bytes())
    }

    fn read_bytes(&mut self, vaddr: u64, dst: &mut [u8]) -> Result<(), Exception> {
        let range = self.range(vaddr, dst.len())?;
        dst.copy_from_slice(&self.mem[range]);
        Ok(())
    }

    fn write_bytes(&mut self, vaddr: u64, src: &[u8]) -> Result<(), Exception> {
        let range = self.range(vaddr, src.len())?;
        self.mem[range].copy_from_slice(src);
        Ok(())
    }

    fn preflight_write_bytes(&mut self, vaddr: u64, len: usize) -> Result<(), Exception> {
        self.range(vaddr, len).map(|_| ())
    }

    fn supports_bulk_copy(&self) -> bool {
        true
    }

    fn bulk_copy(&mut self, dst: u64, src: u64, len: usize) -> Result<bool, Exception> {
        if len == 0 || dst == src {
            return Ok(true);
        }

        let src_range = self.range(src, len)?;
        let dst_range = self.range(dst, len)?;

        if src_range.start == dst_range.start {
            return Ok(true);
        }
        self.mem.copy_within(src_range, dst_range.start);
        Ok(true)
    }

    fn supports_bulk_set(&self) -> bool {
        true
    }

    fn bulk_set(&mut self, dst: u64, pattern: &[u8], repeat: usize) -> Result<bool, Exception> {
        if repeat == 0 || pattern.is_empty() {
            return Ok(true);
        }

        let total = pattern
            .len()
            .checked_mul(repeat)
            .ok_or(Exception::MemoryFault)?;
        let range = self.range(dst, total)?;
        let dst_slice = &mut self.mem[range];

        if pattern.len() == 1 {
            dst_slice.fill(pattern[0]);
            return Ok(true);
        }

        for chunk in dst_slice.chunks_exact_mut(pattern.len()) {
            chunk.copy_from_slice(pattern);
        }
        Ok(true)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        let mut buf = [0u8; 15];
        let len = max_len.min(15);
        if len == 0 {
            return Ok(buf);
        }

        // Conformance executes with RIP=0 and maps the template bytes at vaddr 0.
        // Allow the qemu-reference path to fetch from the backing memory (real-mode snippet at
        // 0x0700) by falling back to data memory when vaddr is outside the code buffer.
        if let Ok(start) = usize::try_from(vaddr) {
            if start.checked_add(len).is_some_and(|end| end <= self.code.len()) {
                buf[..len].copy_from_slice(&self.code[start..start + len]);
                return Ok(buf);
            }
        }

        self.read_bytes(vaddr, &mut buf[..len])?;
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
    use crate::corpus::{self, TemplateKind};
    use crate::FLAG_FIXED_1;

    #[test]
    fn conformance_bus_rejects_port_io() {
        let mut bus = ConformanceBus::new(&[], 0x1000, vec![0u8; 64]);

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
        let mut bus = ConformanceBus::new(&[], 0x1000, vec![0u8; 16]);

        assert_eq!(bus.read_u8(0x0fff).unwrap_err(), Exception::MemoryFault);
        assert_eq!(bus.read_u8(0x1000 + 16).unwrap_err(), Exception::MemoryFault);
        assert_eq!(bus.fetch(0x1000 + 8, 15).unwrap_err(), Exception::MemoryFault);
    }

    #[test]
    fn tier0_backend_executes_and_maps_faults() {
        let templates = corpus::templates();
        let add = templates
            .iter()
            .find(|t| matches!(t.kind, TemplateKind::AddRaxRbx))
            .expect("add template missing");
        let ud2 = templates
            .iter()
            .find(|t| matches!(t.kind, TemplateKind::Ud2))
            .expect("ud2 template missing");
        let mem_fault = templates
            .iter()
            .find(|t| matches!(t.kind, TemplateKind::MovRaxM64Abs0))
            .expect("mem fault template missing");

        let mem_fault_signal = libc::SIGSEGV;
        let mut backend = AeroBackend::new(mem_fault_signal);
        let mem_base = 0x1000u64;
        let mut rng = corpus::XorShift64::new(0x_0bad_f00d_f00d_f00d);

        let mut add_case = TestCase::generate(0, add, &mut rng, mem_base);
        add_case.init.rax = 1;
        add_case.init.rbx = 2;
        add_case.init.rflags = FLAG_FIXED_1;
        let expected_rip = add_case.init.rip.wrapping_add(add.bytes.len() as u64);

        let add_out = backend.execute(&add_case);
        assert_eq!(add_out.fault, None);
        assert_eq!(add_out.state.rax, 3);
        assert_eq!(add_out.state.rip, expected_rip);

        let mut ud2_case = TestCase::generate(1, ud2, &mut rng, mem_base);
        ud2_case.init.rflags = FLAG_FIXED_1;
        let ud2_out = backend.execute(&ud2_case);
        assert_eq!(ud2_out.fault, Some(Fault::Signal(libc::SIGILL)));

        let mut mem_fault_case = TestCase::generate(2, mem_fault, &mut rng, mem_base);
        mem_fault_case.init.rflags = FLAG_FIXED_1;
        let mem_fault_out = backend.execute(&mem_fault_case);
        assert_eq!(
            mem_fault_out.fault,
            Some(Fault::Signal(mem_fault_signal))
        );
    }
}
