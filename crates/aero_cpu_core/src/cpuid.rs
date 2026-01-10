//! CPUID feature reporting.
//!
//! Windows (and many bootloaders) are sensitive to CPUID feature bits. This
//! module provides a deterministic, configurable implementation so callers and
//! tests can assert *exact* register values.

/// A CPUID result tuple (EAX, EBX, ECX, EDX).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

impl CpuidResult {
    pub const ZERO: Self = Self {
        eax: 0,
        ebx: 0,
        ecx: 0,
        edx: 0,
    };
}

/// Configurable CPUID surface.
///
/// The fields map 1:1 to architecturally visible CPUID leaves so tests can
/// assert exact values without having to re-derive leaf packing logic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuFeatures {
    /// Maximum supported basic leaf (returned in CPUID(0).EAX).
    pub max_basic_leaf: u32,
    /// Maximum supported extended leaf (returned in CPUID(0x8000_0000).EAX).
    pub max_extended_leaf: u32,
    /// Vendor ID string, returned split across EBX/EDX/ECX in leaf 0.
    pub vendor_id: [u8; 12],
    /// Processor brand string, returned in leaves 0x8000_0002..=0x8000_0004.
    pub brand_string: [u8; 48],

    /// Leaf 1 EAX (family/model/stepping).
    pub leaf1_eax: u32,
    /// Leaf 1 EBX (brand index / CLFLUSH line size / etc).
    pub leaf1_ebx: u32,
    /// Leaf 1 ECX feature bits.
    pub leaf1_ecx: u32,
    /// Leaf 1 EDX feature bits.
    pub leaf1_edx: u32,

    /// Leaf 7 subleaf 0 EBX feature bits.
    pub leaf7_ebx: u32,
    /// Leaf 7 subleaf 0 ECX feature bits.
    pub leaf7_ecx: u32,
    /// Leaf 7 subleaf 0 EDX feature bits.
    pub leaf7_edx: u32,

    /// Extended leaf 0x8000_0001 ECX feature bits.
    pub ext1_ecx: u32,
    /// Extended leaf 0x8000_0001 EDX feature bits.
    pub ext1_edx: u32,

    /// Physical address width (CPUID.8000_0008H:EAX[7:0]).
    pub physical_address_bits: u8,
    /// Linear address width (CPUID.8000_0008H:EAX[15:8]).
    pub linear_address_bits: u8,
}

impl Default for CpuFeatures {
    fn default() -> Self {
        // A conservative "Windows 7 friendly" baseline. The exact values are
        // less important than consistency across runs and explicit configurability.
        //
        // Leaf 1 required bits commonly assumed by modern OS kernels:
        // - FPU, TSC, MSR, PAE, CX8, APIC, SEP (SYSENTER), FXSR, SSE, SSE2
        // - CMPXCHG16B (required for x86-64 Windows)
        //
        // Extended leaf bits:
        // - SYSCALL/SYSRET, NX, Long Mode.
        let mut vendor_id = [0u8; 12];
        vendor_id.copy_from_slice(b"GenuineIntel");

        let mut brand_string = [0u8; 48];
        let brand =
            b"Aero Virtual CPU\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";
        brand_string.copy_from_slice(&brand[..48]);

        // Intel SDM encoding: family/model/stepping in EAX.
        // Use a common value (family 6, model 0x3A, stepping 9) as a placeholder.
        let leaf1_eax = 0x0003_06A9;

        // Leaf 1 EBX: we expose a 64-byte CLFLUSH line size (value *8).
        let leaf1_ebx = 0x0000_0800;

        // Leaf 1 EDX bits.
        const FEAT_EDX_FPU: u32 = 1 << 0;
        const FEAT_EDX_TSC: u32 = 1 << 4;
        const FEAT_EDX_MSR: u32 = 1 << 5;
        const FEAT_EDX_PAE: u32 = 1 << 6;
        const FEAT_EDX_CX8: u32 = 1 << 8;
        const FEAT_EDX_APIC: u32 = 1 << 9;
        const FEAT_EDX_SEP: u32 = 1 << 11;
        const FEAT_EDX_CMOV: u32 = 1 << 15;
        const FEAT_EDX_MMX: u32 = 1 << 23;
        const FEAT_EDX_FXSR: u32 = 1 << 24;
        const FEAT_EDX_SSE: u32 = 1 << 25;
        const FEAT_EDX_SSE2: u32 = 1 << 26;

        let leaf1_edx = FEAT_EDX_FPU
            | FEAT_EDX_TSC
            | FEAT_EDX_MSR
            | FEAT_EDX_PAE
            | FEAT_EDX_CX8
            | FEAT_EDX_APIC
            | FEAT_EDX_SEP
            | FEAT_EDX_CMOV
            | FEAT_EDX_MMX
            | FEAT_EDX_FXSR
            | FEAT_EDX_SSE
            | FEAT_EDX_SSE2;

        // Leaf 1 ECX bits.
        const FEAT_ECX_SSE3: u32 = 1 << 0;
        const FEAT_ECX_SSSE3: u32 = 1 << 9;
        const FEAT_ECX_CX16: u32 = 1 << 13; // CMPXCHG16B
        const FEAT_ECX_SSE41: u32 = 1 << 19;
        const FEAT_ECX_SSE42: u32 = 1 << 20;
        const FEAT_ECX_POPCNT: u32 = 1 << 23;

        let leaf1_ecx = FEAT_ECX_SSE3
            | FEAT_ECX_SSSE3
            | FEAT_ECX_CX16
            | FEAT_ECX_SSE41
            | FEAT_ECX_SSE42
            | FEAT_ECX_POPCNT;

        // Extended leaf 0x8000_0001 EDX bits.
        const EXT_EDX_SYSCALL: u32 = 1 << 11;
        const EXT_EDX_NX: u32 = 1 << 20;
        const EXT_EDX_1GB_PAGES: u32 = 1 << 26;
        const EXT_EDX_LM: u32 = 1 << 29;
        let ext1_edx = EXT_EDX_SYSCALL | EXT_EDX_NX | EXT_EDX_1GB_PAGES | EXT_EDX_LM;

        // Extended leaf 0x8000_0001 ECX bits.
        const EXT_ECX_LAHF_LM: u32 = 1 << 0;
        let ext1_ecx = EXT_ECX_LAHF_LM;

        Self {
            max_basic_leaf: 7,
            max_extended_leaf: 0x8000_0008,
            vendor_id,
            brand_string,
            leaf1_eax,
            leaf1_ebx,
            leaf1_ecx,
            leaf1_edx,
            leaf7_ebx: 0,
            leaf7_ecx: 0,
            leaf7_edx: 0,
            ext1_ecx,
            ext1_edx,
            physical_address_bits: 48,
            linear_address_bits: 48,
        }
    }
}

