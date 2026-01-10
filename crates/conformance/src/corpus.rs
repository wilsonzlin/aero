use crate::{CpuState, FLAG_AF, FLAG_CF, FLAG_FIXED_1, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF};

pub struct XorShift64 {
    state: u64,
}

impl XorShift64 {
    pub fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }

    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.state = x;
        x
    }

    pub fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }
}

#[derive(Clone, Copy, Debug)]
pub enum TemplateKind {
    MovRaxRbx,
    MovRbxRax,
    AddRaxRbx,
    SubRaxRbx,
    XorRaxRbx,
    AndRaxRbx,
    OrRaxRbx,
    CmpRaxRbx,
    IncRax,
    DecRax,
    MovM64Rax,
    MovRaxM64,
    AddM64Rax,
    SubM64Rax,
}

#[derive(Clone, Copy, Debug)]
pub struct InstructionTemplate {
    pub name: &'static str,
    pub coverage_key: &'static str,
    pub bytes: &'static [u8],
    pub kind: TemplateKind,
    pub flags_mask: u64,
    pub mem_compare_len: usize,
}

pub fn templates() -> Vec<InstructionTemplate> {
    let all_flags = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
    let logic_flags = FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_OF;

    vec![
        InstructionTemplate {
            name: "mov rax, rbx",
            coverage_key: "mov",
            bytes: &[0x48, 0x89, 0xD8],
            kind: TemplateKind::MovRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "mov rbx, rax",
            coverage_key: "mov",
            bytes: &[0x48, 0x89, 0xC3],
            kind: TemplateKind::MovRbxRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "add rax, rbx",
            coverage_key: "add",
            bytes: &[0x48, 0x01, 0xD8],
            kind: TemplateKind::AddRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "sub rax, rbx",
            coverage_key: "sub",
            bytes: &[0x48, 0x29, 0xD8],
            kind: TemplateKind::SubRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "xor rax, rbx",
            coverage_key: "xor",
            bytes: &[0x48, 0x31, 0xD8],
            kind: TemplateKind::XorRaxRbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "and rax, rbx",
            coverage_key: "and",
            bytes: &[0x48, 0x21, 0xD8],
            kind: TemplateKind::AndRaxRbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "or rax, rbx",
            coverage_key: "or",
            bytes: &[0x48, 0x09, 0xD8],
            kind: TemplateKind::OrRaxRbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "cmp rax, rbx",
            coverage_key: "cmp",
            bytes: &[0x48, 0x39, 0xD8],
            kind: TemplateKind::CmpRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "inc rax",
            coverage_key: "inc",
            bytes: &[0x48, 0xFF, 0xC0],
            kind: TemplateKind::IncRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "dec rax",
            coverage_key: "dec",
            bytes: &[0x48, 0xFF, 0xC8],
            kind: TemplateKind::DecRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "mov qword ptr [rdi], rax",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x89, 0x07],
            kind: TemplateKind::MovM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
        },
        InstructionTemplate {
            name: "mov rax, qword ptr [rdi]",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x8B, 0x07],
            kind: TemplateKind::MovRaxM64,
            flags_mask: all_flags,
            mem_compare_len: 0,
        },
        InstructionTemplate {
            name: "add qword ptr [rdi], rax",
            coverage_key: "add_mem",
            bytes: &[0x48, 0x01, 0x07],
            kind: TemplateKind::AddM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
        },
        InstructionTemplate {
            name: "sub qword ptr [rdi], rax",
            coverage_key: "sub_mem",
            bytes: &[0x48, 0x29, 0x07],
            kind: TemplateKind::SubM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
        },
    ]
}

#[derive(Clone, Debug)]
pub struct TestCase {
    pub case_idx: usize,
    pub template: InstructionTemplate,
    pub init: CpuState,
    pub memory: Vec<u8>,
}

impl TestCase {
    pub fn generate(
        case_idx: usize,
        template: &InstructionTemplate,
        rng: &mut XorShift64,
        mem_base: u64,
    ) -> Self {
        let mut init = CpuState {
            rax: rng.next_u64(),
            rbx: rng.next_u64(),
            rcx: rng.next_u64(),
            rdx: rng.next_u64(),
            rsi: rng.next_u64(),
            rdi: rng.next_u64(),
            r8: rng.next_u64(),
            r9: rng.next_u64(),
            r10: rng.next_u64(),
            r11: rng.next_u64(),
            r12: rng.next_u64(),
            r13: rng.next_u64(),
            r14: rng.next_u64(),
            r15: rng.next_u64(),
            rflags: FLAG_FIXED_1,
            rip: 0,
        };

        let relevant_flags = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;
        init.rflags |= rng.next_u64() & relevant_flags;

        let memory_len = 64usize.max(template.mem_compare_len);
        let mut memory = vec![0u8; memory_len];
        for byte in &mut memory {
            *byte = rng.next_u8();
        }

        match template.kind {
            TemplateKind::MovM64Rax
            | TemplateKind::MovRaxM64
            | TemplateKind::AddM64Rax
            | TemplateKind::SubM64Rax => {
                init.rdi = mem_base;
            }
            _ => {}
        }

        Self {
            case_idx,
            template: *template,
            init,
            memory,
        }
    }
}
