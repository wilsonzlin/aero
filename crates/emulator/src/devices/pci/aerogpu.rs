use std::time::{Duration, Instant};

use memory::MemoryBus;

use crate::devices::aerogpu_regs::{
    irq_bits, mmio, ring_control, AeroGpuRegs, AEROGPU_MMIO_MAGIC, AEROGPU_PCI_BAR0_SIZE_BYTES,
    AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID, AEROGPU_PCI_PROG_IF,
    AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_SUBSYSTEM_ID, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID,
    AEROGPU_PCI_VENDOR_ID, FEATURE_VBLANK,
};
use crate::devices::aerogpu_ring::{write_fence_page, AeroGpuRingHeader, RING_TAIL_OFFSET};
use crate::devices::aerogpu_scanout::AeroGpuFormat;
use crate::gpu_worker::aerogpu_backend::AeroGpuCommandBackend;
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

    boot_time: Instant,
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
        let bar0 = bar0 & !(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
        config_space.set_u32(0x10, bar0);

        // Interrupt pin INTA#.
        config_space.write(0x3d, 1, 1);

        let vblank_interval = cfg.vblank_hz.and_then(|hz| {
            if hz == 0 {
                return None;
            }
            // Use ceil division to keep 60 Hz at 16_666_667 ns (rather than truncating to 16_666_666).
            let period_ns = 1_000_000_000u64.div_ceil(hz as u64);
            Some(Duration::from_nanos(period_ns))
        });

        let mut regs = AeroGpuRegs::default();
        if let Some(interval) = vblank_interval {
            regs.scanout0_vblank_period_ns = interval.as_nanos().min(u32::MAX as u128) as u32;
        } else {
            // If vblank is disabled by configuration, also clear the advertised feature bit so
            // guests don't wait on a vblank that will never arrive.
            regs.features &= !FEATURE_VBLANK;
        }

        Self {
            config: config_space,
            bar0,
            bar0_probe: false,
            regs,
            executor: AeroGpuExecutor::new(cfg.executor),
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
        AEROGPU_PCI_BAR0_SIZE_BYTES
    }

    pub fn tick(&mut self, mem: &mut dyn MemoryBus, now: Instant) {
        let dma_enabled = self.bus_master_enabled();

        // Polling completions and flushing fences may write guest memory (fence page / writeback).
        // When PCI bus mastering is disabled (COMMAND.BME=0), the device must not perform DMA.
        if dma_enabled {
            self.executor.poll_backend_completions(&mut self.regs, mem);
        }
        // `tick` has early-return paths (no vblank yet); update IRQ after polling completions.
        self.update_irq_level();

        // If vblank pacing is disabled (by config or by disabling the scanout), do not allow any
        // vsync-delayed fences to remain queued forever.
        if dma_enabled && (self.vblank_interval.is_none() || !self.regs.scanout0.enable) {
            self.executor.flush_pending_fences(&mut self.regs, mem);
            self.update_irq_level();
        }

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

        let mut ticks = 0u32;
        while now >= next {
            // Counters advance even if vblank IRQ delivery is masked.
            self.regs.scanout0_vblank_seq = self.regs.scanout0_vblank_seq.wrapping_add(1);
            let t_ns = next.saturating_duration_since(self.boot_time).as_nanos();
            self.regs.scanout0_vblank_time_ns = t_ns.min(u64::MAX as u128) as u64;

            // Only latch the vblank IRQ status bit while the guest has it enabled.
            // This prevents an immediate "stale" interrupt on re-enable.
            if (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) != 0 {
                self.regs.irq_status |= irq_bits::SCANOUT_VBLANK;
            }

            if dma_enabled {
                self.executor.process_vblank_tick(&mut self.regs, mem);
            }

            next += interval;
            ticks += 1;

            // Avoid unbounded catch-up work if the host stalls for a very long time.
            if ticks >= 1024 {
                next = now + interval;
                break;
            }
        }
        self.next_vblank = Some(next);
        self.update_irq_level();
    }

    pub fn read_scanout0_rgba(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u8>> {
        if !self.bus_master_enabled() {
            return None;
        }
        self.regs.scanout0.read_rgba(mem)
    }

    pub fn read_cursor_rgba(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u8>> {
        if !self.bus_master_enabled() {
            return None;
        }
        self.regs.cursor.read_rgba(mem)
    }

    pub fn complete_fence(&mut self, mem: &mut dyn MemoryBus, fence: u64) {
        if !self.bus_master_enabled() {
            return;
        }
        self.executor.complete_fence(&mut self.regs, mem, fence);
        self.update_irq_level();
    }

    pub fn set_backend(&mut self, backend: Box<dyn AeroGpuCommandBackend>) {
        self.executor.set_backend(backend);
    }

    pub fn read_presented_scanout_rgba8(&mut self, scanout_id: u32) -> Option<(u32, u32, Vec<u8>)> {
        let scanout = self.executor.read_presented_scanout_rgba8(scanout_id)?;
        Some((scanout.width, scanout.height, scanout.rgba8))
    }

    fn update_irq_level(&mut self) {
        self.irq_level = (self.regs.irq_status & self.regs.irq_enable) != 0;
    }

    fn reset_ring(&mut self, mem: &mut dyn MemoryBus) {
        let dma_enabled = self.bus_master_enabled();
        if dma_enabled && self.regs.ring_gpa != 0 {
            let tail = mem.read_u32(self.regs.ring_gpa + RING_TAIL_OFFSET);
            AeroGpuRingHeader::write_head(mem, self.regs.ring_gpa, tail);
        }
        self.executor.reset();
        self.regs.completed_fence = 0;
        if dma_enabled && self.regs.fence_gpa != 0 {
            write_fence_page(
                mem,
                self.regs.fence_gpa,
                self.regs.abi_version,
                self.regs.completed_fence,
            );
        }
        self.regs.irq_status = 0;
        self.update_irq_level();
    }

    fn mmio_read_dword(&self, offset: u64) -> u32 {
        match offset {
            mmio::MAGIC => AEROGPU_MMIO_MAGIC,
            mmio::ABI_VERSION => self.regs.abi_version,
            mmio::FEATURES_LO => (self.regs.features & 0xffff_ffff) as u32,
            mmio::FEATURES_HI => (self.regs.features >> 32) as u32,

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

            mmio::SCANOUT0_VBLANK_SEQ_LO => self.regs.scanout0_vblank_seq as u32,
            mmio::SCANOUT0_VBLANK_SEQ_HI => (self.regs.scanout0_vblank_seq >> 32) as u32,
            mmio::SCANOUT0_VBLANK_TIME_NS_LO => self.regs.scanout0_vblank_time_ns as u32,
            mmio::SCANOUT0_VBLANK_TIME_NS_HI => (self.regs.scanout0_vblank_time_ns >> 32) as u32,
            mmio::SCANOUT0_VBLANK_PERIOD_NS => self.regs.scanout0_vblank_period_ns,

            // Cursor registers are implemented as simple storage; presentation is handled by the caller.
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
                // Keep the vblank clock caught up to real time before accepting new work. Without
                // this, a vsynced PRESENT submitted just after a vblank deadline (but before the
                // next `tick()` call) could complete on that already-elapsed vblank edge.
                self.tick(mem, Instant::now());
                if self.bus_master_enabled() {
                    self.executor.process_doorbell(&mut self.regs, mem);
                    self.update_irq_level();
                }
            }
            mmio::IRQ_ENABLE => {
                // Keep the vblank clock caught up before enabling vblank delivery. Without this,
                // a vblank IRQ can "arrive" immediately on enable due to catch-up ticks, breaking
                // `D3DKMTWaitForVerticalBlankEvent` pacing (it must wait for the *next* vblank).
                let enabling_vblank = (value & irq_bits::SCANOUT_VBLANK) != 0
                    && (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) == 0;
                if enabling_vblank {
                    self.tick(mem, Instant::now());
                }

                self.regs.irq_enable = value;
                if (value & irq_bits::FENCE) == 0 {
                    self.regs.irq_status &= !irq_bits::FENCE;
                }
                if (value & irq_bits::SCANOUT_VBLANK) == 0 {
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                }
                self.update_irq_level();
            }
            mmio::IRQ_ACK => {
                self.regs.irq_status &= !value;
                self.update_irq_level();
            }

            mmio::SCANOUT0_ENABLE => {
                let new_enable = value != 0;
                if self.regs.scanout0.enable && !new_enable {
                    // When scanout is disabled, stop vblank scheduling and drop any pending vblank IRQ.
                    self.next_vblank = None;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                    if self.bus_master_enabled() {
                        self.executor.flush_pending_fences(&mut self.regs, mem);
                        self.update_irq_level();
                    }
                }
                self.regs.scanout0.enable = new_enable;
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
        if offset == 0x10 && size == 4 {
            return if self.bar0_probe {
                (!(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1)) & 0xffff_fff0
            } else {
                self.bar0
            };
        }
        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if offset == 0x10 && size == 4 {
            let addr_mask = !(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
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

impl MmioDevice for AeroGpuPciDevice {
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
        // Gate MMIO decode on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }
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
