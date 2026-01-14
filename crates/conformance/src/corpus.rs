use crate::{
    CpuState, FLAG_AF, FLAG_CF, FLAG_DF, FLAG_FIXED_1, FLAG_OF, FLAG_PF, FLAG_SF, FLAG_ZF,
};

use iced_x86::{Decoder, DecoderOptions, Instruction, Mnemonic, OpKind, Register};

/// Offset into `TestCase::memory` where instruction bytes may be placed.
///
/// The Tier-0 conformance backend fetches instruction bytes from a separate "code" region mapped
/// at vaddr 0, but we still reserve a small `CODE_OFF` prefix in the testcase memory and keep
/// memory operands inside it. This prevents data accesses from overlapping any embedded code bytes
/// (used by the optional QEMU/real-mode harness).
pub const CODE_OFF: usize = 32;

pub(crate) const MAX_TEST_MEMORY_LEN: usize = 64 * 1024;

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
    MovEaxEbx,
    MovAlBl,
    MovAxBx,
    MovzxEaxBl,
    MovsxEaxBl,
    MovzxEaxM8,
    MovsxEaxM8,
    AddRaxRbx,
    SubRaxRbx,
    AdcRaxRbx,
    AdcRaxM64,
    AdcM64Rax,
    AdcAlBl,
    SbbRaxRbx,
    SbbRaxM64,
    SbbM64Rax,
    SbbAlBl,
    XorRaxRbx,
    AndRaxRbx,
    OrRaxRbx,
    TestRaxRbx,
    CmpRaxRbx,
    CmpRaxImm32,
    CmpAlBl,
    IncRax,
    DecRax,
    NotRax,
    NotEax,
    NegRax,
    NegEax,
    ShlRax1,
    ShrRax1,
    SarRax1,
    ShlRaxCl,
    ShrRaxCl,
    SarRaxCl,
    ShlEax1,
    ShrEax1,
    SarEax1,
    ShlEaxCl,
    ShrEaxCl,
    SarEaxCl,
    RolRax1,
    RorRax1,
    RolRaxCl,
    RorRaxCl,
    RolEax1,
    RorEax1,
    RolEaxCl,
    RorEaxCl,
    RclRax1,
    RcrRax1,
    RclRaxCl,
    RcrRaxCl,
    RclEax1,
    RcrEax1,
    RclEaxCl,
    RcrEaxCl,
    ShldRaxRbx1,
    ShrdRaxRbx1,
    ShldRaxRbxCl,
    ShrdRaxRbxCl,
    ShldEaxEbx1,
    ShrdEaxEbx1,
    ShldEaxEbxCl,
    ShrdEaxEbxCl,
    MulRbx,
    MulM64,
    ImulRbx,
    ImulM64,
    ImulRaxRbx,
    ImulRaxM64,
    ImulRaxRbxImm8,
    DivRbx,
    IdivRbx,
    DivEbx,
    IdivEbx,
    BswapRax,
    BswapEax,
    LeaRaxBaseIndexScaleDisp,
    LeaEaxBaseIndexScaleDisp,
    XchgRaxRbx,
    XchgM64Rax,
    XaddRaxRbx,
    XaddEaxEbx,
    XaddM64Rax,
    BsfRaxRbx,
    BsrRaxRbx,
    BsfEaxEbx,
    BsrEaxEbx,
    BtRaxRbx,
    BtsRaxRbx,
    BtrRaxRbx,
    BtcRaxRbx,
    Clc,
    Stc,
    Cmc,
    Cld,
    Std,
    Cbw,
    Cwd,
    Cwde,
    Cdq,
    Cdqe,
    Cqo,
    SetzAl,
    SetcAl,
    CmovzRaxRbx,
    CmovcRaxRbx,
    AddEaxEbx,
    SubEaxEbx,
    AdcEaxEbx,
    AdcEaxM32,
    SbbEaxEbx,
    SbbEaxM32,
    XorEaxEbx,
    CmpEaxEbx,
    MovM64Rax,
    MovRaxM64,
    AddM64Rax,
    SubM64Rax,
    RepStosb,
    Ud2,
    MovRaxM64Abs0,
    GuardedOobLoad,
    GuardedOobStore,
    DivRbxByZero,
    #[cfg(feature = "qemu-reference")]
    /// A 16-bit real-mode snippet exercised via the QEMU reference backend.
    RealModeFarJump,
}

