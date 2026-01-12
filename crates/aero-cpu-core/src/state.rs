//! Canonical CPU state representation (x86 / x86-64).
//!
//! This is the single CPU state model used by Aero's default execution engines:
//! the Tier-0 interpreter and the JIT runtime. Legacy interpreter stacks are
//! feature-gated and must not introduce additional `CpuState` variants.
//!
//! # ABI stability
//! `CpuState` is the *in-memory ABI* between the interpreter and dynamically
//! generated WASM JIT blocks. The layout is intentionally `#[repr(C)]` and the
//! JIT-visible offsets are frozen by unit tests. If you need to change this
//! layout, update the public `CPU_*_OFF` constants and the corresponding tests
//! *together*.

use aero_x86::Register;
use core::fmt;

use crate::{
    exception::Exception,
    fpu::FpuState,
    linear_mem::{read_bytes_wrapped, read_u32_wrapped, write_bytes_wrapped, write_u32_wrapped},
    mem::CpuBus,
    sse_state::SseState,
    FxStateError, FXSAVE_AREA_SIZE,
};

/// Number of general purpose registers in [`CpuState::gpr`].
pub const GPR_COUNT: usize = 16;

/// Canonical register indices for [`CpuState::gpr`].
pub mod gpr {
    pub const RAX: usize = 0;
    pub const RCX: usize = 1;
    pub const RDX: usize = 2;
    pub const RBX: usize = 3;
    pub const RSP: usize = 4;
    pub const RBP: usize = 5;
    pub const RSI: usize = 6;
    pub const RDI: usize = 7;
    pub const R8: usize = 8;
    pub const R9: usize = 9;
    pub const R10: usize = 10;
    pub const R11: usize = 11;
    pub const R12: usize = 12;
    pub const R13: usize = 13;
    pub const R14: usize = 14;
    pub const R15: usize = 15;
}

// ---- RFLAGS bits -----------------------------------------------------------

/// Carry Flag.
pub const RFLAGS_CF: u64 = 1 << 0;
/// Reserved bit 1. Always reads as 1 on real hardware.
pub const RFLAGS_RESERVED1: u64 = 1 << 1;
/// Parity Flag.
pub const RFLAGS_PF: u64 = 1 << 2;
/// Auxiliary Carry Flag.
pub const RFLAGS_AF: u64 = 1 << 4;
/// Zero Flag.
pub const RFLAGS_ZF: u64 = 1 << 6;
/// Sign Flag.
pub const RFLAGS_SF: u64 = 1 << 7;
/// Trap Flag.
pub const RFLAGS_TF: u64 = 1 << 8;
/// Interrupt Enable Flag.
pub const RFLAGS_IF: u64 = 1 << 9;
/// Direction Flag.
pub const RFLAGS_DF: u64 = 1 << 10;
/// Overflow Flag.
pub const RFLAGS_OF: u64 = 1 << 11;
/// I/O Privilege Level (2 bits).
pub const RFLAGS_IOPL_MASK: u64 = 0b11 << 12;
/// Nested Task.
pub const RFLAGS_NT: u64 = 1 << 14;
/// Resume Flag.
pub const RFLAGS_RF: u64 = 1 << 16;
/// Virtual 8086 Mode.
pub const RFLAGS_VM: u64 = 1 << 17;
/// Alignment Check.
pub const RFLAGS_AC: u64 = 1 << 18;
/// Virtual Interrupt Flag.
pub const RFLAGS_VIF: u64 = 1 << 19;
/// Virtual Interrupt Pending.
pub const RFLAGS_VIP: u64 = 1 << 20;
/// ID Flag.
pub const RFLAGS_ID: u64 = 1 << 21;

// Legacy flag aliases used throughout the interpreter.
pub const FLAG_CF: u64 = RFLAGS_CF;
pub const FLAG_PF: u64 = RFLAGS_PF;
pub const FLAG_AF: u64 = RFLAGS_AF;
pub const FLAG_ZF: u64 = RFLAGS_ZF;
pub const FLAG_SF: u64 = RFLAGS_SF;
pub const FLAG_DF: u64 = RFLAGS_DF;
pub const FLAG_OF: u64 = RFLAGS_OF;

// ---- Control/Model-specific register bits ---------------------------------

pub const CR0_PE: u64 = 1 << 0;
pub const CR0_MP: u64 = 1 << 1;
pub const CR0_EM: u64 = 1 << 2;
pub const CR0_TS: u64 = 1 << 3;
pub const CR0_NE: u64 = 1 << 5;
pub const CR0_PG: u64 = 1 << 31;

pub const CR4_PAE: u64 = 1 << 5;
pub const CR4_OSFXSR: u64 = 1 << 9;
pub const CR4_OSXMMEXCPT: u64 = 1 << 10;

// ---- MXCSR bits (SSE control/status register) ------------------------------
//
// Intel SDM Vol. 1, "MXCSR Control and Status Register".
//
// Note: `sse_state::MXCSR_MASK` models which bits are supported/validated by the
// emulator. The constants below describe architectural bit positions.

// Sticky exception status flags.
pub const MXCSR_IE: u32 = 1 << 0; // Invalid operation
pub const MXCSR_DE: u32 = 1 << 1; // Denormal
pub const MXCSR_ZE: u32 = 1 << 2; // Divide-by-zero
pub const MXCSR_OE: u32 = 1 << 3; // Overflow
pub const MXCSR_UE: u32 = 1 << 4; // Underflow
pub const MXCSR_PE: u32 = 1 << 5; // Precision

pub const MXCSR_EXCEPTION_FLAGS_MASK: u32 =
    MXCSR_IE | MXCSR_DE | MXCSR_ZE | MXCSR_OE | MXCSR_UE | MXCSR_PE;

// Exception mask bits (1 = masked, 0 = unmasked).
pub const MXCSR_IM: u32 = 1 << 7;
pub const MXCSR_DM: u32 = 1 << 8;
pub const MXCSR_ZM: u32 = 1 << 9;
pub const MXCSR_OM: u32 = 1 << 10;
pub const MXCSR_UM: u32 = 1 << 11;
pub const MXCSR_PM: u32 = 1 << 12;

/// Mask of all MXCSR exception mask bits (`IM`..`PM`).
pub const MXCSR_EXCEPTION_MASK: u32 =
    MXCSR_IM | MXCSR_DM | MXCSR_ZM | MXCSR_OM | MXCSR_UM | MXCSR_PM;

/// Rounding control field mask (`RC`, bits 13..=14).
pub const MXCSR_RC_MASK: u32 = 0b11 << 13;

pub const EFER_LME: u64 = 1 << 8;
pub const EFER_LMA: u64 = 1 << 10;

// ---- Segment access rights bits --------------------------------------------

/// Segment descriptor cache "access rights" encoding.
///
/// This intentionally matches the layout used by Intel VMX "segment access
/// rights" fields (AR bytes):
/// - bits 0..=3: type
/// - bit 4: S (descriptor type: system vs code/data)
/// - bits 5..=6: DPL
/// - bit 7: P (present)
/// - bit 8: AVL
/// - bit 9: L (64-bit code segment)
/// - bit 10: D/B (default operand size / big)
/// - bit 11: G (granularity)
/// - bit 16: unusable
pub const SEG_ACCESS_L: u32 = 1 << 9;
pub const SEG_ACCESS_DB: u32 = 1 << 10;
pub const SEG_ACCESS_UNUSABLE: u32 = 1 << 16;
pub const SEG_ACCESS_PRESENT: u32 = 1 << 7;

