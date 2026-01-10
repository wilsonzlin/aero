use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::reset_ctrl::{ResetCtrl, ResetKind, RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE};
use aero_platform::io::IoPortBus;

const RESET_CS: u16 = 0xF000;
const RESET_IP: u16 = 0xFFF0;
const RESET_VECTOR_PHYS: u32 = 0xFFFF0;

#[derive(Debug, Clone)]
struct TestCpu {
    cs: u16,
    ip: u16,
    al: u8,
    dx: u16,
    halted: bool,
}

impl TestCpu {
    fn new() -> Self {
        let mut cpu = Self {
            cs: 0,
            ip: 0,
            al: 0,
            dx: 0,
            halted: false,
        };
        cpu.reset();
        cpu
    }

    fn reset(&mut self) {
        self.cs = RESET_CS;
        self.ip = RESET_IP;
        self.al = 0;
        self.dx = 0;
        self.halted = false;
    }

    fn physical_ip(&self) -> u32 {
        ((self.cs as u32) << 4).wrapping_add(self.ip as u32)
    }

    fn fetch_u8(&mut self, mem: &[u8]) -> u8 {
        let addr = self.physical_ip() as usize;
        let byte = mem[addr];
        self.ip = self.ip.wrapping_add(1);
        byte
    }

    fn fetch_u16(&mut self, mem: &[u8]) -> u16 {
        let lo = self.fetch_u8(mem) as u16;
        let hi = self.fetch_u8(mem) as u16;
        lo | (hi << 8)
    }

    fn step(&mut self, mem: &[u8], io: &mut IoPortBus) {
        if self.halted {
            return;
        }

        let opcode = self.fetch_u8(mem);
        match opcode {
            0x90 => {} // NOP
            0xB0 => {
                // MOV AL, imm8
                self.al = self.fetch_u8(mem);
            }
            0xBA => {
                // MOV DX, imm16
                self.dx = self.fetch_u16(mem);
            }
            0xEE => {
                // OUT DX, AL
                io.write(self.dx, 1, self.al as u32);
            }
            0xEB => {
                // JMP rel8
                let rel = self.fetch_u8(mem) as i8;
                self.ip = self.ip.wrapping_add(rel as i16 as u16);
            }
            0xF4 => {
                // HLT
                self.halted = true;
            }
            _ => {
                // Keep the test VM tiny: treat unknown opcodes as a hard halt.
                self.halted = true;
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepResult {
    Continue,
    Halted,
    Reset(ResetKind),
}

struct TestVm {
    cpu: TestCpu,
    mem: Vec<u8>,
    mem_initial: Vec<u8>,
    io: IoPortBus,
    reset_request: Rc<RefCell<Option<ResetKind>>>,
}

impl TestVm {
    fn new(boot_rom: &[u8]) -> Self {
        let mut mem_initial = vec![0u8; 1024 * 1024];
        mem_initial[RESET_VECTOR_PHYS as usize..RESET_VECTOR_PHYS as usize + boot_rom.len()]
            .copy_from_slice(boot_rom);

        let reset_request = Rc::new(RefCell::new(None));
        let reset_request_handle = reset_request.clone();

        let mut io = IoPortBus::new();
        io.register(
            RESET_CTRL_PORT,
            Box::new(ResetCtrl::new(move |kind| {
                *reset_request_handle.borrow_mut() = Some(kind);
            })),
        );

        Self {
            cpu: TestCpu::new(),
            mem: mem_initial.clone(),
            mem_initial,
            io,
            reset_request,
        }
    }

    fn reset_system(&mut self) {
        self.cpu.reset();
        self.io.reset();
        self.mem.clone_from_slice(&self.mem_initial);
    }

    fn step(&mut self) -> StepResult {
        if self.cpu.halted {
            return StepResult::Halted;
        }

        self.cpu.step(&self.mem, &mut self.io);

        let pending_reset = self.reset_request.borrow_mut().take();
        if let Some(kind) = pending_reset {
            match kind {
                ResetKind::Cpu => self.cpu.reset(),
                ResetKind::System => self.reset_system(),
            }
            return StepResult::Reset(kind);
        }

        StepResult::Continue
    }
}

#[test]
fn guest_out_cf9_restarts_at_reset_vector() {
    // Real-mode sequence:
    //   mov al, 0x06
    //   mov dx, 0x0CF9
    //   out dx, al
    //   jmp $
    let payload: [u8; 8] = [
        0xB0,
        RESET_CTRL_RESET_VALUE,
        0xBA,
        (RESET_CTRL_PORT & 0xFF) as u8,
        (RESET_CTRL_PORT >> 8) as u8,
        0xEE,
        0xEB,
        0xFE,
    ];

    let mut vm = TestVm::new(&payload);

    assert_eq!(vm.cpu.physical_ip(), RESET_VECTOR_PHYS);

    // Step until we observe the reset being requested.
    let mut saw_reset = false;
    for _ in 0..16 {
        match vm.step() {
            StepResult::Reset(kind) => {
                assert_eq!(kind, ResetKind::System);
                assert_eq!(vm.cpu.physical_ip(), RESET_VECTOR_PHYS);
                saw_reset = true;
                break;
            }
            StepResult::Halted => panic!("guest halted before triggering reset"),
            StepResult::Continue => {}
        }
    }

    assert!(saw_reset, "guest did not trigger a reset via 0xCF9");

    // After reset, execution restarts at the reset vector. One more step should
    // execute the first instruction (MOV AL, imm8).
    assert_eq!(vm.cpu.al, 0);
    vm.step();
    assert_eq!(vm.cpu.al, RESET_CTRL_RESET_VALUE);
}
