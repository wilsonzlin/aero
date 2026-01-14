//! PCI device glue for the AeroGPU device model.

use std::cell::RefCell;
use std::rc::Rc;

use aero_devices::pci::{profile, PciConfigSpace, PciDevice};
use memory::{MemoryBus, MmioHandler};

use crate::backend::{AeroGpuBackendSubmission, AeroGpuCommandBackend};
use crate::executor::{AeroGpuExecutor, AeroGpuExecutorConfig};
use crate::regs::{irq_bits, mmio, ring_control, AeroGpuRegs, AEROGPU_MMIO_MAGIC, FEATURE_VBLANK};
use crate::ring::{write_fence_page, AeroGpuRingHeader, RING_TAIL_OFFSET};
use crate::scanout::AeroGpuFormat;

const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER_ENABLE: u16 = 1 << 2;
const PCI_COMMAND_INTX_DISABLE: u16 = 1 << 10;

/// Size of the legacy VGA window (`0xA0000..0xC0000`).
///
/// This reflects the guest-visible alias aperture size (128KiB). The canonical VRAM layout keeps a
/// larger (256KiB) reserved region at the start of BAR1 for the 4×64KiB VGA planar backing store;
/// see [`VBE_LFB_OFFSET`].
pub const LEGACY_VGA_VRAM_BYTES: u64 = 0x20_000;

/// Offset within BAR1/VRAM where the VBE linear framebuffer (LFB) region begins.
///
/// The canonical AeroGPU VRAM layout reserves the first 256KiB for legacy VGA plane backing (4 ×
/// 64KiB). VBE packed-pixel framebuffer writes are mapped after this region so VBE mode changes do
/// not clobber VGA plane contents.
///
/// See `docs/16-aerogpu-vga-vesa-compat.md`.
pub const VBE_LFB_OFFSET: u64 = 0x40_000;

/// Start physical address of the legacy VGA window.
pub const LEGACY_VGA_PADDR_BASE: u64 = 0xA_0000;

/// End physical address (exclusive) of the legacy VGA window.
pub const LEGACY_VGA_PADDR_END: u64 = 0xC_0000;

/// PCI BAR index for the AeroGPU MMIO registers window (BAR0).
pub const AEROGPU_PCI_BAR0_INDEX: u8 = profile::AEROGPU_BAR0_INDEX;

/// PCI BAR index for the AeroGPU VRAM aperture (BAR1).
pub const AEROGPU_PCI_BAR1_INDEX: u8 = profile::AEROGPU_BAR1_VRAM_INDEX;

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

/// AeroGPU exposed as a PCI device (config space + BAR0 MMIO + INTx + BAR1 VRAM).
pub struct AeroGpuPciDevice {
    config: PciConfigSpace,
    vram: Rc<RefCell<Vec<u8>>>,

    pub regs: AeroGpuRegs,
    executor: AeroGpuExecutor,

    irq_level: bool,

    vblank_period_ns: Option<u64>,
    next_vblank_deadline_ns: Option<u64>,
    boot_time_ns: Option<u64>,
    vblank_irq_enable_pending: bool,

    doorbell_pending: bool,
    ring_reset_pending_dma: bool,
}

impl Default for AeroGpuPciDevice {
    fn default() -> Self {
        Self::new(AeroGpuDeviceConfig::default())
    }
}

impl AeroGpuPciDevice {
    pub fn new(cfg: AeroGpuDeviceConfig) -> Self {
        let mut config = profile::AEROGPU.build_config_space();

        let bar1_size = config
            .bar_range(AEROGPU_PCI_BAR1_INDEX)
            .map(|r| r.size)
            .unwrap_or(0);
        let vram = Rc::new(RefCell::new(vec![0u8; bar1_size as usize]));

        let vblank_period_ns = cfg.vblank_hz.and_then(|hz| {
            if hz == 0 {
                return None;
            }
            Some(1_000_000_000u64.div_ceil(hz as u64))
        });

        let mut regs = AeroGpuRegs::default();
        if let Some(period_ns) = vblank_period_ns {
            regs.scanout0_vblank_period_ns = period_ns.min(u64::from(u32::MAX)) as u32;
        } else {
            // If vblank is disabled by configuration, also clear the advertised feature bit so
            // guests don't wait on a vblank that will never arrive.
            regs.features &= !FEATURE_VBLANK;
        }

        // Preserve profile BAR programming but disable decoding until the guest enables it.
        config.set_command(0);

        Self {
            config,
            vram,
            regs,
            executor: AeroGpuExecutor::new(cfg.executor),
            irq_level: false,
            vblank_period_ns,
            next_vblank_deadline_ns: None,
            boot_time_ns: None,
            vblank_irq_enable_pending: false,
            doorbell_pending: false,
            ring_reset_pending_dma: false,
        }
    }

