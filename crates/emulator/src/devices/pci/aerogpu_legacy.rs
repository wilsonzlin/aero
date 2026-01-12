//! Legacy AeroGPU PCI/MMIO device model ("ARGP").
//!
//! The repository currently contains two guest↔host ABI generations:
//! - Versioned ABI ("AGPU"): `drivers/aerogpu/protocol/aerogpu_pci.h` +
//!   `drivers/aerogpu/protocol/aerogpu_ring.h` + `drivers/aerogpu/protocol/aerogpu_cmd.h`,
//!   implemented by `crates/emulator/src/devices/pci/aerogpu.rs`.
//! - Legacy bring-up ABI ("ARGP"): `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`,
//!   implemented by this module.
//!
//! The Win7 KMD in `drivers/aerogpu/kmd` supports both ABIs and auto-detects the
//! active one based on the BAR0 MMIO magic. This legacy device model is kept for
//! compatibility/bring-up and is gated behind the `emulator` crate feature
//! `aerogpu-legacy`.
//!
//! The versioned ("AGPU") device model is the primary execution path; new work
//! should generally target the versioned ABI.

use memory::MemoryBus;
use std::time::{Duration, Instant};

use aero_protocol::aerogpu::aerogpu_pci as protocol_pci;

use crate::devices::aerogpu_scanout::{AeroGpuFormat, AeroGpuScanoutConfig};
use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};

// Constants mirrored from `drivers/aerogpu/protocol/legacy/aerogpu_protocol_legacy.h`.
const AEROGPU_LEGACY_PCI_VENDOR_ID: u16 = u16::from_be_bytes([0x1A, 0xED]);
const AEROGPU_LEGACY_PCI_DEVICE_ID: u16 = 0x0001;

const AEROGPU_LEGACY_MMIO_MAGIC: u32 = 0x4152_4750; // 'A''R''G''P'
const AEROGPU_LEGACY_MMIO_VERSION: u32 = 0x0001_0000;

const AEROGPU_LEGACY_BAR0_SIZE_BYTES: u64 = 64 * 1024;

mod mmio {
    // Identification.
    pub const MAGIC: u64 = 0x0000;
    pub const VERSION: u64 = 0x0004;

    // Feature bits (mirrors `drivers/aerogpu/protocol/aerogpu_pci.h`).
    pub const FEATURES_LO: u64 = 0x0008;
    pub const FEATURES_HI: u64 = 0x000c;

    // Ring setup.
    pub const RING_BASE_LO: u64 = 0x0010;
    pub const RING_BASE_HI: u64 = 0x0014;
    pub const RING_ENTRY_COUNT: u64 = 0x0018;
    pub const RING_HEAD: u64 = 0x001c;
    pub const RING_TAIL: u64 = 0x0020;
    pub const RING_DOORBELL: u64 = 0x0024;

    // Interrupt + fence.
    pub const INT_STATUS: u64 = 0x0030;
    pub const INT_ACK: u64 = 0x0034;
    pub const FENCE_COMPLETED: u64 = 0x0038;

    // Scanout.
    pub const SCANOUT_FB_LO: u64 = 0x0100;
    pub const SCANOUT_FB_HI: u64 = 0x0104;
    pub const SCANOUT_PITCH: u64 = 0x0108;
    pub const SCANOUT_WIDTH: u64 = 0x010c;
    pub const SCANOUT_HEIGHT: u64 = 0x0110;
    pub const SCANOUT_FORMAT: u64 = 0x0114;
    pub const SCANOUT_ENABLE: u64 = 0x0118;

    // Newer interrupt + vblank timing block (mirrors `drivers/aerogpu/protocol/aerogpu_pci.h`).
    pub const IRQ_STATUS: u64 = 0x0300;
    pub const IRQ_ENABLE: u64 = 0x0304;
    pub const IRQ_ACK: u64 = 0x0308;

