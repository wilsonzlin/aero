use aero_cpu_core::cpuid::bits;
use aero_cpu_core::interp::tier0::exec::step_with_config;
use aero_cpu_core::interp::tier0::Tier0Config;
use aero_cpu_core::mem::FlatTestBus;
use aero_cpu_core::state::{CpuMode, CpuState, CR4_OSFXSR, FLAG_CF, FLAG_OF, FLAG_SF, FLAG_ZF};
use aero_cpu_core::Exception;
use aero_x86::Register;

const BUS_SIZE: usize = 0x4000;
const CODE_BASE: u64 = 0x1000;

fn crc32c_sw(mut crc: u32, bytes: &[u8]) -> u32 {
    const POLY: u32 = 0x82F63B78;
    for &b in bytes {
        crc ^= b as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ POLY;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

fn xmm_from_bytes(bytes: [u8; 16]) -> u128 {
    u128::from_le_bytes(bytes)
}

fn xmm_to_bytes(xmm: u128) -> [u8; 16] {
    xmm.to_le_bytes()
}

fn xmm_from_i32x4(lanes: [i32; 4]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in lanes.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn xmm_to_i32x4(xmm: u128) -> [i32; 4] {
    let bytes = xmm.to_le_bytes();
    let mut out = [0i32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = i32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
    }
    out
}

fn xmm_from_i16x8(lanes: [i16; 8]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in lanes.into_iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&lane.to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn xmm_to_i16x8(xmm: u128) -> [i16; 8] {
    let bytes = xmm.to_le_bytes();
    let mut out = [0i16; 8];
    for (i, chunk) in bytes.chunks_exact(2).enumerate() {
        out[i] = i16::from_le_bytes([chunk[0], chunk[1]]);
    }
    out
}

fn xmm_from_f32x4(lanes: [f32; 4]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in lanes.into_iter().enumerate() {
        out[i * 4..i * 4 + 4].copy_from_slice(&lane.to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn xmm_to_f32x4(xmm: u128) -> [f32; 4] {
    let bytes = xmm.to_le_bytes();
    let mut out = [0f32; 4];
    for (i, chunk) in bytes.chunks_exact(4).enumerate() {
        out[i] = f32::from_bits(u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    out
}

fn xmm_from_f64x2(lanes: [f64; 2]) -> u128 {
    let mut out = [0u8; 16];
    for (i, lane) in lanes.into_iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&lane.to_bits().to_le_bytes());
    }
    u128::from_le_bytes(out)
}

fn xmm_to_f64x2(xmm: u128) -> [f64; 2] {
    let bytes = xmm.to_le_bytes();
    let mut out = [0f64; 2];
    for (i, chunk) in bytes.chunks_exact(8).enumerate() {
        out[i] = f64::from_bits(u64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]));
    }
    out
}

fn xmm_to_i64x2(xmm: u128) -> [i64; 2] {
    let bytes = xmm.to_le_bytes();
    let mut out = [0i64; 2];
    for (i, chunk) in bytes.chunks_exact(8).enumerate() {
        out[i] = i64::from_le_bytes([
            chunk[0], chunk[1], chunk[2], chunk[3], chunk[4], chunk[5], chunk[6], chunk[7],
        ]);
    }
    out
}

fn new_state(mode: CpuMode) -> CpuState {
    let mut state = CpuState::new(mode);
    state.control.cr4 |= CR4_OSFXSR;
    state
}

fn cfg_with_ext_bits() -> Tier0Config {
    let mut cfg = Tier0Config::default();
    cfg.features.leaf1_ecx |= bits::LEAF1_ECX_SSE3
        | bits::LEAF1_ECX_SSSE3
        | bits::LEAF1_ECX_SSE41
        | bits::LEAF1_ECX_SSE42
        | bits::LEAF1_ECX_POPCNT;
    cfg
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
fn pshufb_shuffle() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    state.sse.xmm[1] = xmm_from_bytes([0x80, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);

    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x00, 0xC1]).unwrap(); // pshufb xmm0,xmm1

    assert_eq!(
        xmm_to_bytes(state.sse.xmm[0]),
        [0, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]
    );
}

#[test]
fn pshufb_memory_operand_protected32() {
    let mut cfg = Tier0Config::default();
    cfg.features.leaf1_ecx |= bits::LEAF1_ECX_SSSE3;

    let mut state = new_state(CpuMode::Bit32);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);

    let mask_addr = 0x200u64;
    state.write_reg(Register::EAX, mask_addr);
    let mask = [0x80, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    bus.load(mask_addr, &mask);

    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x00, 0x00]).unwrap(); // pshufb xmm0,[eax]

    assert_eq!(
        xmm_to_bytes(state.sse.xmm[0]),
        [0, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]
    );
}

#[test]
fn lddqu_memory_operand_protected32() {
    let mut cfg = Tier0Config::default();
    cfg.features.leaf1_ecx |= bits::LEAF1_ECX_SSE3;

    let mut state = new_state(CpuMode::Bit32);
    let mut bus = FlatTestBus::new(0x10_000);

    let addr = 0x400u64;
    state.write_reg(Register::EAX, addr);
    let expected = [0xA5u8; 16];
    bus.load(addr, &expected);

    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0xF0, 0x00]).unwrap(); // lddqu xmm0,[eax]

    assert_eq!(xmm_to_bytes(state.sse.xmm[0]), expected);
}

