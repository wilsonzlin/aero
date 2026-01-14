//! Legacy VGA/SVGA (Bochs VBE) device model.
//!
//! The project previously carried a second VGA/VBE implementation under
//! `crates/emulator/src/devices/vga/*`. That copy has been removed in favour of the
//! canonical `aero-gpu-vga` crate, which is shared by `aero-machine` and `aero-wasm`.
//!
//! This module is intentionally small: it primarily re-exports `aero-gpu-vga` so
//! existing users can depend on `emulator::devices::vga` while the rest of the
//! emulator wiring is migrated.

pub use aero_gpu_vga::*;

use std::cell::RefCell;
use std::rc::Rc;

/// Shared VGA device handle (interior mutable).
///
/// `aero-gpu-vga` uses `&mut self` for port reads because many legacy VGA ports
/// have read side effects (e.g. attribute controller flip-flop). The emulator's
/// port I/O wiring typically shares the VGA device with other components (MMIO,
/// rendering) behind a `RefCell`.
pub type SharedVgaDevice = Rc<RefCell<VgaDevice>>;

pub fn new_shared_vga_device() -> SharedVgaDevice {
    Rc::new(RefCell::new(VgaDevice::new()))
}

/// Create a [`VgaPortIoDevice`] wrapper around a shared VGA device.
pub fn new_vga_portio_device(dev: SharedVgaDevice) -> VgaPortIoDevice {
    VgaPortIoDevice { dev }
}
