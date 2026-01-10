//! Architectural exceptions and deferred event model.
//!
//! The crate already exposes `crate::Exception` (used by the interpreter/system
//! instruction surface). This module provides *architectural* exception vectors
//! used for IDT delivery and stack frame construction.

use crate::system::Cpu;

/// Architecturally defined x86 exception vectors.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Exception {
    DivideError = 0,             // #DE
    Debug = 1,                   // #DB
    NonMaskableInterrupt = 2,    // NMI
    Breakpoint = 3,              // #BP
    Overflow = 4,                // #OF
    BoundRangeExceeded = 5,      // #BR
    InvalidOpcode = 6,           // #UD
    DeviceNotAvailable = 7,      // #NM
    DoubleFault = 8,             // #DF
    InvalidTss = 10,             // #TS
    SegmentNotPresent = 11,      // #NP
    StackFault = 12,             // #SS
    GeneralProtection = 13,      // #GP
    PageFault = 14,              // #PF
    X87Fpu = 16,                 // #MF
    AlignmentCheck = 17,         // #AC
    MachineCheck = 18,           // #MC
    SimdFloatingPoint = 19,      // #XM/#XF
    Virtualization = 20,         // #VE
    ControlProtection = 21,      // #CP
}

impl Exception {
    #[inline]
    pub const fn vector(self) -> u8 {
        self as u8
    }

    /// Whether the CPU pushes an error code for this exception.
    #[inline]
    pub const fn pushes_error_code(self) -> bool {
        matches!(
            self,
            Exception::DoubleFault
                | Exception::InvalidTss
                | Exception::SegmentNotPresent
                | Exception::StackFault
                | Exception::GeneralProtection
                | Exception::PageFault
                | Exception::AlignmentCheck
                | Exception::ControlProtection
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterruptSource {
    Software,
    External,
}

/// A high-level pending event waiting to be delivered at an instruction boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PendingEvent {
    /// A faulting exception (saved RIP is the faulting instruction pointer).
    Fault {
        exception: Exception,
        saved_rip: u64,
        error_code: Option<u32>,
    },
    /// A trap (saved RIP points to the next instruction).
    Trap { vector: u8, saved_rip: u64 },
    /// An interrupt (external or software).
    Interrupt {
        vector: u8,
        saved_rip: u64,
        source: InterruptSource,
    },
}

impl Cpu {
    /// Queue a faulting exception for delivery at the next instruction boundary.
    ///
    /// For page faults this will also update CR2.
    pub fn raise_exception_fault(
        &mut self,
        exception: Exception,
        faulting_rip: u64,
        error_code: Option<u32>,
        cr2: Option<u64>,
    ) {
        if exception == Exception::PageFault {
            if let Some(addr) = cr2 {
                self.cr2 = addr;
            }
        }

        self.pending_event = Some(PendingEvent::Fault {
            exception,
            saved_rip: faulting_rip,
            error_code,
        });
    }

    /// Queue a software interrupt (e.g. `INT n`).
    pub fn raise_software_interrupt(&mut self, vector: u8, return_rip: u64) {
        self.pending_event = Some(PendingEvent::Interrupt {
            vector,
            saved_rip: return_rip,
            source: InterruptSource::Software,
        });
    }
}

