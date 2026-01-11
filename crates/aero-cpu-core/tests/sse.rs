use aero_cpu_core::interp::tier0::exec::step_with_config;
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::interp::{
    clear_exception_flags, set_rounding_mode, RoundingMode, MXCSR_IE, MXCSR_PE,
};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{CpuMode, CpuState, CR4_OSFXSR};
use aero_cpu_core::Exception;
use aero_x86::Register;

const BUS_SIZE: usize = 0x4000;
const CODE_BASE: u64 = 0x1000;

fn xmm(state: &CpuState, idx: usize) -> u128 {
    state.sse.xmm[idx]
}

fn set_xmm(state: &mut CpuState, idx: usize, value: u128) {
    state.sse.xmm[idx] = value;
}

fn u128_from_u32x4(v: [u32; 4]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn u128_from_u16x8(v: [u16; 8]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 2..i * 2 + 2].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn u128_from_u64x2(v: [u64; 2]) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, lane) in v.iter().copied().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(bytes)
}

fn new_sse_state(mode: CpuMode) -> CpuState {
    let mut state = CpuState::new(mode);
    state.control.cr4 |= CR4_OSFXSR;
    state
}

fn exec_once(
    cfg: &Tier0Config,
    state: &mut CpuState,
    bus: &mut FlatTestBus,
    code: &[u8],
) -> Result<(), Exception> {
    bus.load(CODE_BASE, code);
    state.set_rip(CODE_BASE);
    step_with_config(cfg, state, bus).map(|_| ())
}

#[test]
fn movaps_reg_reg_and_alignment_faults() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(&mut state, 0, 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00u128);
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x28, 0xC8]).unwrap(); // movaps xmm1, xmm0
    assert_eq!(xmm(&state, 1), xmm(&state, 0));

    // movaps xmm2, [rax] where rax is unaligned => #GP(0)
    state.write_reg(Register::RAX, 1);
    bus.write_u128(1, 0xdead_beef_dead_beef_dead_beef_dead_beefu128)
        .unwrap();
    let err = exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x28, 0x10]).unwrap_err();
    assert_eq!(err, Exception::gp0());
}

#[test]
fn movups_mem_reg_and_store() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    state.write_reg(Register::RAX, 3);
    bus.write_u128(3, 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10u128)
        .unwrap();
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x10, 0x00]).unwrap(); // movups xmm0, [rax]
    assert_eq!(
        xmm(&state, 0),
        0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10u128
    );

    state.write_reg(Register::RBX, 5);
    set_xmm(&mut state, 1, 0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111u128);
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x11, 0x0B]).unwrap(); // movups [rbx], xmm1
    assert_eq!(
        bus.read_u128(5).unwrap(),
        0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111u128
    );
}

#[test]
fn movdqa_mem_reg_alignment() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    state.write_reg(Register::RAX, 0);
    bus.write_u128(0, 0x1111_2222_3333_4444_5555_6666_7777_8888u128)
        .unwrap();
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x6F, 0x00]).unwrap(); // movdqa xmm0, [rax]
    assert_eq!(
        xmm(&state, 0),
        0x1111_2222_3333_4444_5555_6666_7777_8888u128
    );

    state.write_reg(Register::RAX, 1);
    let err = exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x6F, 0x08]).unwrap_err(); // movdqa xmm1, [rax]
    assert_eq!(err, Exception::gp0());
}

