//! Shared EHCI schedule traversal limits + error types.
//!
//! The EHCI schedule structures (frame list, QHs, qTDs, etc) live in guest memory and are
//! therefore entirely guest-controlled. The schedule engines (`schedule_async` and
//! `schedule_periodic`) enforce strict per-tick budgets and detect cycles to keep controller work
//! bounded even under malformed/adversarial schedules.
/// Max number of horizontal QH links walked in the asynchronous schedule per 1ms tick.
///
/// This bounds the work `tick_1ms()` can do even if the guest provides an adversarial schedule.
pub const MAX_ASYNC_QH_VISITS: usize = 1024;

/// Max number of qTDs walked per QH per 1ms tick.
pub const MAX_QTD_STEPS_PER_QH: usize = 1024;

/// Max number of periodic frame list links walked per frame.
///
/// The periodic schedule can contain a mixture of QHs (interrupt) and other element types (iTD,
/// siTD, FSTN). Unsupported element types are treated as opaque nodes with a "next link pointer" at
/// offset 0 and still count against this hop budget.
pub const MAX_PERIODIC_LINKS_PER_FRAME: usize = 1024;

/// Errors produced by schedule traversal/processing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScheduleError {
    AsyncQhBudgetExceeded,
    AsyncQhCycle,
    QtdBudgetExceeded,
    QtdCycle,
    PeriodicBudgetExceeded,
    PeriodicCycle,
    AddressOverflow,
}

/// Add a byte offset to a 32-bit schedule pointer, returning the resulting physical address.
///
/// EHCI schedule pointers are 32-bit physical addresses (AC64=0). Treat arithmetic overflow as a
/// schedule fault rather than allowing wraparound to alias low memory.
pub(crate) fn addr_add(base: u32, offset: u32) -> Result<u64, ScheduleError> {
    base.checked_add(offset)
        .map(u64::from)
        .ok_or(ScheduleError::AddressOverflow)
}
