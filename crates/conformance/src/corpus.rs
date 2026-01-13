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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InitPreset {
    /// No fixups; keep whatever random state was generated.
    None,
    /// Force `CL` to a bounded value (typically used for shifts/rotates).
    ///
    /// This only modifies the low byte of `RCX` (i.e. `CL`), leaving the upper bits unchanged.
    #[allow(dead_code)]
    ShiftCountCl { mask: u8 },
    /// Force `RCX` into a small non-zero range (typically used for REP/LOOP style instructions).
    ///
    /// The resulting value is always in `1..=max` (or `1` if `max == 0`).
    #[allow(dead_code)]
    SmallRcx { max: u32 },
    /// Force `RDI` to point at `mem_base + data_off`.
    MemAtRdi { data_off: u32 },
}

impl InitPreset {
    fn apply(self, init: &mut CpuState, mem_base: u64) {
        match self {
            InitPreset::None => {}
            InitPreset::ShiftCountCl { mask } => {
                let cl = (init.rcx as u8) & mask;
                init.rcx = (init.rcx & !0xff) | (cl as u64);
            }
            InitPreset::SmallRcx { max } => {
                let max = (max as u64).max(1);
                init.rcx = (init.rcx % max) + 1;
            }
            InitPreset::MemAtRdi { data_off } => {
                init.rdi = mem_base + data_off as u64;
            }
        }
    }

    fn required_memory_len(self) -> Option<usize> {
        match self {
            InitPreset::MemAtRdi { data_off } => Some(data_off as usize + 64),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct InstructionTemplate {
    pub name: &'static str,
    pub coverage_key: &'static str,
    pub bytes: &'static [u8],
    pub kind: TemplateKind,
    pub flags_mask: u64,
    pub mem_compare_len: usize,
    pub init: InitPreset,
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
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "mov rbx, rax",
            coverage_key: "mov",
            bytes: &[0x48, 0x89, 0xC3],
            kind: TemplateKind::MovRbxRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "add rax, rbx",
            coverage_key: "add",
            bytes: &[0x48, 0x01, 0xD8],
            kind: TemplateKind::AddRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sub rax, rbx",
            coverage_key: "sub",
            bytes: &[0x48, 0x29, 0xD8],
            kind: TemplateKind::SubRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "xor rax, rbx",
            coverage_key: "xor",
            bytes: &[0x48, 0x31, 0xD8],
            kind: TemplateKind::XorRaxRbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "and rax, rbx",
            coverage_key: "and",
            bytes: &[0x48, 0x21, 0xD8],
            kind: TemplateKind::AndRaxRbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "or rax, rbx",
            coverage_key: "or",
            bytes: &[0x48, 0x09, 0xD8],
            kind: TemplateKind::OrRaxRbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cmp rax, rbx",
            coverage_key: "cmp",
            bytes: &[0x48, 0x39, 0xD8],
            kind: TemplateKind::CmpRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "inc rax",
            coverage_key: "inc",
            bytes: &[0x48, 0xFF, 0xC0],
            kind: TemplateKind::IncRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "dec rax",
            coverage_key: "dec",
            bytes: &[0x48, 0xFF, 0xC8],
            kind: TemplateKind::DecRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "mov qword ptr [rdi], rax",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x89, 0x07],
            kind: TemplateKind::MovM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "mov rax, qword ptr [rdi]",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x8B, 0x07],
            kind: TemplateKind::MovRaxM64,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "add qword ptr [rdi], rax",
            coverage_key: "add_mem",
            bytes: &[0x48, 0x01, 0x07],
            kind: TemplateKind::AddM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "sub qword ptr [rdi], rax",
            coverage_key: "sub_mem",
            bytes: &[0x48, 0x29, 0x07],
            kind: TemplateKind::SubM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
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

        let mut memory_len = 64usize.max(template.mem_compare_len);
        if let Some(required) = template.init.required_memory_len() {
            memory_len = memory_len.max(required);
        }
        let mut memory = vec![0u8; memory_len];
        for byte in &mut memory {
            *byte = rng.next_u8();
        }

        template.init.apply(&mut init, mem_base);

        Self {
            case_idx,
            template: *template,
            init,
            memory,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn init_preset_shift_count_cl_masks_low_byte() {
        let template = InstructionTemplate {
            name: "test",
            coverage_key: "test",
            bytes: &[0x90],
            kind: TemplateKind::MovRaxRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        };
        let mut rng = XorShift64::new(1);

        for case_idx in 0..256 {
            let case = TestCase::generate(case_idx, &template, &mut rng, 0x1000);
            let cl = case.init.rcx as u8;
            assert!(cl <= 0x1f, "CL={cl:#x} should be <= 0x1f");
        }
    }

    #[test]
    fn init_preset_small_rcx_bounds_non_zero() {
        let max = 17u32;
        let template = InstructionTemplate {
            name: "test",
            coverage_key: "test",
            bytes: &[0x90],
            kind: TemplateKind::MovRaxRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::SmallRcx { max },
        };
        let mut rng = XorShift64::new(1);

        for case_idx in 0..256 {
            let case = TestCase::generate(case_idx, &template, &mut rng, 0x1000);
            assert!(
                (1..=(max as u64)).contains(&case.init.rcx),
                "RCX={} should be in 1..={}",
                case.init.rcx,
                max
            );
        }
    }

    #[test]
    fn init_preset_mem_at_rdi_sets_pointer() {
        let mem_base = 0x0123_4567_89ab_c000u64;
        let data_off = 128u32;
        let template = InstructionTemplate {
            name: "test",
            coverage_key: "test",
            bytes: &[0x90],
            kind: TemplateKind::MovRaxRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::MemAtRdi { data_off },
        };
        let mut rng = XorShift64::new(1);
        let case = TestCase::generate(0, &template, &mut rng, mem_base);
        assert_eq!(case.init.rdi, mem_base + data_off as u64);
        assert!(case.memory.len() >= data_off as usize + 64);
    }

    #[test]
    fn existing_memory_templates_still_pin_rdi_to_mem_base() {
        let mem_base = 0x1000u64;
        let mut rng = XorShift64::new(1);

        for template in templates() {
            let case = TestCase::generate(0, &template, &mut rng, mem_base);
            match template.kind {
                TemplateKind::MovM64Rax
                | TemplateKind::MovRaxM64
                | TemplateKind::AddM64Rax
                | TemplateKind::SubM64Rax => {
                    assert_eq!(case.init.rdi, mem_base);
                }
                _ => {}
            }
        }
    }
}
