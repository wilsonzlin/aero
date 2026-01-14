//! VGA/SVGA (VBE) device model.
//!
//! This crate is intentionally self-contained so it can be wired into the rest
//! of the emulator later. It provides:
//! - VGA register file emulation (sequencer/graphics/attribute/CRTC) with the
//!   subset of behavior needed for BIOS + early boot.
//! - Text mode (80x25) rendering with a built-in bitmap font and cursor.
//! - Mode 13h (320x200x256) rendering (chain-4).
//! - A Bochs-compatible VBE ("VBE_DISPI") interface for linear framebuffer
//!   modes commonly used by boot loaders/Windows boot splash.
//! - VRAM access helpers for mapping the legacy regions (0xA0000 etc) and the
//!   SVGA VRAM MMIO aperture (0xE0000000 by default), with the VBE LFB starting
//!   at an offset within that aperture (see [`SVGA_LFB_BASE`]).
//!
//! The `u32` framebuffer format is RGBA8888 in native-endian `u32`, where the
//! least significant byte is **R** (i.e. `0xAABBGGRR` on big-endian, but the
//! byte order in memory on little-endian is `[R, G, B, A]`, matching Canvas
//! `ImageData`).

mod palette;
mod snapshot;
mod text_font;

#[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
use aero_shared::scanout_state::{
    ScanoutStateUpdate, SCANOUT_FORMAT_B8G8R8X8, SCANOUT_SOURCE_LEGACY_TEXT,
    SCANOUT_SOURCE_LEGACY_VBE_LFB,
};
use palette::{rgb_to_rgba_u32, Rgb};
pub use snapshot::{VgaSnapshotError, VgaSnapshotV1, VgaSnapshotV2};
pub use text_font::FONT8X8_CP437;

#[cfg(feature = "integration-memory")]
mod integration_memory;
#[cfg(feature = "integration-memory")]
pub use integration_memory::{VgaLegacyMmioHandler, VgaLfbMmioHandler};

#[cfg(feature = "integration-platform")]
mod integration_platform;
#[cfg(feature = "integration-platform")]
pub use integration_platform::VgaPortIoDevice;

#[cfg(any(test, feature = "io-snapshot"))]
use aero_io_snapshot::io::state::codec::{Decoder, Encoder};
#[cfg(any(test, feature = "io-snapshot"))]
use aero_io_snapshot::io::state::{
    IoSnapshot, SnapshotError, SnapshotReader, SnapshotResult, SnapshotVersion, SnapshotWriter,
};

/// Default physical base address for the Bochs VBE linear framebuffer (LFB).
///
/// Real Bochs/QEMU configurations commonly map the LFB at `0xE000_0000`, but Aero's VGA/VBE device
/// model supports overriding the base at runtime via [`VgaDevice::set_svga_lfb_base`].
pub const SVGA_LFB_BASE: u32 = 0xE000_0000;

// -----------------------------------------------------------------------------
// Bochs/QEMU "Standard VGA" PCI identity + legacy decode ranges.
// -----------------------------------------------------------------------------
//
// `aero-gpu-vga` is the canonical legacy VGA/VBE device model crate. Several other crates expose
// this device behind different integration layers (e.g. the canonical `aero-machine` and the
// standalone `emulator`), so these constants live here to avoid value drift.

/// PCI vendor ID used by Bochs/QEMU for their "Standard VGA" device.
pub const VGA_PCI_VENDOR_ID: u16 = 0x1234;
/// PCI device ID used by Bochs/QEMU for their "Standard VGA" device.
pub const VGA_PCI_DEVICE_ID: u16 = 0x1111;

/// PCI base class code for a display controller (VGA-compatible).
pub const VGA_PCI_CLASS_CODE: u8 = 0x03;
/// PCI subclass for a VGA-compatible controller.
pub const VGA_PCI_SUBCLASS: u8 = 0x00;
/// PCI programming interface for the Bochs/QEMU "Standard VGA" device.
pub const VGA_PCI_PROG_IF: u8 = 0x00;

/// Full legacy VGA I/O decode range, including the mono + color CRTC aliasing ranges.
pub const VGA_LEGACY_IO_START: u16 = 0x3B0;
pub const VGA_LEGACY_IO_END: u16 = 0x3DF;
/// Length of [`VGA_LEGACY_IO_START`]..=[`VGA_LEGACY_IO_END`] in I/O ports.
pub const VGA_LEGACY_IO_LEN: u16 = VGA_LEGACY_IO_END - VGA_LEGACY_IO_START + 1;

/// Bochs VBE ("VBE_DISPI") index register port.
pub const VBE_DISPI_INDEX_PORT: u16 = 0x01CE;
/// Bochs VBE ("VBE_DISPI") data register port.
pub const VBE_DISPI_DATA_PORT: u16 = 0x01CF;
/// Full Bochs VBE ("VBE_DISPI") I/O decode range.
pub const VBE_DISPI_IO_START: u16 = VBE_DISPI_INDEX_PORT;
pub const VBE_DISPI_IO_END: u16 = VBE_DISPI_DATA_PORT;
/// Length of [`VBE_DISPI_IO_START`]..=[`VBE_DISPI_IO_END`] in I/O ports.
pub const VBE_DISPI_IO_LEN: u16 = VBE_DISPI_IO_END - VBE_DISPI_IO_START + 1;

/// Legacy VGA memory window covering the 128KiB aperture (`A0000-BFFFF`).
pub const VGA_LEGACY_MEM_START: u32 = 0xA0000;
pub const VGA_LEGACY_MEM_END: u32 = 0xBFFFF;
/// Length of [`VGA_LEGACY_MEM_START`]..=[`VGA_LEGACY_MEM_END`] in bytes.
pub const VGA_LEGACY_MEM_LEN: u32 = VGA_LEGACY_MEM_END - VGA_LEGACY_MEM_START + 1;

/// Default Bochs VBE linear framebuffer base address (alias for [`SVGA_LFB_BASE`]).
pub const DEFAULT_LFB_BASE: u32 = SVGA_LFB_BASE;
/// Default Bochs VBE linear framebuffer size in bytes (alias for [`DEFAULT_VRAM_SIZE`]).
pub const DEFAULT_LFB_SIZE: u32 = DEFAULT_VRAM_SIZE as u32;

/// Size of VGA plane memory (64KiB).
pub const VGA_PLANE_SIZE: usize = 64 * 1024;

/// Total VGA memory for 4 planes (256KiB).
pub const VGA_VRAM_SIZE: usize = 4 * VGA_PLANE_SIZE;

/// Offset within `vram` where the VBE ("SVGA") packed-pixel framebuffer begins.
///
/// The first 256KiB of VRAM is reserved for legacy VGA planar memory (4 × 64KiB planes) used by
/// text mode, mode 13h (chain-4), and planar graphics modes.
///
/// Bochs/QEMU style VBE linear/banked framebuffer accesses are mapped **after** this region so
/// switching between VBE graphics modes and legacy text/planar modes does not result in VBE writes
/// clobbering VGA plane 0/1 contents.
///
/// We align the start of the VBE framebuffer to the full planar region size (256KiB / 0x40000).
pub const VBE_FRAMEBUFFER_OFFSET: usize = VGA_VRAM_SIZE;

/// Default size of the VBE ("SVGA") framebuffer region in bytes (16MiB), enough for common VBE
/// modes.
///
/// Note: the device's *total* VRAM allocation also includes the legacy VGA planar region at the
/// start of `vram` (`[0, VBE_FRAMEBUFFER_OFFSET)`).
pub const DEFAULT_VRAM_SIZE: usize = 16 * 1024 * 1024;

/// Configuration for [`VgaDevice`].
///
/// This exists to support embedding the VGA/VBE frontend behind a PCI BAR-backed VRAM aperture
/// (e.g. AeroGPU BAR1), where:
/// - the *entire* VRAM aperture lives at a guest physical base (`vram_bar_base`), and
/// - the VBE linear framebuffer (LFB) begins at a fixed offset (`lfb_offset`) within that VRAM.
///
/// The default configuration matches Bochs/QEMU-style VGA/VBE:
/// - legacy VGA planar memory occupies the first 256KiB of VRAM (4 × 64KiB planes)
/// - the VBE packed-pixel framebuffer begins immediately after that region
/// - the VBE LFB is exposed at [`SVGA_LFB_BASE`] in guest physical address space
///
/// Since the LFB base is *not* the start of the VRAM allocation (it begins after the legacy VGA
/// planes), the default `vram_bar_base` is set such that `vram_bar_base + lfb_offset ==
/// SVGA_LFB_BASE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VgaConfig {
    /// Total VRAM size in bytes.
    pub vram_size: usize,
    /// Guest physical base of the VRAM aperture (e.g. PCI BAR base).
    pub vram_bar_base: u32,
    /// Offset within VRAM where the VBE linear framebuffer (LFB) begins.
    pub lfb_offset: u32,
    /// Number of legacy VGA planes backed by VRAM starting at offset 0.
    ///
    /// - `4` matches traditional VGA (256KiB planar memory).
    /// - `2` is sufficient for VGA text mode (character + attribute planes) and is useful when the
    ///   guest VRAM layout reserves only 128KiB for legacy VGA (e.g. `0x00000..0x1FFFF`).
    pub legacy_plane_count: usize,
}

impl Default for VgaConfig {
    fn default() -> Self {
        Self {
            vram_size: DEFAULT_VRAM_SIZE,
            vram_bar_base: SVGA_LFB_BASE.wrapping_sub(VBE_FRAMEBUFFER_OFFSET as u32),
            lfb_offset: VBE_FRAMEBUFFER_OFFSET as u32,
            legacy_plane_count: 4,
        }
    }
}

impl VgaConfig {
    /// Returns the guest physical base address of the VBE linear framebuffer (LFB).
    pub fn lfb_base(self) -> u32 {
        self.vram_bar_base.wrapping_add(self.lfb_offset)
    }
}

fn io_all_ones(size: usize) -> u32 {
    match size {
        0 => 0,
        1 => 0xFF,
        2 => 0xFFFF,
        4 => 0xFFFF_FFFF,
        _ => 0xFFFF_FFFF,
    }
}

/// Host-facing display trait (to be shared with the rest of the emulator).
pub trait DisplayOutput {
    /// Returns the current visible framebuffer (front buffer) as RGBA8888.
    fn get_framebuffer(&self) -> &[u32];

    /// Returns the current output resolution.
    fn get_resolution(&self) -> (u32, u32);

    /// Re-renders into the front buffer if necessary.
    fn present(&mut self);
}

/// Port I/O trait (to be shared with the CPU/machine).
pub trait PortIO {
    fn port_read(&mut self, port: u16, size: usize) -> u32;
    fn port_write(&mut self, port: u16, size: usize, val: u32);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RenderMode {
    Text80x25,
    Mode13h,
    Planar4bpp { width: u32, height: u32 },
    SvgaLinear { width: u32, height: u32, bpp: u16 },
}

#[derive(Debug, Clone, Copy, Default)]
pub struct VbeRegs {
    pub xres: u16,
    pub yres: u16,
    pub bpp: u16,
    pub enable: u16,
    pub bank: u16,
    pub virt_width: u16,
    pub virt_height: u16,
    pub x_offset: u16,
    pub y_offset: u16,
}

impl VbeRegs {
    fn enabled(self) -> bool {
        (self.enable & 0x0001) != 0
    }

    fn lfb_enabled(self) -> bool {
        (self.enable & 0x0040) != 0
    }

    fn effective_stride_pixels(self) -> u32 {
        let vw = self.virt_width as u32;
        if vw != 0 {
            vw
        } else {
            self.xres as u32
        }
    }
}

/// VGA/SVGA device.
pub struct VgaDevice {
    // Configuration.
    svga_lfb_base: u32,

    // Core VGA registers.
    misc_output: u8,

    sequencer_index: u8,
    sequencer: [u8; 5],
    /// Storage for sequencer registers outside the standard VGA range (0x00..=0x04).
    ///
    /// Some guests probe "extended" VGA registers by reading/writing indices that aren't
    /// implemented on plain VGA hardware. Keep those values independent from the core register
    /// file so probing doesn't clobber real mode state.
    sequencer_ext: [u8; 256],

    graphics_index: u8,
    graphics: [u8; 9],
    /// Storage for graphics controller registers outside the standard VGA range (0x00..=0x08).
    graphics_ext: [u8; 256],

    crtc_index: u8,
    crtc: [u8; 25],
    /// Storage for CRTC registers outside the standard VGA range (0x00..=0x18).
    crtc_ext: [u8; 256],

    attribute_index: u8,
    attribute_flip_flop_data: bool,
    attribute: [u8; 21],
    /// Storage for attribute controller registers outside the standard VGA range (0x00..=0x14).
    ///
    /// The attribute controller index is masked to 0x1F, so 0x20 entries is sufficient.
    attribute_ext: [u8; 0x20],
    input_status1_vretrace: bool,
    /// Deterministic vblank clock used to model the VGA Input Status 1 vertical retrace bit.
    ///
    /// This advances via [`VgaDevice::tick`], which is wired to the machine's deterministic
    /// `tick_platform` path.
    vblank_time_ns: u64,

    // DAC / palette.
    pel_mask: u8,
    dac_write_index: u8,
    dac_write_subindex: u8,
    dac_write_latch: [u8; 3],
    dac_read_index: u8,
    dac_read_subindex: u8,
    dac: [Rgb; 256],

    // Bochs VBE.
    vbe_index: u16,
    pub vbe: VbeRegs,
    /// Optional override for the logical scanline length (bytes per scan line).
    ///
    /// The Bochs VBE_DISPI interface represents the logical scanline length in *pixels* via the
    /// `virt_width` register. BIOS VBE (INT 10h AX=4F06) can set the scanline length in *bytes*.
    ///
    /// Aero's HLE BIOS rounds the requested byte length up to a whole number of pixels for packed
    /// pixel modes, but we still keep this as an explicit byte override so scanout/panning can
    /// follow BIOS-driven stride changes (and to support other integrations that may supply a
    /// byte-granular pitch).
    ///
    /// When this value is non-zero, the SVGA scanout renderer uses it as the exact scanline
    /// stride in bytes (instead of deriving stride from `virt_width`). This keeps BIOS-driven
    /// panning/stride semantics (`4F06`/`4F07`) faithful even when the requested stride is not
    /// representable by `virt_width`.
    vbe_bytes_per_scan_line_override: u16,

    // VRAM layout:
    // - the first `legacy_plane_count × 64KiB` backs legacy VGA planar memory (text/planar/13h).
    // - the VBE/SVGA packed-pixel framebuffer begins at `lfb_offset`.
    vram: Vec<u8>,
    latches: [u8; 4],

    config: VgaConfig,

    // Output buffers.
    front: Vec<u32>,
    back: Vec<u32>,
    width: u32,
    height: u32,
    dirty: bool,
}

impl Default for VgaDevice {
    fn default() -> Self {
        Self::new()
    }
}

impl VgaDevice {
    /// Default vblank period used for legacy software that polls the VGA status register.
    ///
    /// This is intentionally a fixed 60Hz model today (rounded up to integer nanoseconds), and is
    /// only used for the Input Status 1 retrace bit; it does not affect rendering.
    const VBLANK_PERIOD_NS: u64 = 16_666_667;
    /// Width of the vblank pulse within the period.
    ///
    /// A short pulse is sufficient for retrace polling loops. Use ~5% of the frame period.
    const VBLANK_PULSE_NS: u64 = Self::VBLANK_PERIOD_NS / 20;

    pub fn new() -> Self {
        Self::new_with_config(VgaConfig::default())
    }

