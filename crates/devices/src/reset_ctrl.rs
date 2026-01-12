//! Reset Control Register (I/O port `0xCF9`).
//!
//! Many PC chipsets expose a "Reset Control Register" at port `0xCF9`. A common
//! reboot sequence is to write `0x06`, which sets:
//! - bit 1: system reset request
//! - bit 2: reset enable
//!
//! This module models that register and raises a callback when the guest
//! requests a reset.

use aero_platform::io::PortIoDevice;
use aero_platform::reset::PlatformResetSink;

pub const RESET_CTRL_PORT: u16 = 0xCF9;
pub const RESET_CTRL_RESET_VALUE: u8 = 0x06;

const BIT_CPU_RESET: u8 = 1 << 0;
const BIT_SYSTEM_RESET: u8 = 1 << 1;
const BIT_RESET_ENABLE: u8 = 1 << 2;

pub use aero_platform::reset::ResetKind;

/// Emulates the chipset reset control register at port `0xCF9`.
///
/// Chosen semantics:
/// - bit 2 (`0x04`) is treated as a reset-enable gate.
/// - bit 1 (`0x02`) requests a full system reset.
/// - bit 0 (`0x01`) requests a CPU-only reset.
///
/// Any write with bit 2 set and either bit 1 or bit 0 set will trigger the
/// callback once per write. If both bit 1 and bit 0 are set, `System` wins.
pub struct ResetCtrl {
    value: u8,
    reset_sink: Box<dyn PlatformResetSink>,
}

impl ResetCtrl {
    pub fn new(reset_sink: impl PlatformResetSink + 'static) -> Self {
        Self {
            value: 0,
            reset_sink: Box::new(reset_sink),
        }
    }

    fn maybe_trigger_reset(&mut self, value: u8) {
        if (value & BIT_RESET_ENABLE) == 0 {
            return;
        }

        if (value & BIT_SYSTEM_RESET) != 0 {
            self.reset_sink.request_reset(ResetKind::System);
            return;
        }

        if (value & BIT_CPU_RESET) != 0 {
            self.reset_sink.request_reset(ResetKind::Cpu);
        }
    }
}

impl PortIoDevice for ResetCtrl {
    fn read(&mut self, _port: u16, size: u8) -> u32 {
        match size {
            1 | 2 | 4 => self.value as u32,
            _ => 0,
        }
    }

    fn write(&mut self, _port: u16, size: u8, value: u32) {
        if size == 0 {
            return;
        }
        let value = value as u8;
        self.value = value;
        self.maybe_trigger_reset(value);
    }

    fn reset(&mut self) {
        self.value = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[test]
    fn writing_reset_value_invokes_callback() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let mut dev = ResetCtrl::new(move |kind| {
            assert_eq!(kind, ResetKind::System);
            calls_clone.fetch_add(1, Ordering::SeqCst);
        });

        dev.write(RESET_CTRL_PORT, 1, RESET_CTRL_RESET_VALUE as u32);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn reset_enable_gate_is_required() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let mut dev = ResetCtrl::new(move |_kind| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
        });

        // System reset bit without enable should be ignored.
        dev.write(RESET_CTRL_PORT, 1, BIT_SYSTEM_RESET as u32);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn size0_write_is_noop() {
        let calls = Arc::new(AtomicUsize::new(0));
        let calls_clone = Arc::clone(&calls);

        let mut dev = ResetCtrl::new(move |_kind| {
            calls_clone.fetch_add(1, Ordering::SeqCst);
        });

        dev.write(RESET_CTRL_PORT, 0, RESET_CTRL_RESET_VALUE as u32);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(dev.read(RESET_CTRL_PORT, 1), 0);
    }
}
