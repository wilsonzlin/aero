use std::cell::Cell;
use std::rc::Rc;

/// Reset request kind emitted by chipset devices.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetKind {
    /// Reset the CPU core(s) while leaving device state intact (warm reset).
    Cpu,
    /// Full system reset (CPU + devices + firmware re-entry).
    System,
}

/// Platform-level sink for reset requests coming from chipset devices (e.g. i8042, 0xCF9).
///
/// Device models should *not* reset the system directly from inside an I/O handler; doing so can
/// create re-entrancy/borrowing issues in bus routers. Instead, devices should report a request to
/// a sink and let the platform coordinator apply the reset at a safe boundary.
pub trait PlatformResetSink {
    fn request_reset(&mut self, kind: ResetKind);
}

impl<F> PlatformResetSink for F
where
    F: FnMut(ResetKind),
{
    fn request_reset(&mut self, kind: ResetKind) {
        self(kind);
    }
}

/// A cloneable reset request latch used to bridge device reset requests into a platform loop.
///
/// The latch stores at most one pending request. If multiple devices request a reset before the
/// platform consumes it, [`ResetKind::System`] wins over [`ResetKind::Cpu`].
#[derive(Debug, Clone, Default)]
pub struct ResetLatch {
    pending: Rc<Cell<Option<ResetKind>>>,
}

impl ResetLatch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the currently pending reset request without clearing it.
    pub fn peek(&self) -> Option<ResetKind> {
        self.pending.get()
    }

    /// Takes and clears the pending reset request.
    pub fn take(&self) -> Option<ResetKind> {
        let pending = self.pending.get();
        self.pending.set(None);
        pending
    }

    pub fn clear(&self) {
        self.pending.set(None);
    }

    fn set_pending(&self, kind: ResetKind) {
        let next = match (self.pending.get(), kind) {
            (Some(ResetKind::System), _) => ResetKind::System,
            (_, ResetKind::System) => ResetKind::System,
            (Some(ResetKind::Cpu), _) => ResetKind::Cpu,
            (None, kind) => kind,
        };
        self.pending.set(Some(next));
    }
}

impl PlatformResetSink for ResetLatch {
    fn request_reset(&mut self, kind: ResetKind) {
        self.set_pending(kind);
    }
}
