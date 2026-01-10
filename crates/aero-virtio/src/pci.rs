use crate::devices::VirtioDevice;
use crate::memory::{read_u16_le, GuestMemory};
use crate::queue::{VirtQueue, VirtQueueConfig};

pub const PCI_VENDOR_ID_VIRTIO: u16 = 0x1af4;

pub const VIRTIO_PCI_DEVICE_ID_BASE: u16 = 0x1040;

pub const PCI_CAP_ID_VENDOR_SPECIFIC: u8 = 0x09;

pub const VIRTIO_PCI_CAP_COMMON_CFG: u8 = 1;
pub const VIRTIO_PCI_CAP_NOTIFY_CFG: u8 = 2;
pub const VIRTIO_PCI_CAP_ISR_CFG: u8 = 3;
pub const VIRTIO_PCI_CAP_DEVICE_CFG: u8 = 4;

pub const VIRTIO_F_RING_INDIRECT_DESC: u64 = 1 << 28;
pub const VIRTIO_F_RING_EVENT_IDX: u64 = 1 << 29;
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

pub const VIRTIO_STATUS_ACKNOWLEDGE: u8 = 1;
pub const VIRTIO_STATUS_DRIVER: u8 = 2;
pub const VIRTIO_STATUS_DRIVER_OK: u8 = 4;
pub const VIRTIO_STATUS_FEATURES_OK: u8 = 8;
pub const VIRTIO_STATUS_FAILED: u8 = 0x80;

#[derive(Debug, Default, Clone)]
pub struct InterruptLog {
    pub legacy_irq_count: u64,
    pub msix_vectors: Vec<u16>,
}

/// A sink for interrupts produced by virtio devices.
pub trait InterruptSink {
    fn raise_legacy_irq(&mut self);
    fn lower_legacy_irq(&mut self);
    fn signal_msix(&mut self, vector: u16);
}

/// A very small virtio-pci implementation (modern capabilities + split virtqueues).
///
/// The wider emulator's PCI framework will likely wrap this type, but the
/// transport logic lives here so it can be unit-tested in isolation.
pub struct VirtioPciDevice {
    config_space: [u8; 256],

    // BAR0 layout (all capabilities point into BAR0 for now).
    bar0_common_offset: u64,
    bar0_notify_offset: u64,
    bar0_isr_offset: u64,
    bar0_device_offset: u64,
    bar0_size: u64,

    notify_off_multiplier: u32,

    device: Box<dyn VirtioDevice>,
    interrupts: Box<dyn InterruptSink>,

    device_feature_select: u32,
    driver_feature_select: u32,
    driver_features: u64,
    negotiated_features: u64,

    msix_config_vector: u16,
    device_status: u8,
    config_generation: u8,

    queue_select: u16,
    queues: Vec<QueueState>,

    isr_status: u8,
    legacy_irq_asserted: bool,
}

#[derive(Debug, Clone)]
struct QueueState {
    max_size: u16,
    msix_vector: u16,
    enable: bool,
    notify_off: u16,
    desc_addr: u64,
    avail_addr: u64,
    used_addr: u64,
    queue: Option<VirtQueue>,
}

impl QueueState {
    fn new(max_size: u16, notify_off: u16) -> Self {
        Self {
            max_size,
            msix_vector: 0xffff,
            enable: false,
            notify_off,
            desc_addr: 0,
            avail_addr: 0,
            used_addr: 0,
            queue: None,
        }
    }
}

impl VirtioPciDevice {
    pub fn new(device: Box<dyn VirtioDevice>, interrupts: Box<dyn InterruptSink>) -> Self {
        let mut me = Self {
            config_space: [0u8; 256],
            bar0_common_offset: 0x0000,
            bar0_notify_offset: 0x1000,
            bar0_isr_offset: 0x2000,
            bar0_device_offset: 0x3000,
            bar0_size: 0x4000,
            notify_off_multiplier: 4,
            device,
            interrupts,
            device_feature_select: 0,
            driver_feature_select: 0,
            driver_features: 0,
            negotiated_features: 0,
            msix_config_vector: 0xffff,
            device_status: 0,
            config_generation: 0,
            queue_select: 0,
            queues: Vec::new(),
            isr_status: 0,
            legacy_irq_asserted: false,
        };
        me.reset_transport();
        me.build_config_space();
        me
    }

