use crate::io::state::codec::{Decoder, Encoder};
use crate::io::state::{SnapshotError, SnapshotResult};

/// Maximum number of virtqueues supported by the virtio-pci snapshot schema.
///
/// This is a defensive bound: snapshot files may come from untrusted sources, and a corrupted
/// snapshot must not trigger pathological allocations.
pub const MAX_VIRTIO_QUEUES: usize = 16;

/// Serializable PCI config-space runtime state for a single PCI function.
///
/// This mirrors the state needed by Aero's PCI framework to restore deterministic config-space
/// behavior (including BAR probing state).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PciConfigSpaceState {
    pub bytes: [u8; 256],
    pub bar_base: [u64; 6],
    pub bar_probe: [bool; 6],
}

impl PciConfigSpaceState {
    /// Encode the config-space state into a deterministic byte sequence.
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new().bytes(&self.bytes);
        for i in 0..6 {
            enc = enc.u64(self.bar_base[i]).bool(self.bar_probe[i]);
        }
        enc.finish()
    }

    /// Decode a config-space state image.
    pub fn decode(bytes: &[u8]) -> SnapshotResult<Self> {
        const PCI_CONFIG_SPACE_SIZE: usize = 256;

        // 256 bytes config space + (6 * (u64 + bool)).
        const EXPECTED_LEN: usize = PCI_CONFIG_SPACE_SIZE + 6 * (8 + 1);

        if bytes.len() != EXPECTED_LEN {
            return Err(SnapshotError::InvalidFieldEncoding(
                "invalid pci config state length",
            ));
        }

        let mut d = Decoder::new(bytes);
        let mut cfg_bytes = [0u8; PCI_CONFIG_SPACE_SIZE];
        cfg_bytes.copy_from_slice(d.bytes(PCI_CONFIG_SPACE_SIZE)?);

        let mut bar_base = [0u64; 6];
        let mut bar_probe = [false; 6];
        for i in 0..6 {
            bar_base[i] = d.u64()?;
            bar_probe[i] = d.bool()?;
        }
        d.finish()?;

        Ok(Self {
            bytes: cfg_bytes,
            bar_base,
            bar_probe,
        })
    }
}

/// Serializable virtqueue progress state.
///
/// This captures the device-side indices tracked by the virtqueue implementation so restored
/// devices do not reprocess already-consumed avail ring entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VirtQueueProgressState {
    pub next_avail: u16,
    pub next_used: u16,
    pub event_idx: bool,
}

/// Serializable state for a single virtio-pci queue (modern transport).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPciQueueState {
    pub desc_addr: u64,
    pub avail_addr: u64,
    pub used_addr: u64,
    pub enable: bool,
    pub msix_vector: u16,
    pub notify_off: u16,
    pub progress: VirtQueueProgressState,
}

/// Serializable virtio-pci (modern) transport state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPciTransportState {
    pub device_status: u8,
    pub negotiated_features: u64,
    pub device_feature_select: u32,
    pub driver_feature_select: u32,
    pub driver_features: u64,
    pub msix_config_vector: u16,
    pub queue_select: u16,
    pub isr_status: u8,
    /// Internal legacy INTx latch (level-triggered).
    ///
    /// This represents whether the device has an unacknowledged legacy interrupt pending
    /// independent of `PCI COMMAND.INTX_DISABLE` gating. Platform integrations should gate
    /// delivery of INTx based on the live PCI command register.
    pub legacy_intx_level: bool,
    pub queues: Vec<VirtioPciQueueState>,
}

impl VirtioPciTransportState {
    /// Encode the transport state into a deterministic byte sequence.
    pub fn encode(&self) -> Vec<u8> {
        let mut enc = Encoder::new()
            .u8(self.device_status)
            .u64(self.negotiated_features)
            .u32(self.device_feature_select)
            .u32(self.driver_feature_select)
            .u64(self.driver_features)
            .u16(self.msix_config_vector)
            .u16(self.queue_select)
            .u8(self.isr_status)
            .bool(self.legacy_intx_level)
            .u32(self.queues.len() as u32);

        for q in &self.queues {
            enc = enc
                .u64(q.desc_addr)
                .u64(q.avail_addr)
                .u64(q.used_addr)
                .bool(q.enable)
                .u16(q.msix_vector)
                .u16(q.notify_off)
                .u16(q.progress.next_avail)
                .u16(q.progress.next_used)
                .bool(q.progress.event_idx);
        }

        enc.finish()
    }

    /// Decode a transport state image.
    pub fn decode(bytes: &[u8]) -> SnapshotResult<Self> {
        // Fixed-size transport header fields before the queue array.
        const FIXED_LEN: usize = 1 + 8 + 4 + 4 + 8 + 2 + 2 + 1 + 1 + 4;
        // Per-queue entry size.
        const QUEUE_ENTRY_LEN: usize = 8 + 8 + 8 + 1 + 2 + 2 + 2 + 2 + 1;

        if bytes.len() < FIXED_LEN {
            return Err(SnapshotError::UnexpectedEof);
        }

        let mut d = Decoder::new(bytes);
        let device_status = d.u8()?;
        let negotiated_features = d.u64()?;
        let device_feature_select = d.u32()?;
        let driver_feature_select = d.u32()?;
        let driver_features = d.u64()?;
        let msix_config_vector = d.u16()?;
        let queue_select = d.u16()?;
        let isr_status = d.u8()?;
        let legacy_intx_level = d.bool()?;
        let queue_count = d.u32()? as usize;

        if queue_count > MAX_VIRTIO_QUEUES {
            return Err(SnapshotError::InvalidFieldEncoding(
                "too many virtio queues",
            ));
        }

        let expected_len = FIXED_LEN
            .checked_add(queue_count.saturating_mul(QUEUE_ENTRY_LEN))
            .ok_or(SnapshotError::InvalidFieldEncoding(
                "virtio queue state size overflow",
            ))?;
        if bytes.len() != expected_len {
            return Err(SnapshotError::InvalidFieldEncoding(
                "invalid virtio transport state length",
            ));
        }

        let mut queues = Vec::with_capacity(queue_count);
        for _ in 0..queue_count {
            let desc_addr = d.u64()?;
            let avail_addr = d.u64()?;
            let used_addr = d.u64()?;
            let enable = d.bool()?;
            let msix_vector = d.u16()?;
            let notify_off = d.u16()?;
            let next_avail = d.u16()?;
            let next_used = d.u16()?;
            let event_idx = d.bool()?;

            queues.push(VirtioPciQueueState {
                desc_addr,
                avail_addr,
                used_addr,
                enable,
                msix_vector,
                notify_off,
                progress: VirtQueueProgressState {
                    next_avail,
                    next_used,
                    event_idx,
                },
            });
        }
        d.finish()?;

        Ok(Self {
            device_status,
            negotiated_features,
            device_feature_select,
            driver_feature_select,
            driver_features,
            msix_config_vector,
            queue_select,
            isr_status,
            legacy_intx_level,
            queues,
        })
    }
}

/// Full virtio-pci (modern) PCI function snapshot state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VirtioPciDeviceState {
    pub pci_config: PciConfigSpaceState,
    pub transport: VirtioPciTransportState,
}
