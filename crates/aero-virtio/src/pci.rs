use crate::devices::VirtioDevice;
use crate::memory::{read_u16_le, GuestMemory};
use crate::queue::{VirtQueue, VirtQueueConfig};
use core::any::Any;

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
    command: u16,
    bar0: u32,
    bar0_probe: bool,

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
    size: u16,
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
            size: max_size,
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
            command: 0,
            bar0: 0,
            bar0_probe: false,
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

    pub fn bar0_base(&self) -> u32 {
        self.bar0
    }

    pub fn device_as_any_mut(&mut self) -> &mut dyn Any {
        self.device.as_any_mut()
    }

    pub fn device_mut<T: VirtioDevice + 'static>(&mut self) -> Option<&mut T> {
        self.device.as_any_mut().downcast_mut::<T>()
    }

    pub fn config_read(&self, offset: u16, data: &mut [u8]) {
        let offset = offset as usize;
        for (i, b) in data.iter_mut().enumerate() {
            *b = self.read_config_u8(offset + i);
        }
    }

    pub fn config_write(&mut self, offset: u16, data: &[u8]) {
        let offset = offset as usize;
        match (offset, data.len()) {
            // command register (status is read-only).
            (0x04, 2) => {
                self.command = u16::from_le_bytes(data.try_into().unwrap());
                self.config_space[0x04..0x06].copy_from_slice(&self.command.to_le_bytes());
            }
            (0x04, 4) => {
                self.command = u16::from_le_bytes([data[0], data[1]]);
                self.config_space[0x04..0x06].copy_from_slice(&self.command.to_le_bytes());
            }
            // BAR0 (32-bit MMIO)
            (0x10, 4) => {
                let value = u32::from_le_bytes(data.try_into().unwrap());
                if value == 0xffff_ffff {
                    self.bar0_probe = true;
                    self.bar0 = 0;
                    self.config_space[0x10..0x14].fill(0);
                } else {
                    self.bar0_probe = false;
                    self.bar0 = value & 0xffff_fff0;
                    self.config_space[0x10..0x14].copy_from_slice(&self.bar0.to_le_bytes());
                }
            }
            // interrupt line (writable)
            (0x3c, 1) => self.config_space[0x3c] = data[0],
            _ => {}
        }
    }

    fn read_config_u8(&self, offset: usize) -> u8 {
        match offset {
            // BAR0 is emulated to support size probing.
            0x10..=0x13 => {
                let value = if self.bar0_probe {
                    let size = u32::try_from(self.bar0_size).unwrap_or(u32::MAX);
                    if size.is_power_of_two() {
                        (!(size - 1)) & 0xffff_fff0
                    } else {
                        0
                    }
                } else {
                    self.bar0
                };
                let shift = (offset - 0x10) * 8;
                ((value >> shift) & 0xff) as u8
            }
            _ => *self.config_space.get(offset).unwrap_or(&0),
        }
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

    /// Process any pending queue work (including device-driven paths such as
    /// network RX) and deliver interrupts when required.
    pub fn poll(&mut self, mem: &mut dyn GuestMemory) {
        let queue_count = self.queues.len();
        for queue_index in 0..queue_count {
            self.process_queue_activity(queue_index as u16, mem);
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

        // Command (rw) and status (ro-ish). We set the status bit that indicates a
        // capabilities list is present.
        self.command = 0;
        self.config_space[0x04..0x06].copy_from_slice(&self.command.to_le_bytes());

        // Revision + class code.
        self.config_space[0x08] = 0x01; // revision
        let (class, subclass) = match self.device.device_type() {
            // Network controller / Ethernet controller.
            1 => (0x02, 0x00),
            // Mass storage / SCSI (commonly used for virtio-blk).
            2 => (0x01, 0x00),
            // Input device controller.
            18 => (0x09, 0x00),
            // Multimedia / audio device.
            25 => (0x04, 0x01),
            _ => (0x00, 0x00),
        };
        self.config_space[0x09] = 0; // prog-if
        self.config_space[0x0a] = subclass;
        self.config_space[0x0b] = class;
        self.config_space[0x0e] = 0x00; // header type

        // Subsystem vendor/device (mirror primary IDs by default).
        self.config_space[0x2c..0x2e].copy_from_slice(&PCI_VENDOR_ID_VIRTIO.to_le_bytes());
        self.config_space[0x2e..0x30].copy_from_slice(&device_id.to_le_bytes());

        // INTA#
        self.config_space[0x3d] = 0x01;

        // Status register: capability list.
        // PCI status is at 0x06.
        let status = 1u16 << 4;
        self.config_space[6..8].copy_from_slice(&status.to_le_bytes());

        // BAR0 (32-bit MMIO)
        self.config_space[0x10..0x14].copy_from_slice(&self.bar0.to_le_bytes());

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
        buf[24..26].copy_from_slice(&q.size.to_le_bytes());
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
            (0x18, 2) => {
                let val = u16::from_le_bytes(data.try_into().unwrap());
                let q = self.selected_queue_mut();
                if val != 0 && val <= q.max_size && val.is_power_of_two() {
                    q.size = val;
                }
            }
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
                size: q.size,
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
        let mult = u64::from(self.notify_off_multiplier);
        if mult == 0 || offset % mult != 0 {
            return;
        }
        let notify_off = (offset / mult) as u16;
        let Some(queue_index) = self
            .queues
            .iter()
            .position(|q| q.notify_off == notify_off)
            .map(|idx| idx as u16)
        else {
            return;
        };
        self.process_queue_activity(queue_index, mem);
    }

    fn process_queue_activity(&mut self, queue_index: u16, mem: &mut dyn GuestMemory) {
        let mut need_irq = false;
        {
            let Some(q) = self.queues.get_mut(queue_index as usize) else {
                return;
            };
            let Some(queue) = q.queue.as_mut() else {
                return;
            };

            while let Ok(Some(chain)) = queue.pop_descriptor_chain(mem) {
                need_irq |= self
                    .device
                    .process_queue(queue_index, chain, queue, mem)
                    .unwrap_or(false);
            }
            need_irq |= self
                .device
                .poll_queue(queue_index, queue, mem)
                .unwrap_or(false);
        }
        if need_irq {
            self.signal_queue_interrupt(queue_index);
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