    pub fn bar0_size(&self) -> u64 {
        self.bar0_size
    }

    pub fn config_read(&self, offset: u16, data: &mut [u8]) {
        let offset = offset as usize;
        for (i, b) in data.iter_mut().enumerate() {
            *b = *self.config_space.get(offset + i).unwrap_or(&0);
        }
    }

    pub fn config_write(&mut self, offset: u16, data: &[u8]) {
        // This is a minimal implementation; only BAR programming and command/status bits
        // are relevant to virtio, and the emulator's PCI layer will usually handle that.
        // For now, we treat config space as mostly read-only.
        let _ = (offset, data);
    }

    pub fn bar0_read(&mut self, offset: u64, data: &mut [u8]) {
        if offset >= self.bar0_size {
            data.fill(0);
            return;
        }
        if offset >= self.bar0_common_offset && offset < self.bar0_common_offset + 0x100 {
            self.common_cfg_read(offset - self.bar0_common_offset, data);
        } else if offset >= self.bar0_isr_offset && offset < self.bar0_isr_offset + 0x20 {
            self.isr_cfg_read(offset - self.bar0_isr_offset, data);
        } else if offset >= self.bar0_device_offset && offset < self.bar0_device_offset + 0x100 {
            self.device_cfg_read(offset - self.bar0_device_offset, data);
        } else {
            // notify is write-only; unknown region reads as 0.
            data.fill(0);
        }
    }

    pub fn bar0_write(&mut self, offset: u64, data: &[u8], mem: &mut dyn GuestMemory) {
        if offset >= self.bar0_size {
            return;
        }
        if offset >= self.bar0_common_offset && offset < self.bar0_common_offset + 0x100 {
            self.common_cfg_write(offset - self.bar0_common_offset, data, mem);
        } else if offset >= self.bar0_notify_offset && offset < self.bar0_notify_offset + 0x100 {
            self.notify_cfg_write(offset - self.bar0_notify_offset, data, mem);
        } else if offset >= self.bar0_device_offset && offset < self.bar0_device_offset + 0x100 {
            self.device_cfg_write(offset - self.bar0_device_offset, data);
        } else {
            // ignore
        }
    }

    fn device_features(&self) -> u64 {
        self.device.device_features()
    }

    fn negotiated_event_idx(&self) -> bool {
        (self.negotiated_features & VIRTIO_F_RING_EVENT_IDX) != 0
    }

    fn reset_transport(&mut self) {
        self.device_feature_select = 0;
        self.driver_feature_select = 0;
        self.driver_features = 0;
        self.negotiated_features = 0;
        self.msix_config_vector = 0xffff;
        self.device_status = 0;
        self.config_generation = 0;
        self.queue_select = 0;
        self.isr_status = 0;
        self.legacy_irq_asserted = false;

        let num_queues = self.device.num_queues();
        self.queues.clear();
        for q in 0..num_queues {
            let max = self.device.queue_max_size(q);
            self.queues.push(QueueState::new(max, q));
        }
        self.device.reset();
    }

