use std::time::{Duration, Instant};

use memory::MemoryBus;

use crate::devices::aerogpu_regs::{
    irq_bits, mmio, ring_control, AeroGpuRegs, AEROGPU_MMIO_MAGIC, AEROGPU_PCI_BAR0_SIZE_BYTES,
    AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_PROG_IF,
    AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_SUBSYSTEM_ID, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
    AEROGPU_PCI_VENDOR_ID, SUPPORTED_FEATURES,
};
use crate::devices::aerogpu_ring::{AeroGpuRingHeader, RING_TAIL_OFFSET};
use crate::devices::aerogpu_scanout::AeroGpuFormat;
use crate::gpu_worker::aerogpu_executor::{AeroGpuExecutor, AeroGpuExecutorConfig};
use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};

#[derive(Clone, Debug)]
pub struct AeroGpuDeviceConfig {
    pub executor: AeroGpuExecutorConfig,
    pub vblank_hz: Option<u32>,
}

impl Default for AeroGpuDeviceConfig {
    fn default() -> Self {
        Self {
            executor: AeroGpuExecutorConfig::default(),
            vblank_hz: Some(60),
        }
    }
}

pub struct AeroGpuPciDevice {
    config: PciConfigSpace,
    pub bar0: u32,
    bar0_probe: bool,

    pub regs: AeroGpuRegs,
    executor: AeroGpuExecutor,
    irq_level: bool,

    vblank_interval: Option<Duration>,
    next_vblank: Option<Instant>,
}

impl AeroGpuPciDevice {
    pub fn new(cfg: AeroGpuDeviceConfig, bar0: u32) -> Self {
        let mut config_space = PciConfigSpace::new();

        config_space.set_u16(0x00, AEROGPU_PCI_VENDOR_ID);
        config_space.set_u16(0x02, AEROGPU_PCI_DEVICE_ID);

        config_space.set_u16(0x2c, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID);
        config_space.set_u16(0x2e, AEROGPU_PCI_SUBSYSTEM_ID);

        config_space.write(0x09, 1, AEROGPU_PCI_PROG_IF as u32);
        config_space.write(0x0a, 1, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE as u32);
        config_space.write(0x0b, 1, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER as u32);

        // BAR0 (MMIO regs), non-prefetchable 32-bit.
        config_space.set_u32(0x10, bar0 & 0xffff_fff0);

        // Interrupt pin INTA#.
        config_space.write(0x3d, 1, 1);

        let vblank_interval = cfg.vblank_hz.and_then(|hz| {
            if hz == 0 {
                None
            } else {
                Some(Duration::from_nanos(1_000_000_000u64 / hz as u64))
            }
        });

        Self {
            config: config_space,
            bar0,
            bar0_probe: false,
            regs: AeroGpuRegs::default(),
            executor: AeroGpuExecutor::new(cfg.executor),
            irq_level: false,
            vblank_interval,
            next_vblank: None,
        }
    }

    pub fn irq_level(&self) -> bool {
        self.irq_level
    }

    pub fn bar0_size(&self) -> u64 {
        AEROGPU_PCI_BAR0_SIZE_BYTES
    }

    pub fn tick(&mut self, now: Instant) {
        let Some(interval) = self.vblank_interval else {
            return;
        };
        if !self.regs.scanout0.enable {
            return;
        }
        let mut next = self.next_vblank.unwrap_or(now + interval);
        if now < next {
            self.next_vblank = Some(next);
            return;
        }
        let mut fired = 0u32;
        while now >= next && fired < 4 {
            self.regs.irq_status |= irq_bits::SCANOUT_VBLANK;
            self.update_irq_level();
            next += interval;
            fired += 1;
        }
        self.next_vblank = Some(next);
    }