    /// Returns a handle to the VRAM vector (shared with BAR1 MMIO handlers).
    pub fn vram_shared(&self) -> Rc<RefCell<Vec<u8>>> {
        Rc::clone(&self.vram)
    }

    /// Returns an [`MmioHandler`] implementing the BAR1 VRAM aperture.
    pub fn bar1_mmio_handler(&self) -> AeroGpuBar1VramMmio {
        AeroGpuBar1VramMmio {
            vram: Rc::clone(&self.vram),
        }
    }

    /// Translate a physical address in the legacy VGA window (`0xA0000..0xC0000`) into a VRAM
    /// offset starting at 0.
    pub fn legacy_vga_paddr_to_vram_offset(paddr: u64) -> Option<u64> {
        if !(LEGACY_VGA_PADDR_BASE..LEGACY_VGA_PADDR_END).contains(&paddr) {
            return None;
        }
        Some(paddr - LEGACY_VGA_PADDR_BASE)
    }

    /// Translate a physical address in the VBE linear framebuffer region into a VRAM offset.
    ///
    /// The VBE LFB is expected to live at `bar1_base + VBE_LFB_OFFSET`.
    pub fn vbe_lfb_paddr_to_vram_offset(bar1_base: u64, paddr: u64) -> Option<u64> {
        let lfb_base = bar1_base.checked_add(VBE_LFB_OFFSET)?;
        if paddr < lfb_base {
            return None;
        }
        let off = paddr.checked_sub(bar1_base)?;
        let end = bar1_base.checked_add(Self::bar1_size_bytes()?)?;
        if paddr >= end {
            return None;
        }
        Some(off)
    }

    fn bar1_size_bytes() -> Option<u64> {
        // Keep this as a helper so callers don't need to plumb the config space just to validate
        // offsets. `profile::AEROGPU` defines BAR1; `new()` allocates VRAM accordingly.
        profile::AEROGPU
            .bars
            .iter()
            .find(|bar| bar.index == AEROGPU_PCI_BAR1_INDEX)
            .map(|bar| bar.size)
    }

    pub fn set_backend(&mut self, backend: Box<dyn AeroGpuCommandBackend>) {
        self.executor.set_backend(backend);
    }

    /// Drain newly-decoded AeroGPU submissions queued since the last call.
    ///
    /// This is intended for WASM/browser integrations where command execution happens out-of-process
    /// (e.g. via `aero-gpu-wasm`). The device model (ring processing, fence page updates, IRQ state)
    /// runs in-process, but the host is responsible for executing each returned submission and then
    /// calling [`AeroGpuPciDevice::complete_fence`].
    pub fn drain_pending_submissions(&mut self) -> Vec<AeroGpuBackendSubmission> {
        self.executor.drain_pending_submissions()
    }

    fn mem_space_enabled(&self) -> bool {
        (self.config.command() & PCI_COMMAND_MEM_ENABLE) != 0
    }

    fn bus_master_enabled(&self) -> bool {
        (self.config.command() & PCI_COMMAND_BUS_MASTER_ENABLE) != 0
    }

    fn intx_disabled(&self) -> bool {
        (self.config.command() & PCI_COMMAND_INTX_DISABLE) != 0
    }

    pub fn irq_level(&self) -> bool {
        if self.intx_disabled() {
            return false;
        }
        self.irq_level
    }

