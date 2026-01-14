use memory::MemoryBus;

use crate::executor::{AeroGpuExecutor, AeroGpuExecutorConfig};
use crate::regs::{irq_bits, mmio, AeroGpuRegs, FEATURE_VBLANK};
use crate::scanout::AeroGpuFormat;
use crate::vblank::{period_ns_from_hz, period_ns_to_reg};

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
    boot_time_ns: Option<u64>,
    vblank_period_ns: Option<u64>,
    next_vblank_deadline_ns: Option<u64>,

    /// Pending LO dword for `SCANOUT0_FB_GPA` while waiting for the HI write commit.
    scanout0_fb_gpa_pending_lo: u32,
    /// Whether the guest has written `SCANOUT0_FB_GPA_LO` without a subsequent HI write.
    ///
    /// This avoids exposing a torn 64-bit framebuffer address to scanout readers; drivers
    /// typically write LO then HI.
    scanout0_fb_gpa_lo_pending: bool,

    /// Pending LO dword for `CURSOR_FB_GPA` while waiting for the HI write commit.
    cursor_fb_gpa_pending_lo: u32,
    /// Whether the guest has written `CURSOR_FB_GPA_LO` without a subsequent HI write.
    ///
    /// This avoids exposing a torn 64-bit cursor framebuffer address; drivers typically write LO
    /// then HI.
    cursor_fb_gpa_lo_pending: bool,
}

impl AeroGpuBar0MmioDevice {
    pub fn new(cfg: AeroGpuBar0MmioDeviceConfig) -> Self {
        let vblank_period_ns = period_ns_from_hz(cfg.vblank_hz);

        let mut regs = AeroGpuRegs::default();
        if let Some(period_ns) = vblank_period_ns {
            regs.scanout0_vblank_period_ns = period_ns_to_reg(period_ns);
        } else {
            // If vblank is disabled by configuration, also clear the advertised feature bit so
            // guests don't wait on a vblank that will never arrive.
            regs.features &= !FEATURE_VBLANK;
        }

        Self {
            regs,
            executor: AeroGpuExecutor::new(cfg.executor),
            boot_time_ns: None,
            vblank_period_ns,
            next_vblank_deadline_ns: None,
            scanout0_fb_gpa_pending_lo: 0,
            scanout0_fb_gpa_lo_pending: false,
            cursor_fb_gpa_pending_lo: 0,
            cursor_fb_gpa_lo_pending: false,
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
    pub fn tick(&mut self, mem: &mut dyn MemoryBus, now_ns: u64, dma_enabled: bool) {
        // Establish a monotonic boot time reference from the externally supplied `now_ns` timebase.
        self.boot_time_ns.get_or_insert(now_ns);

        // Polling completions and flushing fences may write guest memory (fence page / writeback).
        // When DMA is disabled by the platform, the device must not perform DMA.
        if dma_enabled {
            self.executor.poll_backend_completions(&mut self.regs, mem);
        }

        let Some(period_ns) = self.vblank_period_ns else {
            return;
        };

        if (self.regs.features & FEATURE_VBLANK) == 0 {
            return;
        }

        // Vblank ticks are gated on scanout enable. When scanout is disabled, stop scheduling and
        // clear any pending vblank IRQ status bit.
        if !self.regs.scanout0.enable {
            self.next_vblank_deadline_ns = None;
            self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
            return;
        }

        let mut next = self
            .next_vblank_deadline_ns
            .unwrap_or(now_ns.saturating_add(period_ns));
        if now_ns < next {
            self.next_vblank_deadline_ns = Some(next);
            return;
        }

        let boot = self.boot_time_ns.unwrap_or(0);
        let mut ticks = 0u32;
        while now_ns >= next {
            // Counters advance even if vblank IRQ delivery is masked.
            self.regs.scanout0_vblank_seq = self.regs.scanout0_vblank_seq.wrapping_add(1);
            self.regs.scanout0_vblank_time_ns = next.saturating_sub(boot);

            // Only latch the vblank IRQ status bit while the guest has it enabled.
            // This prevents an immediate "stale" interrupt on re-enable.
            if (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) != 0 {
                self.regs.irq_status |= irq_bits::SCANOUT_VBLANK;
            }

            if dma_enabled {
                self.executor.process_vblank_tick(&mut self.regs, mem);
            }

            next = next.saturating_add(period_ns);
            ticks += 1;

            // Avoid unbounded catch-up work if the host stalls for a very long time.
            if ticks >= 1024 {
                next = now_ns.saturating_add(period_ns);
                break;
            }
        }

        self.next_vblank_deadline_ns = Some(next);
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
            mmio::SCANOUT0_FB_GPA_LO => {
                if self.scanout0_fb_gpa_lo_pending {
                    self.scanout0_fb_gpa_pending_lo
                } else {
                    self.regs.scanout0.fb_gpa as u32
                }
            }
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
            mmio::CURSOR_FB_GPA_LO => {
                if self.cursor_fb_gpa_lo_pending {
                    self.cursor_fb_gpa_pending_lo
                } else {
                    self.regs.cursor.fb_gpa as u32
                }
            }
            mmio::CURSOR_FB_GPA_HI => (self.regs.cursor.fb_gpa >> 32) as u32,
            mmio::CURSOR_PITCH_BYTES => self.regs.cursor.pitch_bytes,

            _ => 0,
        }
    }

    pub fn mmio_write_dword(
        &mut self,
        mem: &mut dyn MemoryBus,
        now_ns: u64,
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
                    self.tick(mem, now_ns, dma_enabled);
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
                    self.next_vblank_deadline_ns = None;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                    if dma_enabled {
                        self.executor.flush_pending_fences(&mut self.regs, mem);
                    }
                }
                if !new_enable {
                    self.scanout0_fb_gpa_lo_pending = false;
                }
                self.regs.scanout0.enable = new_enable;
            }
            mmio::SCANOUT0_WIDTH => self.regs.scanout0.width = value,
            mmio::SCANOUT0_HEIGHT => self.regs.scanout0.height = value,
            mmio::SCANOUT0_FORMAT => self.regs.scanout0.format = AeroGpuFormat::from_u32(value),
            mmio::SCANOUT0_PITCH_BYTES => self.regs.scanout0.pitch_bytes = value,
            mmio::SCANOUT0_FB_GPA_LO => {
                // Avoid exposing torn 64-bit addresses; treat the HI write as the commit point.
                self.scanout0_fb_gpa_pending_lo = value;
                self.scanout0_fb_gpa_lo_pending = true;
            }
            mmio::SCANOUT0_FB_GPA_HI => {
                let lo = if self.scanout0_fb_gpa_lo_pending {
                    self.scanout0_fb_gpa_pending_lo
                } else {
                    self.regs.scanout0.fb_gpa as u32
                };
                self.regs.scanout0.fb_gpa = (u64::from(value) << 32) | u64::from(lo);
                self.scanout0_fb_gpa_lo_pending = false;
            }

