//! PCI device glue for the AeroGPU device model.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;

use aero_devices::pci::{profile, PciConfigSpace, PciDevice};
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};
use aero_protocol::aerogpu::aerogpu_pci as proto;
use memory::{MemoryBus, MmioHandler};

use crate::backend::{AeroGpuBackendSubmission, AeroGpuCommandBackend};
use crate::executor::{AeroGpuExecutor, AeroGpuExecutorConfig};
use crate::regs::{
    irq_bits, mmio, ring_control, AeroGpuRegs, AerogpuErrorCode, AEROGPU_MMIO_MAGIC, FEATURE_VBLANK,
};
use crate::ring::{write_fence_page, AeroGpuRingHeader, RING_TAIL_OFFSET};
use crate::scanout::AeroGpuFormat;
use crate::vblank::{period_ns_from_hz, period_ns_to_reg};

const PCI_COMMAND_MEM_ENABLE: u16 = 1 << 1;
const PCI_COMMAND_BUS_MASTER_ENABLE: u16 = 1 << 2;
const PCI_COMMAND_INTX_DISABLE: u16 = 1 << 10;

/// Size of the guest-visible legacy VGA window backing region (`0xA0000..0xBFFFF`, 128KiB).
///
/// In Aero's canonical AeroGPU BAR1 layout (shared with `aero_gpu_vga` / `aero_machine`), the
/// legacy VGA window aliases into `VRAM[0..LEGACY_VGA_VRAM_BYTES)`. The full 4-plane VGA planar
/// region spans `VRAM[0..VBE_LFB_OFFSET)`, and the VBE packed-pixel framebuffer begins at
/// [`VBE_LFB_OFFSET`].
pub const LEGACY_VGA_VRAM_BYTES: u64 = aero_gpu_vga::VGA_LEGACY_MEM_LEN as u64;

/// Offset within BAR1/VRAM where the VBE linear framebuffer (LFB) region begins.
///
/// The canonical AeroGPU VRAM layout reserves the first 256KiB for legacy VGA planar memory
/// (4 × 64KiB planes). VBE packed-pixel framebuffer writes are mapped after this region so
/// firmware/bootloaders/Windows can draw into the LFB without overwriting VGA plane contents.
///
/// See `docs/16-aerogpu-vga-vesa-compat.md`.
pub const VBE_LFB_OFFSET: u64 = proto::AEROGPU_PCI_BAR1_VBE_LFB_OFFSET_BYTES as u64;

const _: () = {
    assert!(VBE_LFB_OFFSET == aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET as u64);
};

/// Start physical address of the legacy VGA window.
pub const LEGACY_VGA_PADDR_BASE: u64 = aero_gpu_vga::VGA_LEGACY_MEM_START as u64;

