use aero_cpu_core::interp::tier0::exec::{step, StepExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState, FLAG_CF, FLAG_OF, FLAG_SF, FLAG_ZF};
use aero_cpu_core::Exception;
use aero_x86::Register;

const BUS_SIZE: usize = 0x10_000;
const CODE_ADDR: u64 = 0x1000;

fn setup_bus() -> FlatTestBus {
    FlatTestBus::new(BUS_SIZE)
}

fn exec_once(state: &mut CpuState, bus: &mut FlatTestBus, bytes: &[u8]) -> Result<(), Exception> {
    bus.load(CODE_ADDR, bytes);
    state.set_rip(CODE_ADDR);
    match step(state, bus)? {
        StepExit::Continue | StepExit::Branch => Ok(()),
        StepExit::Halted => panic!("unexpected HLT"),
        StepExit::BiosInterrupt(vector) => panic!("unexpected BIOS interrupt: {vector:#x}"),
        StepExit::Assist(r) => panic!("unexpected assist: {r:?}"),
    }
}

#[test]
fn lock_cmpxchg_rmw_sizes_success_and_failure() {
    // r/m8, r8
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x200;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AL, 0x11);
        state.write_reg(Register::CL, 0x22);
        bus.write_u8(addr, 0x11).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xB0, 0x0E]).unwrap(); // LOCK CMPXCHG byte ptr [rsi], cl

        assert_eq!(bus.read_u8(addr).unwrap(), 0x22);
        assert_eq!(state.read_reg(Register::AL) as u8, 0x11);
        assert!(state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
        assert!(!state.get_flag(FLAG_OF));
    }

    // r/m8, r8 failure (exercise OF for subtraction).
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x210;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AL, 0x01);
        state.write_reg(Register::CL, 0x33);
        bus.write_u8(addr, 0x80).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xB0, 0x0E]).unwrap();

        assert_eq!(bus.read_u8(addr).unwrap(), 0x80);
        assert_eq!(state.read_reg(Register::AL) as u8, 0x80);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(state.get_flag(FLAG_CF));
        assert!(state.get_flag(FLAG_OF));
        assert!(state.get_flag(FLAG_SF));
    }

    // r/m16, r16
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x220;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AX, 0x1234);
        state.write_reg(Register::CX, 0xBEEF);
        bus.write_u16(addr, 0x1234).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x66, 0x0F, 0xB1, 0x0E]).unwrap(); // LOCK CMPXCHG word ptr [rsi], cx

        assert_eq!(bus.read_u16(addr).unwrap(), 0xBEEF);
        assert_eq!(state.read_reg(Register::AX) as u16, 0x1234);
        assert!(state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
    }

    // r/m16, r16 failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x230;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::AX, 0x0003);
        state.write_reg(Register::CX, 0x2222);
        bus.write_u16(addr, 0x0001).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x66, 0x0F, 0xB1, 0x0E]).unwrap();

        assert_eq!(bus.read_u16(addr).unwrap(), 0x0001);
        assert_eq!(state.read_reg(Register::AX) as u16, 0x0001);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
        assert!(!state.get_flag(FLAG_OF));
    }

    // r/m32, r32
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x240;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::EAX, 0x1111_1111);
        state.write_reg(Register::ECX, 0x2222_2222);
        bus.write_u32(addr, 0x1111_1111).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xB1, 0x0E]).unwrap(); // LOCK CMPXCHG dword ptr [rsi], ecx

        assert_eq!(bus.read_u32(addr).unwrap(), 0x2222_2222);
        assert_eq!(state.read_reg(Register::EAX) as u32, 0x1111_1111);
        assert!(state.get_flag(FLAG_ZF));
    }

    // r/m32, r32 failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x250;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::EAX, 2);
        state.write_reg(Register::ECX, 0x3333_3333);
        bus.write_u32(addr, 1).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xB1, 0x0E]).unwrap();

        assert_eq!(bus.read_u32(addr).unwrap(), 1);
        assert_eq!(state.read_reg(Register::EAX) as u32, 1);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
    }

    // r/m64, r64
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x260;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::RAX, 0x1111_1111_2222_2222);
        state.write_reg(Register::RCX, 0x3333_3333_4444_4444);
        bus.write_u64(addr, 0x1111_1111_2222_2222).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xB1, 0x0E]).unwrap(); // LOCK CMPXCHG qword ptr [rsi], rcx

        assert_eq!(bus.read_u64(addr).unwrap(), 0x3333_3333_4444_4444);
        assert_eq!(state.read_reg(Register::RAX), 0x1111_1111_2222_2222);
        assert!(state.get_flag(FLAG_ZF));
    }

    // r/m64, r64 failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x270;
        state.write_reg(Register::RSI, addr);
        state.write_reg(Register::RAX, 2);
        state.write_reg(Register::RCX, 0xAAAA);
        bus.write_u64(addr, 1).unwrap();

        exec_once(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xB1, 0x0E]).unwrap();

        assert_eq!(bus.read_u64(addr).unwrap(), 1);
        assert_eq!(state.read_reg(Register::RAX), 1);
        assert!(!state.get_flag(FLAG_ZF));
        assert!(!state.get_flag(FLAG_CF));
    }
}