    fn build_config_space(&mut self) {
        // Minimal PCI header.
        self.config_space = [0u8; 256];
        self.config_space[0..2].copy_from_slice(&PCI_VENDOR_ID_VIRTIO.to_le_bytes());
        let device_id = VIRTIO_PCI_DEVICE_ID_BASE + self.device.device_type();
        self.config_space[2..4].copy_from_slice(&device_id.to_le_bytes());

        // Status register: capability list.
        // PCI status is at 0x06.
        let status = 1u16 << 4;
        self.config_space[6..8].copy_from_slice(&status.to_le_bytes());

        // Capability pointer at 0x34.
        let cap_base = 0x50u8;
        self.config_space[0x34] = cap_base;

        // Build vendor capabilities chain.
        let caps = [
            (
                VIRTIO_PCI_CAP_COMMON_CFG,
                self.bar0_common_offset as u32,
                0x100u32,
                16u8,
                None,
            ),
            (
                VIRTIO_PCI_CAP_NOTIFY_CFG,
                self.bar0_notify_offset as u32,
                0x100u32,
                20u8,
                Some(self.notify_off_multiplier),
            ),
            (
                VIRTIO_PCI_CAP_ISR_CFG,
                self.bar0_isr_offset as u32,
                0x20u32,
                16u8,
                None,
            ),
            (
                VIRTIO_PCI_CAP_DEVICE_CFG,
                self.bar0_device_offset as u32,
                0x100u32,
                16u8,
                None,
            ),
        ];

        let mut next_ptr = cap_base;
        for (i, (cfg_type, offset, length, cap_len, extra)) in caps.iter().enumerate() {
            let cap_off = next_ptr as usize;
            let next = if i + 1 == caps.len() {
                0u8
            } else {
                next_ptr + cap_len
            };

            // struct virtio_pci_cap
            self.config_space[cap_off + 0] = PCI_CAP_ID_VENDOR_SPECIFIC;
            self.config_space[cap_off + 1] = next;
            self.config_space[cap_off + 2] = *cap_len;
            self.config_space[cap_off + 3] = *cfg_type;
            self.config_space[cap_off + 4] = 0; // BAR0
            // padding [5..8]
            self.config_space[cap_off + 8..cap_off + 12].copy_from_slice(&offset.to_le_bytes());
            self.config_space[cap_off + 12..cap_off + 16].copy_from_slice(&length.to_le_bytes());
            if let Some(mult) = extra {
                self.config_space[cap_off + 16..cap_off + 20].copy_from_slice(&mult.to_le_bytes());
            }

            next_ptr = next;
        }
    }

    fn common_cfg_read(&self, offset: u64, data: &mut [u8]) {
        let mut buf = [0u8; 56];
        buf[0..4].copy_from_slice(&self.device_feature_select.to_le_bytes());
        let df = match self.device_feature_select {
            0 => (self.device_features() & 0xffff_ffff) as u32,
            1 => (self.device_features() >> 32) as u32,
            _ => 0,
        };
        buf[4..8].copy_from_slice(&df.to_le_bytes());
        buf[8..12].copy_from_slice(&self.driver_feature_select.to_le_bytes());
        let drf = match self.driver_feature_select {
            0 => (self.driver_features & 0xffff_ffff) as u32,
            1 => (self.driver_features >> 32) as u32,
            _ => 0,
        };
        buf[12..16].copy_from_slice(&drf.to_le_bytes());
        buf[16..18].copy_from_slice(&self.msix_config_vector.to_le_bytes());
        buf[18..20].copy_from_slice(&(self.queues.len() as u16).to_le_bytes());
        buf[20] = self.device_status;
        buf[21] = self.config_generation;
        buf[22..24].copy_from_slice(&self.queue_select.to_le_bytes());

        let q = self.selected_queue();
        buf[24..26].copy_from_slice(&q.max_size.to_le_bytes());
        buf[26..28].copy_from_slice(&q.msix_vector.to_le_bytes());
        buf[28..30].copy_from_slice(&(q.enable as u16).to_le_bytes());
        buf[30..32].copy_from_slice(&q.notify_off.to_le_bytes());
        buf[32..40].copy_from_slice(&q.desc_addr.to_le_bytes());
        buf[40..48].copy_from_slice(&q.avail_addr.to_le_bytes());
        buf[48..56].copy_from_slice(&q.used_addr.to_le_bytes());

        let start = offset as usize;
        for (i, b) in data.iter_mut().enumerate() {
            *b = *buf.get(start + i).unwrap_or(&0);
        }
    }

