use aero_cpu_core::{
    interp::{
        clear_exception_flags, set_rounding_mode, sse, sse2, RoundingMode, XmmOperand,
        XmmReg, MXCSR_IE, MXCSR_PE,
    },
    Bus, CpuState, Exception, RamBus,
};

fn xmm(cpu: &CpuState, reg: XmmReg) -> u128 {
    cpu.sse.xmm[reg.index()]
}

fn set_xmm(cpu: &mut CpuState, reg: XmmReg, value: u128) {
    cpu.sse.xmm[reg.index()] = value;
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

#[test]
fn movaps_reg_reg_and_alignment_faults() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm0,
        0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00u128,
    );
    sse::movaps(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm1),
        XmmOperand::Reg(XmmReg::Xmm0),
        true,
    )
    .unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm1), xmm(&cpu, XmmReg::Xmm0));

    bus.write_u128(1, 0xdead_beef_dead_beef_dead_beef_dead_beefu128);
    let err = sse::movaps(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm2),
        XmmOperand::Mem(1),
        true,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());

    // With alignment checks disabled, MOVAPS behaves like MOVUPS.
    sse::movaps(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm2),
        XmmOperand::Mem(1),
        false,
    )
    .unwrap();
}

#[test]
fn movups_mem_reg_and_store() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    bus.write_u128(3, 0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10u128);
    sse::movups(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm0),
        XmmOperand::Mem(3),
    )
    .unwrap();
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0),
        0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10u128
    );

    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111u128,
    );
    sse::movups(
        &mut cpu,
        &mut bus,
        XmmOperand::Mem(5),
        XmmOperand::Reg(XmmReg::Xmm1),
    )
    .unwrap();
    assert_eq!(
        bus.read_u128(5),
        0xaaaa_bbbb_cccc_dddd_eeee_ffff_0000_1111u128
    );
}

#[test]
fn movdqa_mem_reg_alignment() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    bus.write_u128(0, 0x1111_2222_3333_4444_5555_6666_7777_8888u128);
    sse2::movdqa(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm0),
        XmmOperand::Mem(0),
        true,
    )
    .unwrap();
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0),
        0x1111_2222_3333_4444_5555_6666_7777_8888u128
    );

    let err = sse2::movdqa(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm1),
        XmmOperand::Mem(1),
        true,
    )
    .unwrap_err();
    assert_eq!(err, Exception::gp0());
}

#[test]
fn movss_and_movsd_preserve_upper_lanes() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm0,
        0x1111_2222_3333_4444_5555_6666_7777_8888u128,
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000u128,
    );
    sse::movss(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm0),
        XmmOperand::Reg(XmmReg::Xmm1),
    )
    .unwrap();
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0) & !0xFFFF_FFFFu128,
        0x1111_2222_3333_4444_5555_6666_0000_0000u128
    );
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0) as u32,
        xmm(&cpu, XmmReg::Xmm1) as u32
    );

    set_xmm(
        &mut cpu,
        XmmReg::Xmm2,
        0x1111_2222_3333_4444_5555_6666_7777_8888u128,
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm3,
        0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000u128,
    );
    sse2::movsd(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm2),
        XmmOperand::Reg(XmmReg::Xmm3),
    )
    .unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm2) >> 64, 0x1111_2222_3333_4444u128);
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm2) as u64,
        xmm(&cpu, XmmReg::Xmm3) as u64
    );

    // Memory source/dest.
    bus.write_u32(16, 0x3f80_0000); // 1.0f32
    set_xmm(
        &mut cpu,
        XmmReg::Xmm4,
        0xffff_eeee_dddd_cccc_bbbb_aaaa_9999_8888u128,
    );
    sse::movss(
        &mut cpu,
        &mut bus,
        XmmOperand::Reg(XmmReg::Xmm4),
        XmmOperand::Mem(16),
    )
    .unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm4) as u32, 0x3f80_0000);
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm4) & !0xFFFF_FFFFu128,
        0xffff_eeee_dddd_cccc_bbbb_aaaa_0000_0000u128
    );
}

