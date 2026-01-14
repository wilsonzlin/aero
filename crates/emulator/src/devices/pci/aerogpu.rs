use std::collections::VecDeque;
use std::sync::Arc;

use aero_devices_gpu::vblank::{period_ns_from_hz, period_ns_to_reg};
use aero_gpu_vga::{PortIO as _, VgaDevice};
use aero_shared::scanout_state::{ScanoutState, ScanoutStateUpdate, SCANOUT_SOURCE_WDDM};
use memory::MemoryBus;

use crate::devices::aerogpu_regs::{
    irq_bits, mmio, ring_control, AeroGpuRegs, AerogpuErrorCode, AEROGPU_MMIO_MAGIC,
    AEROGPU_PCI_BAR0_SIZE_BYTES, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER, AEROGPU_PCI_DEVICE_ID,
    AEROGPU_PCI_PROG_IF, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE, AEROGPU_PCI_SUBSYSTEM_ID,
    AEROGPU_PCI_SUBSYSTEM_VENDOR_ID, AEROGPU_PCI_VENDOR_ID, FEATURE_CURSOR, FEATURE_VBLANK,
};
use crate::devices::aerogpu_ring::{write_fence_page, AeroGpuRingHeader, RING_TAIL_OFFSET};
use crate::devices::aerogpu_scanout::{
    composite_cursor_rgba_over_scanout, scanout_config_to_scanout_state_update, AeroGpuFormat,
};
use crate::gpu_worker::aerogpu_backend::{AeroGpuBackendSubmission, AeroGpuCommandBackend};
use crate::gpu_worker::aerogpu_executor::{AeroGpuExecutor, AeroGpuExecutorConfig};
use crate::io::pci::{MmioDevice, PciConfigSpace, PciDevice};

#[derive(Clone, Debug)]
pub struct AeroGpuDeviceConfig {
    pub executor: AeroGpuExecutorConfig,
    pub vblank_hz: Option<u32>,
    /// Size in bytes of the BAR1 VRAM aperture.
    ///
    /// Must be a power-of-two and large enough for the legacy VGA backing region plus the VBE
    /// linear framebuffer (LFB) region.
    pub vram_size_bytes: u32,
}

impl Default for AeroGpuDeviceConfig {
    fn default() -> Self {
        Self {
            executor: AeroGpuExecutorConfig::default(),
            vblank_hz: Some(60),
            vram_size_bytes: 32 * 1024 * 1024,
        }
    }
}

// Canonical AeroGPU BAR1 VRAM layout (see `docs/16-aerogpu-vga-vesa-compat.md`):
// - `VRAM[0..VBE_LFB_OFFSET)`: legacy VGA planar storage (4 × 64KiB planes). The guest-visible
//   legacy VGA window (`0xA0000..0xBFFFF`, 128KiB) aliases into `VRAM[0..LEGACY_VGA_VRAM_BYTES)`.
// - `VRAM[VBE_LFB_OFFSET..]`: VBE linear framebuffer (LFB).
const AEROGPU_PCI_BAR1_LFB_OFFSET: u32 = aero_devices_gpu::VBE_LFB_OFFSET as u32;
const LEGACY_VGA_WINDOW_BASE: u32 = aero_gpu_vga::VGA_LEGACY_MEM_START;
const LEGACY_VGA_WINDOW_SIZE: u32 = aero_gpu_vga::VGA_LEGACY_MEM_LEN;
const _: () = {
    assert!(
        AEROGPU_PCI_BAR1_LFB_OFFSET == aero_gpu_vga::VBE_FRAMEBUFFER_OFFSET as u32,
        "AeroGPU BAR1 VBE LFB offset must match the canonical VGA/VBE VRAM layout"
    );
    assert!(
        aero_devices_gpu::LEGACY_VGA_PADDR_BASE as u32 == LEGACY_VGA_WINDOW_BASE,
        "AeroGPU legacy VGA base must match the canonical VGA legacy decode window"
    );
    assert!(
        aero_devices_gpu::LEGACY_VGA_VRAM_BYTES as u32 == LEGACY_VGA_WINDOW_SIZE,
        "AeroGPU legacy VGA window size must match the canonical VGA legacy decode window"
    );
};

pub struct AeroGpuPciDevice {
    config: PciConfigSpace,
    pub bar0: u32,
    bar0_probe: bool,

    pub bar1: u32,
    bar1_probe: bool,
    bar1_size_bytes: u32,

    pub regs: AeroGpuRegs,
    executor: AeroGpuExecutor,
    irq_level: bool,

    vga: VgaDevice,

    scanout_state: Option<Arc<ScanoutState>>,
    last_published_scanout0: Option<ScanoutStateUpdate>,
    /// Sticky ownership latch: once the guest has successfully programmed a valid WDDM scanout0
    /// configuration and enabled it, legacy VGA/VBE must not steal scanout back until reset.
    ///
    /// `SCANOUT0_ENABLE=0` is treated as a visibility toggle (blanking / stop vblank pacing), not a
    /// release back to legacy scanout.
    wddm_scanout_active: bool,
    /// Pending LO dword for `SCANOUT0_FB_GPA` while waiting for the HI write commit.
    scanout0_fb_gpa_pending_lo: u32,
    /// Set after a write to `SCANOUT0_FB_GPA_LO` and cleared on `SCANOUT0_FB_GPA_HI`.
    ///
    /// This ensures we don't publish a scanout state update with a torn 64-bit `fb_gpa`
    /// (drivers typically write LO then HI).
    scanout0_fb_gpa_lo_pending: bool,

    /// Pending LO dword for `CURSOR_FB_GPA` while waiting for the HI write commit.
    cursor_fb_gpa_pending_lo: u32,
    /// Set after a write to `CURSOR_FB_GPA_LO` and cleared on `CURSOR_FB_GPA_HI`.
    ///
    /// This avoids exposing a torn 64-bit cursor framebuffer address (drivers typically write LO
    /// then HI).
    cursor_fb_gpa_lo_pending: bool,

    boot_time_ns: Option<u64>,
    /// Last `now_ns` value observed by [`AeroGpuPciDevice::tick`].
    ///
    /// MMIO writes do not carry a timebase in the legacy `MmioDevice` shim, so whenever we need to
    /// approximate "the current time" for state transitions (e.g. starting a vblank schedule on a
    /// `SCANOUT0_ENABLE` 0→1 transition) we anchor it to the last tick.
    last_tick_ns: u64,
    vblank_interval_ns: Option<u64>,
    next_vblank_ns: Option<u64>,
    suppress_vblank_irq: bool,

    ring_reset_pending: bool,
    ring_reset_pending_dma: bool,
    doorbell_pending: bool,
    pending_fence_completions: VecDeque<u64>,
}