#[test]
fn pshufb_real16_segment_override() {
    let mut cfg = Tier0Config::default();
    cfg.features.leaf1_ecx |= bits::LEAF1_ECX_SSSE3;

    let mut state = new_state(CpuMode::Bit16);
    let mut bus = FlatTestBus::new(0x20_000);

    state.sse.xmm[0] = xmm_from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);

    // Addressing form [BX+SI] with an ES override.
    state.write_reg(Register::BX, 0x0010);
    state.write_reg(Register::SI, 0x0020);
    state.segments.es.base = 0x1000;
    let addr = state.segments.es.base + 0x30;

    let mask = [0x80, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0];
    bus.load(addr, &mask);

    exec_once(
        &cfg,
        &mut state,
        &mut bus,
        &[0x26, 0x66, 0x0F, 0x38, 0x00, 0x00],
    )
    .unwrap(); // pshufb xmm0,es:[bx+si]

    assert_eq!(
        xmm_to_bytes(state.sse.xmm[0]),
        [0, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]
    );
}

#[test]
fn haddps_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_f32x4([1.0, 2.0, 3.0, 4.0]);
    state.sse.xmm[1] = xmm_from_f32x4([10.0, 20.0, 30.0, 40.0]);

    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x7C, 0xC1]).unwrap(); // haddps xmm0,xmm1

    assert_eq!(xmm_to_f32x4(state.sse.xmm[0]), [3.0, 7.0, 30.0, 70.0]);
}

#[test]
fn haddpd_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_f64x2([1.0, 2.0]);
    state.sse.xmm[1] = xmm_from_f64x2([10.0, 20.0]);

    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x7C, 0xC1]).unwrap(); // haddpd xmm0,xmm1

    assert_eq!(xmm_to_f64x2(state.sse.xmm[0]), [3.0, 30.0]);
}

#[test]
fn hsubps_and_hsubpd_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_f32x4([1.0, 2.0, 3.0, 4.0]);
    state.sse.xmm[1] = xmm_from_f32x4([10.0, 20.0, 30.0, 40.0]);
    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x7D, 0xC1]).unwrap(); // hsubps xmm0,xmm1
    assert_eq!(xmm_to_f32x4(state.sse.xmm[0]), [-1.0, -1.0, -10.0, -10.0]);

    state.sse.xmm[0] = xmm_from_f64x2([1.0, 2.0]);
    state.sse.xmm[1] = xmm_from_f64x2([10.0, 20.0]);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x7D, 0xC1]).unwrap(); // hsubpd xmm0,xmm1
    assert_eq!(xmm_to_f64x2(state.sse.xmm[0]), [-1.0, -10.0]);
}

#[test]
fn movdup_variants() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    let low = 0x1122_3344_5566_7788u64;
    state.sse.xmm[1] = (low as u128) | ((0xdead_beef_dead_beefu64 as u128) << 64);
    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x12, 0xC1]).unwrap(); // movddup xmm0,xmm1
    assert_eq!(state.sse.xmm[0], (low as u128) | ((low as u128) << 64));

    state.sse.xmm[1] = xmm_from_f32x4([1.0, 2.0, 3.0, 4.0]);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x12, 0xC1]).unwrap(); // movsldup xmm0,xmm1
    assert_eq!(xmm_to_f32x4(state.sse.xmm[0]), [1.0, 1.0, 3.0, 3.0]);

    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0x16, 0xC1]).unwrap(); // movshdup xmm0,xmm1
    assert_eq!(xmm_to_f32x4(state.sse.xmm[0]), [2.0, 2.0, 4.0, 4.0]);
}

#[test]
fn phaddw_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_i16x8([1, 2, 3, 4, 5, 6, 7, 8]);
    state.sse.xmm[1] = xmm_from_i16x8([10, 20, 30, 40, 50, 60, 70, 80]);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x01, 0xC1]).unwrap(); // phaddw xmm0,xmm1
    assert_eq!(
        xmm_to_i16x8(state.sse.xmm[0]),
        [3, 7, 11, 15, 30, 70, 110, 150]
    );
}