impl TemplateKind {
    pub fn is_fault_template(self) -> bool {
        matches!(
            self,
            TemplateKind::Ud2
                | TemplateKind::MovRaxM64Abs0
                | TemplateKind::GuardedOobLoad
                | TemplateKind::GuardedOobStore
                | TemplateKind::DivRbxByZero
        )
    }
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
    /// Ensure `RBX` is non-zero (useful for instructions like BSF/BSR where a zero source
    /// produces architecturally-undefined destination results).
    NonZeroRbx,
    /// Force `RBX` to 0 (useful for divide-by-zero fault templates).
    ZeroRbx,
    /// Force `RDI = mem_base + data_off` and `RCX` into `1..=max`.
    ///
    /// This is intended for `REP STOS*`-style templates where both the destination pointer and
    /// iteration count must be constrained to avoid hangs or memory faults.
    MemAtRdiSmallRcx { data_off: u32, max: u32 },
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
            InitPreset::NonZeroRbx => {
                init.rbx |= 1;
            }
            InitPreset::ZeroRbx => {
                init.rbx = 0;
            }
            InitPreset::MemAtRdiSmallRcx { data_off, max } => {
                init.rdi = mem_base + data_off as u64;
                let max = (max as u64).max(1);
                init.rcx = (init.rcx % max) + 1;
            }
        }
    }

    fn required_memory_len(self) -> Option<usize> {
        match self {
            InitPreset::MemAtRdi { data_off } => Some(data_off as usize + 64),
            InitPreset::MemAtRdiSmallRcx { data_off, .. } => Some(data_off as usize + 64),
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
    let shift_flags_imm1 = FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF | FLAG_OF;
    let shift_flags_cl = FLAG_CF | FLAG_PF | FLAG_ZF | FLAG_SF;
    let rotate_flags_cl = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF;
    let mul_flags = FLAG_CF | FLAG_OF;
    let all_flags_df = all_flags | FLAG_DF;

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
            name: "mov eax, ebx",
            coverage_key: "mov32",
            bytes: &[0x89, 0xD8],
            kind: TemplateKind::MovEaxEbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "mov al, bl",
            coverage_key: "mov8",
            bytes: &[0x88, 0xD8],
            kind: TemplateKind::MovAlBl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "mov ax, bx",
            coverage_key: "mov16",
            bytes: &[0x66, 0x89, 0xD8],
            kind: TemplateKind::MovAxBx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "movzx eax, bl",
            coverage_key: "movzx",
            bytes: &[0x0F, 0xB6, 0xC3],
            kind: TemplateKind::MovzxEaxBl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "movsx eax, bl",
            coverage_key: "movsx",
            bytes: &[0x0F, 0xBE, 0xC3],
            kind: TemplateKind::MovsxEaxBl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "movzx eax, byte ptr [rdi]",
            coverage_key: "movzx_mem",
            bytes: &[0x0F, 0xB6, 0x07],
            kind: TemplateKind::MovzxEaxM8,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "movsx eax, byte ptr [rdi]",
            coverage_key: "movsx_mem",
            bytes: &[0x0F, 0xBE, 0x07],
            kind: TemplateKind::MovsxEaxM8,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
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
            name: "adc rax, rbx",
            coverage_key: "adc",
            bytes: &[0x48, 0x11, 0xD8],
            kind: TemplateKind::AdcRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "adc al, bl",
            coverage_key: "adc",
            bytes: &[0x10, 0xD8],
            kind: TemplateKind::AdcAlBl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "adc rax, qword ptr [rdi]",
            coverage_key: "adc_mem",
            bytes: &[0x48, 0x13, 0x07],
            kind: TemplateKind::AdcRaxM64,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "adc qword ptr [rdi], rax",
            coverage_key: "adc_mem",
            bytes: &[0x48, 0x11, 0x07],
            kind: TemplateKind::AdcM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "sbb rax, rbx",
            coverage_key: "sbb",
            bytes: &[0x48, 0x19, 0xD8],
            kind: TemplateKind::SbbRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sbb al, bl",
            coverage_key: "sbb",
            bytes: &[0x18, 0xD8],
            kind: TemplateKind::SbbAlBl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sbb rax, qword ptr [rdi]",
            coverage_key: "sbb_mem",
            bytes: &[0x48, 0x1B, 0x07],
            kind: TemplateKind::SbbRaxM64,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "sbb qword ptr [rdi], rax",
            coverage_key: "sbb_mem",
            bytes: &[0x48, 0x19, 0x07],
            kind: TemplateKind::SbbM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
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
            name: "test rax, rbx",
            coverage_key: "test",
            bytes: &[0x48, 0x85, 0xD8],
            kind: TemplateKind::TestRaxRbx,
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
            name: "cmp rax, 0x80000000",
            coverage_key: "cmp_imm32",
            bytes: &[0x48, 0x81, 0xF8, 0x00, 0x00, 0x00, 0x80],
            kind: TemplateKind::CmpRaxImm32,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cmp al, bl",
            coverage_key: "cmp",
            bytes: &[0x38, 0xD8],
            kind: TemplateKind::CmpAlBl,
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
            name: "not rax",
            coverage_key: "not",
            bytes: &[0x48, 0xF7, 0xD0],
            kind: TemplateKind::NotRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "not eax",
            coverage_key: "not32",
            bytes: &[0xF7, 0xD0],
            kind: TemplateKind::NotEax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "neg rax",
            coverage_key: "neg",
            bytes: &[0x48, 0xF7, 0xD8],
            kind: TemplateKind::NegRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "neg eax",
            coverage_key: "neg32",
            bytes: &[0xF7, 0xD8],
            kind: TemplateKind::NegEax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shl rax, 1",
            coverage_key: "shl",
            bytes: &[0x48, 0xC1, 0xE0, 0x01],
            kind: TemplateKind::ShlRax1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shr rax, 1",
            coverage_key: "shr",
            bytes: &[0x48, 0xC1, 0xE8, 0x01],
            kind: TemplateKind::ShrRax1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sar rax, 1",
            coverage_key: "sar",
            bytes: &[0x48, 0xC1, 0xF8, 0x01],
            kind: TemplateKind::SarRax1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shl rax, cl",
            coverage_key: "shl_cl",
            bytes: &[0x48, 0xD3, 0xE0],
            kind: TemplateKind::ShlRaxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "shr rax, cl",
            coverage_key: "shr_cl",
            bytes: &[0x48, 0xD3, 0xE8],
            kind: TemplateKind::ShrRaxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "sar rax, cl",
            coverage_key: "sar_cl",
            bytes: &[0x48, 0xD3, 0xF8],
            kind: TemplateKind::SarRaxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "shl eax, 1",
            coverage_key: "shl32",
            bytes: &[0xC1, 0xE0, 0x01],
            kind: TemplateKind::ShlEax1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shr eax, 1",
            coverage_key: "shr32",
            bytes: &[0xC1, 0xE8, 0x01],
            kind: TemplateKind::ShrEax1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sar eax, 1",
            coverage_key: "sar32",
            bytes: &[0xC1, 0xF8, 0x01],
            kind: TemplateKind::SarEax1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shl eax, cl",
            coverage_key: "shl32_cl",
            bytes: &[0xD3, 0xE0],
            kind: TemplateKind::ShlEaxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "shr eax, cl",
            coverage_key: "shr32_cl",
            bytes: &[0xD3, 0xE8],
            kind: TemplateKind::ShrEaxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "sar eax, cl",
            coverage_key: "sar32_cl",
            bytes: &[0xD3, 0xF8],
            kind: TemplateKind::SarEaxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "rol rax, 1",
            coverage_key: "rol",
            bytes: &[0x48, 0xC1, 0xC0, 0x01],
            kind: TemplateKind::RolRax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "ror rax, 1",
            coverage_key: "ror",
            bytes: &[0x48, 0xC1, 0xC8, 0x01],
            kind: TemplateKind::RorRax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "rol rax, cl",
            coverage_key: "rol_cl",
            bytes: &[0x48, 0xD3, 0xC0],
            kind: TemplateKind::RolRaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "ror rax, cl",
            coverage_key: "ror_cl",
            bytes: &[0x48, 0xD3, 0xC8],
            kind: TemplateKind::RorRaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "rol eax, 1",
            coverage_key: "rol32",
            bytes: &[0xC1, 0xC0, 0x01],
            kind: TemplateKind::RolEax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "ror eax, 1",
            coverage_key: "ror32",
            bytes: &[0xC1, 0xC8, 0x01],
            kind: TemplateKind::RorEax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "rol eax, cl",
            coverage_key: "rol32_cl",
            bytes: &[0xD3, 0xC0],
            kind: TemplateKind::RolEaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "ror eax, cl",
            coverage_key: "ror32_cl",
            bytes: &[0xD3, 0xC8],
            kind: TemplateKind::RorEaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "rcl rax, 1",
            coverage_key: "rcl",
            bytes: &[0x48, 0xD1, 0xD0],
            kind: TemplateKind::RclRax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "rcr rax, 1",
            coverage_key: "rcr",
            bytes: &[0x48, 0xD1, 0xD8],
            kind: TemplateKind::RcrRax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "rcl rax, cl",
            coverage_key: "rcl_cl",
            bytes: &[0x48, 0xD3, 0xD0],
            kind: TemplateKind::RclRaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "rcr rax, cl",
            coverage_key: "rcr_cl",
            bytes: &[0x48, 0xD3, 0xD8],
            kind: TemplateKind::RcrRaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "rcl eax, 1",
            coverage_key: "rcl32",
            bytes: &[0xD1, 0xD0],
            kind: TemplateKind::RclEax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "rcr eax, 1",
            coverage_key: "rcr32",
            bytes: &[0xD1, 0xD8],
            kind: TemplateKind::RcrEax1,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "rcl eax, cl",
            coverage_key: "rcl32_cl",
            bytes: &[0xD3, 0xD0],
            kind: TemplateKind::RclEaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "rcr eax, cl",
            coverage_key: "rcr32_cl",
            bytes: &[0xD3, 0xD8],
            kind: TemplateKind::RcrEaxCl,
            flags_mask: rotate_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "shld rax, rbx, 1",
            coverage_key: "shld",
            bytes: &[0x48, 0x0F, 0xA4, 0xD8, 0x01],
            kind: TemplateKind::ShldRaxRbx1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shrd rax, rbx, 1",
            coverage_key: "shrd",
            bytes: &[0x48, 0x0F, 0xAC, 0xD8, 0x01],
            kind: TemplateKind::ShrdRaxRbx1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shld rax, rbx, cl",
            coverage_key: "shld_cl",
            bytes: &[0x48, 0x0F, 0xA5, 0xD8],
            kind: TemplateKind::ShldRaxRbxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "shrd rax, rbx, cl",
            coverage_key: "shrd_cl",
            bytes: &[0x48, 0x0F, 0xAD, 0xD8],
            kind: TemplateKind::ShrdRaxRbxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x3f },
        },
        InstructionTemplate {
            name: "shld eax, ebx, 1",
            coverage_key: "shld32",
            bytes: &[0x0F, 0xA4, 0xD8, 0x01],
            kind: TemplateKind::ShldEaxEbx1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shrd eax, ebx, 1",
            coverage_key: "shrd32",
            bytes: &[0x0F, 0xAC, 0xD8, 0x01],
            kind: TemplateKind::ShrdEaxEbx1,
            flags_mask: shift_flags_imm1,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "shld eax, ebx, cl",
            coverage_key: "shld32_cl",
            bytes: &[0x0F, 0xA5, 0xD8],
            kind: TemplateKind::ShldEaxEbxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "shrd eax, ebx, cl",
            coverage_key: "shrd32_cl",
            bytes: &[0x0F, 0xAD, 0xD8],
            kind: TemplateKind::ShrdEaxEbxCl,
            flags_mask: shift_flags_cl,
            mem_compare_len: 0,
            init: InitPreset::ShiftCountCl { mask: 0x1f },
        },
        InstructionTemplate {
            name: "mul rbx",
            coverage_key: "mul",
            bytes: &[0x48, 0xF7, 0xE3],
            kind: TemplateKind::MulRbx,
            flags_mask: mul_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "mul qword ptr [rdi]",
            coverage_key: "mul_mem",
            bytes: &[0x48, 0xF7, 0x27],
            kind: TemplateKind::MulM64,
            flags_mask: mul_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "imul rbx",
            coverage_key: "imul1",
            bytes: &[0x48, 0xF7, 0xEB],
            kind: TemplateKind::ImulRbx,
            flags_mask: mul_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "imul qword ptr [rdi]",
            coverage_key: "imul1_mem",
            bytes: &[0x48, 0xF7, 0x2F],
            kind: TemplateKind::ImulM64,
            flags_mask: mul_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "imul rax, rbx",
            coverage_key: "imul2",
            bytes: &[0x48, 0x0F, 0xAF, 0xC3],
            kind: TemplateKind::ImulRaxRbx,
            flags_mask: mul_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "imul rax, qword ptr [rdi]",
            coverage_key: "imul2_mem",
            bytes: &[0x48, 0x0F, 0xAF, 0x07],
            kind: TemplateKind::ImulRaxM64,
            flags_mask: mul_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "imul rax, rbx, 7",
            coverage_key: "imul3",
            bytes: &[0x48, 0x6B, 0xC3, 0x07],
            kind: TemplateKind::ImulRaxRbxImm8,
            flags_mask: mul_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "div rbx",
            coverage_key: "div",
            bytes: &[0x48, 0xF7, 0xF3],
            kind: TemplateKind::DivRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "idiv rbx",
            coverage_key: "idiv",
            bytes: &[0x48, 0xF7, 0xFB],
            kind: TemplateKind::IdivRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "div ebx",
            coverage_key: "div32",
            bytes: &[0xF7, 0xF3],
            kind: TemplateKind::DivEbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "idiv ebx",
            coverage_key: "idiv32",
            bytes: &[0xF7, 0xFB],
            kind: TemplateKind::IdivEbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "bswap rax",
            coverage_key: "bswap",
            bytes: &[0x48, 0x0F, 0xC8],
            kind: TemplateKind::BswapRax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "bswap eax",
            coverage_key: "bswap32",
            bytes: &[0x0F, 0xC8],
            kind: TemplateKind::BswapEax,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "lea rax, [rbx + rcx*4 + 0x10]",
            coverage_key: "lea",
            bytes: &[0x48, 0x8D, 0x44, 0x8B, 0x10],
            kind: TemplateKind::LeaRaxBaseIndexScaleDisp,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "lea eax, [rbx + rcx*4 + 0x10]",
            coverage_key: "lea32",
            bytes: &[0x8D, 0x44, 0x8B, 0x10],
            kind: TemplateKind::LeaEaxBaseIndexScaleDisp,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "xchg rax, rbx",
            coverage_key: "xchg",
            bytes: &[0x48, 0x87, 0xD8],
            kind: TemplateKind::XchgRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "xchg qword ptr [rdi], rax",
            coverage_key: "xchg",
            bytes: &[0x48, 0x87, 0x07],
            kind: TemplateKind::XchgM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "xadd rax, rbx",
            coverage_key: "xadd",
            bytes: &[0x48, 0x0F, 0xC1, 0xD8],
            kind: TemplateKind::XaddRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "xadd eax, ebx",
            coverage_key: "xadd32",
            bytes: &[0x0F, 0xC1, 0xD8],
            kind: TemplateKind::XaddEaxEbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "xadd qword ptr [rdi], rax",
            coverage_key: "xadd_mem",
            bytes: &[0x48, 0x0F, 0xC1, 0x07],
            kind: TemplateKind::XaddM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "bsf rax, rbx",
            coverage_key: "bsf",
            bytes: &[0x48, 0x0F, 0xBC, 0xC3],
            kind: TemplateKind::BsfRaxRbx,
            flags_mask: FLAG_ZF,
            mem_compare_len: 0,
            init: InitPreset::NonZeroRbx,
        },
        InstructionTemplate {
            name: "bsr rax, rbx",
            coverage_key: "bsr",
            bytes: &[0x48, 0x0F, 0xBD, 0xC3],
            kind: TemplateKind::BsrRaxRbx,
            flags_mask: FLAG_ZF,
            mem_compare_len: 0,
            init: InitPreset::NonZeroRbx,
        },
        InstructionTemplate {
            name: "bsf eax, ebx",
            coverage_key: "bsf32",
            bytes: &[0x0F, 0xBC, 0xC3],
            kind: TemplateKind::BsfEaxEbx,
            flags_mask: FLAG_ZF,
            mem_compare_len: 0,
            init: InitPreset::NonZeroRbx,
        },
        InstructionTemplate {
            name: "bsr eax, ebx",
            coverage_key: "bsr32",
            bytes: &[0x0F, 0xBD, 0xC3],
            kind: TemplateKind::BsrEaxEbx,
            flags_mask: FLAG_ZF,
            mem_compare_len: 0,
            init: InitPreset::NonZeroRbx,
        },
        InstructionTemplate {
            name: "bt rax, rbx",
            coverage_key: "bt",
            bytes: &[0x48, 0x0F, 0xA3, 0xD8],
            kind: TemplateKind::BtRaxRbx,
            flags_mask: FLAG_CF,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "bts rax, rbx",
            coverage_key: "bts",
            bytes: &[0x48, 0x0F, 0xAB, 0xD8],
            kind: TemplateKind::BtsRaxRbx,
            flags_mask: FLAG_CF,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "btr rax, rbx",
            coverage_key: "btr",
            bytes: &[0x48, 0x0F, 0xB3, 0xD8],
            kind: TemplateKind::BtrRaxRbx,
            flags_mask: FLAG_CF,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "btc rax, rbx",
            coverage_key: "btc",
            bytes: &[0x48, 0x0F, 0xBB, 0xD8],
            kind: TemplateKind::BtcRaxRbx,
            flags_mask: FLAG_CF,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "clc",
            coverage_key: "flag",
            bytes: &[0xF8],
            kind: TemplateKind::Clc,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "stc",
            coverage_key: "flag",
            bytes: &[0xF9],
            kind: TemplateKind::Stc,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cmc",
            coverage_key: "flag",
            bytes: &[0xF5],
            kind: TemplateKind::Cmc,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cld",
            coverage_key: "flag_df",
            bytes: &[0xFC],
            kind: TemplateKind::Cld,
            flags_mask: all_flags_df,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "std",
            coverage_key: "flag_df",
            bytes: &[0xFD],
            kind: TemplateKind::Std,
            flags_mask: all_flags_df,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cbw",
            coverage_key: "signext",
            bytes: &[0x66, 0x98],
            kind: TemplateKind::Cbw,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cwd",
            coverage_key: "signext",
            bytes: &[0x66, 0x99],
            kind: TemplateKind::Cwd,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cwde",
            coverage_key: "signext",
            bytes: &[0x98],
            kind: TemplateKind::Cwde,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cdq",
            coverage_key: "signext",
            bytes: &[0x99],
            kind: TemplateKind::Cdq,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cdqe",
            coverage_key: "signext",
            bytes: &[0x48, 0x98],
            kind: TemplateKind::Cdqe,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cqo",
            coverage_key: "signext",
            bytes: &[0x48, 0x99],
            kind: TemplateKind::Cqo,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "setz al",
            coverage_key: "setcc",
            bytes: &[0x0F, 0x94, 0xC0],
            kind: TemplateKind::SetzAl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "setc al",
            coverage_key: "setcc",
            bytes: &[0x0F, 0x92, 0xC0],
            kind: TemplateKind::SetcAl,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cmovz rax, rbx",
            coverage_key: "cmov",
            bytes: &[0x48, 0x0F, 0x44, 0xC3],
            kind: TemplateKind::CmovzRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cmovc rax, rbx",
            coverage_key: "cmov",
            bytes: &[0x48, 0x0F, 0x42, 0xC3],
            kind: TemplateKind::CmovcRaxRbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "add eax, ebx",
            coverage_key: "add32",
            bytes: &[0x01, 0xD8],
            kind: TemplateKind::AddEaxEbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sub eax, ebx",
            coverage_key: "sub32",
            bytes: &[0x29, 0xD8],
            kind: TemplateKind::SubEaxEbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "adc eax, ebx",
            coverage_key: "adc32",
            bytes: &[0x11, 0xD8],
            kind: TemplateKind::AdcEaxEbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "adc eax, dword ptr [rdi]",
            coverage_key: "adc32_mem",
            bytes: &[0x13, 0x07],
            kind: TemplateKind::AdcEaxM32,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "sbb eax, ebx",
            coverage_key: "sbb32",
            bytes: &[0x19, 0xD8],
            kind: TemplateKind::SbbEaxEbx,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "sbb eax, dword ptr [rdi]",
            coverage_key: "sbb32_mem",
            bytes: &[0x1B, 0x07],
            kind: TemplateKind::SbbEaxM32,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "xor eax, ebx",
            coverage_key: "xor32",
            bytes: &[0x31, 0xD8],
            kind: TemplateKind::XorEaxEbx,
            flags_mask: logic_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "cmp eax, ebx",
            coverage_key: "cmp32",
            bytes: &[0x39, 0xD8],
            kind: TemplateKind::CmpEaxEbx,
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
            name: "mov qword ptr [rdi], rax (rdi=mem_base+8)",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x89, 0x07],
            kind: TemplateKind::MovM64Rax,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 8 },
        },
        InstructionTemplate {
            name: "mov rax, qword ptr [rdi]",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x8B, 0x07],
            kind: TemplateKind::MovRaxM64,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "mov rax, qword ptr [rdi] (rdi=mem_base+8)",
            coverage_key: "mov_mem",
            bytes: &[0x48, 0x8B, 0x07],
            kind: TemplateKind::MovRaxM64,
            flags_mask: all_flags,
            mem_compare_len: 16,
            init: InitPreset::MemAtRdi { data_off: 8 },
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
        InstructionTemplate {
            name: "rep stosb",
            coverage_key: "rep_stos",
            bytes: &[0xF3, 0xAA],
            kind: TemplateKind::RepStosb,
            flags_mask: all_flags,
            mem_compare_len: CODE_OFF,
            init: InitPreset::MemAtRdiSmallRcx {
                data_off: 0,
                max: 16,
            },
        },
        InstructionTemplate {
            name: "ud2",
            coverage_key: "fault_ud2",
            bytes: &[0x0F, 0x0B],
            kind: TemplateKind::Ud2,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::None,
        },
        InstructionTemplate {
            name: "mov rax, qword ptr [0]",
            coverage_key: "fault_mem",
            // Absolute disp32 addressing via SIB (mod=00, r/m=100, base=101) to force a
            // user-mode page fault at address 0.
            bytes: &[0x48, 0x8B, 0x04, 0x25, 0x00, 0x00, 0x00, 0x00],
            kind: TemplateKind::MovRaxM64Abs0,
            flags_mask: all_flags,
            mem_compare_len: 0,
            // Not used by the instruction itself, but needed so the Aero backend anchors its
            // in-memory buffer at `mem_base` and treats address 0 as out-of-bounds.
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "guarded OOB load (mov rax, qword ptr [rdi-8])",
            coverage_key: "fault_oob_load",
            bytes: &[0x48, 0x8B, 0x47, 0xF8],
            kind: TemplateKind::GuardedOobLoad,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "guarded OOB store (mov qword ptr [rdi-8], rax)",
            coverage_key: "fault_oob_store",
            bytes: &[0x48, 0x89, 0x47, 0xF8],
            kind: TemplateKind::GuardedOobStore,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::MemAtRdi { data_off: 0 },
        },
        InstructionTemplate {
            name: "div rbx (divide-by-zero)",
            coverage_key: "fault_div0",
            bytes: &[0x48, 0xF7, 0xF3],
            kind: TemplateKind::DivRbxByZero,
            flags_mask: all_flags,
            mem_compare_len: 0,
            init: InitPreset::ZeroRbx,
        },
    ]
}

