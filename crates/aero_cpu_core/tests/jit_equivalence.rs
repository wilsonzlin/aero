mod common;

use common::*;

#[test]
fn batch_vs_single_step_straight_line() {
    let mut code = Vec::new();
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0x1234));
    code.extend_from_slice(&add_reg_imm16(Reg16::Ax, 0x0001));
    code.extend_from_slice(&sub_reg_imm16(Reg16::Ax, 0x0002));
    code.extend_from_slice(&ret());

    let case = SnippetCase::with_code(code);
    let batch = run_tier0_batch(&case);
    let step = run_tier0_single_step(&case);
    assert_eq!(batch, step);
}

#[test]
fn batch_vs_single_step_memory_load_store() {
    let addr = MEM_BASE as u16;
    let mut code = Vec::new();
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0xBEEF));
    code.extend_from_slice(&mov_mem_abs_reg16(addr, Reg16::Ax));
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0x0000));
    code.extend_from_slice(&mov_reg16_mem_abs(Reg16::Ax, addr));
    code.extend_from_slice(&ret());

    let case = SnippetCase::with_code(code);
    let batch = run_tier0_batch(&case);
    let step = run_tier0_single_step(&case);
    assert_eq!(batch, step);
    assert_eq!(batch.ax, 0xBEEF);
}

#[test]
fn batch_vs_single_step_branches_and_loops() {
    // mov cx, 3
    // mov ax, 0
    // loop: inc ax; dec cx; jnz loop
    // ret
    let mut code = Vec::new();
    code.extend_from_slice(&mov_reg_imm16(Reg16::Cx, 3));
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0));
    code.extend_from_slice(&inc_reg(Reg16::Ax));
    code.extend_from_slice(&dec_reg(Reg16::Cx));
    // jnz back to inc ax. Next IP is +2, target is -4 from there.
    code.extend_from_slice(&jnz(-4));
    code.extend_from_slice(&ret());

    let case = SnippetCase::with_code(code);
    let batch = run_tier0_batch(&case);
    let step = run_tier0_single_step(&case);
    assert_eq!(batch, step);
    assert_eq!(batch.ax, 3);
    assert_eq!(batch.cx, 0);
}

#[test]
fn batch_vs_single_step_flag_dependent_jcc() {
    // bx = 2 if ZF=1 after cmp ax,0
    let mut code = Vec::new();
    code.extend_from_slice(&mov_reg_imm16(Reg16::Ax, 0));
    code.extend_from_slice(&cmp_reg_imm16(Reg16::Ax, 0));
    // jz +5 to skip "mov bx,1; jmp +3"
    code.extend_from_slice(&jz(5));
    code.extend_from_slice(&mov_reg_imm16(Reg16::Bx, 1));
    code.extend_from_slice(&jmp(3));
    code.extend_from_slice(&mov_reg_imm16(Reg16::Bx, 2));
    code.extend_from_slice(&ret());

    let case = SnippetCase::with_code(code);
    let batch = run_tier0_batch(&case);
    let step = run_tier0_single_step(&case);
    assert_eq!(batch, step);
    assert_eq!(batch.bx, 2);
}

#[test]
fn batch_vs_single_step_randomized_regression() {
    let mut rng = XorShift64::new(0x4d59_5df4_d0f3_3173);

    for _ in 0..500 {
        let mut code = Vec::new();
        for _ in 0..6 {
            let reg = match rng.next_u8() % 4 {
                0 => Reg16::Ax,
                1 => Reg16::Bx,
                2 => Reg16::Cx,
                _ => Reg16::Dx,
            };
            let imm = rng.next_u16();
            match rng.next_u8() % 4 {
                0 => code.extend_from_slice(&mov_reg_imm16(reg, imm)),
                1 => code.extend_from_slice(&add_reg_imm16(reg, imm)),
                2 => code.extend_from_slice(&sub_reg_imm16(reg, imm)),
                _ => code.extend_from_slice(&cmp_reg_imm16(reg, imm)),
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

        let batch = run_tier0_batch(&case);
        let step = run_tier0_single_step(&case);
        assert_eq!(batch, step);
    }
}

