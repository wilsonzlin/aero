use crate::exception::Exception as Tier0Exception;
use crate::exceptions::Exception as ArchException;
use crate::interrupts::CpuExit;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExceptionFault {
    pub exception: ArchException,
    pub error_code: Option<u32>,
    pub cr2: Option<u64>,
}

/// Map a Tier-0/interpreter exception into an architectural exception vector.
///
/// Some interpreter failures are non-architectural (e.g. bus failures for
/// unmapped physical memory, or unimplemented instruction stubs). Those are
/// returned as a [`CpuExit`] instead of being delivered through the guest IDT.
pub fn map_tier0_exception(e: &Tier0Exception) -> Result<ExceptionFault, CpuExit> {
    use Tier0Exception as T;
    Ok(match *e {
        T::GeneralProtection(code) => ExceptionFault {
            exception: ArchException::GeneralProtection,
            error_code: Some(code as u32),
            cr2: None,
        },
        T::DivideError => ExceptionFault {
            exception: ArchException::DivideError,
            error_code: None,
            cr2: None,
        },
        T::PageFault { addr, error_code } => ExceptionFault {
            exception: ArchException::PageFault,
            error_code: Some(error_code),
            cr2: Some(addr),
        },
        T::SegmentNotPresent(code) => ExceptionFault {
            exception: ArchException::SegmentNotPresent,
            error_code: Some(code as u32),
            cr2: None,
        },
        T::StackSegment(code) => ExceptionFault {
            exception: ArchException::StackFault,
            error_code: Some(code as u32),
            cr2: None,
        },
        T::InvalidTss(code) => ExceptionFault {
            exception: ArchException::InvalidTss,
            error_code: Some(code as u32),
            cr2: None,
        },
        T::InvalidOpcode => ExceptionFault {
            exception: ArchException::InvalidOpcode,
            error_code: None,
            cr2: None,
        },
        T::DeviceNotAvailable => ExceptionFault {
            exception: ArchException::DeviceNotAvailable,
            error_code: None,
            cr2: None,
        },
        T::X87Fpu => ExceptionFault {
            exception: ArchException::X87Fpu,
            error_code: None,
            cr2: None,
        },
        T::SimdFloatingPointException => ExceptionFault {
            exception: ArchException::SimdFloatingPoint,
            error_code: None,
            cr2: None,
        },
        T::MemoryFault => return Err(CpuExit::MemoryFault),
        T::Unimplemented(name) => return Err(CpuExit::UnimplementedInstruction(name)),
    })
}
