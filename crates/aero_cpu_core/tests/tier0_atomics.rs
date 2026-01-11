use aero_cpu_core::interp::tier0::exec::step;
use aero_cpu_core::interp::tier0::exec::StepExit;
use aero_cpu_core::mem::{CpuBus, CpuBusValue, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState, FLAG_CF, FLAG_OF, FLAG_SF, FLAG_ZF};
use aero_cpu_core::Exception;
use aero_x86::Register;

const BUS_SIZE: usize = 0x10_000;
const CODE_BASE: u64 = 0x1000;

fn exec_steps(
    state: &mut CpuState,
    bus: &mut FlatTestBus,
    code: &[u8],
    steps: usize,
) -> Result<(), Exception> {
    bus.load(CODE_BASE, code);
    state.set_rip(CODE_BASE);
    for _ in 0..steps {
        let exit = step(state, bus)?;
        assert!(
            matches!(
                exit,
                StepExit::Continue | StepExit::ContinueInhibitInterrupts | StepExit::Branch
            ),
            "unexpected tier0 exit: {exit:?}"
        );
    }
    Ok(())
}

#[derive(Debug)]
struct CountingBus {
    inner: FlatTestBus,
    atomic_rmw_calls: u64,
}

impl CountingBus {
    fn new(size: usize) -> Self {
        Self {
            inner: FlatTestBus::new(size),
            atomic_rmw_calls: 0,
        }
    }

    fn load(&mut self, addr: u64, bytes: &[u8]) {
        self.inner.load(addr, bytes);
    }
}

impl CpuBus for CountingBus {
    fn read_u8(&mut self, vaddr: u64) -> Result<u8, Exception> {
        self.inner.read_u8(vaddr)
    }

    fn read_u16(&mut self, vaddr: u64) -> Result<u16, Exception> {
        self.inner.read_u16(vaddr)
    }

    fn read_u32(&mut self, vaddr: u64) -> Result<u32, Exception> {
        self.inner.read_u32(vaddr)
    }

    fn read_u64(&mut self, vaddr: u64) -> Result<u64, Exception> {
        self.inner.read_u64(vaddr)
    }

    fn read_u128(&mut self, vaddr: u64) -> Result<u128, Exception> {
        self.inner.read_u128(vaddr)
    }

    fn write_u8(&mut self, vaddr: u64, val: u8) -> Result<(), Exception> {
        self.inner.write_u8(vaddr, val)
    }

    fn write_u16(&mut self, vaddr: u64, val: u16) -> Result<(), Exception> {
        self.inner.write_u16(vaddr, val)
    }

    fn write_u32(&mut self, vaddr: u64, val: u32) -> Result<(), Exception> {
        self.inner.write_u32(vaddr, val)
    }

    fn write_u64(&mut self, vaddr: u64, val: u64) -> Result<(), Exception> {
        self.inner.write_u64(vaddr, val)
    }

    fn write_u128(&mut self, vaddr: u64, val: u128) -> Result<(), Exception> {
        self.inner.write_u128(vaddr, val)
    }

    fn atomic_rmw<T, R>(&mut self, vaddr: u64, f: impl FnOnce(T) -> (T, R)) -> Result<R, Exception>
    where
        T: CpuBusValue,
        Self: Sized,
    {
        self.atomic_rmw_calls += 1;
        CpuBus::atomic_rmw(&mut self.inner, vaddr, f)
    }

    fn fetch(&mut self, vaddr: u64, max_len: usize) -> Result<[u8; 15], Exception> {
        self.inner.fetch(vaddr, max_len)
    }

    fn io_read(&mut self, port: u16, size: u32) -> Result<u64, Exception> {
        self.inner.io_read(port, size)
    }

    fn io_write(&mut self, port: u16, size: u32, val: u64) -> Result<(), Exception> {
        self.inner.io_write(port, size, val)
    }
}