#[cfg(feature = "qemu-reference")]
pub fn templates_qemu() -> Vec<InstructionTemplate> {
    let all_flags = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_OF;

    const CODE_BASE: u16 = 0x0700;
    const TARGET_OFF: u16 = CODE_BASE + 5;
    const REAL_MODE_LJMP_SNIPPET: &[u8] = &[
        // ljmp 0x0000:0x0705 (far control transfer; not host-user-mode safe)
        0xEA,
        (TARGET_OFF & 0xFF) as u8,
        (TARGET_OFF >> 8) as u8,
        0x00,
        0x00,
        // mov ax, 0x1234
        0xB8,
        0x34,
        0x12,
        // ret
        0xC3,
    ];

    vec![InstructionTemplate {
        name: "ljmp 0:0705; mov ax,0x1234; ret",
        coverage_key: "rm_far_jmp",
        bytes: REAL_MODE_LJMP_SNIPPET,
        kind: TemplateKind::RealModeFarJump,
        flags_mask: all_flags,
        // QEMU harness only reports a scratch memory hash, not the full bytes.
        mem_compare_len: 4,
        init: InitPreset::None,
    }]
}

#[derive(Clone, Debug)]
pub struct TestCase {
    pub case_idx: usize,
    pub template: InstructionTemplate,
    pub init: CpuState,
    /// Base address for interpreting `memory` as a linear region.
    ///
    /// `memory[0]` corresponds to this address in both backends.
    pub mem_base: u64,
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
            // Conformance templates execute with RIP=0, and the Aero Tier-0 backend fetches from a
            // separate "code" region at vaddr 0.
            rip: 0,
        };

        #[cfg(feature = "qemu-reference")]
        if matches!(template.kind, TemplateKind::RealModeFarJump) {
            // The QEMU harness only initializes 16-bit registers; keep the rest zero so the state
            // round-trips cleanly.
            init = CpuState {
                rax: rng.next_u64() as u16 as u64,
                rbx: rng.next_u64() as u16 as u64,
                rcx: rng.next_u64() as u16 as u64,
                rdx: rng.next_u64() as u16 as u64,
                rsi: rng.next_u64() as u16 as u64,
                rdi: rng.next_u64() as u16 as u64,
                rflags: FLAG_FIXED_1,
                // QEMU executes the snippet at 0x0000:0x0700.
                rip: 0x0700,
                ..CpuState::default()
            };
        }

        let relevant_flags = FLAG_CF | FLAG_PF | FLAG_AF | FLAG_ZF | FLAG_SF | FLAG_DF | FLAG_OF;
        init.rflags |= rng.next_u64() & relevant_flags;

        let mut memory_len = 64usize.max(template.mem_compare_len);
        if let Some(required) = template.init.required_memory_len() {
            memory_len = memory_len.max(required);
        }
        let code_off = {
            #[cfg(feature = "qemu-reference")]
            if matches!(template.kind, TemplateKind::RealModeFarJump) {
                0x0700usize
                    .checked_sub(mem_base as usize)
                    .expect("qemu code base must be >= mem_base")
            } else {
                CODE_OFF
            }
            #[cfg(not(feature = "qemu-reference"))]
            {
                CODE_OFF
            }
        };
        let min_len = code_off
            .checked_add(template.bytes.len())
            .and_then(|v| v.checked_add(1))
            .expect("memory length overflow");
        memory_len = memory_len.max(min_len);

        #[cfg(feature = "qemu-reference")]
        if matches!(template.kind, TemplateKind::RealModeFarJump) {
            // Ensure the synthetic stack slot (0x8FFE) exists so the `ret` in the snippet can
            // return cleanly.
            let required = 0x8FFFusize
                .checked_sub(mem_base as usize)
                .and_then(|v| v.checked_add(1))
                .expect("stack length overflow");
            memory_len = memory_len.max(required);
        }
        if memory_len > MAX_TEST_MEMORY_LEN {
            panic!(
                "testcase memory length {memory_len} exceeds MAX_TEST_MEMORY_LEN={MAX_TEST_MEMORY_LEN}"
            );
        }
        let mut memory = vec![0u8; memory_len];
        for byte in &mut memory {
            *byte = rng.next_u8();
        }
        memory[code_off..code_off + template.bytes.len()].copy_from_slice(template.bytes);

        template.init.apply(&mut init, mem_base);
        apply_auto_fixups(case_idx, template, &mut init, &mut memory, mem_base, rng);

        Self {
            case_idx,
            template: *template,
            init,
            mem_base,
            memory,
        }
    }
}