// ---- JIT ABI offsets --------------------------------------------------------

/// Offset (in bytes) of [`CpuState::gpr`].
///
/// This is intentionally 0 to make JIT loads/stores cheap.
pub const CPU_GPR_BASE_OFF: usize = 0;

/// Offsets (in bytes) of [`CpuState::gpr`] elements in architectural order.
pub const CPU_GPR_OFF: [usize; GPR_COUNT] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104, 112, 120,
];

/// Offset (in bytes) of [`CpuState::rip`].
pub const CPU_RIP_OFF: usize = 128;

/// Offset (in bytes) of [`CpuState::rflags`].
pub const CPU_RFLAGS_OFF: usize = 136;

/// Offset (in bytes) of `CpuState.sse.xmm[i]` for each XMM register.
pub const CPU_XMM_OFF: [usize; 16] = [
    784, 800, 816, 832, 848, 864, 880, 896, 912, 928, 944, 960, 976, 992, 1008, 1024,
];

/// Total size (in bytes) of [`CpuState`].
pub const CPU_STATE_SIZE: usize = 1072;

/// Alignment (in bytes) of [`CpuState`].
pub const CPU_STATE_ALIGN: usize = 16;

// ---- Public types -----------------------------------------------------------

/// High-level execution mode classification.
///
/// This enum is a coarse classification used by the interpreter and JIT tiering.
/// For instruction decoding/execution, the effective operand/address size still
/// depends on CS.D and prefixes.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq, Default)]
pub enum CpuMode {
    /// Real mode (CR0.PE = 0).
    #[default]
    Real = 0,
    /// Protected mode (CR0.PE = 1, not executing 64-bit code).
    Protected = 1,
    /// Long mode, 64-bit code segment (IA-32e active and CS.L = 1).
    Long = 2,
    /// Virtual 8086 mode (placeholder; semantics largely like real mode but
    /// under protected-mode paging/privilege rules).
    Vm86 = 3,
}

impl CpuMode {
    /// Backwards-compatible aliases for tier-0 interpreter tests.
    #[allow(non_upper_case_globals)]
    pub const Bit16: CpuMode = CpuMode::Real;
    #[allow(non_upper_case_globals)]
    pub const Bit32: CpuMode = CpuMode::Protected;
    #[allow(non_upper_case_globals)]
    pub const Bit64: CpuMode = CpuMode::Long;

    /// Returns the effective code bitness implied by this coarse mode.
    pub fn bitness(self) -> u32 {
        match self {
            CpuMode::Real | CpuMode::Vm86 => 16,
            CpuMode::Protected => 32,
            CpuMode::Long => 64,
        }
    }

    /// Returns the mask applied to RIP/EIP/IP in this coarse mode.
    pub fn ip_mask(self) -> u64 {
        match self {
            CpuMode::Real | CpuMode::Vm86 => 0xFFFF,
            CpuMode::Protected => 0xFFFF_FFFF,
            CpuMode::Long => u64::MAX,
        }
    }

    pub fn addr_mask(self) -> u64 {
        self.ip_mask()
    }
}

/// Architectural operand size used by flag computation and partial register
/// helpers.
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OperandSize {
    Byte = 1,
    Word = 2,
    Dword = 4,
    Qword = 8,
}

impl OperandSize {
    #[inline]
    pub const fn bytes(self) -> u8 {
        self as u8
    }

    #[inline]
    pub const fn bits(self) -> u32 {
        (self as u32) * 8
    }

    #[inline]
    pub const fn mask(self) -> u64 {
        match self {
            OperandSize::Byte => 0xFF,
            OperandSize::Word => 0xFFFF,
            OperandSize::Dword => 0xFFFF_FFFF,
            OperandSize::Qword => 0xFFFF_FFFF_FFFF_FFFF,
        }
    }

    #[inline]
    pub const fn sign_bit(self) -> u64 {
        1u64 << (self.bits() - 1)
    }
}

/// The last flag-producing operation (for lazy flag evaluation).
#[repr(u8)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LazyFlagOp {
    None = 0,
    Add = 1,
    Adc = 2,
    Sub = 3,
    Sbb = 4,
    Logic = 5,
}

/// Lazy flags state stored in [`CpuState`].
///
/// For most ALU instructions we do not eagerly update `rflags` – instead we
/// record enough information to materialize specific flags on demand.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct LazyFlags {
    pub op: LazyFlagOp,
    pub size: OperandSize,
    /// Carry-in for ADC/SBB (lower bit is used).
    pub carry_in: u8,
    pub _pad0: [u8; 5],
    pub result: u64,
    pub op1: u64,
    pub op2: u64,
}

impl Default for LazyFlags {
    fn default() -> Self {
        Self {
            op: LazyFlagOp::None,
            size: OperandSize::Qword,
            carry_in: 0,
            _pad0: [0; 5],
            result: 0,
            op1: 0,
            op2: 0,
        }
    }
}

impl LazyFlags {
    #[inline]
    pub const fn is_active(&self) -> bool {
        !matches!(self.op, LazyFlagOp::None)
    }

    #[inline]
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    #[inline]
    pub const fn for_add(size: OperandSize, op1: u64, op2: u64, result: u64) -> Self {
        Self {
            op: LazyFlagOp::Add,
            size,
            carry_in: 0,
            _pad0: [0; 5],
            result,
            op1,
            op2,
        }
    }

    #[inline]
    pub const fn for_adc(
        size: OperandSize,
        op1: u64,
        op2: u64,
        carry_in: bool,
        result: u64,
    ) -> Self {
        Self {
            op: LazyFlagOp::Adc,
            size,
            carry_in: carry_in as u8,
            _pad0: [0; 5],
            result,
            op1,
            op2,
        }
    }

    #[inline]
    pub const fn for_sub(size: OperandSize, op1: u64, op2: u64, result: u64) -> Self {
        Self {
            op: LazyFlagOp::Sub,
            size,
            carry_in: 0,
            _pad0: [0; 5],
            result,
            op1,
            op2,
        }
    }

    #[inline]
    pub const fn for_sbb(
        size: OperandSize,
        op1: u64,
        op2: u64,
        carry_in: bool,
        result: u64,
    ) -> Self {
        Self {
            op: LazyFlagOp::Sbb,
            size,
            carry_in: carry_in as u8,
            _pad0: [0; 5],
            result,
            op1,
            op2,
        }
    }

    #[inline]
    pub const fn for_logic(size: OperandSize, result: u64) -> Self {
        Self {
            op: LazyFlagOp::Logic,
            size,
            carry_in: 0,
            _pad0: [0; 5],
            result,
            op1: 0,
            op2: 0,
        }
    }

    #[inline]
    fn masked(&self, v: u64) -> u64 {
        v & self.size.mask()
    }

    #[inline]
    fn masked_result(&self) -> u64 {
        self.masked(self.result)
    }

    #[inline]
    fn masked_op1(&self) -> u64 {
        self.masked(self.op1)
    }

    #[inline]
    fn masked_op2(&self) -> u64 {
        self.masked(self.op2)
    }

    #[inline]
    fn carry_in_u64(&self) -> u64 {
        (self.carry_in & 1) as u64
    }

    #[inline]
    fn add_rhs(&self) -> u64 {
        self.masked_op2().wrapping_add(self.carry_in_u64()) & self.size.mask()
    }