#[test]
fn lock_cmpxchg_rmw_sizes_success_and_failure() {
    // r/m8, r8 success.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x200;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AL, 0x11);
        state.write_reg(Register::CL, 0x22);
        bus.write_u8(addr, 0x11).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xB0, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u8(addr).unwrap(), 0x22);
        assert_eq!(state.read_reg(Register::AL), 0x11);
        assert!(state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
        assert!(!state.get_flag(FLAG_OF));
    }

    // r/m8, r8 failure (exercise OF for subtraction).
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x210;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AL, 0x01);
        state.write_reg(Register::CL, 0x33);
        bus.write_u8(addr, 0x80).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xB0, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u8(addr).unwrap(), 0x80);
        assert_eq!(state.read_reg(Register::AL), 0x80);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(state.get_flag(FLAG_CF));
        assert!(state.get_flag(FLAG_OF));
        assert!(state.get_flag(FLAG_SF));
    }

    // r/m16, r16 success.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x220;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AX, 0x1234);
        state.write_reg(Register::CX, 0xBEEF);
        bus.write_u16(addr, 0x1234).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x66, 0x0F, 0xB1, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u16(addr).unwrap(), 0xBEEF);
        assert_eq!(state.read_reg(Register::AX), 0x1234);
        assert!(state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
    }

    // r/m16, r16 failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x230;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AX, 0x0003);
        state.write_reg(Register::CX, 0x2222);
        bus.write_u16(addr, 0x0001).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x66, 0x0F, 0xB1, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u16(addr).unwrap(), 0x0001);
        assert_eq!(state.read_reg(Register::AX), 0x0001);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
        assert!(!state.get_flag(FLAG_OF));
    }

    // r/m32, r32 success.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x240;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::EAX, 0x1111_1111);
        state.write_reg(Register::ECX, 0x2222_2222);
        bus.write_u32(addr, 0x1111_1111).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xB1, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 0x2222_2222);
        assert_eq!(state.read_reg(Register::EAX), 0x1111_1111);
        assert!(state.get_flag(FLAG_ZF));
    }

    // r/m32, r32 failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x250;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::EAX, 2);
        state.write_reg(Register::ECX, 0x3333_3333);
        bus.write_u32(addr, 1).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xB1, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 1);
        assert_eq!(state.read_reg(Register::EAX), 1);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
    }

    // r/m64, r64 success.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x260;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::RAX, 0x1111_1111_2222_2222);
        state.write_reg(Register::RCX, 0x3333_3333_4444_4444);
        bus.write_u64(addr, state.read_reg(Register::RAX)).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xB1, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u64(addr).unwrap(), 0x3333_3333_4444_4444);
        assert_eq!(state.read_reg(Register::RAX), 0x1111_1111_2222_2222);
        assert!(state.get_flag(FLAG_ZF));
    }

    // r/m64, r64 failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x270;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::RAX, 2);
        state.write_reg(Register::RCX, 0xAAAA);
        bus.write_u64(addr, 1).unwrap();

        exec_steps(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xB1, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u64(addr).unwrap(), 1);
        assert_eq!(state.read_reg(Register::RAX), 1);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
    }
}

#[test]
fn lock_cmpxchg8b_success_and_failure() {
    let addr = 0x300u64;

    // Success.
    {
        let mut state = CpuState::new(CpuMode::Bit32);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        state.write_reg(Register::ESI, addr);

        let expected = 0x1122_3344_5566_7788u64;
        let replacement = 0xAABB_CCDD_EEFF_0011u64;
        bus.write_u64(addr, expected).unwrap();

        state.write_reg(Register::EAX, expected as u32 as u64);
        state.write_reg(Register::EDX, (expected >> 32) as u32 as u64);
        state.write_reg(Register::EBX, replacement as u32 as u64);
        state.write_reg(Register::ECX, (replacement >> 32) as u32 as u64);

        exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xC7, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u64(addr).unwrap(), replacement);
        assert!(state.get_flag(FLAG_ZF));
        assert_eq!(state.read_reg(Register::EAX), expected as u32 as u64);
        assert_eq!(
            state.read_reg(Register::EDX),
            (expected >> 32) as u32 as u64
        );
    }

    // Failure.
    {
        let mut state = CpuState::new(CpuMode::Bit32);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        state.write_reg(Register::ESI, addr);

        let old = 0x0123_4567_89AB_CDEFu64;
        bus.write_u64(addr, old).unwrap();

        state.write_reg(Register::EAX, 0x1111_1111);
        state.write_reg(Register::EDX, 0x2222_2222);
        state.write_reg(Register::EBX, 0x3333_3333);
        state.write_reg(Register::ECX, 0x4444_4444);

        exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xC7, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u64(addr).unwrap(), old);
        assert!(!state.get_flag(FLAG_ZF));
        assert_eq!(state.read_reg(Register::EAX), old as u32 as u64);
        assert_eq!(state.read_reg(Register::EDX), (old >> 32) as u32 as u64);
    }
}