fn pack_u32(bytes: &[u8; 4]) -> u32 {
    u32::from_le_bytes(*bytes)
}

fn vendor_regs(vendor_id: [u8; 12]) -> (u32, u32, u32) {
    // CPUID leaf 0: EBX, EDX, ECX.
    let ebx = pack_u32((&vendor_id[0..4]).try_into().unwrap());
    let edx = pack_u32((&vendor_id[4..8]).try_into().unwrap());
    let ecx = pack_u32((&vendor_id[8..12]).try_into().unwrap());
    (ebx, ecx, edx)
}

/// Compute CPUID(leaf, subleaf) for the given feature configuration.
pub fn cpuid(features: &CpuFeatures, leaf: u32, subleaf: u32) -> CpuidResult {
    match leaf {
        0x0000_0000 => {
            let (ebx, ecx, edx) = vendor_regs(features.vendor_id);
            CpuidResult {
                eax: features.max_basic_leaf,
                ebx,
                ecx,
                edx,
            }
        }
        0x0000_0001 => CpuidResult {
            eax: features.leaf1_eax,
            ebx: features.leaf1_ebx,
            ecx: features.leaf1_ecx,
            edx: features.leaf1_edx,
        },
        0x0000_0007 if subleaf == 0 => CpuidResult {
            eax: 0,
            ebx: features.leaf7_ebx,
            ecx: features.leaf7_ecx,
            edx: features.leaf7_edx,
        },
        0x8000_0000 => CpuidResult {
            eax: features.max_extended_leaf,
            ebx: 0,
            ecx: 0,
            edx: 0,
        },
        0x8000_0001 => CpuidResult {
            eax: 0,
            ebx: 0,
            ecx: features.ext1_ecx,
            edx: features.ext1_edx,
        },
        0x8000_0002..=0x8000_0004 => {
            let chunk = (leaf - 0x8000_0002) as usize;
            let base = chunk * 16;
            let bytes = &features.brand_string[base..base + 16];

            let eax = pack_u32(bytes[0..4].try_into().unwrap());
            let ebx = pack_u32(bytes[4..8].try_into().unwrap());
            let ecx = pack_u32(bytes[8..12].try_into().unwrap());
            let edx = pack_u32(bytes[12..16].try_into().unwrap());
            CpuidResult { eax, ebx, ecx, edx }
        }
        0x8000_0008 => {
            let eax = (features.physical_address_bits as u32)
                | ((features.linear_address_bits as u32) << 8);
            CpuidResult {
                eax,
                ebx: 0,
                ecx: 0,
                edx: 0,
            }
        }
        _ => CpuidResult::ZERO,
    }
}