    #[inline]
    pub fn cf(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        let a = self.masked_op1() as u128;
        let mask = self.size.mask() as u128;
        match self.op {
            LazyFlagOp::Add => {
                let b = self.masked_op2() as u128;
                (a + b) > mask
            }
            LazyFlagOp::Adc => {
                let b = self.masked_op2() as u128;
                (a + b + (self.carry_in_u64() as u128)) > mask
            }
            LazyFlagOp::Sub => {
                let b = self.masked_op2() as u128;
                a < b
            }
            LazyFlagOp::Sbb => {
                let b = self.masked_op2() as u128 + (self.carry_in_u64() as u128);
                a < b
            }
            LazyFlagOp::Logic => false,
            LazyFlagOp::None => false,
        }
    }

    #[inline]
    pub fn zf(&self) -> bool {
        self.masked_result() == 0
    }

    #[inline]
    pub fn sf(&self) -> bool {
        (self.masked_result() & self.size.sign_bit()) != 0
    }

    #[inline]
    pub fn pf(&self) -> bool {
        let b = self.masked_result() as u8;
        (b.count_ones() & 1) == 0
    }

    #[inline]
    pub fn af(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        let a = self.masked_op1();
        let b = match self.op {
            LazyFlagOp::Adc | LazyFlagOp::Sbb => self.add_rhs(),
            LazyFlagOp::Add | LazyFlagOp::Sub => self.masked_op2(),
            LazyFlagOp::Logic | LazyFlagOp::None => 0,
        };
        let r = self.masked_result();
        ((a ^ b ^ r) & 0x10) != 0
    }

    #[inline]
    pub fn of(&self) -> bool {
        if !self.is_active() {
            return false;
        }
        let sign = self.size.sign_bit();
        let a = self.masked_op1();
        let b = match self.op {
            LazyFlagOp::Adc | LazyFlagOp::Sbb => self.add_rhs(),
            LazyFlagOp::Add | LazyFlagOp::Sub => self.masked_op2(),
            LazyFlagOp::Logic | LazyFlagOp::None => 0,
        };
        let r = self.masked_result();
        match self.op {
            LazyFlagOp::Add | LazyFlagOp::Adc => ((a ^ r) & (b ^ r) & sign) != 0,
            LazyFlagOp::Sub | LazyFlagOp::Sbb => ((a ^ b) & (a ^ r) & sign) != 0,
            LazyFlagOp::Logic => false,
            LazyFlagOp::None => false,
        }
    }
}

/// Segment register (visible selector + hidden descriptor cache).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct Segment {
    pub selector: u16,
    pub _pad0: u16,
    pub _pad1: u32,
    pub base: u64,
    pub limit: u32,
    pub access: u32,
}

impl Segment {
    #[inline]
    pub const fn typ(&self) -> u8 {
        (self.access & 0xF) as u8
    }

    #[inline]
    pub const fn s(&self) -> bool {
        (self.access & (1 << 4)) != 0
    }

    #[inline]
    pub const fn dpl(&self) -> u8 {
        ((self.access >> 5) & 0b11) as u8
    }

    #[inline]
    pub const fn is_present(&self) -> bool {
        (self.access & SEG_ACCESS_PRESENT) != 0
    }

    #[inline]
    pub const fn is_unusable(&self) -> bool {
        (self.access & SEG_ACCESS_UNUSABLE) != 0
    }

    #[inline]
    pub const fn is_code(&self) -> bool {
        self.s() && (self.typ() & 0b1000 != 0)
    }

    #[inline]
    pub const fn is_data(&self) -> bool {
        self.s() && (self.typ() & 0b1000 == 0)
    }

    #[inline]
    pub const fn code_conforming(&self) -> bool {
        self.is_code() && (self.typ() & 0b0100 != 0)
    }

    #[inline]
    pub const fn code_readable(&self) -> bool {
        self.is_code() && (self.typ() & 0b0010 != 0)
    }

    #[inline]
    pub const fn data_expand_down(&self) -> bool {
        self.is_data() && (self.typ() & 0b0100 != 0)
    }

    #[inline]
    pub const fn data_writable(&self) -> bool {
        self.is_data() && (self.typ() & 0b0010 != 0)
    }

    #[inline]
    pub const fn is_long(&self) -> bool {
        (self.access & SEG_ACCESS_L) != 0
    }

    #[inline]
    pub const fn is_default_32bit(&self) -> bool {
        (self.access & SEG_ACCESS_DB) != 0
    }

    #[inline]
    pub const fn rpl(&self) -> u8 {
        (self.selector & 0b11) as u8
    }
}

/// User-visible segment registers.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SegmentRegs {
    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub fs: Segment,
    pub gs: Segment,
    pub ss: Segment,
}

/// Descriptor table register (GDTR/IDTR).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DescriptorTable {
    pub limit: u16,
    pub _pad0: u16,
    pub _pad1: u32,
    pub base: u64,
}

/// Descriptor table and system segment state (GDTR/IDTR/LDTR/TR).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DescriptorTables {
    pub gdtr: DescriptorTable,
    pub idtr: DescriptorTable,
    pub ldtr: Segment,
    pub tr: Segment,
}

/// Control register subset needed for Windows 7 boot and paging.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct ControlRegs {
    pub cr0: u64,
    pub cr2: u64,
    pub cr3: u64,
    pub cr4: u64,
    pub cr8: u64,
}

/// Debug registers needed for guest probing (hardware breakpoints).
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DebugRegs {
    /// DR0..DR3
    pub dr: [u64; 4],
    pub dr6: u64,
    pub dr7: u64,
}

/// MSR subset required for Windows 7 syscall/sysenter paths.
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MsrState {
    pub efer: u64,
    pub star: u64,
    pub lstar: u64,
    pub cstar: u64,
    pub fmask: u64,
    pub sysenter_cs: u64,
    pub sysenter_eip: u64,
    pub sysenter_esp: u64,
    pub fs_base: u64,
    pub gs_base: u64,
    pub kernel_gs_base: u64,
    pub apic_base: u64,
    pub tsc: u64,
    pub tsc_aux: u32,
    pub _pad0: u32,
}

impl Default for MsrState {
    fn default() -> Self {
        Self {
            efer: 0,
            star: 0,
            lstar: 0,
            cstar: 0,
            fmask: 0,
            sysenter_cs: 0,
            sysenter_eip: 0,
            sysenter_esp: 0,
            fs_base: 0,
            gs_base: 0,
            kernel_gs_base: 0,
            // Typical reset value: APIC enabled at 0xFEE00000 with BSP bit set.
            // (Intel SDM: IA32_APIC_BASE[11]=global enable, [8]=BSP).
            apic_base: 0xFEE0_0000 | (1 << 11) | (1 << 8),
            tsc: 0,
            tsc_aux: 0,
            _pad0: 0,
        }
    }
}

/// Canonical CPU state.
///
/// Field order is chosen to keep the most commonly accessed state (GPRs, RIP,
/// RFLAGS) in the first cache line and to keep JIT offsets small/simple.
#[repr(C, align(16))]
#[derive(Clone)]
pub struct CpuState {
    /// General purpose registers in architectural order:
    /// RAX, RCX, RDX, RBX, RSP, RBP, RSI, RDI, R8..R15.
    pub gpr: [u64; GPR_COUNT],
    /// Instruction pointer (RIP/EIP/IP). Masked by [`CpuState::mode`] and CS.D
    /// on access.
    pub rip: u64,
    /// Raw RFLAGS value. When [`CpuState::lazy_flags`] is active, the status
    /// flags in `rflags` (CF/PF/AF/ZF/SF/OF) may be stale until committed.
    pub rflags: u64,
    /// Lazy status flag computation state.
    pub lazy_flags: LazyFlags,
    /// Coarse execution mode (bitness + privilege classification).
    pub mode: CpuMode,
    /// Set by `HLT` and cleared by interrupt delivery/reset.
    pub halted: bool,
    /// Interrupt vector recorded when real/v8086 vector delivery transfers control to
    /// a BIOS ROM stub (`HLT; IRET`).
    ///
    /// Tier-0 treats `HLT` as a BIOS hypercall boundary only when this marker is set,
    /// surfacing the event as `BiosInterrupt(vector)` instead of permanently halting.
    pub pending_bios_int: u8,
    pub pending_bios_int_valid: bool,
    pub _pad0: [u8; 4],