#[test]
fn lock_cmpxchg16b_success_failure_and_alignment() {
    // Success.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x400;
        state.write_reg(Register::RSI, addr);

        let expected_lo = 0x1122_3344_5566_7788u64;
        let expected_hi = 0x99AA_BBCC_DDEE_FF00u64;
        let expected = ((expected_hi as u128) << 64) | expected_lo as u128;

        let replacement_lo = 0xA0A1_A2A3_A4A5_A6A7u64;
        let replacement_hi = 0xB0B1_B2B3_B4B5_B6B7u64;
        let replacement = ((replacement_hi as u128) << 64) | replacement_lo as u128;

        bus.write_u128(addr, expected).unwrap();
        state.write_reg(Register::RAX, expected_lo);
        state.write_reg(Register::RDX, expected_hi);
        state.write_reg(Register::RBX, replacement_lo);
        state.write_reg(Register::RCX, replacement_hi);

        exec_steps(
            &mut state,
            &mut bus,
            &[0xF0, 0x48, 0x0F, 0xC7, 0x0E], // LOCK CMPXCHG16B oword ptr [rsi]
            1,
        )
        .unwrap();

        assert!(state.get_flag(FLAG_ZF));
        assert_eq!(bus.read_u128(addr).unwrap(), replacement);
    }

    // Failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x420;
        state.write_reg(Register::RSI, addr);

        let old_lo = 0x1111_1111_2222_2222u64;
        let old_hi = 0x3333_3333_4444_4444u64;
        let old = ((old_hi as u128) << 64) | old_lo as u128;
        bus.write_u128(addr, old).unwrap();

        state.write_reg(Register::RAX, 0x5555_5555_6666_6666);
        state.write_reg(Register::RDX, 0x7777_7777_8888_8888);
        state.write_reg(Register::RBX, 0x9999_9999_AAAA_AAAA);
        state.write_reg(Register::RCX, 0xBBBB_BBBB_CCCC_CCCC);

        exec_steps(
            &mut state,
            &mut bus,
            &[0xF0, 0x48, 0x0F, 0xC7, 0x0E], // LOCK CMPXCHG16B oword ptr [rsi]
            1,
        )
        .unwrap();

        assert_eq!(bus.read_u128(addr).unwrap(), old);
        assert!(!state.get_flag(FLAG_ZF));
        assert_eq!(state.read_reg(Register::RAX), old_lo);
        assert_eq!(state.read_reg(Register::RDX), old_hi);
    }

    // Alignment fault (#GP(0)).
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x401;
        state.write_reg(Register::RSI, addr);

        bus.load(CODE_BASE, &[0xF0, 0x48, 0x0F, 0xC7, 0x0E]);
        state.set_rip(CODE_BASE);
        let res = step(&mut state, &mut bus);
        assert_eq!(res, Err(Exception::gp0()));
    }
}

#[test]
fn lock_xadd_updates_memory_register_and_flags() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let addr = 0x500;
    state.write_reg(Register::RSI, addr);

    bus.write_u32(addr, 0x8000_0000).unwrap();
    state.write_reg(Register::ECX, 0x8000_0001);

    exec_steps(&mut state, &mut bus, &[0xF0, 0x0F, 0xC1, 0x0E], 1).unwrap();

    assert_eq!(bus.read_u32(addr).unwrap(), 1);
    assert_eq!(state.read_reg(Register::ECX), 0x8000_0000);
    assert!(state.get_flag(FLAG_CF));
    assert!(state.get_flag(FLAG_OF));
    assert!(!state.get_flag(FLAG_ZF));
    assert!(!state.get_flag(FLAG_SF));
}

#[test]
fn lock_add_and_logic_ops_update_memory_and_flags() {
    // LOCK ADD dword ptr [rsi], ecx
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x540;
        state.write_reg(Register::RSI, addr);

        bus.write_u32(addr, 0x8000_0000).unwrap();
        state.write_reg(Register::ECX, 0x8000_0001);

        exec_steps(&mut state, &mut bus, &[0xF0, 0x01, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 1);
        assert_eq!(state.read_reg(Register::ECX), 0x8000_0001);
        assert!(state.get_flag(FLAG_CF));
        assert!(state.get_flag(FLAG_OF));
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_SF));
    }

    // LOCK OR dword ptr [rsi], ecx
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x550;
        state.write_reg(Register::RSI, addr);

        bus.write_u32(addr, 0).unwrap();
        state.write_reg(Register::ECX, 0x8000_0001);

        exec_steps(&mut state, &mut bus, &[0xF0, 0x09, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 0x8000_0001);
        assert_eq!(state.read_reg(Register::ECX), 0x8000_0001);
        assert!(!state.get_flag(FLAG_CF));
        assert!(!state.get_flag(FLAG_OF));
        assert!(!state.get_flag(FLAG_ZF));
        assert!(state.get_flag(FLAG_SF));
    }
}

