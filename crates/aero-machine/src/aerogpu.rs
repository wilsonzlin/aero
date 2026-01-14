#![forbid(unsafe_code)]

use core::mem::offset_of;

use aero_devices::clock::{Clock, ManualClock};
use aero_devices::pci::{PciBarMmioHandler, PciConfigSpace, PciDevice};
use aero_devices_gpu::ring::write_fence_page;
use aero_protocol::aerogpu::aerogpu_pci as pci;
use aero_protocol::aerogpu::aerogpu_ring as ring;
use memory::MemoryBus;

#[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
use aero_shared::scanout_state::{
    ScanoutStateUpdate, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_WDDM,
};

const RING_HEAD_OFFSET: u64 = offset_of!(ring::AerogpuRingHeader, head) as u64;
const RING_TAIL_OFFSET: u64 = offset_of!(ring::AerogpuRingHeader, tail) as u64;
const RING_HEADER_SIZE_BYTES: u64 = ring::AerogpuRingHeader::SIZE_BYTES as u64;

// -----------------------------------------------------------------------------
// Defensive caps (host readback paths)
// -----------------------------------------------------------------------------
//
// `Machine::display_present` can read scanout and cursor bitmaps directly from guest memory to
// produce a host-visible RGBA framebuffer. These reads are driven by guest-controlled registers,
// so they must not allocate unbounded memory.
//
// Note: these caps apply only to the host-side "readback to Vec<u32>" helpers. The browser runtime
// uses a separate scanout pipeline (shared scanout state + GPU worker) and has its own sizing and
// allocation limits.
const MAX_HOST_SCANOUT_RGBA8888_BYTES: usize = 64 * 1024 * 1024; // 16,777,216 pixels (~4K@32bpp)
const MAX_HOST_CURSOR_RGBA8888_BYTES: usize = 4 * 1024 * 1024; // 1,048,576 pixels (~1024x1024)

fn supported_features() -> u64 {
    pci::AEROGPU_FEATURE_FENCE_PAGE
        | pci::AEROGPU_FEATURE_CURSOR
        | pci::AEROGPU_FEATURE_SCANOUT
        | pci::AEROGPU_FEATURE_VBLANK
        | pci::AEROGPU_FEATURE_ERROR_INFO
}

#[derive(Debug, Clone, Copy)]
pub struct AeroGpuScanout0State {
    pub wddm_scanout_active: bool,
    pub enable: bool,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub pitch_bytes: u32,
    pub fb_gpa: u64,
}

impl AeroGpuScanout0State {
    pub fn read_rgba8888(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u32>> {
        if !self.enable {
            return None;
        }
        if self.width == 0 || self.height == 0 {
            return None;
        }
        if self.fb_gpa == 0 {
            return None;
        }

        let bytes_per_pixel = match self.format {
            x if x == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8X8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8X8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
                || x == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
            {
                4usize
            }
            x if x == pci::AerogpuFormat::B5G6R5Unorm as u32
                || x == pci::AerogpuFormat::B5G5R5A1Unorm as u32 =>
            {
                2usize
            }
            _ => return None,
        };

        let width = usize::try_from(self.width).ok()?;
        let height = usize::try_from(self.height).ok()?;
        let pitch = usize::try_from(self.pitch_bytes).ok()?;
        if pitch == 0 {
            return None;
        }

        let row_bytes = width.checked_mul(bytes_per_pixel)?;
        if pitch < row_bytes {
            return None;
        }

        // Validate GPA arithmetic does not wrap.
        let pitch_u64 = u64::from(self.pitch_bytes);
        let row_bytes_u64 = u64::try_from(row_bytes).ok()?;
        let last_row_gpa = self
            .fb_gpa
            .checked_add((height as u64).checked_sub(1)?.checked_mul(pitch_u64)?)?;
        last_row_gpa.checked_add(row_bytes_u64)?;

        let pixel_count = width.checked_mul(height)?;
        let out_bytes = pixel_count.checked_mul(core::mem::size_of::<u32>())?;
        if out_bytes > MAX_HOST_SCANOUT_RGBA8888_BYTES {
            return None;
        }

        let mut out = vec![0u32; pixel_count];
        let mut row = vec![0u8; row_bytes];

        for y in 0..height {
            let row_gpa = self.fb_gpa + (y as u64) * pitch_u64;
            mem.read_physical(row_gpa, &mut row);

            let dst_row = &mut out[y * width..(y + 1) * width];

            match self.format {
                x if x == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                    || x == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        let b = src[0];
                        let g = src[1];
                        let r = src[2];
                        let a = src[3];
                        *dst = u32::from_le_bytes([r, g, b, a]);
                    }
                }
                x if x == pci::AerogpuFormat::B8G8R8X8Unorm as u32
                    || x == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        let b = src[0];
                        let g = src[1];
                        let r = src[2];
                        *dst = u32::from_le_bytes([r, g, b, 0xFF]);
                    }
                }
                x if x == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                    || x == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        *dst = u32::from_le_bytes([src[0], src[1], src[2], src[3]]);
                    }
                }
                x if x == pci::AerogpuFormat::R8G8B8X8Unorm as u32
                    || x == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        *dst = u32::from_le_bytes([src[0], src[1], src[2], 0xFF]);
                    }
                }
                x if x == pci::AerogpuFormat::B5G6R5Unorm as u32 => {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(2)) {
                        let pix = u16::from_le_bytes([src[0], src[1]]);
                        let b = (pix & 0x1f) as u8;
                        let g = ((pix >> 5) & 0x3f) as u8;
                        let r = ((pix >> 11) & 0x1f) as u8;
                        let r8 = (r << 3) | (r >> 2);
                        let g8 = (g << 2) | (g >> 4);
                        let b8 = (b << 3) | (b >> 2);
                        *dst = u32::from_le_bytes([r8, g8, b8, 0xFF]);
                    }
                }
                x if x == pci::AerogpuFormat::B5G5R5A1Unorm as u32 => {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(2)) {
                        let pix = u16::from_le_bytes([src[0], src[1]]);
                        let b = (pix & 0x1f) as u8;
                        let g = ((pix >> 5) & 0x1f) as u8;
                        let r = ((pix >> 10) & 0x1f) as u8;
                        let a = ((pix >> 15) & 0x1) as u8;
                        let r8 = (r << 3) | (r >> 2);
                        let g8 = (g << 3) | (g >> 2);
                        let b8 = (b << 3) | (b >> 2);
                        *dst = u32::from_le_bytes([r8, g8, b8, if a != 0 { 0xFF } else { 0 }]);
                    }
                }
                _ => return None,
            }
        }

        Some(out)
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct AeroGpuCursorConfig {
    pub enable: bool,
    pub x: i32,
    pub y: i32,
    pub hot_x: u32,
    pub hot_y: u32,
    pub width: u32,
    pub height: u32,
    pub format: u32,
    pub fb_gpa: u64,
    pub pitch_bytes: u32,
}

