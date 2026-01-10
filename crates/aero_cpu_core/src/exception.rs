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
    /// #UD
    InvalidOpcode,
    /// Instruction was decoded but is not yet implemented.
    Unimplemented(&'static str),
}

impl Exception {
    #[inline]
    pub fn gp0() -> Self {
        Self::GeneralProtection(0)
    }
}

impl fmt::Display for Exception {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Exception::GeneralProtection(code) => write!(f, "#GP({code})"),
            Exception::DivideError => write!(f, "#DE"),
            Exception::MemoryFault => write!(f, "memory fault"),
            Exception::InvalidOpcode => write!(f, "#UD"),
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
