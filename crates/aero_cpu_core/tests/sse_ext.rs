use aero_cpu_core::interp::ExecError;
use aero_cpu_core::{Cpu, CpuMode, RamBus};

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

fn setup() -> (Cpu, RamBus) {
    (Cpu::new(CpuMode::Long64), RamBus::new(0x10_000))
}

#[test]
fn pshufb_shuffle() {
    let (mut cpu, mut bus) = setup();
    cpu.sse.xmm[0] = xmm_from_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    cpu.sse.xmm[1] =
        xmm_from_bytes([0x80, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);

    cpu.execute_bytes(&mut bus, &[0x66, 0x0F, 0x38, 0x00, 0xC1])
        .unwrap();

    assert_eq!(
        xmm_to_bytes(cpu.sse.xmm[0]),
        [0, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]
    );
}

#[test]
fn ud_when_disabled() {
    let (mut cpu, mut bus) = setup();
    cpu.features.win7_x86_extensions = false;
    cpu.sse.xmm[0] = 0;
    cpu.sse.xmm[1] = 0;

    let err = cpu
        .execute_bytes(&mut bus, &[0x66, 0x0F, 0x38, 0x00, 0xC1])
        .unwrap_err();
    assert!(matches!(err, ExecError::InvalidOpcode(_)));
}

#[test]
fn popcnt_widths() {
    let (mut cpu, mut bus) = setup();

    // 16-bit: popcnt ax, cx
    cpu.regs.rax = 0xFFFF_0000_0000_0000;
    cpu.regs.rcx = 0b1011_0001_0000_1111;
    cpu.execute_bytes(&mut bus, &[0x66, 0xF3, 0x0F, 0xB8, 0xC1])
        .unwrap();
    assert_eq!(cpu.regs.rax, 0xFFFF_0000_0000_0000 | 8);

    // 32-bit: popcnt eax, ecx (zero-extends into rax in long mode)
    cpu.regs.rax = 0xFFFF_FFFF_FFFF_FFFF;
    cpu.regs.rcx = 0xFFFF_0000_F0F0_F0F0;
    cpu.execute_bytes(&mut bus, &[0xF3, 0x0F, 0xB8, 0xC1]).unwrap();
    assert_eq!(cpu.regs.rax, (0xF0F0_F0F0u32.count_ones()) as u64);

    // 64-bit: popcnt rax, rcx
    cpu.regs.rax = 0;
    cpu.regs.rcx = 0x8000_0000_0000_0001;
    cpu.execute_bytes(&mut bus, &[0xF3, 0x48, 0x0F, 0xB8, 0xC1])
        .unwrap();
    assert_eq!(cpu.regs.rax, 2);
}

#[test]
fn crc32_vectors() {
    let (mut cpu, mut bus) = setup();

    // CRC32 over "123456789" using crc32 eax, cl.
    let seed = 0xFFFF_FFFFu32;
    cpu.regs.set_eax(seed, cpu.mode);
    for &b in b"123456789" {
        cpu.regs.set_rcx(b as u64);
        cpu.execute_bytes(&mut bus, &[0xF2, 0x0F, 0x38, 0xF0, 0xC1])
            .unwrap();
    }
    let expected = crc32c_sw(seed, b"123456789");
    assert_eq!(cpu.regs.eax(), expected);

    // CRC32 eax, ecx (dword) processes little-endian bytes of the source.
    cpu.regs.set_eax(0, cpu.mode);
    cpu.regs.set_ecx(0x1234_5678, cpu.mode);
    cpu.execute_bytes(&mut bus, &[0xF2, 0x0F, 0x38, 0xF1, 0xC1])
        .unwrap();
    assert_eq!(
        cpu.regs.eax(),
        crc32c_sw(0, &0x1234_5678u32.to_le_bytes())
    );

    // CRC32 rax, rcx (qword) zero-extends the 32-bit CRC result.
    cpu.regs.rax = 0xFFFF_FFFF_FFFF_FFFF;
    cpu.regs.rcx = 0x0102_0304_0506_0708;
    cpu.execute_bytes(&mut bus, &[0xF2, 0x48, 0x0F, 0x38, 0xF1, 0xC1])
        .unwrap();
    let expected64 = crc32c_sw(0xFFFF_FFFF, &0x0102_0304_0506_0708u64.to_le_bytes());
    assert_eq!(cpu.regs.rax, expected64 as u64);
}

#[test]
fn pmulld_basic() {
    let (mut cpu, mut bus) = setup();
    cpu.sse.xmm[0] = xmm_from_i32x4([2, -3, 4, 0x4000_0000]);
    cpu.sse.xmm[1] = xmm_from_i32x4([5, 7, -8, 4]);
    cpu.execute_bytes(&mut bus, &[0x66, 0x0F, 0x38, 0x40, 0xC1])
        .unwrap();
    assert_eq!(
        xmm_to_i32x4(cpu.sse.xmm[0]),
        [10, -21, -32, 0x0000_0000]
    );
}

#[test]
fn pcmpestri_finds_nul() {
    let (mut cpu, mut bus) = setup();

    let mut chunk = [0u8; 16];
    chunk[..5].copy_from_slice(b"he\0lo");
    cpu.sse.xmm[0] = 0;
    cpu.sse.xmm[1] = xmm_from_bytes(chunk);

    cpu.regs.set_eax(16, cpu.mode);
    cpu.regs.rdx = 16;
    cpu.execute_bytes(&mut bus, &[0x66, 0x0F, 0x3A, 0x61, 0xC1, 0x08])
        .unwrap();
    assert_eq!(cpu.regs.ecx(), 2);
}

#[test]
fn scalar_ud_when_disabled() {
    let (mut cpu, mut bus) = setup();
    cpu.features.win7_x86_extensions = false;
    cpu.regs.rax = 0;
    cpu.regs.rcx = 123;

    assert!(matches!(
        cpu.execute_bytes(&mut bus, &[0xF3, 0x0F, 0xB8, 0xC1])
            .unwrap_err(),
        ExecError::InvalidOpcode(_)
    ));
    assert!(matches!(
        cpu.execute_bytes(&mut bus, &[0xF2, 0x0F, 0x38, 0xF0, 0xC1])
            .unwrap_err(),
        ExecError::InvalidOpcode(_)
    ));
}