impl AeroGpuCursorConfig {
    pub(crate) fn read_rgba8888(&self, mem: &mut dyn MemoryBus) -> Option<Vec<u32>> {
        if !self.enable {
            return None;
        }
        if self.width == 0 || self.height == 0 {
            return None;
        }
        if self.fb_gpa == 0 {
            return None;
        }

        // MVP: only accept 32bpp cursor formats.
        let is_32bpp = matches!(
            self.format,
            x if x == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8X8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8X8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
                || x == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32
        );
        if !is_32bpp {
            return None;
        }

        let width = usize::try_from(self.width).ok()?;
        let height = usize::try_from(self.height).ok()?;
        let pitch = usize::try_from(self.pitch_bytes).ok()?;
        if pitch == 0 {
            return None;
        }

        let row_bytes = width.checked_mul(4)?;
        if pitch < row_bytes {
            return None;
        }

        // Validate GPA arithmetic does not wrap.
        let pitch_u64 = u64::from(self.pitch_bytes);
        let row_bytes_u64 = u64::try_from(row_bytes).ok()?;
        let last_row_gpa = self
            .fb_gpa
            .checked_add((height as u64).checked_sub(1)?.checked_mul(pitch_u64)?)?;
        last_row_gpa.checked_add(row_bytes_u64)?;

        let pixel_count = width.checked_mul(height)?;
        let out_bytes = pixel_count.checked_mul(core::mem::size_of::<u32>())?;
        if out_bytes > MAX_HOST_CURSOR_RGBA8888_BYTES {
            return None;
        }

        let mut out = vec![0u32; pixel_count];
        let mut row = vec![0u8; row_bytes];

        for y in 0..height {
            let row_gpa = self.fb_gpa + (y as u64) * pitch_u64;
            mem.read_physical(row_gpa, &mut row);
            let dst_row = &mut out[y * width..(y + 1) * width];

            match self.format {
                x if x == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                    || x == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        let b = src[0];
                        let g = src[1];
                        let r = src[2];
                        let a = src[3];
                        *dst = u32::from_le_bytes([r, g, b, a]);
                    }
                }
                x if x == pci::AerogpuFormat::B8G8R8X8Unorm as u32
                    || x == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        let b = src[0];
                        let g = src[1];
                        let r = src[2];
                        *dst = u32::from_le_bytes([r, g, b, 0xFF]);
                    }
                }
                x if x == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                    || x == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        *dst = u32::from_le_bytes([src[0], src[1], src[2], src[3]]);
                    }
                }
                x if x == pci::AerogpuFormat::R8G8B8X8Unorm as u32
                    || x == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32 =>
                {
                    for (dst, src) in dst_row.iter_mut().zip(row.chunks_exact(4)) {
                        *dst = u32::from_le_bytes([src[0], src[1], src[2], 0xFF]);
                    }
                }
                _ => return None,
            }
        }

        Some(out)
    }
}

pub(crate) fn composite_cursor_rgba8888_over_scanout(
    scanout: &mut [u32],
    scanout_width: u32,
    scanout_height: u32,
    cursor_cfg: &AeroGpuCursorConfig,
    cursor: &[u32],
) {
    if !cursor_cfg.enable {
        return;
    }
    let Some(sw) = usize::try_from(scanout_width).ok() else {
        return;
    };
    let Some(sh) = usize::try_from(scanout_height).ok() else {
        return;
    };
    if scanout.len() < sw.saturating_mul(sh) {
        return;
    }

    let Ok(cw) = usize::try_from(cursor_cfg.width) else {
        return;
    };
    let Ok(ch) = usize::try_from(cursor_cfg.height) else {
        return;
    };
    if cw == 0 || ch == 0 {
        return;
    }
    if cursor.len() < cw.saturating_mul(ch) {
        return;
    }

    let left = cursor_cfg.x.saturating_sub(cursor_cfg.hot_x as i32);
    let top = cursor_cfg.y.saturating_sub(cursor_cfg.hot_y as i32);

    for cy in 0..ch {
        let dst_y = top.saturating_add(cy as i32);
        if dst_y < 0 || dst_y >= scanout_height as i32 {
            continue;
        }
        for cx in 0..cw {
            let dst_x = left.saturating_add(cx as i32);
            if dst_x < 0 || dst_x >= scanout_width as i32 {
                continue;
            }
            let src = cursor[cy * cw + cx];
            let src_bytes = src.to_le_bytes();
            let sa = src_bytes[3] as u32;
            if sa == 0 {
                continue;
            }

            let dst_index = dst_y as usize * sw + dst_x as usize;
            let dst = scanout[dst_index];
            if sa == 0xFF {
                scanout[dst_index] = src;
                continue;
            }

            let dst_bytes = dst.to_le_bytes();
            let inv = 255u32 - sa;
            let blend = |s: u8, d: u8| -> u8 {
                let v = u32::from(s) * sa + u32::from(d) * inv;
                ((v + 127) / 255) as u8
            };
            let r = blend(src_bytes[0], dst_bytes[0]);
            let g = blend(src_bytes[1], dst_bytes[1]);
            let b = blend(src_bytes[2], dst_bytes[2]);
            scanout[dst_index] = u32::from_le_bytes([r, g, b, 0xFF]);
        }
    }
}
pub struct AeroGpuMmioDevice {
    /// Internal PCI config image used by the device model for COMMAND/BAR gating.
    ///
    /// The canonical PCI config space is owned by the machine (via `SharedPciConfigPorts`), so this
    /// config must be explicitly synchronized from the platform before ticking / IRQ polling.
    config: PciConfigSpace,
    abi_version: u32,
    supported_features: u64,