    pub fn tick(&mut self, mem: &mut dyn MemoryBus, now_ns: u64) {
        self.boot_time_ns.get_or_insert(now_ns);

        let dma_enabled = self.bus_master_enabled();
        let suppress_vblank_irq = self.vblank_irq_enable_pending;

        if dma_enabled {
            self.executor.poll_backend_completions(&mut self.regs, mem);
        }
        // `tick` has early-return paths (no vblank yet); update IRQ after polling completions.
        self.update_irq_level();

        // Complete any pending ring reset DMA work (head update + fence page) if bus mastering is
        // enabled.
        if self.ring_reset_pending_dma {
            if dma_enabled {
                self.reset_ring_dma(mem);
            }
            self.ring_reset_pending_dma = false;
        }

        // If vblank pacing is disabled (by config or by disabling the scanout), do not allow any
        // vsync-delayed fences to remain queued forever.
        if dma_enabled && (self.vblank_period_ns.is_none() || !self.regs.scanout0.enable) {
            self.executor.flush_pending_fences(&mut self.regs, mem);
            self.update_irq_level();
        }

        // Keep the vblank clock caught up to real time before accepting new doorbells.
        // Without this, a vsynced PRESENT submitted just after a vblank deadline (but before the
        // tick that advances the vblank clock) could complete on that already-elapsed vblank edge.
        if let Some(period_ns) = self.vblank_period_ns {
            if self.regs.scanout0.enable {
                let mut next = self
                    .next_vblank_deadline_ns
                    .unwrap_or(now_ns.saturating_add(period_ns));
                if now_ns >= next {
                    let boot = self.boot_time_ns.unwrap_or(0);
                    let mut ticks = 0u32;
                    while now_ns >= next {
                        // Counters advance even if vblank IRQ delivery is masked.
                        self.regs.scanout0_vblank_seq =
                            self.regs.scanout0_vblank_seq.wrapping_add(1);
                        self.regs.scanout0_vblank_time_ns = next.saturating_sub(boot);

                        // Only latch the vblank IRQ status bit while the guest has it enabled.
                        // This prevents an immediate "stale" interrupt on re-enable.
                        if !suppress_vblank_irq
                            && (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) != 0
                        {
                            self.regs.irq_status |= irq_bits::SCANOUT_VBLANK;
                        }

                        if dma_enabled {
                            self.executor.process_vblank_tick(&mut self.regs, mem);
                            if suppress_vblank_irq {
                                self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                            }
                        }

                        next = next.saturating_add(period_ns);
                        ticks += 1;
                        if ticks >= 1024 {
                            next = now_ns.saturating_add(period_ns);
                            break;
                        }
                    }
                    self.next_vblank_deadline_ns = Some(next);
                    self.update_irq_level();
                } else {
                    self.next_vblank_deadline_ns = Some(next);
                }
            }
        }

        // Process any pending doorbell writes (which may DMA to guest memory).
        if self.doorbell_pending {
            if dma_enabled {
                self.executor.process_doorbell(&mut self.regs, mem);
                self.update_irq_level();
                // Doorbells are edge-triggered: once DMA is permitted and we've consumed the
                // notification, clear the pending flag. If bus mastering is disabled, preserve the
                // pending doorbell so it can be processed once the guest enables COMMAND.BME.
                //
                // This matches the canonical machine behavior and avoids requiring guests to
                // re-ring the doorbell after enabling bus mastering.
                self.doorbell_pending = false;
            }
        }

        // Only suppress vblank IRQ latching for a single `tick` call after the enable transition.
        self.vblank_irq_enable_pending = false;
    }

