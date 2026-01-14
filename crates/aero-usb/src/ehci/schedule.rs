/// Max number of horizontal QH links walked in the asynchronous schedule per 1ms tick.
///
/// This bounds the work `tick_1ms()` can do even if the guest provides an adversarial schedule.
pub const MAX_ASYNC_QH_VISITS: usize = 1024;

/// Max number of qTDs walked per QH per 1ms tick.
pub const MAX_QTD_STEPS_PER_QH: usize = 1024;

/// Max number of periodic frame list links walked per frame.
///
/// The periodic schedule can contain a mixture of QHs (interrupt) and other element types (iTD,
/// siTD, FSTN). We treat unknown types as opaque nodes with a "next link pointer" at offset 0 and
/// enforce a strict hop budget.
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
}