/// End physical address (exclusive) of the legacy VGA window.
pub const LEGACY_VGA_PADDR_END: u64 = LEGACY_VGA_PADDR_BASE + LEGACY_VGA_VRAM_BYTES;

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

    /// Pending LO dword for `SCANOUT0_FB_GPA` while waiting for the HI write commit.
    scanout0_fb_gpa_pending_lo: u32,
    /// Whether the guest has written `SCANOUT0_FB_GPA_LO` without a subsequent HI write.
    ///
    /// This avoids exposing a torn 64-bit framebuffer address to scanout readers; drivers
    /// typically write LO then HI.
    scanout0_fb_gpa_lo_pending: bool,
    /// Whether to surface `scanout0_fb_gpa_pending_lo` via MMIO reads while the HI dword is still
    /// pending.
    ///
    /// Normal runtime behavior is to return the last written LO dword when a guest reads
    /// `SCANOUT0_FB_GPA_LO` mid-update. After snapshot restore, however, we want reads to reflect
    /// the committed snapshot state while still allowing a subsequent HI write to commit using the
    /// preserved pending LO dword.
    scanout0_fb_gpa_lo_pending_visible: bool,

    /// Pending LO dword for `CURSOR_FB_GPA` while waiting for the HI write commit.
    cursor_fb_gpa_pending_lo: u32,
    /// Whether the guest has written `CURSOR_FB_GPA_LO` without a subsequent HI write.
    ///
    /// This avoids exposing a torn 64-bit cursor framebuffer address; drivers typically write LO
    /// then HI.
    cursor_fb_gpa_lo_pending: bool,
    /// Whether to surface `cursor_fb_gpa_pending_lo` via MMIO reads while the HI dword is still
    /// pending. See `scanout0_fb_gpa_lo_pending_visible`.
    cursor_fb_gpa_lo_pending_visible: bool,

    doorbell_pending: bool,
    ring_reset_pending_dma: bool,
    pending_fence_completions: VecDeque<u64>,
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
        let bar1_size_usize = bar1_size as usize;

        // The canonical AeroGPU PCI profile advertises a 64MiB BAR1 VRAM aperture. In wasm32 builds
        // (browser runtime), eagerly allocating the full 64MiB backing store can exceed the
        // sandbox's heap limits in constrained environments and tests.
        //
        // Keep the PCI BAR size (device contract) unchanged, but allocate a smaller backing store
        // on wasm32.
        //
        // In that configuration, BAR1 reads within the guest-visible BAR aperture but beyond the
        // allocated backing store return 0 (and writes are ignored). Reads past the BAR aperture
        // float high (0xFF).
        #[cfg(target_arch = "wasm32")]
        const VRAM_ALLOC_BYTES: usize = 32 * 1024 * 1024;
        #[cfg(target_arch = "wasm32")]
        let vram_len = bar1_size_usize.min(VRAM_ALLOC_BYTES);
        #[cfg(not(target_arch = "wasm32"))]
        let vram_len = bar1_size_usize;

        // The default `vec![0; len]` allocation will abort the process on OOM. Since VRAM backing
        // is an internal implementation detail (the guest-visible BAR size is fixed by the PCI
        // profile), allocate fallibly and fall back to a smaller buffer instead of crashing.
        let mut backing = Vec::new();
        if backing.try_reserve_exact(vram_len).is_ok() {
            backing.resize(vram_len, 0u8);
        } else {
            let vbe_lfb_offset = usize::try_from(VBE_LFB_OFFSET).unwrap_or(0);
            let legacy_len = usize::try_from(LEGACY_VGA_VRAM_BYTES).unwrap_or(0);

            // Try to preserve at least the VGA/VBE prefix for boot visuals if possible, but do not
            // assume these allocations will succeed under memory pressure.
            let candidates = [
                vbe_lfb_offset.min(vram_len),
                legacy_len.min(vram_len),
                0usize,
            ];
            for &len in &candidates {
                if len == 0 {
                    break;
                }
                if backing.try_reserve_exact(len).is_ok() {
                    backing.resize(len, 0u8);
                    break;
                }
            }
        }

        let vram = Rc::new(RefCell::new(backing));

        let vblank_period_ns = period_ns_from_hz(cfg.vblank_hz);

        let mut regs = AeroGpuRegs::default();
        if let Some(period_ns) = vblank_period_ns {
            regs.scanout0_vblank_period_ns = period_ns_to_reg(period_ns);
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
            scanout0_fb_gpa_pending_lo: 0,
            scanout0_fb_gpa_lo_pending: false,
            scanout0_fb_gpa_lo_pending_visible: false,
            cursor_fb_gpa_pending_lo: 0,
            cursor_fb_gpa_lo_pending: false,
            cursor_fb_gpa_lo_pending_visible: false,
            doorbell_pending: false,
            ring_reset_pending_dma: false,
            pending_fence_completions: VecDeque::new(),
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

    /// Returns an [`MmioHandler`] implementing the legacy VGA window alias
    /// (`0xA0000..0xC0000` -> `VRAM[0..LEGACY_VGA_VRAM_BYTES)`).
    pub fn legacy_vga_mmio_handler(&self) -> AeroGpuLegacyVgaMmio {
        AeroGpuLegacyVgaMmio {
            vram: Rc::clone(&self.vram),
        }
    }

    /// Translate a physical address in the AeroGPU BAR1 VRAM aperture into a VRAM offset starting
    /// at 0.
    pub fn bar1_paddr_to_vram_offset(bar1_base: u64, paddr: u64) -> Option<u64> {
        if paddr < bar1_base {
            return None;
        }
        let size = Self::bar1_size_bytes()?;
        let off = paddr.checked_sub(bar1_base)?;
        if off >= size {
            return None;
        }
        Some(off)
    }

    /// Translate a VRAM offset into an absolute physical address in the AeroGPU BAR1 range.
    pub fn vram_offset_to_bar1_paddr(bar1_base: u64, vram_offset: u64) -> Option<u64> {
        let size = Self::bar1_size_bytes()?;
        if vram_offset >= size {
            return None;
        }
        bar1_base.checked_add(vram_offset)
    }

    /// Translate a physical address in the legacy VGA window (`0xA0000..0xC0000`) into a VRAM
    /// offset starting at 0.
    pub fn legacy_vga_paddr_to_vram_offset(paddr: u64) -> Option<u64> {
        if !(LEGACY_VGA_PADDR_BASE..LEGACY_VGA_PADDR_END).contains(&paddr) {
            return None;
        }
        Some(paddr - LEGACY_VGA_PADDR_BASE)
    }

    /// Translate a VRAM offset (within the legacy VGA alias region) back into a physical address
    /// in the legacy VGA window (`0xA0000..0xC0000`).
    pub fn legacy_vga_vram_offset_to_paddr(vram_offset: u64) -> Option<u64> {
        if vram_offset >= LEGACY_VGA_VRAM_BYTES {
            return None;
        }
        LEGACY_VGA_PADDR_BASE.checked_add(vram_offset)
    }

    /// Physical base address of the VBE linear framebuffer (LFB) given the assigned BAR1 base.
    pub fn vbe_lfb_base_paddr(bar1_base: u64) -> Option<u64> {
        bar1_base.checked_add(VBE_LFB_OFFSET)
    }

    /// Translate a physical address in the VBE linear framebuffer region into a VRAM offset.
    ///
    /// The VBE LFB is expected to live at `bar1_base + VBE_LFB_OFFSET`.
    pub fn vbe_lfb_paddr_to_vram_offset(bar1_base: u64, paddr: u64) -> Option<u64> {
        let off = Self::bar1_paddr_to_vram_offset(bar1_base, paddr)?;
        if off < VBE_LFB_OFFSET {
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
            while let Some(fence) = self.pending_fence_completions.pop_front() {
                self.executor.complete_fence(&mut self.regs, mem, fence);
            }
        }
        // `tick` has early-return paths (no vblank yet); update IRQ after polling completions.
        self.update_irq_level();

        // Complete any pending ring reset DMA work (head update + fence page).
        // If bus mastering is disabled, defer this until DMA is permitted.
        if self.ring_reset_pending_dma && dma_enabled {
            self.reset_ring_dma(mem);
            self.ring_reset_pending_dma = false;
            self.update_irq_level();
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
        if self.doorbell_pending && dma_enabled {
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

        // Only suppress vblank IRQ latching for a single `tick` call after the enable transition.
        self.vblank_irq_enable_pending = false;
    }

    pub fn complete_fence(&mut self, mem: &mut dyn MemoryBus, fence: u64) {
        if !self.bus_master_enabled() {
            if fence > self.regs.completed_fence {
                self.pending_fence_completions.push_back(fence);
            }
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
        // Treat ring reset as a device-local recovery point: clear any previously latched error
        // payload so guests do not observe stale `ERROR_*` values after resetting the ring.
        self.regs.error_code = AerogpuErrorCode::None as u32;
        self.regs.error_fence = 0;
        self.regs.error_count = 0;
        self.regs.current_submission_fence = 0;
        self.update_irq_level();
        // A ring reset discards any pending doorbell notification. The guest is expected to
        // reinitialize the ring state (including head/tail) before submitting more work.
        self.doorbell_pending = false;
        self.pending_fence_completions.clear();
        // DMA portion (updating ring head + fence page) will run on the next tick if bus mastering
        // is enabled.
        self.ring_reset_pending_dma = true;
    }

    fn reset_ring_dma(&mut self, mem: &mut dyn MemoryBus) {
        if self.regs.ring_gpa != 0 {
            match self.regs.ring_gpa.checked_add(RING_TAIL_OFFSET) {
                Some(tail_addr) if tail_addr.checked_add(4).is_some() => {
                    let tail = mem.read_u32(tail_addr);
                    AeroGpuRingHeader::write_head(mem, self.regs.ring_gpa, tail);
                }
                _ => {
                    // Treat arithmetic overflow as an out-of-bounds guest address. This is a
                    // guest-controlled pointer; record an error rather than silently ignoring the
                    // ring reset side-effect.
                    self.regs.record_error(AerogpuErrorCode::Oob, 0);
                }
            }
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
            mmio::SCANOUT0_FB_GPA_LO => {
                // Expose the pending LO value while keeping `fb_gpa` stable to avoid consumers
                // observing a torn 64-bit address mid-update.
                if self.scanout0_fb_gpa_lo_pending && self.scanout0_fb_gpa_lo_pending_visible {
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
                if self.cursor_fb_gpa_lo_pending && self.cursor_fb_gpa_lo_pending_visible {
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
                    // Reset torn-update tracking so a stale LO write can't affect future updates.
                    self.scanout0_fb_gpa_pending_lo = 0;
                    self.scanout0_fb_gpa_lo_pending = false;
                    self.scanout0_fb_gpa_lo_pending_visible = false;
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
                // Avoid exposing a torn 64-bit `fb_gpa` update. Treat the LO write as starting a
                // new update and commit the combined value on the subsequent HI write.
                self.scanout0_fb_gpa_pending_lo = value;
                self.scanout0_fb_gpa_lo_pending = true;
                self.scanout0_fb_gpa_lo_pending_visible = true;
            }
            mmio::SCANOUT0_FB_GPA_HI => {
                // Drivers typically write LO then HI; treat HI as the commit point.
                let lo = if self.scanout0_fb_gpa_lo_pending {
                    u64::from(self.scanout0_fb_gpa_pending_lo)
                } else {
                    self.regs.scanout0.fb_gpa & 0xffff_ffff
                };
                self.regs.scanout0.fb_gpa = (u64::from(value) << 32) | lo;
                self.scanout0_fb_gpa_lo_pending = false;
                self.scanout0_fb_gpa_lo_pending_visible = false;
            }

            mmio::CURSOR_ENABLE => {
                let prev_enable = self.regs.cursor.enable;
                let new_enable = value != 0;
                self.regs.cursor.enable = new_enable;
                if prev_enable && !new_enable {
                    // Reset torn-update tracking so a stale LO write can't affect future updates.
                    self.cursor_fb_gpa_pending_lo = 0;
                    self.cursor_fb_gpa_lo_pending = false;
                    self.cursor_fb_gpa_lo_pending_visible = false;
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
                // Avoid exposing a torn 64-bit cursor base update. Treat the LO write as starting a
                // new update and commit the combined value on the subsequent HI write.
                self.cursor_fb_gpa_pending_lo = value;
                self.cursor_fb_gpa_lo_pending = true;
                self.cursor_fb_gpa_lo_pending_visible = true;
            }
            mmio::CURSOR_FB_GPA_HI => {
                // Drivers typically write LO then HI; treat HI as the commit point.
                let lo = if self.cursor_fb_gpa_lo_pending {
                    u64::from(self.cursor_fb_gpa_pending_lo)
                } else {
                    self.regs.cursor.fb_gpa & 0xffff_ffff
                };
                self.regs.cursor.fb_gpa = (u64::from(value) << 32) | lo;
                self.cursor_fb_gpa_lo_pending = false;
                self.cursor_fb_gpa_lo_pending_visible = false;
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
            self.regs.scanout0_vblank_period_ns = period_ns_to_reg(period_ns);
        } else {
            self.regs.features &= !FEATURE_VBLANK;
        }
        self.executor.reset();
        self.irq_level = false;
        self.scanout0_fb_gpa_pending_lo = 0;
        self.scanout0_fb_gpa_lo_pending = false;
        self.scanout0_fb_gpa_lo_pending_visible = false;
        self.cursor_fb_gpa_pending_lo = 0;
        self.cursor_fb_gpa_lo_pending = false;
        self.cursor_fb_gpa_lo_pending_visible = false;
        self.doorbell_pending = false;
        self.ring_reset_pending_dma = false;
        self.pending_fence_completions.clear();
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

        let bar_size = u64::from(proto::AEROGPU_PCI_BAR1_SIZE_BYTES);

        let vram = self.vram.borrow();
        let mut out = 0u64;
        for i in 0..size {
            let Some(addr) = offset.checked_add(i as u64) else {
                // Address arithmetic overflow; treat as an out-of-range read ("floating bus").
                out |= 0xFFu64 << (i * 8);
                continue;
            };
            // Reads outside the BAR1 aperture float high (all-ones), matching typical MMIO decode
            // behavior.
            //
            // Note: wasm32 builds may allocate a smaller backing store than the canonical BAR1
            // aperture; reads within the BAR range but beyond the backing store return 0 (and
            // writes are ignored).
            let b = if addr >= bar_size {
                0xFF
            } else {
                usize::try_from(addr)
                    .ok()
                    .and_then(|idx| vram.get(idx).copied())
                    .unwrap_or(0)
            };
            out |= (b as u64) << (i * 8);
        }
        out
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || size > 8 {
            return;
        }

        let bar_size = u64::from(proto::AEROGPU_PCI_BAR1_SIZE_BYTES);

        let mut vram = self.vram.borrow_mut();
        for i in 0..size {
            let Some(addr) = offset.checked_add(i as u64) else {
                continue;
            };
            if addr >= bar_size {
                continue;
            }
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

/// MMIO handler for the legacy VGA window alias (`0xA0000..0xC0000`).
///
/// This is a simple linear alias into the start of BAR1-backed VRAM:
/// `legacy_offset -> VRAM[legacy_offset]`.
pub struct AeroGpuLegacyVgaMmio {
    vram: Rc<RefCell<Vec<u8>>>,
}

impl MmioHandler for AeroGpuLegacyVgaMmio {
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
            // Guard against wrapping physical address arithmetic on malformed offsets.
            let Some(addr) = offset.checked_add(i as u64) else {
                continue;
            };
            if addr >= LEGACY_VGA_VRAM_BYTES {
                continue;
            }
            let b = usize::try_from(addr)
                .ok()
                .and_then(|idx| vram.get(idx).copied())
                .unwrap_or(0);
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
            // Guard against wrapping physical address arithmetic on malformed offsets.
            let Some(addr) = offset.checked_add(i as u64) else {
                continue;
            };
            if addr >= LEGACY_VGA_VRAM_BYTES {
                continue;
            }
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

impl IoSnapshot for AeroGpuPciDevice {
    const DEVICE_ID: [u8; 4] = *b"AGPU";
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 2);

    fn save_state(&self) -> Vec<u8> {
        const TAG_REGS: u16 = 1;
        const TAG_EXECUTOR: u16 = 2;
        const TAG_VBLANK_PERIOD_NS: u16 = 3;
        const TAG_NEXT_VBLANK_DEADLINE_NS: u16 = 4;
        const TAG_BOOT_TIME_NS: u16 = 5;
        const TAG_VBLANK_IRQ_ENABLE_PENDING: u16 = 6;
        const TAG_DOORBELL_PENDING: u16 = 7;
        const TAG_RING_RESET_PENDING_DMA: u16 = 8;
        const TAG_PENDING_SUBMISSIONS: u16 = 9;
        const TAG_SCANOUT0_FB_GPA_PENDING_LO: u16 = 10;
        const TAG_SCANOUT0_FB_GPA_LO_PENDING: u16 = 11;
        const TAG_CURSOR_FB_GPA_PENDING_LO: u16 = 12;
        const TAG_CURSOR_FB_GPA_LO_PENDING: u16 = 13;
        const TAG_PENDING_FENCE_COMPLETIONS: u16 = 14;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);
        w.field_bytes(TAG_REGS, encode_regs(&self.regs));
        w.field_bytes(TAG_EXECUTOR, self.executor.save_snapshot_state());
        if let Some(bytes) = self.executor.save_pending_submissions_snapshot_state() {
            w.field_bytes(TAG_PENDING_SUBMISSIONS, bytes);
        }
        if !self.pending_fence_completions.is_empty() {
            let mut e = Encoder::new().u32(self.pending_fence_completions.len() as u32);
            for fence in &self.pending_fence_completions {
                e = e.u64(*fence);
            }
            w.field_bytes(TAG_PENDING_FENCE_COMPLETIONS, e.finish());
        }
        if let Some(vblank_period_ns) = self.vblank_period_ns {
            w.field_u64(TAG_VBLANK_PERIOD_NS, vblank_period_ns);
        }
        if let Some(next) = self.next_vblank_deadline_ns {
            w.field_u64(TAG_NEXT_VBLANK_DEADLINE_NS, next);
        }
        if let Some(boot) = self.boot_time_ns {
            w.field_u64(TAG_BOOT_TIME_NS, boot);
        }
        w.field_bool(
            TAG_VBLANK_IRQ_ENABLE_PENDING,
            self.vblank_irq_enable_pending,
        );
        w.field_bool(TAG_DOORBELL_PENDING, self.doorbell_pending);
        w.field_bool(TAG_RING_RESET_PENDING_DMA, self.ring_reset_pending_dma);
        w.field_u32(
            TAG_SCANOUT0_FB_GPA_PENDING_LO,
            self.scanout0_fb_gpa_pending_lo,
        );
        w.field_bool(
            TAG_SCANOUT0_FB_GPA_LO_PENDING,
            self.scanout0_fb_gpa_lo_pending,
        );
        w.field_u32(TAG_CURSOR_FB_GPA_PENDING_LO, self.cursor_fb_gpa_pending_lo);
        w.field_bool(TAG_CURSOR_FB_GPA_LO_PENDING, self.cursor_fb_gpa_lo_pending);
        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_REGS: u16 = 1;
        const TAG_EXECUTOR: u16 = 2;
        const TAG_VBLANK_PERIOD_NS: u16 = 3;
        const TAG_NEXT_VBLANK_DEADLINE_NS: u16 = 4;
        const TAG_BOOT_TIME_NS: u16 = 5;
        const TAG_VBLANK_IRQ_ENABLE_PENDING: u16 = 6;
        const TAG_DOORBELL_PENDING: u16 = 7;
        const TAG_RING_RESET_PENDING_DMA: u16 = 8;
        const TAG_PENDING_SUBMISSIONS: u16 = 9;
        const TAG_SCANOUT0_FB_GPA_PENDING_LO: u16 = 10;
        const TAG_SCANOUT0_FB_GPA_LO_PENDING: u16 = 11;
        const TAG_CURSOR_FB_GPA_PENDING_LO: u16 = 12;
        const TAG_CURSOR_FB_GPA_LO_PENDING: u16 = 13;
        const TAG_PENDING_FENCE_COMPLETIONS: u16 = 14;
        const TAG_PENDING_FENCE_COMPLETIONS_LEGACY: u16 = 10;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;

        let regs = match r.bytes(TAG_REGS) {
            Some(buf) => decode_regs(buf)?,
            None => return Err(SnapshotError::InvalidFieldEncoding("missing regs")),
        };

        let vblank_period_ns = r.u64(TAG_VBLANK_PERIOD_NS)?;
        let next_vblank_deadline_ns = r.u64(TAG_NEXT_VBLANK_DEADLINE_NS)?;
        let boot_time_ns = r.u64(TAG_BOOT_TIME_NS)?;
        let vblank_irq_enable_pending = r.bool(TAG_VBLANK_IRQ_ENABLE_PENDING)?.unwrap_or(false);
        let doorbell_pending = r.bool(TAG_DOORBELL_PENDING)?.unwrap_or(false);
        let ring_reset_pending_dma = r.bool(TAG_RING_RESET_PENDING_DMA)?.unwrap_or(false);
        let snapshot_minor = r.header().device_version.minor;

        let (
            scanout0_fb_gpa_pending_lo,
            scanout0_fb_gpa_lo_pending,
            cursor_fb_gpa_pending_lo,
            cursor_fb_gpa_lo_pending,
        ) = if snapshot_minor >= 2 {
            (
                r.u32(TAG_SCANOUT0_FB_GPA_PENDING_LO)?.unwrap_or(0),
                r.bool(TAG_SCANOUT0_FB_GPA_LO_PENDING)?.unwrap_or(false),
                r.u32(TAG_CURSOR_FB_GPA_PENDING_LO)?.unwrap_or(0),
                r.bool(TAG_CURSOR_FB_GPA_LO_PENDING)?.unwrap_or(false),
            )
        } else {
            (0, false, 0, false)
        };

        let pending_fence_completions_tag = if snapshot_minor <= 1 {
            TAG_PENDING_FENCE_COMPLETIONS_LEGACY
        } else {
            TAG_PENDING_FENCE_COMPLETIONS
        };

        let pending_fence_completions = match r.bytes(pending_fence_completions_tag) {
            Some(buf) => {
                // Snapshots may come from untrusted sources; cap sizes to keep decode bounded.
                const MAX_PENDING_FENCE_COMPLETIONS: usize = 65_536;

                let mut d = Decoder::new(buf);
                let count = d.u32()? as usize;
                if count > MAX_PENDING_FENCE_COMPLETIONS {
                    return Err(SnapshotError::InvalidFieldEncoding(
                        "pending_fence_completions",
                    ));
                }
                let mut out = VecDeque::new();
                out.try_reserve(count)
                    .map_err(|_| SnapshotError::OutOfMemory)?;
                for _ in 0..count {
                    out.push_back(d.u64()?);
                }
                d.finish()?;
                out
            }
            None => VecDeque::new(),
        };

        // Reset executor state up-front so any fields not restored from the snapshot do not leak
        // across restore calls.
        self.executor.reset();
        self.executor.last_submissions.clear();
        self.pending_fence_completions.clear();
        // Snapshot state records committed 64-bit GPAs; clear any torn-update tracking so stale LO
        // writes from a previous execution do not affect MMIO reads or future HI write commits.
        self.scanout0_fb_gpa_pending_lo = 0;
        self.scanout0_fb_gpa_lo_pending = false;
        self.scanout0_fb_gpa_lo_pending_visible = false;
        self.cursor_fb_gpa_pending_lo = 0;
        self.cursor_fb_gpa_lo_pending = false;
        self.cursor_fb_gpa_lo_pending_visible = false;

        // Executor state is optional for forward/backward compatibility; missing means "reset".
        if let Some(exec_bytes) = r.bytes(TAG_EXECUTOR) {
            self.executor.load_snapshot_state(exec_bytes)?;
        }
        if let Some(buf) = r.bytes(TAG_PENDING_SUBMISSIONS) {
            self.executor.load_pending_submissions_snapshot_state(buf)?;
        }

        self.regs = regs;
        self.vblank_period_ns = vblank_period_ns;
        self.next_vblank_deadline_ns = next_vblank_deadline_ns;
        self.boot_time_ns = boot_time_ns;
        self.vblank_irq_enable_pending = vblank_irq_enable_pending;
        self.doorbell_pending = doorbell_pending;
        self.ring_reset_pending_dma = ring_reset_pending_dma;
        self.scanout0_fb_gpa_pending_lo = scanout0_fb_gpa_pending_lo;
        self.scanout0_fb_gpa_lo_pending = scanout0_fb_gpa_lo_pending;
        self.cursor_fb_gpa_pending_lo = cursor_fb_gpa_pending_lo;
        self.cursor_fb_gpa_lo_pending = cursor_fb_gpa_lo_pending;
        self.pending_fence_completions = pending_fence_completions;

        // If vblank is disabled or scanout is off, ensure no vblank deadline remains scheduled.
        if self.vblank_period_ns.is_none() || !self.regs.scanout0.enable {
            self.next_vblank_deadline_ns = None;
        }

        self.update_irq_level();
        Ok(())
    }
}

fn encode_regs(regs: &AeroGpuRegs) -> Vec<u8> {
    Encoder::new()
        .u32(regs.abi_version)
        .u64(regs.features)
        .u64(regs.ring_gpa)
        .u32(regs.ring_size_bytes)
        .u32(regs.ring_control)
        .u64(regs.fence_gpa)
        .u64(regs.completed_fence)
        .u32(regs.irq_status)
        .u32(regs.irq_enable)
        // error reporting
        .u32(regs.error_code)
        .u64(regs.error_fence)
        .u32(regs.error_count)
        .u64(regs.current_submission_fence)
        // scanout0
        .bool(regs.scanout0.enable)
        .u32(regs.scanout0.width)
        .u32(regs.scanout0.height)
        .u32(regs.scanout0.format as u32)
        .u32(regs.scanout0.pitch_bytes)
        .u64(regs.scanout0.fb_gpa)
        // vblank
        .u64(regs.scanout0_vblank_seq)
        .u64(regs.scanout0_vblank_time_ns)
        .u32(regs.scanout0_vblank_period_ns)
        // cursor
        .bool(regs.cursor.enable)
        .i32(regs.cursor.x)
        .i32(regs.cursor.y)
        .u32(regs.cursor.hot_x)
        .u32(regs.cursor.hot_y)
        .u32(regs.cursor.width)
        .u32(regs.cursor.height)
        .u32(regs.cursor.format as u32)
        .u64(regs.cursor.fb_gpa)
        .u32(regs.cursor.pitch_bytes)
        // stats (not guest-visible, but deterministic for host debugging)
        .u64(regs.stats.doorbells)
        .u64(regs.stats.submissions)
        .u64(regs.stats.malformed_submissions)
        .u64(regs.stats.gpu_exec_errors)
        .finish()
}

fn decode_regs(bytes: &[u8]) -> SnapshotResult<AeroGpuRegs> {
    let mut d = Decoder::new(bytes);

    let abi_version = d.u32()?;
    let features = d.u64()?;
    let ring_gpa = d.u64()?;
    let ring_size_bytes = d.u32()?;
    let ring_control = d.u32()?;
    let fence_gpa = d.u64()?;
    let completed_fence = d.u64()?;
    let irq_status = d.u32()?;
    let irq_enable = d.u32()?;

    let error_code = d.u32()?;
    let error_fence = d.u64()?;
    let error_count = d.u32()?;
    let current_submission_fence = d.u64()?;

    let scanout0_enable = d.bool()?;
    let scanout0_width = d.u32()?;
    let scanout0_height = d.u32()?;
    let scanout0_format = AeroGpuFormat::from_u32(d.u32()?);
    let scanout0_pitch_bytes = d.u32()?;
    let scanout0_fb_gpa = d.u64()?;

    let scanout0_vblank_seq = d.u64()?;
    let scanout0_vblank_time_ns = d.u64()?;
    let scanout0_vblank_period_ns = d.u32()?;

    let cursor_enable = d.bool()?;
    let cursor_x = d.i32()?;
    let cursor_y = d.i32()?;
    let cursor_hot_x = d.u32()?;
    let cursor_hot_y = d.u32()?;
    let cursor_width = d.u32()?;
    let cursor_height = d.u32()?;
    let cursor_format = AeroGpuFormat::from_u32(d.u32()?);
    let cursor_fb_gpa = d.u64()?;
    let cursor_pitch_bytes = d.u32()?;

    let stats_doorbells = d.u64()?;
    let stats_submissions = d.u64()?;
    let stats_malformed_submissions = d.u64()?;
    let stats_gpu_exec_errors = d.u64()?;

    d.finish()?;

    Ok(AeroGpuRegs {
        abi_version,
        features,
        ring_gpa,
        ring_size_bytes,
        ring_control,
        fence_gpa,
        completed_fence,
        irq_status,
        irq_enable,
        error_code,
        error_fence,
        error_count,
        current_submission_fence,
        scanout0: crate::scanout::AeroGpuScanoutConfig {
            enable: scanout0_enable,
            width: scanout0_width,
            height: scanout0_height,
            format: scanout0_format,
            pitch_bytes: scanout0_pitch_bytes,
            fb_gpa: scanout0_fb_gpa,
        },
        scanout0_vblank_seq,
        scanout0_vblank_time_ns,
        scanout0_vblank_period_ns,
        cursor: crate::scanout::AeroGpuCursorConfig {
            enable: cursor_enable,
            x: cursor_x,
            y: cursor_y,
            hot_x: cursor_hot_x,
            hot_y: cursor_hot_y,
            width: cursor_width,
            height: cursor_height,
            format: cursor_format,
            fb_gpa: cursor_fb_gpa,
            pitch_bytes: cursor_pitch_bytes,
        },
        stats: crate::regs::AeroGpuStats {
            doorbells: stats_doorbells,
            submissions: stats_submissions,
            malformed_submissions: stats_malformed_submissions,
            gpu_exec_errors: stats_gpu_exec_errors,
        },
    })
}
