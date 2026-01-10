use core::fmt;

/// CPU exception/fault reported back to the instruction dispatcher.
///
/// This models architecturally visible exceptions at the point an instruction
/// would fault (e.g. `#GP(0)` for privilege violations).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Exception {
    /// #GP(error_code)
    GeneralProtection(u16),
    /// #DE
    DivideError,
    /// Memory access fault (e.g. page fault/MMIO error).
    ///
    /// The real project will likely split this into architecturally correct
    /// faults (#PF/#GP) once paging/MMIO are modeled. For now, Tier-0 uses this
    /// as a catch-all for bus failures.
    MemoryFault,
    /// #NP(error_code)
    SegmentNotPresent(u16),
    /// #SS(error_code)
    StackSegment(u16),
    /// #TS(error_code)
    InvalidTss(u16),
    /// #UD
    InvalidOpcode,
    /// #NM (Device Not Available) - raised when floating point is unavailable
    /// due to `CR0.TS` (lazy context switching).
    DeviceNotAvailable,
    /// #MF (x87 floating-point error).
    X87Fpu,
    /// #XM/#XF (SIMD Floating-Point Exception).
    SimdFloatingPointException,
    /// Instruction was decoded but is not yet implemented.
    Unimplemented(&'static str),
}

impl Exception {
    #[inline]
    pub fn gp0() -> Self {
        Self::GeneralProtection(0)
    }

    #[inline]
    pub fn gp(code: u16) -> Self {
        Self::GeneralProtection(code)
    }

    #[inline]
    pub fn np(code: u16) -> Self {
        Self::SegmentNotPresent(code)
    }

    #[inline]
    pub fn ss(code: u16) -> Self {
        Self::StackSegment(code)
    }

    #[inline]
    pub fn ts(code: u16) -> Self {
        Self::InvalidTss(code)
    }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exception::GeneralProtection(code) => write!(f, "#GP({code})"),
            Exception::DivideError => write!(f, "#DE"),
            Exception::MemoryFault => write!(f, "memory fault"),
            Exception::SegmentNotPresent(code) => write!(f, "#NP({code})"),
            Exception::StackSegment(code) => write!(f, "#SS({code})"),
            Exception::InvalidTss(code) => write!(f, "#TS({code})"),
            Exception::InvalidOpcode => write!(f, "#UD"),
            Exception::DeviceNotAvailable => write!(f, "#NM"),
            Exception::X87Fpu => write!(f, "#MF"),
            Exception::SimdFloatingPointException => write!(f, "#XM"),
            Exception::Unimplemented(name) => write!(f, "unimplemented: {name}"),
        }
    }
}

impl std::error::Error for Exception {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssistReason {
    Io,
    Privileged,
    Interrupt,
    Cpuid,
    Msr,
    Unsupported,
}
