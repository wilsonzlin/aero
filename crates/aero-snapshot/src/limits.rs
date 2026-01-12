//! Snapshot format limits shared by the encoder, decoder, and tooling.
//!
//! These bounds are enforced at *restore time* to keep memory usage bounded when decoding
//! potentially-corrupt snapshot inputs. The encoder (`save_snapshot`) also enforces the same
//! limits so Aero never produces snapshots it cannot restore itself.

/// Maximum number of vCPU entries supported by the snapshot format.
pub const MAX_CPU_COUNT: u32 = 256;

/// Maximum number of device entries supported by the snapshot format.
pub const MAX_DEVICE_COUNT: u32 = 4096;

/// Maximum size of the `DEVICES` section payload.
pub const MAX_DEVICES_SECTION_LEN: u64 = 256 * 1024 * 1024;

/// Maximum size of a single device state entry payload.
pub const MAX_DEVICE_ENTRY_LEN: u64 = 64 * 1024 * 1024;

/// Maximum size of `VcpuSnapshot::internal_state`.
pub const MAX_VCPU_INTERNAL_LEN: u64 = 64 * 1024 * 1024;

/// Maximum number of disk overlay references supported by the snapshot format.
pub const MAX_DISK_REFS: u32 = 256;

/// Maximum length in bytes of disk overlay reference paths.
pub const MAX_DISK_PATH_LEN: u32 = 64 * 1024;

/// Maximum length in bytes of `SnapshotMeta::label`.
pub const MAX_LABEL_LEN: u32 = 4 * 1024;

/// Maximum number of pending interrupt bytes stored in `CpuInternalState`.
pub const MAX_PENDING_INTERRUPTS: u32 = 1024 * 1024;

