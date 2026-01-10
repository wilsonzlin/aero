use std::cell::RefCell;
use std::rc::Rc;

use aero_cpu_core::cpu::{InterruptLine, RFlags};
use aero_cpu_core::interp::ExecError;
use aero_cpu_core::{Bus, Cpu, CpuMode, Exception, RamBus};

fn setup_bus() -> RamBus {
    RamBus::new(0x10_000)
}

#[test]
fn lock_cmpxchg_rmw_sizes_success_and_failure() {
    // r/m8, r8
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x200;
        cpu.regs.rsi = addr;
        cpu.regs.set_al(0x11);
        cpu.regs.rcx = 0x22;
        bus.write_u8(addr, 0x11);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xB0, 0x0E])
            .unwrap(); // LOCK CMPXCHG byte ptr [rsi], cl

        assert_eq!(bus.read_u8(addr), 0x22);
        assert_eq!(cpu.regs.al(), 0x11);
        assert!(cpu.rflags.zf());
        assert!(!cpu.rflags.get(RFlags::CF));
        assert!(!cpu.rflags.get(RFlags::OF));
    }

    // r/m8, r8 failure (exercise OF for subtraction).
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x210;
        cpu.regs.rsi = addr;
        cpu.regs.set_al(0x01);
        cpu.regs.rcx = 0x33;
        bus.write_u8(addr, 0x80);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xB0, 0x0E])
            .unwrap();

        assert_eq!(bus.read_u8(addr), 0x80);
        assert_eq!(cpu.regs.al(), 0x80);
        assert!(!cpu.rflags.zf());
        assert!(!cpu.rflags.get(RFlags::CF));
        assert!(cpu.rflags.get(RFlags::OF));
    }

    // r/m16, r16
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x220;
        cpu.regs.rsi = addr;
        cpu.regs.set_ax(0x1234);
        cpu.regs.rcx = 0xBEEF;
        bus.write_u16(addr, 0x1234);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x66, 0x0F, 0xB1, 0x0E])
            .unwrap(); // LOCK CMPXCHG word ptr [rsi], cx

        assert_eq!(bus.read_u16(addr), 0xBEEF);
        assert_eq!(cpu.regs.ax(), 0x1234);
        assert!(cpu.rflags.zf());
        assert!(!cpu.rflags.get(RFlags::CF));
    }

    // r/m16, r16 failure (exercise CF for subtraction borrow).
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x230;
        cpu.regs.rsi = addr;
        cpu.regs.set_ax(0x0003);
        cpu.regs.rcx = 0x2222;
        bus.write_u16(addr, 0x0001);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x66, 0x0F, 0xB1, 0x0E])
            .unwrap();

        assert_eq!(bus.read_u16(addr), 0x0001);
        assert_eq!(cpu.regs.ax(), 0x0001);
        assert!(!cpu.rflags.zf());
        assert!(cpu.rflags.get(RFlags::CF));
        assert!(!cpu.rflags.get(RFlags::OF));
    }

    // r/m32, r32
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x240;
        cpu.regs.rsi = addr;
        cpu.regs.set_eax(0x1111_1111, cpu.mode);
        cpu.regs.set_ecx(0x2222_2222, cpu.mode);
        bus.write_u32(addr, 0x1111_1111);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xB1, 0x0E])
            .unwrap(); // LOCK CMPXCHG dword ptr [rsi], ecx

        assert_eq!(bus.read_u32(addr), 0x2222_2222);
        assert_eq!(cpu.regs.eax(), 0x1111_1111);
        assert!(cpu.rflags.zf());
    }

    // r/m32, r32 failure.
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x250;
        cpu.regs.rsi = addr;
        cpu.regs.set_eax(2, cpu.mode);
        cpu.regs.set_ecx(0x3333_3333, cpu.mode);
        bus.write_u32(addr, 1);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xB1, 0x0E])
            .unwrap();

        assert_eq!(bus.read_u32(addr), 1);
        assert_eq!(cpu.regs.eax(), 1);
        assert!(!cpu.rflags.zf());
        assert!(cpu.rflags.get(RFlags::CF));
    }

    // r/m64, r64
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x260;
        cpu.regs.rsi = addr;
        cpu.regs.rax = 0x1111_1111_2222_2222;
        cpu.regs.rcx = 0x3333_3333_4444_4444;
        bus.write_u64(addr, cpu.regs.rax);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x48, 0x0F, 0xB1, 0x0E])
            .unwrap(); // LOCK CMPXCHG qword ptr [rsi], rcx

        assert_eq!(bus.read_u64(addr), 0x3333_3333_4444_4444);
        assert_eq!(cpu.regs.rax, 0x1111_1111_2222_2222);
        assert!(cpu.rflags.zf());
    }

    // r/m64, r64 failure.
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x270;
        cpu.regs.rsi = addr;
        cpu.regs.rax = 2;
        cpu.regs.rcx = 0xAAAA;
        bus.write_u64(addr, 1);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x48, 0x0F, 0xB1, 0x0E])
            .unwrap();

        assert_eq!(bus.read_u64(addr), 1);
        assert_eq!(cpu.regs.rax, 1);
        assert!(!cpu.rflags.zf());
        assert!(cpu.rflags.get(RFlags::CF));
    }
}

