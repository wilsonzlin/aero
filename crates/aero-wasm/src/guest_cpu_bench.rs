use aero_cpu_core::interp::tier0::exec::{run_batch, BatchExit};
use aero_cpu_core::mem::{CpuBus, FlatTestBus};
use aero_cpu_core::state::{gpr, CpuMode, CpuState};

/// Canonical iteration count used by PF-008 checksum verification.
pub const ITERS_PER_RUN_CANONICAL: u32 = 10_000;

// ---------------------------------------------------------------------------
// Flat address space layout (PF-008)
// ---------------------------------------------------------------------------

pub const CODE_BASE: u64 = 0x1000;

pub const SCRATCH_BASE: u64 = 0x2000;
pub const SCRATCH_LEN: usize = 0x1000;

pub const STACK_BASE: u64 = 0x9000;
pub const STACK_LEN: u64 = 0x1000;
pub const STACK_TOP: u64 = STACK_BASE + STACK_LEN;

pub const RET_SENTINEL32: u32 = 0xFFFF_F000;
pub const RET_SENTINEL64: u64 = 0xFFFF_FFFF_FFFF_F000;

// ---------------------------------------------------------------------------
// Payload definitions (single source of truth)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum GuestCpuBenchVariant {
    Alu64,
    Alu32,
    BranchPred64,
    BranchPred32,
    BranchUnpred64,
    BranchUnpred32,
    MemSeq64,
    MemSeq32,
    MemStride64,
    MemStride32,
    CallRet64,
    CallRet32,
}

impl GuestCpuBenchVariant {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Alu64 => "alu64",
            Self::Alu32 => "alu32",
            Self::BranchPred64 => "branch_pred64",
            Self::BranchPred32 => "branch_pred32",
            Self::BranchUnpred64 => "branch_unpred64",
            Self::BranchUnpred32 => "branch_unpred32",
            Self::MemSeq64 => "mem_seq64",
            Self::MemSeq32 => "mem_seq32",
            Self::MemStride64 => "mem_stride64",
            Self::MemStride32 => "mem_stride32",
            Self::CallRet64 => "call_ret64",
            Self::CallRet32 => "call_ret32",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "alu64" => Some(Self::Alu64),
            "alu32" => Some(Self::Alu32),
            "branch_pred64" => Some(Self::BranchPred64),
            "branch_pred32" => Some(Self::BranchPred32),
            "branch_unpred64" => Some(Self::BranchUnpred64),
            "branch_unpred32" => Some(Self::BranchUnpred32),
            "mem_seq64" => Some(Self::MemSeq64),
            "mem_seq32" => Some(Self::MemSeq32),
            "mem_stride64" => Some(Self::MemStride64),
            "mem_stride32" => Some(Self::MemStride32),
            "call_ret64" => Some(Self::CallRet64),
            "call_ret32" => Some(Self::CallRet32),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct GuestCpuPayload {
    pub variant: GuestCpuBenchVariant,
    pub bitness: u32,
    pub bytes: &'static [u8],
    pub expected_checksum_10k: u64,
    pub uses_scratch: bool,
}

// 1) alu64
pub const ALU64_BYTES: &[u8] = &[
    0x48, 0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0x48, 0xba, 0x15, 0x7c, 0x4a, 0x7f,
    0xb9, 0x79, 0x37, 0x9e, 0x48, 0x01, 0xd0, 0x48, 0x89, 0xc3, 0x48, 0xc1, 0xeb, 0x0d, 0x48, 0x31,
    0xd8, 0x48, 0xd1, 0xe0, 0x48, 0xff, 0xc9, 0x75, 0xeb, 0xc3,
];

pub const ALU64: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::Alu64,
    bitness: 64,
    bytes: ALU64_BYTES,
    expected_checksum_10k: 0xf935_f948_2b8f_99b8,
    uses_scratch: false,
};

// 2) alu32
pub const ALU32_BYTES: &[u8] = &[
    0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xba, 0x15, 0x7c, 0x4a, 0x7f, 0x01, 0xd0, 0x89, 0xc3, 0xc1, 0xeb,
    0x0d, 0x31, 0xd8, 0xd1, 0xe0, 0x49, 0x75, 0xf2, 0xc3,
];

pub const ALU32: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::Alu32,
    bitness: 32,
    bytes: ALU32_BYTES,
    expected_checksum_10k: 0x30aa_e0b8,
    uses_scratch: false,
};