impl AeroGpuPciDevice {
    pub fn new(cfg: AeroGpuDeviceConfig, bar0: u32, bar1: u32) -> Self {
        assert!(
            cfg.vram_size_bytes.is_power_of_two(),
            "BAR1 VRAM size must be a power-of-two"
        );
        assert!(
            cfg.vram_size_bytes >= AEROGPU_PCI_BAR1_LFB_OFFSET + 0x1000,
            "BAR1 VRAM size must be large enough for legacy VGA + LFB"
        );

        let mut config_space = PciConfigSpace::new();

        config_space.set_u16(0x00, AEROGPU_PCI_VENDOR_ID);
        config_space.set_u16(0x02, AEROGPU_PCI_DEVICE_ID);

        config_space.set_u16(0x2c, AEROGPU_PCI_SUBSYSTEM_VENDOR_ID);
        config_space.set_u16(0x2e, AEROGPU_PCI_SUBSYSTEM_ID);

        config_space.set_u8(0x09, AEROGPU_PCI_PROG_IF);
        config_space.set_u8(0x0a, AEROGPU_PCI_SUBCLASS_VGA_COMPATIBLE);
        config_space.set_u8(0x0b, AEROGPU_PCI_CLASS_CODE_DISPLAY_CONTROLLER);

        // BAR0 (MMIO regs), non-prefetchable 32-bit.
        let bar0 = bar0 & !(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
        config_space.set_u32(0x10, bar0);

        // BAR1 (VRAM aperture), prefetchable 32-bit.
        let bar1 = bar1 & !(cfg.vram_size_bytes - 1) & 0xffff_fff0;
        config_space.set_u32(0x14, bar1 | (1 << 3));

        // Interrupt pin INTA#.
        config_space.set_u8(0x3d, 1);

        let vblank_interval_ns = period_ns_from_hz(cfg.vblank_hz);

        let mut regs = AeroGpuRegs::default();
        if let Some(interval_ns) = vblank_interval_ns {
            regs.scanout0_vblank_period_ns = period_ns_to_reg(interval_ns);
        } else {
            // If vblank is disabled by configuration, also clear the advertised feature bit so
            // guests don't wait on a vblank that will never arrive.
            regs.features &= !FEATURE_VBLANK;
        }

        let vga = VgaDevice::new_with_config(aero_gpu_vga::VgaConfig {
            vram_size: cfg.vram_size_bytes as usize,
            vram_bar_base: bar1,
            lfb_offset: AEROGPU_PCI_BAR1_LFB_OFFSET,
            // Reserve the full 4-plane VGA planar region (256KiB) so switching between VBE and
            // legacy modes doesn't clobber plane contents.
            legacy_plane_count: 4,
        });

        Self {
            config: config_space,
            bar0,
            bar0_probe: false,
            bar1,
            bar1_probe: false,
            bar1_size_bytes: cfg.vram_size_bytes,
            regs,
            executor: AeroGpuExecutor::new(cfg.executor),
            irq_level: false,
            vga,
            scanout_state: None,
            wddm_scanout_active: false,
            last_published_scanout0: None,
            scanout0_fb_gpa_pending_lo: 0,
            scanout0_fb_gpa_lo_pending: false,
            cursor_fb_gpa_pending_lo: 0,
            cursor_fb_gpa_lo_pending: false,
            // The `now_ns` timebase used by `tick()` is defined as "nanoseconds since device boot"
            // (see `drivers/aerogpu/protocol/vblank.md`). Use 0 as the stable epoch so vblank
            // timestamps remain meaningful even if the first `tick()` is delayed.
            boot_time_ns: Some(0),
            last_tick_ns: 0,
            vblank_interval_ns,
            next_vblank_ns: None,
            suppress_vblank_irq: false,
            ring_reset_pending: false,
            ring_reset_pending_dma: false,
            doorbell_pending: false,
            pending_fence_completions: VecDeque::new(),
        }
    }

    fn maybe_claim_wddm_scanout(&mut self) {
        if self.wddm_scanout_active {
            return;
        }
        // Claim WDDM ownership only once scanout is actually enabled. The Windows driver stages
        // `SCANOUT0_*` values and uses `SCANOUT0_ENABLE` as the commit/visibility toggle.
        //
        // This prevents prematurely suppressing legacy VGA/VBE output while the driver is still
        // programming the scanout registers with `ENABLE=0`.
        if !self.regs.scanout0.enable {
            return;
        }
        // Do not claim while `fb_gpa` is mid-update (LO written without HI).
        if self.scanout0_fb_gpa_lo_pending {
            return;
        }

        // Claim only when the scanout configuration is valid for presentation. Invalid/unsupported
        // configurations are treated as "disabled" and must not steal ownership from legacy sources.
        let update =
            scanout_config_to_scanout_state_update(&self.regs.scanout0, SCANOUT_SOURCE_WDDM);
        if update.width == 0 || update.height == 0 {
            return;
        }

        self.wddm_scanout_active = true;
        // Force an initial publish to any scanout consumer.
        self.last_published_scanout0 = None;
    }

    fn command(&self) -> u16 {
        self.config.read(0x04, 2) as u16
    }

    fn io_space_enabled(&self) -> bool {
        (self.command() & (1 << 0)) != 0
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

    pub fn bar1_size(&self) -> u64 {
        u64::from(self.bar1_size_bytes)
    }

    pub fn bar1_lfb_base(&self) -> u64 {
        u64::from(self.bar1).wrapping_add(u64::from(AEROGPU_PCI_BAR1_LFB_OFFSET))
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    fn maybe_publish_scanout_state(&self, update: ScanoutStateUpdate) {
        let Some(state) = &self.scanout_state else {
            return;
        };

        if let Some(cur) = state.try_snapshot() {
            if cur.source == update.source
                && cur.base_paddr_lo == update.base_paddr_lo
                && cur.base_paddr_hi == update.base_paddr_hi
                && cur.width == update.width
                && cur.height == update.height
                && cur.pitch_bytes == update.pitch_bytes
                && cur.format == update.format
            {
                return;
            }
        }
        let _ = state.try_publish(update);
    }

    #[cfg(all(target_arch = "wasm32", not(feature = "wasm-threaded")))]
    fn update_scanout_state_from_vga(&self) {
        // `aero-gpu-vga::VgaDevice::active_scanout_update()` is not available for `wasm32`
        // builds without `wasm-threaded`. In that configuration the emulator does not have a
        // shared-memory scanout channel, so skip legacy VGA scanout publishing entirely.
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    fn update_scanout_state_from_vga(&self) {
        // Once WDDM has claimed scanout ownership, legacy VGA/VBE must not steal it back until
        // device reset, even if WDDM temporarily disables scanout (`SCANOUT0_ENABLE` acts as a
        // visibility toggle, not a handoff back to legacy).
        if self.wddm_scanout_active {
            return;
        }

        self.maybe_publish_scanout_state(self.vga.active_scanout_update());
    }

    pub fn vga_port_read(&mut self, port: u16, size: usize) -> u32 {
        // Gate legacy port I/O on PCI command I/O Space Enable (bit 0), like other PCI devices.
        // Firmware/OS is expected to set this bit when enumerating the device.
        if !self.io_space_enabled() {
            return match size {
                0 => 0,
                1 => 0xFF,
                2 => 0xFFFF,
                4 => 0xFFFF_FFFF,
                _ => 0xFFFF_FFFF,
            };
        }

        // VGA legacy ports + Bochs VBE ports.
        if (aero_gpu_vga::VGA_LEGACY_IO_START..=aero_gpu_vga::VGA_LEGACY_IO_END).contains(&port)
            || (aero_gpu_vga::VBE_DISPI_IO_START..=aero_gpu_vga::VBE_DISPI_IO_END).contains(&port)
        {
            return self.vga.port_read(port, size);
        }
        match size {
            0 => 0,
            1 => 0xFF,
            2 => 0xFFFF,
            4 => 0xFFFF_FFFF,
            _ => 0xFFFF_FFFF,
        }
    }

    pub fn vga_port_write(&mut self, port: u16, size: usize, value: u32) {
        if !self.io_space_enabled() {
            return;
        }
        if (aero_gpu_vga::VGA_LEGACY_IO_START..=aero_gpu_vga::VGA_LEGACY_IO_END).contains(&port)
            || (aero_gpu_vga::VBE_DISPI_IO_START..=aero_gpu_vga::VBE_DISPI_IO_END).contains(&port)
        {
            self.vga.port_write(port, size, value);
            self.update_scanout_state_from_vga();
        }
    }

    /// Legacy VGA window MMIO handler for guest physical `0xA0000..0xBFFFF`.
    pub fn vga_legacy_mmio_read(&mut self, paddr: u32, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if !(1..=8).contains(&size) {
            return u64::MAX;
        }
        let end = LEGACY_VGA_WINDOW_BASE + LEGACY_VGA_WINDOW_SIZE;
        if paddr < LEGACY_VGA_WINDOW_BASE || paddr >= end {
            return u64::MAX;
        }
        let mut out = 0u64;
        for i in 0..size {
            let b = self.vga.mem_read_u8(paddr.wrapping_add(i as u32)) as u64;
            out |= b << (i * 8);
        }
        out
    }

    pub fn vga_legacy_mmio_write(&mut self, paddr: u32, size: usize, value: u64) {
        if size == 0 || !(1..=8).contains(&size) {
            return;
        }
        let end = LEGACY_VGA_WINDOW_BASE + LEGACY_VGA_WINDOW_SIZE;
        if paddr < LEGACY_VGA_WINDOW_BASE || paddr >= end {
            return;
        }
        for i in 0..size {
            let b = ((value >> (i * 8)) & 0xFF) as u8;
            self.vga.mem_write_u8(paddr.wrapping_add(i as u32), b);
        }
        // Legacy window writes can affect text rendering; update scanout if still legacy-owned.
        self.update_scanout_state_from_vga();
    }

    /// BAR1 VRAM aperture MMIO handler (raw VRAM bytes).
    pub fn vram_mmio_read(&mut self, offset: u64, size: usize) -> u64 {
        if size == 0 {
            return 0;
        }
        if !(1..=8).contains(&size) {
            return u64::MAX;
        }
        // Gate VRAM decode on PCI command Memory Space Enable (bit 1).
        if !self.mem_space_enabled() {
            return u64::MAX;
        }
        let mut out = 0u64;
        let vram = self.vga.vram();
        for i in 0..size {
            // Defensive: avoid wrapping physical address arithmetic on malformed offsets.
            let Some(addr) = offset.checked_add(i as u64) else {
                out |= 0xFFu64 << (i * 8);
                continue;
            };
            let b = usize::try_from(addr)
                .ok()
                .and_then(|idx| vram.get(idx).copied())
                .unwrap_or(0xFF) as u64;
            out |= b << (i * 8);
        }
        out
    }

    pub fn vram_mmio_write(&mut self, offset: u64, size: usize, value: u64) {
        if size == 0 || !(1..=8).contains(&size) {
            return;
        }
        if !self.mem_space_enabled() {
            return;
        }
        let vram_len = self.vga.vram().len();
        let vram = self.vga.vram_mut();
        for i in 0..size {
            // Defensive: avoid wrapping physical address arithmetic on malformed offsets.
            let Some(addr) = offset.checked_add(i as u64) else {
                continue;
            };
            let Some(idx) = usize::try_from(addr).ok() else {
                continue;
            };
            if idx >= vram_len {
                continue;
            }
            let b = ((value >> (i * 8)) & 0xFF) as u8;
            vram[idx] = b;
        }
    }
    pub fn reset(&mut self) {
        // Preserve the configured vblank cadence and executor backend while resetting all
        // guest-visible state.
        let vblank_interval_ns = self.vblank_interval_ns;

        self.regs = AeroGpuRegs::default();
        if let Some(interval_ns) = vblank_interval_ns {
            self.regs.scanout0_vblank_period_ns = period_ns_to_reg(interval_ns);
        } else {
            self.regs.features &= !FEATURE_VBLANK;
        }

        self.executor.reset();
        self.irq_level = false;
        self.boot_time_ns = Some(0);
        self.last_tick_ns = 0;
        self.next_vblank_ns = None;
        self.suppress_vblank_irq = false;
        self.ring_reset_pending = false;
        self.ring_reset_pending_dma = false;
        self.doorbell_pending = false;
        self.pending_fence_completions.clear();

        // Reset scanout-state publish bookkeeping. If a reset occurs mid-framebuffer-address update,
        // we must not leave the scanout state publisher permanently blocked on a stale LO write.
        self.scanout0_fb_gpa_pending_lo = 0;
        self.scanout0_fb_gpa_lo_pending = false;
        self.wddm_scanout_active = false;
        self.last_published_scanout0 = None;
        self.wddm_scanout_active = false;

        // Reset torn cursor framebuffer address tracking.
        self.cursor_fb_gpa_pending_lo = 0;
        self.cursor_fb_gpa_lo_pending = false;

        // Resetting guest-visible registers implicitly disables scanout0. Ensure scanout consumers
        // see ownership revert back to the legacy VGA/VBE path rather than continuing to treat WDDM
        // as the active scanout source.
        self.publish_legacy_scanout_state();
    }

    pub fn tick(&mut self, mem: &mut dyn MemoryBus, now_ns: u64) {
        // Coalesce any deferred scanout register updates (page flips / dynamic reprogramming).
        self.maybe_publish_wddm_scanout0_state();

        // Record time first so MMIO writes that occur after this `tick()` can anchor their state
        // transitions to a sensible "current time" approximation.
        self.last_tick_ns = now_ns;

        let boot_time_ns = *self.boot_time_ns.get_or_insert(now_ns);

        let dma_enabled = self.bus_master_enabled();
        let suppress_vblank_irq = self.suppress_vblank_irq;
        // Suppression is a one-tick guard against "catch-up" vblank edges causing an immediate IRQ
        // when the guest enables vblank delivery. It must not persist across multiple `tick()` calls
        // or we'd miss the first vblank after enable.
        self.suppress_vblank_irq = false;

        if self.ring_reset_pending {
            self.ring_reset_pending = false;
            self.reset_ring_internal();
        }

        // Ring reset has DMA side effects (sync head -> tail and rewrite the fence page). If bus
        // mastering is disabled, defer the DMA portion until COMMAND.BME is enabled.
        if self.ring_reset_pending_dma && dma_enabled {
            self.reset_ring_dma(mem);
            self.ring_reset_pending_dma = false;
        }

        // Polling completions and flushing fences may write guest memory (fence page / writeback).
        // When PCI bus mastering is disabled (COMMAND.BME=0), the device must not perform DMA.
        if dma_enabled {
            self.executor.poll_backend_completions(&mut self.regs, mem);
            while let Some(fence) = self.pending_fence_completions.pop_front() {
                self.executor.complete_fence(&mut self.regs, mem, fence);
            }
        }
        // `tick` has early-return paths (no vblank yet); update IRQ after polling completions.
        self.update_irq_level();

        // If vblank pacing is disabled (by config or by disabling the scanout), do not allow any
        // vsync-delayed fences to remain queued forever.
        if dma_enabled && (self.vblank_interval_ns.is_none() || !self.regs.scanout0.enable) {
            self.executor.flush_pending_fences(&mut self.regs, mem);
            self.update_irq_level();
        }

        // Vblank bookkeeping and vblank-paced fence completion. This is intentionally independent
        // of doorbell processing so we can catch up vblank state before consuming new submissions.
        if let (Some(interval_ns), true) = (self.vblank_interval_ns, self.regs.scanout0.enable) {
            let mut next = self
                .next_vblank_ns
                .unwrap_or_else(|| now_ns.saturating_add(interval_ns));
            if now_ns < next {
                self.next_vblank_ns = Some(next);
            } else {
                let mut ticks = 0u32;
                while now_ns >= next {
                    // Counters advance even if vblank IRQ delivery is masked.
                    self.regs.scanout0_vblank_seq = self.regs.scanout0_vblank_seq.wrapping_add(1);
                    let t_ns = next.saturating_sub(boot_time_ns);
                    self.regs.scanout0_vblank_time_ns = t_ns;

                    // Only latch the vblank IRQ status bit while the guest has it enabled.
                    // This prevents an immediate "stale" interrupt on re-enable.
                    if !suppress_vblank_irq
                        && (self.regs.irq_enable & irq_bits::SCANOUT_VBLANK) != 0
                    {
                        self.regs.irq_status |= irq_bits::SCANOUT_VBLANK;
                    }

                    if dma_enabled {
                        self.executor.process_vblank_tick(&mut self.regs, mem);
                    }

                    next = next.saturating_add(interval_ns);
                    ticks += 1;

                    // Avoid unbounded catch-up work if the host stalls for a very long time.
                    if ticks >= 1024 {
                        next = now_ns.saturating_add(interval_ns);
                        break;
                    }
                }
                self.next_vblank_ns = Some(next);
                self.update_irq_level();
            }
        }

        // Consume any pending doorbells after catching up vblank state so vsync-paced submissions
        // cannot complete on an already-elapsed vblank edge.
        //
        // If bus mastering is disabled, preserve the pending doorbell so it can be processed once
        // the guest enables COMMAND.BME. This matches the canonical machine behavior and avoids
        // requiring guests to re-ring the doorbell after enabling bus mastering.
        if self.doorbell_pending && dma_enabled {
            self.doorbell_pending = false;
            self.executor.process_doorbell(&mut self.regs, mem);
            self.update_irq_level();
        }
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
            if fence > self.regs.completed_fence {
                self.pending_fence_completions.push_back(fence);
            }
            return;
        }
        self.executor.complete_fence(&mut self.regs, mem, fence);
        self.update_irq_level();
    }

    pub fn set_backend(&mut self, backend: Box<dyn AeroGpuCommandBackend>) {
        self.executor.set_backend(backend);
    }

    pub fn set_scanout_state(&mut self, scanout_state: Option<Arc<ScanoutState>>) {
        self.scanout_state = scanout_state;
        // `last_published_scanout0` caches the last update we published to a `ScanoutState`. If the
        // host swaps out the `ScanoutState` instance (e.g. attaching a new scanout consumer), force
        // a re-publish so the new consumer immediately receives the current scanout descriptor.
        self.last_published_scanout0 = None;
        // Publish a best-effort current state immediately so consumers don't have to wait for the
        // next register write / tick.
        self.maybe_publish_wddm_scanout0_state();
        if !self.wddm_scanout_active {
            self.publish_legacy_scanout_state();
        }
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
    pub fn read_presented_scanout_rgba8(
        &mut self,
        mem: &mut dyn MemoryBus,
        scanout_id: u32,
    ) -> Option<(u32, u32, Vec<u8>)> {
        let mut scanout = self.executor.read_presented_scanout_rgba8(scanout_id)?;

        /*
         * Presentation path: composite the hardware cursor over the last-presented
         * scanout so host screenshots/display match what a real GPU scanout would
         * show.
         *
         * Note: `read_scanout0_rgba` reads scanout0 from guest memory and does
         * *not* include the cursor overlay.
         */
        // Cursor DMA is gated by PCI COMMAND.BME (bus mastering enable). When BME is disabled, a
        // real device cannot fetch the cursor framebuffer from guest memory, so do not composite
        // it into the presented scanout.
        if self.bus_master_enabled() && (self.regs.features & FEATURE_CURSOR) != 0 {
            if let Some(cursor_rgba) = self.regs.cursor.read_rgba(mem) {
                let _ = composite_cursor_rgba_over_scanout(
                    &mut scanout.rgba8,
                    scanout.width as usize,
                    scanout.height as usize,
                    &self.regs.cursor,
                    &cursor_rgba,
                );
            }
        }
        Some((scanout.width, scanout.height, scanout.rgba8))
    }

    fn maybe_publish_wddm_scanout0_state(&mut self) {
        self.maybe_claim_wddm_scanout();
        if !self.wddm_scanout_active {
            return;
        }

        // Do not publish while `fb_gpa` is mid-update (LO written without HI).
        if self.scanout0_fb_gpa_lo_pending {
            return;
        }

        let Some(state) = self.scanout_state.as_ref().map(Arc::clone) else {
            return;
        };

        let update =
            scanout_config_to_scanout_state_update(&self.regs.scanout0, SCANOUT_SOURCE_WDDM);

        if self.last_published_scanout0 == Some(update) {
            return;
        }

        state.publish(update);
        self.last_published_scanout0 = Some(update);
    }

    fn publish_legacy_scanout_state(&self) {
        self.update_scanout_state_from_vga();
    }
    fn update_irq_level(&mut self) {
        self.irq_level = (self.regs.irq_status & self.regs.irq_enable) != 0;
    }

    fn reset_ring_internal(&mut self) {
        self.executor.reset();
        self.regs.completed_fence = 0;
        // Treat ring reset as a device-local recovery point: clear any previously latched error
        // payload so the guest does not observe stale `ERROR_*` values after resetting the ring.
        self.regs.error_code = AerogpuErrorCode::None as u32;
        self.regs.error_fence = 0;
        self.regs.error_count = 0;
        self.regs.current_submission_fence = 0;
        self.regs.irq_status = 0;
        self.update_irq_level();
        // A ring reset discards any pending doorbell notification. The guest is expected to
        // reinitialize ring state (including head/tail) before submitting more work.
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
                    self.ring_reset_pending = true;
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
                if enabling_vblank {
                    // Suppress vblank IRQ latching on the next `tick` so we don't immediately raise
                    // an interrupt due to catch-up vblank edges that occurred before the guest
                    // enabled vblank delivery.
                    self.suppress_vblank_irq = true;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
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
                let prev_enable = self.regs.scanout0.enable;
                let new_enable = value != 0;
                self.regs.scanout0.enable = new_enable;
                if !prev_enable && new_enable {
                    // Start vblank scheduling relative to the time of the enable transition.
                    //
                    // The legacy MMIO shim does not provide a `now_ns` parameter, so we approximate
                    // the enable timestamp using the last `tick()` time. This is sufficient to
                    // model catch-up behavior in tests and keeps the vblank clock "free-running"
                    // while scanout is enabled.
                    if let Some(interval_ns) = self.vblank_interval_ns {
                        self.next_vblank_ns = Some(self.last_tick_ns.saturating_add(interval_ns));
                    }
                }
                if prev_enable && !new_enable {
                    // When scanout is disabled, stop vblank scheduling and drop any pending vblank IRQ.
                    self.next_vblank_ns = None;
                    self.regs.irq_status &= !irq_bits::SCANOUT_VBLANK;
                    // Reset torn-update tracking so a stale LO write can't block future publishes.
                    self.scanout0_fb_gpa_pending_lo = 0;
                    self.scanout0_fb_gpa_lo_pending = false;
                }

                self.maybe_claim_wddm_scanout();

                if prev_enable && !new_enable && !self.wddm_scanout_active {
                    // If WDDM never successfully claimed scanout ownership, disabling scanout
                    // returns output to the legacy VGA/VBE sources.
                    self.publish_legacy_scanout_state();
                } else {
                    // Once WDDM has claimed scanout ownership, SCANOUT0_ENABLE acts as a visibility
                    // toggle: disabling scanout blanks presentation but does not revert ownership
                    // back to legacy until reset.
                    self.maybe_publish_wddm_scanout0_state();
                }
                self.update_irq_level();
            }
            mmio::SCANOUT0_WIDTH => {
                self.regs.scanout0.width = value;
                self.maybe_claim_wddm_scanout();
                if self.regs.scanout0.enable {
                    self.maybe_publish_wddm_scanout0_state();
                }
            }
            mmio::SCANOUT0_HEIGHT => {
                self.regs.scanout0.height = value;
                self.maybe_claim_wddm_scanout();
                if self.regs.scanout0.enable {
                    self.maybe_publish_wddm_scanout0_state();
                }
            }
            mmio::SCANOUT0_FORMAT => {
                self.regs.scanout0.format = AeroGpuFormat::from_u32(value);
                self.maybe_claim_wddm_scanout();
                if self.regs.scanout0.enable {
                    self.maybe_publish_wddm_scanout0_state();
                }
            }
            mmio::SCANOUT0_PITCH_BYTES => {
                self.regs.scanout0.pitch_bytes = value;
                self.maybe_claim_wddm_scanout();
                if self.regs.scanout0.enable {
                    self.maybe_publish_wddm_scanout0_state();
                }
            }
            mmio::SCANOUT0_FB_GPA_LO => {
                // Avoid exposing a torn 64-bit `fb_gpa` update. Treat the LO write as starting a
                // new update and commit the combined value on the subsequent HI write.
                self.scanout0_fb_gpa_pending_lo = value;
                self.scanout0_fb_gpa_lo_pending = true;
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
                self.maybe_claim_wddm_scanout();
                if self.regs.scanout0.enable {
                    self.maybe_publish_wddm_scanout0_state();
                }
            }

            mmio::CURSOR_ENABLE => {
                let prev = self.regs.cursor.enable;
                let new_enable = value != 0;
                self.regs.cursor.enable = new_enable;
                if prev && !new_enable {
                    // Reset torn-update tracking so a stale LO write can't affect future cursor
                    // updates.
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
                // Avoid exposing a torn 64-bit cursor framebuffer address. Treat the LO write as
                // starting a new update and commit the combined value on the subsequent HI write.
                self.cursor_fb_gpa_pending_lo = value;
                self.cursor_fb_gpa_lo_pending = true;
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
            }
            mmio::CURSOR_PITCH_BYTES => self.regs.cursor.pitch_bytes = value,

            // Ignore writes to read-only / unknown registers.
            _ => {}
        }
    }

    fn mmio_read_u32(&mut self, offset: u64, size: usize) -> u32 {
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

    fn mmio_write_u32(&mut self, offset: u64, size: usize, value: u32) {
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
            let cur = self.mmio_read_u32(aligned, 4);
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

impl PciDevice for AeroGpuPciDevice {
    fn config_read(&self, offset: u16, size: usize) -> u32 {
        if !matches!(size, 1 | 2 | 4) {
            return 0;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return 0;
        };
        if end as usize > 256 {
            return 0;
        }

        let bar0_off = 0x10u16;
        let bar0_end = bar0_off + 4;
        let bar1_off = 0x14u16;
        let bar1_end = bar1_off + 4;

        let overlaps_bar0 = offset < bar0_end && end > bar0_off;
        let overlaps_bar1 = offset < bar1_end && end > bar1_off;

        if overlaps_bar0 || overlaps_bar1 {
            let bar0_mask = (!(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1)) & 0xffff_fff0;
            let bar0_val = if self.bar0_probe {
                bar0_mask
            } else {
                self.bar0
            };

            let bar1_mask = (!(self.bar1_size_bytes - 1)) & 0xffff_fff0;
            let bar1_val = if self.bar1_probe {
                bar1_mask | (1 << 3)
            } else {
                self.bar1 | (1 << 3)
            };

            let mut out = 0u32;
            for i in 0..size {
                let byte_off = offset + i as u16;
                let byte = if (bar0_off..bar0_end).contains(&byte_off) {
                    let shift = u32::from(byte_off - bar0_off) * 8;
                    (bar0_val >> shift) & 0xFF
                } else if (bar1_off..bar1_end).contains(&byte_off) {
                    let shift = u32::from(byte_off - bar1_off) * 8;
                    (bar1_val >> shift) & 0xFF
                } else {
                    self.config.read(byte_off, 1) & 0xFF
                };
                out |= byte << (8 * i);
            }
            return out;
        }

        self.config.read(offset, size)
    }

    fn config_write(&mut self, offset: u16, size: usize, value: u32) {
        if !matches!(size, 1 | 2 | 4) {
            return;
        }
        let Some(end) = offset.checked_add(size as u16) else {
            return;
        };
        if end as usize > 256 {
            return;
        }

        let bar0_off = 0x10u16;
        let bar0_end = bar0_off + 4;
        let bar1_off = 0x14u16;
        let bar1_end = bar1_off + 4;

        let overlaps_bar0 = offset < bar0_end && end > bar0_off;
        let overlaps_bar1 = offset < bar1_end && end > bar1_off;

        // PCI BAR probing uses an all-ones write to discover the size mask.
        if offset == bar0_off && size == 4 && value == 0xffff_ffff {
            self.bar0_probe = true;
            self.bar0 = 0;
            self.config.write(bar0_off, 4, 0);
            return;
        }
        if offset == bar1_off && size == 4 && value == 0xffff_ffff {
            self.bar1_probe = true;
            self.bar1 = 0;
            self.config.write(bar1_off, 4, 0);
            return;
        }

        if overlaps_bar0 || overlaps_bar1 {
            if overlaps_bar0 {
                self.bar0_probe = false;
            }
            if overlaps_bar1 {
                self.bar1_probe = false;
            }

            self.config.write(offset, size, value);

            if overlaps_bar0 {
                let raw = self.config.read(bar0_off, 4);
                let addr_mask = !(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
                let base = raw & addr_mask;
                self.bar0 = base;
                self.config.write(bar0_off, 4, base);
            }

            if overlaps_bar1 {
                let raw = self.config.read(bar1_off, 4);
                let addr_mask = (!(self.bar1_size_bytes - 1)) & 0xffff_fff0;
                let base = raw & addr_mask;
                self.bar1 = base;
                self.config.write(bar1_off, 4, base | (1 << 3));
                self.vga.set_vram_bar_base(self.bar1);
                self.update_scanout_state_from_vga();
            }

            return;
        }

        self.config.write(offset, size, value);
    }
}

impl MmioDevice for AeroGpuPciDevice {
    fn mmio_read(&mut self, _mem: &mut dyn MemoryBus, offset: u64, size: usize) -> u32 {
        self.mmio_read_u32(offset, size)
    }

    fn mmio_write(&mut self, mem: &mut dyn MemoryBus, offset: u64, size: usize, value: u32) {
        let _ = mem;
        self.mmio_write_u32(offset, size, value);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use super::*;

    use crate::devices::aerogpu_regs::{AerogpuErrorCode, FEATURE_ERROR_INFO};
    use crate::gpu_worker::aerogpu_backend::{
        AeroGpuBackendCompletion, AeroGpuBackendScanout, AeroGpuBackendSubmission,
    };
    use aero_protocol::aerogpu::aerogpu_cmd::{
        AerogpuCmdHdr, AerogpuCmdOpcode, AerogpuCmdPresent, AerogpuCmdStreamHeader,
        AEROGPU_CMD_STREAM_MAGIC, AEROGPU_PRESENT_FLAG_VSYNC,
    };
    use aero_protocol::aerogpu::aerogpu_pci::AEROGPU_ABI_VERSION_U32;
    use memory::bus::PhysicalMemoryBus;
    use memory::phys::DenseMemory;

    #[test]
    fn pci_bar_probe_subword_reads_return_mask_bytes() {
        let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0x1000, 0x2000_0000);

        dev.config_write(0x10, 4, 0xffff_ffff);
        let mask0 = dev.config_read(0x10, 4);
        assert_eq!(
            mask0,
            (!(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1)) & 0xffff_fff0
        );
        assert_eq!(dev.config_read(0x10, 1), mask0 & 0xFF);
        assert_eq!(dev.config_read(0x11, 1), (mask0 >> 8) & 0xFF);
        assert_eq!(dev.config_read(0x12, 2), (mask0 >> 16) & 0xFFFF);

        dev.config_write(0x14, 4, 0xffff_ffff);
        let mask1 = dev.config_read(0x14, 4);
        let expected_mask1 = (!(dev.bar1_size_bytes - 1)) & 0xffff_fff0 | (1 << 3);
        assert_eq!(mask1, expected_mask1);
        assert_eq!(dev.config_read(0x14, 1), mask1 & 0xFF);
        assert_eq!(dev.config_read(0x15, 1), (mask1 >> 8) & 0xFF);
        assert_eq!(dev.config_read(0x16, 2), (mask1 >> 16) & 0xFFFF);
    }

    #[test]
    fn pci_bar_subword_write_updates_bar_bases() {
        let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0, 0);

        dev.config_write(0x10, 2, 0x1235);
        let expected_bar0 = 0x0000_1235 & !(AEROGPU_PCI_BAR0_SIZE_BYTES as u32 - 1) & 0xffff_fff0;
        assert_eq!(dev.bar0, expected_bar0);
        assert_eq!(dev.config_read(0x10, 4), expected_bar0);

        dev.config_write(0x16, 2, 0xdead);
        let expected_bar1 = 0xdead_0000 & !(dev.bar1_size_bytes - 1) & 0xffff_fff0;
        assert_eq!(dev.bar1, expected_bar1);
        assert_eq!(dev.config_read(0x14, 4), expected_bar1 | (1 << 3));
    }

    #[derive(Debug, Default)]
    struct FailOnceBackend {
        completed: Vec<AeroGpuBackendCompletion>,
        failed_once: bool,
    }

    impl AeroGpuCommandBackend for FailOnceBackend {
        fn reset(&mut self) {
            self.completed.clear();
            self.failed_once = false;
        }

        fn submit(
            &mut self,
            _mem: &mut dyn MemoryBus,
            submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            if !self.failed_once {
                self.failed_once = true;
                self.completed.push(AeroGpuBackendCompletion {
                    fence: submission.signal_fence,
                    error: Some("forced backend execution error".into()),
                });
            } else {
                self.completed.push(AeroGpuBackendCompletion {
                    fence: submission.signal_fence,
                    error: None,
                });
            }
            Ok(())
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            self.completed.drain(..).collect()
        }

        fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            None
        }
    }

    #[derive(Default)]
    struct TestErrorBackend {
        completions: VecDeque<AeroGpuBackendCompletion>,
    }

    impl AeroGpuCommandBackend for TestErrorBackend {
        fn reset(&mut self) {
            self.completions.clear();
        }

        fn submit(
            &mut self,
            _mem: &mut dyn MemoryBus,
            _submission: AeroGpuBackendSubmission,
        ) -> Result<(), String> {
            Ok(())
        }

        fn poll_completions(&mut self) -> Vec<AeroGpuBackendCompletion> {
            self.completions.drain(..).collect()
        }

        fn read_scanout_rgba8(&mut self, _scanout_id: u32) -> Option<AeroGpuBackendScanout> {
            None
        }
    }

    #[test]
    fn backend_exec_error_sets_error_irq_and_fence_advances() {
        let mut mem = PhysicalMemoryBus::new(Box::new(DenseMemory::new(0x10000).unwrap()));

        let cfg = AeroGpuDeviceConfig {
            vram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };
        let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
        dev.set_backend(Box::new(FailOnceBackend::default()));

        // Enable PCI MMIO decode and DMA.
        dev.config_write(0x04, 2, (1u32 << 1) | (1u32 << 2));

        // Program IRQ mask (fence + error).
        dev.mmio_write(
            &mut mem,
            mmio::IRQ_ENABLE,
            4,
            irq_bits::FENCE | irq_bits::ERROR,
        );

        // Ring + fence page.
        let ring_gpa: u64 = 0x1000;
        let cmd_gpa: u64 = 0x2000;
        let fence_gpa: u64 = 0x3000;

        // Command stream: [stream header][NOP].
        let stream = AerogpuCmdStreamHeader {
            magic: AEROGPU_CMD_STREAM_MAGIC,
            abi_version: AEROGPU_ABI_VERSION_U32,
            size_bytes: (AerogpuCmdStreamHeader::SIZE_BYTES + AerogpuCmdHdr::SIZE_BYTES) as u32,
            flags: 0,
            reserved0: 0,
            reserved1: 0,
        };
        let nop = AerogpuCmdHdr {
            opcode: AerogpuCmdOpcode::Nop as u32,
            size_bytes: AerogpuCmdHdr::SIZE_BYTES as u32,
        };
        let mut cmd_bytes = Vec::with_capacity(stream.size_bytes as usize);
        cmd_bytes.extend_from_slice(&stream.magic.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.abi_version.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.size_bytes.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.flags.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.reserved0.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.reserved1.to_le_bytes());
        cmd_bytes.extend_from_slice(&nop.opcode.to_le_bytes());
        cmd_bytes.extend_from_slice(&nop.size_bytes.to_le_bytes());
        assert_eq!(cmd_bytes.len(), stream.size_bytes as usize);
        mem.write_physical(cmd_gpa, &cmd_bytes);

        // Ring header: 8 entries, 64-byte stride.
        let entry_count: u32 = 8;
        let entry_stride: u32 = crate::devices::aerogpu_ring::AeroGpuSubmitDesc::SIZE_BYTES;
        let ring_size_bytes: u32 = (crate::devices::aerogpu_ring::AEROGPU_RING_HEADER_SIZE_BYTES
            + u64::from(entry_count) * u64::from(entry_stride))
            as u32;

        let mut ring_hdr =
            [0u8; crate::devices::aerogpu_ring::AeroGpuRingHeader::SIZE_BYTES as usize];
        ring_hdr[0..4]
            .copy_from_slice(&crate::devices::aerogpu_ring::AEROGPU_RING_MAGIC.to_le_bytes());
        ring_hdr[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        ring_hdr[8..12].copy_from_slice(&ring_size_bytes.to_le_bytes());
        ring_hdr[12..16].copy_from_slice(&entry_count.to_le_bytes());
        ring_hdr[16..20].copy_from_slice(&entry_stride.to_le_bytes());
        ring_hdr[20..24].copy_from_slice(&0u32.to_le_bytes()); // flags
        ring_hdr[24..28].copy_from_slice(&0u32.to_le_bytes()); // head
        ring_hdr[28..32].copy_from_slice(&1u32.to_le_bytes()); // tail (1 pending)
                                                               // reserved fields already zero.
        mem.write_physical(ring_gpa, &ring_hdr);

        // Submission descriptor at slot 0.
        let mut desc = [0u8; crate::devices::aerogpu_ring::AeroGpuSubmitDesc::SIZE_BYTES as usize];
        desc[0..4].copy_from_slice(
            &crate::devices::aerogpu_ring::AeroGpuSubmitDesc::SIZE_BYTES.to_le_bytes(),
        );
        desc[4..8].copy_from_slice(&0u32.to_le_bytes()); // flags
        desc[8..12].copy_from_slice(&0u32.to_le_bytes()); // context_id
        desc[12..16].copy_from_slice(&0u32.to_le_bytes()); // engine_id
        desc[16..24].copy_from_slice(&cmd_gpa.to_le_bytes());
        desc[24..28].copy_from_slice(&(cmd_bytes.len() as u32).to_le_bytes());
        desc[28..32].copy_from_slice(&0u32.to_le_bytes()); // cmd_reserved0
        desc[32..40].copy_from_slice(&0u64.to_le_bytes()); // alloc_table_gpa
        desc[40..44].copy_from_slice(&0u32.to_le_bytes()); // alloc_table_size_bytes
        desc[44..48].copy_from_slice(&0u32.to_le_bytes()); // alloc_table_reserved0
        desc[48..56].copy_from_slice(&1u64.to_le_bytes()); // signal_fence
        desc[56..64].copy_from_slice(&0u64.to_le_bytes()); // reserved0

        mem.write_physical(
            ring_gpa + crate::devices::aerogpu_ring::AEROGPU_RING_HEADER_SIZE_BYTES,
            &desc,
        );

        // Program device registers.
        dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
        dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
        dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, 0x1000);
        dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
        dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);
        dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

        // Kick.
        dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
        // Doorbell processing and backend completion are driven by `tick()`.
        dev.tick(&mut mem, 0);
        dev.tick(&mut mem, 0);

        // Fence must still advance even on backend error, and the ERROR IRQ bit must latch.
        assert_eq!(dev.regs.completed_fence, 1);
        assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
        assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
        assert_eq!(dev.regs.error_code, AerogpuErrorCode::Backend as u32);
        assert_eq!(dev.regs.error_fence, 1);
        assert_eq!(dev.regs.error_count, 1);
        assert!(dev.irq_level());

        // Clearing IRQ status does not clear the latched error payload (ABI contract).
        dev.mmio_write(&mut mem, mmio::IRQ_ACK, 4, irq_bits::ERROR);
        assert_eq!(dev.regs.irq_status & irq_bits::ERROR, 0);
        assert_eq!(dev.regs.error_code, AerogpuErrorCode::Backend as u32);
        assert_eq!(dev.regs.error_fence, 1);
        assert_eq!(dev.regs.error_count, 1);
        // Fence IRQ is still pending, so the interrupt line remains asserted.
        assert!(dev.irq_level());
    }

    #[test]
    fn backend_exec_error_with_vsync_present_sets_error_before_fence_and_fence_advances_on_vblank()
    {
        let mut mem = PhysicalMemoryBus::new(Box::new(DenseMemory::new(0x10000).unwrap()));

        let cfg = AeroGpuDeviceConfig {
            vram_size_bytes: 2 * 1024 * 1024,
            ..Default::default()
        };
        let mut dev = AeroGpuPciDevice::new(cfg, 0, 0);
        dev.set_backend(Box::new(FailOnceBackend::default()));

        // Enable PCI MMIO decode and DMA.
        dev.config_write(0x04, 2, (1u32 << 1) | (1u32 << 2));

        // Program IRQ mask (fence + error).
        dev.mmio_write(
            &mut mem,
            mmio::IRQ_ENABLE,
            4,
            irq_bits::FENCE | irq_bits::ERROR,
        );

        // Enable scanout0 so vblank pacing is active (needed for VSYNC presents).
        dev.mmio_write(&mut mem, mmio::SCANOUT0_ENABLE, 4, 1);

        // Ring + fence page.
        let ring_gpa: u64 = 0x1000;
        let cmd_gpa: u64 = 0x2000;
        let fence_gpa: u64 = 0x3000;

        // Command stream: [stream header][PRESENT(vsync)].
        let stream = AerogpuCmdStreamHeader {
            magic: AEROGPU_CMD_STREAM_MAGIC,
            abi_version: AEROGPU_ABI_VERSION_U32,
            size_bytes: (AerogpuCmdStreamHeader::SIZE_BYTES + AerogpuCmdPresent::SIZE_BYTES) as u32,
            flags: 0,
            reserved0: 0,
            reserved1: 0,
        };
        let present = AerogpuCmdPresent {
            hdr: AerogpuCmdHdr {
                opcode: AerogpuCmdOpcode::Present as u32,
                size_bytes: AerogpuCmdPresent::SIZE_BYTES as u32,
            },
            scanout_id: 0,
            flags: AEROGPU_PRESENT_FLAG_VSYNC,
        };

        let mut cmd_bytes = Vec::with_capacity(stream.size_bytes as usize);
        cmd_bytes.extend_from_slice(&stream.magic.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.abi_version.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.size_bytes.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.flags.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.reserved0.to_le_bytes());
        cmd_bytes.extend_from_slice(&stream.reserved1.to_le_bytes());
        cmd_bytes.extend_from_slice(&present.hdr.opcode.to_le_bytes());
        cmd_bytes.extend_from_slice(&present.hdr.size_bytes.to_le_bytes());
        cmd_bytes.extend_from_slice(&present.scanout_id.to_le_bytes());
        cmd_bytes.extend_from_slice(&present.flags.to_le_bytes());
        assert_eq!(cmd_bytes.len(), stream.size_bytes as usize);
        mem.write_physical(cmd_gpa, &cmd_bytes);

        // Ring header: 8 entries, 64-byte stride.
        let entry_count: u32 = 8;
        let entry_stride: u32 = crate::devices::aerogpu_ring::AeroGpuSubmitDesc::SIZE_BYTES;
        let ring_size_bytes: u32 = (crate::devices::aerogpu_ring::AEROGPU_RING_HEADER_SIZE_BYTES
            + u64::from(entry_count) * u64::from(entry_stride))
            as u32;

        let mut ring_hdr =
            [0u8; crate::devices::aerogpu_ring::AeroGpuRingHeader::SIZE_BYTES as usize];
        ring_hdr[0..4]
            .copy_from_slice(&crate::devices::aerogpu_ring::AEROGPU_RING_MAGIC.to_le_bytes());
        ring_hdr[4..8].copy_from_slice(&AEROGPU_ABI_VERSION_U32.to_le_bytes());
        ring_hdr[8..12].copy_from_slice(&ring_size_bytes.to_le_bytes());
        ring_hdr[12..16].copy_from_slice(&entry_count.to_le_bytes());
        ring_hdr[16..20].copy_from_slice(&entry_stride.to_le_bytes());
        ring_hdr[20..24].copy_from_slice(&0u32.to_le_bytes()); // flags
        ring_hdr[24..28].copy_from_slice(&0u32.to_le_bytes()); // head
        ring_hdr[28..32].copy_from_slice(&1u32.to_le_bytes()); // tail (1 pending)
                                                               // reserved fields already zero.
        mem.write_physical(ring_gpa, &ring_hdr);

        // Submission descriptor at slot 0.
        let mut desc = [0u8; crate::devices::aerogpu_ring::AeroGpuSubmitDesc::SIZE_BYTES as usize];
        desc[0..4].copy_from_slice(
            &crate::devices::aerogpu_ring::AeroGpuSubmitDesc::SIZE_BYTES.to_le_bytes(),
        );
        desc[4..8].copy_from_slice(
            &crate::devices::aerogpu_ring::AeroGpuSubmitDesc::FLAG_PRESENT.to_le_bytes(),
        );
        desc[8..12].copy_from_slice(&0u32.to_le_bytes()); // context_id
        desc[12..16].copy_from_slice(&0u32.to_le_bytes()); // engine_id
        desc[16..24].copy_from_slice(&cmd_gpa.to_le_bytes());
        desc[24..28].copy_from_slice(&(cmd_bytes.len() as u32).to_le_bytes());
        desc[28..32].copy_from_slice(&0u32.to_le_bytes()); // cmd_reserved0
        desc[32..40].copy_from_slice(&0u64.to_le_bytes()); // alloc_table_gpa
        desc[40..44].copy_from_slice(&0u32.to_le_bytes()); // alloc_table_size_bytes
        desc[44..48].copy_from_slice(&0u32.to_le_bytes()); // alloc_table_reserved0
        desc[48..56].copy_from_slice(&1u64.to_le_bytes()); // signal_fence
        desc[56..64].copy_from_slice(&0u64.to_le_bytes()); // reserved0

        mem.write_physical(
            ring_gpa + crate::devices::aerogpu_ring::AEROGPU_RING_HEADER_SIZE_BYTES,
            &desc,
        );

        // Program device registers.
        dev.mmio_write(&mut mem, mmio::RING_GPA_LO, 4, ring_gpa as u32);
        dev.mmio_write(&mut mem, mmio::RING_GPA_HI, 4, (ring_gpa >> 32) as u32);
        dev.mmio_write(&mut mem, mmio::RING_SIZE_BYTES, 4, 0x1000);
        dev.mmio_write(&mut mem, mmio::FENCE_GPA_LO, 4, fence_gpa as u32);
        dev.mmio_write(&mut mem, mmio::FENCE_GPA_HI, 4, (fence_gpa >> 32) as u32);
        dev.mmio_write(&mut mem, mmio::RING_CONTROL, 4, ring_control::ENABLE);

        // Kick.
        dev.mmio_write(&mut mem, mmio::DOORBELL, 4, 1);
        // Drive doorbell processing and poll the backend completion so the error IRQ payload
        // becomes visible before the vsync-delayed fence completes.
        dev.tick(&mut mem, 0);
        dev.tick(&mut mem, 0);

        // Backend error should set ERROR before the vsync-delayed fence completes.
        assert_eq!(dev.regs.completed_fence, 0);
        assert_eq!(dev.regs.irq_status & irq_bits::FENCE, 0);
        assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
        assert_eq!(dev.regs.error_code, AerogpuErrorCode::Backend as u32);
        assert_eq!(dev.regs.error_fence, 1);
        assert_eq!(dev.regs.error_count, 1);
        assert!(dev.irq_level());

        // Force a vblank edge and ensure the fence still advances (even though the backend errored).
        let next = dev
            .next_vblank_ns
            .expect("vblank scheduling should be active");
        dev.tick(&mut mem, next);

        assert_eq!(dev.regs.completed_fence, 1);
        assert_ne!(dev.regs.irq_status & irq_bits::FENCE, 0);
        assert_ne!(dev.regs.irq_status & irq_bits::ERROR, 0);
        assert!(dev.irq_level());
    }

    #[test]
    fn mmio_error_info_regs_update_on_backend_error() {
        let ram = Box::new(DenseMemory::new(1024 * 1024).unwrap());
        let mut mem = PhysicalMemoryBus::new(ram);

        let mut dev = AeroGpuPciDevice::new(AeroGpuDeviceConfig::default(), 0, 0);

        // Enable PCI memory space and bus mastering so MMIO reads decode and the device is allowed
        // to poll backend completions.
        dev.config_write(0x04, 2, (1 << 1) | (1 << 2));

        let mut backend = TestErrorBackend::default();
        backend.completions.push_back(AeroGpuBackendCompletion {
            fence: 0x1234_5678_9abc_def0,
            error: Some("boom".to_string()),
        });
        dev.set_backend(Box::new(backend));

        dev.tick(&mut mem, 0);

        let features_lo = dev.mmio_read(&mut mem, mmio::FEATURES_LO, 4) as u64;
        let features_hi = dev.mmio_read(&mut mem, mmio::FEATURES_HI, 4) as u64;
        let features = features_lo | (features_hi << 32);
        assert_ne!(features & FEATURE_ERROR_INFO, 0);

        let irq_status = dev.mmio_read(&mut mem, mmio::IRQ_STATUS, 4);
        assert_ne!(irq_status & irq_bits::ERROR, 0);

        let code = dev.mmio_read(&mut mem, mmio::ERROR_CODE, 4);
        assert_eq!(code, AerogpuErrorCode::Backend as u32);

        let fence_lo = dev.mmio_read(&mut mem, mmio::ERROR_FENCE_LO, 4) as u64;
        let fence_hi = dev.mmio_read(&mut mem, mmio::ERROR_FENCE_HI, 4) as u64;
        let fence = fence_lo | (fence_hi << 32);
        assert_eq!(fence, 0x1234_5678_9abc_def0);

        let count = dev.mmio_read(&mut mem, mmio::ERROR_COUNT, 4);
        assert_eq!(count, 1);
    }
}
