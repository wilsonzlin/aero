//! xHCI (USB 3.0) host controller scaffolding.
//!
//! This module currently focuses on the core data types required by an eventual xHCI controller
//! model:
//! - TRB encoding/decoding (`trb`)
//! - TRB ring walking helpers (`ring`)
//! - Register offsets/constants (`regs`)
//! - Context structures and field helpers (`context`)
//!
//! The controller implementation itself is intentionally minimal for now and may not be wired up to
//! any MMIO model yet.

pub mod context;
pub mod regs;
pub mod ring;
pub mod trb;

use aero_io_snapshot::io::state::{IoSnapshot, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter};

/// Stub xHCI controller model.
///
/// This currently exists to anchor the module and reserve a snapshot ID. Future work will extend
/// this with MMIO register modelling, interrupters, command/event ring processing, and device
/// context management.
#[derive(Debug, Default)]
pub struct XhciController {}

impl XhciController {
    pub fn new() -> Self {
        Self::default()
    }
}

impl IoSnapshot for XhciController {
    const DEVICE_ID: [u8; 4] = *b"XHCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(0, 1);

    fn save_state(&self) -> Vec<u8> {
        // No controller-local state is persisted yet.
        SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION).finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;
        Ok(())
    }
}