#[test]
fn lock_cmpxchg8b_success_and_failure() {
    let addr: u64 = 0x300;

    // Success.
    {
        let mut state = CpuState::new(CpuMode::Bit32);
        let mut bus = setup_bus();
        state.write_reg(Register::ESI, addr);

        let expected = 0x1122_3344_5566_7788u64;
        let replacement = 0xAABB_CCDD_EEFF_0011u64;
        bus.write_u64(addr, expected).unwrap();

        state.write_reg(Register::EAX, expected as u32 as u64);
        state.write_reg(Register::EDX, (expected >> 32) as u32 as u64);
        state.write_reg(Register::EBX, replacement as u32 as u64);
        state.write_reg(Register::ECX, (replacement >> 32) as u32 as u64);

        exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xC7, 0x0E]).unwrap(); // LOCK CMPXCHG8B qword ptr [esi]

        assert_eq!(bus.read_u64(addr).unwrap(), replacement);
        assert!(state.get_flag(FLAG_ZF));
        assert_eq!(state.read_reg(Register::EAX) as u32, expected as u32);
        assert_eq!(
            state.read_reg(Register::EDX) as u32,
            (expected >> 32) as u32
        );
    }

    // Failure.
    {
        let mut state = CpuState::new(CpuMode::Bit32);
        let mut bus = setup_bus();
        state.write_reg(Register::ESI, addr);

        let old = 0x0123_4567_89AB_CDEFu64;
        bus.write_u64(addr, old).unwrap();

        state.write_reg(Register::EAX, 0x1111_1111);
        state.write_reg(Register::EDX, 0x2222_2222);
        state.write_reg(Register::EBX, 0x3333_3333);
        state.write_reg(Register::ECX, 0x4444_4444);

        exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xC7, 0x0E]).unwrap();

        assert_eq!(bus.read_u64(addr).unwrap(), old);
        assert!(!state.get_flag(FLAG_ZF));
        assert_eq!(state.read_reg(Register::EAX) as u32, old as u32);
        assert_eq!(state.read_reg(Register::EDX) as u32, (old >> 32) as u32);
    }
}