    pub const SCANOUT0_VBLANK_SEQ_LO: u64 = 0x0420;
    pub const SCANOUT0_VBLANK_SEQ_HI: u64 = 0x0424;
    pub const SCANOUT0_VBLANK_TIME_NS_LO: u64 = 0x0428;
    pub const SCANOUT0_VBLANK_TIME_NS_HI: u64 = 0x042c;
    pub const SCANOUT0_VBLANK_PERIOD_NS: u64 = 0x0430;
}

mod int_bits {
    pub const FENCE: u32 = 0x0000_0001;
}

mod irq_bits {
    pub const FENCE: u32 = 1 << 0;
    pub const SCANOUT_VBLANK: u32 = 1 << 1;
}

mod ring_entry_type {
    pub const SUBMIT: u32 = 1;
}

mod scanout_format {
    // `enum aerogpu_scanout_format` (legacy): only one format currently defined.
    pub const X8R8G8B8: u32 = 1;
}

// Feature bits (mirrors `drivers/aerogpu/protocol/aerogpu_pci.h`).
const FEATURE_VBLANK: u64 = 1u64 << 3;

const LEGACY_RING_ENTRY_STRIDE_BYTES: u64 = 24;
const LEGACY_SUBMISSION_HEADER_SIZE_BYTES: u32 = 32;

#[derive(Clone, Debug, Default)]
pub struct AeroGpuLegacyStats {
    pub doorbells: u64,
    pub submissions: u64,
    pub malformed_submissions: u64,
}

#[derive(Clone, Debug)]
pub struct AeroGpuLegacyRegs {
    pub ring_base_gpa: u64,
    pub ring_entry_count: u32,
    pub ring_head: u32,
    pub ring_tail: u32,

    pub int_status: u32,
    pub fence_completed: u32,

    pub scanout: AeroGpuScanoutConfig,

    // Newer interrupt + vblank timing block (mirrors `drivers/aerogpu/protocol/aerogpu_pci.h`).
    pub features: u64,
    pub irq_status: u32,
    pub irq_enable: u32,
    pub scanout0_vblank_seq: u64,
    pub scanout0_vblank_time_ns: u64,
    pub scanout0_vblank_period_ns: u32,

    pub stats: AeroGpuLegacyStats,
}

impl Default for AeroGpuLegacyRegs {
    fn default() -> Self {
        Self {
            ring_base_gpa: 0,
            ring_entry_count: 0,
            ring_head: 0,
            ring_tail: 0,
            int_status: 0,
            fence_completed: 0,
            scanout: AeroGpuScanoutConfig::default(),
            features: FEATURE_VBLANK,
            irq_status: 0,
            irq_enable: 0,
            scanout0_vblank_seq: 0,
            scanout0_vblank_time_ns: 0,
            scanout0_vblank_period_ns: 0,
            stats: AeroGpuLegacyStats::default(),
        }
    }
}

#[derive(Clone, Debug)]
pub struct AeroGpuLegacyDeviceConfig {
    pub vendor_id: u16,
    pub device_id: u16,
    pub vblank_hz: Option<u32>,
}

impl Default for AeroGpuLegacyDeviceConfig {
    fn default() -> Self {
        Self {
            vendor_id: AEROGPU_LEGACY_PCI_VENDOR_ID,
            device_id: AEROGPU_LEGACY_PCI_DEVICE_ID,
            vblank_hz: Some(60),
        }
    }
}

pub struct AeroGpuLegacyPciDevice {
    config: PciConfigSpace,
    pub bar0: u32,
    bar0_probe: bool,

    pub regs: AeroGpuLegacyRegs,
    irq_level: bool,

    boot_time: Instant,
    vblank_interval: Option<Duration>,
    next_vblank: Option<Instant>,
}