    pub fn new_with_config(config: VgaConfig) -> Self {
        assert!(
            config.vram_size <= u32::MAX as usize,
            "vram_size {} exceeds u32::MAX; aero-gpu-vga uses 32-bit guest physical addresses",
            config.vram_size
        );
        assert!(
            (1..=4).contains(&config.legacy_plane_count),
            "legacy_plane_count must be in 1..=4"
        );
        assert!(
            config.vram_size >= config.legacy_plane_count * VGA_PLANE_SIZE,
            "vram_size too small for legacy planes"
        );
        assert!(
            usize::try_from(config.lfb_offset)
                .ok()
                .is_some_and(|off| off <= config.vram_size),
            "lfb_offset out of range"
        );

        let mut device = Self {
            svga_lfb_base: config.lfb_base(),
            misc_output: 0,
            sequencer_index: 0,
            sequencer: [0; 5],
            sequencer_ext: [0; 256],
            graphics_index: 0,
            graphics: [0; 9],
            graphics_ext: [0; 256],
            crtc_index: 0,
            crtc: [0; 25],
            crtc_ext: [0; 256],
            attribute_index: 0,
            attribute_flip_flop_data: false,
            attribute: [0; 21],
            attribute_ext: [0; 0x20],
            input_status1_vretrace: false,
            vblank_time_ns: 0,
            pel_mask: 0xFF,
            dac_write_index: 0,
            dac_write_subindex: 0,
            dac_write_latch: [0; 3],
            dac_read_index: 0,
            dac_read_subindex: 0,
            dac: [Rgb::BLACK; 256],
            vbe_index: 0,
            vbe: VbeRegs::default(),
            vbe_bytes_per_scan_line_override: 0,
            vram: vec![0; config.vram_size],
            latches: [0; 4],
            config,
            front: Vec::new(),
            back: Vec::new(),
            width: 0,
            height: 0,
            dirty: true,
        };

        device.reset_palette();
        device.set_text_mode_80x25();
        device.present();
        device
    }

    /// Advance the deterministic vblank clock.
    ///
    /// This does not affect rendering (Aero's VGA model is not scanline-accurate), but it allows
    /// legacy guests to poll the VGA status register (`0x3DA`) for vertical retrace pacing.
    pub fn tick(&mut self, delta_ns: u64) {
        self.vblank_time_ns = self.vblank_time_ns.wrapping_add(delta_ns);
    }

    fn in_vblank(&self) -> bool {
        if Self::VBLANK_PERIOD_NS == 0 {
            return false;
        }
        let pos = self.vblank_time_ns % Self::VBLANK_PERIOD_NS;
        pos < Self::VBLANK_PULSE_NS
    }

    pub fn config(&self) -> VgaConfig {
        self.config
    }

    /// Updates the guest physical base address of the VRAM aperture (e.g. on PCI BAR reprogram).
    pub fn set_vram_bar_base(&mut self, vram_bar_base: u32) {
        self.config.vram_bar_base = vram_bar_base;
        self.svga_lfb_base = self.config.lfb_base();
    }

    pub fn lfb_base(&self) -> u32 {
        self.svga_lfb_base
    }

    pub fn svga_lfb_base(&self) -> u32 {
        self.svga_lfb_base
    }

    pub fn set_svga_lfb_base(&mut self, base: u32) {
        self.svga_lfb_base = base;
        // Keep the config-derived base address in sync so callers that only update the LFB base
        // still get correct address translation for the configured VRAM layout.
        self.config.vram_bar_base = base.wrapping_sub(self.config.lfb_offset);
    }

    /// Total VRAM backing size in bytes.
    pub fn vram_size(&self) -> usize {
        self.vram.len()
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    /// Returns a [`ScanoutStateUpdate`] describing the VGA/VBE source that should currently be
    /// presented.
    ///
    /// For VBE linear modes, this describes the active LFB in guest physical address space. For
    /// legacy VGA modes (including text mode), this reports a `LegacyText` source with zeroed
    /// geometry; presentation is considered "implicit" and handled by the VGA renderer rather than
    /// as a B8G8R8X8 scanout in guest memory.
    pub fn active_scanout_update(&self) -> ScanoutStateUpdate {
        // VBE scanout is only describable via `ScanoutState` when the guest has enabled the linear
        // framebuffer and the pixel format matches the VBE scanout pixel format we currently
        // support (`B8G8R8X8`).
        if self.vbe.enabled() && self.vbe.lfb_enabled() && self.vbe.bpp == 32 {
            let disabled = ScanoutStateUpdate {
                source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
                base_paddr_lo: 0,
                base_paddr_hi: 0,
                width: 0,
                height: 0,
                pitch_bytes: 0,
                format: SCANOUT_FORMAT_B8G8R8X8,
            };

            let width = u32::from(self.vbe.xres);
            let height = u32::from(self.vbe.yres);
            if width == 0 || height == 0 {
                return disabled;
            }

            // Only support the canonical boot pixel format for now:
            // little-endian packed pixels B8G8R8X8 (the high X byte is unused/undefined; presenters
            // treat it as opaque alpha (`0xFF`) when converting to RGBA).
            let bytes_per_pixel: u32 = 4;

            // Derive the pitch from the Bochs "virtual width" (stride) unless a BIOS VBE call has
            // overridden it explicitly via `4F06`.
            let pitch_bytes = if self.vbe_bytes_per_scan_line_override != 0 {
                u32::from(self.vbe_bytes_per_scan_line_override)
            } else {
                let stride_pixels = self.vbe.effective_stride_pixels();
                match stride_pixels.checked_mul(bytes_per_pixel) {
                    Some(v) => v,
                    None => return disabled,
                }
            };
            if pitch_bytes == 0 {
                return disabled;
            }
            // Packed pixel scanout requires whole-pixel alignment so each scanline begins on a
            // pixel boundary. This matches the assumptions made by scanout consumers (e.g. web
            // readback paths require `pitchBytes % bytesPerPixel == 0`).
            if pitch_bytes % bytes_per_pixel != 0 {
                return disabled;
            }

            let row_bytes = match width.checked_mul(bytes_per_pixel) {
                Some(v) => v,
                None => return disabled,
            };
            if pitch_bytes < row_bytes {
                return disabled;
            }

            // Compute the displayed base address in guest physical memory.
            //
            // `ScanoutState` has no explicit panning fields, so `(x_offset, y_offset)` must be
            // encoded by adjusting the base.
            let lfb_base = u64::from(self.svga_lfb_base);
            let x_off_bytes =
                match u64::from(self.vbe.x_offset).checked_mul(u64::from(bytes_per_pixel)) {
                    Some(v) => v,
                    None => return disabled,
                };
            let y_off_bytes = match u64::from(self.vbe.y_offset).checked_mul(u64::from(pitch_bytes))
            {
                Some(v) => v,
                None => return disabled,
            };
            let base_paddr = match lfb_base
                .checked_add(y_off_bytes)
                .and_then(|v| v.checked_add(x_off_bytes))
            {
                Some(v) => v,
                None => return disabled,
            };

            // Validate that the scanout rectangle fits within the backing VRAM aperture.
            let fb_base = match usize::try_from(self.config.lfb_offset) {
                Ok(v) => v,
                Err(_) => return disabled,
            };
            let vbe_len = match self.vram.len().checked_sub(fb_base) {
                Some(v) => v as u64,
                None => return disabled,
            };
            let lfb_end = match lfb_base.checked_add(vbe_len) {
                Some(v) => v,
                None => return disabled,
            };

            let last_row = match height.checked_sub(1) {
                Some(v) => v,
                None => return disabled,
            };
            let needed_bytes = match u64::from(pitch_bytes)
                .checked_mul(u64::from(last_row))
                .and_then(|v| v.checked_add(u64::from(row_bytes)))
            {
                Some(v) => v,
                None => return disabled,
            };
            let scanout_end = match base_paddr.checked_add(needed_bytes) {
                Some(v) => v,
                None => return disabled,
            };

            if base_paddr < lfb_base || scanout_end > lfb_end {
                return disabled;
            }
            return ScanoutStateUpdate {
                source: SCANOUT_SOURCE_LEGACY_VBE_LFB,
                base_paddr_lo: base_paddr as u32,
                base_paddr_hi: (base_paddr >> 32) as u32,
                width,
                height,
                pitch_bytes,
                format: SCANOUT_FORMAT_B8G8R8X8,
            };
        }

        // Legacy VGA text/planar modes are not a linear pixel framebuffer in guest physical
        // memory. Consumers should treat this as an "implicit" legacy scanout and use the VGA
        // renderer output.
        ScanoutStateUpdate {
            source: SCANOUT_SOURCE_LEGACY_TEXT,
            base_paddr_lo: 0xB8000,
            base_paddr_hi: 0,
            width: 0,
            height: 0,
            pitch_bytes: 0,
            format: SCANOUT_FORMAT_B8G8R8X8,
        }
    }
    /// Resets the DAC to a sensible default VGA palette (EGA 16-color + 256-color cube).
    pub fn reset_palette(&mut self) {
        self.dac = palette::default_vga_palette();
        self.pel_mask = 0xFF;
    }

    /// Convenience helper: configure the register file for VGA text mode 80x25.
    pub fn set_text_mode_80x25(&mut self) {
        // Attribute mode control: bit0=0 => text.
        self.attribute[0x10] = 1 << 2; // line graphics enable
                                       // Enable all 4 color planes by default; otherwise color indices would be masked to 0.
        self.attribute[0x12] = 0x0F; // color plane enable
                                     // Default color select: choose palette page 0.
        self.attribute[0x14] = 0x00; // color select
                                     // Identity palette mapping for indices 0..15.
        for i in 0..16 {
            self.attribute[i] = i as u8;
        }

        // Sequencer memory mode: chain-4 disabled (bit3 = 0) and odd/even enabled
        // (odd/even disable bit2 = 0).
        self.sequencer[4] = 0x02;
        // Sequencer map mask: enable planes 0 and 1 for text.
        self.sequencer[2] = 0x03;

        // Graphics controller misc: memory map = 0b11 => B8000, and odd/even.
        self.graphics[6] = 0x0C; // bits 2-3 = 3 (B8000)
        self.graphics[5] = 0x10; // set odd/even (bit4)
        self.graphics[4] = 0x00; // read map select

        // Cursor: enable, full block by default at 0.
        self.crtc[0x0A] = 0x00;
        self.crtc[0x0B] = 0x0F;
        // Start address: reset the displayed top-left cell to 0.
        self.crtc[0x0C] = 0x00;
        self.crtc[0x0D] = 0x00;
        self.crtc[0x0E] = 0x00;
        self.crtc[0x0F] = 0x00;

        self.vbe.enable = 0;
        self.vbe_bytes_per_scan_line_override = 0;
        self.ensure_buffers(80 * 9, 25 * 16);
        self.dirty = true;
    }

    /// Convenience helper: configure registers for VGA mode 13h (320x200x256).
    pub fn set_mode_13h(&mut self) {
        // Attribute mode control: graphics enable.
        self.attribute[0x10] = 0x01;
        self.attribute[0x12] = 0x0F; // color plane enable (not used by mode 13h path, but matches VGA defaults)
        self.attribute[0x14] = 0x00; // color select
                                     // Identity palette mapping for indices 0..15; in 256-color mode the mapping is bypassed.
        for i in 0..16 {
            self.attribute[i] = i as u8;
        }

        // Sequencer memory mode: enable chain-4 (bit3) and disable odd/even (bit2).
        // The commonly used VGA register table for mode 13h programs 0x0E here.
        self.sequencer[4] = 0x0E;
        self.sequencer[2] = 0x0F; // enable all planes

        // Graphics controller misc: memory map = 0b01 => A0000 64KB.
        self.graphics[6] = 0x04;
        self.graphics[5] = 0x40; // 256-color shift register (bit6), no odd/even
        self.graphics[4] = 0x00;

        self.vbe.enable = 0;
        self.vbe_bytes_per_scan_line_override = 0;
        self.ensure_buffers(320, 200);
        self.dirty = true;
    }

    /// Convenience helper: enable a VBE linear mode (Bochs VBE_DISPI).
    pub fn set_svga_mode(&mut self, width: u16, height: u16, bpp: u16, lfb: bool) {
        self.vbe.xres = width;
        self.vbe.yres = height;
        self.vbe.bpp = bpp;
        self.vbe.virt_width = width;
        self.vbe.virt_height = height;
        self.vbe.x_offset = 0;
        self.vbe.y_offset = 0;
        self.vbe.bank = 0;
        self.vbe_bytes_per_scan_line_override = 0;

        self.vbe.enable = 0x0001 | if lfb { 0x0040 } else { 0 };
        self.ensure_buffers(width as u32, height as u32);
        self.dirty = true;
    }

    /// Override the VBE scanline length in bytes for SVGA scanout.
    ///
    /// This is intended for the BIOS VBE (INT 10h 4F06) compatibility path, where the logical
    /// scanline length is specified in bytes (`bios.video.vbe.bytes_per_scan_line`).
    ///
    /// A value of 0 clears the override and restores the default Bochs behavior of deriving stride
    /// from `virt_width` and `bpp`.
    pub fn set_vbe_bytes_per_scan_line_override(&mut self, bytes: u16) {
        self.vbe_bytes_per_scan_line_override = bytes;
        self.dirty = true;
    }

    pub fn vram(&self) -> &[u8] {
        &self.vram
    }

    pub fn vram_mut(&mut self) -> &mut [u8] {
        self.dirty = true;
        &mut self.vram
    }

    /// Reads from guest physical memory, covering legacy VGA windows and the VBE linear framebuffer.
    pub fn mem_read_u8(&mut self, paddr: u32) -> u8 {
        if let Some(offset) = self.map_svga_lfb(paddr) {
            return self.vram.get(offset).copied().unwrap_or(0xFF);
        }

        if self.vbe.enabled() {
            if let Some(offset) = self.map_svga_bank_window(paddr) {
                return self.vram.get(offset).copied().unwrap_or(0xFF);
            }
        }

        if let Some(access) = self.map_legacy_vga(paddr) {
            match access {
                LegacyReadTarget::Single { plane, off } => {
                    return self.vram[plane * VGA_PLANE_SIZE + off];
                }
                LegacyReadTarget::Planar { off } => {
                    return self.read_u8_planar(off);
                }
            }
        }

        0xFF
    }

    /// Writes to guest physical memory, covering legacy VGA windows and the VBE linear framebuffer.
    pub fn mem_write_u8(&mut self, paddr: u32, value: u8) {
        if let Some(offset) = self.map_svga_lfb(paddr) {
            if let Some(byte) = self.vram.get_mut(offset) {
                *byte = value;
                self.dirty = true;
            }
            return;
        }

        if self.vbe.enabled() {
            if let Some(offset) = self.map_svga_bank_window(paddr) {
                if let Some(byte) = self.vram.get_mut(offset) {
                    *byte = value;
                    self.dirty = true;
                }
                return;
            }
        }

        if let Some(access) = self.legacy_vga_write_targets(paddr) {
            match access {
                LegacyWriteTargets::Single { plane, off } => {
                    self.vram[plane * VGA_PLANE_SIZE + off] = value;
                }
                LegacyWriteTargets::Planar { off } => {
                    self.write_u8_planar(off, value);
                }
            }
            self.dirty = true;
        }
    }

    fn map_svga_lfb(&self, paddr: u32) -> Option<usize> {
        if !self.vbe.enabled() || !self.vbe.lfb_enabled() {
            return None;
        }
        let start = u64::from(self.svga_lfb_base);
        let paddr = u64::from(paddr);
        if paddr < start {
            return None;
        }

        let fb_base = usize::try_from(self.config.lfb_offset).ok()?;
        let vbe_len = self.vram.len().checked_sub(fb_base)? as u64;
        let off = paddr - start;
        if off >= vbe_len {
            return None;
        }

        fb_base.checked_add(off as usize)
    }

    fn map_svga_bank_window(&self, paddr: u32) -> Option<usize> {
        // Traditional banked window is a 64KiB aperture at A0000.
        let start = 0xA0000;
        let end = 0xB0000;
        if paddr < start || paddr >= end {
            return None;
        }
        let window_off = (paddr - start) as usize;
        let bank_base = (self.vbe.bank as usize) * 64 * 1024;

        let fb_base = usize::try_from(self.config.lfb_offset).ok()?;
        let vbe_len = self.vram.len().checked_sub(fb_base)?;
        let off = bank_base.checked_add(window_off)?;
        if off >= vbe_len {
            return None;
        }

        fb_base.checked_add(off)
    }

    fn legacy_memory_map(&self) -> u8 {
        (self.graphics[6] >> 2) & 0x03
    }

    fn map_legacy_vga(&self, paddr: u32) -> Option<LegacyReadTarget> {
        let map = self.legacy_memory_map();
        let (base, size) = match map {
            0 => (0xA0000, 0x20000), // A0000-BFFFF
            1 => (0xA0000, 0x10000), // A0000-AFFFF
            2 => (0xB0000, 0x08000), // B0000-B7FFF
            3 => (0xB8000, 0x08000), // B8000-BFFFF
            _ => (0xA0000, 0x10000),
        };
        if paddr < base || paddr >= base + size {
            return None;
        }
        let off = (paddr - base) as usize;

        if self.chain4_enabled() {
            let plane = off & 0x03;
            let plane_off = off >> 2;
            if plane >= self.config.legacy_plane_count {
                return None;
            }
            Some(LegacyReadTarget::Single {
                plane,
                off: plane_off,
            })
        } else if self.odd_even_enabled() {
            let plane = off & 0x01;
            let plane_off = off >> 1;
            if plane >= self.config.legacy_plane_count {
                return None;
            }
            Some(LegacyReadTarget::Single {
                plane,
                off: plane_off,
            })
        } else {
            Some(LegacyReadTarget::Planar { off })
        }
    }

    fn legacy_vga_write_targets(&self, paddr: u32) -> Option<LegacyWriteTargets> {
        let map = self.legacy_memory_map();
        let (base, size) = match map {
            0 => (0xA0000, 0x20000), // A0000-BFFFF
            1 => (0xA0000, 0x10000), // A0000-AFFFF
            2 => (0xB0000, 0x08000), // B0000-B7FFF
            3 => (0xB8000, 0x08000), // B8000-BFFFF
            _ => (0xA0000, 0x10000),
        };
        if paddr < base || paddr >= base + size {
            return None;
        }
        let off = (paddr - base) as usize;

        if self.chain4_enabled() {
            let plane = off & 0x03;
            let plane_off = off >> 2;
            if plane >= self.config.legacy_plane_count {
                return None;
            }
            Some(LegacyWriteTargets::Single {
                plane,
                off: plane_off,
            })
        } else if self.odd_even_enabled() {
            let plane = off & 0x01;
            let plane_off = off >> 1;
            if plane >= self.config.legacy_plane_count {
                return None;
            }
            Some(LegacyWriteTargets::Single {
                plane,
                off: plane_off,
            })
        } else {
            Some(LegacyWriteTargets::Planar { off })
        }
    }

    fn plane_offset(&self, off: usize) -> usize {
        // VGA planes are 64KiB. Some memory map configurations expose a 128KiB window; on real
        // hardware the address decode effectively wraps, so we do the same.
        off & (VGA_PLANE_SIZE - 1)
    }

    fn load_latches(&mut self, off: usize) {
        let off = self.plane_offset(off);
        for plane in 0..4 {
            if plane < self.config.legacy_plane_count {
                self.latches[plane] = self.vram[plane * VGA_PLANE_SIZE + off];
            } else {
                self.latches[plane] = 0;
            }
        }
    }

    fn read_u8_planar(&mut self, off: usize) -> u8 {
        let off = self.plane_offset(off);
        self.load_latches(off);
        let gc_mode = self.graphics[5];
        let read_mode = (gc_mode >> 3) & 0x01;
        if read_mode == 0 {
            let plane = (self.graphics[4] & 0x03) as usize;
            self.latches[plane]
        } else {
            let color_compare = self.graphics[2];
            let color_dont_care = self.graphics[7];
            self.read_mode_1_color_compare(color_compare, color_dont_care)
        }
    }

    fn read_mode_1_color_compare(&self, color_compare: u8, color_dont_care: u8) -> u8 {
        // VGA "Read Mode 1" returns a byte where each bit indicates whether the corresponding
        // pixel's 4-bit color matches the Color Compare register (with planes masked by the
        // Color Don't Care register).
        let mut diff = 0u8;
        let compare = color_compare & 0x0F;
        let dont_care = color_dont_care & 0x0F;

        for plane in 0..4 {
            let plane_mask_bit = 1u8 << plane;
            // "Color Don't Care" is a *mask* of planes to compare; cleared bits are treated as
            // don't-care.
            let care_mask = if (dont_care & plane_mask_bit) != 0 {
                0xFF
            } else {
                0x00
            };
            let compare_byte = if (compare & plane_mask_bit) != 0 {
                0xFF
            } else {
                0x00
            };
            diff |= (self.latches[plane] ^ compare_byte) & care_mask;
        }

        !diff
    }

    fn write_u8_planar(&mut self, off: usize, value: u8) {
        let off = self.plane_offset(off);

        let write_mode = self.graphics[5] & 0x03;
        if write_mode != 1 {
            // VGA implements read-modify-write via latches for most write modes.
            self.load_latches(off);
        }

        let data_rotate = self.graphics[3];
        let rotate_count = data_rotate & 0x07;
        let func_select = (data_rotate >> 3) & 0x03;
        let bit_mask = self.graphics[8];

        let rotated = value.rotate_right(rotate_count as u32);

        let map_mask = self.sequencer[2] & 0x0F;
        let set_reset = self.graphics[0];
        let enable_set_reset = self.graphics[1];

        for plane in 0..self.config.legacy_plane_count {
            let plane_mask_bit = 1u8 << plane;
            if (map_mask & plane_mask_bit) == 0 {
                continue;
            }

            let latch = self.latches[plane];
            let result = match write_mode {
                0 => {
                    let mut data = rotated;
                    if (enable_set_reset & plane_mask_bit) != 0 {
                        data = if (set_reset & plane_mask_bit) != 0 {
                            0xFF
                        } else {
                            0x00
                        };
                    }

                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };

                    (alu & bit_mask) | (latch & !bit_mask)
                }
                1 => latch,
                2 => {
                    let data = if (value & plane_mask_bit) != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };
                    (alu & bit_mask) | (latch & !bit_mask)
                }
                3 => {
                    let data = if (set_reset & plane_mask_bit) != 0 {
                        0xFF
                    } else {
                        0x00
                    };
                    let alu = match func_select {
                        0 => data,
                        1 => data & latch,
                        2 => data | latch,
                        3 => data ^ latch,
                        _ => unreachable!(),
                    };
                    let mask = bit_mask & rotated;
                    (alu & mask) | (latch & !mask)
                }
                _ => unreachable!("VGA write mode {write_mode} is invalid"),
            };

            self.vram[plane * VGA_PLANE_SIZE + off] = result;
        }
    }