#[test]
fn lock_inc_and_dec_update_memory_and_preserve_cf() {
    // INC
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x600;
        state.write_reg(Register::RSI, addr);
        state.set_flag(FLAG_CF, true);

        bus.write_u32(addr, 0x7FFF_FFFF).unwrap();
        exec_steps(&mut state, &mut bus, &[0xF0, 0xFF, 0x06], 1).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 0x8000_0000);
        assert!(state.get_flag(FLAG_CF));
        assert!(state.get_flag(FLAG_OF));
        assert!(state.get_flag(FLAG_SF));
        assert!(!state.get_flag(FLAG_ZF));
    }

    // DEC
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = FlatTestBus::new(BUS_SIZE);
        let addr = 0x610;
        state.write_reg(Register::RSI, addr);
        state.set_flag(FLAG_CF, false);

        bus.write_u32(addr, 0x8000_0000).unwrap();
        exec_steps(&mut state, &mut bus, &[0xF0, 0xFF, 0x0E], 1).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 0x7FFF_FFFF);
        assert!(!state.get_flag(FLAG_CF));
        assert!(state.get_flag(FLAG_OF));
        assert!(!state.get_flag(FLAG_SF));
        assert!(!state.get_flag(FLAG_ZF));
    }
}

#[test]
fn lock_bit_test_ops_update_memory_and_cf() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);
    let base = 0x700;
    state.write_reg(Register::RSI, base);

    bus.write_u32(base, 0).unwrap();
    bus.write_u32(base + 4, 0).unwrap();
    state.write_reg(Register::ECX, 33);

    bus.load(
        CODE_BASE,
        &[
            0xF0, 0x0F, 0xAB, 0x0E, // LOCK BTS dword ptr [rsi], ecx
            0xF0, 0x0F, 0xB3, 0x0E, // LOCK BTR dword ptr [rsi], ecx
            0xF0, 0x0F, 0xBB, 0x0E, // LOCK BTC dword ptr [rsi], ecx
        ],
    );
    state.set_rip(CODE_BASE);

    let _ = step(&mut state, &mut bus).unwrap();
    assert_eq!(bus.read_u32(base).unwrap(), 0);
    assert_eq!(bus.read_u32(base + 4).unwrap(), 0x2);
    assert!(!state.get_flag(FLAG_CF));

    let _ = step(&mut state, &mut bus).unwrap();
    assert_eq!(bus.read_u32(base + 4).unwrap(), 0);
    assert!(state.get_flag(FLAG_CF));

    let _ = step(&mut state, &mut bus).unwrap();
    assert_eq!(bus.read_u32(base + 4).unwrap(), 0x2);
    assert!(!state.get_flag(FLAG_CF));
}

#[test]
fn lock_prefix_on_register_operand_is_invalid_opcode() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // LOCK ADD eax, ecx
    bus.load(CODE_BASE, &[0xF0, 0x01, 0xC8]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));

    // LOCK CMPXCHG eax, ecx
    bus.load(CODE_BASE, &[0xF0, 0x0F, 0xB1, 0xC8]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));

    // LOCK XADD eax, ecx
    bus.load(CODE_BASE, &[0xF0, 0x0F, 0xC1, 0xC8]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));

    // LOCK XCHG eax, ecx
    bus.load(CODE_BASE, &[0xF0, 0x87, 0xC8]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));

    // LOCK INC eax
    bus.load(CODE_BASE, &[0xF0, 0xFF, 0xC0]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));

    // LOCK BTS eax, ecx
    bus.load(CODE_BASE, &[0xF0, 0x0F, 0xAB, 0xC8]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));
}

#[test]
fn xchg_memory_operand_is_implicitly_atomic_and_swaps() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = CountingBus::new(BUS_SIZE);
    let addr = 0x200;
    state.write_reg(Register::RSI, addr);

    bus.write_u32(addr, 0xAABB_CCDD).unwrap();
    state.write_reg(Register::EAX, 0x1234_5678);

    // XCHG dword ptr [rsi], eax
    bus.load(CODE_BASE, &[0x87, 0x06]);
    state.set_rip(CODE_BASE);
    let exit = step(&mut state, &mut bus).unwrap();
    assert!(matches!(
        exit,
        StepExit::Continue | StepExit::ContinueInhibitInterrupts | StepExit::Branch
    ));

    assert_eq!(bus.atomic_rmw_calls, 1);
    assert_eq!(bus.read_u32(addr).unwrap(), 0x1234_5678);
    assert_eq!(state.read_reg(Register::EAX), 0xAABB_CCDD);
}

#[test]
fn lock_prefix_on_non_lockable_alu_instruction_is_invalid_opcode() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // LOCK CLC
    bus.load(CODE_BASE, &[0xF0, 0xF8]);
    state.set_rip(CODE_BASE);
    assert_eq!(step(&mut state, &mut bus), Err(Exception::InvalidOpcode));
}
