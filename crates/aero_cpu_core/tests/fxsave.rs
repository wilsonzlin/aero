use aero_cpu_core::{sse_state::MXCSR_MASK, CpuState, FXSAVE_AREA_SIZE};

const ST80_MASK: u128 = (1u128 << 80) - 1;

fn patterned_u128(seed: u8) -> u128 {
    let mut bytes = [0u8; 16];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = seed.wrapping_add(i as u8);
    }
    u128::from_le_bytes(bytes)
}

fn patterned_st80(seed: u8) -> u128 {
    let mut bytes = [0u8; 16];
    for i in 0..10 {
        bytes[i] = seed.wrapping_add(i as u8);
    }
    u128::from_le_bytes(bytes)
}

fn mask_st80(raw: u128) -> u128 {
    raw & ST80_MASK
}

#[test]
fn fxsave_legacy_layout_matches_intel_sdm() {
    let mut cpu = CpuState::default();
    cpu.fpu.fcw = 0x1234;
    cpu.fpu.fsw = 0x4567;
    cpu.fpu.top = 3;
    cpu.fpu.ftw = 0x9A;
    cpu.fpu.fop = 0xBEEF;
    cpu.fpu.fip = 0x1122_3344;
    cpu.fpu.fcs = 0x5566;
    cpu.fpu.fdp = 0x7788_99AA;
    cpu.fpu.fds = 0xBBCC;

    cpu.sse.mxcsr = 0xAABB_CCDD;

    for i in 0..8 {
        // Fill the reserved bytes to ensure FXSAVE zeroes them in the output.
        cpu.fpu.st[i] = patterned_u128(0x10 + i as u8);
    }

    for i in 0..16 {
        cpu.sse.xmm[i] = patterned_u128(0x80 + i as u8);
    }

    let mut image = [0u8; FXSAVE_AREA_SIZE];
    cpu.fxsave(&mut image);

    let mut expected = [0u8; FXSAVE_AREA_SIZE];
    expected[0..2].copy_from_slice(&0x1234u16.to_le_bytes());
    expected[2..4].copy_from_slice(&(cpu.fpu.fsw_with_top()).to_le_bytes());
    expected[4] = 0x9A;
    expected[6..8].copy_from_slice(&0xBEEFu16.to_le_bytes());
    expected[8..12].copy_from_slice(&(0x1122_3344u32).to_le_bytes());
    expected[12..14].copy_from_slice(&0x5566u16.to_le_bytes());
    expected[16..20].copy_from_slice(&(0x7788_99AAu32).to_le_bytes());
    expected[20..22].copy_from_slice(&0xBBCCu16.to_le_bytes());
    expected[24..28].copy_from_slice(&0xAABB_CCDDu32.to_le_bytes());
    expected[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

    for i in 0..8 {
        let start = 32 + i * 16;
        expected[start..start + 16].copy_from_slice(&patterned_st80(0x10 + i as u8).to_le_bytes());
    }

    for i in 0..8 {
        let start = 160 + i * 16;
        expected[start..start + 16].copy_from_slice(&cpu.sse.xmm[i].to_le_bytes());
    }

    assert_eq!(image, expected);
}

#[test]
fn fxrstor_legacy_restores_state() {
    let mut image = [0u8; FXSAVE_AREA_SIZE];

    // Fill in a recognisable state.
    image[0..2].copy_from_slice(&0x1111u16.to_le_bytes());
    image[2..4].copy_from_slice(&0x2222u16.to_le_bytes());
    image[4] = 0xFE;
    image[6..8].copy_from_slice(&0x3333u16.to_le_bytes());
    image[8..12].copy_from_slice(&0x4444_5555u32.to_le_bytes());
    image[12..14].copy_from_slice(&0x6666u16.to_le_bytes());
    image[16..20].copy_from_slice(&0x7777_8888u32.to_le_bytes());
    image[20..22].copy_from_slice(&0x9999u16.to_le_bytes());
    image[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());
    image[28..32].copy_from_slice(&0xFFFFu32.to_le_bytes());

    for i in 0..8 {
        let start = 32 + i * 16;
        image[start..start + 16].copy_from_slice(&patterned_u128(0x20 + i as u8).to_le_bytes());
    }

    for i in 0..8 {
        let start = 160 + i * 16;
        image[start..start + 16].copy_from_slice(&patterned_u128(0xA0 + i as u8).to_le_bytes());
    }

    let mut cpu = CpuState::default();
    cpu.fxrstor(&image).unwrap();

    assert_eq!(cpu.fpu.fcw, 0x1111);
    assert_eq!(cpu.fpu.fsw, 0x2222);
    assert_eq!(cpu.fpu.top, ((0x2222u16 >> 11) & 0b111) as u8);
    assert_eq!(cpu.fpu.ftw, 0xFE);
    assert_eq!(cpu.fpu.fop, 0x3333);
    assert_eq!(cpu.fpu.fip, 0x4444_5555);
    assert_eq!(cpu.fpu.fcs, 0x6666);
    assert_eq!(cpu.fpu.fdp, 0x7777_8888);
    assert_eq!(cpu.fpu.fds, 0x9999);
    assert_eq!(cpu.sse.mxcsr, 0x1F80);

    for i in 0..8 {
        assert_eq!(cpu.fpu.st[i], mask_st80(patterned_u128(0x20 + i as u8)));
        assert_eq!(cpu.sse.xmm[i], patterned_u128(0xA0 + i as u8));
    }
}

#[test]
fn fxsave64_roundtrip_restores_state() {
    let mut original = CpuState::default();
    original.fpu.fcw = 0xABCD;
    original.fpu.fsw = 0x7777;
    original.fpu.top = 7;
    original.fpu.ftw = 0x55;
    original.fpu.fop = 0x0BAD;
    original.fpu.fip = 0x1122_3344_5566_7788;
    original.fpu.fdp = 0x99AA_BBCC_DDEE_FF00;

    original.sse.mxcsr = 0x1F80;

    for i in 0..8 {
        original.fpu.st[i] = patterned_st80(0x40 + i as u8);
    }
    for i in 0..16 {
        original.sse.xmm[i] = patterned_u128(0xC0 + i as u8);
    }

    let mut image = [0u8; FXSAVE_AREA_SIZE];
    original.fxsave64(&mut image);

    let mut restored = CpuState::default();
    // Ensure we're not accidentally passing due to defaults.
    restored.fpu.fcw = 0;
    restored.fpu.fsw = 0;
    restored.fpu.ftw = 0;
    restored.fpu.fop = 0;
    restored.fpu.fip = 0;
    restored.fpu.fdp = 0;
    restored.sse.mxcsr = 0;
    restored.fpu.st = [0u128; 8];
    restored.sse.xmm = [0u128; 16];

    restored.fxrstor64(&image).unwrap();

    assert_eq!(restored.fpu.fcw, original.fpu.fcw);
    assert_eq!(restored.fpu.fsw, original.fpu.fsw_with_top());
    assert_eq!(restored.fpu.top, original.fpu.top);
    assert_eq!(restored.fpu.ftw, original.fpu.ftw);
    assert_eq!(restored.fpu.fop, original.fpu.fop);
    assert_eq!(restored.fpu.fip, original.fpu.fip);
    assert_eq!(restored.fpu.fdp, original.fpu.fdp);
    assert_eq!(restored.sse.mxcsr, original.sse.mxcsr);

    assert_eq!(restored.fpu.st, original.fpu.st);
    assert_eq!(restored.sse.xmm, original.sse.xmm);
}