// 3) branch_pred64
pub const BRANCH_PRED64_BYTES: &[u8] = &[
    0x48, 0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0x48, 0xbb, 0x15, 0x7c, 0x4a, 0x7f,
    0xb9, 0x79, 0x37, 0x9e, 0x48, 0x31, 0xd2, 0x75, 0x03, 0x48, 0x01, 0xd8, 0x48, 0x31, 0xd2, 0x75,
    0x03, 0x48, 0x31, 0xd8, 0x48, 0xd1, 0xe0, 0x48, 0x83, 0xc0, 0x01, 0x48, 0xff, 0xc9, 0x75, 0xe4,
    0xc3,
];

pub const BRANCH_PRED64: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::BranchPred64,
    bitness: 64,
    bytes: BRANCH_PRED64_BYTES,
    expected_checksum_10k: 0xd7ab_5d5a_aad6_afab,
    uses_scratch: false,
};

// 4) branch_pred32
pub const BRANCH_PRED32_BYTES: &[u8] = &[
    0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0x31, 0xd2, 0x75, 0x02, 0x01, 0xd8,
    0x31, 0xd2, 0x75, 0x02, 0x31, 0xd8, 0xd1, 0xe0, 0x83, 0xc0, 0x01, 0x49, 0x75, 0xec, 0xc3,
];

pub const BRANCH_PRED32: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::BranchPred32,
    bitness: 32,
    bytes: BRANCH_PRED32_BYTES,
    expected_checksum_10k: 0xaad6_afab,
    uses_scratch: false,
};

// 5) branch_unpred64
pub const BRANCH_UNPRED64_BYTES: &[u8] = &[
    0x48, 0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0x78, 0x56, 0x34, 0x12, 0x48, 0xbb, 0x08, 0x09, 0x0a, 0x0b,
    0x0c, 0x0d, 0x0e, 0x0f, 0x48, 0x89, 0xc2, 0x48, 0xc1, 0xe2, 0x0d, 0x48, 0x31, 0xd0, 0x48, 0x89,
    0xc2, 0x48, 0xc1, 0xea, 0x07, 0x48, 0x31, 0xd0, 0x48, 0x89, 0xc2, 0x48, 0xc1, 0xe2, 0x11, 0x48,
    0x31, 0xd0, 0x48, 0x89, 0xc2, 0x48, 0x83, 0xe2, 0x01, 0x74, 0x05, 0x48, 0x01, 0xc3, 0xeb, 0x05,
    0x48, 0x31, 0xc3, 0xeb, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xca, 0x48, 0x89, 0xd8, 0xc3,
];

pub const BRANCH_UNPRED64: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::BranchUnpred64,
    bitness: 64,
    bytes: BRANCH_UNPRED64_BYTES,
    expected_checksum_10k: 0xdf14_f128_b035_d3f0,
    uses_scratch: false,
};

// 6) branch_unpred32
pub const BRANCH_UNPRED32_BYTES: &[u8] = &[
    0xb8, 0xf0, 0xde, 0xbc, 0x9a, 0xbb, 0x08, 0x09, 0x0a, 0x0b, 0x89, 0xc2, 0xc1, 0xe2, 0x0d, 0x31,
    0xd0, 0x89, 0xc2, 0xc1, 0xea, 0x07, 0x31, 0xd0, 0x89, 0xc2, 0xc1, 0xe2, 0x11, 0x31, 0xd0, 0x89,
    0xc2, 0x83, 0xe2, 0x01, 0x74, 0x04, 0x01, 0xc3, 0xeb, 0x04, 0x31, 0xc3, 0xeb, 0x00, 0x49, 0x75,
    0xd9, 0x89, 0xd8, 0xc3,
];

pub const BRANCH_UNPRED32: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::BranchUnpred32,
    bitness: 32,
    bytes: BRANCH_UNPRED32_BYTES,
    expected_checksum_10k: 0xb1fd_f341,
    uses_scratch: false,
};

// 7) mem_seq64
pub const MEM_SEQ64_BYTES: &[u8] = &[
    0x48, 0xb8, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, 0x48, 0x31, 0xf6, 0x48, 0x8b, 0x14,
    0x37, 0x48, 0x01, 0xd0, 0x48, 0x31, 0xc2, 0x48, 0x89, 0x14, 0x37, 0x48, 0x83, 0xc6, 0x08, 0x48,
    0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xe2, 0xc3,
];

