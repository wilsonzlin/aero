//! Deterministic snapshot encoding for I/O devices and host-side I/O state.
//!
//! The snapshot format uses a small tag-length-value (TLV) encoding to provide:
//! - deterministic byte output (canonical tag ordering)
//! - forward compatibility (unknown tags are skipped)
//! - explicit versioning (major/minor) at both format and device level

mod version;

pub use version::{
    codec, SnapshotError, SnapshotHeader, SnapshotReader, SnapshotResult, SnapshotVersion,
    SnapshotWriter,
};

/// Snapshotting contract for emulated I/O devices and host-side I/O state.
///
/// Implementations must keep `DEVICE_ID` stable forever and only perform forward-compatible
/// additions within the same major version by adding new TLV fields.
pub trait IoSnapshot {
    const DEVICE_ID: [u8; 4];
    const DEVICE_VERSION: SnapshotVersion;

    fn save_state(&self) -> Vec<u8>;
    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()>;
}
