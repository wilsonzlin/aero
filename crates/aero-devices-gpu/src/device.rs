use std::time::{Duration, Instant};

use memory::MemoryBus;

use crate::executor::{AeroGpuExecutor, AeroGpuExecutorConfig};
use crate::regs::{irq_bits, mmio, AeroGpuRegs, FEATURE_VBLANK};
use crate::scanout::AeroGpuFormat;

#[derive(Clone, Debug)]
pub struct AeroGpuBar0MmioDeviceConfig {
    pub executor: AeroGpuExecutorConfig,
    pub vblank_hz: Option<u32>,
}

impl Default for AeroGpuBar0MmioDeviceConfig {
    fn default() -> Self {
        Self {
            executor: AeroGpuExecutorConfig::default(),
            vblank_hz: Some(60),
        }
    }
}

/// Canonical AeroGPU BAR0/MMIO device model core.
///
/// This is the "registers + vblank clock + ring executor" state machine, without any PCI config
/// space wrapper. Platform-specific PCI/MMIO frontends are expected to:
///
/// - gate MMIO decode on PCI COMMAND.MEM,
/// - gate DMA (ring processing, fence page writes) on PCI COMMAND.BME, and
/// - wire INTx/MSI delivery based on [`AeroGpuBar0MmioDevice::irq_level`].
///
/// Vblank semantics are documented in:
/// - `drivers/aerogpu/protocol/vblank.md`, and
/// - `docs/graphics/win7-vblank-present-requirements.md`.
pub struct AeroGpuBar0MmioDevice {
    pub regs: AeroGpuRegs,
    executor: AeroGpuExecutor,
    boot_time: Instant,
    vblank_interval: Option<Duration>,
    next_vblank: Option<Instant>,
}

impl AeroGpuBar0MmioDevice {
    pub fn new(cfg: AeroGpuBar0MmioDeviceConfig) -> Self {
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
            regs,
            executor: AeroGpuExecutor::new(cfg.executor),
            boot_time: Instant::now(),
            vblank_interval,
            next_vblank: None,
        }
    }

    pub fn irq_level(&self) -> bool {
        (self.regs.irq_status & self.regs.irq_enable) != 0
    }

    pub fn set_backend(&mut self, backend: Box<dyn crate::backend::AeroGpuCommandBackend>) {
        self.executor.set_backend(backend);
    }

    /// Advance the device model by one time quantum.
    ///
    /// `dma_enabled` should reflect whether the platform is willing to allow this device to
    /// perform DMA (e.g. PCI COMMAND.BME in a PCI wrapper).
    pub fn tick(&mut self, mem: &mut dyn MemoryBus, now: Instant, dma_enabled: bool) {
        // Polling completions and flushing fences may write guest memory (fence page / writeback).
        // When DMA is disabled by the platform, the device must not perform DMA.
        if dma_enabled {
            self.executor.poll_backend_completions(&mut self.regs, mem);
        }

        let Some(interval) = self.vblank_interval else {
            return;
        };

        if (self.regs.features & FEATURE_VBLANK) == 0 {
            return;
        }

        // Vblank ticks are gated on scanout enable. When scanout is disabled, stop scheduling and
        // clear any pending vblank IRQ status bit.
        if !self.regs.scanout0.enable {
            self.next_vblank = None;
            self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
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
    }

    pub fn mmio_read_dword(&self, offset: u64) -> u32 {
        match offset {
            mmio::MAGIC => crate::regs::AEROGPU_MMIO_MAGIC,
            mmio::ABI_VERSION => self.regs.abi_version,
            mmio::FEATURES_LO => (self.regs.features & 0xffff_ffff) as u32,
            mmio::FEATURES_HI => (self.regs.features >> 32) as u32,

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

            _ => 0,
        }
    }

    pub fn mmio_write_dword(
        &mut self,
        mem: &mut dyn MemoryBus,
        now: Instant,
        dma_enabled: bool,
        offset: u64,
        value: u32,
    ) {
        match offset {
            mmio::IRQ_ENABLE => {
                // Keep the vblank clock caught up before enabling vblank delivery. Without this,
                // a vblank IRQ can "arrive" immediately on enable due to catch-up ticks, breaking
                // `D3DKMTWaitForVerticalBlankEvent` pacing (it must wait for the *next* vblank).
                let enabling_vblank = (value & irq_bits::SCANOUT_VBLANK) != 0
                    && (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) == 0;
                if enabling_vblank {
                    self.tick(mem, now, dma_enabled);
                }

                self.regs.irq_enable = value;
                // Clear any IRQ status bits that are now masked so re-enabling doesn't immediately
                // deliver a stale interrupt.
                if (value & irq_bits::SCANOUT_VBLANK) == 0 {
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                }
                if (value & irq_bits::FENCE) == 0 {
                    self.regs.irq_status &= !irq_bits::FENCE;
                }
            }
            mmio::IRQ_ACK => {
                self.regs.irq_status &= !value;
            }

            mmio::SCANOUT0_ENABLE => {
                let new_enable = value != 0;
                if self.regs.scanout0.enable && !new_enable {
                    // When scanout is disabled, stop vblank scheduling and drop any pending vblank IRQ.
                    self.next_vblank = None;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                    if dma_enabled {
                        self.executor.flush_pending_fences(&mut self.regs, mem);
                    }
                }
                self.regs.scanout0.enable = new_enable;
            }
            mmio::SCANOUT0_WIDTH => self.regs.scanout0.width = value,
            mmio::SCANOUT0_HEIGHT => self.regs.scanout0.height = value,
            mmio::SCANOUT0_FORMAT => self.regs.scanout0.format = AeroGpuFormat::from_u32(value),
            mmio::SCANOUT0_PITCH_BYTES => self.regs.scanout0.pitch_bytes = value,
            mmio::SCANOUT0_FB_GPA_LO => {
                self.regs.scanout0.fb_gpa =
                    (self.regs.scanout0.fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            mmio::SCANOUT0_FB_GPA_HI => {
                self.regs.scanout0.fb_gpa =
                    (self.regs.scanout0.fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }

            _ => {
                let _ = (mem, now, dma_enabled);
                // Ignore writes to unimplemented registers; this BAR0 model is focused on vblank +
                // scanout primitives.
            }
        }
    }
}