    fn chain4_enabled(&self) -> bool {
        (self.sequencer[4] & 0x08) != 0
    }

    fn odd_even_enabled(&self) -> bool {
        // Odd/even requires the graphics controller bit plus the sequencer not disabling it.
        (self.graphics[5] & 0x10) != 0 && (self.sequencer[4] & 0x04) == 0
    }

    fn derived_render_mode(&self) -> RenderMode {
        if self.vbe.enabled() {
            return RenderMode::SvgaLinear {
                width: self.vbe.xres as u32,
                height: self.vbe.yres as u32,
                bpp: self.vbe.bpp,
            };
        }

        let attr_mode = self.attribute[0x10];
        let graphics_enabled = (attr_mode & 0x01) != 0;

        if !graphics_enabled {
            return RenderMode::Text80x25;
        }

        if self.chain4_enabled() {
            return RenderMode::Mode13h;
        }

        let (width, height) = self.derive_crtc_resolution();
        RenderMode::Planar4bpp { width, height }
    }

    fn derive_crtc_resolution(&self) -> (u32, u32) {
        // Horizontal display end is in character clocks (8 pixels).
        let width = (self.crtc[1] as u32 + 1) * 8;

        // Vertical display end is extended using overflow bits.
        let vde_low = self.crtc[0x12] as u32;
        let overflow = self.crtc[0x07];
        let vde =
            vde_low | (((overflow as u32 >> 1) & 1) << 8) | (((overflow as u32 >> 6) & 1) << 9);
        let height = vde + 1;

        // Clamp to something sane to avoid accidental huge allocations.
        let width = width.clamp(1, 2048);
        let height = height.clamp(1, 1536);
        (width, height)
    }

    fn crtc_start_address(&self) -> u16 {
        let hi = self.crtc.get(0x0C).copied().unwrap_or(0);
        let lo = self.crtc.get(0x0D).copied().unwrap_or(0);
        (u16::from(hi) << 8) | u16::from(lo)
    }

    fn crtc_byte_mode(&self) -> bool {
        // VGA CRTC Mode Control (index 0x17) bit 6: Byte mode.
        //
        // When set, the start address and scanline offset registers address memory in bytes rather
        // than words (legacy CGA-compatible behavior uses word addressing).
        (self.crtc.get(0x17).copied().unwrap_or(0) & 0x40) != 0
    }

    fn crtc_start_address_bytes(&self) -> usize {
        let start = usize::from(self.crtc_start_address());
        if self.crtc_byte_mode() {
            start
        } else {
            start << 1
        }
    }

    fn crtc_offset_bytes(&self) -> Option<usize> {
        // VGA CRTC Offset (index 0x13) gives the scanline pitch in addressable units.
        // When byte mode is disabled, the unit is 2-byte words; otherwise it is bytes.
        let offset = self.crtc.get(0x13).copied().unwrap_or(0) as usize;
        if offset == 0 {
            return None;
        }
        Some(if self.crtc_byte_mode() {
            offset
        } else {
            offset << 1
        })
    }

    fn ensure_buffers(&mut self, width: u32, height: u32) {
        if self.width == width && self.height == height && !self.front.is_empty() {
            return;
        }
        self.width = width;
        self.height = height;
        let pixels = width as usize * height as usize;
        self.front.resize(pixels, 0);
        self.back.resize(pixels, 0);
    }

    fn render(&mut self) {
        let mode = self.derived_render_mode();
        match mode {
            RenderMode::Text80x25 => {
                self.ensure_buffers(80 * 9, 25 * 16);
                self.render_text_mode();
            }
            RenderMode::Mode13h => {
                self.ensure_buffers(320, 200);
                self.render_mode_13h();
            }
            RenderMode::Planar4bpp { width, height } => {
                self.ensure_buffers(width, height);
                self.render_planar_4bpp(width, height);
            }
            RenderMode::SvgaLinear { width, height, bpp } => {
                self.ensure_buffers(width, height);
                self.render_svga(width, height, bpp);
            }
        }
    }

    fn render_text_mode(&mut self) {
        self.back.fill(0);
        let cols = 80usize;
        let rows = 25usize;
        let cell_w = 9usize;
        let cell_h = 16usize;
        let width = self.width as usize;

        let line_graphics_enable = (self.attribute[0x10] & (1 << 2)) != 0;
        let blink_enabled = (self.attribute[0x10] & (1 << 3)) != 0;

        // Text start address (CRTC regs 0x0C/0x0D) is specified in units of character cells.
        // Real VGA hardware uses only the low 14 bits and wraps within the 16KiB text window.
        let start_addr = (self.crtc_start_address_bytes() >> 1) & 0x3FFF;

        // CRTC offset (0x13) gives the scanline pitch. In text mode, each cell is 2 bytes (char +
        // attr), so treat the pitch as "cells per row".
        let row_stride_cells = self
            .crtc_offset_bytes()
            .and_then(|b| b.checked_div(2))
            .filter(|&v| v != 0)
            .unwrap_or(cols);

        for row in 0..rows {
            for col in 0..cols {
                let cell_index = row * row_stride_cells + col;
                let mem_index = (start_addr + cell_index) & 0x3FFF;
                let ch = self.vram[mem_index];
                let attr = if self.config.legacy_plane_count >= 2 {
                    self.vram[VGA_PLANE_SIZE + mem_index]
                } else {
                    0
                };

                let fg = attr & 0x0F;
                let bg = if blink_enabled {
                    (attr >> 4) & 0x07
                } else {
                    (attr >> 4) & 0x0F
                };

                let fg_dac = self.attribute_palette_lookup(fg);
                let bg_dac = self.attribute_palette_lookup(bg);

                // Apply the PEL mask (0x3C6) to the final DAC index like real VGA hardware.
                let fg_px = rgb_to_rgba_u32(self.dac[(fg_dac & self.pel_mask) as usize]);
                let bg_px = rgb_to_rgba_u32(self.dac[(bg_dac & self.pel_mask) as usize]);

                for y in 0..cell_h {
                    let glyph_row = self.font_row_8x16(ch, y as u8);
                    let dst_y = row * cell_h + y;
                    let dst_row_base = dst_y * width + col * cell_w;

                    for x in 0..8 {
                        let bit = (glyph_row >> (7 - x)) & 1;
                        self.back[dst_row_base + x] = if bit != 0 { fg_px } else { bg_px };
                    }

                    // 9th column: replicate for box drawing range when enabled; otherwise background.
                    let ninth_bit = if line_graphics_enable && (0xC0..=0xDF).contains(&ch) {
                        glyph_row & 1
                    } else {
                        0
                    };
                    self.back[dst_row_base + 8] = if ninth_bit != 0 { fg_px } else { bg_px };
                }

                // Cursor overlay.
                if self.cursor_visible_at(mem_index as u16) {
                    let (start, end) = self.cursor_scanlines();
                    if start <= end {
                        for y in start..=end {
                            if y >= cell_h as u8 {
                                continue;
                            }
                            let dst_y = row * cell_h + y as usize;
                            let dst_row_base = dst_y * width + col * cell_w;
                            for x in 0..cell_w {
                                let px = &mut self.back[dst_row_base + x];
                                *px = if *px == fg_px { bg_px } else { fg_px };
                            }
                        }
                    }
                }
            }
        }
    }

    fn cursor_visible_at(&self, cell_index: u16) -> bool {
        // Cursor disable bit is bit5 of cursor start register.
        if (self.crtc[0x0A] & 0x20) != 0 {
            return false;
        }
        let cursor_pos = (((self.crtc[0x0E] as u16) << 8) | self.crtc[0x0F] as u16) & 0x3FFF;
        cursor_pos == (cell_index & 0x3FFF)
    }

    fn cursor_scanlines(&self) -> (u8, u8) {
        let start = self.crtc[0x0A] & 0x1F;
        let end = self.crtc[0x0B] & 0x1F;
        (start, end)
    }

    fn font_row_8x16(&self, ch: u8, row: u8) -> u8 {
        let row8 = (row / 2) as usize;
        FONT8X8_CP437[ch as usize][row8]
    }

    fn attribute_palette_lookup(&self, color: u8) -> u8 {
        // Attribute Controller indices.
        const MODE_CONTROL: usize = 0x10;
        const COLOR_PLANE_ENABLE: usize = 0x12;
        const COLOR_SELECT: usize = 0x14;

        // Mirror the VGA Attribute Controller palette mapping logic:
        // - Color Plane Enable masks the 4-bit color index.
        // - Palette registers provide a 6-bit "PEL" (0..=63).
        // - When the Mode Control P54S bit is set, palette bits 5-4 are sourced from
        //   Color Select bits 3-2 instead of the palette register.
        // - The top 2 bits of the DAC index (7-6) come from Color Select bits 1-0.
        let mode_control = self.attribute[MODE_CONTROL];
        let color_plane_enable = self.attribute[COLOR_PLANE_ENABLE] & 0x0F;
        let color_select = self.attribute[COLOR_SELECT];

        let masked = (color & 0x0F) & color_plane_enable;

        // Palette entry is 6-bit (0..=63).
        let mut pel = self.attribute[masked as usize] & 0x3F;

        // VGA "Palette bits 5-4 select" (P54S): when set, bits 5-4 of the palette entry come from
        // Color Select bits 3-2 instead of the palette register.
        if (mode_control & 0x80) != 0 {
            pel = (pel & 0x0F) | ((color_select & 0x0C) << 2);
        }

        // Bits 7-6 of the final DAC index come from Color Select bits 1-0.
        ((color_select & 0x03) << 6) | pel
    }

