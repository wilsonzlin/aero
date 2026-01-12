use crate::devices::VirtioDevice;
use crate::memory::{read_u16_le, GuestMemory};
use crate::queue::{PoppedDescriptorChain, VirtQueue, VirtQueueConfig};
use aero_devices::pci::capabilities::VendorSpecificCapability;
use aero_devices::pci::{PciBarDefinition, PciConfigSpace, PciInterruptPin, PciSubsystemIds};
use core::any::Any;

use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_io_snapshot::io::virtio::state::{
    PciConfigSpaceState as SnapshotPciConfigSpaceState,
    VirtQueueProgressState as SnapshotVirtQueueProgressState,
    VirtioPciQueueState as SnapshotVirtioPciQueueState,
    VirtioPciTransportState as SnapshotVirtioPciTransportState,
};

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
    config: PciConfigSpace,

    // BAR0 layout (all capabilities point into BAR0 for now).
    bar0_common_offset: u64,
    bar0_notify_offset: u64,
    bar0_isr_offset: u64,
    bar0_device_offset: u64,
    bar0_size: u64,
    bar2_size: u64,

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
    /// Internal legacy INTx latch: whether the device would like to assert legacy INTx
    /// (subject to PCI COMMAND.INTX_DISABLE gating).
    legacy_irq_pending: bool,
    /// Whether the device has currently asserted the legacy INTx line via the interrupt sink.
    ///
    /// This is tracked separately from [`Self::legacy_irq_pending`] so we can correctly emulate
    /// `PCI COMMAND.INTX_DISABLE` (bit 10): when INTx is disabled we keep the internal pending
    /// state but must deassert the external line.
    legacy_irq_line: bool,
}