    // Segmentation / system tables.
    pub segments: SegmentRegs,
    pub tables: DescriptorTables,

    // Control/debug/MSR state.
    pub control: ControlRegs,
    pub debug: DebugRegs,
    pub msr: MsrState,

    /// x87/MMX architectural state (enough for `FXSAVE`/`FXRSTOR`).
    pub fpu: FpuState,
    /// SSE architectural state (XMM0-15 + MXCSR).
    pub sse: SseState,

    /// A20 gate state (real mode address wrap behaviour).
    ///
    /// When disabled, the A20 address line is forced low, aliasing addresses that
    /// differ only by bit 20 (the 1MiB bit). For addresses below 2MiB this matches
    /// the traditional "1MiB wraparound" behaviour.
    ///
    /// This is only applied by real/v8086 mode linearization helpers.
    pub a20_enabled: bool,
    /// x87 external interrupt indicator for `CR0.NE = 0` mode (IRQ13).
    pub irq13_pending: bool,
    pub _pad_irq13: [u8; 14],
}

impl Default for CpuState {
    fn default() -> Self {
        Self {
            gpr: [0; GPR_COUNT],
            rip: 0,
            rflags: RFLAGS_RESERVED1,
            lazy_flags: LazyFlags::default(),
            mode: CpuMode::Real,
            halted: false,
            pending_bios_int: 0,
            pending_bios_int_valid: false,
            _pad0: [0; 4],
            segments: SegmentRegs::default(),
            tables: DescriptorTables::default(),
            control: ControlRegs::default(),
            debug: DebugRegs::default(),
            msr: MsrState::default(),
            fpu: FpuState::default(),
            sse: SseState::default(),
            a20_enabled: true,
            irq13_pending: false,
            _pad_irq13: [0; 14],
        }
    }
}

impl fmt::Debug for CpuState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CpuState")
            .field("mode", &self.mode)
            .field("rip", &self.rip())
            .field("rflags", &self.rflags_snapshot())
            .finish_non_exhaustive()
    }
}

impl CpuState {
    /// Constructs a new CPU state with a coarse initial mode.
    ///
    /// This is primarily used by unit tests and the tier-0 interpreter. More
    /// complete mode transitions should be performed by updating CR0/CR4/EFER
    /// and segment caches, then calling [`CpuState::update_mode`].
    pub fn new(mode: CpuMode) -> Self {
        let mut state = Self {
            mode,
            ..Self::default()
        };

        // Configure CS cache bits so helpers like `bitness()` and `ip_mask()`
        // behave consistently with the requested coarse mode.
        match mode {
            CpuMode::Real | CpuMode::Vm86 => {
                state.segments.cs.access &= !(SEG_ACCESS_DB | SEG_ACCESS_L);
            }
            CpuMode::Protected => {
                state.segments.cs.access |= SEG_ACCESS_DB;
                state.segments.cs.access &= !SEG_ACCESS_L;
            }
            CpuMode::Long => {
                state.segments.cs.access |= SEG_ACCESS_L;
                // CS.D is ignored for 64-bit code but keeping it set avoids
                // accidentally selecting 16-bit widths in helper code.
                state.segments.cs.access |= SEG_ACCESS_DB;
            }
        }

        state
    }

    #[inline]
    pub fn set_pending_bios_int(&mut self, vector: u8) {
        self.pending_bios_int = vector;
        self.pending_bios_int_valid = true;
    }

    #[inline]
    pub fn take_pending_bios_int(&mut self) -> Option<u8> {
        if self.pending_bios_int_valid {
            self.pending_bios_int_valid = false;
            Some(self.pending_bios_int)
        } else {
            None
        }
    }

    #[inline]
    pub fn clear_pending_bios_int(&mut self) {
        self.pending_bios_int_valid = false;
    }

    /// Returns the current effective code bitness (16/32/64).
    pub fn bitness(&self) -> u32 {
        match self.mode {
            CpuMode::Long => 64,
            CpuMode::Protected => {
                if self.segments.cs.is_default_32bit() {
                    32
                } else {
                    16
                }
            }
            CpuMode::Real | CpuMode::Vm86 => 16,
        }
    }

    /// Legacy name for the instruction pointer accessor.
    #[inline]
    pub fn rip(&self) -> u64 {
        self.get_ip()
    }

    /// Legacy name for setting the instruction pointer.
    #[inline]
    pub fn set_rip(&mut self, rip: u64) {
        self.set_ip(rip)
    }

    #[inline]
    pub fn advance_rip(&mut self, delta: u64) {
        self.advance_ip(delta)
    }

    /// Returns the current RFLAGS value with lazy status flags materialized.
    #[inline]
    pub fn rflags(&self) -> u64 {
        self.rflags_snapshot()
    }

    /// Returns the current privilege level (CPL).
    #[inline]
    pub fn cpl(&self) -> u8 {
        match self.mode {
            CpuMode::Real => 0,
            // Virtual-8086 mode always executes with CPL=3, regardless of the
            // low bits of the real-mode segment selectors.
            CpuMode::Vm86 => 3,
            _ => self.segments.cs.rpl(),
        }
    }

    /// Recomputes [`CpuState::mode`] from control registers, EFER and CS
    /// descriptor cache.
    ///
    /// This should be called after writes to CR0/CR4/EFER or after loading CS.
    #[inline]
    pub fn update_mode(&mut self) -> CpuMode {
        if (self.control.cr0 & CR0_PE) == 0 {
            // IA32_EFER.LMA is cleared whenever protected mode is disabled.
            self.msr.efer &= !EFER_LMA;
            self.mode = CpuMode::Real;
            return self.mode;
        }

        // Keep IA32_EFER.LMA coherent with the architectural enable conditions.
        //
        // On real hardware LMA becomes active once paging is enabled with EFER.LME=1 and CR4.PAE=1
        // (and clears as soon as the conditions are no longer met).
        let ia32e_enabled = (self.msr.efer & EFER_LME) != 0
            && (self.control.cr0 & CR0_PG) != 0
            && (self.control.cr4 & CR4_PAE) != 0;
        if ia32e_enabled {
            self.msr.efer |= EFER_LMA;
        } else {
            self.msr.efer &= !EFER_LMA;
        }

        if ia32e_enabled && self.segments.cs.is_long() {
            self.mode = CpuMode::Long;
            return self.mode;
        }

        if (self.rflags & RFLAGS_VM) != 0 {
            self.mode = CpuMode::Vm86;
        } else {
            self.mode = CpuMode::Protected;
        }

        self.mode
    }

    /// Synchronize an [`aero_mmu::Mmu`] instance with the paging-related CPU state.
    ///
    /// This is used by paging-aware [`crate::mem::CpuBus`] implementations to
    /// observe changes to CR0/CR3/CR4/EFER.
    pub fn sync_mmu(&self, mmu: &mut aero_mmu::Mmu) {
        if mmu.cr0() != self.control.cr0 {
            mmu.set_cr0(self.control.cr0);
        }
        if mmu.cr3() != self.control.cr3 {
            mmu.set_cr3(self.control.cr3);
        }
        if mmu.cr4() != self.control.cr4 {
            mmu.set_cr4(self.control.cr4);
        }
        if mmu.efer() != self.msr.efer {
            mmu.set_efer(self.msr.efer);
        }
    }

