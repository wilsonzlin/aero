//! Debug console ("DebugCon") device (I/O port `0xE9`).
//!
//! Many emulators (notably Bochs and QEMU) expose a simple byte sink at I/O port `0xE9`. Guests
//! (boot sectors, BIOSes, early kernels) can log with a single instruction:
//!
//! ```text
//! out 0xE9, al
//! ```
//!
//! This device models that behavior for Aero's canonical machine integration tests: byte writes to
//! `0xE9` are appended to a host-visible buffer.
use aero_platform::io::{IoPortBus, PortIoDevice};
use std::cell::RefCell;
use std::rc::Rc;

/// Canonical DebugCon port used by Bochs/QEMU.
pub const DEBUGCON_PORT: u16 = 0xE9;

/// Shared host-visible DebugCon output buffer.
pub type SharedDebugConLog = Rc<RefCell<Vec<u8>>>;

/// A minimal debug console device that appends bytes written to port `0xE9` into a shared buffer.
#[derive(Debug)]
pub struct DebugCon {
    log: SharedDebugConLog,
}

impl DebugCon {
    pub fn new(log: SharedDebugConLog) -> Self {
        Self { log }
    }
}

impl PortIoDevice for DebugCon {
    fn read(&mut self, _port: u16, size: u8) -> u32 {
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }

    fn write(&mut self, _port: u16, size: u8, value: u32) {
        if size == 0 {
            return;
        }

        let mut log = self.log.borrow_mut();
        match size {
            1 => log.push(value as u8),
            2 => log.extend_from_slice(&(value as u16).to_le_bytes()),
            4 => log.extend_from_slice(&value.to_le_bytes()),
            // Defensive default: treat any other (non-x86) access size as a single byte write.
            _ => log.push(value as u8),
        }
    }

    fn reset(&mut self) {
        self.log.borrow_mut().clear();
    }
}

/// Register a [`DebugCon`] device on `bus` at [`DEBUGCON_PORT`].
pub fn register_debugcon(bus: &mut IoPortBus, log: SharedDebugConLog) {
    bus.register(DEBUGCON_PORT, Box::new(DebugCon::new(log)));
}