#[test]
fn movss_and_movsd_preserve_upper_lanes() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(&mut state, 0, 0x1111_2222_3333_4444_5555_6666_7777_8888u128);
    set_xmm(&mut state, 1, 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x10, 0xC1]).unwrap(); // movss xmm0, xmm1
    assert_eq!(
        xmm(&state, 0) & !0xFFFF_FFFFu128,
        0x1111_2222_3333_4444_5555_6666_0000_0000u128
    );
    assert_eq!(xmm(&state, 0) as u32, xmm(&state, 1) as u32);

    set_xmm(&mut state, 2, 0x1111_2222_3333_4444_5555_6666_7777_8888u128);
    set_xmm(&mut state, 3, 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x10, 0xD3]).unwrap(); // movsd xmm2, xmm3
    assert_eq!(xmm(&state, 2) >> 64, 0x1111_2222_3333_4444u128);
    assert_eq!(xmm(&state, 2) as u64, xmm(&state, 3) as u64);

    // Memory source preserves upper 96 bits.
    state.write_reg(Register::RAX, 16);
    bus.write_u32(16, 0x3f80_0000).unwrap(); // 1.0f32
    set_xmm(&mut state, 4, 0xffff_eeee_dddd_cccc_bbbb_aaaa_9999_8888u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x10, 0x20]).unwrap(); // movss xmm4, [rax]
    assert_eq!(xmm(&state, 4) as u32, 0x3f80_0000);
    assert_eq!(
        xmm(&state, 4) & !0xFFFF_FFFFu128,
        0xffff_eeee_dddd_cccc_bbbb_aaaa_0000_0000u128
    );
}

#[test]
fn movd_and_movq_roundtrip() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    state.write_reg(Register::RAX, 0x1122_3344_5566_7788);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x6E, 0xC0]).unwrap(); // movd xmm0, eax
    assert_eq!(xmm(&state, 0), 0x5566_7788u128);

    set_xmm(&mut state, 1, 0xdead_beef_cafe_babe_0123_4567_89ab_cdefu128);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x7E, 0xC8]).unwrap(); // movd eax, xmm1
    assert_eq!(state.read_reg(Register::EAX) as u32, 0x89ab_cdef);

    state.write_reg(Register::RBX, 32);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x7E, 0x0B]).unwrap(); // movd [rbx], xmm1
    assert_eq!(bus.read_u32(32).unwrap(), 0x89ab_cdef);

    state.write_reg(Register::RCX, 0x0102_0304_0506_0708);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x48, 0x0F, 0x6E, 0xD1]).unwrap(); // movq xmm2, rcx
    assert_eq!(xmm(&state, 2), 0x0102_0304_0506_0708u128);

    set_xmm(&mut state, 3, 0xaaaa_bbbb_cccc_dddd_eeee_ffff_1111_2222u128);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x48, 0x0F, 0x7E, 0xD9]).unwrap(); // movq rcx, xmm3
    assert_eq!(state.read_reg(Register::RCX), 0xeeee_ffff_1111_2222);

    state.write_reg(Register::RBX, 40);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x48, 0x0F, 0x7E, 0x1B]).unwrap(); // movq [rbx], xmm3
    assert_eq!(bus.read_u64(40).unwrap(), 0xeeee_ffff_1111_2222);
}

#[test]
fn movhlps_movlhps_and_unpcklps_unpckhps() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(
        &mut state,
        0,
        u128_from_u64x2([0xaaaabbbbccccdddd, 0x1111222233334444]),
    );
    set_xmm(
        &mut state,
        1,
        u128_from_u64x2([0x5555666677778888, 0x9999aaaabbbbcccc]),
    );

    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x12, 0xC1]).unwrap(); // movhlps xmm0, xmm1
    assert_eq!(
        xmm(&state, 0),
        u128_from_u64x2([0x9999aaaabbbbcccc, 0x1111222233334444])
    );

    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x16, 0xC1]).unwrap(); // movlhps xmm0, xmm1
    assert_eq!(
        xmm(&state, 0),
        u128_from_u64x2([0x9999aaaabbbbcccc, 0x5555666677778888])
    );

    set_xmm(&mut state, 2, u128_from_u32x4([1, 2, 3, 4]));
    set_xmm(&mut state, 3, u128_from_u32x4([10, 20, 30, 40]));
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x14, 0xD3]).unwrap(); // unpcklps xmm2,xmm3
    assert_eq!(xmm(&state, 2), u128_from_u32x4([1, 10, 2, 20]));

    set_xmm(&mut state, 4, u128_from_u32x4([1, 2, 3, 4]));
    set_xmm(&mut state, 5, u128_from_u32x4([10, 20, 30, 40]));
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x15, 0xE5]).unwrap(); // unpckhps xmm4,xmm5
    assert_eq!(xmm(&state, 4), u128_from_u32x4([3, 30, 4, 40]));
}