pub const MEM_SEQ64: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::MemSeq64,
    bitness: 64,
    bytes: MEM_SEQ64_BYTES,
    expected_checksum_10k: 0xb744_7613_0686_5560,
    uses_scratch: true,
};

// 8) mem_seq32
pub const MEM_SEQ32_BYTES: &[u8] = &[
    0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2, 0x89, 0x14,
    0x37, 0x83, 0xc6, 0x04, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75, 0xea, 0xc3,
];

pub const MEM_SEQ32: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::MemSeq32,
    bitness: 32,
    bytes: MEM_SEQ32_BYTES,
    expected_checksum_10k: 0x0cc5_0aff,
    uses_scratch: true,
};

// 9) mem_stride64
pub const MEM_STRIDE64_BYTES: &[u8] = &[
    0x48, 0xb8, 0xef, 0xcd, 0xab, 0x89, 0x67, 0x45, 0x23, 0x01, 0x48, 0x31, 0xf6, 0x48, 0x8b, 0x14,
    0x37, 0x48, 0x01, 0xd0, 0x48, 0x31, 0xc2, 0x48, 0x89, 0x14, 0x37, 0x48, 0x83, 0xc6, 0x40, 0x48,
    0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xe2, 0xc3,
];

pub const MEM_STRIDE64: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::MemStride64,
    bitness: 64,
    bytes: MEM_STRIDE64_BYTES,
    expected_checksum_10k: 0xd8d5_ee9d_0da7_ebb4,
    uses_scratch: true,
};

// 10) mem_stride32
pub const MEM_STRIDE32_BYTES: &[u8] = &[
    0xb8, 0xef, 0xcd, 0xab, 0x89, 0x31, 0xf6, 0x8b, 0x14, 0x37, 0x01, 0xd0, 0x31, 0xc2, 0x89, 0x14,
    0x37, 0x83, 0xc6, 0x40, 0x81, 0xe6, 0xff, 0x0f, 0x00, 0x00, 0x49, 0x75, 0xea, 0xc3,
];

pub const MEM_STRIDE32: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::MemStride32,
    bitness: 32,
    bytes: MEM_STRIDE32_BYTES,
    expected_checksum_10k: 0x0da7_ebb4,
    uses_scratch: true,
};

// 11) call_ret64
pub const CALL_RET64_BYTES: &[u8] = &[
    0x48, 0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xce, 0xfa, 0xed, 0xfe, 0x48, 0xbb, 0x15, 0x7c, 0x4a, 0x7f,
    0xb9, 0x79, 0x37, 0x9e, 0xe8, 0x06, 0x00, 0x00, 0x00, 0x48, 0xff, 0xc9, 0x75, 0xf6, 0xc3, 0x53,
    0x56, 0x48, 0x01, 0xd8, 0x48, 0x35, 0xb5, 0x3b, 0x12, 0x1f, 0x48, 0xc1, 0xe0, 0x03, 0x5e, 0x5b,
    0xc3,
];

pub const CALL_RET64: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::CallRet64,
    bitness: 64,
    bytes: CALL_RET64_BYTES,
    expected_checksum_10k: 0x0209_be07_71df_5500,
    uses_scratch: false,
};

// 12) call_ret32
pub const CALL_RET32_BYTES: &[u8] = &[
    0xb8, 0xef, 0xbe, 0xfe, 0xca, 0xbb, 0x15, 0x7c, 0x4a, 0x7f, 0xe8, 0x04, 0x00, 0x00, 0x00, 0x49,
    0x75, 0xf8, 0xc3, 0x53, 0x56, 0x01, 0xd8, 0x35, 0xb5, 0x3b, 0x12, 0x1f, 0xc1, 0xe0, 0x03, 0x5e,
    0x5b, 0xc3,
];

pub const CALL_RET32: GuestCpuPayload = GuestCpuPayload {
    variant: GuestCpuBenchVariant::CallRet32,
    bitness: 32,
    bytes: CALL_RET32_BYTES,
    expected_checksum_10k: 0x71df_5500,
    uses_scratch: false,
};

pub const PAYLOADS: &[GuestCpuPayload] = &[
    ALU64,
    ALU32,
    BRANCH_PRED64,
    BRANCH_PRED32,
    BRANCH_UNPRED64,
    BRANCH_UNPRED32,
    MEM_SEQ64,
    MEM_SEQ32,
    MEM_STRIDE64,
    MEM_STRIDE32,
    CALL_RET64,
    CALL_RET32,
];