    fn common_cfg_write(&mut self, offset: u64, data: &[u8], mem: &mut dyn GuestMemory) {
        match (offset, data.len()) {
            (0x00, 4) => self.device_feature_select = u32::from_le_bytes(data.try_into().unwrap()),
            (0x08, 4) => self.driver_feature_select = u32::from_le_bytes(data.try_into().unwrap()),
            (0x0c, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                match self.driver_feature_select {
                    0 => {
                        self.driver_features =
                            (self.driver_features & 0xffff_ffff_0000_0000) | val;
                    }
                    1 => {
                        self.driver_features = (self.driver_features & 0x0000_0000_ffff_ffff)
                            | (val << 32);
                    }
                    _ => {}
                }
            }
            (0x10, 2) => self.msix_config_vector = u16::from_le_bytes(data.try_into().unwrap()),
            (0x14, 1) => {
                let new_status = data[0];
                if new_status == 0 {
                    self.reset_transport();
                    return;
                }
                self.device_status = new_status;
                if (self.device_status & VIRTIO_STATUS_FEATURES_OK) != 0 {
                    self.negotiate_features();
                }
            }
            (0x16, 2) => self.queue_select = u16::from_le_bytes(data.try_into().unwrap()),
            (0x1a, 2) => {
                self.selected_queue_mut().msix_vector = u16::from_le_bytes(data.try_into().unwrap())
            }
            (0x1c, 2) => {
                let enabled = u16::from_le_bytes(data.try_into().unwrap()) != 0;
                if enabled {
                    self.enable_selected_queue();
                } else {
                    let q = self.selected_queue_mut();
                    q.enable = false;
                    q.queue = None;
                }
            }
            (0x20, 8) => {
                self.selected_queue_mut().desc_addr = u64::from_le_bytes(data.try_into().unwrap())
            }
            (0x20, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                let q = self.selected_queue_mut();
                q.desc_addr = (q.desc_addr & 0xffff_ffff_0000_0000) | val;
            }
            (0x24, 4) => {
                let val = (u32::from_le_bytes(data.try_into().unwrap()) as u64) << 32;
                let q = self.selected_queue_mut();
                q.desc_addr = (q.desc_addr & 0x0000_0000_ffff_ffff) | val;
            }
            (0x28, 8) => {
                self.selected_queue_mut().avail_addr = u64::from_le_bytes(data.try_into().unwrap())
            }
            (0x28, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                let q = self.selected_queue_mut();
                q.avail_addr = (q.avail_addr & 0xffff_ffff_0000_0000) | val;
            }
            (0x2c, 4) => {
                let val = (u32::from_le_bytes(data.try_into().unwrap()) as u64) << 32;
                let q = self.selected_queue_mut();
                q.avail_addr = (q.avail_addr & 0x0000_0000_ffff_ffff) | val;
            }
            (0x30, 8) => {
                self.selected_queue_mut().used_addr = u64::from_le_bytes(data.try_into().unwrap())
            }
            (0x30, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                let q = self.selected_queue_mut();
                q.used_addr = (q.used_addr & 0xffff_ffff_0000_0000) | val;
            }
            (0x34, 4) => {
                let val = (u32::from_le_bytes(data.try_into().unwrap()) as u64) << 32;
                let q = self.selected_queue_mut();
                q.used_addr = (q.used_addr & 0x0000_0000_ffff_ffff) | val;
            }
            // Ignore everything else (including writes to read-only fields).
            _ => {
                let _ = mem;
            }
        }
    }

    fn negotiate_features(&mut self) {
        let offered = self.device_features();
        self.negotiated_features = self.driver_features & offered;
        self.device.set_features(self.negotiated_features);
        // If the device accepted the features, leave FEATURES_OK set. A device that can't
        // operate with the chosen feature set would clear it and/or set FAILED.
        self.device_status |= VIRTIO_STATUS_FEATURES_OK;
    }