fn apply_auto_fixups(
    case_idx: usize,
    template: &InstructionTemplate,
    init: &mut CpuState,
    memory: &mut [u8],
    mem_base: u64,
    rng: &mut XorShift64,
) {
    // Fault templates intentionally crash in user-mode; don't "fix" them into non-faulting cases.
    if template.kind.is_fault_template() {
        return;
    }

    let instruction = decode_instruction(template.bytes);
    if instruction.is_invalid() {
        return;
    }

    // Defensive: clamp potentially-unbounded loop counts even if the template forgot to apply a
    // specific preset.
    fixup_shift_rotate_using_cl(&instruction, init);

    // Fix memory operands first: DIV/IDIV may use a memory divisor and we want it in-bounds before
    // we write a non-zero divisor value into memory.
    fixup_memory_operands(template, &instruction, init, memory.len(), mem_base, rng);

    fixup_rep_string(case_idx, &instruction, init, memory.len(), mem_base, rng);
    fixup_div_idiv(&instruction, init, memory, mem_base, rng);
}

fn decode_instruction(bytes: &[u8]) -> Instruction {
    let mut decoder = Decoder::with_ip(64, bytes, 0, DecoderOptions::NONE);
    decoder.decode()
}

fn fixup_shift_rotate_using_cl(instruction: &Instruction, init: &mut CpuState) {
    let uses_cl = instruction.op_count() >= 2
        && instruction.op1_kind() == OpKind::Register
        && instruction.op1_register() == Register::CL;
    if !uses_cl {
        return;
    }

    match instruction.mnemonic() {
        Mnemonic::Rol
        | Mnemonic::Ror
        | Mnemonic::Rcl
        | Mnemonic::Rcr
        | Mnemonic::Shl
        | Mnemonic::Shr
        | Mnemonic::Sar
        | Mnemonic::Sal => {
            // Hardware masks the count, but the Aero interpreter may implement these in a loop.
            // Clamp to the architecturally-observable range while preserving the upper bits of RCX.
            let cl = (init.rcx as u8) & 0x3f;
            init.rcx = (init.rcx & !0xff) | (cl as u64);
        }
        _ => {}
    }
}