pub fn payload_by_variant(variant: GuestCpuBenchVariant) -> &'static GuestCpuPayload {
    match variant {
        GuestCpuBenchVariant::Alu64 => &ALU64,
        GuestCpuBenchVariant::Alu32 => &ALU32,
        GuestCpuBenchVariant::BranchPred64 => &BRANCH_PRED64,
        GuestCpuBenchVariant::BranchPred32 => &BRANCH_PRED32,
        GuestCpuBenchVariant::BranchUnpred64 => &BRANCH_UNPRED64,
        GuestCpuBenchVariant::BranchUnpred32 => &BRANCH_UNPRED32,
        GuestCpuBenchVariant::MemSeq64 => &MEM_SEQ64,
        GuestCpuBenchVariant::MemSeq32 => &MEM_SEQ32,
        GuestCpuBenchVariant::MemStride64 => &MEM_STRIDE64,
        GuestCpuBenchVariant::MemStride32 => &MEM_STRIDE32,
        GuestCpuBenchVariant::CallRet64 => &CALL_RET64,
        GuestCpuBenchVariant::CallRet32 => &CALL_RET32,
    }
}

pub fn payload_by_name(name: &str) -> Option<&'static GuestCpuPayload> {
    GuestCpuBenchVariant::parse(name).map(payload_by_variant)
}

// ---------------------------------------------------------------------------
// Deterministic Tier-0 execution harness
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuestCpuRunResult {
    pub checksum: u64,
    pub retired_instructions: u64,
}

#[derive(Debug, Clone)]
pub enum GuestCpuBenchError {
    UnknownVariant(String),
    UnexpectedExit { exit: BatchExit, rip: u64 },
}

impl core::fmt::Display for GuestCpuBenchError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownVariant(v) => write!(f, "Unknown guest CPU bench variant: {v}"),
            Self::UnexpectedExit { exit, rip } => {
                write!(f, "Unexpected Tier-0 batch exit {exit:?} at rip=0x{rip:x}")
            }
        }
    }
}

impl std::error::Error for GuestCpuBenchError {}

pub struct GuestCpuBenchCoreRunner {
    bus: FlatTestBus,
    scratch_zero: Vec<u8>,
}

impl GuestCpuBenchCoreRunner {
    /// Create a new runner with a reusable flat bus.
    ///
    /// The bus must be large enough to cover the PF-008 flat layout constants
    /// (code/scratch/stack).
    pub fn new() -> Self {
        // 64KiB is enough to cover CODE_BASE (0x1000), SCRATCH_BASE (0x2000) and
        // the stack at STACK_TOP (0xA000).
        let bus = FlatTestBus::new(0x1_0000);
        let scratch_zero = vec![0u8; SCRATCH_LEN];
        Self { bus, scratch_zero }
    }

    pub fn bus(&self) -> &FlatTestBus {
        &self.bus
    }

    pub fn bus_mut(&mut self) -> &mut FlatTestBus {
        &mut self.bus
    }

