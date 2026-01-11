use crate::address_filter::AddressFilter;
use crate::chipset::ChipsetState;
use crate::interrupts::{PlatformInterrupts, SharedPlatformInterrupts};
use crate::io::IoPortBus;
use crate::memory::MemoryBus;
use crate::reset::{ResetKind, ResetLatch};
use std::cell::RefCell;
use std::rc::Rc;

pub struct Platform {
    pub chipset: ChipsetState,
    pub io: IoPortBus,
    pub memory: MemoryBus,
    pub interrupts: SharedPlatformInterrupts,
    reset_latch: ResetLatch,
}

impl Platform {
    pub fn new(ram_size: usize) -> Self {
        let chipset = ChipsetState::new(false);
        let filter = AddressFilter::new(chipset.a20());
        Self {
            chipset,
            io: IoPortBus::new(),
            memory: MemoryBus::new(filter, ram_size),
            interrupts: Rc::new(RefCell::new(PlatformInterrupts::new())),
            reset_latch: ResetLatch::new(),
        }
    }

    /// Returns a cloneable reset latch that implements [`crate::reset::PlatformResetSink`].
    ///
    /// Chipset devices that can request a platform reset (e.g. i8042, 0xCF9 reset control) should
    /// be wired to this sink. The VM loop is expected to call [`Platform::take_reset_request`] and
    /// perform the actual reset at a safe boundary.
    pub fn reset_latch(&self) -> ResetLatch {
        self.reset_latch.clone()
    }

    /// Take the pending reset request, if any.
    pub fn take_reset_request(&self) -> Option<ResetKind> {
        self.reset_latch.take()
    }

    /// Reset platform devices back to their power-on state.
    ///
    /// Note: CPU state reset and firmware POST are performed by the VM coordinator.
    pub fn reset_platform_state(&mut self) {
        self.chipset.a20().set_enabled(false);
        self.io.reset();
        self.interrupts.borrow_mut().reset();
    }

    /// Apply a reset request using platform-provided hooks for CPU and firmware handling.
    ///
    /// - [`ResetKind::Cpu`]: invokes `reset_cpu` only.
    /// - [`ResetKind::System`]: resets platform devices + interrupts, invokes `reset_mmio_devices`,
    ///   resets the CPU, then invokes `bios_post`.
    pub fn apply_reset(
        &mut self,
        kind: ResetKind,
        reset_cpu: &mut dyn FnMut(),
        reset_mmio_devices: &mut dyn FnMut(),
        bios_post: &mut dyn FnMut(&mut Self),
    ) {
        match kind {
            ResetKind::Cpu => reset_cpu(),
            ResetKind::System => {
                self.reset_platform_state();
                reset_mmio_devices();
                reset_cpu();
                bios_post(self);
            }
        }
    }

    /// Drain and handle a pending reset request, returning the applied reset kind.
    pub fn handle_pending_reset(
        &mut self,
        reset_cpu: &mut dyn FnMut(),
        reset_mmio_devices: &mut dyn FnMut(),
        bios_post: &mut dyn FnMut(&mut Self),
    ) -> Option<ResetKind> {
        let kind = self.take_reset_request()?;
        self.apply_reset(kind, reset_cpu, reset_mmio_devices, bios_post);
        Some(kind)
    }
}