fn string_element_size(instruction: &Instruction) -> Option<usize> {
    if instruction.op_count() != 0 {
        return None;
    }

    match instruction.mnemonic() {
        Mnemonic::Movsb | Mnemonic::Stosb | Mnemonic::Cmpsb | Mnemonic::Scasb | Mnemonic::Lodsb => {
            Some(1)
        }
        Mnemonic::Movsw | Mnemonic::Stosw | Mnemonic::Cmpsw | Mnemonic::Scasw | Mnemonic::Lodsw => {
            Some(2)
        }
        Mnemonic::Movsd | Mnemonic::Stosd | Mnemonic::Cmpsd | Mnemonic::Scasd | Mnemonic::Lodsd => {
            Some(4)
        }
        Mnemonic::Movsq | Mnemonic::Stosq | Mnemonic::Cmpsq | Mnemonic::Scasq | Mnemonic::Lodsq => {
            Some(8)
        }
        _ => None,
    }
}

fn fixup_rep_string(
    _case_idx: usize,
    instruction: &Instruction,
    init: &mut CpuState,
    memory_len: usize,
    mem_base: u64,
    rng: &mut XorShift64,
) {
    let elem_size = match string_element_size(instruction) {
        Some(size) => size,
        None => return,
    };

    let df_set = (init.rflags & FLAG_DF) != 0;

    // Keep all string operations inside the "data" prefix of the testcase buffer (avoid the code
    // region at `CODE_OFF..`).
    let data_len = memory_len.min(CODE_OFF);

    let has_rep = instruction.has_rep_prefix() || instruction.has_repne_prefix();
    let max_count = (data_len / elem_size).min(32);
    let count = if has_rep {
        if max_count == 0 {
            init.rcx = 0;
            0
        } else if init.rcx == 0 {
            0
        } else {
            // Clamp to a safe non-zero range while preserving the randomized seed (and any
            // template-provided init preset).
            let count = (((init.rcx - 1) % (max_count as u64)) + 1) as usize;
            init.rcx = count as u64;
            count
        }
    } else {
        1
    };

    let count_for_bounds = count.max(1);
    let total_bytes = count_for_bounds.saturating_mul(elem_size);
    if total_bytes == 0 || data_len == 0 {
        init.rsi = mem_base;
        init.rdi = mem_base;
        return;
    }

    let (min_start, max_start) = if !df_set {
        (0usize, data_len.saturating_sub(total_bytes))
    } else {
        (
            (count_for_bounds - 1).saturating_mul(elem_size),
            data_len.saturating_sub(elem_size),
        )
    };

    let choose_off = |addr: u64, rng: &mut XorShift64| -> usize {
        let current = addr
            .checked_sub(mem_base)
            .and_then(|v| usize::try_from(v).ok());
        if let Some(off) = current {
            if (min_start..=max_start).contains(&off) {
                return off;
            }
        }
        if min_start == max_start {
            return min_start;
        }
        let span = (max_start - min_start + 1) as u64;
        min_start + (rng.next_u64() % span) as usize
    };

    let rsi_off = choose_off(init.rsi, rng);
    let rdi_off = choose_off(init.rdi, rng);

    init.rsi = mem_base.wrapping_add(rsi_off as u64);
    init.rdi = mem_base.wrapping_add(rdi_off as u64);
}