#[test]
fn movd_and_movq_roundtrip() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    let rax = 0x1122_3344_5566_7788u64;
    sse2::movd_xmm_from_u32(&mut cpu, XmmReg::Xmm0, rax as u32);
    assert_eq!(xmm(&cpu, XmmReg::Xmm0), 0x5566_7788u128);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        0xdead_beef_cafe_babe_0123_4567_89ab_cdefu128,
    );
    assert_eq!(sse2::movd_u32_from_xmm(&cpu, XmmReg::Xmm1), 0x89ab_cdef);
    sse2::movd_mem_from_xmm(&cpu, &mut bus, 32, XmmReg::Xmm1);
    assert_eq!(bus.read_u32(32), 0x89ab_cdef);

    let rcx = 0x0102_0304_0506_0708u64;
    sse2::movq_xmm_from_u64(&mut cpu, XmmReg::Xmm2, rcx);
    assert_eq!(xmm(&cpu, XmmReg::Xmm2), 0x0102_0304_0506_0708u128);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm3,
        0xaaaa_bbbb_cccc_dddd_eeee_ffff_1111_2222u128,
    );
    assert_eq!(
        sse2::movq_u64_from_xmm(&cpu, XmmReg::Xmm3),
        0xeeee_ffff_1111_2222
    );
    sse2::movq_mem_from_xmm(&cpu, &mut bus, 40, XmmReg::Xmm3);
    assert_eq!(bus.read_u64(40), 0xeeee_ffff_1111_2222);
}

#[test]
fn movhlps_movlhps_and_unpcklps_unpckhps() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm0,
        u128_from_u64x2([0xaaaabbbbccccdddd, 0x1111222233334444]),
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        u128_from_u64x2([0x5555666677778888, 0x9999aaaabbbbcccc]),
    );

    sse::movhlps(&mut cpu, XmmReg::Xmm0, XmmReg::Xmm1);
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0),
        u128_from_u64x2([0x9999aaaabbbbcccc, 0x1111222233334444])
    );

    sse::movlhps(&mut cpu, XmmReg::Xmm0, XmmReg::Xmm1);
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0),
        u128_from_u64x2([0x9999aaaabbbbcccc, 0x5555666677778888])
    );

    set_xmm(&mut cpu, XmmReg::Xmm2, u128_from_u32x4([1, 2, 3, 4]));
    set_xmm(&mut cpu, XmmReg::Xmm3, u128_from_u32x4([10, 20, 30, 40]));
    sse::unpcklps(&mut cpu, &mut bus, XmmReg::Xmm2, XmmOperand::Reg(XmmReg::Xmm3)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm2), u128_from_u32x4([1, 10, 2, 20]));

    set_xmm(&mut cpu, XmmReg::Xmm4, u128_from_u32x4([1, 2, 3, 4]));
    set_xmm(&mut cpu, XmmReg::Xmm5, u128_from_u32x4([10, 20, 30, 40]));
    sse::unpckhps(&mut cpu, &mut bus, XmmReg::Xmm4, XmmOperand::Reg(XmmReg::Xmm5)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm4), u128_from_u32x4([3, 30, 4, 40]));
}

#[test]
fn shuffle_and_saturating_word_ops() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(&mut cpu, XmmReg::Xmm0, u128_from_u32x4([1, 2, 3, 4]));
    set_xmm(&mut cpu, XmmReg::Xmm1, u128_from_u32x4([10, 20, 30, 40]));
    sse::shufps(
        &mut cpu,
        &mut bus,
        XmmReg::Xmm0,
        XmmOperand::Reg(XmmReg::Xmm1),
        0x1B,
    )
    .unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm0), u128_from_u32x4([4, 3, 20, 10]));

    bus.write_u128(0, u128_from_u32x4([1, 2, 3, 4]));
    sse2::pshufd(&mut cpu, &mut bus, XmmReg::Xmm2, XmmOperand::Mem(0), 0x1B).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm2), u128_from_u32x4([4, 3, 2, 1]));

    // Signed-saturating word add.
    set_xmm(
        &mut cpu,
        XmmReg::Xmm3,
        u128_from_u16x8([0x7FFF, 0x8000, 1, 0, 0, 0, 0, 0]),
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm4,
        u128_from_u16x8([1, 0xFFFF, 0xFFFF, 2, 0, 0, 0, 0]),
    );
    sse2::paddsw(&mut cpu, &mut bus, XmmReg::Xmm3, XmmOperand::Reg(XmmReg::Xmm4)).unwrap();
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm3),
        u128_from_u16x8([0x7FFF, 0x8000, 0, 2, 0, 0, 0, 0])
    );

    // Unsigned-saturating word subtract.
    set_xmm(&mut cpu, XmmReg::Xmm5, u128_from_u16x8([1, 0xFFFF, 5, 0, 0, 0, 0, 0]));
    set_xmm(&mut cpu, XmmReg::Xmm6, u128_from_u16x8([2, 2, 10, 0, 0, 0, 0, 0]));
    sse2::psubusw(&mut cpu, &mut bus, XmmReg::Xmm5, XmmOperand::Reg(XmmReg::Xmm6)).unwrap();
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm5),
        u128_from_u16x8([0, 0xFFFD, 0, 0, 0, 0, 0, 0])
    );
}