            mmio::CURSOR_ENABLE => {
                let prev_enable = self.regs.cursor.enable;
                let new_enable = value != 0;
                self.regs.cursor.enable = new_enable;
                if prev_enable && !new_enable {
                    // Reset torn-update tracking so a stale LO write can't affect future updates.
                    self.cursor_fb_gpa_pending_lo = 0;
                    self.cursor_fb_gpa_lo_pending = false;
                }
            }
            mmio::CURSOR_X => self.regs.cursor.x = value as i32,
            mmio::CURSOR_Y => self.regs.cursor.y = value as i32,
            mmio::CURSOR_HOT_X => self.regs.cursor.hot_x = value,
            mmio::CURSOR_HOT_Y => self.regs.cursor.hot_y = value,
            mmio::CURSOR_WIDTH => self.regs.cursor.width = value,
            mmio::CURSOR_HEIGHT => self.regs.cursor.height = value,
            mmio::CURSOR_FORMAT => self.regs.cursor.format = AeroGpuFormat::from_u32(value),
            mmio::CURSOR_FB_GPA_LO => {
                // Avoid exposing torn 64-bit cursor base updates; treat the HI write as the commit point.
                self.cursor_fb_gpa_pending_lo = value;
                self.cursor_fb_gpa_lo_pending = true;
            }
            mmio::CURSOR_FB_GPA_HI => {
                let lo = if self.cursor_fb_gpa_lo_pending {
                    self.cursor_fb_gpa_pending_lo
                } else {
                    self.regs.cursor.fb_gpa as u32
                };
                self.regs.cursor.fb_gpa = (u64::from(value) << 32) | u64::from(lo);
                self.cursor_fb_gpa_lo_pending = false;
            }
            mmio::CURSOR_PITCH_BYTES => self.regs.cursor.pitch_bytes = value,

            _ => {
                let _ = (mem, now_ns, dma_enabled);
                // Ignore writes to unimplemented registers; this BAR0 model is focused on vblank +
                // scanout primitives.
            }
        }
    }
}
