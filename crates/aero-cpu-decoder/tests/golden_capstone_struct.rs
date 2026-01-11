use aero_cpu_decoder::{
    decode_instruction, DecodeMode, Instruction, OpKind, Register, MAX_INSTRUCTION_LEN,
};
use capstone::arch::x86::{X86Operand, X86OperandType};
use capstone::prelude::*;

// Tiny deterministic PRNG for test input generation.
struct XorShift64(u64);

impl XorShift64 {
    fn next_u64(&mut self) -> u64 {
        // xorshift64*
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545F4914F6CDD1D)
    }

    fn fill(&mut self, buf: &mut [u8]) {
        for chunk in buf.chunks_mut(8) {
            let v = self.next_u64().to_le_bytes();
            let n = chunk.len();
            chunk.copy_from_slice(&v[..n]);
        }
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum OperandCat {
    Reg,
    Mem,
    Imm,
}

fn iced_op_cat(kind: OpKind) -> OperandCat {
    match kind {
        OpKind::Register => OperandCat::Reg,
        OpKind::Memory
        | OpKind::MemorySegSI
        | OpKind::MemorySegESI
        | OpKind::MemorySegRSI
        | OpKind::MemorySegDI
        | OpKind::MemorySegEDI
        | OpKind::MemorySegRDI
        | OpKind::MemoryESDI
        | OpKind::MemoryESEDI
        | OpKind::MemoryESRDI => OperandCat::Mem,
        _ => OperandCat::Imm,
    }
}

fn cap_op_cat(op: &X86Operand) -> OperandCat {
    match op.op_type {
        X86OperandType::Reg(_) => OperandCat::Reg,
        X86OperandType::Mem(_) => OperandCat::Mem,
        X86OperandType::Imm(_) => OperandCat::Imm,
        X86OperandType::Invalid => OperandCat::Imm,
    }
}

fn iced_reg_name(reg: Register) -> Option<String> {
    if reg == Register::None {
        return None;
    }
    let mut s = format!("{reg:?}").to_lowercase();
    // Capstone uses `r8b`..`r15b`, iced uses `r8l`..`r15l` in Debug formatting.
    if let Some(prefix) = s.strip_suffix('l') {
        if prefix.starts_with('r') && prefix[1..].chars().all(|c| c.is_ascii_digit()) {
            s.truncate(s.len() - 1);
            s.push('b');
        }
    }
    Some(s)
}

fn cap_reg_name(cs: &Capstone, reg: RegId) -> Option<String> {
    let name = cs.reg_name(reg)?.to_lowercase();
    if let Some(idx) = name.strip_prefix("st(").and_then(|s| s.strip_suffix(')')) {
        return Some(format!("st{idx}"));
    }
    match name.as_str() {
        // Capstone uses `eiz`/`riz` pseudo-registers to represent “no index”.
        "eiz" | "riz" => None,
        _ => Some(name),
    }
}

fn iced_mem_disp_i64(ins: &Instruction, ip: u64) -> i64 {
    let disp = ins.memory_displacement64();
    if ins.memory_base() == Register::RIP {
        let next_ip = ip.wrapping_add(ins.len() as u64);
        disp.wrapping_sub(next_ip) as i64
    } else {
        disp as i64
    }
}

fn iced_mem_fields(
    ins: &Instruction,
    ip: u64,
    op_kind: OpKind,
) -> (Option<String>, Option<String>, i32, i64) {
    match op_kind {
        OpKind::Memory => (
            iced_reg_name(ins.memory_base()),
            iced_reg_name(ins.memory_index()),
            ins.memory_index_scale() as i32,
            iced_mem_disp_i64(ins, ip),
        ),

        OpKind::MemorySegSI => (iced_reg_name(Register::SI), None, 1, 0),
        OpKind::MemorySegESI => (iced_reg_name(Register::ESI), None, 1, 0),
        OpKind::MemorySegRSI => (iced_reg_name(Register::RSI), None, 1, 0),
        OpKind::MemorySegDI => (iced_reg_name(Register::DI), None, 1, 0),
        OpKind::MemorySegEDI => (iced_reg_name(Register::EDI), None, 1, 0),
        OpKind::MemorySegRDI => (iced_reg_name(Register::RDI), None, 1, 0),
        OpKind::MemoryESDI => (iced_reg_name(Register::DI), None, 1, 0),
        OpKind::MemoryESEDI => (iced_reg_name(Register::EDI), None, 1, 0),
        OpKind::MemoryESRDI => (iced_reg_name(Register::RDI), None, 1, 0),

        _ => (None, None, 1, 0),
    }
}

#[test]
fn golden_decode_operands_match_capstone_x86_64() {
    let mut cs = Capstone::new()
        .x86()
        .mode(arch::x86::ArchMode::Mode64)
        .syntax(arch::x86::ArchSyntax::Intel)
        .detail(true)
        .build()
        .expect("capstone init");

    // Ensure Capstone doesn't try to "skipdata" over invalid bytes; we want strict decode.
    let _ = cs.set_skipdata(false);

    let mut rng = XorShift64(0xFEED_FACE_CAFE_BEEFu64);

    const TARGET_MATCHES: usize = 2_000;
    const MAX_ATTEMPTS: usize = 300_000;

    let mut matches = 0usize;
    let mut attempts = 0usize;
    while matches < TARGET_MATCHES && attempts < MAX_ATTEMPTS {
        attempts += 1;

        let mut bytes = [0u8; MAX_INSTRUCTION_LEN];
        rng.fill(&mut bytes);

        let ip = 0x1000u64;
        let ours = decode_instruction(DecodeMode::Bits64, ip, &bytes);
        let cap = cs.disasm_count(&bytes, ip, 1);

        let (Ok(ours), Ok(cap)) = (ours, cap) else {
            continue;
        };
        let Some(cap_ins) = cap.iter().next() else {
            continue;
        };

        assert_eq!(
            ours.len(),
            cap_ins.bytes().len(),
            "length mismatch @attempt={attempts}, bytes={:02X?}",
            bytes
        );

        let detail = cs.insn_detail(&cap_ins).expect("capstone detail");
        let arch_detail = detail.arch_detail();
        let x86_detail = arch_detail.x86().expect("x86 detail");
        let cap_ops: Vec<X86Operand> = x86_detail.operands().collect();

        // Capstone and iced-x86 don't always agree on whether some implicit operands
        // (eg. shift-by-1) are represented as explicit operands. Skip those cases.
        if ours.op_count() as usize != cap_ops.len() {
            continue;
        }

        for (op_idx, cap_op) in cap_ops.iter().enumerate() {
            let iced_kind = ours.op_kind(op_idx as u32);
            let iced_cat = iced_op_cat(iced_kind);
            let cap_cat = cap_op_cat(cap_op);
            assert_eq!(
                iced_cat, cap_cat,
                "operand kind mismatch @op={op_idx} attempt={attempts}, bytes={:02X?}",
                bytes
            );

            match (&cap_op.op_type, iced_cat) {
                (X86OperandType::Reg(cap_reg), OperandCat::Reg) => {
                    let cap_name = cap_reg_name(&cs, *cap_reg).unwrap_or_default();
                    let iced_reg = ours.op_register(op_idx as u32);
                    let iced_name = iced_reg_name(iced_reg).unwrap_or_default();
                    assert_eq!(
                        iced_name, cap_name,
                        "reg mismatch @op={op_idx} attempt={attempts}, bytes={:02X?}",
                        bytes
                    );
                }
                (X86OperandType::Mem(cap_mem), OperandCat::Mem) => {
                    let cap_base = cap_reg_name(&cs, cap_mem.base());
                    let cap_index = cap_reg_name(&cs, cap_mem.index());
                    let cap_scale = cap_mem.scale();
                    let cap_disp = cap_mem.disp();

                    let (iced_base, iced_index, iced_scale, iced_disp) =
                        iced_mem_fields(&ours, ip, iced_kind);

                    assert_eq!(
                        iced_base, cap_base,
                        "mem.base mismatch @attempt={attempts}, bytes={:02X?}",
                        bytes
                    );
                    assert_eq!(
                        iced_index, cap_index,
                        "mem.index mismatch @attempt={attempts}, bytes={:02X?}",
                        bytes
                    );
                    assert_eq!(
                        iced_scale, cap_scale,
                        "mem.scale mismatch @attempt={attempts}, bytes={:02X?}",
                        bytes
                    );
                    assert_eq!(
                        iced_disp, cap_disp,
                        "mem.disp mismatch @attempt={attempts}, bytes={:02X?}",
                        bytes
                    );
                }
                // Immediate comparisons are intentionally skipped here since different
                // disassemblers may choose different sign/zero-extension representations.
                _ => {}
            }
        }

        matches += 1;
    }

    assert!(
        matches >= TARGET_MATCHES,
        "only matched {matches} instructions after {attempts} attempts"
    );
}