#[test]
fn shuffle_and_saturating_word_ops() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(&mut state, 0, u128_from_u32x4([1, 2, 3, 4]));
    set_xmm(&mut state, 1, u128_from_u32x4([10, 20, 30, 40]));
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0xC6, 0xC1, 0x1B]).unwrap(); // shufps xmm0,xmm1,0x1B
    assert_eq!(xmm(&state, 0), u128_from_u32x4([4, 3, 20, 10]));

    let src_addr = 0x200u64;
    state.write_reg(Register::RAX, src_addr);
    bus.write_u128(src_addr, u128_from_u32x4([1, 2, 3, 4]))
        .unwrap();
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x70, 0x10, 0x1B]).unwrap(); // pshufd xmm2,[rax],0x1B
    assert_eq!(xmm(&state, 2), u128_from_u32x4([4, 3, 2, 1]));

    set_xmm(
        &mut state,
        3,
        u128_from_u16x8([0x7FFF, 0x8000, 1, 0, 0, 0, 0, 0]),
    );
    set_xmm(
        &mut state,
        4,
        u128_from_u16x8([1, 0xFFFF, 0xFFFF, 2, 0, 0, 0, 0]),
    );
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xED, 0xDC]).unwrap(); // paddsw xmm3,xmm4
    assert_eq!(
        xmm(&state, 3),
        u128_from_u16x8([0x7FFF, 0x8000, 0, 2, 0, 0, 0, 0])
    );

    set_xmm(
        &mut state,
        5,
        u128_from_u16x8([1, 0xFFFF, 5, 0, 0, 0, 0, 0]),
    );
    set_xmm(&mut state, 6, u128_from_u16x8([2, 2, 10, 0, 0, 0, 0, 0]));
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xD9, 0xEE]).unwrap(); // psubusw xmm5,xmm6
    assert_eq!(
        xmm(&state, 5),
        u128_from_u16x8([0, 0xFFFD, 0, 0, 0, 0, 0, 0])
    );
}

#[test]
fn logical_ops_and_integer_arith() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(&mut state, 0, 0x00ff_00ff_00ff_00ff_00ff_00ff_00ff_00ffu128);
    set_xmm(&mut state, 1, 0x0f0f_f0f0_aaaa_5555_ffff_0000_1234_5678u128);
    exec_once(&cfg, &mut state, &mut bus, &[0x0F, 0x54, 0xC1]).unwrap(); // andps xmm0,xmm1
    assert_eq!(
        xmm(&state, 0),
        0x000f_00f0_00aa_0055_00ff_0000_0034_0078u128
    );

    set_xmm(&mut state, 2, 0xf0f0_f0f0_f0f0_f0f0_f0f0_f0f0_f0f0_f0f0u128);
    let src_addr = 0x220u64;
    state.write_reg(Register::RAX, src_addr);
    bus.write_u128(src_addr, 0x0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0fu128)
        .unwrap();
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xDB, 0x10]).unwrap(); // pand xmm2,[rax]
    assert_eq!(xmm(&state, 2), 0);

    set_xmm(&mut state, 3, u128_from_u32x4([0xffff_fffe, 1, 0, 5]));
    set_xmm(&mut state, 4, u128_from_u32x4([2, 2, 1, 10]));
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xFE, 0xDC]).unwrap(); // paddd xmm3,xmm4
    assert_eq!(xmm(&state, 3), u128_from_u32x4([0, 3, 1, 15]));

    set_xmm(&mut state, 5, u128_from_u64x2([u64::MAX, 1]));
    let src_addr2 = 0x240u64;
    state.write_reg(Register::RAX, src_addr2);
    bus.write_u128(src_addr2, u128_from_u64x2([1, 2])).unwrap();
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xD4, 0x28]).unwrap(); // paddq xmm5,[rax]
    assert_eq!(xmm(&state, 5), u128_from_u64x2([0, 3]));
}

