use crate::devices::VirtioDevice;
use crate::memory::{read_u16_le, GuestMemory};
use crate::queue::{VirtQueue, VirtQueueConfig};
use core::any::Any;

pub const PCI_VENDOR_ID_VIRTIO: u16 = 0x1af4;

/// Modern virtio-pci device IDs: `0x1040 + <virtio device id>`.
pub const VIRTIO_PCI_DEVICE_ID_BASE: u16 = 0x1040;

/// Transitional virtio-pci device IDs: `0x1000 + (<virtio device id> - 1)`.
///
/// This matches the IDs historically used by virtio-win drivers that bind to
/// QEMU/KVM "transitional" devices (legacy + modern transports exposed).
pub const VIRTIO_PCI_DEVICE_ID_TRANSITIONAL_BASE: u16 = 0x1000;

// Legacy virtio-pci (0.9) I/O port register layout (BAR I/O space).
pub const VIRTIO_PCI_LEGACY_HOST_FEATURES: u64 = 0x00; // u32 (low 32 bits)
pub const VIRTIO_PCI_LEGACY_GUEST_FEATURES: u64 = 0x04; // u32 (low 32 bits)
pub const VIRTIO_PCI_LEGACY_QUEUE_PFN: u64 = 0x08; // u32
pub const VIRTIO_PCI_LEGACY_QUEUE_NUM: u64 = 0x0c; // u16 (max size)
pub const VIRTIO_PCI_LEGACY_QUEUE_SEL: u64 = 0x0e; // u16
pub const VIRTIO_PCI_LEGACY_QUEUE_NOTIFY: u64 = 0x10; // u16
pub const VIRTIO_PCI_LEGACY_STATUS: u64 = 0x12; // u8
pub const VIRTIO_PCI_LEGACY_ISR: u64 = 0x13; // u8 (read clears)
pub const VIRTIO_PCI_LEGACY_DEVICE_CFG: u64 = 0x14; // device-specific config space

pub const VIRTIO_PCI_LEGACY_ISR_QUEUE: u8 = 0x01;
pub const VIRTIO_PCI_LEGACY_ISR_CONFIG: u8 = 0x02;

pub const VIRTIO_PCI_LEGACY_VRING_ALIGN: u64 = 4096;

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TransportMode {
    Unknown,
    Modern,
    Legacy,
}

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
    bar1: u32,
    bar1_probe: bool,

    // BAR0 layout (all capabilities point into BAR0 for now).
    bar0_common_offset: u64,
    bar0_notify_offset: u64,
    bar0_isr_offset: u64,
    bar0_device_offset: u64,
    bar0_size: u64,
    bar1_size: u64,

    modern_enabled: bool,
    legacy_io_enabled: bool,
    use_transitional_device_id: bool,
    transport_mode: TransportMode,
    features_negotiated: bool,

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
    legacy_pfn: u32,
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
            legacy_pfn: 0,
        }
    }
}

impl VirtioPciDevice {
    pub fn new(device: Box<dyn VirtioDevice>, interrupts: Box<dyn InterruptSink>) -> Self {
        Self::new_with_options(device, interrupts, VirtioPciOptions::modern_only())
    }

    /// Create a virtio-pci *transitional* device that exposes both:
    /// - virtio 1.0+ PCI capabilities ("modern"), and
    /// - the virtio 0.9 I/O port register layout ("legacy").
    pub fn new_transitional(
        device: Box<dyn VirtioDevice>,
        interrupts: Box<dyn InterruptSink>,
    ) -> Self {
        Self::new_with_options(device, interrupts, VirtioPciOptions::transitional())
    }

    /// Create a legacy-only virtio-pci device by disabling modern PCI capabilities.
    ///
    /// This is mainly useful for testing legacy driver flows.
    pub fn new_legacy_only(
        device: Box<dyn VirtioDevice>,
        interrupts: Box<dyn InterruptSink>,
    ) -> Self {
        Self::new_with_options(device, interrupts, VirtioPciOptions::legacy_only())
    }