#[test]
fn logical_ops_and_integer_arith() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm0,
        0x00ff_00ff_00ff_00ff_00ff_00ff_00ff_00ffu128,
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        0x0f0f_f0f0_aaaa_5555_ffff_0000_1234_5678u128,
    );
    sse::andps(&mut cpu, &mut bus, XmmReg::Xmm0, XmmOperand::Reg(XmmReg::Xmm1)).unwrap();
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm0),
        0x000f_00f0_00aa_0055_00ff_0000_0034_0078u128
    );

    set_xmm(
        &mut cpu,
        XmmReg::Xmm2,
        0xf0f0_f0f0_f0f0_f0f0_f0f0_f0f0_f0f0_f0f0u128,
    );
    bus.write_u128(0, 0x0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0fu128);
    sse2::pand(&mut cpu, &mut bus, XmmReg::Xmm2, XmmOperand::Mem(0)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm2), 0);

    set_xmm(&mut cpu, XmmReg::Xmm3, u128_from_u32x4([0xffff_fffe, 1, 0, 5]));
    set_xmm(&mut cpu, XmmReg::Xmm4, u128_from_u32x4([2, 2, 1, 10]));
    sse2::paddd(&mut cpu, &mut bus, XmmReg::Xmm3, XmmOperand::Reg(XmmReg::Xmm4)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm3), u128_from_u32x4([0, 3, 1, 15]));

    set_xmm(&mut cpu, XmmReg::Xmm5, u128_from_u64x2([u64::MAX, 1]));
    bus.write_u128(16, u128_from_u64x2([1, 2]));
    sse2::paddq(&mut cpu, &mut bus, XmmReg::Xmm5, XmmOperand::Mem(16)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm5), u128_from_u64x2([0, 3]));
}

#[test]
fn saturating_arith_and_shifts() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(&mut cpu, XmmReg::Xmm0, u128::from_le_bytes([127u8; 16]));
    set_xmm(&mut cpu, XmmReg::Xmm1, u128::from_le_bytes([1u8; 16]));
    sse2::paddsb(&mut cpu, &mut bus, XmmReg::Xmm0, XmmOperand::Reg(XmmReg::Xmm1)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm0), u128::from_le_bytes([127u8; 16]));

    set_xmm(&mut cpu, XmmReg::Xmm2, u128::from_le_bytes([250u8; 16]));
    set_xmm(&mut cpu, XmmReg::Xmm3, u128::from_le_bytes([10u8; 16]));
    sse2::paddusb(&mut cpu, &mut bus, XmmReg::Xmm2, XmmOperand::Reg(XmmReg::Xmm3)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm2), u128::from_le_bytes([255u8; 16]));

    set_xmm(&mut cpu, XmmReg::Xmm4, u128_from_u16x8([0x8000, 0x7fff, 0, 1, 2, 3, 4, 5]));
    sse2::psraw(&mut cpu, XmmReg::Xmm4, 15);
    assert_eq!(xmm(&cpu, XmmReg::Xmm4) as u16, 0xFFFF);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm5,
        u128_from_u64x2([0x1122_3344_5566_7788, 0x99aa_bbcc_ddee_ff00]),
    );
    sse2::pslldq(&mut cpu, XmmReg::Xmm5, 4);
    assert_eq!(
        xmm(&cpu, XmmReg::Xmm5),
        0xddee_ff00_1122_3344_5566_7788_0000_0000u128
    );
}

#[test]
fn comparisons_and_mul() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm0,
        u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        u128::from_le_bytes([1, 0, 3, 0, 5, 0, 7, 0, 9, 0, 11, 0, 13, 0, 15, 0]),
    );
    sse2::pcmpeqb(&mut cpu, &mut bus, XmmReg::Xmm0, XmmOperand::Reg(XmmReg::Xmm1)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm0) as u8, 0xFF);
    assert_eq!((xmm(&cpu, XmmReg::Xmm0) >> 8) as u8, 0);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm2,
        u128::from_le_bytes([0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm3,
        u128::from_le_bytes([0x7f, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
    );
    sse2::pcmpgtb(&mut cpu, &mut bus, XmmReg::Xmm2, XmmOperand::Reg(XmmReg::Xmm3)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm2) as u8, 0);

    set_xmm(&mut cpu, XmmReg::Xmm4, u128_from_u16x8([3, 4, 5, 6, 7, 8, 9, 10]));
    set_xmm(&mut cpu, XmmReg::Xmm5, u128_from_u16x8([2, 2, 2, 2, 2, 2, 2, 2]));
    sse2::pmullw(&mut cpu, &mut bus, XmmReg::Xmm4, XmmOperand::Reg(XmmReg::Xmm5)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm4), u128_from_u16x8([6, 8, 10, 12, 14, 16, 18, 20]));

    set_xmm(&mut cpu, XmmReg::Xmm6, u128_from_u32x4([3, 0, 4, 0]));
    set_xmm(&mut cpu, XmmReg::Xmm7, u128_from_u32x4([5, 0, 6, 0]));
    sse2::pmuludq(&mut cpu, &mut bus, XmmReg::Xmm6, XmmOperand::Reg(XmmReg::Xmm7)).unwrap();
    assert_eq!(xmm(&cpu, XmmReg::Xmm6), u128_from_u64x2([15, 24]));
}