    clock: Option<ManualClock>,

    ring_gpa: u64,
    ring_size_bytes: u32,
    ring_control: u32,

    fence_gpa: u64,
    completed_fence: u64,

    irq_status: u32,
    irq_enable: u32,

    // ---------------------------------------------------------------------
    // Error reporting (ABI 1.3+)
    // ---------------------------------------------------------------------
    //
    // These mirror the optional MMIO error registers in `drivers/aerogpu/protocol/aerogpu_pci.h`.
    //
    // Clearing `IRQ_STATUS.ERROR` via `IRQ_ACK` must *not* clear the latched payload; it remains
    // valid until overwritten by a subsequent error (or until the device is reset).
    error_code: u32,
    error_fence: u64,
    error_count: u32,

    scanout0_enable: bool,
    scanout0_width: u32,
    scanout0_height: u32,
    scanout0_format: u32,
    scanout0_pitch_bytes: u32,
    scanout0_fb_gpa: u64,
    /// Pending LO dword for `SCANOUT0_FB_GPA` while waiting for the HI write commit.
    scanout0_fb_gpa_pending_lo: u32,
    scanout0_fb_gpa_lo_pending: bool,
    scanout0_vblank_seq: u64,
    scanout0_vblank_time_ns: u64,
    scanout0_vblank_period_ns: u32,
    vblank_interval_ns: Option<u64>,
    next_vblank_ns: Option<u64>,
    wddm_scanout_active: bool,
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    scanout0_dirty: bool,

    cursor_enable: bool,
    cursor_x: i32,
    cursor_y: i32,
    cursor_hot_x: u32,
    cursor_hot_y: u32,
    cursor_width: u32,
    cursor_height: u32,
    cursor_format: u32,
    cursor_fb_gpa: u64,
    cursor_pitch_bytes: u32,

    doorbell_pending: bool,
    ring_reset_pending: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AeroGpuMmioSnapshotV1 {
    pub abi_version: u32,
    pub features: u64,

    pub ring_gpa: u64,
    pub ring_size_bytes: u32,
    pub ring_control: u32,

    pub fence_gpa: u64,
    pub completed_fence: u64,

    pub irq_status: u32,
    pub irq_enable: u32,

    pub error_code: u32,
    pub error_fence: u64,
    pub error_count: u32,

    pub scanout0_enable: u32,
    pub scanout0_width: u32,
    pub scanout0_height: u32,
    pub scanout0_format: u32,
    pub scanout0_pitch_bytes: u32,
    pub scanout0_fb_gpa: u64,
    pub scanout0_vblank_seq: u64,
    pub scanout0_vblank_time_ns: u64,
    pub scanout0_vblank_period_ns: u32,

    pub cursor_enable: u32,
    pub cursor_x: u32,
    pub cursor_y: u32,
    pub cursor_hot_x: u32,
    pub cursor_hot_y: u32,
    pub cursor_width: u32,
    pub cursor_height: u32,
    pub cursor_format: u32,
    pub cursor_fb_gpa: u64,
    pub cursor_pitch_bytes: u32,

    /// Host-only latch tracking whether the guest has claimed WDDM scanout ownership.
    pub wddm_scanout_active: bool,
}

impl Default for AeroGpuMmioDevice {
    fn default() -> Self {
        // Default vblank pacing is 60Hz.
        let vblank_period_ns = 1_000_000_000u64.div_ceil(60);
        let scanout0_vblank_period_ns = vblank_period_ns.min(u64::from(u32::MAX)) as u32;

        let mut config = aero_devices::pci::profile::AEROGPU.build_config_space();
        // Start with decoding disabled; the canonical PCI config space (owned by `Machine`) will be
        // mirrored into this internal copy from `Machine::sync_pci_intx_sources_to_interrupts`.
        config.set_command(0);

        Self {
            config,
            abi_version: pci::AEROGPU_ABI_VERSION_U32,
            supported_features: supported_features(),

            clock: None,

            ring_gpa: 0,
            ring_size_bytes: 0,
            ring_control: 0,

            fence_gpa: 0,
            completed_fence: 0,

            irq_status: 0,
            irq_enable: 0,
            error_code: pci::AerogpuErrorCode::None as u32,
            error_fence: 0,
            error_count: 0,

            scanout0_enable: false,
            scanout0_width: 0,
            scanout0_height: 0,
            scanout0_format: 0,
            scanout0_pitch_bytes: 0,
            scanout0_fb_gpa: 0,
            scanout0_fb_gpa_pending_lo: 0,
            scanout0_fb_gpa_lo_pending: false,
            scanout0_vblank_seq: 0,
            scanout0_vblank_time_ns: 0,
            scanout0_vblank_period_ns,
            vblank_interval_ns: Some(vblank_period_ns),
            next_vblank_ns: None,
            wddm_scanout_active: false,
            #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
            scanout0_dirty: false,

            cursor_enable: false,
            cursor_x: 0,
            cursor_y: 0,
            cursor_hot_x: 0,
            cursor_hot_y: 0,
            cursor_width: 0,
            cursor_height: 0,
            cursor_format: 0,
            cursor_fb_gpa: 0,
            cursor_pitch_bytes: 0,

            doorbell_pending: false,
            ring_reset_pending: false,
        }
    }
}

impl AeroGpuMmioDevice {
    pub fn set_clock(&mut self, clock: ManualClock) {
        self.clock = Some(clock);
    }

    pub fn reset(&mut self) {
        let supported_features = self.supported_features;
        let abi_version = self.abi_version;
        let clock = self.clock.clone();
        *self = Self {
            supported_features,
            abi_version,
            clock,
            ..Default::default()
        };
    }

