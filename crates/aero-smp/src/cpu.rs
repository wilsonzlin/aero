//! Per-vCPU architectural state (high level).

/// x86 reset vector for the BSP.
///
/// Architecturally the CPU begins execution at `CS:IP = F000:FFF0`, which maps
/// to physical `0xFFFF_FFF0` in a typical 32-bit reset mapping.
pub const RESET_VECTOR: u64 = 0xFFFF_FFF0;

/// vCPU run state from the perspective of the scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VcpuRunState {
    /// Normal execution.
    Running,
    /// CPU is halted (HLT). It may resume on interrupt.
    Halted,
    /// Application processor (AP) has been reset (INIT) and is waiting for SIPI.
    WaitForSipi,
}

/// Minimal CPU state needed for SMP boot and APIC IPI semantics.
#[derive(Debug, Clone)]
pub struct CpuState {
    pub apic_id: u8,
    pub is_bsp: bool,
    pub rip: u64,
    pub run_state: VcpuRunState,
    /// The SIPI vector that started this CPU (if any).
    pub sipi_vector: Option<u8>,
}

impl CpuState {
    pub fn new_bsp(apic_id: u8) -> Self {
        Self {
            apic_id,
            is_bsp: true,
            rip: RESET_VECTOR,
            run_state: VcpuRunState::Running,
            sipi_vector: None,
        }
    }

    pub fn new_ap(apic_id: u8) -> Self {
        Self {
            apic_id,
            is_bsp: false,
            // APs do not execute at reset; they wait for SIPI.
            rip: 0,
            run_state: VcpuRunState::WaitForSipi,
            sipi_vector: None,
        }
    }

    /// Apply the architectural effects of receiving an INIT IPI.
    pub fn receive_init(&mut self) {
        // A real INIT resets most architectural state. For SMP boot we only
        // need to model the transition into the wait-for-SIPI state.
        self.run_state = VcpuRunState::WaitForSipi;
        self.rip = 0;
        self.sipi_vector = None;
    }

    /// Apply the architectural effects of receiving a SIPI.
    pub fn receive_sipi(&mut self, vector: u8) {
        if self.run_state != VcpuRunState::WaitForSipi {
            return;
        }

        // Startup IPI vector specifies the 4KiB page the AP begins executing at.
        self.sipi_vector = Some(vector);
        self.rip = (vector as u64) << 12;
        self.run_state = VcpuRunState::Running;
    }
}