    pub fn run_payload_once(
        &mut self,
        payload: &GuestCpuPayload,
        iters: u32,
    ) -> Result<GuestCpuRunResult, GuestCpuBenchError> {
        // Reset guest-visible memory state.
        self.bus.load(CODE_BASE, payload.bytes);
        if payload.uses_scratch {
            self.bus.load(SCRATCH_BASE, &self.scratch_zero);
        }

        // Reset CPU state per invocation.
        let (mut cpu, ret_sentinel, sp_init) = match payload.bitness {
            32 => {
                let mut cpu = CpuState::new(CpuMode::Protected);
                let sp = STACK_TOP - 4;
                // Stack sentinel (return address).
                // Ignore errors here: FlatTestBus is sized to cover this region.
                let _ = self.bus.write_u32(sp, RET_SENTINEL32);
                cpu.write_gpr32(gpr::RSP, sp as u32);
                cpu.write_gpr32(gpr::RCX, iters);
                if payload.uses_scratch {
                    cpu.write_gpr32(gpr::RDI, SCRATCH_BASE as u32);
                }
                cpu.set_rip(CODE_BASE);
                (cpu, RET_SENTINEL32 as u64, sp)
            }
            64 => {
                let mut cpu = CpuState::new(CpuMode::Long);
                let sp = STACK_TOP - 8;
                let _ = self.bus.write_u64(sp, RET_SENTINEL64);
                cpu.write_gpr64(gpr::RSP, sp);
                cpu.write_gpr64(gpr::RCX, iters as u64);
                if payload.uses_scratch {
                    cpu.write_gpr64(gpr::RDI, SCRATCH_BASE);
                }
                cpu.set_rip(CODE_BASE);
                (cpu, RET_SENTINEL64, sp)
            }
            other => panic!("invalid payload bitness: {other}"),
        };

        // Run Tier-0 batches until the payload returns to the sentinel address.
        let mut retired = 0u64;
        loop {
            let res = run_batch(&mut cpu, &mut self.bus, 1_000_000);
            retired = retired.wrapping_add(res.executed);
            match res.exit {
                BatchExit::Completed => {
                    // No branch in this batch; keep running.
                    continue;
                }
                BatchExit::Branch => {
                    // Stop after the final RET: it will pop our sentinel return
                    // address and set RIP to it.
                    if cpu.rip() == ret_sentinel {
                        break;
                    }
                    continue;
                }
                other => {
                    return Err(GuestCpuBenchError::UnexpectedExit {
                        exit: other,
                        rip: cpu.rip(),
                    });
                }
            }
        }

        // Sanity check: ensure the sentinel slot address we computed is consistent
        // with the CPU's stack pointer bookkeeping (RET should have popped it).
        //
        // We don't fail the run on mismatch (to keep this harness minimal), but
        // keeping this calculation here helps future debugging.
        let _ = sp_init;

        let checksum = match payload.bitness {
            32 => u64::from(cpu.read_gpr32(gpr::RAX)),
            64 => cpu.read_gpr64(gpr::RAX),
            _ => 0,
        };

        Ok(GuestCpuRunResult {
            checksum,
            retired_instructions: retired,
        })
    }
}

// ---------------------------------------------------------------------------
// wasm-bindgen harness (JS ABI)
// ---------------------------------------------------------------------------

#[cfg(target_arch = "wasm32")]
use js_sys::{Object, Reflect};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;

#[cfg(target_arch = "wasm32")]
fn js_error(message: &str) -> JsValue {
    js_sys::Error::new(message).into()
}

#[cfg(target_arch = "wasm32")]
fn u64_hi(v: u64) -> u32 {
    (v >> 32) as u32
}

#[cfg(target_arch = "wasm32")]
fn u64_lo(v: u64) -> u32 {
    v as u32
}

/// JS-callable deterministic Tier-0 guest payload runner (PF-008).
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub struct GuestCpuBenchHarness {
    runner: GuestCpuBenchCoreRunner,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
impl GuestCpuBenchHarness {
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            runner: GuestCpuBenchCoreRunner::new(),
        }
    }

    pub fn payload_info(&self, variant: String) -> Result<JsValue, JsValue> {
        let payload = payload_by_name(&variant)
            .ok_or_else(|| js_error(&format!("Unknown guest CPU bench variant: {variant}")))?;

        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("bitness"),
            &JsValue::from(payload.bitness),
        )?;
        Reflect::set(
            &obj,
            &JsValue::from_str("expected_hi"),
            &JsValue::from(u64_hi(payload.expected_checksum_10k)),
        )?;
        Reflect::set(
            &obj,
            &JsValue::from_str("expected_lo"),
            &JsValue::from(u64_lo(payload.expected_checksum_10k)),
        )?;
        Reflect::set(
            &obj,
            &JsValue::from_str("uses_scratch"),
            &JsValue::from(payload.uses_scratch),
        )?;

        Ok(obj.into())
    }

    pub fn run_payload_once(&mut self, variant: String, iters: u32) -> Result<JsValue, JsValue> {
        let payload = payload_by_name(&variant)
            .ok_or_else(|| js_error(&format!("Unknown guest CPU bench variant: {variant}")))?;

        let res = self
            .runner
            .run_payload_once(payload, iters)
            .map_err(|e| js_error(&e.to_string()))?;

        let obj = Object::new();
        Reflect::set(
            &obj,
            &JsValue::from_str("checksum_hi"),
            &JsValue::from(u64_hi(res.checksum)),
        )?;
        Reflect::set(
            &obj,
            &JsValue::from_str("checksum_lo"),
            &JsValue::from(u64_lo(res.checksum)),
        )?;
        Reflect::set(
            &obj,
            &JsValue::from_str("retired_hi"),
            &JsValue::from(u64_hi(res.retired_instructions)),
        )?;
        Reflect::set(
            &obj,
            &JsValue::from_str("retired_lo"),
            &JsValue::from(u64_lo(res.retired_instructions)),
        )?;

        Ok(obj.into())
    }
}