fn fixup_div_idiv(
    instruction: &Instruction,
    init: &mut CpuState,
    memory: &mut [u8],
    mem_base: u64,
    rng: &mut XorShift64,
) {
    if !matches!(instruction.mnemonic(), Mnemonic::Div | Mnemonic::Idiv) {
        return;
    }

    // Pick small, positive operands and clear the high half of the dividend so we don't hit #DE
    // (div-by-zero or quotient overflow).
    let dividend = rng.next_u64() & 0xFFFF;
    init.rax = dividend;
    init.rdx = 0;

    let divisor = (rng.next_u64() & 0xFFFF) | 1;

    match instruction.op0_kind() {
        OpKind::Register => {
            let reg = instruction.op0_register();
            let _ = write_register_part(init, reg, divisor);
        }
        OpKind::Memory => {
            let addr = match effective_address(instruction, init) {
                Some(addr) => addr,
                None => return,
            };
            let size = instruction.memory_size().size().clamp(1, 8);
            write_memory_le(memory, mem_base, addr, divisor, size);
        }
        _ => {}
    }
}

fn fixup_memory_operands(
    template: &InstructionTemplate,
    instruction: &Instruction,
    init: &mut CpuState,
    memory_len: usize,
    mem_base: u64,
    rng: &mut XorShift64,
) {
    // LEA uses ModR/M addressing but does not dereference memory.
    if instruction.mnemonic() == Mnemonic::Lea {
        return;
    }

    let mut has_mem = false;
    for idx in 0..instruction.op_count() {
        if instruction.op_kind(idx) == OpKind::Memory {
            has_mem = true;
            break;
        }
    }
    if !has_mem {
        return;
    }

    let access_size = instruction.memory_size().size();
    let access_size = access_size.max(1);

    // Keep all regular memory operands inside the data prefix, before the code bytes at CODE_OFF.
    let mut target_len = memory_len.min(CODE_OFF);
    if template.mem_compare_len > 0 {
        target_len = target_len.min(template.mem_compare_len);
    }
    target_len = target_len.max(access_size);

    let max_eff_off = target_len.saturating_sub(access_size) as u64;

    // If the current address is already safe, keep it (preserves template-provided presets like
    // `MemAtRdi`).
    if let Some(addr) = effective_address(instruction, init) {
        if let Some(off) = addr.checked_sub(mem_base) {
            if off <= max_eff_off && addr.checked_add(access_size as u64).is_some() {
                return;
            }
        }
    }

    let index_reg = instruction.memory_index();
    if index_reg != Register::None {
        let _ = write_register_part(init, index_reg, 0);
    }

    // If neutralizing the index was enough, stop.
    if let Some(addr) = effective_address(instruction, init) {
        if let Some(off) = addr.checked_sub(mem_base) {
            if off <= max_eff_off {
                return;
            }
        }
    }

    let base_reg = instruction.memory_base();
    if base_reg == Register::None || base_reg == Register::RIP || base_reg == Register::EIP {
        return;
    }

    let disp = instruction.memory_displacement64() as i64;
    let base = choose_base_for_disp(max_eff_off as i64, disp, mem_base, rng.next_u64());
    let _ = write_register_part(init, base_reg, base);
}