#[test]
fn lock_cmpxchg8b_success_and_failure() {
    let addr: u64 = 0x300;

    // Success.
    {
        let mut cpu = Cpu::new(CpuMode::Protected32);
        let mut bus = setup_bus();
        cpu.regs.set_esi(addr as u32, cpu.mode);

        let expected = 0x1122_3344_5566_7788u64;
        let replacement = 0xAABB_CCDD_EEFF_0011u64;
        bus.write_u64(addr, expected);

        cpu.regs.set_eax(expected as u32, cpu.mode);
        cpu.regs.set_edx((expected >> 32) as u32, cpu.mode);
        cpu.regs.set_ebx(replacement as u32, cpu.mode);
        cpu.regs.set_ecx((replacement >> 32) as u32, cpu.mode);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC7, 0x0E])
            .unwrap(); // LOCK CMPXCHG8B qword ptr [esi]

        assert_eq!(bus.read_u64(addr), replacement);
        assert!(cpu.rflags.zf());
        assert_eq!(cpu.regs.eax(), expected as u32);
        assert_eq!(cpu.regs.edx(), (expected >> 32) as u32);
    }

    // Failure.
    {
        let mut cpu = Cpu::new(CpuMode::Protected32);
        let mut bus = setup_bus();
        cpu.regs.set_esi(addr as u32, cpu.mode);

        let old = 0x0123_4567_89AB_CDEFu64;
        bus.write_u64(addr, old);

        cpu.regs.set_eax(0x1111_1111, cpu.mode);
        cpu.regs.set_edx(0x2222_2222, cpu.mode);
        cpu.regs.set_ebx(0x3333_3333, cpu.mode);
        cpu.regs.set_ecx(0x4444_4444, cpu.mode);

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC7, 0x0E])
            .unwrap();

        assert_eq!(bus.read_u64(addr), old);
        assert!(!cpu.rflags.zf());
        assert_eq!(cpu.regs.eax(), old as u32);
        assert_eq!(cpu.regs.edx(), (old >> 32) as u32);
    }
}

#[test]
fn lock_cmpxchg16b_success_failure_and_alignment() {
    // Success.
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x400;
        cpu.regs.rsi = addr;

        let expected_lo = 0x1122_3344_5566_7788u64;
        let expected_hi = 0x99AA_BBCC_DDEE_FF00u64;
        let expected = ((expected_hi as u128) << 64) | expected_lo as u128;

        let replacement_lo = 0xA0A1_A2A3_A4A5_A6A7u64;
        let replacement_hi = 0xB0B1_B2B3_B4B5_B6B7u64;
        let replacement = ((replacement_hi as u128) << 64) | replacement_lo as u128;

        bus.write_u128(addr, expected);
        cpu.regs.rax = expected_lo;
        cpu.regs.rdx = expected_hi;
        cpu.regs.rbx = replacement_lo;
        cpu.regs.rcx = replacement_hi;

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC7, 0x0E])
            .unwrap(); // LOCK CMPXCHG16B oword ptr [rsi]

        assert_eq!(bus.read_u128(addr), replacement);
        assert!(cpu.rflags.zf());
    }

    // Failure.
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x420;
        cpu.regs.rsi = addr;

        let old_lo = 0x1111_1111_2222_2222u64;
        let old_hi = 0x3333_3333_4444_4444u64;
        let old = ((old_hi as u128) << 64) | old_lo as u128;
        bus.write_u128(addr, old);

        cpu.regs.rax = 0x5555_5555_6666_6666;
        cpu.regs.rdx = 0x7777_7777_8888_8888;
        cpu.regs.rbx = 0x9999_9999_AAAA_AAAA;
        cpu.regs.rcx = 0xBBBB_BBBB_CCCC_CCCC;

        cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC7, 0x0E])
            .unwrap();

        assert_eq!(bus.read_u128(addr), old);
        assert!(!cpu.rflags.zf());
        assert_eq!(cpu.regs.rax, old_lo);
        assert_eq!(cpu.regs.rdx, old_hi);
    }

    // Alignment fault (#GP(0)).
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x401;
        cpu.regs.rsi = addr;

        let res = cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC7, 0x0E]);
        assert_eq!(res, Err(ExecError::Exception(Exception::gp0())));
    }
}

#[test]
fn lock_xadd_updates_memory_register_and_flags() {
    let mut cpu = Cpu::new(CpuMode::Long64);
    let mut bus = setup_bus();
    let addr = 0x500;
    cpu.regs.rsi = addr;

    bus.write_u32(addr, 0x8000_0000);
    cpu.regs.set_ecx(0x8000_0001, cpu.mode);

    cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC1, 0x0E])
        .unwrap(); // LOCK XADD dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(addr), 1);
    assert_eq!(cpu.regs.ecx(), 0x8000_0000);
    assert!(cpu.rflags.get(RFlags::CF));
    assert!(cpu.rflags.get(RFlags::OF));
    assert!(!cpu.rflags.zf());
    assert!(!cpu.rflags.get(RFlags::SF));
}