#[derive(Debug, Clone)]
struct QueueState {
    max_size: u16,
    size: u16,
    msix_vector: u16,
    enable: bool,
    pending_notify: bool,
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
            pending_notify: false,
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
            // Placeholder identity; overwritten by `build_config_space`.
            config: PciConfigSpace::new(PCI_VENDOR_ID_VIRTIO, 0),
            bar0_common_offset: 0x0000,
            bar0_notify_offset: 0x1000,
            bar0_isr_offset: 0x2000,
            bar0_device_offset: 0x3000,
            bar0_size: 0x4000,
            bar2_size: if options.legacy_io_enabled { 0x100 } else { 0 },
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
            legacy_irq_pending: false,
            legacy_irq_line: false,
        };
        me.reset_transport();
        me.build_config_space();
        me
    }

    pub fn bar0_size(&self) -> u64 {
        self.bar0_size
    }

    pub fn bar0_base(&self) -> u64 {
        self.config.bar_range(0).map(|r| r.base).unwrap_or(0)
    }

    pub fn legacy_io_size(&self) -> u64 {
        self.bar2_size
    }

    pub fn legacy_io_base(&self) -> u32 {
        self.config.bar_range(2).map(|r| r.base as u32).unwrap_or(0)
    }

    /// Reset the virtio-pci transport back to its power-on baseline.
    ///
    /// This is intended for platform-level resets (e.g. `PcPlatform::reset()`), where the device
    /// should forget any negotiated features, queue configuration, and pending interrupts while
    /// **preserving** any host-attached backend owned by the inner [`VirtioDevice`].
    ///
    /// This mirrors the reset semantics used by other PCI device models in this repo:
    /// - BAR programming is preserved
    /// - PCI decoding is disabled (`COMMAND = 0`)
    pub fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        self.config.set_command(0);
        self.reset_transport();
    }

    /// Returns whether the device is currently asserting its legacy INTx interrupt line.
    ///
    /// Virtio legacy interrupts are level-triggered and are deasserted when the guest reads the
    /// ISR status register.
    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.legacy_irq_pending
    }

    fn command(&self) -> u16 {
        self.config.command()
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & (1 << 2)) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    /// Set the device's view of the PCI command register (offset 0x04, low 16 bits).
    ///
    /// Some platform integrations maintain PCI config space separately from the virtio transport
    /// model; those integrations should call this method whenever the guest updates the PCI
    /// command register so the virtio transport can correctly apply:
    /// - bus mastering (DMA) gating via `COMMAND.BME` (bit 2), and
    /// - legacy INTx gating via `COMMAND.INTX_DISABLE` (bit 10).
    pub fn set_pci_command(&mut self, command: u16) {
        self.config.set_command(command);
        self.sync_legacy_irq_line();
    }

    pub fn device_as_any_mut(&mut self) -> &mut dyn Any {
        self.device.as_any_mut()
    }

    pub fn device_mut<T: VirtioDevice + 'static>(&mut self) -> Option<&mut T> {
        self.device.as_any_mut().downcast_mut::<T>()
    }

    pub fn config_read(&mut self, offset: u16, data: &mut [u8]) {
        if data.is_empty() {
            return;
        }
        for (i, b) in data.iter_mut().enumerate() {
            let off = offset.wrapping_add(i as u16);
            *b = self.config.read(off, 1) as u8;
        }
    }

    pub fn config_write(&mut self, offset: u16, data: &[u8]) {
        let prev_command = self.command();
        match data.len() {
            0 => {}
            1 => self.config.write(offset, 1, u32::from(data[0])),
            2 => self
                .config
                .write(offset, 2, u32::from(u16::from_le_bytes([data[0], data[1]]))),
            4 => self
                .config
                .write(offset, 4, u32::from_le_bytes(data.try_into().unwrap())),
            _ => {
                // Byte-split any odd writes; this is not performance-critical for our unit tests.
                for (i, b) in data.iter().enumerate() {
                    let off = offset.wrapping_add(i as u16);
                    self.config.write(off, 1, u32::from(*b));
                }
            }
        }

        // Apply INTx disable gating if the guest updated the PCI command register.
        if self.command() != prev_command {
            self.sync_legacy_irq_line();
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

    /// Write to the modern virtio-pci BAR0 register space.
    ///
    /// This method is intentionally **side-effect free with respect to guest memory**: queue
    /// notifications only mark a queue as needing service. Call [`VirtioPciDevice::process_notified_queues`]
    /// later with access to guest RAM to execute the notified virtqueues.
    pub fn bar0_write(&mut self, offset: u64, data: &[u8]) {
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
            self.common_cfg_write(offset - self.bar0_common_offset, data);
        } else if offset >= self.bar0_notify_offset && offset < self.bar0_notify_offset + 0x100 {
            self.notify_cfg_write(offset - self.bar0_notify_offset, data);
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
    ///
    /// Like [`VirtioPciDevice::bar0_write`], this method does not access guest memory. Queue
    /// notifications only mark a queue as pending; call [`VirtioPciDevice::process_notified_queues`]
    /// later with access to guest RAM to execute the virtqueue(s).
    pub fn legacy_io_write(&mut self, offset: u64, data: &[u8]) {
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
                if let Some(q) = self.queues.get_mut(queue_index as usize) {
                    q.pending_notify = true;
                }
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
        // Gate virtqueue DMA on PCI command Bus Master Enable (bit 2).
        //
        // This prevents the device from touching guest memory (virtqueue structures + buffers)
        // before the guest explicitly enables PCI bus mastering during enumeration.
        if !self.bus_master_enabled() {
            return;
        }
        let queue_count = self.queues.len();
        for queue_index in 0..queue_count {
            if let Some(q) = self.queues.get_mut(queue_index) {
                if q.queue.is_some() {
                    q.pending_notify = false;
                }
            }
            self.process_queue_activity(queue_index as u16, mem);
        }
    }

    /// Process any virtqueues that have pending work.
    ///
    /// This is intended for platform integrations that cannot perform guest-memory DMA from inside
    /// MMIO handlers. In such setups, BAR0 notify writes should only record that the queue needs
    /// servicing, and the platform should call this method during its main processing loop with
    /// access to guest RAM.
    ///
    /// In addition to explicit notify writes, this method also treats a queue as pending when it
    /// has unconsumed available entries (`avail.idx != next_avail`). This makes snapshot/restore
    /// robust if a snapshot is taken after the guest posts buffers (and possibly kicks the queue)
    /// but before the platform gets a chance to service the notify.
    pub fn process_notified_queues(&mut self, mem: &mut dyn GuestMemory) {
        // Gate virtqueue DMA on PCI command Bus Master Enable (bit 2).
        if !self.bus_master_enabled() {
            return;
        }
        let queue_count = self.queues.len();
        for queue_index in 0..queue_count {
            let pending = self.queues.get(queue_index).is_some_and(|q| {
                let Some(vq) = q.queue.as_ref() else {
                    return false;
                };

                // In addition to explicit notify writes, treat any queue with unconsumed avail
                // entries as pending. This makes snapshot/restore robust when a snapshot is taken
                // after the guest posts buffers but before the platform processes the notify.
                if q.pending_notify {
                    return true;
                }

                let Some(avail_idx_addr) = q.avail_addr.checked_add(2) else {
                    return false;
                };
                let Ok(avail_idx) = read_u16_le(&*mem, avail_idx_addr) else {
                    return false;
                };
                avail_idx != vq.next_avail()
            });
            if !pending {
                continue;
            }
            if let Some(q) = self.queues.get_mut(queue_index) {
                q.pending_notify = false;
            }
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
        self.legacy_irq_pending = false;
        self.sync_legacy_irq_line();
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
        let device_id = self.pci_device_id();
        let mut cfg = PciConfigSpace::new(PCI_VENDOR_ID_VIRTIO, device_id);

        // Revision + class code.
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
        cfg.set_class_code(class, subclass, 0, 0x01);
        cfg.write(0x0e, 1, u32::from(self.device.pci_header_type()));

        cfg.set_subsystem_ids(PciSubsystemIds {
            subsystem_vendor_id: PCI_VENDOR_ID_VIRTIO,
            subsystem_id: self.device.subsystem_device_id(),
        });

        // Expose as INTA#.
        cfg.set_interrupt_pin(PciInterruptPin::IntA.to_config_u8());
        cfg.set_interrupt_line(0xFF);

        // BAR0 (modern virtio-pci): 64-bit MMIO, 0x4000 bytes.
        cfg.set_bar_definition(
            0,
            PciBarDefinition::Mmio64 {
                size: self.bar0_size,
                prefetchable: false,
            },
        );

        // BAR2 (legacy virtio-pci): I/O space register block (optional).
        if self.legacy_io_enabled {
            cfg.set_bar_definition(
                2,
                PciBarDefinition::Io {
                    size: u32::try_from(self.bar2_size).unwrap_or(0),
                },
            );
        }

        // Modern virtio-pci capabilities.
        if self.modern_enabled {
            use aero_devices::pci::profile::{
                VIRTIO_CAP_COMMON, VIRTIO_CAP_DEVICE, VIRTIO_CAP_ISR, VIRTIO_CAP_NOTIFY,
            };

            cfg.add_capability(Box::new(VendorSpecificCapability::new(
                VIRTIO_CAP_COMMON.to_vec(),
            )));
            cfg.add_capability(Box::new(VendorSpecificCapability::new(
                VIRTIO_CAP_NOTIFY.to_vec(),
            )));
            cfg.add_capability(Box::new(VendorSpecificCapability::new(
                VIRTIO_CAP_ISR.to_vec(),
            )));
            cfg.add_capability(Box::new(VendorSpecificCapability::new(
                VIRTIO_CAP_DEVICE.to_vec(),
            )));
        }

        self.config = cfg;
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

        if let Some(q) = self.selected_queue() {
            // Contract v1 fixes queue sizes; `queue_size` is treated as read-only and always
            // returns the maximum supported size.
            buf[24..26].copy_from_slice(&q.max_size.to_le_bytes());
            buf[26..28].copy_from_slice(&q.msix_vector.to_le_bytes());
            buf[28..30].copy_from_slice(&(q.enable as u16).to_le_bytes());
            buf[30..32].copy_from_slice(&q.notify_off.to_le_bytes());
            buf[32..40].copy_from_slice(&q.desc_addr.to_le_bytes());
            buf[40..48].copy_from_slice(&q.avail_addr.to_le_bytes());
            buf[48..56].copy_from_slice(&q.used_addr.to_le_bytes());
        }

        let start = offset as usize;
        for (i, b) in data.iter_mut().enumerate() {
            *b = *buf.get(start + i).unwrap_or(&0);
        }
    }

    fn common_cfg_write(&mut self, offset: u64, data: &[u8]) {
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
            (0x1a, 2) => {
                if let Some(q) = self.selected_queue_mut() {
                    q.msix_vector = u16::from_le_bytes(data.try_into().unwrap());
                }
            }
            (0x1c, 2) => {
                let enabled = u16::from_le_bytes(data.try_into().unwrap()) != 0;
                if enabled {
                    self.enable_selected_queue();
                } else if let Some(q) = self.selected_queue_mut() {
                    q.enable = false;
                    q.queue = None;
                }
            }
            (0x20, 8) => {
                if let Some(q) = self.selected_queue_mut() {
                    q.desc_addr = u64::from_le_bytes(data.try_into().unwrap());
                }
            }
            (0x20, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                if let Some(q) = self.selected_queue_mut() {
                    q.desc_addr = (q.desc_addr & 0xffff_ffff_0000_0000) | val;
                }
            }
            (0x24, 4) => {
                let val = (u32::from_le_bytes(data.try_into().unwrap()) as u64) << 32;
                if let Some(q) = self.selected_queue_mut() {
                    q.desc_addr = (q.desc_addr & 0x0000_0000_ffff_ffff) | val;
                }
            }
            (0x28, 8) => {
                if let Some(q) = self.selected_queue_mut() {
                    q.avail_addr = u64::from_le_bytes(data.try_into().unwrap());
                }
            }
            (0x28, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                if let Some(q) = self.selected_queue_mut() {
                    q.avail_addr = (q.avail_addr & 0xffff_ffff_0000_0000) | val;
                }
            }
            (0x2c, 4) => {
                let val = (u32::from_le_bytes(data.try_into().unwrap()) as u64) << 32;
                if let Some(q) = self.selected_queue_mut() {
                    q.avail_addr = (q.avail_addr & 0x0000_0000_ffff_ffff) | val;
                }
            }
            (0x30, 8) => {
                if let Some(q) = self.selected_queue_mut() {
                    q.used_addr = u64::from_le_bytes(data.try_into().unwrap());
                }
            }
            (0x30, 4) => {
                let val = u32::from_le_bytes(data.try_into().unwrap()) as u64;
                if let Some(q) = self.selected_queue_mut() {
                    q.used_addr = (q.used_addr & 0xffff_ffff_0000_0000) | val;
                }
            }
            (0x34, 4) => {
                let val = (u32::from_le_bytes(data.try_into().unwrap()) as u64) << 32;
                if let Some(q) = self.selected_queue_mut() {
                    q.used_addr = (q.used_addr & 0x0000_0000_ffff_ffff) | val;
                }
            }
            // Ignore everything else (including writes to read-only fields).
            _ => {}
        }
    }

    fn negotiate_features(&mut self) {
        let offered = self.device_features();
        let requested = self.driver_features;

        // A driver that sets any feature bit the device did not offer has violated the virtio
        // feature negotiation contract. Per the Aero Win7 contract, the device must clear
        // FEATURES_OK so the driver can detect the failure and reset.
        if (requested & !offered) != 0 {
            self.negotiated_features = 0;
            self.features_negotiated = false;
            self.device_status &= !VIRTIO_STATUS_FEATURES_OK;
            return;
        }

        // Contract v1 requires modern drivers to accept VIRTIO_F_VERSION_1.
        if self.transport_mode == TransportMode::Modern && (requested & VIRTIO_F_VERSION_1) == 0 {
            self.negotiated_features = 0;
            self.features_negotiated = false;
            self.device_status &= !VIRTIO_STATUS_FEATURES_OK;
            return;
        }

        self.negotiated_features = requested & offered;
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
        let Some(q) = self.selected_queue_mut() else {
            return;
        };
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

    fn selected_queue(&self) -> Option<&QueueState> {
        self.queues.get(self.queue_select as usize)
    }

    fn selected_queue_mut(&mut self) -> Option<&mut QueueState> {
        self.queues.get_mut(self.queue_select as usize)
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
    }

    fn notify_cfg_write(&mut self, offset: u64, _data: &[u8]) {
        let mult = u64::from(self.notify_off_multiplier);
        if mult == 0 || !offset.is_multiple_of(mult) {
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
        if let Some(q) = self.queues.get_mut(queue_index as usize) {
            q.pending_notify = true;
        }
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

            // Bound the amount of guest-driven work processed per poll so a corrupted/malicious
            // driver can't force us into a long (potentially 65k-iteration) drain loop by
            // bumping `avail.idx` far ahead of `next_avail`.
            let max_chains = queue.size() as usize;
            for _ in 0..max_chains {
                let popped = match queue.pop_descriptor_chain(mem) {
                    Ok(Some(popped)) => popped,
                    Ok(None) => break,
                    Err(_) => break,
                };

                match popped {
                    PoppedDescriptorChain::Chain(chain) => {
                        let head_index = chain.head_index();
                        need_irq |= match self.device.process_queue(queue_index, chain, queue, mem)
                        {
                            Ok(irq) => irq,
                            Err(_) => {
                                // VirtioDevice implementations are expected to add a used entry for
                                // every descriptor chain they pop. Historically, some devices returned
                                // an error for malformed chains without completing them; because the
                                // transport ignores device errors, that behaviour wedges the virtqueue
                                // (the driver waits forever for used->idx to advance).
                                //
                                // As a safety net, complete the chain with `used.len = 0` on any device
                                // error so the guest can recover and continue issuing requests.
                                queue.add_used(mem, head_index, 0).unwrap_or(false)
                            }
                        };
                    }
                    PoppedDescriptorChain::Invalid { head_index, .. } => {
                        // The guest posted an avail entry, but we could not parse the descriptor
                        // chain (e.g. loop, out-of-range index, invalid indirect table). Complete
                        // the chain with `used.len = 0` so the driver can recover instead of
                        // wedging the queue.
                        need_irq |= queue.add_used(mem, head_index, 0).unwrap_or(false);
                    }
                }
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
        } else {
            self.legacy_irq_pending = true;
            self.sync_legacy_irq_line();
        }
    }

    fn sync_legacy_irq_line(&mut self) {
        let should_assert = self.legacy_irq_pending && !self.intx_disabled();
        if should_assert == self.legacy_irq_line {
            return;
        }
        if should_assert {
            self.interrupts.raise_legacy_irq();
        } else {
            self.interrupts.lower_legacy_irq();
        }
        self.legacy_irq_line = should_assert;
    }

    pub fn debug_queue_used_idx(&self, mem: &dyn GuestMemory, queue: u16) -> Option<u16> {
        let q = self.queues.get(queue as usize)?;
        let used_addr = q.used_addr;
        let used_idx_addr = used_addr.checked_add(2)?;
        read_u16_le(mem, used_idx_addr).ok()
    }

    /// Debug helper returning the device-side virtqueue progress counters for the given queue.
    ///
    /// This is primarily intended for snapshot/restore tests to ensure restores do not reprocess
    /// previously-consumed avail ring entries.
    pub fn debug_queue_progress(&self, queue: u16) -> Option<(u16, u16, bool)> {
        let q = self.queues.get(queue as usize)?;
        let vq = q.queue.as_ref()?;
        Some((vq.next_avail(), vq.next_used(), vq.event_idx()))
    }

    /// Rewind a queue's device-side `next_avail` index to its current `next_used`.
    ///
    /// Some virtio devices (notably virtio-snd's `eventq`) can pop available descriptor chains and
    /// cache them internally without producing used entries. Those cached chains are runtime-only
    /// and are not currently serialized in snapshot state. After restoring the virtio-pci transport
    /// snapshot, callers can use this helper to "replay" any such in-flight avail entries by
    /// rewinding `next_avail` back to `next_used` so the transport will re-pop them on the next
    /// poll.
    ///
    /// This is a best-effort operation: if the queue is not configured/enabled, it is a no-op.
    pub fn rewind_queue_next_avail_to_next_used(&mut self, queue: u16) {
        let Some(q) = self.queues.get_mut(queue as usize) else {
            return;
        };
        let Some(vq) = q.queue.as_mut() else {
            return;
        };

        let next_used = vq.next_used();
        let event_idx = vq.event_idx();
        vq.restore_progress(next_used, next_used, event_idx);
        // Mark as pending so integrations that only service queues on notifications will still
        // re-process the rewritten entries without requiring an explicit guest kick.
        q.pending_notify = true;
    }

    fn snapshot_pci_state(&self) -> SnapshotPciConfigSpaceState {
        let state = self.config.snapshot_state();
        SnapshotPciConfigSpaceState {
            bytes: state.bytes,
            bar_base: state.bar_base,
            bar_probe: state.bar_probe,
        }
    }

    fn snapshot_transport_state(&self) -> SnapshotVirtioPciTransportState {
        let mut queues = Vec::with_capacity(self.queues.len());

        for q in &self.queues {
            let progress = if let Some(vq) = q.queue.as_ref() {
                SnapshotVirtQueueProgressState {
                    next_avail: vq.next_avail(),
                    next_used: vq.next_used(),
                    event_idx: vq.event_idx(),
                }
            } else {
                SnapshotVirtQueueProgressState {
                    next_avail: 0,
                    next_used: 0,
                    event_idx: self.negotiated_event_idx(),
                }
            };

            queues.push(SnapshotVirtioPciQueueState {
                desc_addr: q.desc_addr,
                avail_addr: q.avail_addr,
                used_addr: q.used_addr,
                enable: q.enable,
                msix_vector: q.msix_vector,
                notify_off: q.notify_off,
                progress,
            });
        }

        SnapshotVirtioPciTransportState {
            device_status: self.device_status,
            negotiated_features: self.negotiated_features,
            device_feature_select: self.device_feature_select,
            driver_feature_select: self.driver_feature_select,
            driver_features: self.driver_features,
            msix_config_vector: self.msix_config_vector,
            queue_select: self.queue_select,
            isr_status: self.isr_status,
            // Store the internal legacy INTx latch (not gated by `PCI COMMAND.INTX_DISABLE`).
            //
            // This ensures a pending interrupt is not lost across snapshot/restore if the guest
            // had temporarily disabled INTx delivery.
            legacy_intx_level: self.legacy_irq_pending,
            queues,
        }
    }

    fn restore_pci_state(&mut self, state: &SnapshotPciConfigSpaceState) {
        let state = aero_devices::pci::PciConfigSpaceState {
            bytes: state.bytes,
            bar_base: state.bar_base,
            bar_probe: state.bar_probe,
        };
        self.config.restore_state(&state);
    }

    fn restore_transport_state(&mut self, state: &SnapshotVirtioPciTransportState) {
        self.device_status = state.device_status;
        self.negotiated_features = state.negotiated_features;
        self.device_feature_select = state.device_feature_select;
        self.driver_feature_select = state.driver_feature_select;
        self.driver_features = state.driver_features;
        self.msix_config_vector = state.msix_config_vector;
        self.queue_select = state.queue_select;

        // Restore virtio device feature state.
        self.device.set_features(self.negotiated_features);
        self.features_negotiated = (self.device_status & VIRTIO_STATUS_FEATURES_OK) != 0;

        // Restore queues.
        let apply = state.queues.len().min(self.queues.len());
        for i in 0..apply {
            let saved = &state.queues[i];
            let q = &mut self.queues[i];

            q.desc_addr = saved.desc_addr;
            q.avail_addr = saved.avail_addr;
            q.used_addr = saved.used_addr;
            q.msix_vector = saved.msix_vector;
            q.notify_off = saved.notify_off;

            q.enable = saved.enable;
            if q.enable {
                q.queue = VirtQueue::new(
                    VirtQueueConfig {
                        size: q.size,
                        desc_addr: q.desc_addr,
                        avail_addr: q.avail_addr,
                        used_addr: q.used_addr,
                    },
                    saved.progress.event_idx,
                )
                .ok()
                .map(|mut vq| {
                    vq.restore_progress(
                        saved.progress.next_avail,
                        saved.progress.next_used,
                        saved.progress.event_idx,
                    );
                    vq
                });
            } else {
                q.queue = None;
            }
        }

        // Restore interrupt state deterministically.
        self.isr_status = state.isr_status;
        self.legacy_irq_pending = state.legacy_intx_level;
        self.sync_legacy_irq_line();

        // Snapshot schema is modern-only. If the guest had started driver initialization, ensure we
        // continue to reject legacy register accesses after restore.
        self.transport_mode = if self.device_status == 0 {
            TransportMode::Unknown
        } else {
            TransportMode::Modern
        };
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
        if desired == TransportMode::Legacy
            && offset == VIRTIO_PCI_LEGACY_STATUS
            && data.first().copied().unwrap_or(0) == 0
        {
            return true;
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
        self.legacy_irq_pending = false;
        self.sync_legacy_irq_line();
        isr
    }
}

impl IoSnapshot for VirtioPciDevice {
    const DEVICE_ID: [u8; 4] = *b"VPCI";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 0);

    fn save_state(&self) -> Vec<u8> {
        const TAG_PCI_CONFIG: u16 = 1;
        const TAG_TRANSPORT: u16 = 2;
        const TAG_VIRTIO_DEVICE_TYPE: u16 = 3;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u16(TAG_VIRTIO_DEVICE_TYPE, self.device.device_type());
        w.field_bytes(TAG_PCI_CONFIG, self.snapshot_pci_state().encode());
        w.field_bytes(TAG_TRANSPORT, self.snapshot_transport_state().encode());

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_PCI_CONFIG: u16 = 1;
        const TAG_TRANSPORT: u16 = 2;
        const TAG_VIRTIO_DEVICE_TYPE: u16 = 3;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        if let Some(v) = r.u16(TAG_VIRTIO_DEVICE_TYPE)? {
            if v != self.device.device_type() {
                return Err(SnapshotError::InvalidFieldEncoding(
                    "virtio device type mismatch",
                ));
            }
        }

        if let Some(buf) = r.bytes(TAG_PCI_CONFIG) {
            let pci = SnapshotPciConfigSpaceState::decode(buf)?;
            self.restore_pci_state(&pci);
        }

        let Some(buf) = r.bytes(TAG_TRANSPORT) else {
            return Err(SnapshotError::InvalidFieldEncoding(
                "missing virtio transport state",
            ));
        };
        let transport = SnapshotVirtioPciTransportState::decode(buf)?;
        self.restore_transport_state(&transport);

        Ok(())
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

impl aero_devices::pci::PciDevice for VirtioPciDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        // Preserve BAR programming but reset the virtio transport + device state back to its
        // power-on baseline.
        self.reset_transport();
        self.config.set_command(0);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::devices::{VirtioDevice, VirtioDeviceError};
    use crate::memory::{read_u16_le, write_u16_le, write_u32_le, write_u64_le, GuestRam};
    use crate::queue::VirtQueue;

    #[derive(Default)]
    struct NoopInterrupts;

    impl InterruptSink for NoopInterrupts {
        fn raise_legacy_irq(&mut self) {}
        fn lower_legacy_irq(&mut self) {}
        fn signal_msix(&mut self, _vector: u16) {}
    }

    struct CountingDevice {
        calls: usize,
        max_calls: usize,
    }

    impl CountingDevice {
        fn new(max_calls: usize) -> Self {
            Self {
                calls: 0,
                max_calls,
            }
        }
    }

    impl VirtioDevice for CountingDevice {
        fn device_type(&self) -> u16 {
            0
        }

        fn device_features(&self) -> u64 {
            0
        }

        fn set_features(&mut self, _features: u64) {}

        fn num_queues(&self) -> u16 {
            1
        }

        fn queue_max_size(&self, _queue: u16) -> u16 {
            8
        }

        fn process_queue(
            &mut self,
            _queue_index: u16,
            chain: crate::queue::DescriptorChain,
            queue: &mut VirtQueue,
            mem: &mut dyn crate::memory::GuestMemory,
        ) -> Result<bool, VirtioDeviceError> {
            self.calls += 1;
            assert!(
                self.calls <= self.max_calls,
                "VirtioPciDevice should not process more than the queue size worth of descriptor chains per poll"
            );
            queue
                .add_used(mem, chain.head_index(), 0)
                .map_err(|_| VirtioDeviceError::IoError)
        }

        fn read_config(&self, _offset: u64, data: &mut [u8]) {
            data.fill(0);
        }

        fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

        fn reset(&mut self) {
            self.calls = 0;
        }

        fn as_any(&self) -> &dyn core::any::Any {
            self
        }

        fn as_any_mut(&mut self) -> &mut dyn core::any::Any {
            self
        }
    }

    fn write_desc(
        mem: &mut GuestRam,
        table: u64,
        index: u16,
        addr: u64,
        len: u32,
        flags: u16,
        next: u16,
    ) {
        let base = table + u64::from(index) * 16;
        write_u64_le(mem, base, addr).unwrap();
        write_u32_le(mem, base + 8, len).unwrap();
        write_u16_le(mem, base + 12, flags).unwrap();
        write_u16_le(mem, base + 14, next).unwrap();
    }

    #[test]
    fn poll_limits_descriptor_chain_processing_to_queue_size() {
        let mut mem = GuestRam::new(0x3000);

        let mut pci = VirtioPciDevice::new_legacy_only(
            Box::new(CountingDevice::new(8)),
            Box::new(NoopInterrupts::default()),
        );

        // Enable queue 0 using a legacy PFN at 0x1000.
        pci.legacy_io_write(VIRTIO_PCI_LEGACY_QUEUE_SEL, &0u16.to_le_bytes());
        pci.legacy_io_write(VIRTIO_PCI_LEGACY_QUEUE_PFN, &1u32.to_le_bytes());

        let (desc, avail, used) = legacy_vring_addresses(1, 8);

        // Descriptor 0: a trivially-valid one-element chain.
        write_desc(&mut mem, desc, 0, 0, 0, 0, 0);

        // avail.flags = 0 (interrupts enabled), avail.idx far ahead to simulate a corrupted/malicious driver.
        write_u16_le(&mut mem, avail, 0).unwrap();
        write_u16_le(&mut mem, avail + 2, u16::MAX).unwrap();
        for i in 0..8u64 {
            write_u16_le(&mut mem, avail + 4 + i * 2, 0).unwrap();
        }

        write_u16_le(&mut mem, used, 0).unwrap();
        write_u16_le(&mut mem, used + 2, 0).unwrap();

        // Without the poll budget, this would try to drain ~65k descriptor chains.
        pci.poll(&mut mem);

        let calls = pci
            .device_mut::<CountingDevice>()
            .expect("device downcast")
            .calls;
        assert_eq!(calls, 8);

        // We should have produced 8 used entries.
        assert_eq!(read_u16_le(&mem, used + 2).unwrap(), 8);
    }
}