    fn render_mode_13h(&mut self) {
        let width = 320usize;
        let height = 200usize;
        self.back.fill(0);
        let start = self.crtc_start_address_bytes() & 0xFFFF;
        // In chain-4 modes, the CRTC offset register (0x13) is programmed in terms of the per-plane
        // scanline pitch. Multiply by 4 to get the logical pixel pitch in the CPU-visible A0000
        // aperture.
        let pitch = self
            .crtc_offset_bytes()
            .and_then(|bytes_per_plane| bytes_per_plane.checked_mul(4))
            .unwrap_or(width);
        for y in 0..height {
            for x in 0..width {
                let dst_linear = y * width + x;
                let mem_linear = y * pitch + x;
                let addr = start.wrapping_add(mem_linear) & 0xFFFF;
                let plane = addr & 3;
                let off = addr >> 2;
                let idx = if plane < self.config.legacy_plane_count {
                    self.vram[plane * VGA_PLANE_SIZE + off]
                } else {
                    0
                };
                let color = self.dac[(idx & self.pel_mask) as usize];
                self.back[dst_linear] = rgb_to_rgba_u32(color);
            }
        }
    }

    fn render_planar_4bpp(&mut self, width: u32, height: u32) {
        self.back.fill(0);
        let width_usize = width as usize;
        let height_usize = height as usize;
        let bytes_per_line = self
            .crtc_offset_bytes()
            .unwrap_or_else(|| width_usize.div_ceil(8));
        let start = self.crtc_start_address_bytes();

        for y in 0..height_usize {
            for x in 0..width_usize {
                let byte_index =
                    self.plane_offset(start.wrapping_add(y * bytes_per_line + (x / 8)));
                let bit = 7 - (x & 7);
                let mut color = 0u8;
                for plane in 0..4 {
                    if plane >= self.config.legacy_plane_count {
                        continue;
                    }
                    let b = self.vram[plane * VGA_PLANE_SIZE + byte_index];
                    let v = (b >> bit) & 1;
                    color |= v << plane;
                }
                // Apply the PEL mask (0x3C6) to the final DAC index.
                let dac_idx = self.attribute_palette_lookup(color) & self.pel_mask;
                self.back[y * width_usize + x] = rgb_to_rgba_u32(self.dac[dac_idx as usize]);
            }
        }
    }

    fn render_svga(&mut self, width: u32, height: u32, bpp: u16) {
        self.back.fill(0);
        let x_off = usize::from(self.vbe.x_offset);
        let y_off = usize::from(self.vbe.y_offset);
        let Ok(fb_base) = usize::try_from(self.config.lfb_offset) else {
            return;
        };

        let bytes_per_pixel = match bpp {
            32 => 4,
            24 => 3,
            16 | 15 => 2,
            8 => 1,
            _ => 4,
        } as u32;

        let stride_bytes = if self.vbe_bytes_per_scan_line_override != 0 {
            u32::from(self.vbe_bytes_per_scan_line_override)
        } else {
            let stride_pixels = self.vbe.effective_stride_pixels();
            stride_pixels.saturating_mul(bytes_per_pixel)
        };
        let Ok(stride_bytes) = usize::try_from(stride_bytes) else {
            return;
        };
        let bytes_per_pixel_usize = bytes_per_pixel as usize;
        if stride_bytes == 0 || bytes_per_pixel_usize == 0 {
            return;
        }

        // Compute the displayed base address within VRAM:
        //   base = fb_base + y_offset * bytes_per_scan_line + x_offset * bytes_per_pixel
        //
        // Clamp/fail gracefully if the arithmetic overflows or if the computed base is outside VRAM.
        let Some(base) = y_off
            .checked_mul(stride_bytes)
            .and_then(|v| v.checked_add(x_off.checked_mul(bytes_per_pixel_usize)?))
            .and_then(|v| fb_base.checked_add(v))
        else {
            return;
        };
        if base >= self.vram.len() {
            return;
        }

        for y in 0..height {
            let dst_row = (y * width) as usize;
            let Some(src_row_base) = (y as usize)
                .checked_mul(stride_bytes)
                .and_then(|v| base.checked_add(v))
            else {
                continue;
            };
            for x in 0..width {
                let Some(src) = (x as usize)
                    .checked_mul(bytes_per_pixel_usize)
                    .and_then(|v| src_row_base.checked_add(v))
                else {
                    continue;
                };
                let px = match bpp {
                    32 => {
                        // VBE packed pixels: little-endian B,G,R,X
                        let b = *self.vram.get(src).unwrap_or(&0);
                        let g = *self.vram.get(src.saturating_add(1)).unwrap_or(&0);
                        let r = *self.vram.get(src.saturating_add(2)).unwrap_or(&0);
                        rgb_to_rgba_u32(Rgb { r, g, b })
                    }
                    24 => {
                        let b = *self.vram.get(src).unwrap_or(&0);
                        let g = *self.vram.get(src.saturating_add(1)).unwrap_or(&0);
                        let r = *self.vram.get(src.saturating_add(2)).unwrap_or(&0);
                        rgb_to_rgba_u32(Rgb { r, g, b })
                    }
                    16 => {
                        let lo = *self.vram.get(src).unwrap_or(&0) as u16;
                        let hi = *self.vram.get(src.saturating_add(1)).unwrap_or(&0) as u16;
                        let v = lo | (hi << 8);
                        let r = ((v >> 11) & 0x1F) as u8;
                        let g = ((v >> 5) & 0x3F) as u8;
                        let b = (v & 0x1F) as u8;
                        rgb_to_rgba_u32(Rgb {
                            r: (r << 3) | (r >> 2),
                            g: (g << 2) | (g >> 4),
                            b: (b << 3) | (b >> 2),
                        })
                    }
                    15 => {
                        let lo = *self.vram.get(src).unwrap_or(&0) as u16;
                        let hi = *self.vram.get(src.saturating_add(1)).unwrap_or(&0) as u16;
                        let v = lo | (hi << 8);
                        let r = ((v >> 10) & 0x1F) as u8;
                        let g = ((v >> 5) & 0x1F) as u8;
                        let b = (v & 0x1F) as u8;
                        rgb_to_rgba_u32(Rgb {
                            r: (r << 3) | (r >> 2),
                            g: (g << 3) | (g >> 2),
                            b: (b << 3) | (b >> 2),
                        })
                    }
                    8 => {
                        let idx = *self.vram.get(src).unwrap_or(&0);
                        rgb_to_rgba_u32(self.dac[(idx & self.pel_mask) as usize])
                    }
                    _ => 0,
                };
                self.back[dst_row + x as usize] = px;
            }
        }
    }

    fn write_dac_data(&mut self, value: u8) {
        let idx = self.dac_write_index as usize;
        let component = (self.dac_write_subindex as usize) % 3;

        // Real VGA hardware uses a 6-bit DAC. In practice, a lot of guest software writes
        // 8-bit (0..=255) component values directly. We support both by detecting 8-bit mode per
        // palette entry:
        //
        // - If any component in the 3-byte RGB sequence is > 63, treat the whole entry as 8-bit
        //   and downscale all components via `>> 2`.
        // - Otherwise, treat the entry as 6-bit.
        //
        // This handles common 8-bit palettes where some components are <= 63 (dark) as long as
        // at least one of R/G/B exceeds 63 within the same entry.
        self.dac_write_latch[component] = value;

        self.dac_write_subindex = (self.dac_write_subindex + 1) % 3;
        if self.dac_write_subindex != 0 {
            return;
        }

        let is_8bit = self.dac_write_latch.iter().any(|&v| v > 0x3F);
        let to_vga_6bit = |v: u8| -> u8 {
            if is_8bit {
                v >> 2
            } else {
                v & 0x3F
            }
        };

        let r = palette::vga_6bit_to_8bit(to_vga_6bit(self.dac_write_latch[0]));
        let g = palette::vga_6bit_to_8bit(to_vga_6bit(self.dac_write_latch[1]));
        let b = palette::vga_6bit_to_8bit(to_vga_6bit(self.dac_write_latch[2]));
        self.dac[idx] = Rgb { r, g, b };

        self.dac_write_index = self.dac_write_index.wrapping_add(1);
        self.dirty = true;
    }

    fn read_dac_data(&mut self) -> u8 {
        let idx = self.dac_read_index as usize;
        let component = self.dac_read_subindex;
        let v = match component {
            0 => palette::vga_8bit_to_6bit(self.dac[idx].r),
            1 => palette::vga_8bit_to_6bit(self.dac[idx].g),
            2 => palette::vga_8bit_to_6bit(self.dac[idx].b),
            _ => 0,
        };
        self.dac_read_subindex = (self.dac_read_subindex + 1) % 3;
        if self.dac_read_subindex == 0 {
            self.dac_read_index = self.dac_read_index.wrapping_add(1);
        }
        v
    }

    fn vbe_read_reg(&self, index: u16) -> u16 {
        match index {
            0x0000 => 0xB0C5, // ID
            0x0001 => self.vbe.xres,
            0x0002 => self.vbe.yres,
            0x0003 => self.vbe.bpp,
            0x0004 => self.vbe.enable,
            0x0005 => self.vbe.bank,
            0x0006 => self.vbe.virt_width,
            0x0007 => self.vbe.virt_height,
            0x0008 => self.vbe.x_offset,
            0x0009 => self.vbe.y_offset,
            0x000A => {
                let fb_base = usize::try_from(self.config.lfb_offset).unwrap_or(0);
                u16::try_from(self.vram.len().saturating_sub(fb_base) / (64 * 1024))
                    .unwrap_or(u16::MAX)
            }
            _ => 0,
        }
    }

    fn vbe_write_reg(&mut self, index: u16, value: u16) {
        match index {
            0x0001 => {
                self.vbe.xres = value;
                self.vbe_bytes_per_scan_line_override = 0;
            }
            0x0002 => {
                self.vbe.yres = value;
                self.vbe_bytes_per_scan_line_override = 0;
            }
            0x0003 => {
                self.vbe.bpp = value;
                self.vbe_bytes_per_scan_line_override = 0;
            }
            0x0004 => {
                self.vbe.enable = value;
                self.vbe_bytes_per_scan_line_override = 0;
            }
            0x0005 => self.vbe.bank = value,
            0x0006 => {
                self.vbe.virt_width = value;
                self.vbe_bytes_per_scan_line_override = 0;
            }
            0x0007 => {
                self.vbe.virt_height = value;
                self.vbe_bytes_per_scan_line_override = 0;
            }
            0x0008 => self.vbe.x_offset = value,
            0x0009 => self.vbe.y_offset = value,
            _ => {}
        }
        self.dirty = true;
    }
}

#[derive(Debug, Clone, Copy)]
enum LegacyWriteTargets {
    Single { plane: usize, off: usize },
    Planar { off: usize },
}

#[derive(Debug, Clone, Copy)]
enum LegacyReadTarget {
    Single { plane: usize, off: usize },
    Planar { off: usize },
}

impl DisplayOutput for VgaDevice {
    fn get_framebuffer(&self) -> &[u32] {
        &self.front
    }

    fn get_resolution(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    fn present(&mut self) {
        if !self.dirty {
            return;
        }
        self.render();
        std::mem::swap(&mut self.front, &mut self.back);
        self.dirty = false;
    }
}

impl VgaDevice {
    fn read_sequencer_reg(&self, idx: u8) -> u8 {
        let idx = idx as usize;
        if idx < self.sequencer.len() {
            self.sequencer[idx]
        } else {
            self.sequencer_ext[idx]
        }
    }

    fn write_sequencer_reg(&mut self, idx: u8, value: u8) {
        let idx = idx as usize;
        if idx < self.sequencer.len() {
            self.sequencer[idx] = value;
        } else {
            self.sequencer_ext[idx] = value;
        }
    }

    fn read_graphics_reg(&self, idx: u8) -> u8 {
        let idx = idx as usize;
        if idx < self.graphics.len() {
            self.graphics[idx]
        } else {
            self.graphics_ext[idx]
        }
    }

    fn write_graphics_reg(&mut self, idx: u8, value: u8) {
        let idx = idx as usize;
        if idx < self.graphics.len() {
            self.graphics[idx] = value;
        } else {
            self.graphics_ext[idx] = value;
        }
    }

    fn read_crtc_reg(&self, idx: u8) -> u8 {
        let idx = idx as usize;
        if idx < self.crtc.len() {
            self.crtc[idx]
        } else {
            self.crtc_ext[idx]
        }
    }

    fn write_crtc_reg(&mut self, idx: u8, value: u8) {
        let idx_usize = idx as usize;
        if idx_usize < self.crtc.len() {
            self.crtc[idx_usize] = value;
        } else {
            self.crtc_ext[idx_usize] = value;
        }
    }

    fn read_attribute_reg(&self, idx: u8) -> u8 {
        let idx = (idx & 0x1F) as usize;
        if idx < self.attribute.len() {
            self.attribute[idx]
        } else {
            self.attribute_ext[idx]
        }
    }

    fn write_attribute_reg(&mut self, idx: u8, value: u8) {
        let idx = (idx & 0x1F) as usize;
        if idx < self.attribute.len() {
            self.attribute[idx] = value;
        } else {
            self.attribute_ext[idx] = value;
        }
    }

    fn port_read_u8(&mut self, port: u16) -> u8 {
        match port {
            // VGA misc output.
            //
            // Real hardware reads Misc Output at 0x3CC (and 0x3C2 is Input Status 0). We accept
            // reads from either port for maximum guest compatibility.
            0x3CC | 0x3C2 => self.misc_output,

            // VGA register files are exposed as adjacent index/data port pairs. Multi-byte port
            // accesses (e.g. `inw`/`outw` on the index port) are handled by `PortIO::port_read`
            // reading consecutive bytes.
            // Sequencer.
            0x3C4 => self.sequencer_index,
            0x3C5 => self.read_sequencer_reg(self.sequencer_index),

            // Graphics controller.
            0x3CE => self.graphics_index,
            0x3CF => self.read_graphics_reg(self.graphics_index),

            // CRTC.
            //
            // VGA adapters can expose the CRTC register file at either:
            // - 0x3D4/0x3D5 (color I/O decode), or
            // - 0x3B4/0x3B5 (mono I/O decode).
            //
            // Aero's VGA model keeps a single CRTC register array, so we accept both port bases.
            0x3D4 | 0x3B4 => self.crtc_index,
            0x3D5 | 0x3B5 => self.read_crtc_reg(self.crtc_index),

            // Attribute controller data read (index written via 0x3C0).
            0x3C1 => self.read_attribute_reg(self.attribute_index),

            // Input status 1. Reading resets the attribute flip-flop.
            0x3DA | 0x3BA => {
                self.attribute_flip_flop_data = false;
                let in_vblank = self.in_vblank();
                self.input_status1_vretrace = in_vblank;
                let v = if in_vblank { 0x08 } else { 0x00 };
                // Bit 3: vertical retrace. Bit 0: display enable (rough approximation).
                v | (v >> 3)
            }

            // DAC.
            0x3C6 => self.pel_mask,
            0x3C7 => self.dac_read_index,
            0x3C8 => self.dac_write_index,
            0x3C9 => self.read_dac_data(),

            // Unimplemented ports.
            _ => 0xFF,
        }
    }

    fn port_write_u8(&mut self, port: u16, val: u8) {
        match port {
            // VGA misc output.
            0x3C2 => {
                self.misc_output = val;
                self.dirty = true;
            }

            // VGA register files are exposed as adjacent index/data port pairs. Multi-byte port
            // accesses (e.g. `outw dx, ax` on the index port) are handled by `PortIO::port_write`
            // splitting the word into consecutive byte writes.
            // Sequencer.
            0x3C4 => self.sequencer_index = val,
            0x3C5 => {
                self.write_sequencer_reg(self.sequencer_index, val);
                self.dirty = true;
            }

            // Graphics controller.
            0x3CE => self.graphics_index = val,
            0x3CF => {
                self.write_graphics_reg(self.graphics_index, val);
                self.dirty = true;
            }

            // CRTC.
            0x3D4 | 0x3B4 => self.crtc_index = val,
            0x3D5 | 0x3B5 => {
                let idx = self.crtc_index;
                if idx <= 0x07 && (self.crtc.get(0x11).copied().unwrap_or(0) & 0x80) != 0 {
                    return;
                }
                self.write_crtc_reg(idx, val);
                self.dirty = true;
            }

            // Attribute controller (index/data with flip-flop).
            0x3C0 => {
                if !self.attribute_flip_flop_data {
                    self.attribute_index = val & 0x1F;
                    self.attribute_flip_flop_data = true;
                } else {
                    self.write_attribute_reg(self.attribute_index, val);
                    self.attribute_flip_flop_data = false;
                    self.dirty = true;
                }
            }

            // DAC.
            0x3C6 => {
                self.pel_mask = val;
                self.dirty = true;
            }
            0x3C7 => {
                self.dac_read_index = val;
                self.dac_read_subindex = 0;
            }
            0x3C8 => {
                self.dac_write_index = val;
                self.dac_write_subindex = 0;
            }
            0x3C9 => self.write_dac_data(val),

            // Writes to unimplemented ports are ignored.
            _ => {}
        }
    }
}

impl PortIO for VgaDevice {
    fn port_read(&mut self, port: u16, size: usize) -> u32 {
        match size {
            0 => 0,
            1 => u32::from(self.port_read_u8(port)),
            2 => {
                // Bochs VBE.
                if port == VBE_DISPI_INDEX_PORT {
                    return u32::from(self.vbe_index);
                }
                if port == VBE_DISPI_DATA_PORT {
                    return u32::from(self.vbe_read_reg(self.vbe_index));
                }

                let lo = self.port_read_u8(port);
                let hi = self.port_read_u8(port.wrapping_add(1));
                u32::from(u16::from_le_bytes([lo, hi]))
            }
            4 => {
                let b0 = self.port_read_u8(port);
                let b1 = self.port_read_u8(port.wrapping_add(1));
                let b2 = self.port_read_u8(port.wrapping_add(2));
                let b3 = self.port_read_u8(port.wrapping_add(3));
                u32::from_le_bytes([b0, b1, b2, b3])
            }
            _ => io_all_ones(size),
        }
    }

