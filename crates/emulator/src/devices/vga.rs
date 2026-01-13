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

use crate::io::PortIO as EmulatorPortIO;
use std::cell::RefCell;
use std::rc::Rc;

/// Shared VGA device handle (interior mutable) that implements the emulator's
/// [`crate::io::PortIO`] trait.
///
/// `aero-gpu-vga` uses `&mut self` for port reads because many legacy VGA ports
/// have read side effects (e.g. attribute controller flip-flop). The emulator's
/// port I/O bus uses `&self` for reads, so the canonical device is typically
/// wired up behind a `RefCell`.
pub type SharedVgaDevice = Rc<RefCell<VgaDevice>>;

pub fn new_shared_vga_device() -> SharedVgaDevice {
    Rc::new(RefCell::new(VgaDevice::new()))
}

impl EmulatorPortIO for SharedVgaDevice {
    fn port_read(&self, port: u16, size: usize) -> u32 {
        aero_gpu_vga::PortIO::port_read(&mut *self.borrow_mut(), port, size)
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        aero_gpu_vga::PortIO::port_write(&mut *self.borrow_mut(), port, size, val)
    }
}