fn choose_base_for_disp(max_eff_off: i64, disp: i64, mem_base: u64, randomness: u64) -> u64 {
    // Solve for an effective offset in [0..=max_eff_off] with index=0.
    let eff_span = (max_eff_off + 1).max(1) as u64;
    let eff_off = (randomness % eff_span) as i64;
    let base_off = eff_off - disp;
    add_signed(mem_base, base_off)
}

fn add_signed(base: u64, offset: i64) -> u64 {
    if offset >= 0 {
        base.wrapping_add(offset as u64)
    } else {
        base.wrapping_sub((-offset) as u64)
    }
}

fn effective_address(instruction: &Instruction, state: &CpuState) -> Option<u64> {
    let base_reg = instruction.memory_base();
    let base = if base_reg == Register::None {
        0
    } else {
        read_register(state, base_reg)?
    };

    let index_reg = instruction.memory_index();
    let index = if index_reg == Register::None {
        0
    } else {
        read_register(state, index_reg)?
    };

    let scale = instruction.memory_index_scale() as u64;
    let disp = instruction.memory_displacement64();
    Some(
        base.wrapping_add(index.wrapping_mul(scale))
            .wrapping_add(disp),
    )
}

fn write_memory_le(memory: &mut [u8], mem_base: u64, addr: u64, value: u64, size: usize) {
    let Some(offset) = addr
        .checked_sub(mem_base)
        .and_then(|v| usize::try_from(v).ok())
    else {
        return;
    };
    let Some(end) = offset.checked_add(size) else {
        return;
    };
    if end > memory.len() {
        return;
    }
    memory[offset..end].copy_from_slice(&value.to_le_bytes()[..size]);
}

#[derive(Clone, Copy, Debug)]
enum Gpr {
    Rax,
    Rbx,
    Rcx,
    Rdx,
    Rsi,
    Rdi,
    R8,
    R9,
    R10,
    R11,
    R12,
    R13,
    R14,
    R15,
}

#[derive(Clone, Copy, Debug)]
enum RegPart {
    Full,
    Low32,
    Low16,
    Low8,
    High8,
}

fn read_register(state: &CpuState, reg: Register) -> Option<u64> {
    let (gpr, part) = gpr_part(reg)?;
    let full = match gpr {
        Gpr::Rax => state.rax,
        Gpr::Rbx => state.rbx,
        Gpr::Rcx => state.rcx,
        Gpr::Rdx => state.rdx,
        Gpr::Rsi => state.rsi,
        Gpr::Rdi => state.rdi,
        Gpr::R8 => state.r8,
        Gpr::R9 => state.r9,
        Gpr::R10 => state.r10,
        Gpr::R11 => state.r11,
        Gpr::R12 => state.r12,
        Gpr::R13 => state.r13,
        Gpr::R14 => state.r14,
        Gpr::R15 => state.r15,
    };

    Some(match part {
        RegPart::Full => full,
        RegPart::Low32 => full & 0xFFFF_FFFF,
        RegPart::Low16 => full & 0xFFFF,
        RegPart::Low8 => full & 0xFF,
        RegPart::High8 => (full >> 8) & 0xFF,
    })
}

fn write_register_part(state: &mut CpuState, reg: Register, value: u64) -> Option<()> {
    let (gpr, part) = gpr_part(reg)?;
    let slot = match gpr {
        Gpr::Rax => &mut state.rax,
        Gpr::Rbx => &mut state.rbx,
        Gpr::Rcx => &mut state.rcx,
        Gpr::Rdx => &mut state.rdx,
        Gpr::Rsi => &mut state.rsi,
        Gpr::Rdi => &mut state.rdi,
        Gpr::R8 => &mut state.r8,
        Gpr::R9 => &mut state.r9,
        Gpr::R10 => &mut state.r10,
        Gpr::R11 => &mut state.r11,
        Gpr::R12 => &mut state.r12,
        Gpr::R13 => &mut state.r13,
        Gpr::R14 => &mut state.r14,
        Gpr::R15 => &mut state.r15,
    };

    match part {
        RegPart::Full => {
            *slot = value;
        }
        RegPart::Low32 => {
            *slot = (*slot & !0xFFFF_FFFF) | (value & 0xFFFF_FFFF);
        }
        RegPart::Low16 => {
            *slot = (*slot & !0xFFFF) | (value & 0xFFFF);
        }
        RegPart::Low8 => {
            *slot = (*slot & !0xFF) | (value & 0xFF);
        }
        RegPart::High8 => {
            *slot = (*slot & !(0xFF << 8)) | ((value & 0xFF) << 8);
        }
    }

    Some(())
}