#[test]
fn pabsb_and_palignr_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[1] = xmm_from_bytes([
        0x00, 0x01, 0xFF, 0x80, 0x7F, 0x10, 0xF0, 0xAA, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x1C, 0xC1]).unwrap(); // pabsb xmm0,xmm1
    assert_eq!(
        xmm_to_bytes(state.sse.xmm[0]),
        [0x00, 0x01, 0x01, 0x80, 0x7F, 0x10, 0x10, 0x56, 0, 0, 0, 0, 0, 0, 0, 0]
    );

    let mut dst = [0u8; 16];
    let mut src = [0u8; 16];
    for i in 0..16 {
        dst[i] = i as u8;
        src[i] = 16 + i as u8;
    }
    state.sse.xmm[0] = xmm_from_bytes(dst);
    state.sse.xmm[1] = xmm_from_bytes(src);
    exec_once(
        &cfg,
        &mut state,
        &mut bus,
        &[0x66, 0x0F, 0x3A, 0x0F, 0xC1, 0x04],
    )
    .unwrap(); // palignr xmm0,xmm1,4
    assert_eq!(
        xmm_to_bytes(state.sse.xmm[0]),
        [4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19]
    );
}

#[test]
fn pblendw_and_ptest() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_i16x8([0, 1, 2, 3, 4, 5, 6, 7]);
    state.sse.xmm[1] = xmm_from_i16x8([10, 11, 12, 13, 14, 15, 16, 17]);
    exec_once(
        &cfg,
        &mut state,
        &mut bus,
        &[0x66, 0x0F, 0x3A, 0x0E, 0xC1, 0xAA],
    )
    .unwrap(); // pblendw xmm0,xmm1,0xAA
    assert_eq!(xmm_to_i16x8(state.sse.xmm[0]), [0, 11, 2, 13, 4, 15, 6, 17]);

    state.sse.xmm[0] = 0;
    state.sse.xmm[1] = 1;
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x17, 0xC1]).unwrap(); // ptest xmm0,xmm1
    assert!(state.get_flag(FLAG_ZF));
    assert!(!state.get_flag(FLAG_CF));
    assert!(!state.get_flag(FLAG_OF));
    assert!(!state.get_flag(FLAG_SF));
}

#[test]
fn pmovsxbw_and_pmovsxbq_memory_operand() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[1] = xmm_from_bytes([
        0x01, 0x80, 0x7F, 0xFF, 0x00, 0x10, 0xF0, 0xAA, 0, 0, 0, 0, 0, 0, 0, 0,
    ]);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x20, 0xC1]).unwrap(); // pmovsxbw xmm0,xmm1
    assert_eq!(
        xmm_to_i16x8(state.sse.xmm[0]),
        [1, -128, 127, -1, 0, 16, -16, -86]
    );

    // pmovsxbq uses a 2-byte memory source; ensure Tier-0 doesn't over-read past the end of RAM.
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(CODE_BASE as usize + 0x20);
    let addr = CODE_BASE + 0x1E;
    state.write_reg(Register::RAX, addr);
    bus.load(addr, &[0x80, 0x7F]);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x22, 0x00]).unwrap(); // pmovsxbq xmm0,[rax]
    assert_eq!(xmm_to_i64x2(state.sse.xmm[0]), [-128, 127]);
}

#[test]
fn insertps_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);

    state.sse.xmm[0] = xmm_from_f32x4([1.0, 2.0, 3.0, 4.0]);
    state.sse.xmm[1] = xmm_from_f32x4([10.0, 20.0, 30.0, 40.0]);

    // insertps xmm0, xmm1, 0x61
    exec_once(
        &cfg,
        &mut state,
        &mut bus,
        &[0x66, 0x0F, 0x3A, 0x21, 0xC1, 0x61],
    )
    .unwrap();

    assert_eq!(xmm_to_f32x4(state.sse.xmm[0]), [0.0, 2.0, 20.0, 4.0]);
}

#[test]
fn ud_when_disabled() {
    let mut cfg = cfg_with_ext_bits();
    cfg.features.leaf1_ecx &= !bits::LEAF1_ECX_SSSE3;

    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(0x10_000);
    state.sse.xmm[0] = 0;
    state.sse.xmm[1] = 0;

    let err = exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x00, 0xC1]).unwrap_err();
    assert_eq!(err, Exception::InvalidOpcode);
}

