use crate::corpus::TestCase;
use crate::{CpuState, ExecOutcome, Fault, FLAG_FIXED_1};

const QEMU_MEM_BASE: u64 = 0x0500;
const QEMU_MEM_LEN: usize = 256;

pub struct QemuReferenceBackend;

impl QemuReferenceBackend {
    pub fn available() -> bool {
        qemu_diff::qemu_available()
    }

    pub fn new() -> Result<Self, &'static str> {
        if !Self::available() {
            return Err("qemu-system-* not found");
        }
        Ok(Self)
    }

    pub fn memory_base(&self) -> u64 {
        QEMU_MEM_BASE
    }

    pub fn execute(&mut self, case: &TestCase) -> ExecOutcome {
        let mut mem_init = [0u8; QEMU_MEM_LEN];
        let copy_len = QEMU_MEM_LEN.min(case.memory.len());
        mem_init[..copy_len].copy_from_slice(&case.memory[..copy_len]);

        let qemu_case = qemu_diff::TestCase {
            ax: case.init.rax as u16,
            bx: case.init.rbx as u16,
            cx: case.init.rcx as u16,
            dx: case.init.rdx as u16,
            si: case.init.rsi as u16,
            di: case.init.rdi as u16,
            // Conformance state currently doesn't model BP/SP; pick fixed values.
            // Keep SP away from the 0x0500..0x05FF scratch region hashed by the harness to avoid
            // implicit stack writes (CALL/RET) affecting the memory hash.
            bp: 0,
            sp: 0x9000,
            flags: case.init.rflags as u16,
            ds: 0,
            es: 0,
            ss: 0,
            mem_init,
            code: case.template.bytes.to_vec(),
        };

        let outcome = match qemu_diff::run(&qemu_case) {
            Ok(outcome) => outcome,
            Err(_) => {
                return ExecOutcome {
                    state: CpuState::default(),
                    memory: Vec::new(),
                    fault: Some(Fault::Unsupported("qemu reference execution failed")),
                };
            }
        };

        let state = CpuState {
            rax: outcome.ax as u64,
            rbx: outcome.bx as u64,
            rcx: outcome.cx as u64,
            rdx: outcome.dx as u64,
            rsi: outcome.si as u64,
            rdi: outcome.di as u64,
            rflags: (outcome.flags as u64) | FLAG_FIXED_1,
            // We cannot observe IP/CS from the harness; use the same "fallthrough" convention as
            // the host backend so the conformance harness has a stable comparison value.
            rip: case.init.rip.wrapping_add(case.template.bytes.len() as u64),
            ..Default::default()
        };

        ExecOutcome {
            state,
            memory: outcome.mem_hash.to_le_bytes().to_vec(),
            fault: None,
        }
    }
}