    fn enable_selected_queue(&mut self) {
        let event_idx = self.negotiated_event_idx();
        let q = self.selected_queue_mut();
        q.enable = true;
        q.queue = VirtQueue::new(
            VirtQueueConfig {
                size: q.max_size,
                desc_addr: q.desc_addr,
                avail_addr: q.avail_addr,
                used_addr: q.used_addr,
            },
            event_idx,
        )
        .ok();
    }

    fn selected_queue(&self) -> &QueueState {
        self.queues
            .get(self.queue_select as usize)
            .unwrap_or_else(|| &self.queues[0])
    }

    fn selected_queue_mut(&mut self) -> &mut QueueState {
        let idx = self.queue_select as usize;
        if idx >= self.queues.len() {
            self.queue_select = 0;
        }
        &mut self.queues[self.queue_select as usize]
    }

    fn isr_cfg_read(&mut self, offset: u64, data: &mut [u8]) {
        if offset != 0 || data.is_empty() {
            data.fill(0);
            return;
        }
        data[0] = self.isr_status;
        self.isr_status = 0;
        if self.legacy_irq_asserted {
            self.interrupts.lower_legacy_irq();
            self.legacy_irq_asserted = false;
        }
        for b in data.iter_mut().skip(1) {
            *b = 0;
        }
    }

    fn device_cfg_read(&self, offset: u64, data: &mut [u8]) {
        self.device.read_config(offset, data);
    }

    fn device_cfg_write(&mut self, offset: u64, data: &[u8]) {
        self.device.write_config(offset, data);
        self.signal_config_interrupt();
    }

    fn notify_cfg_write(&mut self, offset: u64, _data: &[u8], mem: &mut dyn GuestMemory) {
        let q_index = (offset / u64::from(self.notify_off_multiplier)) as u16;
        let Some(q) = self.queues.get_mut(q_index as usize) else {
            return;
        };
        let Some(queue) = q.queue.as_mut() else {
            return;
        };
        let mut need_irq = false;
        while let Ok(Some(chain)) = queue.pop_descriptor_chain(mem) {
            need_irq |= self
                .device
                .process_queue(q_index, chain, queue, mem)
                .unwrap_or(false);
        }
        if need_irq {
            self.signal_queue_interrupt(q_index);
        }
    }

    fn signal_queue_interrupt(&mut self, queue_index: u16) {
        self.isr_status |= 0x1;
        let vec = self
            .queues
            .get(queue_index as usize)
            .map(|q| q.msix_vector)
            .unwrap_or(0xffff);
        if vec != 0xffff {
            self.interrupts.signal_msix(vec);
        } else if !self.legacy_irq_asserted {
            self.interrupts.raise_legacy_irq();
            self.legacy_irq_asserted = true;
        }
    }

    fn signal_config_interrupt(&mut self) {
        self.isr_status |= 0x2;
        if self.msix_config_vector != 0xffff {
            self.interrupts.signal_msix(self.msix_config_vector);
        } else if !self.legacy_irq_asserted {
            self.interrupts.raise_legacy_irq();
            self.legacy_irq_asserted = true;
        }
    }

    pub fn debug_queue_used_idx(&self, mem: &dyn GuestMemory, queue: u16) -> Option<u16> {
        let q = self.queues.get(queue as usize)?;
        let used_addr = q.used_addr;
        read_u16_le(mem, used_addr + 2).ok()
    }
}

impl InterruptSink for InterruptLog {
    fn raise_legacy_irq(&mut self) {
        self.legacy_irq_count += 1;
    }

    fn lower_legacy_irq(&mut self) {
        // level-triggered: no-op for the log.
    }

    fn signal_msix(&mut self, vector: u16) {
        self.msix_vectors.push(vector);
    }
}