#[test]
fn saturating_arith_and_shifts() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(&mut state, 0, u128::from_le_bytes([127u8; 16]));
    set_xmm(&mut state, 1, u128::from_le_bytes([1u8; 16]));
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xEC, 0xC1]).unwrap(); // paddsb xmm0,xmm1
    assert_eq!(xmm(&state, 0), u128::from_le_bytes([127u8; 16]));

    set_xmm(&mut state, 2, u128::from_le_bytes([250u8; 16]));
    set_xmm(&mut state, 3, u128::from_le_bytes([10u8; 16]));
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xDC, 0xD3]).unwrap(); // paddusb xmm2,xmm3
    assert_eq!(xmm(&state, 2), u128::from_le_bytes([255u8; 16]));

    set_xmm(
        &mut state,
        4,
        u128_from_u16x8([0x8000, 0x7fff, 0, 1, 2, 3, 4, 5]),
    );
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x71, 0xE4, 0x0F]).unwrap(); // psraw xmm4,15
    assert_eq!(xmm(&state, 4) as u16, 0xFFFF);

    set_xmm(
        &mut state,
        5,
        u128_from_u64x2([0x1122_3344_5566_7788, 0x99aa_bbcc_ddee_ff00]),
    );
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x73, 0xFD, 0x04]).unwrap(); // pslldq xmm5,4
    assert_eq!(
        xmm(&state, 5),
        0xddee_ff00_1122_3344_5566_7788_0000_0000u128
    );
}

#[test]
fn comparisons_and_mul() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(
        &mut state,
        0,
        u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    );
    set_xmm(
        &mut state,
        1,
        u128::from_le_bytes([1, 0, 3, 0, 5, 0, 7, 0, 9, 0, 11, 0, 13, 0, 15, 0]),
    );
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x74, 0xC1]).unwrap(); // pcmpeqb xmm0,xmm1
    assert_eq!(xmm(&state, 0) as u8, 0xFF);
    assert_eq!((xmm(&state, 0) >> 8) as u8, 0);

    set_xmm(
        &mut state,
        2,
        u128::from_le_bytes([0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    set_xmm(
        &mut state,
        3,
        u128::from_le_bytes([0x7f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x64, 0xD3]).unwrap(); // pcmpgtb xmm2,xmm3
    assert_eq!(xmm(&state, 2) as u8, 0);

    set_xmm(&mut state, 4, u128_from_u16x8([3, 4, 5, 6, 7, 8, 9, 10]));
    set_xmm(&mut state, 5, u128_from_u16x8([2, 2, 2, 2, 2, 2, 2, 2]));
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xD5, 0xE5]).unwrap(); // pmullw xmm4,xmm5
    assert_eq!(
        xmm(&state, 4),
        u128_from_u16x8([6, 8, 10, 12, 14, 16, 18, 20])
    );

    set_xmm(&mut state, 6, u128_from_u32x4([3, 0, 4, 0]));
    set_xmm(&mut state, 7, u128_from_u32x4([5, 0, 6, 0]));
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0xF4, 0xF7]).unwrap(); // pmuludq xmm6,xmm7
    assert_eq!(xmm(&state, 6), u128_from_u64x2([15, 24]));
}