    fn port_write(&mut self, port: u16, size: usize, val: u32) {
        match size {
            0 => {}
            1 => self.port_write_u8(port, val as u8),
            2 => {
                // Bochs VBE.
                if port == VBE_DISPI_INDEX_PORT {
                    self.vbe_index = (val & 0xFFFF) as u16;
                    return;
                }
                if port == VBE_DISPI_DATA_PORT {
                    self.vbe_write_reg(self.vbe_index, (val & 0xFFFF) as u16);
                    return;
                }

                let [b0, b1] = (val as u16).to_le_bytes();
                self.port_write_u8(port, b0);
                self.port_write_u8(port.wrapping_add(1), b1);
            }
            4 => {
                let [b0, b1, b2, b3] = val.to_le_bytes();
                self.port_write_u8(port, b0);
                self.port_write_u8(port.wrapping_add(1), b1);
                self.port_write_u8(port.wrapping_add(2), b2);
                self.port_write_u8(port.wrapping_add(3), b3);
            }
            _ => {}
        }
    }
}

#[cfg(any(test, feature = "io-snapshot"))]
impl IoSnapshot for VgaDevice {
    const DEVICE_ID: [u8; 4] = *b"VGAD";
    // This device uses TLV-encoded optional fields, so new state can be added without bumping the
    // major version as long as defaults are sensible.
    const DEVICE_VERSION: SnapshotVersion = SnapshotVersion::new(1, 1);

    fn save_state(&self) -> Vec<u8> {
        const TAG_MISC_OUTPUT: u16 = 1;
        const TAG_SEQUENCER_INDEX: u16 = 2;
        const TAG_SEQUENCER: u16 = 3;
        const TAG_GRAPHICS_INDEX: u16 = 4;
        const TAG_GRAPHICS: u16 = 5;
        const TAG_CRTC_INDEX: u16 = 6;
        const TAG_CRTC: u16 = 7;
        const TAG_ATTRIBUTE_INDEX: u16 = 8;
        const TAG_ATTRIBUTE_FLIP_FLOP: u16 = 9;
        const TAG_ATTRIBUTE: u16 = 10;
        const TAG_INPUT_STATUS1_VRETRACE: u16 = 11;
        const TAG_PEL_MASK: u16 = 12;
        const TAG_DAC_WRITE_INDEX: u16 = 13;
        const TAG_DAC_WRITE_SUBINDEX: u16 = 14;
        const TAG_DAC_READ_INDEX: u16 = 15;
        const TAG_DAC_READ_SUBINDEX: u16 = 16;
        const TAG_DAC: u16 = 17;
        const TAG_VBE_INDEX: u16 = 18;
        const TAG_VBE_REGS: u16 = 19;
        const TAG_VRAM: u16 = 20;
        const TAG_LATCHES: u16 = 21;
        const TAG_DAC_WRITE_LATCH: u16 = 22;
        const TAG_VBLANK_TIME_NS: u16 = 23;
        const TAG_VBE_BYTES_PER_SCAN_LINE_OVERRIDE: u16 = 24;
        const TAG_SEQUENCER_EXT: u16 = 25;
        const TAG_GRAPHICS_EXT: u16 = 26;
        const TAG_CRTC_EXT: u16 = 27;
        const TAG_ATTRIBUTE_EXT: u16 = 28;
        const TAG_LFB_BASE: u16 = 29;
        const TAG_VRAM_LEN: u16 = 30;

        let mut w = SnapshotWriter::new(Self::DEVICE_ID, Self::DEVICE_VERSION);

        w.field_u8(TAG_MISC_OUTPUT, self.misc_output);

        w.field_u8(TAG_SEQUENCER_INDEX, self.sequencer_index);
        w.field_bytes(TAG_SEQUENCER, self.sequencer.to_vec());
        w.field_bytes(TAG_SEQUENCER_EXT, self.sequencer_ext.to_vec());

        w.field_u8(TAG_GRAPHICS_INDEX, self.graphics_index);
        w.field_bytes(TAG_GRAPHICS, self.graphics.to_vec());
        w.field_bytes(TAG_GRAPHICS_EXT, self.graphics_ext.to_vec());

        w.field_u8(TAG_CRTC_INDEX, self.crtc_index);
        w.field_bytes(TAG_CRTC, self.crtc.to_vec());
        w.field_bytes(TAG_CRTC_EXT, self.crtc_ext.to_vec());

        w.field_u8(TAG_ATTRIBUTE_INDEX, self.attribute_index);
        w.field_bool(TAG_ATTRIBUTE_FLIP_FLOP, self.attribute_flip_flop_data);
        w.field_bytes(TAG_ATTRIBUTE, self.attribute.to_vec());
        w.field_bytes(TAG_ATTRIBUTE_EXT, self.attribute_ext.to_vec());
        w.field_bool(TAG_INPUT_STATUS1_VRETRACE, self.input_status1_vretrace);

        w.field_u8(TAG_PEL_MASK, self.pel_mask);
        w.field_u8(TAG_DAC_WRITE_INDEX, self.dac_write_index);
        w.field_u8(TAG_DAC_WRITE_SUBINDEX, self.dac_write_subindex);
        w.field_u8(TAG_DAC_READ_INDEX, self.dac_read_index);
        w.field_u8(TAG_DAC_READ_SUBINDEX, self.dac_read_subindex);
        w.field_bytes(TAG_DAC_WRITE_LATCH, self.dac_write_latch.to_vec());

        // Palette: pack as tightly as possible (RGB triplets).
        let mut pal = Vec::with_capacity(256 * 3);
        for rgb in &self.dac {
            pal.push(rgb.r);
            pal.push(rgb.g);
            pal.push(rgb.b);
        }
        w.field_bytes(TAG_DAC, pal);

        w.field_u16(TAG_VBE_INDEX, self.vbe_index);
        w.field_bytes(
            TAG_VBE_REGS,
            Encoder::new()
                .u16(self.vbe.xres)
                .u16(self.vbe.yres)
                .u16(self.vbe.bpp)
                .u16(self.vbe.enable)
                .u16(self.vbe.bank)
                .u16(self.vbe.virt_width)
                .u16(self.vbe.virt_height)
                .u16(self.vbe.x_offset)
                .u16(self.vbe.y_offset)
                .finish(),
        );
        w.field_u16(
            TAG_VBE_BYTES_PER_SCAN_LINE_OVERRIDE,
            self.vbe_bytes_per_scan_line_override,
        );

        // Device configuration.
        w.field_u32(TAG_LFB_BASE, self.svga_lfb_base);
        w.field_u32(
            TAG_VRAM_LEN,
            u32::try_from(self.vram.len()).unwrap_or(u32::MAX),
        );

        // VRAM is the main framebuffer backing store; include it verbatim for deterministic output.
        w.field_bytes(TAG_VRAM, self.vram.clone());
        w.field_bytes(TAG_LATCHES, self.latches.to_vec());
        // Deterministic vblank clock used by the legacy Input Status 1 register.
        w.field_u64(TAG_VBLANK_TIME_NS, self.vblank_time_ns);

        w.finish()
    }

    fn load_state(&mut self, bytes: &[u8]) -> SnapshotResult<()> {
        const TAG_MISC_OUTPUT: u16 = 1;
        const TAG_SEQUENCER_INDEX: u16 = 2;
        const TAG_SEQUENCER: u16 = 3;
        const TAG_GRAPHICS_INDEX: u16 = 4;
        const TAG_GRAPHICS: u16 = 5;
        const TAG_CRTC_INDEX: u16 = 6;
        const TAG_CRTC: u16 = 7;
        const TAG_ATTRIBUTE_INDEX: u16 = 8;
        const TAG_ATTRIBUTE_FLIP_FLOP: u16 = 9;
        const TAG_ATTRIBUTE: u16 = 10;
        const TAG_INPUT_STATUS1_VRETRACE: u16 = 11;
        const TAG_PEL_MASK: u16 = 12;
        const TAG_DAC_WRITE_INDEX: u16 = 13;
        const TAG_DAC_WRITE_SUBINDEX: u16 = 14;
        const TAG_DAC_READ_INDEX: u16 = 15;
        const TAG_DAC_READ_SUBINDEX: u16 = 16;
        const TAG_DAC: u16 = 17;
        const TAG_VBE_INDEX: u16 = 18;
        const TAG_VBE_REGS: u16 = 19;
        const TAG_VRAM: u16 = 20;
        const TAG_LATCHES: u16 = 21;
        const TAG_DAC_WRITE_LATCH: u16 = 22;
        const TAG_VBLANK_TIME_NS: u16 = 23;
        const TAG_VBE_BYTES_PER_SCAN_LINE_OVERRIDE: u16 = 24;
        const TAG_SEQUENCER_EXT: u16 = 25;
        const TAG_GRAPHICS_EXT: u16 = 26;
        const TAG_CRTC_EXT: u16 = 27;
        const TAG_ATTRIBUTE_EXT: u16 = 28;
        const TAG_LFB_BASE: u16 = 29;
        const TAG_VRAM_LEN: u16 = 30;

        let r = SnapshotReader::parse(bytes, Self::DEVICE_ID)?;
        r.ensure_device_major(Self::DEVICE_VERSION.major)?;
        let snapshot_minor = r.header().device_version.minor;

        // Reset to a deterministic baseline while preserving config unless the snapshot provides
        // an explicit override.
        let cfg = self.config;
        let lfb_base = r.u32(TAG_LFB_BASE)?.unwrap_or(self.svga_lfb_base);
        let vram_len = r
            .u32(TAG_VRAM_LEN)?
            .map(|v| v as usize)
            .or_else(|| r.bytes(TAG_VRAM).map(|b| b.len()))
            .unwrap_or(cfg.vram_size);
        if vram_len < cfg.legacy_plane_count * VGA_PLANE_SIZE
            || vram_len < usize::try_from(cfg.lfb_offset).ok().unwrap_or(usize::MAX)
            || vram_len > u32::MAX as usize
        {
            return Err(SnapshotError::InvalidFieldEncoding("vram_len"));
        }
        *self = Self::new_with_config(VgaConfig {
            vram_size: vram_len,
            vram_bar_base: lfb_base.wrapping_sub(cfg.lfb_offset),
            lfb_offset: cfg.lfb_offset,
            legacy_plane_count: cfg.legacy_plane_count,
        });

        if let Some(v) = r.u8(TAG_MISC_OUTPUT)? {
            self.misc_output = v;
        }

        if let Some(v) = r.u8(TAG_SEQUENCER_INDEX)? {
            self.sequencer_index = v;
        }
        if let Some(buf) = r.bytes(TAG_SEQUENCER) {
            if buf.len() != self.sequencer.len() {
                return Err(SnapshotError::InvalidFieldEncoding("sequencer"));
            }
            self.sequencer.copy_from_slice(buf);
        }
        if let Some(buf) = r.bytes(TAG_SEQUENCER_EXT) {
            if buf.len() != self.sequencer_ext.len() {
                return Err(SnapshotError::InvalidFieldEncoding("sequencer_ext"));
            }
            self.sequencer_ext.copy_from_slice(buf);
        }

        if let Some(v) = r.u8(TAG_GRAPHICS_INDEX)? {
            self.graphics_index = v;
        }
        if let Some(buf) = r.bytes(TAG_GRAPHICS) {
            if buf.len() != self.graphics.len() {
                return Err(SnapshotError::InvalidFieldEncoding("graphics"));
            }
            self.graphics.copy_from_slice(buf);
        }
        if let Some(buf) = r.bytes(TAG_GRAPHICS_EXT) {
            if buf.len() != self.graphics_ext.len() {
                return Err(SnapshotError::InvalidFieldEncoding("graphics_ext"));
            }
            self.graphics_ext.copy_from_slice(buf);
        }

        if let Some(v) = r.u8(TAG_CRTC_INDEX)? {
            self.crtc_index = v;
        }
        if let Some(buf) = r.bytes(TAG_CRTC) {
            if buf.len() != self.crtc.len() {
                return Err(SnapshotError::InvalidFieldEncoding("crtc"));
            }
            self.crtc.copy_from_slice(buf);
        }
        if let Some(buf) = r.bytes(TAG_CRTC_EXT) {
            if buf.len() != self.crtc_ext.len() {
                return Err(SnapshotError::InvalidFieldEncoding("crtc_ext"));
            }
            self.crtc_ext.copy_from_slice(buf);
        }

        if let Some(v) = r.u8(TAG_ATTRIBUTE_INDEX)? {
            self.attribute_index = v;
        }
        if let Some(v) = r.bool(TAG_ATTRIBUTE_FLIP_FLOP)? {
            self.attribute_flip_flop_data = v;
        }
        if let Some(buf) = r.bytes(TAG_ATTRIBUTE) {
            if buf.len() != self.attribute.len() {
                return Err(SnapshotError::InvalidFieldEncoding("attribute"));
            }
            self.attribute.copy_from_slice(buf);
        }
        if let Some(buf) = r.bytes(TAG_ATTRIBUTE_EXT) {
            if buf.len() != self.attribute_ext.len() {
                return Err(SnapshotError::InvalidFieldEncoding("attribute_ext"));
            }
            self.attribute_ext.copy_from_slice(buf);
        }
        if let Some(v) = r.bool(TAG_INPUT_STATUS1_VRETRACE)? {
            self.input_status1_vretrace = v;
        }

        if let Some(v) = r.u8(TAG_PEL_MASK)? {
            self.pel_mask = v;
        }
        if let Some(v) = r.u8(TAG_DAC_WRITE_INDEX)? {
            self.dac_write_index = v;
        }
        if let Some(v) = r.u8(TAG_DAC_WRITE_SUBINDEX)? {
            self.dac_write_subindex = v;
        }
        if let Some(buf) = r.bytes(TAG_DAC_WRITE_LATCH) {
            if buf.len() != self.dac_write_latch.len() {
                return Err(SnapshotError::InvalidFieldEncoding("dac_write_latch"));
            }
            self.dac_write_latch.copy_from_slice(buf);
        }
        if let Some(v) = r.u8(TAG_DAC_READ_INDEX)? {
            self.dac_read_index = v;
        }
        if let Some(v) = r.u8(TAG_DAC_READ_SUBINDEX)? {
            self.dac_read_subindex = v;
        }
        if let Some(buf) = r.bytes(TAG_DAC) {
            if buf.len() != 256 * 3 {
                return Err(SnapshotError::InvalidFieldEncoding("dac"));
            }
            for i in 0..256 {
                let base = i * 3;
                self.dac[i] = Rgb {
                    r: buf[base],
                    g: buf[base + 1],
                    b: buf[base + 2],
                };
            }
        }

        if let Some(v) = r.u16(TAG_VBE_INDEX)? {
            self.vbe_index = v;
        }
        if let Some(buf) = r.bytes(TAG_VBE_REGS) {
            let mut d = Decoder::new(buf);
            self.vbe.xres = d.u16()?;
            self.vbe.yres = d.u16()?;
            self.vbe.bpp = d.u16()?;
            self.vbe.enable = d.u16()?;
            self.vbe.bank = d.u16()?;
            self.vbe.virt_width = d.u16()?;
            self.vbe.virt_height = d.u16()?;
            self.vbe.x_offset = d.u16()?;
            self.vbe.y_offset = d.u16()?;
            d.finish()?;
        }
        if let Some(v) = r.u16(TAG_VBE_BYTES_PER_SCAN_LINE_OVERRIDE)? {
            self.vbe_bytes_per_scan_line_override = v;
        }

        if let Some(buf) = r.bytes(TAG_VRAM) {
            if buf.len() != self.vram.len() {
                return Err(SnapshotError::InvalidFieldEncoding("vram"));
            }
            self.vram.copy_from_slice(buf);

            // Backward-compatible migration for snapshots captured before the VRAM layout change
            // that moved the VBE packed-pixel framebuffer after the legacy VGA planes
            // (device_version 1.0).
            //
            // Old layout (v1.0): VBE linear/banked framebuffer starts at vram[0..].
            // New layout (v1.1+): VBE framebuffer starts at vram[lfb_offset..].
            //
            // For v1.0 snapshots captured while VBE is enabled, copy the beginning of VRAM into the
            // new VBE region so the renderer reads the expected pixels.
            if snapshot_minor == 0 && self.vbe.enabled() {
                let fb_base = usize::try_from(self.config.lfb_offset).unwrap_or(0);
                if let Some(vbe_region_len) = self.vram.len().checked_sub(fb_base) {
                    let vbe_len = std::cmp::min(buf.len(), vbe_region_len);
                    if vbe_len != 0 {
                        if let Some(dst_end) = fb_base.checked_add(vbe_len) {
                            if dst_end <= self.vram.len() && vbe_len <= buf.len() {
                                self.vram[fb_base..dst_end].copy_from_slice(&buf[..vbe_len]);
                            }
                        }
                    }
                }
            }
        }

        if let Some(buf) = r.bytes(TAG_LATCHES) {
            if buf.len() != self.latches.len() {
                return Err(SnapshotError::InvalidFieldEncoding("latches"));
            }
            self.latches.copy_from_slice(buf);
        }
        if let Some(v) = r.u64(TAG_VBLANK_TIME_NS)? {
            self.vblank_time_ns = v;
        }

        // Output buffers are derived; force a re-render on the next `present()`.
        self.width = 0;
        self.height = 0;
        self.front.clear();
        self.back.clear();
        self.dirty = true;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    use aero_shared::scanout_state::{SCANOUT_SOURCE_LEGACY_TEXT, SCANOUT_SOURCE_LEGACY_VBE_LFB};

    fn vbe_write(dev: &mut VgaDevice, index: u16, val: u16) {
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, index as u32);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, val as u32);
    }