    /// Apply architectural side effects of an exception raised by the interpreter.
    ///
    /// For now this only models CR2 updates on #PF.
    pub fn apply_exception_side_effects(&mut self, exception: &Exception) {
        if let Exception::PageFault { addr, .. } = exception {
            self.control.cr2 = *addr;
        }
    }

    #[inline]
    fn ip_mask(&self) -> u64 {
        match self.mode {
            CpuMode::Long => u64::MAX,
            CpuMode::Protected => {
                if self.segments.cs.is_default_32bit() {
                    0xFFFF_FFFF
                } else {
                    0xFFFF
                }
            }
            CpuMode::Real | CpuMode::Vm86 => 0xFFFF,
        }
    }

    /// Returns the instruction pointer masked to the current effective width.
    #[inline]
    pub fn get_ip(&self) -> u64 {
        self.rip & self.ip_mask()
    }

    /// Sets the instruction pointer, masking to the current effective width.
    #[inline]
    pub fn set_ip(&mut self, ip: u64) {
        self.rip = ip & self.ip_mask();
    }

    /// Advances IP by `delta` (wrapping within the current effective width).
    #[inline]
    pub fn advance_ip(&mut self, delta: u64) {
        let next = self.get_ip().wrapping_add(delta) & self.ip_mask();
        self.rip = next;
    }

    // ---- GPR accessors -----------------------------------------------------

    #[inline]
    pub fn read_gpr64(&self, index: usize) -> u64 {
        self.gpr[index]
    }

    #[inline]
    pub fn write_gpr64(&mut self, index: usize, value: u64) {
        self.gpr[index] = value;
    }

    /// Reads the low 32 bits of a GPR.
    #[inline]
    pub fn read_gpr32(&self, index: usize) -> u32 {
        self.gpr[index] as u32
    }

    /// Writes the low 32 bits of a GPR, zero-extending to 64-bit.
    #[inline]
    pub fn write_gpr32(&mut self, index: usize, value: u32) {
        self.gpr[index] = value as u64;
    }

    #[inline]
    pub fn read_gpr16(&self, index: usize) -> u16 {
        self.gpr[index] as u16
    }

    #[inline]
    pub fn write_gpr16(&mut self, index: usize, value: u16) {
        let old = self.gpr[index];
        self.gpr[index] = (old & !0xFFFF) | (value as u64);
    }

    /// Reads an 8-bit GPR subregister.
    ///
    /// `rex_present` controls the legacy high-byte registers mapping:
    /// - If `rex_present == false`, indices 4..=7 map to AH/CH/DH/BH (bits 8..15
    ///   of RAX/RCX/RDX/RBX).
    /// - If `rex_present == true`, indices 4..=7 map to SPL/BPL/SIL/DIL (low
    ///   byte of RSP/RBP/RSI/RDI).
    #[inline]
    pub fn read_gpr8(&self, index: usize, rex_present: bool) -> u8 {
        let (base, shift) = gpr8_mapping(index, rex_present);
        ((self.gpr[base] >> shift) & 0xFF) as u8
    }

    /// Writes an 8-bit GPR subregister.
    ///
    /// See [`CpuState::read_gpr8`] for the `rex_present` high-byte mapping.
    #[inline]
    pub fn write_gpr8(&mut self, index: usize, rex_present: bool, value: u8) {
        let (base, shift) = gpr8_mapping(index, rex_present);
        let mask = !(0xFFu64 << shift);
        let old = self.gpr[base] & mask;
        self.gpr[base] = old | ((value as u64) << shift);
    }

    // ---- Flag accessors ----------------------------------------------------

    /// Returns the current value of a flag bit, consulting [`CpuState::lazy_flags`]
    /// if present.
    #[inline]
    pub fn get_flag(&self, mask: u64) -> bool {
        if self.lazy_flags.is_active() {
            match mask {
                RFLAGS_CF => return self.lazy_flags.cf(),
                RFLAGS_PF => return self.lazy_flags.pf(),
                RFLAGS_AF => return self.lazy_flags.af(),
                RFLAGS_ZF => return self.lazy_flags.zf(),
                RFLAGS_SF => return self.lazy_flags.sf(),
                RFLAGS_OF => return self.lazy_flags.of(),
                _ => {}
            }
        }
        (self.rflags & mask) != 0
    }

    /// Sets/clears a flag bit.
    ///
    /// This commits any pending lazy flags first so the write applies on top of
    /// the architecturally correct flag state.
    #[inline]
    pub fn set_flag(&mut self, mask: u64, value: bool) {
        self.commit_lazy_flags();
        if value {
            self.rflags |= mask;
        } else {
            self.rflags &= !mask;
        }
        self.rflags |= RFLAGS_RESERVED1;
    }

    /// Returns a snapshot of RFLAGS with lazy status flags materialized.
    #[inline]
    pub fn rflags_snapshot(&self) -> u64 {
        if !self.lazy_flags.is_active() {
            return self.rflags | RFLAGS_RESERVED1;
        }
        let mut r = self.rflags | RFLAGS_RESERVED1;
        set_bit(&mut r, RFLAGS_CF, self.lazy_flags.cf());
        set_bit(&mut r, RFLAGS_PF, self.lazy_flags.pf());
        set_bit(&mut r, RFLAGS_AF, self.lazy_flags.af());
        set_bit(&mut r, RFLAGS_ZF, self.lazy_flags.zf());
        set_bit(&mut r, RFLAGS_SF, self.lazy_flags.sf());
        set_bit(&mut r, RFLAGS_OF, self.lazy_flags.of());
        r
    }

    /// Commits any pending lazy flags into [`CpuState::rflags`], clearing
    /// [`CpuState::lazy_flags`].
    #[inline]
    pub fn commit_lazy_flags(&mut self) {
        if !self.lazy_flags.is_active() {
            self.rflags |= RFLAGS_RESERVED1;
            return;
        }
        let snapshot = self.rflags_snapshot();
        self.rflags = snapshot;
        self.lazy_flags.clear();
    }

    /// Sets RFLAGS directly, invalidating any pending lazy flags.
    #[inline]
    pub fn set_rflags(&mut self, value: u64) {
        self.rflags = value | RFLAGS_RESERVED1;
        self.lazy_flags.clear();
    }

    // ---- x87/SSE state management (FXSAVE/FXRSTOR, MXCSR) -----------------

    /// Implements `FNINIT` / `FINIT`.
    pub fn fninit(&mut self) {
        self.fpu.reset();
    }

    /// Implements `EMMS` (empty MMX state).
    pub fn emms(&mut self) {
        self.fpu.emms();
    }

    /// Implements `STMXCSR m32`.
    pub fn stmxcsr(&self, dst: &mut [u8; 4]) {
        crate::fxsave::stmxcsr(&self.sse, dst);
    }

    /// Implements `LDMXCSR m32`.
    pub fn ldmxcsr(&mut self, src: &[u8; 4]) -> Result<(), FxStateError> {
        crate::fxsave::ldmxcsr(&mut self.sse, src)
    }

    /// `STMXCSR` convenience wrapper that writes MXCSR via [`crate::mem::CpuBus`].
    pub fn stmxcsr_to_mem<B: CpuBus>(&self, bus: &mut B, addr: u64) -> Result<(), Exception> {
        if addr & 0b11 != 0 {
            return Err(Exception::gp0());
        }
        write_u32_wrapped(self, bus, addr, self.sse.mxcsr)
    }