#[test]
fn lock_inc_and_dec_update_memory_and_preserve_cf() {
    // INC
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x600;
        cpu.regs.rsi = addr;
        cpu.rflags.set(RFlags::CF, true);

        bus.write_u32(addr, 0x7FFF_FFFF);
        cpu.execute_bytes(&mut bus, &[0xF0, 0xFF, 0x06]).unwrap(); // LOCK INC dword ptr [rsi]

        assert_eq!(bus.read_u32(addr), 0x8000_0000);
        assert!(cpu.rflags.get(RFlags::CF));
        assert!(cpu.rflags.get(RFlags::OF));
        assert!(cpu.rflags.get(RFlags::SF));
        assert!(!cpu.rflags.zf());
    }

    // DEC
    {
        let mut cpu = Cpu::new(CpuMode::Long64);
        let mut bus = setup_bus();
        let addr = 0x610;
        cpu.regs.rsi = addr;
        cpu.rflags.set(RFlags::CF, false);

        bus.write_u32(addr, 0x8000_0000);
        cpu.execute_bytes(&mut bus, &[0xF0, 0xFF, 0x0E]).unwrap(); // LOCK DEC dword ptr [rsi]

        assert_eq!(bus.read_u32(addr), 0x7FFF_FFFF);
        assert!(!cpu.rflags.get(RFlags::CF));
        assert!(cpu.rflags.get(RFlags::OF));
        assert!(!cpu.rflags.get(RFlags::SF));
        assert!(!cpu.rflags.zf());
    }
}

#[test]
fn lock_bit_test_ops_update_memory_and_cf() {
    let mut cpu = Cpu::new(CpuMode::Long64);
    let mut bus = setup_bus();
    let base = 0x700;
    cpu.regs.rsi = base;

    bus.write_u32(base, 0);
    bus.write_u32(base + 4, 0);
    cpu.regs.set_ecx(33, cpu.mode); // element 1, bit 1 (32-bit bitmap semantics)

    cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xAB, 0x0E])
        .unwrap(); // LOCK BTS dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(base), 0);
    assert_eq!(bus.read_u32(base + 4), 0x2);
    assert!(!cpu.rflags.get(RFlags::CF));

    cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xB3, 0x0E])
        .unwrap(); // LOCK BTR dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(base + 4), 0);
    assert!(cpu.rflags.get(RFlags::CF));

    cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xBB, 0x0E])
        .unwrap(); // LOCK BTC dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(base + 4), 0x2);
    assert!(!cpu.rflags.get(RFlags::CF));
}

#[test]
fn lock_prefix_on_register_operand_is_invalid_opcode() {
    let mut cpu = Cpu::new(CpuMode::Long64);
    let mut bus = setup_bus();
    let res = cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xB1, 0xC8]); // LOCK CMPXCHG eax, ecx
    assert!(matches!(res, Err(ExecError::InvalidOpcode(_))));
}

struct InterruptingBus {
    inner: RamBus,
    target: u64,
    line: InterruptLine,
    fired: bool,
    log: Rc<RefCell<Vec<&'static str>>>,
}

impl InterruptingBus {
    fn new(
        inner: RamBus,
        target: u64,
        line: InterruptLine,
        log: Rc<RefCell<Vec<&'static str>>>,
    ) -> Self {
        Self {
            inner,
            target,
            line,
            fired: false,
            log,
        }
    }
}

impl Bus for InterruptingBus {
    fn read_u8(&mut self, addr: u64) -> u8 {
        let v = self.inner.read_u8(addr);
        if !self.fired && addr == self.target {
            self.fired = true;
            self.line.raise();
            self.log.borrow_mut().push("interrupt_pending");
        }
        v
    }

    fn write_u8(&mut self, addr: u64, value: u8) {
        self.inner.write_u8(addr, value);
    }
}

#[test]
fn locked_rmw_defers_interrupt_delivery_until_after_atomic_update() {
    let mut cpu = Cpu::new(CpuMode::Long64);
    cpu.rflags.set(RFlags::IF, true);
    let log = Rc::new(RefCell::new(Vec::new()));
    cpu.set_event_log(log.clone());

    let addr = 0x800;
    cpu.regs.rsi = addr;
    cpu.regs.set_ecx(2, cpu.mode);

    let line = cpu.interrupt_line();
    let mut bus = InterruptingBus::new(setup_bus(), addr, line, log.clone());
    bus.write_u32(addr, 1);

    cpu.execute_bytes(&mut bus, &[0xF0, 0x0F, 0xC1, 0x0E])
        .unwrap(); // LOCK XADD dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(addr), 3);
    assert_eq!(cpu.interrupts_delivered(), 1);

    let log = log.borrow();
    let idx_pending = log
        .iter()
        .position(|&evt| evt == "interrupt_pending")
        .unwrap();
    let idx_atomic = log.iter().position(|&evt| evt == "atomic_rmw").unwrap();
    let idx_delivered = log
        .iter()
        .position(|&evt| evt == "interrupt_delivered")
        .unwrap();

    assert!(idx_pending < idx_atomic);
    assert!(idx_atomic < idx_delivered);
}