    fn new_with_options(
        device: Box<dyn VirtioDevice>,
        interrupts: Box<dyn InterruptSink>,
        options: VirtioPciOptions,
    ) -> Self {
        let mut me = Self {
            config_space: [0u8; 256],
            command: 0,
            bar0: 0,
            bar0_probe: false,
            bar1: if options.legacy_io_enabled { 0x1 } else { 0 },
            bar1_probe: false,
            bar0_common_offset: 0x0000,
            bar0_notify_offset: 0x1000,
            bar0_isr_offset: 0x2000,
            bar0_device_offset: 0x3000,
            bar0_size: 0x4000,
            bar1_size: if options.legacy_io_enabled { 0x100 } else { 0 },
            modern_enabled: options.modern_enabled,
            legacy_io_enabled: options.legacy_io_enabled,
            use_transitional_device_id: options.use_transitional_device_id,
            transport_mode: TransportMode::Unknown,
            features_negotiated: false,
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

    pub fn legacy_io_size(&self) -> u64 {
        self.bar1_size
    }

    pub fn legacy_io_base(&self) -> u32 {
        self.bar1
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
            // BAR1 (32-bit I/O) for legacy transport (when enabled).
            (0x14, 4) if self.legacy_io_enabled => {
                let value = u32::from_le_bytes(data.try_into().unwrap());
                if value == 0xffff_ffff {
                    self.bar1_probe = true;
                    self.bar1 = 0;
                    self.config_space[0x14..0x18].fill(0);
                } else {
                    self.bar1_probe = false;
                    self.bar1 = (value & 0xffff_fffc) | 0x1;
                    self.config_space[0x14..0x18].copy_from_slice(&self.bar1.to_le_bytes());
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
            // BAR1 is emulated to support size probing.
            0x14..=0x17 if self.legacy_io_enabled => {
                let value = if self.bar1_probe {
                    let size = u32::try_from(self.bar1_size).unwrap_or(u32::MAX);
                    let mask = if size.is_power_of_two() {
                        (!(size - 1)) & 0xffff_fffc
                    } else {
                        0
                    };
                    mask | 0x1
                } else {
                    self.bar1
                };
                let shift = (offset - 0x14) * 8;
                ((value >> shift) & 0xff) as u8
            }
            _ => *self.config_space.get(offset).unwrap_or(&0),
        }
    }

    pub fn bar0_read(&mut self, offset: u64, data: &mut [u8]) {
        if !self.modern_enabled {
            data.fill(0);
            return;
        }
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
        if !self.modern_enabled {
            return;
        }
        if offset >= self.bar0_size {
            return;
        }
        if !self.lock_transport_mode(TransportMode::Modern, offset, data) {
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

    /// Read from the legacy virtio-pci I/O port register block.
    pub fn legacy_io_read(&mut self, offset: u64, data: &mut [u8]) {
        if !self.legacy_io_enabled {
            data.fill(0);
            return;
        }
        match offset {
            VIRTIO_PCI_LEGACY_HOST_FEATURES => {
                write_u32_to(data, (self.device_features() & 0xffff_ffff) as u32);
            }
            VIRTIO_PCI_LEGACY_QUEUE_PFN => {
                let pfn = self
                    .queues
                    .get(self.queue_select as usize)
                    .map(|q| q.legacy_pfn)
                    .unwrap_or(0);
                write_u32_to(data, pfn);
            }
            VIRTIO_PCI_LEGACY_QUEUE_NUM => {
                let max_size = self
                    .queues
                    .get(self.queue_select as usize)
                    .map(|q| q.max_size)
                    .unwrap_or(0);
                write_u16_to(data, max_size);
            }
            VIRTIO_PCI_LEGACY_STATUS => write_u8_to(data, self.device_status),
            VIRTIO_PCI_LEGACY_ISR => {
                let isr = self.read_isr_and_clear();
                write_u8_to(data, isr);
            }
            off if off >= VIRTIO_PCI_LEGACY_DEVICE_CFG => {
                let cfg_off = off - VIRTIO_PCI_LEGACY_DEVICE_CFG;
                self.device_cfg_read(cfg_off, data);
            }
            _ => data.fill(0),
        }
    }

    /// Write to the legacy virtio-pci I/O port register block.
    pub fn legacy_io_write(&mut self, offset: u64, data: &[u8], mem: &mut dyn GuestMemory) {
        if !self.legacy_io_enabled {
            return;
        }
        if !self.lock_transport_mode(TransportMode::Legacy, offset, data) {
            return;
        }

        match offset {
            VIRTIO_PCI_LEGACY_GUEST_FEATURES => {
                let features = read_le_bytes_u32(data) as u64;
                // Legacy transport can only negotiate the low 32 bits.
                self.driver_features = features;
                // Legacy drivers generally don't set FEATURES_OK; negotiate immediately.
                self.negotiate_features();
            }
            VIRTIO_PCI_LEGACY_QUEUE_SEL => {
                self.queue_select = read_le_bytes_u16(data);
            }
            VIRTIO_PCI_LEGACY_QUEUE_PFN => {
                let pfn = read_le_bytes_u32(data);
                let Some(q) = self.queues.get_mut(self.queue_select as usize) else {
                    return;
                };
                q.legacy_pfn = pfn;
                if pfn == 0 {
                    q.enable = false;
                    q.queue = None;
                } else {
                    q.size = q.max_size;
                    let (desc, avail, used) = legacy_vring_addresses(pfn, q.size);
                    q.desc_addr = desc;
                    q.avail_addr = avail;
                    q.used_addr = used;
                    self.enable_selected_queue();
                }
            }
            VIRTIO_PCI_LEGACY_QUEUE_NOTIFY => {
                let queue_index = read_le_bytes_u16(data);
                if queue_index as usize >= self.queues.len() {
                    return;
                }
                self.process_queue_activity(queue_index, mem);
            }
            VIRTIO_PCI_LEGACY_STATUS => {
                let new_status = data.first().copied().unwrap_or(0);
                if new_status == 0 {
                    self.reset_transport();
                    return;
                }
                self.device_status = new_status;
            }
            off if off >= VIRTIO_PCI_LEGACY_DEVICE_CFG => {
                let cfg_off = off - VIRTIO_PCI_LEGACY_DEVICE_CFG;
                self.device_cfg_write(cfg_off, data);
            }
            _ => {}
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
        self.transport_mode = TransportMode::Unknown;
        self.features_negotiated = false;

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
        let device_id = self.pci_device_id();
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
            // Display controller / other (virtio-gpu).
            16 => (0x03, 0x80),
            // Input device controller / other.
            18 => (0x09, 0x80),
            // Multimedia / audio device.
            25 => (0x04, 0x01),
            _ => (0x00, 0x00),
        };
        self.config_space[0x09] = 0; // prog-if
        self.config_space[0x0a] = subclass;
        self.config_space[0x0b] = class;
        self.config_space[0x0e] = 0x00; // header type

        // Subsystem vendor/device.
        //
        // Subsystem device ID is used as a stable secondary identifier. By default
        // it mirrors the virtio device type, but devices may override it to
        // distinguish variants (see `VirtioDevice::subsystem_device_id()`).
        let subsystem_id = self.device.subsystem_device_id();
        self.config_space[0x2c..0x2e].copy_from_slice(&PCI_VENDOR_ID_VIRTIO.to_le_bytes());
        self.config_space[0x2e..0x30].copy_from_slice(&subsystem_id.to_le_bytes());

        // INTA#
        self.config_space[0x3d] = 0x01;

        // Status register: capability list (only when modern capabilities are exposed).
        // PCI status is at 0x06.
        if self.modern_enabled {
            let status = 1u16 << 4;
            self.config_space[6..8].copy_from_slice(&status.to_le_bytes());
        }

        // BAR0 (32-bit MMIO)
        self.config_space[0x10..0x14].copy_from_slice(&self.bar0.to_le_bytes());

        // BAR1 (32-bit I/O) for legacy transport.
        if self.legacy_io_enabled {
            self.config_space[0x14..0x18].copy_from_slice(&self.bar1.to_le_bytes());
        }

        // Modern virtio-pci capabilities (vendor-specific) live in the PCI capability list.
        if self.modern_enabled {
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
                self.config_space[cap_off + 12..cap_off + 16]
                    .copy_from_slice(&length.to_le_bytes());
                if let Some(mult) = extra {
                    self.config_space[cap_off + 16..cap_off + 20]
                        .copy_from_slice(&mult.to_le_bytes());
                }

                next_ptr = next;
            }
        } else {
            self.config_space[0x34] = 0;
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
                        self.driver_features = (self.driver_features & 0xffff_ffff_0000_0000) | val;
                    }
                    1 => {
                        self.driver_features =
                            (self.driver_features & 0x0000_0000_ffff_ffff) | (val << 32);
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
        self.features_negotiated = true;
        let event_idx = self.negotiated_event_idx();
        for q in &mut self.queues {
            if let Some(vq) = q.queue.as_mut() {
                vq.set_event_idx(event_idx);
            }
        }
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
        data[0] = self.read_isr_and_clear();
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
                let head_index = chain.head_index();
                need_irq |= match self
                    .device
                    .process_queue(queue_index, chain, queue, mem)
                {
                    Ok(irq) => irq,
                    Err(_) => {
                        // VirtioDevice implementations are expected to add a used entry for every
                        // descriptor chain they pop. Historically, some devices returned an error
                        // for malformed chains without completing them; because the transport
                        // ignores device errors, that behaviour wedges the virtqueue (the driver
                        // waits forever for used->idx to advance).
                        //
                        // As a safety net, complete the chain with `used.len = 0` on any device
                        // error so the guest can recover and continue issuing requests.
                        queue.add_used(mem, head_index, 0).unwrap_or(false)
                    }
                };
            }
            need_irq |= self
                .device
                .poll_queue(queue_index, queue, mem)
                .unwrap_or(false);

            // When EVENT_IDX is enabled, keep `avail_event` up-to-date so the guest
            // driver can correctly suppress/publish notifications.
            let _ = queue.update_avail_event(mem);
        }
        if need_irq {
            self.signal_queue_interrupt(queue_index);
        }
    }

    fn signal_queue_interrupt(&mut self, queue_index: u16) {
        self.isr_status |= VIRTIO_PCI_LEGACY_ISR_QUEUE;
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
        self.isr_status |= VIRTIO_PCI_LEGACY_ISR_CONFIG;
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

    fn pci_device_id(&self) -> u16 {
        let typ = self.device.device_type();
        if self.use_transitional_device_id {
            if typ == 0 {
                VIRTIO_PCI_DEVICE_ID_TRANSITIONAL_BASE
            } else {
                VIRTIO_PCI_DEVICE_ID_TRANSITIONAL_BASE + typ.saturating_sub(1)
            }
        } else {
            VIRTIO_PCI_DEVICE_ID_BASE + typ
        }
    }

    fn lock_transport_mode(&mut self, desired: TransportMode, offset: u64, data: &[u8]) -> bool {
        // Always allow reset regardless of mode, so guests can recover.
        if desired == TransportMode::Legacy && offset == VIRTIO_PCI_LEGACY_STATUS {
            if data.first().copied().unwrap_or(0) == 0 {
                return true;
            }
        }
        if desired == TransportMode::Modern
            && offset == 0x14
            && data.first().copied().unwrap_or(0) == 0
        {
            return true;
        }

        match self.transport_mode {
            TransportMode::Unknown => {
                self.transport_mode = desired;
                true
            }
            mode if mode == desired => true,
            _ => false,
        }
    }

    fn read_isr_and_clear(&mut self) -> u8 {
        let isr = self.isr_status;
        self.isr_status = 0;
        if self.legacy_irq_asserted {
            self.interrupts.lower_legacy_irq();
            self.legacy_irq_asserted = false;
        }
        isr
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

#[derive(Debug, Clone, Copy)]
struct VirtioPciOptions {
    modern_enabled: bool,
    legacy_io_enabled: bool,
    use_transitional_device_id: bool,
}

impl VirtioPciOptions {
    fn modern_only() -> Self {
        Self {
            modern_enabled: true,
            legacy_io_enabled: false,
            use_transitional_device_id: false,
        }
    }

    fn transitional() -> Self {
        Self {
            modern_enabled: true,
            legacy_io_enabled: true,
            use_transitional_device_id: true,
        }
    }

    fn legacy_only() -> Self {
        Self {
            modern_enabled: false,
            legacy_io_enabled: true,
            use_transitional_device_id: true,
        }
    }
}

fn align_up(value: u64, align: u64) -> u64 {
    debug_assert!(align.is_power_of_two());
    (value + align - 1) & !(align - 1)
}

fn legacy_vring_addresses(pfn: u32, queue_size: u16) -> (u64, u64, u64) {
    let base = u64::from(pfn) << 12;
    let desc = base;
    let avail = desc + 16 * u64::from(queue_size);
    let used_unaligned = avail + 4 + 2 * u64::from(queue_size) + 2;
    let used = align_up(used_unaligned, VIRTIO_PCI_LEGACY_VRING_ALIGN);
    (desc, avail, used)
}

fn write_u8_to(dst: &mut [u8], value: u8) {
    if dst.is_empty() {
        return;
    }
    dst[0] = value;
    if dst.len() > 1 {
        dst[1..].fill(0);
    }
}

fn write_u16_to(dst: &mut [u8], value: u16) {
    let bytes = value.to_le_bytes();
    let take = dst.len().min(bytes.len());
    dst[..take].copy_from_slice(&bytes[..take]);
    if take < dst.len() {
        dst[take..].fill(0);
    }
}

fn write_u32_to(dst: &mut [u8], value: u32) {
    let bytes = value.to_le_bytes();
    let take = dst.len().min(bytes.len());
    dst[..take].copy_from_slice(&bytes[..take]);
    if take < dst.len() {
        dst[take..].fill(0);
    }
}

fn read_le_bytes_u16(src: &[u8]) -> u16 {
    let mut buf = [0u8; 2];
    let take = src.len().min(2);
    buf[..take].copy_from_slice(&src[..take]);
    u16::from_le_bytes(buf)
}

fn read_le_bytes_u32(src: &[u8]) -> u32 {
    let mut buf = [0u8; 4];
    let take = src.len().min(4);
    buf[..take].copy_from_slice(&src[..take]);
    u32::from_le_bytes(buf)
}