    /// `LDMXCSR` convenience wrapper that loads MXCSR via [`crate::mem::CpuBus`].
    pub fn ldmxcsr_from_mem<B: CpuBus>(&mut self, bus: &mut B, addr: u64) -> Result<(), Exception> {
        if addr & 0b11 != 0 {
            return Err(Exception::gp0());
        }
        let mut value = read_u32_wrapped(self, bus, addr)?;
        if (self.control.cr4 & CR4_OSXMMEXCPT) == 0 {
            // Match the legacy system instruction surface: if the guest OS hasn't
            // enabled SIMD FP exception delivery, keep all exception masks set so
            // we never have to inject #XM/#XF.
            value |= MXCSR_EXCEPTION_MASK;
        }
        self.sse.set_mxcsr(value)?;
        Ok(())
    }

    /// Implements the legacy (32-bit) `FXSAVE m512byte` memory image.
    pub fn fxsave32(&self, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
        crate::fxsave::fxsave_legacy(&self.fpu, &self.sse, dst);
    }

    /// Implements the legacy (32-bit) `FXRSTOR m512byte` memory image.
    pub fn fxrstor32(&mut self, src: &[u8; FXSAVE_AREA_SIZE]) -> Result<(), FxStateError> {
        crate::fxsave::fxrstor_legacy(&mut self.fpu, &mut self.sse, src)
    }

    /// Backwards-compatible alias for `fxsave32`.
    pub fn fxsave(&self, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
        self.fxsave32(dst);
    }

    /// Backwards-compatible alias for `fxrstor32`.
    pub fn fxrstor(&mut self, src: &[u8; FXSAVE_AREA_SIZE]) -> Result<(), FxStateError> {
        self.fxrstor32(src)
    }

    /// Implements the 64-bit `FXSAVE64 m512byte` memory image.
    pub fn fxsave64(&self, dst: &mut [u8; FXSAVE_AREA_SIZE]) {
        crate::fxsave::fxsave64(&self.fpu, &self.sse, dst);
    }

    /// Implements the 64-bit `FXRSTOR64 m512byte` memory image.
    pub fn fxrstor64(&mut self, src: &[u8; FXSAVE_AREA_SIZE]) -> Result<(), FxStateError> {
        crate::fxsave::fxrstor64(&mut self.fpu, &mut self.sse, src)
    }

    /// `FXSAVE` convenience wrapper that writes the 512-byte (legacy) image into guest memory via
    /// [`crate::mem::CpuBus`].
    pub fn fxsave_to_mem<B: CpuBus>(&self, bus: &mut B, addr: u64) -> Result<(), Exception> {
        if addr & 0xF != 0 {
            return Err(Exception::gp0());
        }
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        self.fxsave32(&mut image);
        write_bytes_wrapped(self, bus, addr, &image)?;
        Ok(())
    }

    /// `FXSAVE64` convenience wrapper that writes the 512-byte (64-bit) image into guest memory via
    /// [`crate::mem::CpuBus`].
    pub fn fxsave64_to_mem<B: CpuBus>(&self, bus: &mut B, addr: u64) -> Result<(), Exception> {
        if addr & 0xF != 0 {
            return Err(Exception::gp0());
        }
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        self.fxsave64(&mut image);
        write_bytes_wrapped(self, bus, addr, &image)?;
        Ok(())
    }

