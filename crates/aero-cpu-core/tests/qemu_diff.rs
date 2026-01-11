#![cfg(feature = "qemu-diff")]

mod common;

use common::*;

fn to_qemu_case(case: &SnippetCase) -> qemu_diff::TestCase {
    qemu_diff::TestCase {
        ax: case.ax,
        bx: case.bx,
        cx: case.cx,
        dx: case.dx,
        si: case.si,
        di: case.di,
        bp: case.bp,
        sp: case.sp,
        flags: case.flags,
        ds: 0,
        es: 0,
        ss: 0,
        mem_init: case.mem_init,
        code: case.code.clone(),
    }
}

fn assert_matches_qemu(aero: CpuOutcome, qemu: qemu_diff::QemuOutcome) {
    assert_eq!(aero.ax, qemu.ax, "AX mismatch");
    assert_eq!(aero.bx, qemu.bx, "BX mismatch");
    assert_eq!(aero.cx, qemu.cx, "CX mismatch");
    assert_eq!(aero.dx, qemu.dx, "DX mismatch");
    assert_eq!(aero.si, qemu.si, "SI mismatch");
    assert_eq!(aero.di, qemu.di, "DI mismatch");
    assert_eq!(aero.bp, qemu.bp, "BP mismatch");
    assert_eq!(aero.sp, qemu.sp, "SP mismatch");
    assert_eq!(aero.mem_hash, qemu.mem_hash, "memory hash mismatch");
    assert_eq!(
        aero.flags & FLAG_MASK,
        qemu.flags & FLAG_MASK,
        "FLAGS mismatch (masked)"
    );
}

#[test]
fn qemu_diff_curated_arithmetic_flags() {
    if !qemu_diff::qemu_available() {
        eprintln!("qemu-system-* not found; skipping qemu diff tests");
        return;
    }

    let cases = [
        // 0xFFFF + 1 => 0, CF=1, ZF=1
        {
            let mut code = Vec::new();
            code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0xFFFF));
            code.extend_from_slice(&add_reg_imm16(Reg16::Ax, 1));
            code.extend_from_slice(&ret());
            SnippetCase::with_code(code)
        },
        // 0x7FFF + 1 => 0x8000, OF=1
        {
            let mut code = Vec::new();
            code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0x7FFF));
            code.extend_from_slice(&add_reg_imm16(Reg16::Ax, 1));
            code.extend_from_slice(&ret());
            SnippetCase::with_code(code)
        },
        // 0 - 1 => 0xFFFF, CF=1, SF=1
        {
            let mut code = Vec::new();
            code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0));
            code.extend_from_slice(&sub_reg_imm16(Reg16::Ax, 1));
            code.extend_from_slice(&ret());
            SnippetCase::with_code(code)
        },
    ];

    for case in &cases {
        let aero = run_tier0_batch(case);
        let qemu = qemu_diff::run(&to_qemu_case(case)).unwrap();
        assert_matches_qemu(aero, qemu);
    }
}

#[test]
fn qemu_diff_curated_memory_load_store() {
    if !qemu_diff::qemu_available() {
        eprintln!("qemu-system-* not found; skipping qemu diff tests");
        return;
    }

    let addr = MEM_BASE as u16;
    let mut code = Vec::new();
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0x1234));
    code.extend_from_slice(&mov_mem_abs_reg16(addr, Reg16::Ax));
    code.extend_from_slice(&mov_reg16_mem_abs(Reg16::Bx, addr));
    code.extend_from_slice(&ret());

    let case = SnippetCase::with_code(code);
    let aero = run_tier0_batch(&case);
    let qemu = qemu_diff::run(&to_qemu_case(&case)).unwrap();
    assert_matches_qemu(aero, qemu);
}

#[test]
fn qemu_diff_randomized_smoke() {
    if !qemu_diff::qemu_available() {
        eprintln!("qemu-system-* not found; skipping qemu diff tests");
        return;
    }

    let mut rng = XorShift64::new(0x26a2_6b1c_04c3_5a42);
    for _ in 0..25 {
        let mut code = Vec::new();
        for _ in 0..6 {
            let reg = match rng.next_u8() % 4 {
                0 => Reg16::Ax,
                1 => Reg16::Bx,
                2 => Reg16::Cx,
                _ => Reg16::Dx,
            };
            let imm = rng.next_u16();
            match rng.next_u8() % 3 {
                0 => code.extend_from_slice(&mov_reg_imm16(reg, imm)),
                1 => code.extend_from_slice(&add_reg_imm16(reg, imm)),
                _ => code.extend_from_slice(&sub_reg_imm16(reg, imm)),
            }
        }
        code.extend_from_slice(&ret());

        let mut case = SnippetCase::with_code(code);
        case.ax = rng.next_u16();
        case.bx = rng.next_u16();
        case.cx = rng.next_u16();
        case.dx = rng.next_u16();
        case.si = rng.next_u16();
        case.di = rng.next_u16();
        case.bp = rng.next_u16();
        case.sp = 0x9000;
        case.flags = rng.next_u16() | 0x2;
        rng.fill_bytes(&mut case.mem_init);

        let aero = run_tier0_batch(&case);
        let qemu = qemu_diff::run(&to_qemu_case(&case)).unwrap();
        assert_matches_qemu(aero, qemu);
    }
}

/// Demonstrates the value of the harness: it fails when there is a semantic mismatch.
///
/// Run manually with:
/// `cargo test -p aero-cpu-core --features qemu-diff -- --ignored`
#[test]
#[ignore]
fn demonstrate_seeded_bug_is_caught() {
    if !qemu_diff::qemu_available() {
        eprintln!("qemu-system-* not found; skipping qemu diff tests");
        return;
    }

    let mut code = Vec::new();
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0xFFFF));
    code.extend_from_slice(&add_reg_imm16(Reg16::Ax, 1));
    code.extend_from_slice(&ret());

    let case = SnippetCase::with_code(code);

    let mut aero = run_tier0_batch(&case);
    let qemu = qemu_diff::run(&to_qemu_case(&case)).unwrap();

    // Seed a bug by corrupting CF. The assertion below should fail when this test is un-ignored.
    aero.flags ^= 1;
    assert_matches_qemu(aero, qemu);
}