    fn record_error(&mut self, code: pci::AerogpuErrorCode, fence: u64) {
        self.error_code = code as u32;
        self.error_fence = fence;
        self.error_count = self.error_count.saturating_add(1);
        self.irq_status |= pci::AEROGPU_IRQ_ERROR;
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

    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    fn scanout0_disabled_update() -> ScanoutStateUpdate {
        ScanoutStateUpdate {
            source: SCANOUT_SOURCE_WDDM,
            base_paddr_lo: 0,
            base_paddr_hi: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            // Keep format at a stable default even while disabled.
            format: SCANOUT_FORMAT_B8G8R8X8,
        }
    }

    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    fn scanout_state_format_from_aerogpu_format(fmt: u32) -> Option<u32> {
        // The scanout state format stores the AeroGPU `AerogpuFormat` discriminant values.
        //
        // Only accept the subset of formats that the shared scanout consumer can interpret; if we
        // publish an unsupported format value to the shared state, the browser runtime might treat
        // an unknown pixel layout as RGBA and mis-present memory.
        match fmt {
            x if x == pci::AerogpuFormat::B8G8R8X8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
                || x == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8X8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32 => Some(x),
            _ => None,
        }
    }

    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    fn scanout0_to_scanout_state_update(&self) -> ScanoutStateUpdate {
        let Some(format) = Self::scanout_state_format_from_aerogpu_format(self.scanout0_format)
        else {
            return Self::scanout0_disabled_update();
        };

        let width = self.scanout0_width;
        let height = self.scanout0_height;
        if width == 0 || height == 0 {
            return Self::scanout0_disabled_update();
        }

        let fb_gpa = self.scanout0_fb_gpa;
        if fb_gpa == 0 {
            return Self::scanout0_disabled_update();
        }

        // The shared scanout descriptor assumes 4 bytes per pixel (32bpp). Enforce that assumption
        // here so consumers don't misinterpret memory.
        let bytes_per_pixel = 4u64;

        let Some(row_bytes) = u64::from(width).checked_mul(bytes_per_pixel) else {
            return Self::scanout0_disabled_update();
        };
        let pitch = u64::from(self.scanout0_pitch_bytes);
        if pitch < row_bytes {
            return Self::scanout0_disabled_update();
        }
        if pitch % bytes_per_pixel != 0 {
            // Scanout consumers treat the pitch as a byte stride for `bytes_per_pixel`-sized pixels.
            // If it's not a multiple of the pixel size, row starts would land mid-pixel.
            return Self::scanout0_disabled_update();
        }

        // Ensure `fb_gpa + (height-1)*pitch + row_bytes` does not overflow.
        let Some(last_row_offset) = u64::from(height)
            .checked_sub(1)
            .and_then(|rows| rows.checked_mul(pitch))
        else {
            return Self::scanout0_disabled_update();
        };
        let Some(end_offset) = last_row_offset.checked_add(row_bytes) else {
            return Self::scanout0_disabled_update();
        };
        if fb_gpa.checked_add(end_offset).is_none() {
            return Self::scanout0_disabled_update();
        }

        ScanoutStateUpdate {
            source: SCANOUT_SOURCE_WDDM,
            base_paddr_lo: fb_gpa as u32,
            base_paddr_hi: (fb_gpa >> 32) as u32,
            width,
            height,
            pitch_bytes: self.scanout0_pitch_bytes,
            format,
        }
    }

    fn scanout0_config_is_valid_for_wddm(&self) -> bool {
        if !self.scanout0_enable {
            return false;
        }
        if self.scanout0_width == 0 || self.scanout0_height == 0 {
            return false;
        }
        if self.scanout0_fb_gpa == 0 {
            return false;
        }
        if self.scanout0_fb_gpa_lo_pending {
            // Drivers typically update 64-bit framebuffer addresses by writing LO then HI.
            // Avoid claiming the WDDM scanout while the update is torn so hosts never observe a
            // transient, incorrect base address (especially for scanouts above 4GiB).
            return false;
        }

        // WDDM scanout is currently limited to 32-bit pixel formats that the host scanout/present
        // paths can render deterministically.
        //
        // Note: the shared scanout descriptor (`ScanoutStateUpdate`) is still more restrictive
        // than the full AeroGPU format set; it only supports the packed 32bpp formats that scanout
        // consumers can render deterministically. Keep this validation aligned with what the
        // machine can actually render via `AeroGpuScanout0State::read_rgba8888`.
        match self.scanout0_format {
            x if x == pci::AerogpuFormat::B8G8R8X8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8A8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8X8Unorm as u32
                || x == pci::AerogpuFormat::R8G8B8A8Unorm as u32
                || x == pci::AerogpuFormat::B8G8R8X8UnormSrgb as u32
                || x == pci::AerogpuFormat::B8G8R8A8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8X8UnormSrgb as u32
                || x == pci::AerogpuFormat::R8G8B8A8UnormSrgb as u32 => {}
            _ => return false,
        }

        // Today we only accept 32-bit pixel formats.
        let bytes_per_pixel = 4u64;
        let Some(row_bytes) = u64::from(self.scanout0_width).checked_mul(bytes_per_pixel) else {
            return false;
        };
        let pitch = u64::from(self.scanout0_pitch_bytes);
        pitch >= row_bytes
    }

    fn maybe_claim_wddm_scanout(&mut self) {
        if self.wddm_scanout_active {
            return;
        }
        if !self.scanout0_config_is_valid_for_wddm() {
            return;
        }

        // Sticky handoff: once a valid WDDM scanout config has been programmed, claim scanout even
        // if `SCANOUT0_ENABLE` was already 1 (Win7 KMD init sequence).
        self.wddm_scanout_active = true;

        // Mark dirty so scanout consumers see the transition immediately.
        #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
        {
            self.scanout0_dirty = true;
        }
    }

    /// Consume any pending scanout0 register updates and produce a new shared scanout descriptor.
    ///
    /// Returns `None` when:
    /// - the scanout registers have not changed since the last call, or
    /// - the scanout has never been enabled (so legacy scanout should remain authoritative).
    ///
    /// Returns a disabled descriptor (base/width/height/pitch = 0) when the scanout is enabled but
    /// invalid/unsupported (including unsupported formats).
    #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
    pub fn take_scanout0_state_update(&mut self) -> Option<ScanoutStateUpdate> {
        if !self.scanout0_dirty {
            return None;
        }

        // Avoid publishing a torn 64-bit `fb_gpa` update (drivers typically write LO then HI).
        if self.scanout0_fb_gpa_lo_pending {
            return None;
        }
        self.scanout0_dirty = false;

        // Do not publish WDDM scanout state until scanout has been *claimed* by a valid config.
        // This prevents premature handoff when the guest enables scanout with `FB_GPA=0` during
        // early initialization.
        if !self.wddm_scanout_active {
            return None;
        }

        if !self.scanout0_enable {
            return Some(Self::scanout0_disabled_update());
        }
        Some(self.scanout0_to_scanout_state_update())
    }

    pub fn supported_features(&self) -> u64 {
        self.supported_features
    }

    pub fn irq_level(&self) -> bool {
        // Respect PCI COMMAND.INTX_DISABLE (bit 10).
        if self.intx_disabled() {
            return false;
        }
        (self.irq_status & self.irq_enable) != 0
    }

    pub fn scanout0_state(&self) -> AeroGpuScanout0State {
        AeroGpuScanout0State {
            wddm_scanout_active: self.wddm_scanout_active,
            enable: self.scanout0_enable,
            width: self.scanout0_width,
            height: self.scanout0_height,
            format: self.scanout0_format,
            pitch_bytes: self.scanout0_pitch_bytes,
            fb_gpa: self.scanout0_fb_gpa,
        }
    }

    pub(crate) fn snapshot_v1(&self) -> AeroGpuMmioSnapshotV1 {
        AeroGpuMmioSnapshotV1 {
            abi_version: self.abi_version,
            features: self.supported_features,

            ring_gpa: self.ring_gpa,
            ring_size_bytes: self.ring_size_bytes,
            ring_control: self.ring_control,

            fence_gpa: self.fence_gpa,
            completed_fence: self.completed_fence,

            irq_status: self.irq_status,
            irq_enable: self.irq_enable,

            error_code: self.error_code,
            error_fence: self.error_fence,
            error_count: self.error_count,

            scanout0_enable: self.scanout0_enable as u32,
            scanout0_width: self.scanout0_width,
            scanout0_height: self.scanout0_height,
            scanout0_format: self.scanout0_format,
            scanout0_pitch_bytes: self.scanout0_pitch_bytes,
            scanout0_fb_gpa: self.scanout0_fb_gpa,
            scanout0_vblank_seq: self.scanout0_vblank_seq,
            scanout0_vblank_time_ns: self.scanout0_vblank_time_ns,
            scanout0_vblank_period_ns: self.scanout0_vblank_period_ns,

            cursor_enable: self.cursor_enable as u32,
            cursor_x: self.cursor_x as u32,
            cursor_y: self.cursor_y as u32,
            cursor_hot_x: self.cursor_hot_x,
            cursor_hot_y: self.cursor_hot_y,
            cursor_width: self.cursor_width,
            cursor_height: self.cursor_height,
            cursor_format: self.cursor_format,
            cursor_fb_gpa: self.cursor_fb_gpa,
            cursor_pitch_bytes: self.cursor_pitch_bytes,

            wddm_scanout_active: self.wddm_scanout_active,
        }
    }

    pub(crate) fn restore_snapshot_v1(&mut self, snap: &AeroGpuMmioSnapshotV1) {
        self.abi_version = snap.abi_version;
        // `features` is a device capability bitmask and must reflect the current build's
        // implementation rather than guest-controlled state. Keep the device's
        // `supported_features` unchanged across snapshot restore.

        self.ring_gpa = snap.ring_gpa;
        self.ring_size_bytes = snap.ring_size_bytes;
        // RESET is write-only; only restore the ENABLE bit.
        self.ring_control = snap.ring_control & pci::AEROGPU_RING_CONTROL_ENABLE;

        self.fence_gpa = snap.fence_gpa;
        self.completed_fence = snap.completed_fence;

        self.irq_status = snap.irq_status;
        self.irq_enable = snap.irq_enable;

        self.error_code = snap.error_code;
        self.error_fence = snap.error_fence;
        self.error_count = snap.error_count;

        self.scanout0_enable = snap.scanout0_enable != 0;
        self.scanout0_width = snap.scanout0_width;
        self.scanout0_height = snap.scanout0_height;
        self.scanout0_format = snap.scanout0_format;
        self.scanout0_pitch_bytes = snap.scanout0_pitch_bytes;
        self.scanout0_fb_gpa = snap.scanout0_fb_gpa;
        self.scanout0_vblank_seq = snap.scanout0_vblank_seq;
        self.scanout0_vblank_time_ns = snap.scanout0_vblank_time_ns;
        self.scanout0_vblank_period_ns = snap.scanout0_vblank_period_ns;

        // `scanout0_vblank_period_ns` is the guest-visible register, but the device schedules using
        // the internal u64 `vblank_interval_ns`. This keeps snapshot restore deterministic even if
        // the period field is clamped.
        self.vblank_interval_ns = if snap.scanout0_vblank_period_ns == 0 {
            None
        } else {
            Some(u64::from(snap.scanout0_vblank_period_ns))
        };
        // Host timebase is not snapshotted; restart vblank scheduling from the next tick.
        self.next_vblank_ns = None;

        self.wddm_scanout_active = snap.wddm_scanout_active;

        self.cursor_enable = snap.cursor_enable != 0;
        self.cursor_x = snap.cursor_x as i32;
        self.cursor_y = snap.cursor_y as i32;
        self.cursor_hot_x = snap.cursor_hot_x;
        self.cursor_hot_y = snap.cursor_hot_y;
        self.cursor_width = snap.cursor_width;
        self.cursor_height = snap.cursor_height;
        self.cursor_format = snap.cursor_format;
        self.cursor_fb_gpa = snap.cursor_fb_gpa;
        self.cursor_pitch_bytes = snap.cursor_pitch_bytes;

        // Snapshot v1 does not preserve these internal execution latches.
        self.doorbell_pending = false;
        self.ring_reset_pending = false;
    }

    pub fn tick_vblank(&mut self, now_ns: u64) {
        let Some(interval_ns) = self.vblank_interval_ns else {
            return;
        };

        // When scanout is disabled, stop vblank scheduling and clear any pending vblank IRQ.
        if !self.scanout0_enable {
            self.next_vblank_ns = None;
            self.irq_status &= !pci::AEROGPU_IRQ_SCANOUT_VBLANK;
            return;
        }

        let mut next = self
            .next_vblank_ns
            .unwrap_or(now_ns.saturating_add(interval_ns));
        if now_ns < next {
            self.next_vblank_ns = Some(next);
            return;
        }

        let mut ticks = 0u32;
        while now_ns >= next {
            self.scanout0_vblank_seq = self.scanout0_vblank_seq.wrapping_add(1);
            self.scanout0_vblank_time_ns = next;

            // Only latch the vblank IRQ status bit while the guest has it enabled.
            // This prevents an immediate "stale" interrupt on re-enable.
            if (self.irq_enable & pci::AEROGPU_IRQ_SCANOUT_VBLANK) != 0 {
                self.irq_status |= pci::AEROGPU_IRQ_SCANOUT_VBLANK;
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
    }

    pub(crate) fn cursor_snapshot(&self) -> AeroGpuCursorConfig {
        AeroGpuCursorConfig {
            enable: self.cursor_enable,
            x: self.cursor_x,
            y: self.cursor_y,
            hot_x: self.cursor_hot_x,
            hot_y: self.cursor_hot_y,
            width: self.cursor_width,
            height: self.cursor_height,
            format: self.cursor_format,
            fb_gpa: self.cursor_fb_gpa,
            pitch_bytes: self.cursor_pitch_bytes,
        }
    }

    pub fn process(&mut self, mem: &mut dyn MemoryBus) {
        // Preserve the emulator device model ordering: keep the vblank clock caught up to "now"
        // before processing newly-submitted work, so vsync pacing can't complete on an already-
        // elapsed vblank edge.
        if let Some(clock) = &self.clock {
            self.tick_vblank(clock.now_ns());
        }

        let dma_enabled = self.bus_master_enabled();

        // Ring control RESET is an MMIO write-side effect, but touching the ring header requires
        // DMA; perform the actual memory update from the machine's device tick path when bus
        // mastering is enabled.
        if self.ring_reset_pending {
            self.ring_reset_pending = false;
            self.doorbell_pending = false;

            self.completed_fence = 0;
            self.irq_status = 0;
            self.error_code = pci::AerogpuErrorCode::None as u32;
            self.error_fence = 0;
            self.error_count = 0;

            if dma_enabled && self.ring_gpa != 0 {
                let tail = mem.read_u32(self.ring_gpa + RING_TAIL_OFFSET);
                mem.write_u32(self.ring_gpa + RING_HEAD_OFFSET, tail);
            }

            if dma_enabled
                && self.fence_gpa != 0
                && (self.supported_features() & pci::AEROGPU_FEATURE_FENCE_PAGE) != 0
            {
                write_fence_page(mem, self.fence_gpa, self.abi_version, self.completed_fence);
            }
        }

        if !dma_enabled {
            // Ring DMA is gated by PCI COMMAND.BME.
            return;
        }

        if !self.doorbell_pending {
            return;
        }
        self.doorbell_pending = false;

        if (self.ring_control & pci::AEROGPU_RING_CONTROL_ENABLE) == 0 {
            return;
        }
        if self.ring_gpa == 0 || self.ring_size_bytes == 0 {
            self.record_error(pci::AerogpuErrorCode::CmdDecode, 0);
            return;
        }

        let mut hdr_buf = [0u8; ring::AerogpuRingHeader::SIZE_BYTES];
        mem.read_physical(self.ring_gpa, &mut hdr_buf);
        let Ok(ring_hdr) = ring::AerogpuRingHeader::decode_from_le_bytes(&hdr_buf) else {
            self.record_error(pci::AerogpuErrorCode::CmdDecode, 0);
            return;
        };
        if ring_hdr.validate_prefix().is_err() {
            self.record_error(pci::AerogpuErrorCode::CmdDecode, 0);
            return;
        }

        // The guest-declared ring size must not exceed the MMIO-programmed ring mapping size. The
        // mapping may be larger due to page rounding / extension space.
        if u64::from(ring_hdr.size_bytes) > u64::from(self.ring_size_bytes) {
            self.record_error(pci::AerogpuErrorCode::Oob, 0);
            return;
        }

        let mut head = ring_hdr.head;
        let tail = ring_hdr.tail;
        let pending = tail.wrapping_sub(head);
        if pending == 0 {
            return;
        }

        if pending > ring_hdr.entry_count {
            // Driver and device are out of sync; drop all pending work to avoid looping forever.
            mem.write_u32(self.ring_gpa + RING_HEAD_OFFSET, tail);
            self.record_error(pci::AerogpuErrorCode::CmdDecode, 0);
            return;
        }

        let mut processed = 0u32;
        let max = ring_hdr.entry_count.min(pending);

        while head != tail && processed < max {
            // entry_count is validated as a power-of-two.
            let slot = head & (ring_hdr.entry_count - 1);
            let desc_gpa = self.ring_gpa
                + RING_HEADER_SIZE_BYTES
                + (u64::from(slot) * u64::from(ring_hdr.entry_stride_bytes));

            let mut desc_buf = [0u8; ring::AerogpuSubmitDesc::SIZE_BYTES];
            mem.read_physical(desc_gpa, &mut desc_buf);
            if let Ok(desc) = ring::AerogpuSubmitDesc::decode_from_le_bytes(&desc_buf) {
                // Treat the command stream as a no-op for now. The goal is transport + fence
                // completion so the Win7 KMD doesn't deadlock.
                if desc.validate_prefix().is_err() {
                    self.record_error(pci::AerogpuErrorCode::CmdDecode, desc.signal_fence);
                }
                if desc.signal_fence != 0 && desc.signal_fence > self.completed_fence {
                    self.completed_fence = desc.signal_fence;

                    let wants_irq = (desc.flags & ring::AEROGPU_SUBMIT_FLAG_NO_IRQ) == 0;
                    if wants_irq && (self.irq_enable & pci::AEROGPU_IRQ_FENCE) != 0 {
                        self.irq_status |= pci::AEROGPU_IRQ_FENCE;
                    }
                }
            } else {
                self.record_error(pci::AerogpuErrorCode::CmdDecode, 0);
            }

            head = head.wrapping_add(1);
            processed += 1;
        }

        // Publish the new head after processing submissions.
        mem.write_u32(self.ring_gpa + RING_HEAD_OFFSET, head);

        if self.fence_gpa != 0 && (self.supported_features() & pci::AEROGPU_FEATURE_FENCE_PAGE) != 0
        {
            write_fence_page(mem, self.fence_gpa, self.abi_version, self.completed_fence);
        }
    }

    fn mmio_read_dword(&self, offset: u64) -> u32 {
        match offset {
            x if x == pci::AEROGPU_MMIO_REG_MAGIC as u64 => pci::AEROGPU_MMIO_MAGIC,
            x if x == pci::AEROGPU_MMIO_REG_ABI_VERSION as u64 => self.abi_version,
            x if x == pci::AEROGPU_MMIO_REG_FEATURES_LO as u64 => self.supported_features() as u32,
            x if x == pci::AEROGPU_MMIO_REG_FEATURES_HI as u64 => {
                (self.supported_features() >> 32) as u32
            }

            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_LO as u64 => self.ring_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_HI as u64 => (self.ring_gpa >> 32) as u32,
            x if x == pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES as u64 => self.ring_size_bytes,
            x if x == pci::AEROGPU_MMIO_REG_RING_CONTROL as u64 => self.ring_control,

            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_LO as u64 => self.fence_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_HI as u64 => (self.fence_gpa >> 32) as u32,

            x if x == pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_LO as u64 => {
                self.completed_fence as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_COMPLETED_FENCE_HI as u64 => {
                (self.completed_fence >> 32) as u32
            }

            x if x == pci::AEROGPU_MMIO_REG_IRQ_STATUS as u64 => self.irq_status,
            x if x == pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64 => self.irq_enable,

            x if x == pci::AEROGPU_MMIO_REG_ERROR_CODE as u64 => self.error_code,
            x if x == pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO as u64 => self.error_fence as u32,
            x if x == pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI as u64 => {
                (self.error_fence >> 32) as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_ERROR_COUNT as u64 => self.error_count,

            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64 => self.scanout0_enable as u32,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64 => self.scanout0_width,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64 => self.scanout0_height,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64 => self.scanout0_format,
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64 => {
                self.scanout0_pitch_bytes
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64 => {
                // Expose the pending LO value while keeping `scanout0_fb_gpa` stable to avoid
                // consumers observing a torn 64-bit address mid-update.
                if self.scanout0_fb_gpa_lo_pending {
                    self.scanout0_fb_gpa_pending_lo
                } else {
                    self.scanout0_fb_gpa as u32
                }
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64 => {
                (self.scanout0_fb_gpa >> 32) as u32
            }

            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_LO as u64 => {
                self.scanout0_vblank_seq as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_SEQ_HI as u64 => {
                (self.scanout0_vblank_seq >> 32) as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_LO as u64 => {
                self.scanout0_vblank_time_ns as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_TIME_NS_HI as u64 => {
                (self.scanout0_vblank_time_ns >> 32) as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_VBLANK_PERIOD_NS as u64 => {
                self.scanout0_vblank_period_ns
            }

            x if x == pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64 => self.cursor_enable as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_X as u64 => self.cursor_x as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_Y as u64 => self.cursor_y as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64 => self.cursor_hot_x,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64 => self.cursor_hot_y,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64 => self.cursor_width,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64 => self.cursor_height,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64 => self.cursor_format,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64 => self.cursor_fb_gpa as u32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64 => {
                (self.cursor_fb_gpa >> 32) as u32
            }
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64 => self.cursor_pitch_bytes,

            _ => 0,
        }
    }

    fn mmio_write_dword(&mut self, offset: u64, value: u32) {
        match offset {
            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_LO as u64 => {
                self.ring_gpa = (self.ring_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_RING_GPA_HI as u64 => {
                self.ring_gpa = (self.ring_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            x if x == pci::AEROGPU_MMIO_REG_RING_SIZE_BYTES as u64 => {
                self.ring_size_bytes = value;
            }
            x if x == pci::AEROGPU_MMIO_REG_RING_CONTROL as u64 => {
                if (value & pci::AEROGPU_RING_CONTROL_RESET) != 0 {
                    self.ring_reset_pending = true;
                }
                self.ring_control = value & pci::AEROGPU_RING_CONTROL_ENABLE;
            }

            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_LO as u64 => {
                self.fence_gpa = (self.fence_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_FENCE_GPA_HI as u64 => {
                self.fence_gpa =
                    (self.fence_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }

            x if x == pci::AEROGPU_MMIO_REG_DOORBELL as u64 => {
                let _ = value;
                self.doorbell_pending = true;
            }

            x if x == pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64 => {
                // Match emulator semantics: catch up vblank scheduling before enabling vblank IRQ
                // delivery so catch-up ticks don't immediately latch a "stale" vblank interrupt.
                let enabling_vblank = (value & pci::AEROGPU_IRQ_SCANOUT_VBLANK) != 0
                    && (self.irq_enable & pci::AEROGPU_IRQ_SCANOUT_VBLANK) == 0;
                if enabling_vblank {
                    if let Some(clock) = &self.clock {
                        self.tick_vblank(clock.now_ns());
                    }
                }

                self.irq_enable = value;
                // Clear any IRQ status bits that are now masked so re-enabling doesn't immediately
                // deliver a stale interrupt.
                if (value & pci::AEROGPU_IRQ_FENCE) == 0 {
                    self.irq_status &= !pci::AEROGPU_IRQ_FENCE;
                }
                if (value & pci::AEROGPU_IRQ_SCANOUT_VBLANK) == 0 {
                    self.irq_status &= !pci::AEROGPU_IRQ_SCANOUT_VBLANK;
                }
            }
            x if x == pci::AEROGPU_MMIO_REG_IRQ_ACK as u64 => {
                self.irq_status &= !value;
            }

            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_ENABLE as u64 => {
                let new_enable = value != 0;
                if self.scanout0_enable && !new_enable {
                    self.next_vblank_ns = None;
                    self.irq_status &= !pci::AEROGPU_IRQ_SCANOUT_VBLANK;
                    // Reset torn-update tracking so a stale LO write can't block future publishes.
                    self.scanout0_fb_gpa_pending_lo = 0;
                    self.scanout0_fb_gpa_lo_pending = false;
                }
                self.scanout0_enable = new_enable;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_WIDTH as u64 => {
                self.scanout0_width = value;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_HEIGHT as u64 => {
                self.scanout0_height = value;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FORMAT as u64 => {
                self.scanout0_format = value;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_PITCH_BYTES as u64 => {
                self.scanout0_pitch_bytes = value;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_LO as u64 => {
                // Avoid exposing a torn 64-bit `fb_gpa` update. Treat the LO write as starting a
                // new update and commit the combined value on the subsequent HI write.
                self.scanout0_fb_gpa_pending_lo = value;
                self.scanout0_fb_gpa_lo_pending = true;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }
            x if x == pci::AEROGPU_MMIO_REG_SCANOUT0_FB_GPA_HI as u64 => {
                // Drivers typically write LO then HI; treat HI as the commit point.
                let lo = if self.scanout0_fb_gpa_lo_pending {
                    u64::from(self.scanout0_fb_gpa_pending_lo)
                } else {
                    self.scanout0_fb_gpa & 0xffff_ffff
                };
                self.scanout0_fb_gpa = (u64::from(value) << 32) | lo;
                self.scanout0_fb_gpa_lo_pending = false;
                #[cfg(any(not(target_arch = "wasm32"), target_feature = "atomics"))]
                {
                    self.scanout0_dirty = true;
                }
                self.maybe_claim_wddm_scanout();
            }

            x if x == pci::AEROGPU_MMIO_REG_CURSOR_ENABLE as u64 => self.cursor_enable = value != 0,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_X as u64 => self.cursor_x = value as i32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_Y as u64 => self.cursor_y = value as i32,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_X as u64 => self.cursor_hot_x = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HOT_Y as u64 => self.cursor_hot_y = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_WIDTH as u64 => self.cursor_width = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_HEIGHT as u64 => self.cursor_height = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FORMAT as u64 => self.cursor_format = value,
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_LO as u64 => {
                self.cursor_fb_gpa =
                    (self.cursor_fb_gpa & 0xffff_ffff_0000_0000) | u64::from(value);
            }
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_FB_GPA_HI as u64 => {
                self.cursor_fb_gpa =
                    (self.cursor_fb_gpa & 0x0000_0000_ffff_ffff) | (u64::from(value) << 32);
            }
            x if x == pci::AEROGPU_MMIO_REG_CURSOR_PITCH_BYTES as u64 => {
                self.cursor_pitch_bytes = value;
            }

            // Ignore writes to read-only / unknown registers.
            _ => {}
        }
    }
}

impl PciBarMmioHandler for AeroGpuMmioDevice {
    fn read(&mut self, offset: u64, size: usize) -> u64 {
        match size {
            0 => 0,
            1 | 2 | 4 => {
                let aligned = offset & !3;
                let shift = ((offset & 3) * 8) as u32;
                let dword = self.mmio_read_dword(aligned);
                let mask = if size == 4 {
                    u32::MAX
                } else {
                    (1u32 << (size * 8)) - 1
                };
                u64::from((dword >> shift) & mask)
            }
            8 => {
                // Reads are issued by the physical memory bus using naturally-aligned sizes, so
                // `size=8` implies `offset` is 8-byte aligned.
                let lo = self.mmio_read_dword(offset) as u64;
                let hi = self.mmio_read_dword(offset + 4) as u64;
                lo | (hi << 32)
            }
            _ => 0,
        }
    }

    fn write(&mut self, offset: u64, size: usize, value: u64) {
        match size {
            0 => {}
            1 | 2 => {
                // Read-modify-write within the aligned dword.
                let aligned = offset & !3;
                let shift = ((offset & 3) * 8) as u32;
                let mask = ((1u32 << (size * 8)) - 1) << shift;
                let mut cur = self.mmio_read_dword(aligned);
                cur = (cur & !mask) | ((value as u32) << shift);
                self.mmio_write_dword(aligned, cur);
            }
            4 => {
                self.mmio_write_dword(offset, value as u32);
            }
            8 => {
                // Writes are issued by the physical memory bus using naturally-aligned sizes, so
                // `size=8` implies `offset` is 8-byte aligned.
                self.mmio_write_dword(offset, value as u32);
                self.mmio_write_dword(offset + 4, (value >> 32) as u32);
            }
            _ => {}
        }
    }
}

impl PciDevice for AeroGpuMmioDevice {
    fn config(&self) -> &PciConfigSpace {
        &self.config
    }

    fn config_mut(&mut self) -> &mut PciConfigSpace {
        &mut self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_mmio_regs_latch_and_survive_irq_ack() {
        let mut dev = AeroGpuMmioDevice::default();

        assert_ne!(dev.supported_features() & pci::AEROGPU_FEATURE_ERROR_INFO, 0);

        // Unmask ERROR IRQ delivery so `irq_level` reflects the status bit.
        dev.mmio_write_dword(
            pci::AEROGPU_MMIO_REG_IRQ_ENABLE as u64,
            pci::AEROGPU_IRQ_ERROR,
        );

        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_CODE as u64),
            pci::AerogpuErrorCode::None as u32
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO as u64),
            0
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI as u64),
            0
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_COUNT as u64),
            0
        );

        dev.record_error(pci::AerogpuErrorCode::Backend, 42);

        assert!(dev.irq_level());
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_CODE as u64),
            pci::AerogpuErrorCode::Backend as u32
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO as u64),
            42
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI as u64),
            0
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_COUNT as u64),
            1
        );

        // IRQ_ACK clears only the status bit; the error payload remains latched.
        dev.mmio_write_dword(pci::AEROGPU_MMIO_REG_IRQ_ACK as u64, pci::AEROGPU_IRQ_ERROR);
        assert!(!dev.irq_level());

        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_CODE as u64),
            pci::AerogpuErrorCode::Backend as u32
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_FENCE_LO as u64),
            42
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_FENCE_HI as u64),
            0
        );
        assert_eq!(
            dev.mmio_read_dword(pci::AEROGPU_MMIO_REG_ERROR_COUNT as u64),
            1
        );
    }
}