    /// `FXRSTOR` convenience wrapper that reads the 512-byte (legacy) image from guest memory via
    /// [`crate::mem::CpuBus`].
    pub fn fxrstor_from_mem<B: CpuBus>(&mut self, bus: &mut B, addr: u64) -> Result<(), Exception> {
        if addr & 0xF != 0 {
            return Err(Exception::gp0());
        }
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        read_bytes_wrapped(self, bus, addr, &mut image)?;

        if (self.control.cr4 & CR4_OSXMMEXCPT) == 0 {
            let mxcsr =
                u32::from_le_bytes(image[24..28].try_into().unwrap()) | MXCSR_EXCEPTION_MASK;
            image[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        }

        self.fxrstor32(&image)?;

        Ok(())
    }

    /// `FXRSTOR64` convenience wrapper that reads the 512-byte (64-bit) image from guest memory via
    /// [`crate::mem::CpuBus`].
    pub fn fxrstor64_from_mem<B: CpuBus>(
        &mut self,
        bus: &mut B,
        addr: u64,
    ) -> Result<(), Exception> {
        if addr & 0xF != 0 {
            return Err(Exception::gp0());
        }
        let mut image = [0u8; FXSAVE_AREA_SIZE];
        read_bytes_wrapped(self, bus, addr, &mut image)?;

        if (self.control.cr4 & CR4_OSXMMEXCPT) == 0 {
            let mxcsr =
                u32::from_le_bytes(image[24..28].try_into().unwrap()) | MXCSR_EXCEPTION_MASK;
            image[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        }

        self.fxrstor64(&image)?;
        Ok(())
    }

    // ---- Compatibility helpers for tier-0 interpreter ---------------------

    #[inline]
    pub fn irq13_pending(&self) -> bool {
        self.irq13_pending
    }

    #[inline]
    pub fn set_irq13_pending(&mut self, pending: bool) {
        self.irq13_pending = pending;
    }

    /// Returns the segment base for a segment register.
    ///
    /// In long mode, only FS/GS bases are used for linear address formation
    /// (other segment bases are treated as 0).
    pub fn seg_base_reg(&self, seg: Register) -> u64 {
        use Register::*;
        match self.mode {
            CpuMode::Long => match seg {
                FS => self.msr.fs_base,
                GS => self.msr.gs_base,
                _ => 0,
            },
            _ => match seg {
                ES => self.segments.es.base,
                CS => self.segments.cs.base,
                SS => self.segments.ss.base,
                DS => self.segments.ds.base,
                FS => self.segments.fs.base,
                GS => self.segments.gs.base,
                _ => 0,
            },
        }
    }

    /// Reads a decoded register operand.
    ///
    /// The result is zero-extended to 64 bits.
    pub fn read_reg(&self, reg: Register) -> u64 {
        if let Some((idx, bits, high8)) = gpr_info(reg) {
            let full = self.gpr[idx];
            return match (bits, high8) {
                (8, false) => full & 0xFF,
                (8, true) => (full >> 8) & 0xFF,
                (16, _) => full & 0xFFFF,
                (32, _) => full & 0xFFFF_FFFF,
                (64, _) => full,
                _ => 0,
            };
        }

        match reg {
            Register::ES => self.segments.es.selector as u64,
            Register::CS => self.segments.cs.selector as u64,
            Register::SS => self.segments.ss.selector as u64,
            Register::DS => self.segments.ds.selector as u64,
            Register::FS => self.segments.fs.selector as u64,
            Register::GS => self.segments.gs.selector as u64,
            Register::RIP => self.get_ip(),
            Register::EIP => self.get_ip() & 0xFFFF_FFFF,
            _ => 0,
        }
    }

    /// Writes a decoded register operand.
    pub fn write_reg(&mut self, reg: Register, val: u64) {
        if let Some((idx, bits, high8)) = gpr_info(reg) {
            let cur = self.gpr[idx];
            self.gpr[idx] = match (bits, high8) {
                (64, _) => val,
                // Writes to a 32-bit GPR clear the upper 32 bits, even in 64-bit mode.
                (32, _) => val & 0xFFFF_FFFF,
                (16, _) => (cur & !0xFFFF) | (val & 0xFFFF),
                (8, false) => (cur & !0xFF) | (val & 0xFF),
                (8, true) => (cur & !0xFF00) | ((val & 0xFF) << 8),
                _ => cur,
            };
            return;
        }

        match reg {
            Register::ES => {
                Self::write_segment_register(self.mode, &mut self.segments.es, val as u16)
            }
            Register::CS => {
                Self::write_segment_register(self.mode, &mut self.segments.cs, val as u16)
            }
            Register::SS => {
                Self::write_segment_register(self.mode, &mut self.segments.ss, val as u16)
            }
            Register::DS => {
                Self::write_segment_register(self.mode, &mut self.segments.ds, val as u16)
            }
            Register::FS => {
                Self::write_segment_register(self.mode, &mut self.segments.fs, val as u16)
            }
            Register::GS => {
                Self::write_segment_register(self.mode, &mut self.segments.gs, val as u16)
            }
            _ => {}
        }

        if matches!(
            reg,
            Register::ES | Register::CS | Register::SS | Register::DS | Register::FS | Register::GS
        ) {
            return;
        }

        match reg {
            Register::RIP | Register::EIP => self.set_ip(val),
            _ => {}
        }
    }

    fn write_segment_register(mode: CpuMode, seg: &mut Segment, selector: u16) {
        seg.selector = selector;
        // Tier-0 currently only supports real-mode segment semantics. Protected/long
        // mode segment loads require descriptor lookup and are delegated to assists.
        if matches!(mode, CpuMode::Real | CpuMode::Vm86) {
            seg.base = (selector as u64) << 4;
            seg.limit = 0xFFFF;
            seg.access = 0;
        }
    }

    pub fn stack_ptr_reg(&self) -> Register {
        match self.bitness() {
            16 => Register::SP,
            32 => Register::ESP,
            _ => Register::RSP,
        }
    }

    pub fn stack_ptr_bits(&self) -> u32 {
        match self.bitness() {
            16 => 16,
            32 => 32,
            _ => 64,
        }
    }

    pub fn stack_ptr(&self) -> u64 {
        let reg = self.stack_ptr_reg();
        self.read_reg(reg) & mask_bits(self.stack_ptr_bits())
    }

    pub fn set_stack_ptr(&mut self, val: u64) {
        let reg = self.stack_ptr_reg();
        let bits = self.stack_ptr_bits();
        let v = val & mask_bits(bits);
        self.write_reg(reg, v);
    }
    /// Applies architectural linear-address masking.
    ///
    /// - In non-long modes, linear addresses are 32-bit and wrap around on overflow.
    /// - In real/v8086 mode when the A20 gate is disabled, addresses also wrap at 1MiB.
    #[inline]
    pub fn apply_a20(&self, addr: u64) -> u64 {
        let mut addr = addr;
        if self.mode != CpuMode::Long {
            addr &= 0xFFFF_FFFF;
        }
        if !self.a20_enabled && matches!(self.mode, CpuMode::Real | CpuMode::Vm86) {
            addr &= !(1u64 << 20);
        }
        addr
    }
}

#[inline]
fn set_bit(bits: &mut u64, mask: u64, value: bool) {
    if value {
        *bits |= mask;
    } else {
        *bits &= !mask;
    }
}

#[inline]
fn gpr8_mapping(index: usize, rex_present: bool) -> (usize, u32) {
    debug_assert!(index < GPR_COUNT);
    match index {
        0..=3 => (index, 0),
        4..=7 => {
            if rex_present {
                (index, 0)
            } else {
                (index - 4, 8)
            }
        }
        8..=15 => (index, 0),
        _ => unreachable!(),
    }
}

/// Returns a mask with the low `bits` bits set.
pub fn mask_bits(bits: u32) -> u64 {
    match bits {
        8 => 0xFF,
        16 => 0xFFFF,
        32 => 0xFFFF_FFFF,
        64 => u64::MAX,
        _ => {
            if bits >= 64 {
                u64::MAX
            } else {
                (1u64 << bits) - 1
            }
        }
    }
}

fn gpr_info(reg: Register) -> Option<(usize, u32, bool)> {
    use Register::*;
    let (idx, bits, high8) = match reg {
        AL => (0, 8, false),
        CL => (1, 8, false),
        DL => (2, 8, false),
        BL => (3, 8, false),
        AH => (0, 8, true),
        CH => (1, 8, true),
        DH => (2, 8, true),
        BH => (3, 8, true),
        SPL => (4, 8, false),
        BPL => (5, 8, false),
        SIL => (6, 8, false),
        DIL => (7, 8, false),
        R8L => (8, 8, false),
        R9L => (9, 8, false),
        R10L => (10, 8, false),
        R11L => (11, 8, false),
        R12L => (12, 8, false),
        R13L => (13, 8, false),
        R14L => (14, 8, false),
        R15L => (15, 8, false),

        AX => (0, 16, false),
        CX => (1, 16, false),
        DX => (2, 16, false),
        BX => (3, 16, false),
        SP => (4, 16, false),
        BP => (5, 16, false),
        SI => (6, 16, false),
        DI => (7, 16, false),
        R8W => (8, 16, false),
        R9W => (9, 16, false),
        R10W => (10, 16, false),
        R11W => (11, 16, false),
        R12W => (12, 16, false),
        R13W => (13, 16, false),
        R14W => (14, 16, false),
        R15W => (15, 16, false),

        EAX => (0, 32, false),
        ECX => (1, 32, false),
        EDX => (2, 32, false),
        EBX => (3, 32, false),
        ESP => (4, 32, false),
        EBP => (5, 32, false),
        ESI => (6, 32, false),
        EDI => (7, 32, false),
        R8D => (8, 32, false),
        R9D => (9, 32, false),
        R10D => (10, 32, false),
        R11D => (11, 32, false),
        R12D => (12, 32, false),
        R13D => (13, 32, false),
        R14D => (14, 32, false),
        R15D => (15, 32, false),

        RAX => (0, 64, false),
        RCX => (1, 64, false),
        RDX => (2, 64, false),
        RBX => (3, 64, false),
        RSP => (4, 64, false),
        RBP => (5, 64, false),
        RSI => (6, 64, false),
        RDI => (7, 64, false),
        R8 => (8, 64, false),
        R9 => (9, 64, false),
        R10 => (10, 64, false),
        R11 => (11, 64, false),
        R12 => (12, 64, false),
        R13 => (13, 64, false),
        R14 => (14, 64, false),
        R15 => (15, 64, false),

        _ => return core::option::Option::None,
    };
    Some((idx, bits, high8))
}

// Compile-time ABI checks for all targets (including wasm32).
//
// Unit tests below additionally validate the same invariants via `memoffset`,
// guarding against accidental refactors.
const _: () = {
    use core::mem::{align_of, offset_of, size_of};

    assert!(offset_of!(CpuState, gpr) == CPU_GPR_BASE_OFF);
    assert!(offset_of!(CpuState, rip) == CPU_RIP_OFF);
    assert!(offset_of!(CpuState, rflags) == CPU_RFLAGS_OFF);

    assert!(CPU_GPR_OFF[0] == CPU_GPR_BASE_OFF);
    assert!(CPU_GPR_OFF[15] == CPU_GPR_BASE_OFF + 15 * 8);

    assert!(offset_of!(CpuState, sse) + offset_of!(SseState, xmm) == CPU_XMM_OFF[0]);
    assert!(offset_of!(CpuState, sse) + offset_of!(SseState, xmm) + 15 * 16 == CPU_XMM_OFF[15]);

    assert!(size_of::<CpuState>() == CPU_STATE_SIZE);
    assert!(align_of::<CpuState>() == CPU_STATE_ALIGN);
};

// ---- Tests -----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use memoffset::offset_of;

    #[test]
    fn partial_register_semantics() {
        let mut cpu = CpuState::default();

        cpu.write_gpr64(gpr::RAX, 0xFFFF_FFFF_0000_0000);
        cpu.write_gpr32(gpr::RAX, 0x1234_5678);
        assert_eq!(cpu.read_gpr64(gpr::RAX), 0x1234_5678);

        cpu.write_gpr64(gpr::RAX, 0x1122_3344_5566_7788);
        cpu.write_gpr16(gpr::RAX, 0xABCD);
        assert_eq!(cpu.read_gpr64(gpr::RAX), 0x1122_3344_5566_ABCD);

        cpu.write_gpr64(gpr::RAX, 0x0000_0000_0000_1122);
        cpu.write_gpr8(4, false, 0x33); // AH
        assert_eq!(cpu.read_gpr64(gpr::RAX), 0x0000_0000_0000_3322);
        assert_eq!(cpu.read_gpr8(4, false), 0x33);

        cpu.write_gpr64(gpr::RSP, 0);
        cpu.write_gpr8(4, true, 0x44); // SPL (REX present)
        assert_eq!(cpu.read_gpr64(gpr::RSP), 0x44);
        // REX should have blocked AH access.
        assert_eq!(cpu.read_gpr64(gpr::RAX), 0x0000_0000_0000_3322);
        assert_eq!(cpu.read_gpr8(4, true), 0x44);
    }

    #[test]
    fn lazy_flags_materialize_and_commit() {
        let mut cpu = CpuState::default();
        cpu.set_flag(RFLAGS_DF, true);

        // 0xFF + 1 = 0x00 (8-bit)
        cpu.lazy_flags = LazyFlags::for_add(OperandSize::Byte, 0xFF, 1, 0x00);
        assert!(cpu.get_flag(RFLAGS_CF));
        assert!(cpu.get_flag(RFLAGS_ZF));
        assert!(!cpu.get_flag(RFLAGS_SF));
        assert!(!cpu.get_flag(RFLAGS_OF));
        assert!(cpu.get_flag(RFLAGS_PF));
        assert!(cpu.get_flag(RFLAGS_AF));
        // Unaffected flags come from rflags.
        assert!(cpu.get_flag(RFLAGS_DF));

        cpu.commit_lazy_flags();
        assert!(!cpu.lazy_flags.is_active());
        assert!(cpu.rflags & RFLAGS_DF != 0);
        assert!(cpu.rflags & RFLAGS_CF != 0);
        assert!(cpu.rflags & RFLAGS_ZF != 0);

        // 0x00 - 1 = 0xFF (8-bit)
        cpu.lazy_flags = LazyFlags::for_sub(OperandSize::Byte, 0x00, 1, 0xFF);
        assert!(cpu.get_flag(RFLAGS_CF));
        assert!(!cpu.get_flag(RFLAGS_ZF));
        assert!(cpu.get_flag(RFLAGS_SF));
        assert!(!cpu.get_flag(RFLAGS_OF));
        assert!(cpu.get_flag(RFLAGS_PF));
        assert!(cpu.get_flag(RFLAGS_AF));

        // Logic op clears CF/OF and computes ZF/SF/PF.
        cpu.lazy_flags = LazyFlags::for_logic(OperandSize::Byte, 0);
        assert!(!cpu.get_flag(RFLAGS_CF));
        assert!(!cpu.get_flag(RFLAGS_OF));
        assert!(cpu.get_flag(RFLAGS_ZF));
        assert!(!cpu.get_flag(RFLAGS_SF));
        assert!(cpu.get_flag(RFLAGS_PF));
        assert!(!cpu.get_flag(RFLAGS_AF));

        // ADC chain: 0 + 0 + CF(1) = 1.
        cpu.set_rflags(RFLAGS_CF);
        cpu.lazy_flags = LazyFlags::for_adc(OperandSize::Byte, 0, 0, cpu.get_flag(RFLAGS_CF), 1);
        assert!(!cpu.get_flag(RFLAGS_CF));
        assert!(!cpu.get_flag(RFLAGS_ZF));
        assert_eq!(cpu.lazy_flags.carry_in, 1);
    }

    #[test]
    fn jit_offsets_are_stable() {
        assert_eq!(offset_of!(CpuState, gpr), CPU_GPR_BASE_OFF);
        assert_eq!(offset_of!(CpuState, rip), CPU_RIP_OFF);
        assert_eq!(offset_of!(CpuState, rflags), CPU_RFLAGS_OFF);

        for (i, off) in CPU_GPR_OFF.iter().enumerate() {
            assert_eq!(*off, CPU_GPR_BASE_OFF + i * 8);
        }

        let xmm_base = offset_of!(CpuState, sse) + offset_of!(SseState, xmm);
        for (i, off) in CPU_XMM_OFF.iter().enumerate() {
            assert_eq!(*off, xmm_base + i * core::mem::size_of::<u128>());
        }
    }

    #[test]
    fn seg_base_reg_uses_msr_bases_in_long_mode() {
        use aero_x86::Register;

        let mut cpu = CpuState::new(CpuMode::Long);
        cpu.msr.fs_base = 0x0000_1111_2222_3333;
        cpu.msr.gs_base = 0xFFFF_8000_0000_1000;

        // Segment caches are still populated by descriptor loads, but in long mode
        // address formation uses the MSR-backed bases.
        cpu.segments.fs.base = 0xDEAD_BEEF;
        cpu.segments.gs.base = 0xCAFE_BABE;

        assert_eq!(cpu.seg_base_reg(Register::FS), cpu.msr.fs_base);
        assert_eq!(cpu.seg_base_reg(Register::GS), cpu.msr.gs_base);
    }

    #[test]
    fn update_mode_tracks_efer_lma() {
        let mut cpu = CpuState::new(CpuMode::Bit32);

        // Enable protected mode first.
        cpu.control.cr0 |= CR0_PE;
        cpu.update_mode();
        assert_eq!(cpu.mode, CpuMode::Protected);

        // Enable IA-32e conditions (EFER.LME + CR4.PAE + CR0.PG). In a full CPU model this would
        // place the core in compatibility mode until a 64-bit code segment is loaded. We only
        // model the LMA bit here.
        cpu.msr.efer |= EFER_LME;
        cpu.control.cr4 |= CR4_PAE;
        cpu.control.cr0 |= CR0_PG;
        cpu.update_mode();
        assert_ne!(cpu.msr.efer & EFER_LMA, 0);

        // Clearing paging should clear LMA.
        cpu.control.cr0 &= !CR0_PG;
        cpu.update_mode();
        assert_eq!(cpu.msr.efer & EFER_LMA, 0);

        // Disabling protected mode also clears LMA.
        cpu.control.cr0 &= !CR0_PE;
        cpu.update_mode();
        assert_eq!(cpu.mode, CpuMode::Real);
        assert_eq!(cpu.msr.efer & EFER_LMA, 0);
    }

    #[test]
    fn vm86_cpl_is_always_three() {
        let mut cpu = CpuState::new(CpuMode::Vm86);
        cpu.segments.cs.selector = 0x0000;
        assert_eq!(cpu.cpl(), 3);

        cpu.segments.cs.selector = 0x1234;
        assert_eq!(cpu.cpl(), 3);
    }
}