    fn vbe_write_bgrx32(dev: &mut VgaDevice, byte_offset: u32, b: u8, g: u8, r: u8) {
        let base = dev.lfb_base();
        dev.mem_write_u8(base.wrapping_add(byte_offset), b);
        dev.mem_write_u8(base.wrapping_add(byte_offset + 1), g);
        dev.mem_write_u8(base.wrapping_add(byte_offset + 2), r);
        dev.mem_write_u8(base.wrapping_add(byte_offset + 3), 0x00);
    }

    fn fnv1a64(bytes: &[u8]) -> u64 {
        const FNV_OFFSET: u64 = 0xcbf29ce484222325;
        const FNV_PRIME: u64 = 0x00000100000001B3;
        let mut hash = FNV_OFFSET;
        for b in bytes {
            hash ^= *b as u64;
            hash = hash.wrapping_mul(FNV_PRIME);
        }
        hash
    }

    fn framebuffer_hash(dev: &VgaDevice) -> u64 {
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(dev.front.as_ptr() as *const u8, dev.front.len() * 4)
        };
        fnv1a64(bytes)
    }

    #[test]
    fn port_io_size0_is_noop() {
        let mut dev = VgaDevice::new();

        // Reading input status (0x3DA) normally resets the attribute flip-flop, but a size-0 access
        // must be a true no-op.
        dev.attribute_flip_flop_data = true;
        assert_eq!(dev.port_read(0x3DA, 0), 0);
        assert!(dev.attribute_flip_flop_data);
    }

    #[test]
    fn attribute_flip_flop_resets_on_input_status_read() {
        let mut dev = VgaDevice::new();

        // First write selects attribute index and flips to "data" state.
        dev.port_write(0x3C0, 1, 0x10);
        assert!(dev.attribute_flip_flop_data);

        // Reading input status should reset back to "index" state.
        dev.port_read(0x3DA, 1);
        assert!(!dev.attribute_flip_flop_data);

        // Now writing to 0x3C0 should be treated as an index write again.
        dev.port_write(0x3C0, 1, 0x11);
        assert_eq!(dev.attribute_index, 0x11);
        assert!(dev.attribute_flip_flop_data);
    }

    #[test]
    fn attribute_palette_color_plane_enable_masks_index() {
        let mut dev = VgaDevice::new();

        // Make the mapping deterministic for the test.
        dev.attribute[0x10] = 0x00; // mode control: P54S=0
        dev.attribute[0x14] = 0x00; // color select: high bits=0

        dev.attribute[0] = 0x05;
        dev.attribute[1] = 0x06;
        dev.attribute[2] = 0x07;

        // Disable plane 0 (mask bit0), so index 1 maps to 0, and index 3 maps to 2.
        dev.attribute[0x12] = 0x0E;

        assert_eq!(dev.attribute_palette_lookup(1), 0x05);
        assert_eq!(dev.attribute_palette_lookup(3), 0x07);
    }

    #[test]
    fn attribute_palette_color_select_sets_high_bits() {
        let mut dev = VgaDevice::new();

        dev.attribute[0x10] = 0x00; // mode control: P54S=0
        dev.attribute[0x12] = 0x0F; // color plane enable
        dev.attribute[0x14] = 0x02; // color select bits 1-0 = 2 => DAC bits 7-6 = 0b10

        dev.attribute[0x0F] = 0x3F; // PEL = 0b00_111111

        assert_eq!(dev.attribute_palette_lookup(0x0F), 0xBF);
    }

    #[test]
    fn attribute_palette_mode_control_p54s_overrides_palette_bits() {
        let mut dev = VgaDevice::new();

        dev.attribute[0x10] = 0x80; // mode control: P54S=1
        dev.attribute[0x12] = 0x0F; // color plane enable
        dev.attribute[0x14] = 0x0D; // bits 3-2=0b11 => PEL bits 5-4=0b11, bits 1-0=0b01 => DAC bits 7-6=0b01

        dev.attribute[0x03] = 0x0A; // PEL low bits = 0xA, high bits overridden by color_select

        // Expect: DAC bits 7-6 = 0b01 => 0x40; PEL bits 5-4 = 0b11 => 0x30; PEL bits 3-0 = 0xA
        assert_eq!(dev.attribute_palette_lookup(0x03), 0x7A);
    }

    #[test]
    fn text_mode_golden_hash() {
        let mut dev = VgaDevice::new();
        dev.set_text_mode_80x25();

        // Disable cursor for deterministic output.
        dev.crtc[0x0A] = 0x20;

        // Write "A" in the top-left cell with light grey on blue.
        let base = 0xB8000u32;
        dev.mem_write_u8(base, b'A');
        dev.mem_write_u8(base + 1, 0x1F);

        dev.present();
        assert_eq!(dev.get_resolution(), (720, 400));
        assert_eq!(framebuffer_hash(&dev), 0x5cfe440e33546065);
    }

    #[test]
    fn text_mode_respects_crtc_offset_register() {
        let mut dev = VgaDevice::new();
        dev.set_text_mode_80x25();

        // Disable cursor for deterministic output.
        dev.crtc[0x0A] = 0x20;

        // Set a non-standard row pitch: 81 cells per row (instead of 80).
        dev.crtc[0x13] = 81;

        let base = 0xB8000u32;
        // Row 0, col 0 => cell index 0.
        dev.mem_write_u8(base, b'A');
        dev.mem_write_u8(base + 1, 0x1F); // bg=1 (blue)

        // Row 1, col 0 => cell index 81 when pitch is 81 cells/row.
        let row1_cell0 = base + 81 * 2;
        dev.mem_write_u8(row1_cell0, b'B');
        dev.mem_write_u8(row1_cell0 + 1, 0x2F); // bg=2 (green)

        dev.present();

        let fb = dev.get_framebuffer();
        let width = dev.get_resolution().0 as usize;
        // Use the 9th column of each cell (x=8) which is always background for normal glyphs.
        assert_eq!(fb[8], rgb_to_rgba_u32(dev.dac[1]));
        assert_eq!(fb[16 * width + 8], rgb_to_rgba_u32(dev.dac[2]));
    }

    #[test]
    fn text_mode_respects_crtc_start_address_and_byte_mode() {
        let mut dev = VgaDevice::new();
        dev.set_text_mode_80x25();

        // Disable cursor for deterministic output.
        dev.crtc[0x0A] = 0x20;

        // Two cells with different background colors so we can detect which one is at the origin.
        let base = 0xB8000u32;
        // Cell 0: bg=1 (blue), fg=0 (black).
        dev.mem_write_u8(base, b' ');
        dev.mem_write_u8(base + 1, 0x10);
        // Cell 1: bg=2 (green), fg=0.
        dev.mem_write_u8(base + 2, b' ');
        dev.mem_write_u8(base + 3, 0x20);

        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[1]));

        // Default CRTC byte mode is off; start address is interpreted as a word offset, so
        // start=1 selects cell 1.
        dev.crtc[0x0C] = 0;
        dev.crtc[0x0D] = 1;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[2]));

        // Enable CRTC byte mode (0x17 bit6). Now start=1 is a byte offset and rounds down to cell 0.
        dev.crtc[0x17] |= 0x40;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[1]));

        // start=2 bytes selects cell 1.
        dev.crtc[0x0D] = 2;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[2]));
    }

    #[test]
    fn pel_mask_applies_to_text_mode_palette_lookup() {
        let mut dev = VgaDevice::new();
        dev.set_text_mode_80x25();
        // Disable cursor for deterministic output.
        dev.crtc[0x0A] = 0x20;

        // Put a blank cell with a non-black background at the top-left.
        let base = 0xB8000u32;
        dev.mem_write_u8(base, b' ');
        dev.mem_write_u8(base + 1, 0x1F); // bg=1 (blue), fg=15 (white)

        dev.present();
        // With the default palette, DAC index 1 is blue.
        assert_eq!(dev.get_framebuffer()[0], 0xFFAA_0000);

        // Mask all indices down to 0, forcing both fg/bg to use DAC entry 0 (black).
        dev.port_write(0x3C6, 1, 0x00);
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], 0xFF00_0000);
    }

    #[test]
    fn text_mode_cp437_box_drawing_glyph_is_not_blank() {
        let mut dev = VgaDevice::new();
        dev.set_text_mode_80x25();

        // Disable cursor for deterministic output.
        dev.crtc[0x0A] = 0x20;

        // Use a CP437 box drawing glyph in the "line graphics" range. The renderer has special
        // 9th-column replication logic for 0xC0..=0xDF when line graphics are enabled.
        let base = 0xB8000u32;
        let ch = 0xC4u8; // '─'
        let attr = 0x1F; // fg=15 (light grey), bg=1 (blue)

        dev.mem_write_u8(base, ch);
        dev.mem_write_u8(base + 1, attr);
        dev.present();

        let fg = attr & 0x0F;
        let bg = (attr >> 4) & 0x0F; // blink is disabled in set_text_mode_80x25()

        let fg_px = rgb_to_rgba_u32(dev.dac[dev.attribute_palette_lookup(fg) as usize]);
        let bg_px = rgb_to_rgba_u32(dev.dac[dev.attribute_palette_lookup(bg) as usize]);
        assert_ne!(fg_px, bg_px);

        let fb = dev.get_framebuffer();
        let width = dev.get_resolution().0 as usize;

        // The first cell is 9x16 pixels at the top-left of the framebuffer.
        let mut has_fg = false;
        for y in 0..16usize {
            for x in 0..9usize {
                if fb[y * width + x] == fg_px {
                    has_fg = true;
                    break;
                }
            }
            if has_fg {
                break;
            }
        }
        assert!(
            has_fg,
            "expected CP437 glyph 0x{ch:02X} to render at least one foreground pixel"
        );

        // Optional: validate that the 9th column is replicated for at least one row.
        let mut ninth_col_fg = false;
        for y in 0..16usize {
            if fb[y * width + 8] == fg_px {
                // Replication duplicates the right-most glyph pixel.
                assert_eq!(fb[y * width + 7], fg_px);
                ninth_col_fg = true;
                break;
            }
        }
        assert!(
            ninth_col_fg,
            "expected 9th-column replication for CP437 glyph 0x{ch:02X}"
        );
    }

    #[test]
    fn mode13h_golden_hash() {
        let mut dev = VgaDevice::new();
        dev.set_mode_13h();

        // Fill the 64k window with a repeating ramp.
        let base = 0xA0000u32;
        for i in 0..(320 * 200) {
            dev.mem_write_u8(base + i as u32, (i & 0xFF) as u8);
        }

        dev.present();
        assert_eq!(dev.get_resolution(), (320, 200));
        let hash_before = framebuffer_hash(&dev);
        assert_eq!(hash_before, 0xf54b1d9c21a2a115);

        // Now enable a VBE LFB mode and write a pixel into the LFB. This must not clobber VGA plane
        // storage; if the LFB overlaps a VGA plane, switching back to mode 13h would change the
        // rendered output (and thus the golden hash).
        dev.port_write(0x01CE, 2, 0x0001);
        dev.port_write(0x01CF, 2, 64);
        dev.port_write(0x01CE, 2, 0x0002);
        dev.port_write(0x01CF, 2, 64);
        dev.port_write(0x01CE, 2, 0x0003);
        dev.port_write(0x01CF, 2, 32);
        dev.port_write(0x01CE, 2, 0x0004);
        dev.port_write(0x01CF, 2, 0x0041);

        dev.mem_write_u8(SVGA_LFB_BASE, 0x12);
        dev.mem_write_u8(SVGA_LFB_BASE + 1, 0x34);
        dev.mem_write_u8(SVGA_LFB_BASE + 2, 0x56);
        dev.mem_write_u8(SVGA_LFB_BASE + 3, 0x78);

        // Disable VBE again and ensure the VGA output is unchanged.
        dev.port_write(0x01CE, 2, 0x0004);
        dev.port_write(0x01CF, 2, 0x0000);

        dev.present();
        assert_eq!(dev.get_resolution(), (320, 200));
        assert_eq!(framebuffer_hash(&dev), hash_before);
    }

    #[test]
    fn mode13h_respects_crtc_start_address_and_byte_mode() {
        let mut dev = VgaDevice::new();
        dev.set_mode_13h();

        let base = 0xA0000u32;
        for i in 0..32u32 {
            dev.mem_write_u8(base + i, i as u8);
        }

        // Default CRTC byte mode is off; start address is interpreted as a word offset, so
        // start=1 shifts by 2 bytes (2 pixels).
        dev.crtc[0x0C] = 0;
        dev.crtc[0x0D] = 1;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[2]));

        // Enable CRTC byte mode (0x17 bit6); now start=1 shifts by 1 byte (1 pixel).
        dev.crtc[0x17] |= 0x40;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[1]));
    }

    #[test]
    fn mode13h_respects_crtc_offset_register() {
        let mut dev = VgaDevice::new();
        dev.set_mode_13h();

        let base = 0xA0000u32;
        dev.mem_write_u8(base, 1);
        // If the scanline pitch is 8 bytes (2 bytes/plane), the first pixel of row 1 reads from
        // address 8.
        dev.mem_write_u8(base + 8, 2);

        // CRTC offset is in words when byte mode is disabled; offset=1 => 2 bytes/plane => 8 bytes
        // of pixel data per scanline.
        dev.crtc[0x13] = 1;
        dev.dirty = true;
        dev.present();

        assert_eq!(dev.get_resolution(), (320, 200));
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[1]));
        assert_eq!(dev.get_framebuffer()[320], rgb_to_rgba_u32(dev.dac[2]));
    }

    #[test]
    fn register_writes_switch_to_mode13h() {
        let mut dev = VgaDevice::new();

        // Attribute controller: set graphics enable bit in mode control (index 0x10).
        dev.port_read(0x3DA, 1); // reset flip-flop
        dev.port_write(0x3C0, 1, 0x10);
        dev.port_write(0x3C0, 1, 0x01);

        // Sequencer: enable chain4 in memory mode (index 4).
        dev.port_write(0x3C4, 1, 0x04);
        dev.port_write(0x3C5, 1, 0x08);

        // Graphics controller: map to A0000 64KiB (index 6, bits 2-3 = 01).
        dev.port_write(0x3CE, 1, 0x06);
        dev.port_write(0x3CF, 1, 0x04);

        dev.present();
        assert_eq!(dev.get_resolution(), (320, 200));
    }

    #[test]
    fn planar_render_respects_crtc_start_address_and_byte_mode() {
        let mut dev = VgaDevice::new();

        // Enable graphics mode while keeping chain-4 disabled so the renderer chooses the planar
        // 4bpp path.
        dev.attribute[0x10] |= 0x01;
        dev.sequencer[4] = 0x00;

        // Force a small resolution: 8x1.
        dev.crtc[1] = 0;
        dev.crtc[0x07] = 0;
        dev.crtc[0x12] = 0;

        // Populate three bytes of planar memory with distinct colors at the leftmost pixel (bit7):
        // - byte0 => color 1 (plane0)
        // - byte1 => color 2 (plane1)
        // - byte2 => color 4 (plane2)
        dev.vram[0] = 0x80;
        dev.vram[VGA_PLANE_SIZE + 1] = 0x80;
        dev.vram[2 * VGA_PLANE_SIZE + 2] = 0x80;

        // Byte mode enabled: start=1 selects byte1.
        dev.crtc[0x17] |= 0x40;
        dev.crtc[0x0C] = 0;
        dev.crtc[0x0D] = 1;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_resolution(), (8, 1));
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[2]));

        // Byte mode disabled: start=1 is a word offset => start_byte=2 selects byte2.
        dev.crtc[0x17] &= !0x40;
        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_framebuffer()[0], rgb_to_rgba_u32(dev.dac[4]));
    }

    #[test]
    fn planar_render_respects_crtc_offset_register() {
        let mut dev = VgaDevice::new();

        // Enable graphics mode while keeping chain-4 disabled so the renderer chooses the planar
        // 4bpp path.
        dev.attribute[0x10] |= 0x01;
        dev.sequencer[4] = 0x00;

        // Force a small resolution: 8x2.
        dev.crtc[1] = 0;
        dev.crtc[0x07] = 0;
        dev.crtc[0x12] = 1;

        // Enable CRTC byte mode so the offset register is interpreted in bytes.
        dev.crtc[0x17] |= 0x40;
        // Set the scanline pitch to 2 bytes (normally 1 byte for an 8-pixel-wide planar mode).
        dev.crtc[0x13] = 2;

        // First row leftmost pixel: color 1 (plane0 @ byte 0, bit7).
        dev.vram[0] = 0x80;
        // Second row leftmost pixel: color 2 (plane1 @ byte 2, bit7) due to pitch=2 bytes.
        dev.vram[VGA_PLANE_SIZE + 2] = 0x80;

        dev.dirty = true;
        dev.present();
        assert_eq!(dev.get_resolution(), (8, 2));
        let fb = dev.get_framebuffer();
        assert_eq!(fb[0], rgb_to_rgba_u32(dev.dac[1]));
        assert_eq!(fb[8], rgb_to_rgba_u32(dev.dac[2]));
    }

    #[test]
    fn vbe_linear_framebuffer_write_shows_up_in_output() {
        let mut dev = VgaDevice::new();
        // Use a non-default LFB base to ensure the device model doesn't rely on `SVGA_LFB_BASE`
        // for address translation.
        dev.set_svga_lfb_base(0xD000_0000);

        // Guard against accidental overlap between the VBE LFB region and VGA planes.
        dev.vram[2 * VGA_PLANE_SIZE] = 0xA5;

        // 64x64x32bpp, LFB enabled.
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 64);
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0002);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 64);
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0003);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 32);
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0004);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 0x0041);

        // Write a red pixel at (0,0) in BGRX format.
        let base = dev.lfb_base();
        assert_eq!(base, 0xD000_0000);
        dev.mem_write_u8(base, 0x00); // B
        dev.mem_write_u8(base + 1, 0x00); // G
        dev.mem_write_u8(base + 2, 0xFF); // R
        dev.mem_write_u8(base + 3, 0x00); // X

        assert_eq!(
            &dev.vram[VBE_FRAMEBUFFER_OFFSET..VBE_FRAMEBUFFER_OFFSET + 4],
            &[0x00, 0x00, 0xFF, 0x00],
            "LFB write should land in the LFB region (not VGA planes)"
        );
        assert_eq!(
            dev.vram[2 * VGA_PLANE_SIZE],
            0xA5,
            "LFB write overlapped legacy VGA plane storage"
        );

        dev.present();
        assert_eq!(dev.get_resolution(), (64, 64));
        assert_eq!(dev.get_framebuffer()[0], 0xFF00_00FF);
    }

    #[test]
    fn io_snapshot_vram_layout_migrates_v1_0_vbe_framebuffer_to_offset() {
        // Tags must match the `IoSnapshot` implementation above.
        const TAG_VBE_REGS: u16 = 19;
        const TAG_VRAM: u16 = 20;

        let mut vram = vec![0u8; DEFAULT_VRAM_SIZE];
        // Old layout stored packed-pixel framebuffer at vram[0..]. Write a red pixel at (0,0).
        vram[0] = 0x00; // B
        vram[1] = 0x00; // G
        vram[2] = 0xFF; // R
        vram[3] = 0x00; // X

        let mut w = SnapshotWriter::new(*b"VGAD", SnapshotVersion::new(1, 0));
        w.field_bytes(
            TAG_VBE_REGS,
            Encoder::new()
                .u16(64) // xres
                .u16(64) // yres
                .u16(32) // bpp
                .u16(0x0041) // enable (enabled + LFB)
                .u16(0) // bank
                .u16(64) // virt_width
                .u16(64) // virt_height
                .u16(0) // x_offset
                .u16(0) // y_offset
                .finish(),
        );
        w.field_bytes(TAG_VRAM, vram);
        let snapshot = w.finish();

        let mut dev = VgaDevice::new();
        dev.load_state(&snapshot).unwrap();
        dev.present();
        assert_eq!(dev.get_resolution(), (64, 64));
        assert_eq!(dev.get_framebuffer()[0], 0xFF00_00FF);
        assert_eq!(
            &dev.vram()[VBE_FRAMEBUFFER_OFFSET..VBE_FRAMEBUFFER_OFFSET + 4],
            &[0x00, 0x00, 0xFF, 0x00]
        );
    }

    #[test]
    fn io_snapshot_v1_1_roundtrip_keeps_vram_layout() {
        let mut dev = VgaDevice::new();

        // 64x64x32bpp, LFB enabled.
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 64);
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0002);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 64);
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0003);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 32);
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0004);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 0x0041);

        // Write a red pixel at (0,0) in BGRX format.
        let lfb_base = dev.lfb_base();
        dev.mem_write_u8(lfb_base, 0x00); // B
        dev.mem_write_u8(lfb_base.wrapping_add(1), 0x00); // G
        dev.mem_write_u8(lfb_base.wrapping_add(2), 0xFF); // R
        dev.mem_write_u8(lfb_base.wrapping_add(3), 0x00); // X

        let snapshot = dev.save_state();

        let mut restored = VgaDevice::new();
        restored.load_state(&snapshot).unwrap();
        restored.present();
        assert_eq!(restored.get_resolution(), (64, 64));
        assert_eq!(restored.get_framebuffer()[0], 0xFF00_00FF);
    }

    #[test]
    fn legacy_vga_snapshot_v1_migrates_vbe_framebuffer_to_offset() {
        // Build a legacy `VgaSnapshotV1` that represents the old VRAM layout: VBE framebuffer at
        // vram[0..].
        let mut dev = VgaDevice::new();
        dev.set_svga_mode(64, 64, 32, true);
        let mut snap = dev.snapshot_v1();

        snap.vram.fill(0);
        // Red pixel at (0,0) in BGRX at the legacy base (offset 0).
        snap.vram[0] = 0x00; // B
        snap.vram[1] = 0x00; // G
        snap.vram[2] = 0xFF; // R
        snap.vram[3] = 0x00; // X

        let mut restored = VgaDevice::new();
        restored.restore_snapshot_v1(&snap);
        restored.present();
        assert_eq!(restored.get_resolution(), (64, 64));
        assert_eq!(restored.get_framebuffer()[0], 0xFF00_00FF);
    }

    #[test]
    fn legacy_vga_snapshot_v1_migrates_to_configured_lfb_offset() {
        // Same as `legacy_vga_snapshot_v1_migrates_vbe_framebuffer_to_offset`, but validate that the
        // migration respects `VgaConfig::lfb_offset` (needed for embedding the VGA/VBE frontend
        // behind a BAR-backed VRAM aperture where the LFB starts at a non-standard offset).
        let config = VgaConfig {
            vram_size: 2 * 1024 * 1024,
            vram_bar_base: 0,
            lfb_offset: 0x20000,
            legacy_plane_count: 2,
        };

        let mut dev = VgaDevice::new_with_config(config);
        dev.set_svga_mode(64, 64, 32, true);
        let mut snap = dev.snapshot_v1();

        snap.vram.fill(0);
        // Red pixel at (0,0) in BGRX at the legacy base (offset 0).
        snap.vram[0] = 0x00; // B
        snap.vram[1] = 0x00; // G
        snap.vram[2] = 0xFF; // R
        snap.vram[3] = 0x00; // X

        let mut restored = VgaDevice::new_with_config(config);
        restored.restore_snapshot_v1(&snap);
        restored.present();
        assert_eq!(restored.get_resolution(), (64, 64));
        assert_eq!(restored.get_framebuffer()[0], 0xFF00_00FF);
    }

    #[test]
    fn vbe_banked_window_and_lfb_target_same_memory() {
        let mut dev = VgaDevice::new();

        // 64x64x32bpp, LFB enabled.
        dev.port_write(0x01CE, 2, 0x0001);
        dev.port_write(0x01CF, 2, 64);
        dev.port_write(0x01CE, 2, 0x0002);
        dev.port_write(0x01CF, 2, 64);
        dev.port_write(0x01CE, 2, 0x0003);
        dev.port_write(0x01CF, 2, 32);
        dev.port_write(0x01CE, 2, 0x0004);
        dev.port_write(0x01CF, 2, 0x0041);

        // Write through the banked aperture (A0000) and observe it via the LFB.
        dev.mem_write_u8(0xA0000, 0x5A);
        assert_eq!(dev.mem_read_u8(SVGA_LFB_BASE), 0x5A);

        // Switch to bank 1 and ensure it aliases `LFB + 64KiB`.
        dev.port_write(0x01CE, 2, 0x0005);
        dev.port_write(0x01CF, 2, 1);
        dev.mem_write_u8(0xA0000, 0xA5);
        assert_eq!(dev.mem_read_u8(SVGA_LFB_BASE + 64 * 1024), 0xA5);
    }

    #[test]
    fn planar_write_mode0_set_reset_writes_selected_planes() {
        let mut dev = VgaDevice::new();

        // Seed a byte at the start of the VBE framebuffer region. Planar writes must never clobber
        // it.
        dev.vram[VBE_FRAMEBUFFER_OFFSET] = 0xCC;
        // The last byte of the 64KiB A0000 window is adjacent to the start of the VBE framebuffer
        // region in VRAM (plane 3 ends at `VBE_FRAMEBUFFER_OFFSET - 1`).
        let probe_off = ((VBE_FRAMEBUFFER_OFFSET - 1) & (VGA_PLANE_SIZE - 1)) as u32;

        // Configure a basic planar graphics window at A0000.
        dev.sequencer[4] = 0x00; // chain4 disabled, odd/even disabled
        dev.sequencer[2] = 0x0F; // enable all planes (map mask)

        dev.graphics[6] = 0x04; // memory map 0b01 => A0000 64KiB
        dev.graphics[5] = 0x00; // write mode 0, odd/even off
        dev.graphics[3] = 0x00; // rotate=0, func=replace
        dev.graphics[8] = 0xFF; // bit mask
        dev.graphics[0] = 0b0101; // set/reset: planes 0 and 2 set
        dev.graphics[1] = 0x0F; // enable set/reset for all planes

        dev.mem_write_u8(0xA0000, 0xAA);
        dev.mem_write_u8(0xA0000 + probe_off, 0xAA);

        assert_eq!(dev.vram[0], 0xFF);
        assert_eq!(dev.vram[VGA_PLANE_SIZE], 0x00);
        assert_eq!(dev.vram[2 * VGA_PLANE_SIZE], 0xFF);
        assert_eq!(dev.vram[3 * VGA_PLANE_SIZE], 0x00);

        let probe = probe_off as usize;
        assert_eq!(dev.vram[probe], 0xFF);
        assert_eq!(dev.vram[VGA_PLANE_SIZE + probe], 0x00);
        assert_eq!(dev.vram[2 * VGA_PLANE_SIZE + probe], 0xFF);
        assert_eq!(dev.vram[3 * VGA_PLANE_SIZE + probe], 0x00);

        assert_eq!(
            dev.vram[VBE_FRAMEBUFFER_OFFSET], 0xCC,
            "planar writes overlapped VBE framebuffer region"
        );
    }

    #[test]
    fn planar_write_mode0_applies_bit_mask_and_latches() {
        let mut dev = VgaDevice::new();

        dev.sequencer[4] = 0x00;
        dev.sequencer[2] = 0x01; // plane 0 only

        dev.graphics[6] = 0x04; // A0000 64KiB
        dev.graphics[5] = 0x00; // write mode 0
        dev.graphics[3] = 0x00; // replace
        dev.graphics[8] = 0x0F; // only lower nibble affected
        dev.graphics[0] = 0x00; // set/reset disabled
        dev.graphics[1] = 0x00;

        // Seed destination byte so we can observe latch+mask behavior.
        dev.vram[0] = 0xA0;

        dev.mem_write_u8(0xA0000, 0x05);

        assert_eq!(dev.vram[0], 0xA5);
    }

    #[test]
    fn planar_write_mode1_writes_previous_latches() {
        let mut dev = VgaDevice::new();

        // Configure a basic planar graphics window at A0000.
        dev.sequencer[4] = 0x00; // chain4 disabled, odd/even disabled
        dev.sequencer[2] = 0x0F; // enable all planes (map mask)
        dev.graphics[6] = 0x04; // memory map 0b01 => A0000 64KiB

        // Seed the source byte at offset 0 with distinct plane values.
        dev.vram[0] = 0x11;
        dev.vram[VGA_PLANE_SIZE] = 0x22;
        dev.vram[2 * VGA_PLANE_SIZE] = 0x33;
        dev.vram[3 * VGA_PLANE_SIZE] = 0x44;

        // Seed the destination byte at offset 1 with different values so we can observe the copy.
        dev.vram[1] = 0xAA;
        dev.vram[VGA_PLANE_SIZE + 1] = 0xBB;
        dev.vram[2 * VGA_PLANE_SIZE + 1] = 0xCC;
        dev.vram[3 * VGA_PLANE_SIZE + 1] = 0xDD;

        // Load latches from the source address via a planar read.
        dev.graphics[5] = 0x00; // read mode 0, write mode 0
        dev.graphics[4] = 0x00; // Read Map Select: plane 0 (doesn't matter for latch load)
        let _ = dev.mem_read_u8(0xA0000);

        // Write mode 1 should write the latched values, not the CPU value.
        dev.graphics[5] = 0x01; // write mode 1
        dev.mem_write_u8(0xA0001, 0x00);

        assert_eq!(dev.vram[1], 0x11);
        assert_eq!(dev.vram[VGA_PLANE_SIZE + 1], 0x22);
        assert_eq!(dev.vram[2 * VGA_PLANE_SIZE + 1], 0x33);
        assert_eq!(dev.vram[3 * VGA_PLANE_SIZE + 1], 0x44);
    }

    #[test]
    fn planar_write_mode2_expands_cpu_data_bits_to_planes() {
        let mut dev = VgaDevice::new();

        // Configure a basic planar graphics window at A0000.
        dev.sequencer[4] = 0x00; // chain4 disabled, odd/even disabled
        dev.sequencer[2] = 0x0F; // enable all planes (map mask)
        dev.graphics[6] = 0x04; // memory map 0b01 => A0000 64KiB

        // Write mode 2: each CPU data bit 0..3 expands to a full-byte mask for plane 0..3.
        dev.graphics[5] = 0x02; // write mode 2
        dev.graphics[3] = 0x00; // rotate=0, func=replace
        dev.graphics[8] = 0x0F; // only lower nibble affected (uses latches for upper nibble)

        // Seed destination bytes (per plane) to observe latch+mask behavior.
        dev.vram[0] = 0xA0;
        dev.vram[VGA_PLANE_SIZE] = 0xB0;
        dev.vram[2 * VGA_PLANE_SIZE] = 0xC0;
        dev.vram[3 * VGA_PLANE_SIZE] = 0xD0;

        // Set planes 0 and 2.
        dev.mem_write_u8(0xA0000, 0b0101);

        assert_eq!(dev.vram[0], 0xAF);
        assert_eq!(dev.vram[VGA_PLANE_SIZE], 0xB0);
        assert_eq!(dev.vram[2 * VGA_PLANE_SIZE], 0xCF);
        assert_eq!(dev.vram[3 * VGA_PLANE_SIZE], 0xD0);
    }

    #[test]
    fn planar_write_mode3_uses_set_reset_and_cpu_data_as_mask() {
        let mut dev = VgaDevice::new();

        // Configure a basic planar graphics window at A0000.
        dev.sequencer[4] = 0x00; // chain4 disabled, odd/even disabled
        dev.sequencer[2] = 0x0F; // enable all planes (map mask)
        dev.graphics[6] = 0x04; // memory map 0b01 => A0000 64KiB

        // Write mode 3: CPU data (after rotate) ANDed with bit mask selects which bits are affected;
        // set/reset provides the data (expanded per plane).
        dev.graphics[5] = 0x03; // write mode 3
        dev.graphics[3] = 0x00; // rotate=0, func=replace
        dev.graphics[8] = 0x0F; // only lower nibble eligible for update
        dev.graphics[0] = 0b0101; // set/reset: planes 0 and 2 set, others cleared

        // Seed destination bytes so latch merging is observable.
        for plane in 0..4 {
            dev.vram[plane * VGA_PLANE_SIZE] = 0xAF;
        }

        // Rotated CPU data is 0xAA, so mask = 0x0F & 0xAA = 0x0A.
        dev.mem_write_u8(0xA0000, 0xAA);

        assert_eq!(dev.vram[0], 0xAF);
        assert_eq!(dev.vram[VGA_PLANE_SIZE], 0xA5);
        assert_eq!(dev.vram[2 * VGA_PLANE_SIZE], 0xAF);
        assert_eq!(dev.vram[3 * VGA_PLANE_SIZE], 0xA5);
    }

    #[test]
    fn planar_read_mode0_and_mode1_color_compare() {
        let mut dev = VgaDevice::new();

        // Configure a basic planar graphics window at A0000.
        dev.sequencer[4] = 0x00; // chain4 disabled, odd/even disabled
        dev.graphics[6] = 0x04; // memory map 0b01 => A0000 64KiB

        // Populate one byte of planar data (8 pixels). Each plane byte holds one bit for each
        // pixel, with bit7 corresponding to the leftmost pixel.
        //
        // Colors for the 8 pixels (bit7..bit0): [5, 5, 4, 7, 5, 1, 13, 5]
        dev.vram[0] = 0xDF; // plane 0
        dev.vram[VGA_PLANE_SIZE] = 0x10; // plane 1
        dev.vram[2 * VGA_PLANE_SIZE] = 0xFB; // plane 2
        dev.vram[3 * VGA_PLANE_SIZE] = 0x02; // plane 3

        // Read Mode 0: return the selected plane latch (Read Map Select).
        dev.graphics[5] = 0x00; // read mode 0, write mode 0
        dev.graphics[4] = 0x02; // plane 2
        assert_eq!(dev.mem_read_u8(0xA0000), 0xFB);

        // Read Mode 1: color compare.
        dev.graphics[5] = 0x08; // read mode 1
        dev.graphics[2] = 0x05; // compare against color 5 (0101)
        dev.graphics[7] = 0x0F; // compare all planes
        assert_eq!(dev.mem_read_u8(0xA0000), 0xC9);

        // Mask out plane 3; pixel 6 differs only in that plane, so it now compares equal.
        dev.graphics[7] = 0x07;
        assert_eq!(dev.mem_read_u8(0xA0000), 0xCB);
    }

    #[test]
    fn outw_to_sequencer_index_port_writes_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3C4, 2, 0x0F02);

        assert_eq!(dev.sequencer_index, 0x02);
        assert_eq!(dev.sequencer[0x02], 0x0F);
    }

    #[test]
    fn outw_to_crtc_index_port_writes_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3D4, 2, 0x120E);

        assert_eq!(dev.crtc_index, 0x0E);
        assert_eq!(dev.crtc[0x0E], 0x12);
    }

    #[test]
    fn outw_to_graphics_controller_index_port_writes_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3CE, 2, 0xAA05);

        assert_eq!(dev.graphics_index, 0x05);
        assert_eq!(dev.graphics[0x05], 0xAA);
    }

    #[test]
    fn inw_from_sequencer_index_port_returns_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3C4, 2, 0x0F02);
        assert_eq!(dev.port_read(0x3C4, 2), 0x0F02);
    }

    #[test]
    fn inw_from_graphics_controller_index_port_returns_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3CE, 2, 0xAA05);
        assert_eq!(dev.port_read(0x3CE, 2), 0xAA05);
    }

    #[test]
    fn inw_from_crtc_index_port_returns_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3D4, 2, 0x120E);
        assert_eq!(dev.port_read(0x3D4, 2), 0x120E);
    }

    #[test]
    fn outw_to_crtc_mono_index_port_writes_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3B4, 2, 0x120E);

        assert_eq!(dev.crtc_index, 0x0E);
        assert_eq!(dev.crtc[0x0E], 0x12);
    }

    #[test]
    fn inw_from_crtc_mono_index_port_returns_index_and_data() {
        let mut dev = VgaDevice::new();

        dev.port_write(0x3B4, 2, 0x120E);
        assert_eq!(dev.port_read(0x3B4, 2), 0x120E);
    }

    #[test]
    fn bochs_vbe_ports_are_true_16_bit() {
        let mut dev = VgaDevice::new();

        // Ensure the index port is a true 16-bit port: the full value should be latched, not just
        // the low byte.
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0xBEEF);
        assert_eq!(dev.vbe_index, 0xBEEF);
        assert_eq!(dev.port_read(VBE_DISPI_INDEX_PORT, 2), 0xBEEF);

        // Program a real VBE register (XRES) through the index+data ports and verify we can read
        // it back via `inw` on the data port.
        dev.port_write(VBE_DISPI_INDEX_PORT, 2, 0x0001);
        dev.port_write(VBE_DISPI_DATA_PORT, 2, 0x0123);
        assert_eq!(dev.vbe.xres, 0x0123);
        assert_eq!(dev.port_read(VBE_DISPI_DATA_PORT, 2), 0x0123);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    #[test]
    fn scanout_update_reports_legacy_text_in_text_mode() {
        let dev = VgaDevice::new();
        let update = dev.active_scanout_update();
        assert_eq!(update.source, SCANOUT_SOURCE_LEGACY_TEXT);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    #[test]
    fn scanout_update_reports_vbe_lfb_base_and_pitch() {
        let mut dev = VgaDevice::new();
        dev.set_svga_lfb_base(0xD000_0000);
        dev.set_svga_mode(64, 32, 32, true);

        let update = dev.active_scanout_update();
        assert_eq!(update.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(update.base_paddr_lo, 0xD000_0000);
        assert_eq!(update.base_paddr_hi, 0);
        assert_eq!(update.pitch_bytes, 64 * 4);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    #[test]
    fn scanout_update_accounts_for_stride_and_panning() {
        let mut dev = VgaDevice::new();
        dev.set_svga_lfb_base(0xD000_0000);
        dev.set_svga_mode(64, 32, 32, true);

        // Configure a virtual width larger than the visible resolution and apply a pan offset.
        dev.vbe.virt_width = 128;
        dev.vbe.x_offset = 8;
        dev.vbe.y_offset = 2;

        let update = dev.active_scanout_update();
        assert_eq!(update.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(update.width, 64);
        assert_eq!(update.height, 32);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8X8);

        let expected_pitch = 128u32 * 4;
        assert_eq!(update.pitch_bytes, expected_pitch);

        let base = (update.base_paddr_hi as u64) << 32 | update.base_paddr_lo as u64;
        let expected_base = 0xD000_0000u64 + 2u64 * u64::from(expected_pitch) + 8u64 * 4;
        assert_eq!(base, expected_base);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    #[test]
    fn scanout_update_uses_scanline_override_and_pan_offsets() {
        let mut dev = VgaDevice::new();
        dev.set_svga_lfb_base(0xD000_0000);
        dev.set_svga_mode(64, 32, 32, true);

        // Override pitch to something other than width*4 so we can observe it being used.
        dev.set_vbe_bytes_per_scan_line_override(512);
        dev.vbe.x_offset = 2;
        dev.vbe.y_offset = 3;

        let update = dev.active_scanout_update();
        assert_eq!(update.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(update.pitch_bytes, 512);

        let base = (u64::from(update.base_paddr_hi) << 32) | u64::from(update.base_paddr_lo);
        assert_eq!(base, 0xD000_0000 + 3 * 512 + 2 * 4);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    #[test]
    fn scanout_update_disables_when_pitch_is_not_pixel_aligned() {
        let mut dev = VgaDevice::new();
        dev.set_svga_lfb_base(0xD000_0000);
        dev.set_svga_mode(64, 32, 32, true);

        // Choose a pitch that is >= width*bytes_per_pixel but not divisible by 4.
        dev.set_vbe_bytes_per_scan_line_override(64 * 4 + 1);

        let update = dev.active_scanout_update();
        assert_eq!(update.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(update.base_paddr_lo, 0);
        assert_eq!(update.base_paddr_hi, 0);
        assert_eq!(update.width, 0);
        assert_eq!(update.height, 0);
        assert_eq!(update.pitch_bytes, 0);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8X8);
    }

    #[cfg(any(not(target_arch = "wasm32"), feature = "wasm-threaded"))]
    #[test]
    fn scanout_update_disables_when_pan_offsets_exceed_vram() {
        let mut dev = VgaDevice::new();
        dev.set_svga_lfb_base(0xD000_0000);
        dev.set_svga_mode(64, 32, 32, true);

        // Crank the stride + offset high enough that the visible rectangle would exceed the VBE
        // framebuffer region in VRAM. `active_scanout_update` must detect this and publish a
        // disabled descriptor rather than an out-of-bounds scanout.
        dev.vbe.virt_width = u16::MAX;
        dev.vbe.y_offset = u16::MAX;

        let update = dev.active_scanout_update();
        assert_eq!(update.source, SCANOUT_SOURCE_LEGACY_VBE_LFB);
        assert_eq!(update.base_paddr_lo, 0);
        assert_eq!(update.base_paddr_hi, 0);
        assert_eq!(update.width, 0);
        assert_eq!(update.height, 0);
        assert_eq!(update.pitch_bytes, 0);
        assert_eq!(update.format, SCANOUT_FORMAT_B8G8R8X8);
    }

    #[test]
    fn vbe_banked_window_maps_a0000_to_selected_bank() {
        let mut dev = VgaDevice::new();

        // Enable VBE, but *disable* the linear framebuffer, so A0000 maps to the active bank.
        vbe_write(&mut dev, 0x0001, 640); // xres
        vbe_write(&mut dev, 0x0002, 480); // yres
        vbe_write(&mut dev, 0x0003, 32); // bpp
        vbe_write(&mut dev, 0x0004, 0x0001); // enable, lfb disabled
        vbe_write(&mut dev, 0x0005, 2); // bank

        let window_off = 0x1234u32;
        let paddr = 0xA0000u32 + window_off;

        dev.mem_write_u8(paddr, 0xAB);

        // VBE framebuffer is stored starting at `VBE_FRAMEBUFFER_OFFSET`, and banking is applied
        // within that region.
        let expected_vram_off = VBE_FRAMEBUFFER_OFFSET + (2 * 64 * 1024) + window_off as usize;
        assert_eq!(dev.vram()[expected_vram_off], 0xAB);
        assert_eq!(dev.mem_read_u8(paddr), 0xAB);
    }

    #[test]
    fn vbe_virtual_width_affects_stride_between_scanlines() {
        let mut dev = VgaDevice::new();

        // 4x2 visible, but 8 pixels per scanline in VRAM.
        vbe_write(&mut dev, 0x0001, 4); // xres
        vbe_write(&mut dev, 0x0002, 2); // yres
        vbe_write(&mut dev, 0x0003, 32); // bpp
        vbe_write(&mut dev, 0x0006, 8); // virt_width
        vbe_write(&mut dev, 0x0004, 0x0041); // enable + lfb

        // Seed the start of the first line (byte offset 0) with blue.
        vbe_write_bgrx32(&mut dev, 0, 0xFF, 0x00, 0x00);

        // If the implementation incorrectly uses xres for stride, it will pick this green pixel.
        let wrong_stride_second_line = 4u32 * 4;
        vbe_write_bgrx32(&mut dev, wrong_stride_second_line, 0x00, 0xFF, 0x00);

        // Correct second scanline start: virt_width * bytes_per_pixel.
        let correct_stride_second_line = 8u32 * 4;
        vbe_write_bgrx32(&mut dev, correct_stride_second_line, 0x00, 0x00, 0xFF);

        dev.present();

        assert_eq!(dev.get_resolution(), (4, 2));
        assert_eq!(dev.get_framebuffer()[0], 0xFFFF_0000); // blue
        assert_eq!(dev.get_framebuffer()[4], 0xFF00_00FF); // red
    }

    #[test]
    fn vbe_xy_offsets_shift_the_visible_window_and_16bpp_expands_channels() {
        let mut dev = VgaDevice::new();

        // 2x2 visible, 4x4 virtual, 16bpp RGB565.
        vbe_write(&mut dev, 0x0001, 2); // xres
        vbe_write(&mut dev, 0x0002, 2); // yres
        vbe_write(&mut dev, 0x0003, 16); // bpp
        vbe_write(&mut dev, 0x0006, 4); // virt_width
        vbe_write(&mut dev, 0x0007, 4); // virt_height
        vbe_write(&mut dev, 0x0008, 1); // x_offset
        vbe_write(&mut dev, 0x0009, 1); // y_offset
        vbe_write(&mut dev, 0x0004, 0x0041); // enable + lfb

        let base = dev.lfb_base();
        // Pixel at (0,0): blue (0x001F).
        dev.mem_write_u8(base, 0x1F);
        dev.mem_write_u8(base.wrapping_add(1), 0x00);

        // Pixel at (x_offset,y_offset) = (1,1): 0x8543 (RGB565).
        //
        // r=0b10000 -> 0x84, g=0b101010 -> 0xAA, b=0b00011 -> 0x18.
        // Expected RGBA8888: 0xFF18AA84.
        let bytes_per_pixel = 2u32;
        let virt_width = 4u32;
        let x_offset = 1u32;
        let y_offset = 1u32;
        let offset_bytes = (y_offset * virt_width + x_offset) * bytes_per_pixel;
        dev.mem_write_u8(base.wrapping_add(offset_bytes), 0x43);
        dev.mem_write_u8(base.wrapping_add(offset_bytes + 1), 0x85);

        dev.present();

        assert_eq!(dev.get_resolution(), (2, 2));
        assert_eq!(dev.get_framebuffer()[0], 0xFF18_AA84);
    }
}