impl AeroGpuLegacyPciDevice {
    pub fn new(cfg: AeroGpuLegacyDeviceConfig, bar0: u32) -> Self {
        let mut config_space = PciConfigSpace::new();

        config_space.set_u16(0x00, cfg.vendor_id);
        config_space.set_u16(0x02, cfg.device_id);

        // Class code: display controller (0x03) / VGA-compatible (0x00).
        config_space.write(0x09, 1, protocol_pci::AEROGPU_PCI_PROG_IF as u32);
        config_space.write(
            0x0a,
            1,
            protocol_pci::AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE as u32,
        );
        config_space.write(
            0x0b,
            1,
            protocol_pci::AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER as u32,
        );

        // BAR0 (MMIO regs), non-prefetchable 32-bit.
        let bar0 = bar0 & !(AEROGPU_LEGACY_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
        config_space.set_u32(0x10, bar0);

        // Interrupt pin INTA#.
        config_space.write(0x3d, 1, 1);

        let vblank_interval = cfg.vblank_hz.and_then(|hz| {
            if hz == 0 {
                return None;
            }
            // Use ceil division to keep 60 Hz at 16_666_667 ns (rather than truncating to 16_666_666).
            let period_ns = 1_000_000_000u64.div_ceil(u64::from(hz));
            Some(Duration::from_nanos(period_ns))
        });

        let mut regs = AeroGpuLegacyRegs::default();
        if let Some(interval) = vblank_interval {
            regs.scanout0_vblank_period_ns = interval.as_nanos().min(u32::MAX as u128) as u32;
        } else {
            regs.features &= !FEATURE_VBLANK;
        }

        Self {
            config: config_space,
            bar0,
            bar0_probe: false,
            regs,
            irq_level: false,
            boot_time: Instant::now(),
            vblank_interval,
            next_vblank: None,
        }
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn mem_space_enabled(&self) -> bool {
        (self.command() & (1 << 1)) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.command() & (1 << 2)) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.command() & (1 << 10)) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.irq_level
    }

    pub fn bar0_size(&self) -> u64 {
        AEROGPU_LEGACY_BAR0_SIZE_BYTES
    }