#[test]
fn lock_cmpxchg16b_success_failure_and_alignment() {
    // Success.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
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

        // Note: In 64-bit mode, `CMPXCHG16B` uses `REX.W` to disambiguate it from
        // `CMPXCHG8B` (same opcode map + ModRM extension).
        exec_once(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xC7, 0x0E]).unwrap(); // LOCK CMPXCHG16B oword ptr [rsi]

        assert_eq!(bus.read_u128(addr).unwrap(), replacement);
        assert!(state.get_flag(FLAG_ZF));
    }

    // Failure.
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
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

        exec_once(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xC7, 0x0E]).unwrap();

        assert_eq!(bus.read_u128(addr).unwrap(), old);
        assert!(!state.get_flag(FLAG_ZF));
        assert_eq!(state.read_reg(Register::RAX), old_lo);
        assert_eq!(state.read_reg(Register::RDX), old_hi);
    }

    // Alignment fault (#GP(0)).
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x401;
        state.write_reg(Register::RSI, addr);

        let res = exec_once(&mut state, &mut bus, &[0xF0, 0x48, 0x0F, 0xC7, 0x0E]);
        assert_eq!(res, Err(Exception::gp0()));
    }
}

#[test]
fn lock_xadd_updates_memory_register_and_flags() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = setup_bus();
    let addr = 0x500;
    state.write_reg(Register::RSI, addr);

    bus.write_u32(addr, 0x8000_0000).unwrap();
    state.write_reg(Register::ECX, 0x8000_0001);

    exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xC1, 0x0E]).unwrap(); // LOCK XADD dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(addr).unwrap(), 1);
    assert_eq!(state.read_reg(Register::ECX) as u32, 0x8000_0000);
    assert!(state.get_flag(FLAG_CF));
    assert!(state.get_flag(FLAG_OF));
    assert!(!state.get_flag(FLAG_ZF));
    assert!(!state.get_flag(FLAG_SF));
}

#[test]
fn lock_inc_and_dec_update_memory_and_preserve_cf() {
    // INC
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x600;
        state.write_reg(Register::RSI, addr);
        state.set_flag(FLAG_CF, true);

        bus.write_u32(addr, 0x7FFF_FFFF).unwrap();
        exec_once(&mut state, &mut bus, &[0xF0, 0xFF, 0x06]).unwrap(); // LOCK INC dword ptr [rsi]

        assert_eq!(bus.read_u32(addr).unwrap(), 0x8000_0000);
        assert!(state.get_flag(FLAG_CF));
        assert!(state.get_flag(FLAG_OF));
        assert!(state.get_flag(FLAG_SF));
        assert!(!state.get_flag(FLAG_ZF));
    }

    // DEC
    {
        let mut state = CpuState::new(CpuMode::Bit64);
        let mut bus = setup_bus();
        let addr = 0x610;
        state.write_reg(Register::RSI, addr);
        state.set_flag(FLAG_CF, false);

        bus.write_u32(addr, 0x8000_0000).unwrap();
        exec_once(&mut state, &mut bus, &[0xF0, 0xFF, 0x0E]).unwrap(); // LOCK DEC dword ptr [rsi]

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
    let mut bus = setup_bus();
    let base = 0x700;
    state.write_reg(Register::RSI, base);

    bus.write_u32(base, 0).unwrap();
    bus.write_u32(base + 4, 0).unwrap();
    state.write_reg(Register::ECX, 33); // element 1, bit 1 (32-bit bitmap semantics)

    exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xAB, 0x0E]).unwrap(); // LOCK BTS dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(base).unwrap(), 0);
    assert_eq!(bus.read_u32(base + 4).unwrap(), 0x2);
    assert!(!state.get_flag(FLAG_CF));

    exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xB3, 0x0E]).unwrap(); // LOCK BTR dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(base + 4).unwrap(), 0);
    assert!(state.get_flag(FLAG_CF));

    exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xBB, 0x0E]).unwrap(); // LOCK BTC dword ptr [rsi], ecx

    assert_eq!(bus.read_u32(base + 4).unwrap(), 0x2);
    assert!(!state.get_flag(FLAG_CF));
}

#[test]
fn lock_prefix_on_register_operand_is_invalid_opcode() {
    let mut state = CpuState::new(CpuMode::Bit64);
    let mut bus = setup_bus();
    let res = exec_once(&mut state, &mut bus, &[0xF0, 0x0F, 0xB1, 0xC8]); // LOCK CMPXCHG eax, ecx
    assert_eq!(res, Err(Exception::InvalidOpcode));
}
