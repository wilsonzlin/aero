//! CPUID feature reporting.
//!
//! Windows (and many bootloaders) are sensitive to CPUID feature bits. This
//! module provides a deterministic, configurable implementation so callers and
//! tests can assert *exact* register values.
//!
//! The primary rule: **never advertise a feature bit unless the emulator
//! implements the corresponding behavior** (or it is a safe no-op). Otherwise
//! the guest will execute instructions we can't handle and crash in hard-to-
//! diagnose ways.

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
    /// Extended leaf 0x8000_0007 EDX feature bits (invariant TSC/power management).
    pub ext7_edx: u32,

    /// Physical address width (CPUID.8000_0008H:EAX[7:0]).
    pub physical_address_bits: u8,
    /// Linear address width (CPUID.8000_0008H:EAX[15:8]).
    pub linear_address_bits: u8,

    /// Topology information used by CPUID topology leaves.
    pub topology: CpuTopology,
}

impl CpuFeatures {
    pub fn from_profile(
        profile: CpuProfile,
        implemented: CpuFeatureSet,
        overrides: CpuFeatureOverrides,
        topology: CpuTopology,
    ) -> Result<Self, CpuFeatureError> {
        let required = CpuFeatureSet::win7_minimum();
        if !implemented.contains_all(required) {
            return Err(CpuFeatureError::MissingRequiredFeatures {
                missing: required.without(implemented),
            });
        }

        let allowed = match profile {
            CpuProfile::Win7Minimum => CpuFeatureSet::win7_minimum(),
            CpuProfile::Optimized => CpuFeatureSet::optimized_mask(),
        };

        let mut advertised = implemented.intersect(allowed).sanitize();
        advertised = overrides.apply(implemented, advertised).sanitize();

        let mut vendor_id = [0u8; 12];
        vendor_id.copy_from_slice(b"GenuineIntel");

        let brand_string = brand_string_48("Aero Virtual CPU (Win7)");

        // Intel SDM encoding: family/model/stepping in EAX.
        // Use a common value (family 6, model 0x3A, stepping 9) as a placeholder.
        let leaf1_eax = 0x0003_06A9;

        // Leaf 1 EBX: BrandIndex=0, CLFLUSH line size (64 bytes / 8 = 8),
        // logical processor count, initial APIC ID.
        let logical_count = topology.logical_per_package().min(u32::from(u8::MAX)) as u8;
        let clflush_line_size = 8u8;
        let leaf1_ebx = (u32::from(topology.apic_id) << 24)
            | (u32::from(logical_count) << 16)
            | (u32::from(clflush_line_size) << 8);

        let ext7_edx = bits::EXT7_EDX_INVARIANT_TSC;

        Ok(Self {
            // We implement up to leaf 0x1F and return 0 for unhandled leaves in-between.
            max_basic_leaf: 0x1F,
            max_extended_leaf: 0x8000_0008,
            vendor_id,
            brand_string,
            leaf1_eax,
            leaf1_ebx,
            leaf1_ecx: advertised.leaf1_ecx,
            leaf1_edx: advertised.leaf1_edx,
            leaf7_ebx: advertised.leaf7_ebx,
            leaf7_ecx: advertised.leaf7_ecx,
            leaf7_edx: advertised.leaf7_edx,
            ext1_ecx: advertised.ext1_ecx,
            ext1_edx: advertised.ext1_edx,
            ext7_edx,
            // Windows guests generally assume 48-bit canonical virtual addresses.
            physical_address_bits: 48,
            linear_address_bits: 48,
            topology,
        })
    }
}

impl Default for CpuFeatures {
    fn default() -> Self {
        CpuFeatures::from_profile(
            CpuProfile::Win7Minimum,
            CpuFeatureSet::win7_minimum(),
            CpuFeatureOverrides::default(),
            CpuTopology::default(),
        )
        .expect("default Win7 feature profile must be valid")
    }
}

/// CPU feature policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuProfile {
    /// Minimum viable x86-64 profile for Windows 7 (focus: correctness).
    Win7Minimum,
    /// Enable additional bits when implemented (focus: performance).
    Optimized,
}

/// A compact representation of CPUID feature bits, grouped by the CPUID leaf/register they
/// appear in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuFeatureSet {
    pub leaf1_ecx: u32,
    pub leaf1_edx: u32,
    pub leaf7_ebx: u32,
    pub leaf7_ecx: u32,
    pub leaf7_edx: u32,
    pub ext1_ecx: u32,
    pub ext1_edx: u32,
}

