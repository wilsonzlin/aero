//! Legacy virtio core utilities.
//!
//! This module is part of `aero_devices::io::virtio`, which is a legacy virtio implementation kept
//! for backwards compatibility. New code should use the canonical `aero_virtio` crate instead.

mod queue;

pub use memory::{DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, SparseMemory};
pub use queue::{
    DescChain, VirtQueue, VirtQueueError, VIRTQ_DESC_F_INDIRECT, VIRTQ_DESC_F_NEXT,
    VIRTQ_DESC_F_WRITE, VRING_AVAIL_F_NO_INTERRUPT,
};