#[test]
fn fp_scalar_and_conversions_with_rounding_modes() {
    let mut cpu = CpuState::default();
    let mut bus = RamBus::new(64);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm0,
        (1.0f32.to_bits() as u128) | (0xaaaa_bbbb_cccc_dddd_u128 << 32),
    );
    set_xmm(
        &mut cpu,
        XmmReg::Xmm1,
        (2.5f32.to_bits() as u128) | (0x1111_2222_3333_4444_u128 << 32),
    );
    sse::addss(&mut cpu, &mut bus, XmmReg::Xmm0, XmmOperand::Reg(XmmReg::Xmm1)).unwrap();
    assert_eq!(f32::from_bits(xmm(&cpu, XmmReg::Xmm0) as u32), 3.5);
    assert_eq!(xmm(&cpu, XmmReg::Xmm0) >> 32, 0xaaaa_bbbb_cccc_dddd_u128);

    set_xmm(
        &mut cpu,
        XmmReg::Xmm2,
        (1.0f64.to_bits() as u128) | (0x9999_8888_7777_6666_u128 << 64),
    );
    set_xmm(&mut cpu, XmmReg::Xmm3, 2.0f64.to_bits() as u128);
    sse2::mulsd(&mut cpu, &mut bus, XmmReg::Xmm2, XmmOperand::Reg(XmmReg::Xmm3)).unwrap();
    assert_eq!(f64::from_bits(xmm(&cpu, XmmReg::Xmm2) as u64), 2.0);
    assert_eq!(xmm(&cpu, XmmReg::Xmm2) >> 64, 0x9999_8888_7777_6666_u128);

    // Rounding mode affects CVT* conversions (not CVTT*).
    clear_exception_flags(&mut cpu.sse.mxcsr);
    set_rounding_mode(&mut cpu.sse.mxcsr, RoundingMode::Nearest);
    set_xmm(&mut cpu, XmmReg::Xmm4, 2.5f32.to_bits() as u128);
    assert_eq!(
        sse::cvtss2si32(&mut cpu, &mut bus, XmmOperand::Reg(XmmReg::Xmm4)),
        2
    ); // ties-to-even
    assert_ne!(cpu.sse.mxcsr & MXCSR_PE, 0);

    clear_exception_flags(&mut cpu.sse.mxcsr);
    set_rounding_mode(&mut cpu.sse.mxcsr, RoundingMode::Down);
    set_xmm(&mut cpu, XmmReg::Xmm4, 1.9f32.to_bits() as u128);
    assert_eq!(
        sse::cvtss2si32(&mut cpu, &mut bus, XmmOperand::Reg(XmmReg::Xmm4)),
        1
    );

    clear_exception_flags(&mut cpu.sse.mxcsr);
    set_rounding_mode(&mut cpu.sse.mxcsr, RoundingMode::Up);
    set_xmm(&mut cpu, XmmReg::Xmm4, (-1.1f32).to_bits() as u128);
    assert_eq!(
        sse::cvtss2si32(&mut cpu, &mut bus, XmmOperand::Reg(XmmReg::Xmm4)),
        -1
    );

    clear_exception_flags(&mut cpu.sse.mxcsr);
    set_rounding_mode(&mut cpu.sse.mxcsr, RoundingMode::Up);
    set_xmm(&mut cpu, XmmReg::Xmm4, 1.9f32.to_bits() as u128);
    assert_eq!(
        sse::cvttss2si32(&mut cpu, &mut bus, XmmOperand::Reg(XmmReg::Xmm4)),
        1
    ); // truncates

    clear_exception_flags(&mut cpu.sse.mxcsr);
    set_rounding_mode(&mut cpu.sse.mxcsr, RoundingMode::Nearest);
    set_xmm(&mut cpu, XmmReg::Xmm5, f64::NAN.to_bits() as u128);
    assert_eq!(
        sse2::cvtsd2si32(&mut cpu, &mut bus, XmmOperand::Reg(XmmReg::Xmm5)),
        i32::MIN
    );
    assert_ne!(cpu.sse.mxcsr & MXCSR_IE, 0);
}