impl CpuFeatureSet {
    pub fn intersect(self, other: Self) -> Self {
        Self {
            leaf1_ecx: self.leaf1_ecx & other.leaf1_ecx,
            leaf1_edx: self.leaf1_edx & other.leaf1_edx,
            leaf7_ebx: self.leaf7_ebx & other.leaf7_ebx,
            leaf7_ecx: self.leaf7_ecx & other.leaf7_ecx,
            leaf7_edx: self.leaf7_edx & other.leaf7_edx,
            ext1_ecx: self.ext1_ecx & other.ext1_ecx,
            ext1_edx: self.ext1_edx & other.ext1_edx,
        }
    }

    pub fn union(self, other: Self) -> Self {
        Self {
            leaf1_ecx: self.leaf1_ecx | other.leaf1_ecx,
            leaf1_edx: self.leaf1_edx | other.leaf1_edx,
            leaf7_ebx: self.leaf7_ebx | other.leaf7_ebx,
            leaf7_ecx: self.leaf7_ecx | other.leaf7_ecx,
            leaf7_edx: self.leaf7_edx | other.leaf7_edx,
            ext1_ecx: self.ext1_ecx | other.ext1_ecx,
            ext1_edx: self.ext1_edx | other.ext1_edx,
        }
    }

    pub fn without(self, other: Self) -> Self {
        Self {
            leaf1_ecx: self.leaf1_ecx & !other.leaf1_ecx,
            leaf1_edx: self.leaf1_edx & !other.leaf1_edx,
            leaf7_ebx: self.leaf7_ebx & !other.leaf7_ebx,
            leaf7_ecx: self.leaf7_ecx & !other.leaf7_ecx,
            leaf7_edx: self.leaf7_edx & !other.leaf7_edx,
            ext1_ecx: self.ext1_ecx & !other.ext1_ecx,
            ext1_edx: self.ext1_edx & !other.ext1_edx,
        }
    }

    pub fn contains_all(self, other: Self) -> bool {
        (self.leaf1_ecx & other.leaf1_ecx) == other.leaf1_ecx
            && (self.leaf1_edx & other.leaf1_edx) == other.leaf1_edx
            && (self.leaf7_ebx & other.leaf7_ebx) == other.leaf7_ebx
            && (self.leaf7_ecx & other.leaf7_ecx) == other.leaf7_ecx
            && (self.leaf7_edx & other.leaf7_edx) == other.leaf7_edx
            && (self.ext1_ecx & other.ext1_ecx) == other.ext1_ecx
            && (self.ext1_edx & other.ext1_edx) == other.ext1_edx
    }

    pub fn sanitize(self) -> Self {
        let mut out = self;
        // SSE2 implies SSE.
        if (out.leaf1_edx & bits::LEAF1_EDX_SSE2) != 0 {
            out.leaf1_edx |= bits::LEAF1_EDX_SSE;
        }
        out
    }

    /// Minimum viable x86-64 CPUID bit set for Windows 7.
    pub fn win7_minimum() -> Self {
        Self {
            leaf1_ecx: bits::LEAF1_ECX_CX16,
            leaf1_edx: bits::LEAF1_EDX_FPU
                | bits::LEAF1_EDX_TSC
                | bits::LEAF1_EDX_MSR
                | bits::LEAF1_EDX_PAE
                | bits::LEAF1_EDX_CX8
                | bits::LEAF1_EDX_APIC
                | bits::LEAF1_EDX_SEP
                | bits::LEAF1_EDX_CMOV
                | bits::LEAF1_EDX_MMX
                | bits::LEAF1_EDX_FXSR
                | bits::LEAF1_EDX_SSE
                | bits::LEAF1_EDX_SSE2,
            leaf7_ebx: 0,
            leaf7_ecx: 0,
            leaf7_edx: 0,
            ext1_ecx: bits::EXT1_ECX_LAHF_LM,
            ext1_edx: bits::EXT1_EDX_SYSCALL
                | bits::EXT1_EDX_NX
                | bits::EXT1_EDX_RDTSCP
                | bits::EXT1_EDX_LM,
        }
    }