fn gpr_part(reg: Register) -> Option<(Gpr, RegPart)> {
    let out = match reg {
        Register::RAX | Register::EAX | Register::AX | Register::AL => (Gpr::Rax, RegPart::Full),
        Register::RBX | Register::EBX | Register::BX | Register::BL => (Gpr::Rbx, RegPart::Full),
        Register::RCX | Register::ECX | Register::CX | Register::CL => (Gpr::Rcx, RegPart::Full),
        Register::RDX | Register::EDX | Register::DX | Register::DL => (Gpr::Rdx, RegPart::Full),
        Register::RSI | Register::ESI | Register::SI | Register::SIL => (Gpr::Rsi, RegPart::Full),
        Register::RDI | Register::EDI | Register::DI | Register::DIL => (Gpr::Rdi, RegPart::Full),
        Register::R8 | Register::R8D | Register::R8W | Register::R8L => (Gpr::R8, RegPart::Full),
        Register::R9 | Register::R9D | Register::R9W | Register::R9L => (Gpr::R9, RegPart::Full),
        Register::R10 | Register::R10D | Register::R10W | Register::R10L => {
            (Gpr::R10, RegPart::Full)
        }
        Register::R11 | Register::R11D | Register::R11W | Register::R11L => {
            (Gpr::R11, RegPart::Full)
        }
        Register::R12 | Register::R12D | Register::R12W | Register::R12L => {
            (Gpr::R12, RegPart::Full)
        }
        Register::R13 | Register::R13D | Register::R13W | Register::R13L => {
            (Gpr::R13, RegPart::Full)
        }
        Register::R14 | Register::R14D | Register::R14W | Register::R14L => {
            (Gpr::R14, RegPart::Full)
        }
        Register::R15 | Register::R15D | Register::R15W | Register::R15L => {
            (Gpr::R15, RegPart::Full)
        }
        // High 8-bit regs are only reachable without a REX prefix; include them for completeness.
        Register::AH => (Gpr::Rax, RegPart::High8),
        Register::BH => (Gpr::Rbx, RegPart::High8),
        Register::CH => (Gpr::Rcx, RegPart::High8),
        Register::DH => (Gpr::Rdx, RegPart::High8),
        _ => return None,
    };

    Some(match reg {
        Register::EAX
        | Register::EBX
        | Register::ECX
        | Register::EDX
        | Register::ESI
        | Register::EDI
        | Register::R8D
        | Register::R9D
        | Register::R10D
        | Register::R11D
        | Register::R12D
        | Register::R13D
        | Register::R14D
        | Register::R15D => (out.0, RegPart::Low32),
        Register::AX
        | Register::BX
        | Register::CX
        | Register::DX
        | Register::SI
        | Register::DI
        | Register::R8W
        | Register::R9W
        | Register::R10W
        | Register::R11W
        | Register::R12W
        | Register::R13W
        | Register::R14W
        | Register::R15W => (out.0, RegPart::Low16),
        Register::AL
        | Register::BL
        | Register::CL
        | Register::DL
        | Register::SIL
        | Register::DIL
        | Register::R8L
        | Register::R9L
        | Register::R10L
        | Register::R11L
        | Register::R12L
        | Register::R13L
        | Register::R14L
        | Register::R15L => (out.0, RegPart::Low8),
        _ => out,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generation_is_panic_free_and_pointers_are_in_range() {
        let templates = templates();
        assert!(!templates.is_empty(), "expected at least one template");

        let mut rng = XorShift64::new(0x1234_5678_9abc_def0);
        let mem_base = 0x1_0000_0000u64;

        let n = 256usize;
        for case_idx in 0..n {
            let template = &templates[case_idx % templates.len()];
            let case = TestCase::generate(case_idx, template, &mut rng, mem_base);

            if template.kind.is_fault_template() {
                continue;
            }

            let instruction = decode_instruction(template.bytes);
            if instruction.is_invalid() {
                continue;
            }

            // Regular memory operands (`[base+index*scale+disp]`).
            let has_mem =
                (0..instruction.op_count()).any(|i| instruction.op_kind(i) == OpKind::Memory);
            if has_mem && instruction.mnemonic() != Mnemonic::Lea {
                let addr = effective_address(&instruction, &case.init)
                    .expect("memory operand should have a computable effective address");
                let size = instruction.memory_size().size().max(1) as u64;
                let end = addr.checked_add(size).unwrap_or(addr);
                let mem_end = mem_base + case.memory.len() as u64;
                assert!(
                    addr >= mem_base && end <= mem_end,
                    "template '{}' produced OOB memory address: addr={:#x} end={:#x} mem=[{:#x},{:#x})",
                    template.name,
                    addr,
                    end,
                    mem_base,
                    mem_end
                );
            }

            // String instructions with implicit RSI/RDI.
            if string_element_size(&instruction).is_some() {
                let mem_end = mem_base + case.memory.len() as u64;
                assert!(
                    case.init.rsi >= mem_base && case.init.rsi < mem_end,
                    "template '{}' produced OOB RSI: {:#x} mem=[{:#x},{:#x})",
                    template.name,
                    case.init.rsi,
                    mem_base,
                    mem_end
                );
                assert!(
                    case.init.rdi >= mem_base && case.init.rdi < mem_end,
                    "template '{}' produced OOB RDI: {:#x} mem=[{:#x},{:#x})",
                    template.name,
                    case.init.rdi,
                    mem_base,
                    mem_end
                );
            }
        }
    }

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
    fn init_preset_non_zero_rbx_forces_bit0() {
        let template = InstructionTemplate {
            name: "test",
            coverage_key: "test",
            bytes: &[0x90],
            kind: TemplateKind::MovRaxRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::NonZeroRbx,
        };
        let mut rng = XorShift64::new(1);
        for case_idx in 0..256 {
            let case = TestCase::generate(case_idx, &template, &mut rng, 0x1000);
            assert_ne!(case.init.rbx, 0, "RBX must be non-zero");
            assert_eq!(case.init.rbx & 1, 1, "RBX low bit should be set");
        }
    }

    #[test]
    fn init_preset_mem_at_rdi_small_rcx_sets_both() {
        let mem_base = 0x2000u64;
        let template = InstructionTemplate {
            name: "test",
            coverage_key: "test",
            bytes: &[0x90],
            kind: TemplateKind::MovRaxRbx,
            flags_mask: 0,
            mem_compare_len: 0,
            init: InitPreset::MemAtRdiSmallRcx {
                data_off: 12,
                max: 9,
            },
        };

        let mut rng = XorShift64::new(1);
        for case_idx in 0..128 {
            let case = TestCase::generate(case_idx, &template, &mut rng, mem_base);
            assert_eq!(case.init.rdi, mem_base + 12);
            assert!(
                (1..=9).contains(&(case.init.rcx as u32)),
                "RCX={} should be in 1..=9",
                case.init.rcx
            );
        }
    }

    #[test]
    fn existing_memory_templates_still_pin_rdi_to_mem_base() {
        let mem_base = 0x1000u64;
        let mut rng = XorShift64::new(1);

        for template in templates() {
            let case = TestCase::generate(0, &template, &mut rng, mem_base);
            if let InitPreset::MemAtRdi { data_off: 0 } = template.init {
                assert_eq!(case.init.rdi, mem_base);
            }
        }
    }

    #[test]
    fn mem_at_rdi_offset_is_interpreted_relative_to_mem_base_in_aero() {
        let mem_base = 0x1000u64;
        let data_off = 16u32;
        let template = InstructionTemplate {
            name: "mov [rdi], rax (offset)",
            coverage_key: "test",
            bytes: &[0x48, 0x89, 0x07],
            kind: TemplateKind::MovM64Rax,
            flags_mask: 0,
            mem_compare_len: (data_off as usize) + 8,
            init: InitPreset::MemAtRdi { data_off },
        };

        let mut rng = XorShift64::new(1);
        let case = TestCase::generate(0, &template, &mut rng, mem_base);

        let expected = case.init.rax.to_le_bytes();
        let start = data_off as usize;
        let end = start + expected.len();

        // Ensure the test is meaningful: the bytes we expect to write differ from
        // the existing memory at that location.
        assert_ne!(
            &case.memory[start..end],
            expected,
            "randomized memory already matched expected write"
        );

        let mut aero = crate::aero::AeroBackend::new(libc::SIGSEGV);
        let outcome = aero.execute(&case);
        assert!(outcome.fault.is_none());
        assert_eq!(&outcome.memory[start..end], expected);
    }
}