#[test]
fn popcnt_widths() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // 16-bit: popcnt ax, cx
    state.write_reg(Register::RAX, 0xFFFF_0000_0000_0000);
    state.write_reg(Register::RCX, 0b1011_0001_0000_1111);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0xF3, 0x0F, 0xB8, 0xC1]).unwrap();
    assert_eq!(state.read_reg(Register::RAX), 0xFFFF_0000_0000_0000 | 8);

    // 32-bit: popcnt eax, ecx (zero-extends into rax in long mode)
    state.write_reg(Register::RAX, 0xFFFF_FFFF_FFFF_FFFF);
    state.write_reg(Register::RCX, 0xFFFF_0000_F0F0_F0F0);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0xB8, 0xC1]).unwrap();
    assert_eq!(
        state.read_reg(Register::RAX),
        (0xF0F0_F0F0u32.count_ones()) as u64
    );

    // 64-bit: popcnt rax, rcx
    state.write_reg(Register::RAX, 0);
    state.write_reg(Register::RCX, 0x8000_0000_0000_0001);
    exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x48, 0x0F, 0xB8, 0xC1]).unwrap();
    assert_eq!(state.read_reg(Register::RAX), 2);
}

#[test]
fn crc32_vectors() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    // CRC32 over "123456789" using crc32 eax, cl.
    let seed = 0xFFFF_FFFFu32;
    state.write_reg(Register::EAX, seed as u64);
    for &b in b"123456789" {
        state.write_reg(Register::CL, b as u64);
        exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x38, 0xF0, 0xC1]).unwrap();
    }
    let expected = crc32c_sw(seed, b"123456789");
    assert_eq!(state.read_reg(Register::EAX) as u32, expected);

    // CRC32 eax, ecx (dword) processes little-endian bytes of the source.
    state.write_reg(Register::EAX, 0);
    state.write_reg(Register::ECX, 0x1234_5678);
    exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x38, 0xF1, 0xC1]).unwrap();
    assert_eq!(
        state.read_reg(Register::EAX) as u32,
        crc32c_sw(0, &0x1234_5678u32.to_le_bytes())
    );

    // CRC32 rax, rcx (qword) zero-extends the 32-bit CRC result.
    state.write_reg(Register::RAX, 0xFFFF_FFFF_FFFF_FFFF);
    state.write_reg(Register::RCX, 0x0102_0304_0506_0708);
    exec_once(
        &cfg,
        &mut state,
        &mut bus,
        &[0xF2, 0x48, 0x0F, 0x38, 0xF1, 0xC1],
    )
    .unwrap();
    let expected64 = crc32c_sw(0xFFFF_FFFF, &0x0102_0304_0506_0708u64.to_le_bytes());
    assert_eq!(state.read_reg(Register::RAX), expected64 as u64);
}

#[test]
fn pmulld_basic() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    state.sse.xmm[0] = xmm_from_i32x4([2, -3, 4, 0x4000_0000]);
    state.sse.xmm[1] = xmm_from_i32x4([5, 7, -8, 4]);
    exec_once(&cfg, &mut state, &mut bus, &[0x66, 0x0F, 0x38, 0x40, 0xC1]).unwrap();
    assert_eq!(xmm_to_i32x4(state.sse.xmm[0]), [10, -21, -32, 0x0000_0000]);
}

#[test]
fn pcmpestri_finds_nul() {
    let cfg = cfg_with_ext_bits();
    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);

    let mut chunk = [0u8; 16];
    chunk[..5].copy_from_slice(b"he\0lo");
    state.sse.xmm[0] = 0;
    state.sse.xmm[1] = xmm_from_bytes(chunk);

    state.write_reg(Register::EAX, 16);
    state.write_reg(Register::EDX, 16);
    exec_once(
        &cfg,
        &mut state,
        &mut bus,
        &[0x66, 0x0F, 0x3A, 0x61, 0xC1, 0x08],
    )
    .unwrap();
    assert_eq!(state.read_reg(Register::ECX) as u32, 2);
}

#[test]
fn scalar_ud_when_disabled() {
    let mut cfg = cfg_with_ext_bits();
    cfg.features.leaf1_ecx &= !(bits::LEAF1_ECX_POPCNT | bits::LEAF1_ECX_SSE42);

    let mut state = new_state(CpuMode::Bit64);
    let mut bus = FlatTestBus::new(BUS_SIZE);
    state.write_reg(Register::RAX, 0);
    state.write_reg(Register::RCX, 123);

    assert_eq!(
        exec_once(&cfg, &mut state, &mut bus, &[0xF3, 0x0F, 0xB8, 0xC1]).unwrap_err(),
        Exception::InvalidOpcode
    );
    assert_eq!(
        exec_once(&cfg, &mut state, &mut bus, &[0xF2, 0x0F, 0x38, 0xF0, 0xC1]).unwrap_err(),
        Exception::InvalidOpcode
    );
}