    /// Mask of extra bits that may be exposed in the optimized profile (when implemented).
    pub fn optimized_mask() -> Self {
        Self::win7_minimum().union(Self {
            leaf1_ecx: bits::LEAF1_ECX_SSE3
                | bits::LEAF1_ECX_PCLMULQDQ
                | bits::LEAF1_ECX_SSSE3
                | bits::LEAF1_ECX_SSE41
                | bits::LEAF1_ECX_SSE42
                | bits::LEAF1_ECX_POPCNT,
            ..Self::default()
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CpuFeatureError {
    MissingRequiredFeatures { missing: CpuFeatureSet },
}

/// Debugging overrides applied after the profile/implemented-feature intersection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CpuFeatureOverrides {
    pub force_enable: CpuFeatureSet,
    pub force_disable: CpuFeatureSet,
    pub allow_unsafe: bool,
}

impl CpuFeatureOverrides {
    fn apply(self, implemented: CpuFeatureSet, advertised: CpuFeatureSet) -> CpuFeatureSet {
        let enable = if self.allow_unsafe {
            self.force_enable
        } else {
            self.force_enable.intersect(implemented)
        };
        advertised.union(enable).without(self.force_disable)
    }
}

/// Basic topology information needed for CPUID topology leaves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuTopology {
    pub cores_per_package: u32,
    pub threads_per_core: u32,
    pub apic_id: u8,
    pub x2apic_id: u32,
}

impl CpuTopology {
    pub fn logical_per_package(self) -> u32 {
        self.cores_per_package
            .saturating_mul(self.threads_per_core)
            .max(1)
    }
}

impl Default for CpuTopology {
    fn default() -> Self {
        Self {
            cores_per_package: 1,
            threads_per_core: 1,
            apic_id: 0,
            x2apic_id: 0,
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
    (ebx, edx, ecx)
}

/// Compute CPUID(leaf, subleaf) for the given feature configuration.
pub fn cpuid(features: &CpuFeatures, leaf: u32, subleaf: u32) -> CpuidResult {
    match leaf {
        0x0000_0000 => {
            let (ebx, edx, ecx) = vendor_regs(features.vendor_id);
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
        0x0000_0002 => CpuidResult {
            // QEMU-like cache/TLB descriptor bytes. Modern guests prefer leaf 4.
            eax: 0x7603_6301,
            ebx: 0x00F0_B5FF,
            ecx: 0,
            edx: 0x00C3_0000,
        },
        0x0000_0004 => cpuid_leaf4(features, subleaf),
        0x0000_0006 => CpuidResult::ZERO,
        0x0000_0007 if subleaf == 0 => CpuidResult {
            eax: 0,
            ebx: features.leaf7_ebx,
            ecx: features.leaf7_ecx,
            edx: features.leaf7_edx,
        },
        0x0000_000A => CpuidResult::ZERO,
        0x0000_000B => cpuid_topology(features, subleaf),
        0x0000_001F => cpuid_topology(features, subleaf),
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
        0x8000_0007 => CpuidResult {
            eax: 0,
            ebx: 0,
            ecx: 0,
            edx: features.ext7_edx,
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
        0x8000_0006 => {
            // Extended cache info (mostly AMD-defined; still commonly queried).
            // ECX[7:0] line size (bytes), ECX[15:12] assoc, ECX[31:16] size (KB).
            let l2_line_size = 64u32;
            let l2_assoc = 8u32;
            let l2_size_kb = 256u32;
            CpuidResult {
                eax: 0,
                ebx: 0,
                ecx: (l2_line_size & 0xFF)
                    | ((l2_assoc & 0xF) << 12)
                    | ((l2_size_kb & 0xFFFF) << 16),
                edx: 0,
            }
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

fn brand_string_48(s: &str) -> [u8; 48] {
    let mut out = [b' '; 48];
    let bytes = s.as_bytes();
    let n = bytes.len().min(out.len());
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

fn ilog2_ceil(v: u32) -> u32 {
    if v <= 1 {
        return 0;
    }
    32 - (v - 1).leading_zeros()
}

fn cpuid_topology(features: &CpuFeatures, subleaf: u32) -> CpuidResult {
    // Leaf B/1F are only meaningful when CPUID.1:ECX[x2APIC] is set.
    if (features.leaf1_ecx & bits::LEAF1_ECX_X2APIC) == 0 {
        return CpuidResult::ZERO;
    }

    let threads = features.topology.threads_per_core.max(1);
    let logical = features.topology.logical_per_package().max(1);
    match subleaf {
        0 => CpuidResult {
            eax: ilog2_ceil(threads),
            ebx: threads,
            ecx: 0 | (1 << 8), // level 0 SMT
            edx: features.topology.x2apic_id,
        },
        1 => CpuidResult {
            eax: ilog2_ceil(logical),
            ebx: logical,
            ecx: 1 | (2 << 8), // level 1 Core
            edx: features.topology.x2apic_id,
        },
        _ => CpuidResult::ZERO,
    }
}

fn cpuid_leaf4(features: &CpuFeatures, subleaf: u32) -> CpuidResult {
    let caches = [
        CacheDesc::l1_data(),
        CacheDesc::l1_instruction(),
        CacheDesc::l2_unified(),
        CacheDesc::l3_unified(),
    ];
    let Some(cache) = caches.get(subleaf as usize) else {
        return CpuidResult::ZERO;
    };
    cache.as_cpuid(features.topology)
}

#[derive(Clone, Copy, Debug)]
struct CacheDesc {
    cache_type: u32,
    level: u32,
    line_size: u32,
    partitions: u32,
    ways: u32,
    sets: u32,
    is_inclusive: bool,
}

impl CacheDesc {
    fn l1_data() -> Self {
        Self {
            cache_type: 1,
            level: 1,
            line_size: 64,
            partitions: 1,
            ways: 8,
            sets: 64,
            is_inclusive: false,
        }
    }

    fn l1_instruction() -> Self {
        Self {
            cache_type: 2,
            level: 1,
            line_size: 64,
            partitions: 1,
            ways: 8,
            sets: 64,
            is_inclusive: false,
        }
    }

    fn l2_unified() -> Self {
        Self {
            cache_type: 3,
            level: 2,
            line_size: 64,
            partitions: 1,
            ways: 8,
            sets: 512,
            is_inclusive: false,
        }
    }

    fn l3_unified() -> Self {
        Self {
            cache_type: 3,
            level: 3,
            line_size: 64,
            partitions: 1,
            ways: 16,
            sets: 8192,
            is_inclusive: true,
        }
    }

    fn as_cpuid(self, topo: CpuTopology) -> CpuidResult {
        let threads = topo.threads_per_core.max(1);
        let logical = topo.logical_per_package().max(1);

        let shared_by = match self.level {
            1 | 2 => threads,
            _ => logical,
        };

        // See Intel SDM Vol 2A CPUID leaf 4 format.
        let eax = (self.cache_type & 0x1F)
            | ((self.level & 0x7) << 5)
            | (1 << 8) // self-initializing
            | ((shared_by.saturating_sub(1) & 0xFFF) << 14)
            | ((topo.cores_per_package.saturating_sub(1) & 0x3F) << 26);

        let ebx = ((self.line_size - 1) & 0xFFF)
            | (((self.partitions - 1) & 0x3FF) << 12)
            | (((self.ways - 1) & 0x3FF) << 22);

        let ecx = self.sets - 1;
        let edx = if self.is_inclusive { 1 << 1 } else { 0 };
        CpuidResult { eax, ebx, ecx, edx }
    }
}

/// CPUID feature bit constants used by the profiles and MSR coherence.
pub mod bits {
    // CPUID.1:EDX
    pub const LEAF1_EDX_FPU: u32 = 1 << 0;
    pub const LEAF1_EDX_PSE: u32 = 1 << 3;
    pub const LEAF1_EDX_TSC: u32 = 1 << 4;
    pub const LEAF1_EDX_MSR: u32 = 1 << 5;
    pub const LEAF1_EDX_PAE: u32 = 1 << 6;
    pub const LEAF1_EDX_CX8: u32 = 1 << 8;
    pub const LEAF1_EDX_APIC: u32 = 1 << 9;
    pub const LEAF1_EDX_SEP: u32 = 1 << 11;
    pub const LEAF1_EDX_MTRR: u32 = 1 << 12;
    pub const LEAF1_EDX_PGE: u32 = 1 << 13;
    pub const LEAF1_EDX_CMOV: u32 = 1 << 15;
    pub const LEAF1_EDX_PAT: u32 = 1 << 16;
    pub const LEAF1_EDX_CLFSH: u32 = 1 << 19;
    pub const LEAF1_EDX_MMX: u32 = 1 << 23;
    pub const LEAF1_EDX_FXSR: u32 = 1 << 24;
    pub const LEAF1_EDX_SSE: u32 = 1 << 25;
    pub const LEAF1_EDX_SSE2: u32 = 1 << 26;

    // CPUID.1:ECX
    pub const LEAF1_ECX_SSE3: u32 = 1 << 0;
    pub const LEAF1_ECX_PCLMULQDQ: u32 = 1 << 1;
    pub const LEAF1_ECX_SSSE3: u32 = 1 << 9;
    pub const LEAF1_ECX_CX16: u32 = 1 << 13;
    pub const LEAF1_ECX_SSE41: u32 = 1 << 19;
    pub const LEAF1_ECX_SSE42: u32 = 1 << 20;
    pub const LEAF1_ECX_X2APIC: u32 = 1 << 21;
    pub const LEAF1_ECX_POPCNT: u32 = 1 << 23;

    // CPUID.80000001:EDX
    pub const EXT1_EDX_SYSCALL: u32 = 1 << 11;
    pub const EXT1_EDX_NX: u32 = 1 << 20;
    pub const EXT1_EDX_RDTSCP: u32 = 1 << 27;
    pub const EXT1_EDX_LM: u32 = 1 << 29;

    // CPUID.80000001:ECX
    pub const EXT1_ECX_LAHF_LM: u32 = 1 << 0;

    // CPUID.80000007:EDX
    pub const EXT7_EDX_INVARIANT_TSC: u32 = 1 << 8;
}
