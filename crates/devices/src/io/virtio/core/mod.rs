mod queue;

pub use memory::{DenseMemory, GuestMemory, GuestMemoryError, GuestMemoryResult, SparseMemory};
pub use queue::{
    DescChain, VirtQueue, VirtQueueError, VIRTQ_DESC_F_INDIRECT, VIRTQ_DESC_F_NEXT,
    VIRTQ_DESC_F_WRITE, VRING_AVAIL_F_NO_INTERRUPT,
};
