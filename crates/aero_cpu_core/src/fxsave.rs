use crate::fpu::{canonicalize_st, FpuState};
use crate::sse_state::{SseState, MXCSR_MASK};
use crate::{FxStateError, FXSAVE_AREA_SIZE};

pub fn stmxcsr(sse: &SseState, dst: &mut [u8; 4]) {
    dst.copy_from_slice(&sse.mxcsr.to_le_bytes());
}

pub fn ldmxcsr(sse: &mut SseState, src: &[u8; 4]) -> Result<(), FxStateError> {
    sse.set_mxcsr(u32::from_le_bytes(*src))
}

/// Implements the legacy (32-bit) `FXSAVE m512byte` memory image.
pub fn fxsave_legacy(fpu: &FpuState, sse: &SseState, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
    let mut out = [0u8; FXSAVE_AREA_SIZE];

    // 0x00..0x20: x87 environment + MXCSR.
    out[0..2].copy_from_slice(&fpu.fcw.to_le_bytes());

    let fsw = fpu.fsw_with_top();
    out[2..4].copy_from_slice(&fsw.to_le_bytes());

    out[4] = fpu.ftw as u8;
    // out[5] reserved.
    out[6..8].copy_from_slice(&fpu.fop.to_le_bytes());

    out[8..12].copy_from_slice(&(fpu.fip as u32).to_le_bytes());
    out[12..14].copy_from_slice(&fpu.fcs.to_le_bytes());
    // out[14..16] reserved.

    out[16..20].copy_from_slice(&(fpu.fdp as u32).to_le_bytes());
    out[20..22].copy_from_slice(&fpu.fds.to_le_bytes());
    // out[22..24] reserved.

    out[24..28].copy_from_slice(&sse.mxcsr.to_le_bytes());
    out[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

    // 0x20..0xA0: ST/MM register image.
    for (i, reg) in fpu.st.iter().enumerate() {
        let start = 32 + i * 16;
        out[start..start + 16].copy_from_slice(&canonicalize_st(*reg).to_le_bytes());
    }

    // 0xA0..0x120: XMM0-7 register image.
    for i in 0..8 {
        let start = 160 + i * 16;
        out[start..start + 16].copy_from_slice(&sse.xmm[i].to_le_bytes());
    }

    *dst = out;
}

/// Implements the 64-bit `FXSAVE64 m512byte` memory image.
pub fn fxsave64(fpu: &FpuState, sse: &SseState, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
    let mut out = [0u8; FXSAVE_AREA_SIZE];

    out[0..2].copy_from_slice(&fpu.fcw.to_le_bytes());

    let fsw = fpu.fsw_with_top();
    out[2..4].copy_from_slice(&fsw.to_le_bytes());

    out[4] = fpu.ftw as u8;
    out[6..8].copy_from_slice(&fpu.fop.to_le_bytes());

    out[8..16].copy_from_slice(&fpu.fip.to_le_bytes()); // RIP
    out[16..24].copy_from_slice(&fpu.fdp.to_le_bytes()); // RDP

    out[24..28].copy_from_slice(&sse.mxcsr.to_le_bytes());
    out[28..32].copy_from_slice(&MXCSR_MASK.to_le_bytes());

    for (i, reg) in fpu.st.iter().enumerate() {
        let start = 32 + i * 16;
        out[start..start + 16].copy_from_slice(&canonicalize_st(*reg).to_le_bytes());
    }

    // 16 XMM registers in 64-bit mode.
    for i in 0..16 {
        let start = 160 + i * 16;
        out[start..start + 16].copy_from_slice(&sse.xmm[i].to_le_bytes());
    }

    *dst = out;
}

/// Implements the legacy (32-bit) `FXRSTOR m512byte` memory image.
pub fn fxrstor_legacy(
    fpu: &mut FpuState,
    sse: &mut SseState,
    src: &[u8; FXSAVE_AREA_SIZE],
) -> Result<(), FxStateError> {
    // Intel SDM: if MXCSR is invalid (reserved bits set), `FXRSTOR` raises
    // `#GP(0)` and *does not* restore any state. We model that by validating
    // MXCSR before committing changes to `fpu`/`sse`.
    let mxcsr = read_u32(src, 24);
    let mut new_sse = *sse;
    // `MXCSR_MASK` is a CPU capability and is ignored by `FXRSTOR` on real
    // hardware, but the *value* must still be validated.
    new_sse.set_mxcsr(mxcsr)?;

    let fsw_raw = read_u16(src, 2);
    let top = ((fsw_raw >> 11) & 0b111) as u8;
    let fsw = fsw_raw & !(0b111 << 11);
    let mut new_fpu = fpu.clone();
    new_fpu.fcw = read_u16(src, 0);
    new_fpu.fsw = fsw;
    new_fpu.top = top;
    new_fpu.ftw = src[4] as u16;
    new_fpu.fop = read_u16(src, 6);
    new_fpu.fip = read_u32(src, 8) as u64;
    new_fpu.fcs = read_u16(src, 12);
    new_fpu.fdp = read_u32(src, 16) as u64;
    new_fpu.fds = read_u16(src, 20);

    for i in 0..8 {
        let start = 32 + i * 16;
        new_fpu.st[i] = canonicalize_st(read_u128(src, start));
    }

    for i in 0..8 {
        let start = 160 + i * 16;
        new_sse.xmm[i] = read_u128(src, start);
    }

    *fpu = new_fpu;
    *sse = new_sse;
    Ok(())
}

/// Implements the 64-bit `FXRSTOR64 m512byte` memory image.
pub fn fxrstor64(
    fpu: &mut FpuState,
    sse: &mut SseState,
    src: &[u8; FXSAVE_AREA_SIZE],
) -> Result<(), FxStateError> {
    let mxcsr = read_u32(src, 24);
    let mut new_sse = *sse;
    new_sse.set_mxcsr(mxcsr)?;

    let fsw_raw = read_u16(src, 2);
    let top = ((fsw_raw >> 11) & 0b111) as u8;
    let fsw = fsw_raw & !(0b111 << 11);
    let mut new_fpu = fpu.clone();
    new_fpu.fcw = read_u16(src, 0);
    new_fpu.fsw = fsw;
    new_fpu.top = top;
    new_fpu.ftw = src[4] as u16;
    new_fpu.fop = read_u16(src, 6);
    new_fpu.fip = read_u64(src, 8);
    new_fpu.fdp = read_u64(src, 16);

    for i in 0..8 {
        let start = 32 + i * 16;
        new_fpu.st[i] = canonicalize_st(read_u128(src, start));
    }

    for i in 0..16 {
        let start = 160 + i * 16;
        new_sse.xmm[i] = read_u128(src, start);
    }

    *fpu = new_fpu;
    *sse = new_sse;
    Ok(())
}

fn read_u16(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u16 {
    u16::from_le_bytes(src[offset..offset + 2].try_into().unwrap())
}

fn read_u32(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u32 {
    u32::from_le_bytes(src[offset..offset + 4].try_into().unwrap())
}

fn read_u64(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u64 {
    u64::from_le_bytes(src[offset..offset + 8].try_into().unwrap())
}

fn read_u128(src: &[u8; FXSAVE_AREA_SIZE], offset: usize) -> u128 {
    u128::from_le_bytes(src[offset..offset + 16].try_into().unwrap())
}