    pub fn read_scanout0_rgba(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u8>> {
        self.regs.scanout0.read_rgba(mem)
    }

    fn update_irq_level(&mut self) {
        self.irq_level = (self.regs.irq_status & self.regs.irq_enable) != 0;
    }

    fn reset_ring(&mut self, mem: &mut dyn MemoryBus) {
        if self.regs.ring_gpa != 0 {
            let tail = mem.read_u32(self.regs.ring_gpa + RING_TAIL_OFFSET);
            AeroGpuRingHeader::write_head(mem, self.regs.ring_gpa, tail);
        }
        self.regs.completed_fence = 0;
        self.regs.irq_status = 0;
        self.update_irq_level();
    }

    fn mmio_read_dword(&self, offset: u64) -> u32 {
        match offset {
            mmio::MAGIC => AEROGPU_MMIO_MAGIC,
            mmio::ABI_VERSION => self.regs.abi_version,
            mmio::FEATURES_LO => (SUPPORTED_FEATURES & 0xffff_ffff) as u32,
            mmio::FEATURES_HI => (SUPPORTED_FEATURES >> 32) as u32,

            mmio::RING_GPA_LO => self.regs.ring_gpa as u32,
            mmio::RING_GPA_HI => (self.regs.ring_gpa >> 32) as u32,
            mmio::RING_SIZE_BYTES => self.regs.ring_size_bytes,
            mmio::RING_CONTROL => self.regs.ring_control,

            mmio::FENCE_GPA_LO => self.regs.fence_gpa as u32,
            mmio::FENCE_GPA_HI => (self.regs.fence_gpa >> 32) as u32,

            mmio::COMPLETED_FENCE_LO => self.regs.completed_fence as u32,
            mmio::COMPLETED_FENCE_HI => (self.regs.completed_fence >> 32) as u32,

            mmio::IRQ_STATUS => self.regs.irq_status,
            mmio::IRQ_ENABLE => self.regs.irq_enable,

            mmio::SCANOUT0_ENABLE => self.regs.scanout0.enable as u32,
            mmio::SCANOUT0_WIDTH => self.regs.scanout0.width,
            mmio::SCANOUT0_HEIGHT => self.regs.scanout0.height,
            mmio::SCANOUT0_FORMAT => self.regs.scanout0.format as u32,
            mmio::SCANOUT0_PITCH_BYTES => self.regs.scanout0.pitch_bytes,
            mmio::SCANOUT0_FB_GPA_LO => self.regs.scanout0.fb_gpa as u32,
            mmio::SCANOUT0_FB_GPA_HI => (self.regs.scanout0.fb_gpa >> 32) as u32,

            // Cursor registers are feature-gated; we keep them as inert storage.
            mmio::CURSOR_ENABLE => self.regs.cursor.enable as u32,
            mmio::CURSOR_X => self.regs.cursor.x as u32,
            mmio::CURSOR_Y => self.regs.cursor.y as u32,
            mmio::CURSOR_HOT_X => self.regs.cursor.hot_x,
            mmio::CURSOR_HOT_Y => self.regs.cursor.hot_y,
            mmio::CURSOR_WIDTH => self.regs.cursor.width,
            mmio::CURSOR_HEIGHT => self.regs.cursor.height,
            mmio::CURSOR_FORMAT => self.regs.cursor.format as u32,
            mmio::CURSOR_FB_GPA_LO => self.regs.cursor.fb_gpa as u32,
            mmio::CURSOR_FB_GPA_HI => (self.regs.cursor.fb_gpa >> 32) as u32,
            mmio::CURSOR_PITCH_BYTES => self.regs.cursor.pitch_bytes,

            _ => 0,
        }
    }

    fn mmio_write_dword(&mut self, mem: &mut dyn MemoryBus, offset: u64, value: u32) {
        match offset {
            mmio::RING_GPA_LO => {
                self.regs.ring_gpa =
                    (self.regs.ring_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::RING_GPA_HI => {
                self.regs.ring_gpa =
                    (self.regs.ring_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            mmio::RING_SIZE_BYTES => {
                self.regs.ring_size_bytes = value;
            }
            mmio::RING_CONTROL => {
                if value & ring_control::RESET != 0 {
                    self.reset_ring(mem);
                }
                self.regs.ring_control = value & ring_control::ENABLE;
            }
            mmio::FENCE_GPA_LO => {
                self.regs.fence_gpa =
                    (self.regs.fence_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::FENCE_GPA_HI => {
                self.regs.fence_gpa =
                    (self.regs.fence_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            mmio::DOORBELL => {
                self.executor.process_doorbell(&mut self.regs, mem);
                self.update_irq_level();
            }
            mmio::IRQ_ENABLE => {
                self.regs.irq_enable = value;
                self.update_irq_level();
            }
            mmio::IRQ_ACK => {
                self.regs.irq_status &= !value;
                self.update_irq_level();
            }

            mmio::SCANOUT0_ENABLE => {
                self.regs.scanout0.enable = value != 0;
            }
            mmio::SCANOUT0_WIDTH => {
                self.regs.scanout0.width = value;
            }
            mmio::SCANOUT0_HEIGHT => {
                self.regs.scanout0.height = value;
            }
            mmio::SCANOUT0_FORMAT => {
                self.regs.scanout0.format = AeroGpuFormat::from_u32(value);
            }
            mmio::SCANOUT0_PITCH_BYTES => {
                self.regs.scanout0.pitch_bytes = value;
            }
            mmio::SCANOUT0_FB_GPA_LO => {
                self.regs.scanout0.fb_gpa =
                    (self.regs.scanout0.fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::SCANOUT0_FB_GPA_HI => {
                self.regs.scanout0.fb_gpa =
                    (self.regs.scanout0.fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }

            mmio::CURSOR_ENABLE => self.regs.cursor.enable = value != 0,
            mmio::CURSOR_X => self.regs.cursor.x = value as i32,
            mmio::CURSOR_Y => self.regs.cursor.y = value as i32,
            mmio::CURSOR_HOT_X => self.regs.cursor.hot_x = value,
            mmio::CURSOR_HOT_Y => self.regs.cursor.hot_y = value,
            mmio::CURSOR_WIDTH => self.regs.cursor.width = value,
            mmio::CURSOR_HEIGHT => self.regs.cursor.height = value,
            mmio::CURSOR_FORMAT => self.regs.cursor.format = AeroGpuFormat::from_u32(value),
            mmio::CURSOR_FB_GPA_LO => {
                self.regs.cursor.fb_gpa =
                    (self.regs.cursor.fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::CURSOR_FB_GPA_HI => {
                self.regs.cursor.fb_gpa =
                    (self.regs.cursor.fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            mmio::CURSOR_PITCH_BYTES => self.regs.cursor.pitch_bytes = value,

            // Ignore writes to read-only / unknown registers.
            _ => {}
        }
    }
}

impl PciDevice for AeroGpuPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if offset == 0x10 && size == 4 && self.bar0_probe {
            return (!(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1)) & 0xffff_fff0;
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x10 && size == 4 {
            if value == 0xffff_ffff {
                self.bar0_probe = true;
            } else {
                self.bar0_probe = false;
                self.bar0 = value & 0xffff_fff0;
            }
        }
        self.config.write(offset, size, value);
    }
}

impl MmioDevice for AeroGpuPciDevice {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        if offset >= AEROGPU_PCI_BAR0_SIZE_BYTES {
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
        if offset >= AEROGPU_PCI_BAR0_SIZE_BYTES {
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