    pub fn read_scanout_rgba(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u8>> {
        if !self.bus_master_enabled() {
            return None;
        }
        self.regs.scanout.read_rgba(mem)
    }

    fn update_irq_level(&mut self) {
        self.irq_level =
            self.regs.int_status != 0 || (self.regs.irq_status & self.regs.irq_enable) != 0;
    }

    pub fn tick(&mut self, now: Instant) {
        let Some(interval) = self.vblank_interval else {
            return;
        };
        if !self.regs.scanout.enable {
            return;
        }
        let mut next = self.next_vblank.unwrap_or(now + interval);
        if now < next {
            self.next_vblank = Some(next);
            return;
        }

        let mut ticks = 0u32;
        while now >= next {
            self.regs.scanout0_vblank_seq = self.regs.scanout0_vblank_seq.wrapping_add(1);
            let t_ns = next.saturating_duration_since(self.boot_time).as_nanos();
            self.regs.scanout0_vblank_time_ns = t_ns.min(u64::MAX as u128) as u64;

            // Only latch the vblank IRQ status bit while the guest has it enabled.
            if (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) != 0 {
                self.regs.irq_status |= irq_bits::SCANOUT_VBLANK;
            }

            next += interval;
            ticks += 1;
            if ticks >= 1024 {
                next = now + interval;
                break;
            }
        }

        self.next_vblank = Some(next);
        self.update_irq_level();
    }

    fn map_scanout_format(value: u32) -> AeroGpuFormat {
        match value {
            scanout_format::X8R8G8B8 => AeroGpuFormat::B8G8R8X8Unorm,
            _ => AeroGpuFormat::Invalid,
        }
    }

    fn process_doorbell(&mut self, mem: &mut dyn MemoryBus) {
        self.regs.stats.doorbells = self.regs.stats.doorbells.saturating_add(1);

        let entry_count = self.regs.ring_entry_count;
        if self.regs.ring_base_gpa == 0 || entry_count == 0 {
            self.regs.stats.malformed_submissions =
                self.regs.stats.malformed_submissions.saturating_add(1);
            return;
        }

        let mut head = self.regs.ring_head % entry_count;
        let tail = self.regs.ring_tail % entry_count;
        let mut processed = 0u32;
        let mut fence_advanced = false;

        while head != tail && processed < entry_count {
            let entry_gpa =
                self.regs.ring_base_gpa + u64::from(head) * LEGACY_RING_ENTRY_STRIDE_BYTES;
            let ty = mem.read_u32(entry_gpa);
            if ty != ring_entry_type::SUBMIT {
                self.regs.stats.malformed_submissions =
                    self.regs.stats.malformed_submissions.saturating_add(1);
                head = (head + 1) % entry_count;
                processed += 1;
                continue;
            }

            let fence = mem.read_u32(entry_gpa + 8);
            let desc_size = mem.read_u32(entry_gpa + 12);
            let desc_gpa = mem.read_u64(entry_gpa + 16);

            self.regs.stats.submissions = self.regs.stats.submissions.saturating_add(1);

            if desc_gpa == 0 || desc_size < LEGACY_SUBMISSION_HEADER_SIZE_BYTES {
                self.regs.stats.malformed_submissions =
                    self.regs.stats.malformed_submissions.saturating_add(1);
            } else {
                let version = mem.read_u32(desc_gpa);
                if version != 1 {
                    self.regs.stats.malformed_submissions =
                        self.regs.stats.malformed_submissions.saturating_add(1);
                }
            }

            if fence > self.regs.fence_completed {
                self.regs.fence_completed = fence;
                fence_advanced = true;
            }

            head = (head + 1) % entry_count;
            processed += 1;
        }

        self.regs.ring_head = head;

        if fence_advanced {
            self.regs.int_status |= int_bits::FENCE;
            self.regs.irq_status |= irq_bits::FENCE;
        }

        self.update_irq_level();
    }

    fn mmio_read_dword(&self, offset: u64) -> u32 {
        match offset {
            mmio::MAGIC => AEROGPU_LEGACY_MMIO_MAGIC,
            mmio::VERSION => AEROGPU_LEGACY_MMIO_VERSION,
            mmio::FEATURES_LO => (self.regs.features & 0xffff_ffff) as u32,
            mmio::FEATURES_HI => (self.regs.features >> 32) as u32,

            mmio::RING_BASE_LO => self.regs.ring_base_gpa as u32,
            mmio::RING_BASE_HI => (self.regs.ring_base_gpa >> 32) as u32,
            mmio::RING_ENTRY_COUNT => self.regs.ring_entry_count,
            mmio::RING_HEAD => self.regs.ring_head,
            mmio::RING_TAIL => self.regs.ring_tail,

            mmio::INT_STATUS => self.regs.int_status,
            mmio::FENCE_COMPLETED => self.regs.fence_completed,

            mmio::SCANOUT_FB_LO => self.regs.scanout.fb_gpa as u32,
            mmio::SCANOUT_FB_HI => (self.regs.scanout.fb_gpa >> 32) as u32,
            mmio::SCANOUT_PITCH => self.regs.scanout.pitch_bytes,
            mmio::SCANOUT_WIDTH => self.regs.scanout.width,
            mmio::SCANOUT_HEIGHT => self.regs.scanout.height,
            mmio::SCANOUT_FORMAT => match self.regs.scanout.format {
                // Only one legacy value is defined.
                AeroGpuFormat::B8G8R8X8Unorm => scanout_format::X8R8G8B8,
                _ => 0,
            },
            mmio::SCANOUT_ENABLE => self.regs.scanout.enable as u32,

            mmio::IRQ_STATUS => self.regs.irq_status,
            mmio::IRQ_ENABLE => self.regs.irq_enable,

            mmio::SCANOUT0_VBLANK_SEQ_LO => self.regs.scanout0_vblank_seq as u32,
            mmio::SCANOUT0_VBLANK_SEQ_HI => (self.regs.scanout0_vblank_seq >> 32) as u32,
            mmio::SCANOUT0_VBLANK_TIME_NS_LO => self.regs.scanout0_vblank_time_ns as u32,
            mmio::SCANOUT0_VBLANK_TIME_NS_HI => (self.regs.scanout0_vblank_time_ns >> 32) as u32,
            mmio::SCANOUT0_VBLANK_PERIOD_NS => self.regs.scanout0_vblank_period_ns,

            _ => 0,
        }
    }

    fn mmio_write_dword(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        match offset {
            mmio::RING_BASE_LO => {
                self.regs.ring_base_gpa =
                    (self.regs.ring_base_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::RING_BASE_HI => {
                self.regs.ring_base_gpa =
                    (self.regs.ring_base_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            mmio::RING_ENTRY_COUNT => {
                self.regs.ring_entry_count = value;
                self.regs.ring_head = 0;
                self.regs.ring_tail = 0;
            }
            mmio::RING_HEAD => {
                // Driver writes this during reset paths; accept it.
                self.regs.ring_head = value;
            }
            mmio::RING_TAIL => {
                self.regs.ring_tail = value;
            }
            mmio::RING_DOORBELL => {
                if self.bus_master_enabled() {
                    self.process_doorbell(mem)
                }
            }

            mmio::INT_ACK => {
                self.regs.int_status &= !value;
                if (value & int_bits::FENCE) != 0 {
                    self.regs.irq_status &= !irq_bits::FENCE;
                }
                self.update_irq_level();
            }

            mmio::SCANOUT_FB_LO => {
                self.regs.scanout.fb_gpa =
                    (self.regs.scanout.fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::SCANOUT_FB_HI => {
                self.regs.scanout.fb_gpa =
                    (self.regs.scanout.fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            mmio::SCANOUT_PITCH => self.regs.scanout.pitch_bytes = value,
            mmio::SCANOUT_WIDTH => self.regs.scanout.width = value,
            mmio::SCANOUT_HEIGHT => self.regs.scanout.height = value,
            mmio::SCANOUT_FORMAT => self.regs.scanout.format = Self::map_scanout_format(value),
            mmio::SCANOUT_ENABLE => {
                let new_enable = value != 0;
                if self.regs.scanout.enable && !new_enable {
                    self.next_vblank = None;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                    self.update_irq_level();
                }
                self.regs.scanout.enable = new_enable;
            }

            mmio::IRQ_ENABLE => {
                self.regs.irq_enable = value;
                if (value & irq_bits::SCANOUT_VBLANK) == 0 {
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                }
                self.update_irq_level();
            }
            mmio::IRQ_ACK => {
                self.regs.irq_status &= !value;
                self.update_irq_level();
            }

            // Ignore writes to read-only / unknown registers.
            _ => {}
        }
    }
}

impl PciDevice for AeroGpuLegacyPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x10 && size == 4 {
            return if self.bar0_probe {
                (!(AEROGPU_LEGACY_BAR0_SIZE_BYTES as u32 - 1)) & 0xffff_fff0
            } else {
                self.bar0
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x10 && size == 4 {
            let addr_mask = !(AEROGPU_LEGACY_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
            if value == 0xffff_ffff {
                self.bar0_probe = true;
                self.bar0 = 0;
                self.config.write(offset, size, 0);
            } else {
                self.bar0_probe = false;
                self.bar0 = value & addr_mask;
                self.config.write(offset, size, self.bar0);
            }
            return;
        }
        self.config.write(offset, size, value);
    }
}

impl MmioDevice for AeroGpuLegacyPciDevice {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        // Gate MMIO decode on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return match size {
                1 => 0xff,
                2 => 0xffff,
                4 => u32::MAX,
                _ => u32::MAX,
            };
        }
        if offset >= AEROGPU_LEGACY_BAR0_SIZE_BYTES {
            return 0;
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let value = self.mmio_read_dword(aligned);

        match size {
            1 => (value >> shift) & 0xff,
            2 => (value >> shift) & 0xffff,
            4 => value,
            _ => 0,
        }
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        // Gate MMIO decode on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }
        if offset >= AEROGPU_LEGACY_BAR0_SIZE_BYTES {
            return;
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;

        let value32 = match size {
            1 => (value & 0xff) << shift,
            2 => (value & 0xffff) << shift,
            4 => value,
            _ => return,
        };

        let merged = if size == 4 {
            value32
        } else {
            let cur = self.mmio_read(mem, aligned, 4);
            let mask = match size {
                1 => 0xffu32 << shift,
                2 => 0xffffu32 << shift,
                _ => 0,
            };
            (cur & !mask) | value32
        };

        self.mmio_write_dword(mem, aligned, merged);
    }
}
