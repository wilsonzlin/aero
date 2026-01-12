use aero_devices::i8042::{
    I8042Ports, PlatformSystemControlSink, I8042_DATA_PORT, I8042_STATUS_PORT,
};
use aero_devices::reset_ctrl::{ResetCtrl, ResetKind, RESET_CTRL_PORT, RESET_CTRL_RESET_VALUE};
use aero_platform::io::IoPortBus;
use aero_platform::reset::ResetLatch;
use aero_platform::ChipsetState;

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
    reset_latch: ResetLatch,
    chipset: ChipsetState,
}

impl TestVm {
    fn new(boot_rom: &[u8]) -> Self {
        let mut mem_initial = vec![0u8; 1024 * 1024];
        mem_initial[RESET_VECTOR_PHYS as usize..RESET_VECTOR_PHYS as usize + boot_rom.len()]
            .copy_from_slice(boot_rom);

        let reset_latch = ResetLatch::new();
        let reset_sink = reset_latch.clone();

        let chipset = ChipsetState::new(false);

        let mut io = IoPortBus::new();
        io.register(RESET_CTRL_PORT, Box::new(ResetCtrl::new(reset_sink)));

        let i8042 = I8042Ports::new();
        let controller = i8042.controller();
        controller.borrow_mut().set_system_control_sink(Box::new(
            PlatformSystemControlSink::with_reset_sink(chipset.a20(), reset_latch.clone()),
        ));
        io.register(I8042_DATA_PORT, Box::new(i8042.port60()));
        io.register(I8042_STATUS_PORT, Box::new(i8042.port64()));

        Self {
            cpu: TestCpu::new(),
            mem: mem_initial.clone(),
            mem_initial,
            io,
            reset_latch,
            chipset,
        }
    }

    fn reset_system(&mut self) {
        self.cpu.reset();
        self.chipset.a20().set_enabled(false);
        self.io.reset();
        self.mem.clone_from_slice(&self.mem_initial);
    }

    fn step(&mut self) -> StepResult {
        if self.cpu.halted {
            return StepResult::Halted;
        }

        self.cpu.step(&self.mem, &mut self.io);

        let pending_reset = self.reset_latch.take();
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

    // Dirty some device state so we can verify the platform reset clears it.
    // Change the i8042 command byte away from its power-on value (0x45).
    vm.io.write_u8(I8042_STATUS_PORT, 0x60);
    vm.io.write_u8(I8042_DATA_PORT, 0x00);
    vm.io.write_u8(I8042_STATUS_PORT, 0x20);
    assert_eq!(vm.io.read_u8(I8042_DATA_PORT), 0x00);

    assert_eq!(vm.cpu.physical_ip(), RESET_VECTOR_PHYS);

    // Step until we observe the reset being requested.
    let mut saw_reset = false;
    for _ in 0..16 {
        match vm.step() {
            StepResult::Reset(kind) => {
                assert_eq!(kind, ResetKind::System);
                assert_eq!(vm.cpu.physical_ip(), RESET_VECTOR_PHYS);

                // Platform reset should have restored device power-on state.
                vm.io.write_u8(I8042_STATUS_PORT, 0x20);
                assert_eq!(vm.io.read_u8(I8042_DATA_PORT), 0x45);
                assert_eq!(vm.io.read_u8(RESET_CTRL_PORT), 0x00);
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

#[test]
fn guest_i8042_reset_pulse_restarts_at_reset_vector_and_resets_devices() {
    // Real-mode sequence:
    //   mov al, 0xFE
    //   mov dx, 0x0064
    //   out dx, al
    //   jmp $
    let payload: [u8; 8] = [
        0xB0,
        0xFE,
        0xBA,
        (I8042_STATUS_PORT & 0xFF) as u8,
        (I8042_STATUS_PORT >> 8) as u8,
        0xEE,
        0xEB,
        0xFE,
    ];

    let mut vm = TestVm::new(&payload);

    // Dirty device state before issuing the reset.
    vm.io.write_u8(RESET_CTRL_PORT, 0x04);
    vm.io.write_u8(I8042_STATUS_PORT, 0x60);
    vm.io.write_u8(I8042_DATA_PORT, 0x00);

    vm.io.write_u8(I8042_STATUS_PORT, 0x20);
    assert_eq!(vm.io.read_u8(I8042_DATA_PORT), 0x00);
    assert_eq!(vm.io.read_u8(RESET_CTRL_PORT), 0x04);

    let mut saw_reset = false;
    for _ in 0..16 {
        match vm.step() {
            StepResult::Reset(kind) => {
                assert_eq!(kind, ResetKind::System);
                assert_eq!(vm.cpu.physical_ip(), RESET_VECTOR_PHYS);

                // Device state should be restored to power-on defaults.
                vm.io.write_u8(I8042_STATUS_PORT, 0x20);
                assert_eq!(vm.io.read_u8(I8042_DATA_PORT), 0x45);
                assert_eq!(vm.io.read_u8(RESET_CTRL_PORT), 0x00);

                saw_reset = true;
                break;
            }
            StepResult::Halted => panic!("guest halted before triggering i8042 reset"),
            StepResult::Continue => {}
        }
    }

    assert!(saw_reset, "guest did not trigger a reset via i8042");
}