#[test]
fn fp_scalar_and_conversions_with_rounding_modes() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    set_xmm(
        &mut state,
        0,
        (1.0f32.to_bits() as u128) | (0xaaaa_bbbb_cccc_dddd_u128 << 32),
    );
    set_xmm(
        &mut state,
        1,
        (2.5f32.to_bits() as u128) | (0x1111_2222_3333_4444_u128 << 32),
    );
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x58, 0xC1]).unwrap(); // addss xmm0,xmm1
    assert_eq!(f32::from_bits(xmm(&state, 0) as u32), 3.5);
    assert_eq!(xmm(&state, 0) >> 32, 0xaaaa_bbbb_cccc_dddd_u128);

    set_xmm(
        &mut state,
        2,
        (1.0f64.to_bits() as u128) | (0x9999_8888_7777_6666_u128 << 64),
    );
    set_xmm(&mut state, 3, 2.0f64.to_bits() as u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x59, 0xD3]).unwrap(); // mulsd xmm2,xmm3
    assert_eq!(f64::from_bits(xmm(&state, 2) as u64), 2.0);
    assert_eq!(xmm(&state, 2) >> 64, 0x9999_8888_7777_6666_u128);

    // Rounding mode affects CVT* conversions (not CVTT*).
    clear_exception_flags(&mut state.sse.mxcsr);
    set_rounding_mode(&mut state.sse.mxcsr, RoundingMode::Nearest);
    set_xmm(&mut state, 4, 2.5f32.to_bits() as u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x2D, 0xC4]).unwrap(); // cvtss2si eax,xmm4
    assert_eq!(state.read_reg(Register::EAX) as i32, 2); // ties-to-even
    assert_ne!(state.sse.mxcsr & MXCSR_PE, 0);

    clear_exception_flags(&mut state.sse.mxcsr);
    set_rounding_mode(&mut state.sse.mxcsr, RoundingMode::Down);
    set_xmm(&mut state, 4, 1.9f32.to_bits() as u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x2D, 0xC4]).unwrap(); // cvtss2si eax,xmm4
    assert_eq!(state.read_reg(Register::EAX) as i32, 1);

    clear_exception_flags(&mut state.sse.mxcsr);
    set_rounding_mode(&mut state.sse.mxcsr, RoundingMode::Up);
    set_xmm(&mut state, 4, (-1.1f32).to_bits() as u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x2D, 0xC4]).unwrap(); // cvtss2si eax,xmm4
    assert_eq!(state.read_reg(Register::EAX) as i32, -1);

    clear_exception_flags(&mut state.sse.mxcsr);
    set_rounding_mode(&mut state.sse.mxcsr, RoundingMode::Up);
    set_xmm(&mut state, 4, 1.9f32.to_bits() as u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x2C, 0xC4]).unwrap(); // cvttss2si eax,xmm4
    assert_eq!(state.read_reg(Register::EAX) as i32, 1); // truncates

    clear_exception_flags(&mut state.sse.mxcsr);
    set_rounding_mode(&mut state.sse.mxcsr, RoundingMode::Nearest);
    set_xmm(&mut state, 5, f64::NAN.to_bits() as u128);
    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x2D, 0xC5]).unwrap(); // cvtsd2si eax,xmm5
    assert_eq!(state.read_reg(Register::EAX) as i32, i32::MIN);
    assert_ne!(state.sse.mxcsr & MXCSR_IE, 0);
}

#[test]
fn xmm0_15_are_addressable_in_long_mode() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    let pattern = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128;
    state.sse.xmm[0] = pattern;
    state.sse.xmm[1] = 0;
    state.sse.xmm[15] = 0xdead_beef;

    // movaps xmm15, xmm0 (REX.R)
    // movaps xmm1, xmm15 (REX.B)
    let code = [0x44, 0x0F, 0x28, 0xF8, 0x41, 0x0F, 0x28, 0xCF];
    bus.load(CODE_BASE, &code);
    state.set_rip(CODE_BASE);
    step_with_config(&cfg, &mut state, &mut bus).unwrap();
    step_with_config(&cfg, &mut state, &mut bus).unwrap();

    assert_eq!(state.sse.xmm[15], pattern);
    assert_eq!(state.sse.xmm[1], pattern);
}

#[test]
fn instruction_stream_mixed_sse_sse2() {
    let cfg = Tier0Config::default();
    let mut state = new_sse_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // movups xmm0, [rax]     ; SSE
    // paddd xmm0, xmm1       ; SSE2
    // movups [rbx], xmm0     ; SSE
    let code = [
        0x0F, 0x10, 0x00, // movups xmm0, [rax]
        0x66, 0x0F, 0xFE, 0xC1, // paddd xmm0, xmm1
        0x0F, 0x11, 0x03, // movups [rbx], xmm0
    ];
    bus.load(CODE_BASE, &code);
    state.set_rip(CODE_BASE);

    state.write_reg(Register::RAX, 0);
    state.write_reg(Register::RBX, 0x80);
    bus.write_u128(0, u128_from_u32x4([1, 2, 3, 4])).unwrap();
    state.sse.xmm[1] = u128_from_u32x4([10, 20, 30, 40]);

    step_with_config(&cfg, &mut state, &mut bus).unwrap();
    step_with_config(&cfg, &mut state, &mut bus).unwrap();
    step_with_config(&cfg, &mut state, &mut bus).unwrap();

    assert_eq!(bus.read_u128(0x80).unwrap(), u128_from_u32x4([11, 22, 33, 44]));
}