    pub fn complete_fence(&mut self, mem: &mut dyn MemoryBus, fence: u64) {
        if !self.bus_master_enabled() {
            return;
        }
        self.executor.complete_fence(&mut self.regs, mem, fence);
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

    pub fn read_presented_scanout_rgba8(&mut self, scanout_id: u32) -> Option<(u32, u32, Vec<u8>)> {
        let scanout = self.executor.read_presented_scanout_rgba8(scanout_id)?;
        Some((scanout.width, scanout.height, scanout.rgba8))
    }

    fn update_irq_level(&mut self) {
        self.irq_level = (self.regs.irq_status & self.regs.irq_enable) != 0;
    }

    fn reset_ring_internal(&mut self) {
        self.executor.reset();
        self.regs.completed_fence = 0;
        self.regs.irq_status = 0;
        self.update_irq_level();
        // DMA portion (updating ring head + fence page) will run on the next tick if bus mastering
        // is enabled.
        self.ring_reset_pending_dma = true;
    }

    fn reset_ring_dma(&mut self, mem: &mut dyn MemoryBus) {
        if self.regs.ring_gpa != 0 {
            let tail = mem.read_u32(self.regs.ring_gpa + RING_TAIL_OFFSET);
            AeroGpuRingHeader::write_head(mem, self.regs.ring_gpa, tail);
        }

        if self.regs.fence_gpa != 0 {
            write_fence_page(
                mem,
                self.regs.fence_gpa,
                self.regs.abi_version,
                self.regs.completed_fence,
            );
        }
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

            mmio::ERROR_CODE => self.regs.error_code,
            mmio::ERROR_FENCE_LO => self.regs.error_fence as u32,
            mmio::ERROR_FENCE_HI => (self.regs.error_fence >> 32) as u32,
            mmio::ERROR_COUNT => self.regs.error_count,

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

    fn mmio_write_dword(&mut self, offset: u64, value: u32) {
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
                    self.reset_ring_internal();
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
                self.doorbell_pending = true;
            }
            mmio::IRQ_ENABLE => {
                let enabling_vblank = (value & irq_bits::SCANOUT_VBLANK) != 0
                    && (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) == 0;
                self.regs.irq_enable = value;
                if (value & irq_bits::FENCE) == 0 {
                    self.regs.irq_status &= !irq_bits::FENCE;
                }
                if (value & irq_bits::SCANOUT_VBLANK) == 0 {
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                }
                if enabling_vblank {
                    // Mirror the legacy emulator behavior: when the guest enables vblank IRQs, do
                    // not immediately latch an IRQ for any vblank edges that the device must
                    // \"catch up\" (host stall / guest paused). The pending vblank edges should
                    // still advance counters/timestamps, but the IRQ status bit must only be
                    // latched on the *next* vblank edge after the enable becomes effective.
                    self.vblank_irq_enable_pending = true;
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
                    self.next_vblank_deadline_ns = None;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                    self.update_irq_level();
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
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }

    fn reset(&mut self) {
        // Preserve BAR programming but disable decoding.
        self.config.set_command(0);

        // Reset register/executor state (device-local reset; guest memory DMA state is not touched).
        self.regs = AeroGpuRegs::default();
        // Re-apply device-model configuration that affects what the guest sees in registers.
        // `vblank_hz` is not guest-controlled; resets must preserve whether vblank is advertised.
        if let Some(period_ns) = self.vblank_period_ns {
            self.regs.scanout0_vblank_period_ns = period_ns.min(u64::from(u32::MAX)) as u32;
        } else {
            self.regs.features &= !FEATURE_VBLANK;
        }
        self.executor.reset();
        self.irq_level = false;
        self.doorbell_pending = false;
        self.ring_reset_pending_dma = false;
        self.next_vblank_deadline_ns = None;
        self.boot_time_ns = None;
        self.vblank_irq_enable_pending = false;
    }
}

impl MmioHandler for AeroGpuPciDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        let size = size.clamp(1, 8);

        // Gate MMIO decode on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return all_ones(size);
        }
        if offset >= crate::regs::AEROGPU_PCI_BAR0_SIZE_BYTES {
            return 0;
        }

        if size == 8 {
            // Build from two dwords to support naturally aligned 64-bit reads.
            let lo = self.read(offset, 4);
            let hi = self.read(offset + 4, 4);
            return lo | (hi << 32);
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let value = self.mmio_read_dword(aligned) as u64;
        match size {
            1 => (value >> shift) & 0xff,
            2 => (value >> shift) & 0xffff,
            4 => value & 0xffff_ffff,
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 {
            return;
        }
        let size = size.clamp(1, 8);

        // Gate MMIO decode on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return;
        }
        if offset >= crate::regs::AEROGPU_PCI_BAR0_SIZE_BYTES {
            return;
        }

        if size == 8 {
            self.write(offset, 4, value & 0xffff_ffff);
            self.write(offset + 4, 4, (value >> 32) & 0xffff_ffff);
            return;
        }

        let aligned = offset & !3;
        let shift = (offset & 3) * 8;
        let value32 = match size {
            1 => ((value as u32) & 0xff) << shift,
            2 => ((value as u32) & 0xffff) << shift,
            4 => value as u32,
            _ => return,
        };

        let merged = if size == 4 {
            value32
        } else {
            let cur = self.mmio_read_dword(aligned);
            let mask = match size {
                1 => 0xffu32 << shift,
                2 => 0xffffu32 << shift,
                _ => 0,
            };
            (cur & !mask) | value32
        };

        self.mmio_write_dword(aligned, merged);
    }
}

/// MMIO handler for the AeroGPU VRAM aperture (PCI BAR1).
///
/// Reads and writes access a byte-addressed VRAM vector. The handler is deliberately "dumb": it
/// does not attempt to emulate VGA planar behavior; it simply exposes the raw bytes.
pub struct AeroGpuBar1VramMmio {
    vram: Rc<RefCell<Vec<u8>>>,
}

impl MmioHandler for AeroGpuBar1VramMmio {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if size > 8 {
            return u64::MAX;
        }

        let vram = self.vram.borrow();
        let mut out = 0u64;
        for i in 0..size {
            let addr = offset.wrapping_add(i as u64);
            let b = usize::try_from(addr)
                .ok()
                .and_then(|idx| vram.get(idx).copied())
                .unwrap_or(0xFF);
            out |= (b as u64) << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        let mut vram = self.vram.borrow_mut();
        for i in 0..size {
            let addr = offset.wrapping_add(i as u64);
            let Some(idx) = usize::try_from(addr).ok() else {
                continue;
            };
            if idx >= vram.len() {
                continue;
            }
            vram[idx] = ((value >> (i * 8)) & 0xFF) as u8;
        }
    }
}

fn all_ones(size: usize) -> u64 {
    if size == 0 {
        return 0;
    }
    if size >= 8 {
        return u64::MAX;
    }
    (1u64 << (size * 8)) - 1
}
