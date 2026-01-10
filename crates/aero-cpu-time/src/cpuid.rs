use aero_timers::LocalApicTimer;
use aero_time::Tsc;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CpuidResult {
    pub eax: u32,
    pub ebx: u32,
    pub ecx: u32,
    pub edx: u32,
}

#[derive(Debug, Clone)]
pub struct CpuidModel {
    pub has_tsc: bool,
    pub has_rdtscp: bool,
    pub has_invariant_tsc: bool,
    pub has_tsc_deadline: bool,
}

impl CpuidModel {
    pub fn for_time(tsc: &Tsc, apic_timer: &LocalApicTimer) -> Self {
        Self {
            has_tsc: true,
            has_rdtscp: true,
            has_invariant_tsc: tsc.invariant(),
            has_tsc_deadline: apic_timer.supports_tsc_deadline(),
        }
    }

    pub fn cpuid(&self, leaf: u32, _subleaf: u32) -> CpuidResult {
        match leaf {
            0x0000_0001 => {
                let mut ecx = 0u32;
                let mut edx = 0u32;
                if self.has_tsc {
                    edx |= 1 << 4;
                }
                if self.has_tsc_deadline {
                    ecx |= 1 << 24;
                }
                CpuidResult {
                    eax: 0,
                    ebx: 0,
                    ecx,
                    edx,
                }
            }
            0x8000_0001 => {
                let mut ecx = 0u32;
                if self.has_rdtscp {
                    ecx |= 1 << 27;
                }
                CpuidResult {
                    eax: 0,
                    ebx: 0,
                    ecx,
                    edx: 0,
                }
            }
            0x8000_0007 => {
                let mut edx = 0u32;
                if self.has_invariant_tsc {
                    edx |= 1 << 8;
                }
                CpuidResult {
                    eax: 0,
                    ebx: 0,
                    ecx: 0,
                    edx,
                }
            }
            _ => CpuidResult {
                eax: 0,
                ebx: 0,
                ecx: 0,
                edx: 0,
            },
        }
    }
}
